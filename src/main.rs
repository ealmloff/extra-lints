use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::{Metadata, MetadataCommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct DefKey {
    path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateRecord {
    def_key: DefKey,
    kind: String,
    display_path: String,
    file: String,
    line: u32,
    column: u32,
    required_defs: Vec<DefKey>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UseRecord {
    def_key: DefKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TargetArtifact {
    crate_stable_id: String,
    crate_name: String,
    target_name: String,
    target_kind: String,
    candidates: Vec<CandidateRecord>,
    uses: Vec<UseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UnusedDef {
    def_key: DefKey,
    crate_name: String,
    target_name: String,
    kind: String,
    display_path: String,
    file: String,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AggregatedReport {
    unused: Vec<UnusedDef>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args_os();
    let _bin = args.next();

    let command = args.next().unwrap_or_else(|| OsString::from("check"));
    let is_fix = command == "fix";
    if command != "check" && !is_fix {
        bail!(
            "unsupported command `{}`; expected `check` or `fix`",
            command.to_string_lossy()
        );
    }

    let workspace_hint = parse_workspace_path(args)?;
    let workspace_hint = if workspace_hint.is_absolute() {
        workspace_hint
    } else {
        env::current_dir()?.join(workspace_hint)
    };
    let metadata_dir = if workspace_hint.is_dir() {
        workspace_hint
    } else {
        workspace_hint
            .parent()
            .map(Path::to_path_buf)
            .context("workspace hint path must have a parent directory")?
    };

    let metadata = MetadataCommand::new()
        .current_dir(&metadata_dir)
        .exec()
        .with_context(|| {
            format!(
                "failed to read cargo metadata for {}",
                metadata_dir.display()
            )
        })?;

    let workspace_root = PathBuf::from(&metadata.workspace_root);
    let workspace_manifest_path = workspace_root.join("Cargo.toml");
    let target_dir = PathBuf::from(&metadata.target_directory);
    let artifact_dir = target_dir.join("unused_public_items");
    let collect_target_dir = target_dir.join("unused_public_items_collect");
    let emit_target_dir = target_dir.join("unused_public_items_emit");
    let report_path = artifact_dir.join("report.json");
    let lint_crate_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("unused_public_items_in_workspace");
    let lint_library_path = build_lint_library(&lint_crate_path)?;

    if artifact_dir.exists() {
        fs::remove_dir_all(&artifact_dir)
            .with_context(|| format!("failed to clear {}", artifact_dir.display()))?;
    }
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;
    run_dylint(
        &workspace_root,
        &workspace_manifest_path,
        &lint_library_path,
        &artifact_dir,
        &collect_target_dir,
        None,
        "collect",
    )?;

    let report = aggregate_artifacts(&artifact_dir)?;
    let report_json = serde_json::to_vec_pretty(&report)?;
    fs::write(&report_path, &report_json)
        .with_context(|| format!("failed to write {}", report_path.display()))?;

    if is_fix {
        let crate_roots = workspace_crate_roots(&metadata)?;
        apply_fixes(&workspace_root, &report, &crate_roots)?;
    } else {
        run_dylint(
            &workspace_root,
            &workspace_manifest_path,
            &lint_library_path,
            &artifact_dir,
            &emit_target_dir,
            Some(&report_path),
            "emit",
        )?;
    }

    Ok(())
}

fn parse_workspace_path(mut args: impl Iterator<Item = OsString>) -> Result<PathBuf> {
    let mut workspace_path = env::current_dir()?;

    while let Some(arg) = args.next() {
        if arg == "--path" {
            let Some(path) = args.next() else {
                bail!("expected a path after --path");
            };
            workspace_path = PathBuf::from(path);
            continue;
        }

        bail!(
            "unsupported argument `{}`; only --path is supported",
            arg.to_string_lossy()
        );
    }

    Ok(workspace_path)
}

fn run_dylint(
    workspace_root: &Path,
    manifest_path: &Path,
    lint_library_path: &Path,
    artifact_dir: &Path,
    cargo_target_dir: &Path,
    report_path: Option<&Path>,
    mode: &str,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace_root)
        .arg("dylint")
        .arg("--lib-path")
        .arg(lint_library_path)
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--workspace")
        .arg("--")
        .arg("--all-features")
        .arg("--all-targets")
        .env("CARGO_TARGET_DIR", cargo_target_dir)
        .env("RUSTC_WRAPPER", "")
        .env("UNUSED_PUBLIC_ITEMS_MODE", mode)
        .env("UNUSED_PUBLIC_ITEMS_DIR", artifact_dir);

    if let Some(report_path) = report_path {
        command.env("UNUSED_PUBLIC_ITEMS_REPORT", report_path);
    }

    let status = command.status().context("failed to launch cargo dylint")?;
    if !status.success() {
        bail!("cargo dylint failed in `{mode}` mode with status {status}");
    }

    Ok(())
}

fn build_lint_library(lint_crate_path: &Path) -> Result<PathBuf> {
    let manifest_path = lint_crate_path.join("Cargo.toml");

    let mut command = Command::new("cargo");
    command
        .current_dir(lint_crate_path)
        .arg("+nightly-2025-09-18")
        .arg("build")
        .arg("--release")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .env("RUSTC_WRAPPER", "");

    let status = command
        .status()
        .context("failed to build the lint library")?;
    if !status.success() {
        bail!("failed to build the lint library with status {status}");
    }

    let built_library_path = lint_crate_path.join("target").join("release").join(format!(
        "{}unused_public_items_in_workspace.{}",
        dylib_prefix(),
        dylib_extension(),
    ));

    if !built_library_path.exists() {
        bail!(
            "could not find compiled lint library at {}",
            built_library_path.display()
        );
    }

    let dylint_library_path = lint_crate_path.join("target").join("release").join(format!(
        "{}unused_public_items_in_workspace@{}.{}",
        dylib_prefix(),
        dylint_toolchain_name(),
        dylib_extension(),
    ));

    fs::copy(&built_library_path, &dylint_library_path).with_context(|| {
        format!(
            "failed to copy {} to {}",
            built_library_path.display(),
            dylint_library_path.display()
        )
    })?;

    Ok(dylint_library_path)
}

fn dylib_prefix() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        ""
    }

    #[cfg(not(target_os = "windows"))]
    {
        "lib"
    }
}

fn dylib_extension() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "dylib"
    }

    #[cfg(target_os = "linux")]
    {
        "so"
    }

    #[cfg(target_os = "windows")]
    {
        "dll"
    }
}

fn dylint_toolchain_name() -> &'static str {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    {
        "nightly-2025-09-18-aarch64-apple-darwin"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    {
        "nightly-2025-09-18-x86_64-apple-darwin"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    {
        "nightly-2025-09-18-x86_64-unknown-linux-gnu"
    }

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        "nightly-2025-09-18-aarch64-unknown-linux-gnu"
    }

    #[cfg(all(target_arch = "x86_64", target_os = "windows"))]
    {
        "nightly-2025-09-18-x86_64-pc-windows-msvc"
    }
}

