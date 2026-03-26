use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct DefKey {
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum CandidateKey {
    FunctionParam { function: DefKey, index: usize },
    TraitConst { trait_item: DefKey },
    StructField { struct_item: DefKey, index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum OptionVariant {
    Some,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum ObservedValue {
    Some,
    None,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateRecord {
    key: CandidateKey,
    display_path: String,
    description: String,
    file: String,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservationRecord {
    key: CandidateKey,
    value: ObservedValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TargetArtifact {
    crate_stable_id: String,
    crate_name: String,
    target_name: String,
    target_kind: String,
    candidates: Vec<CandidateRecord>,
    observations: Vec<ObservationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReportEntry {
    key: CandidateKey,
    display_path: String,
    description: String,
    variant: OptionVariant,
    file: String,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AggregatedReport {
    redundant: Vec<ReportEntry>,
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
    if command != "check" {
        bail!(
            "unsupported command `{}`; expected `check`",
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
    let artifact_dir = target_dir.join("option_single_variant");
    let collect_target_dir = target_dir.join("option_single_variant_collect");
    let emit_target_dir = target_dir.join("option_single_variant_emit");
    let report_path = artifact_dir.join("report.json");
    let lint_crate_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("option_single_variant");
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
    fs::write(&report_path, report_json)
        .with_context(|| format!("failed to write {}", report_path.display()))?;

    run_dylint(
        &workspace_root,
        &workspace_manifest_path,
        &lint_library_path,
        &artifact_dir,
        &emit_target_dir,
        Some(&report_path),
        "emit",
    )?;

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
        .arg("--all-targets")
        .env("CARGO_TARGET_DIR", cargo_target_dir)
        .env("RUSTC_WRAPPER", "")
        .env("OPTION_SINGLE_VARIANT_MODE", mode)
        .env("OPTION_SINGLE_VARIANT_DIR", artifact_dir);

    if let Some(report_path) = report_path {
        command.env("OPTION_SINGLE_VARIANT_REPORT", report_path);
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
        "{}option_single_variant.{}",
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
        "{}option_single_variant@{}.{}",
        dylib_prefix(),
        dylint_toolchain_name(),
        dylib_extension(),
    ));

    let needs_copy = match (
        fs::metadata(&built_library_path),
        fs::metadata(&dylint_library_path),
    ) {
        (Ok(src), Ok(dst)) => src.modified().ok() > dst.modified().ok(),
        _ => true,
    };
    if needs_copy {
        fs::copy(&built_library_path, &dylint_library_path).with_context(|| {
            format!(
                "failed to copy {} to {}",
                built_library_path.display(),
                dylint_library_path.display()
            )
        })?;
    }

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

    let mut candidates = std::collections::BTreeMap::<CandidateKey, CandidateRecord>::new();
    let mut observations =
        std::collections::BTreeMap::<CandidateKey, std::collections::BTreeSet<ObservedValue>>::new(
        );

    for artifact in artifacts {
        for candidate in artifact.candidates {
            candidates.entry(candidate.key.clone()).or_insert(candidate);
        }

        for observation in artifact.observations {
            observations
                .entry(observation.key.clone())
                .or_default()
                .insert(observation.value);
        }
    }

    let mut redundant = candidates
        .into_iter()
        .filter_map(|(key, candidate)| {
            let values = observations.get(&key)?;
            let variant = match values.iter().copied().collect::<Vec<_>>().as_slice() {
                [ObservedValue::Some] => Some(OptionVariant::Some),
                [ObservedValue::None] => Some(OptionVariant::None),
                _ => None,
            }?;

            Some(ReportEntry {
                key,
                display_path: candidate.display_path,
                description: candidate.description,
                variant,
                file: candidate.file,
                line: candidate.line,
                column: candidate.column,
            })
        })
        .collect::<Vec<_>>();

    redundant.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.display_path.cmp(&right.display_path))
    });

    Ok(AggregatedReport { redundant })
}