fn apply_fixes(
    workspace_root: &Path,
    report: &AggregatedReport,
    crate_roots: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    // Group fixes by file, so we can apply multiple fixes to the same file at once.
    let mut fixes_by_file: BTreeMap<PathBuf, Vec<&UnusedDef>> = BTreeMap::new();
    for def in &report.unused {
        let file_path = workspace_root.join(&def.file);
        fixes_by_file.entry(file_path).or_default().push(def);
    }

    let mut total_fixed = 0;

    for (file_path, mut fixes) in fixes_by_file {
        let source = fs::read_to_string(&file_path)
            .with_context(|| format!("failed to read {}", file_path.display()))?;
        let mut lines: Vec<String> = source.lines().map(String::from).collect();

        // Sort fixes in reverse line order so earlier edits don't shift later ones.
        fixes.sort_by(|a, b| b.line.cmp(&a.line).then(b.column.cmp(&a.column)));

        for def in &fixes {
            let line_idx = (def.line as usize).checked_sub(1);
            let col_idx = (def.column as usize).checked_sub(1);
            let (Some(line_idx), Some(col_idx)) = (line_idx, col_idx) else {
                continue;
            };
            let Some(line) = lines.get(line_idx) else {
                continue;
            };

            // The span starts at the item, which should begin with `pub`.
            if !line[col_idx..].starts_with("pub ") {
                if line[col_idx..].starts_with("pub(") {
                    // Already has a visibility modifier, skip.
                } else {
                    eprintln!(
                        "  skipped: {} ({}:{}) - unexpected token at pub position",
                        def.display_path, def.file, def.line
                    );
                }
                continue;
            }

            if let Some(reason) = visibility_fix_skip_reason(&lines, line_idx) {
                eprintln!(
                    "  skipped: {} ({}:{}) - {reason}",
                    def.display_path, def.file, def.line
                );
                continue;
            }

            let Some(line) = lines.get_mut(line_idx) else {
                continue;
            };
            if line[col_idx..].starts_with("pub ") {
                line.replace_range(col_idx..col_idx + 3, "pub(crate)");
                total_fixed += 1;
                eprintln!("  fixed: {} ({}:{})", def.display_path, def.file, def.line);
            }
        }

        // Preserve trailing newline if the original had one.
        let mut output = lines.join("\n");
        if source.ends_with('\n') {
            output.push('\n');
        }

        fs::write(&file_path, output)
            .with_context(|| format!("failed to write {}", file_path.display()))?;
    }

    let reexport_fixes = apply_reexport_fixes(report, crate_roots)?;
    total_fixed += reexport_fixes;

    eprintln!("fixed {total_fixed} items");
    Ok(())
}

fn visibility_fix_skip_reason(lines: &[String], line_idx: usize) -> Option<&'static str> {
    let attrs = attached_attributes(lines, line_idx);
    if attrs.is_empty() {
        return None;
    }

    if attrs
        .iter()
        .any(|attr| attribute_requires_public_visibility(attr))
    {
        return Some("attached attributes may require public visibility");
    }

    None
}

fn attached_attributes(lines: &[String], line_idx: usize) -> Vec<String> {
    let mut attrs = Vec::new();
    let mut current = String::new();
    let mut collecting = false;

    for raw_line in lines[..line_idx].iter().rev() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            if collecting {
                break;
            }
            continue;
        }

        if trimmed.starts_with("#[") || collecting {
            if !current.is_empty() {
                current.insert(0, '\n');
            }
            current.insert_str(0, trimmed);
            collecting = !trimmed.contains(']');
            if !collecting {
                attrs.push(current.clone());
                current.clear();
            }
            continue;
        }

        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }

        break;
    }

    attrs.reverse();
    attrs
}

fn attribute_requires_public_visibility(attr: &str) -> bool {
    let attr = attr.trim();
    let Some(inner) = attr
        .strip_prefix("#[")
        .and_then(|rest| rest.strip_suffix(']'))
    else {
        return false;
    };
    let inner = inner.trim();
    let attr_name = inner
        .split(|ch: char| ch == '(' || ch == ' ' || ch == '\t')
        .next()
        .unwrap_or(inner);

    if attr_name == "derive" {
        return derive_requires_public_visibility(inner);
    }

    !matches!(
        attr_name,
        "allow"
            | "warn"
            | "deny"
            | "forbid"
            | "cfg"
            | "cfg_attr"
            | "doc"
            | "must_use"
            | "inline"
            | "cold"
            | "deprecated"
            | "expect"
            | "non_exhaustive"
            | "repr"
    )
}

fn derive_requires_public_visibility(attr: &str) -> bool {
    let Some(open) = attr.find('(') else {
        return true;
    };
    let Some(close) = attr.rfind(')') else {
        return true;
    };

    let safe_derives = [
        "Clone",
        "Copy",
        "Debug",
        "Default",
        "Eq",
        "Hash",
        "Ord",
        "PartialEq",
        "PartialOrd",
    ];

    split_top_level(&attr[open + 1..close], ',')
        .into_iter()
        .map(|entry| entry.trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .any(|entry| !safe_derives.contains(&entry.as_str()))
}

fn workspace_crate_roots(metadata: &Metadata) -> Result<BTreeMap<String, PathBuf>> {
    let mut crate_roots = BTreeMap::new();

    for package in &metadata.packages {
        let Some(package_root) = Path::new(package.manifest_path.as_str()).parent() else {
            continue;
        };
        let package_root = package_root.to_path_buf();

        crate_roots.insert(package.name.replace('-', "_"), package_root.clone());
        for target in &package.targets {
            crate_roots.insert(target.name.replace('-', "_"), package_root.clone());
        }
    }

    Ok(crate_roots)
}

fn apply_reexport_fixes(
    report: &AggregatedReport,
    crate_roots: &BTreeMap<String, PathBuf>,
) -> Result<usize> {
    let mut defs_by_crate = BTreeMap::<String, Vec<&UnusedDef>>::new();
    for def in &report.unused {
        defs_by_crate
            .entry(def.crate_name.clone())
            .or_default()
            .push(def);
    }

    let mut total_fixed = 0;

    for (crate_name, defs) in defs_by_crate {
        let Some(crate_root) = crate_roots.get(&crate_name) else {
            continue;
        };
        let src_root = crate_root.join("src");
        if !src_root.is_dir() {
            continue;
        }

        let reexport_hints = defs
            .iter()
            .filter_map(|def| ReexportHint::from_display_path(&def.display_path))
            .collect::<BTreeSet<_>>();
        if reexport_hints.is_empty() {
            continue;
        }

        let rust_files = collect_rust_files(&src_root)?;
        let alias_map = collect_reexport_aliases(&rust_files, &src_root)?;
        for file_path in rust_files {
            let source = fs::read_to_string(&file_path)
                .with_context(|| format!("failed to read {}", file_path.display()))?;
            let module_path = module_path_for_file(&src_root, &file_path)?;
            let (rewritten_source, fixed_lines) =
                rewrite_reexport_statements(&source, &module_path, &reexport_hints, &alias_map);
            let file_fixed = fixed_lines.len();

            if file_fixed == 0 {
                continue;
            }

            for line in fixed_lines {
                eprintln!("  fixed reexport: {}:{}", file_path.display(), line);
            }
            fs::write(&file_path, rewritten_source)
                .with_context(|| format!("failed to write {}", file_path.display()))?;
            total_fixed += file_fixed;
        }
    }

    Ok(total_fixed)
}

fn module_path_for_file(src_root: &Path, file_path: &Path) -> Result<String> {
    let relative = file_path.strip_prefix(src_root).with_context(|| {
        format!(
            "{} is not under {}",
            file_path.display(),
            src_root.display()
        )
    })?;
    let mut components = relative
        .iter()
        .map(|component| component.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let Some(last) = components.pop() else {
        return Ok(String::new());
    };

    match last.as_str() {
        "lib.rs" | "main.rs" => {}
        "mod.rs" => {}
        _ => {
            let stem = last.strip_suffix(".rs").unwrap_or(&last).to_owned();
            components.push(stem);
        }
    }

    Ok(components.join("::"))
}

fn collect_rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    Ok(files)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ReexportHint {
    path: String,
}

impl ReexportHint {
    fn from_display_path(display_path: &str) -> Option<Self> {
        let mut parts = display_path.split("::").collect::<Vec<_>>();
        if parts.len() < 3 {
            return None;
        }

        let _crate_name = parts.remove(0);
        let path = parts.join("::");
        if path.is_empty() {
            return None;
        }

        Some(Self { path })
    }
}

#[derive(Debug, Clone)]
struct ReexportBinding {
    source_path: String,
    rendered: String,
}

fn collect_reexport_aliases(
    rust_files: &[PathBuf],
    src_root: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut aliases = BTreeMap::new();

    for file_path in rust_files {
        let source = fs::read_to_string(file_path)
            .with_context(|| format!("failed to read {}", file_path.display()))?;
        let module_path = module_path_for_file(src_root, file_path)?;

        for statement in collect_use_statements(&source) {
            let Some((_, bindings)) = parse_reexport_statement(&statement, &module_path) else {
                continue;
            };
            for (alias_path, binding) in bindings {
                aliases.insert(alias_path, binding.source_path);
            }
        }
    }

    Ok(aliases)
}

fn resolve_use_path(current_module_path: &str, path: &str) -> Option<String> {
    let path = path.split(" as ").next().unwrap_or(path).trim();
    let mut segments = path
        .split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return None;
    }

    let mut base = current_module_path
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    match segments.first().copied()? {
        "crate" => {
            segments.remove(0);
            if segments.is_empty() {
                return None;
            }
            return Some(segments.join("::"));
        }
        "self" => {
            segments.remove(0);
        }
        _ => {}
    }

    while segments.first().copied() == Some("super") {
        segments.remove(0);
        base.pop()?;
    }

    if segments.is_empty() {
        return None;
    }

    base.extend(segments);
    Some(base.join("::"))
}

fn canonicalize_reexport_path(path: &str, aliases: &BTreeMap<String, String>) -> String {
    let mut current = path.to_owned();
    let mut seen = BTreeSet::new();

    while seen.insert(current.clone()) {
        let Some(next) = aliases.get(&current) else {
            break;
        };
        current = next.clone();
    }

    current
}

fn rewrite_reexport_statements(
    source: &str,
    current_module_path: &str,
    hints: &BTreeSet<ReexportHint>,
    aliases: &BTreeMap<String, String>,
) -> (String, Vec<usize>) {
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len());
    let mut fixed_lines = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if !trimmed.starts_with("pub use ") && !trimmed.starts_with("pub(crate) use ") {
            output.push_str(line);
            index += 1;
            continue;
        }

        let start = index;
        let mut statement = String::new();
        while index < lines.len() {
            statement.push_str(lines[index]);
            let reached_semicolon = lines[index].contains(';');
            index += 1;
            if reached_semicolon {
                break;
            }
        }

        let Some((is_public, bindings)) = parse_reexport_statement(&statement, current_module_path)
        else {
            output.push_str(&statement);
            continue;
        };

        if !is_public {
            output.push_str(&statement);
            continue;
        }

        let mut public_bindings = Vec::new();
        let mut private_bindings = Vec::new();

        for binding in bindings.into_values() {
            let canonical_path = canonicalize_reexport_path(&binding.source_path, aliases);
            if hints.iter().any(|hint| hint.path == canonical_path) {
                private_bindings.push(binding.rendered);
            } else {
                public_bindings.push(binding.rendered);
            }
        }

        if private_bindings.is_empty() {
            output.push_str(&statement);
            continue;
        }

        output.push_str(&render_reexport_statement(
            &statement,
            &public_bindings,
            &private_bindings,
        ));
        fixed_lines.push(start + 1);
    }

    (output, fixed_lines)
}

fn collect_use_statements(source: &str) -> Vec<String> {
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let mut statements = Vec::new();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if !trimmed.starts_with("pub use ") && !trimmed.starts_with("pub(crate) use ") {
            index += 1;
            continue;
        }

        let mut statement = String::new();
        while index < lines.len() {
            statement.push_str(lines[index]);
            let reached_semicolon = lines[index].contains(';');
            index += 1;
            if reached_semicolon {
                break;
            }
        }
        statements.push(statement);
    }

    statements
}

fn parse_reexport_statement(
    statement: &str,
    current_module_path: &str,
) -> Option<(bool, BTreeMap<String, ReexportBinding>)> {
    let trimmed = statement.trim_start();
    let (is_public, use_body) = if let Some(use_body) = trimmed.strip_prefix("pub use ") {
        (true, use_body)
    } else if let Some(use_body) = trimmed.strip_prefix("pub(crate) use ") {
        (false, use_body)
    } else {
        return None;
    };
    let semicolon_idx = use_body.find(';')?;
    let use_body = use_body[..semicolon_idx].trim();

    let bindings = if use_body.contains('{') {
        parse_grouped_reexport_bindings(use_body, current_module_path)?
    } else {
        let binding = parse_simple_reexport_binding(use_body, current_module_path)?;
        let mut bindings = BTreeMap::new();
        bindings.insert(alias_path(current_module_path, &binding.rendered), binding);
        bindings
    };

    Some((is_public, bindings))
}

fn parse_simple_reexport_binding(
    use_body: &str,
    current_module_path: &str,
) -> Option<ReexportBinding> {
    let (path, alias) = split_alias(use_body.trim());
    let source_path = resolve_use_path(current_module_path, path)?;
    let rendered = alias
        .map(|alias| format!("{path} as {alias}"))
        .unwrap_or_else(|| path.to_owned());

    Some(ReexportBinding {
        source_path,
        rendered,
    })
}

fn parse_grouped_reexport_bindings(
    use_body: &str,
    current_module_path: &str,
) -> Option<BTreeMap<String, ReexportBinding>> {
    let open_brace = use_body.find('{')?;
    let close_brace = use_body.rfind('}')?;
    let prefix = use_body[..open_brace].trim_end();
    let prefix = prefix.strip_suffix("::").unwrap_or(prefix).trim_end();
    let group = &use_body[open_brace + 1..close_brace];

    let segments = split_top_level(group, ',');
    if segments
        .iter()
        .any(|segment| segment.contains('{') || segment.contains('}'))
    {
        return None;
    }

    let mut bindings = BTreeMap::new();

    for segment in segments {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }

        let (path, alias) = split_alias(segment);
        let full_path = if prefix.is_empty() {
            path.to_owned()
        } else {
            format!("{prefix}::{path}")
        };
        let source_path = resolve_use_path(current_module_path, &full_path)?;
        let rendered = alias
            .map(|alias| format!("{path} as {alias}"))
            .unwrap_or_else(|| path.to_owned());
        bindings.insert(
            alias_path(current_module_path, &rendered),
            ReexportBinding {
                source_path,
                rendered,
            },
        );
    }

    Some(bindings)
}

fn render_reexport_statement(
    statement: &str,
    public_bindings: &[String],
    private_bindings: &[String],
) -> String {
    let trimmed = statement.trim_start();
    let indent = &statement[..statement.len() - trimmed.len()];
    let use_body = trimmed
        .strip_prefix("pub use ")
        .unwrap_or(trimmed)
        .split(';')
        .next()
        .unwrap_or(trimmed)
        .trim();
    let uses_group = use_body.contains('{');
    let trailing_newline = statement.ends_with('\n');

    let mut rendered = Vec::new();
    if uses_group {
        let open_brace = use_body.find('{').expect("grouped use has open brace");
        let prefix = use_body[..open_brace].trim_end();
        let prefix = prefix.strip_suffix("::").unwrap_or(prefix).trim_end();

        if !public_bindings.is_empty() {
            rendered.push(format!(
                "{indent}pub use {prefix}::{{{}}};",
                public_bindings.join(", ")
            ));
        }
        rendered.push(format!(
            "{indent}pub(crate) use {prefix}::{{{}}};",
            private_bindings.join(", ")
        ));
    } else {
        let binding = private_bindings
            .first()
            .expect("simple reexport rewrite has a private binding");
        if !public_bindings.is_empty() {
            rendered.push(format!("{indent}pub use {};", public_bindings[0]));
        }
        rendered.push(format!("{indent}pub(crate) use {binding};"));
    }

    let mut output = rendered.join("\n");
    if trailing_newline {
        output.push('\n');
    }
    output
}

fn split_alias(segment: &str) -> (&str, Option<&str>) {
    let segment = segment.trim();
    let Some((path, alias)) = segment.rsplit_once(" as ") else {
        return (segment, None);
    };
    (path.trim(), Some(alias.trim()))
}

fn alias_path(current_module_path: &str, rendered: &str) -> String {
    let (_, alias) = split_alias(rendered);
    let leaf = alias.unwrap_or_else(|| rendered.rsplit("::").next().unwrap_or(rendered));
    if current_module_path.is_empty() {
        leaf.to_owned()
    } else {
        format!("{current_module_path}::{leaf}")
    }
}

fn split_top_level(input: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;

    for ch in input.chars() {
        match ch {
            '{' | '(' | '[' => {
                depth += 1;
                current.push(ch);
            }
            '}' | ')' | ']' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            _ if ch == separator && depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        parts.push(current);
    }

    parts
}

fn aggregate_artifacts(artifact_dir: &Path) -> Result<AggregatedReport> {
    let mut artifacts = Vec::new();

    for entry in fs::read_dir(artifact_dir)
        .with_context(|| format!("failed to read {}", artifact_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if entry.file_name() == "report.json" {
            continue;
        }

        let bytes = fs::read(entry.path())
            .with_context(|| format!("failed to read {}", entry.path().display()))?;
        let artifact: TargetArtifact = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", entry.path().display()))?;
        artifacts.push(artifact);
    }

    if artifacts.is_empty() {
        bail!(
            "no collection artifacts were produced in {}",
            artifact_dir.display()
        );
    }

    let used = artifacts
        .iter()
        .flat_map(|artifact| artifact.uses.iter())
        .map(|use_record| use_record.def_key.clone())
        .collect::<std::collections::BTreeSet<_>>();

    let mut candidates = std::collections::BTreeMap::<DefKey, UnusedDef>::new();
    let mut required_defs_by_candidate =
        std::collections::BTreeMap::<DefKey, std::collections::BTreeSet<DefKey>>::new();

    for artifact in artifacts {
        for candidate in artifact.candidates {
            required_defs_by_candidate
                .entry(candidate.def_key.clone())
                .or_default()
                .extend(candidate.required_defs.iter().cloned());

            candidates
                .entry(candidate.def_key.clone())
                .or_insert(UnusedDef {
                    def_key: candidate.def_key,
                    crate_name: artifact.crate_name.clone(),
                    target_name: artifact.target_name.clone(),
                    kind: candidate.kind,
                    display_path: candidate.display_path,
                    file: candidate.file,
                    line: candidate.line,
                    column: candidate.column,
                });
        }
    }

    let mut used = used;
    let mut stack = used.iter().cloned().collect::<Vec<_>>();
    while let Some(def_key) = stack.pop() {
        let Some(required_defs) = required_defs_by_candidate.get(&def_key) else {
            continue;
        };
        for required_def in required_defs {
            if used.insert(required_def.clone()) {
                stack.push(required_def.clone());
            }
        }
    }

    let mut unused = candidates
        .into_iter()
        .filter(|(def_key, _)| !used.contains(def_key))
        .map(|(_, candidate)| candidate)
        .collect::<Vec<_>>();

    unused.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.display_path.cmp(&right.display_path))
    });

    Ok(AggregatedReport { unused })
}
