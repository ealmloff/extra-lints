#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_span;

use std::{
    cell::RefCell,
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{Expr, ExprKind, ImplItem, ImplItemKind, Item, ItemKind, Node, QPath, TyKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::{FileName, Span, def_id::LOCAL_CRATE};
use serde::{Deserialize, Serialize};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when a public item is not referenced from any other crate target
    /// compiled in the current workspace.
    ///
    /// ### Why is this bad?
    ///
    /// Public items expand a crate's API surface. If nothing else in the
    /// workspace references an item, it is often a sign that the item should be
    /// removed or its visibility reduced.
    ///
    /// ### Known problems
    ///
    /// This lint intentionally ignores modules, reexports, trait-associated
    /// items, macro exports, and proc macros in v1 to avoid false positives.
    ///
    /// ### Example
    ///
    /// ```rust
    /// pub fn helper() {}
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// fn helper() {}
    /// ```
    pub UNUSED_PUBLIC_ITEMS_IN_WORKSPACE,
    Warn,
    "public item is not referenced from any other workspace crate"
}

thread_local! {
    static STATE: RefCell<LintState> = RefCell::new(LintState::default());
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Collect { artifact_dir: PathBuf },
    Emit { unused: BTreeSet<DefKey> },
    Disabled,
}

#[derive(Debug, Clone)]
struct Candidate {
    def_key: DefKey,
    kind: &'static str,
    display_path: String,
    span: Span,
}

#[derive(Debug, Clone)]
struct LintState {
    mode: Mode,
    crate_stable_id: String,
    crate_name: String,
    target_name: String,
    target_kind: String,
    candidates: Vec<Candidate>,
    uses: BTreeSet<DefKey>,
}

impl Default for LintState {
    fn default() -> Self {
        Self {
            mode: Mode::Disabled,
            crate_stable_id: String::new(),
            crate_name: String::new(),
            target_name: String::new(),
            target_kind: String::new(),
            candidates: Vec::new(),
            uses: BTreeSet::new(),
        }
    }
}

impl LintState {
    fn for_crate<'tcx>(cx: &LateContext<'tcx>) -> Self {
        Self {
            mode: Mode::from_env(),
            crate_stable_id: stable_crate_id(cx, LOCAL_CRATE),
            crate_name: cx.tcx.crate_name(LOCAL_CRATE).to_string(),
            target_name: env::var("CARGO_CRATE_NAME").unwrap_or_else(|_| "unknown".to_owned()),
            target_kind: env::var("CARGO_BIN_NAME")
                .map(|_| "bin".to_owned())
                .unwrap_or_else(|_| "lib".to_owned()),
            candidates: Vec::new(),
            uses: BTreeSet::new(),
        }
    }

    fn enabled(&self) -> bool {
        !matches!(self.mode, Mode::Disabled)
    }
}

impl Mode {
    fn from_env() -> Self {
        match env::var("UNUSED_PUBLIC_ITEMS_MODE").as_deref() {
            Ok("collect") => env::var_os("UNUSED_PUBLIC_ITEMS_DIR")
                .map(PathBuf::from)
                .map(|artifact_dir| Self::Collect { artifact_dir })
                .unwrap_or(Self::Disabled),
            Ok("emit") => {
                let Some(report_path) = env::var_os("UNUSED_PUBLIC_ITEMS_REPORT") else {
                    return Self::Disabled;
                };
                let Ok(bytes) = fs::read(report_path) else {
                    return Self::Disabled;
                };
                let Ok(report) = serde_json::from_slice::<AggregatedReport>(&bytes) else {
                    return Self::Disabled;
                };
                let unused = report
                    .unused
                    .into_iter()
                    .map(|entry| entry.def_key)
                    .collect();
                Self::Emit { unused }
            }
            _ => Self::Disabled,
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for UnusedPublicItemsInWorkspace {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| *state.borrow_mut() = LintState::for_crate(cx));
    }

    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_item_candidate(cx, item, &mut state);
        });
    }

    fn check_impl_item(&mut self, cx: &LateContext<'tcx>, impl_item: &'tcx ImplItem<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_impl_item_candidate(cx, impl_item, &mut state);
        });
    }

    fn check_path(
        &mut self,
        cx: &LateContext<'tcx>,
        path: &rustc_hir::Path<'tcx>,
        hir_id: rustc_hir::HirId,
    ) {
        if matches!(
            cx.tcx.hir_node(hir_id),
            Node::Item(Item {
                kind: ItemKind::Use(..),
                ..
            })
        ) {
            return;
        }

        let Some(def_id) = path.res.opt_def_id() else {
            return;
        };

        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            record_use(cx, def_id, &mut state);
        });
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if !matches!(expr.kind, ExprKind::MethodCall(..) | ExprKind::Field(..)) {
            return;
        }

        let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
            return;
        };

        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            record_use(cx, def_id, &mut state);
        });
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| {
            let state = std::mem::take(&mut *state.borrow_mut());
            match state.mode {
                Mode::Collect { ref artifact_dir } => {
                    if let Err(error) = write_artifact(cx, &state, &artifact_dir) {
                        cx.tcx.sess.dcx().warn(format!(
                            "unused_public_items_in_workspace failed to write artifact: {error}"
                        ));
                    }
                }
                Mode::Emit { unused } => {
                    for candidate in &state.candidates {
                        if unused.contains(&candidate.def_key) {
                            span_lint_and_help(
                                cx,
                                UNUSED_PUBLIC_ITEMS_IN_WORKSPACE,
                                candidate.span,
                                "public item is not referenced from any other workspace crate",
                                None,
                                "consider reducing visibility or removing the item",
                            );
                        }
                    }
                }
                Mode::Disabled => {}
            }
        });
    }
}

fn maybe_record_item_candidate<'tcx>(
    cx: &LateContext<'tcx>,
    item: &'tcx Item<'tcx>,
    state: &mut LintState,
) {
    if !cx.tcx.visibility(item.owner_id).is_public() || item.span.from_expansion() {
        return;
    }

    let kind = match item.kind {
        ItemKind::Fn { .. } => Some("fn"),
        ItemKind::Const(..) => Some("const"),
        ItemKind::Static(..) => Some("static"),
        ItemKind::TyAlias(..) => Some("type_alias"),
        ItemKind::Struct(..) => Some("struct"),
        ItemKind::Enum(..) => Some("enum"),
        ItemKind::Union(..) => Some("union"),
        ItemKind::Trait(..) => Some("trait"),
        _ => None,
    };

    if let Some(kind) = kind {
        state.candidates.push(Candidate {
            def_key: def_key(cx, item.owner_id.to_def_id()),
            kind,
            display_path: normalized_def_path(cx, item.owner_id.to_def_id()),
            span: item.span,
        });
    }
}

fn maybe_record_impl_item_candidate<'tcx>(
    cx: &LateContext<'tcx>,
    impl_item: &'tcx ImplItem<'tcx>,
    state: &mut LintState,
) {
    if !cx.tcx.visibility(impl_item.owner_id).is_public() || impl_item.span.from_expansion() {
        return;
    }

    let parent_def_id = cx.tcx.hir_get_parent_item(impl_item.hir_id()).def_id;
    let Node::Item(item) = cx.tcx.hir_node_by_def_id(parent_def_id) else {
        return;
    };
    let ItemKind::Impl(impl_) = item.kind else {
        return;
    };
    if impl_.of_trait.is_some() {
        return;
    }

    let TyKind::Path(QPath::Resolved(_, path)) = impl_.self_ty.kind else {
        return;
    };
    let Some(self_def_id) = path.res.opt_def_id() else {
        return;
    };
    if !self_def_id.is_local() || !cx.tcx.visibility(self_def_id).is_public() {
        return;
    }

    let kind = match impl_item.kind {
        ImplItemKind::Fn(..) => Some("inherent_method"),
        ImplItemKind::Const(..) => Some("inherent_const"),
        ImplItemKind::Type(..) => Some("inherent_type"),
    };

    if let Some(kind) = kind {
        state.candidates.push(Candidate {
            def_key: def_key(cx, impl_item.owner_id.to_def_id()),
            kind,
            display_path: normalized_def_path(cx, impl_item.owner_id.to_def_id()),
            span: impl_item.span,
        });
    }
}

fn record_use<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
    state: &mut LintState,
) {
    if def_id.is_local() {
        return;
    }

    state.uses.insert(def_key(cx, def_id));
}

fn write_artifact<'tcx>(
    cx: &LateContext<'tcx>,
    state: &LintState,
    artifact_dir: &Path,
) -> Result<(), String> {
    let candidates = state
        .candidates
        .iter()
        .filter_map(|candidate| {
            span_location(cx, candidate.span).map(|(file, line, column)| CandidateRecord {
                def_key: candidate.def_key.clone(),
                kind: candidate.kind.to_owned(),
                display_path: candidate.display_path.clone(),
                file,
                line,
                column,
            })
        })
        .collect::<Vec<_>>();

    let uses = state
        .uses
        .iter()
        .cloned()
        .map(|def_key| UseRecord { def_key })
        .collect::<Vec<_>>();

    let artifact = TargetArtifact {
        crate_stable_id: state.crate_stable_id.clone(),
        crate_name: state.crate_name.clone(),
        target_name: state.target_name.clone(),
        target_kind: state.target_kind.clone(),
        candidates,
        uses,
    };

    let file_name = format!(
        "{}-{}-{}.json",
        sanitize(&artifact.crate_name),
        sanitize(&artifact.target_name),
        std::process::id(),
    );
    let final_path = artifact_dir.join(file_name);
    let temp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(&artifact).map_err(|error| error.to_string())?;

    fs::write(&temp_path, json).map_err(|error| error.to_string())?;
    fs::rename(&temp_path, &final_path).map_err(|error| error.to_string())?;

    Ok(())
}

fn stable_crate_id<'tcx>(
    cx: &LateContext<'tcx>,
    crate_num: rustc_span::def_id::CrateNum,
) -> String {
    format!("{:?}", cx.tcx.stable_crate_id(crate_num))
}

fn def_key<'tcx>(cx: &LateContext<'tcx>, def_id: rustc_span::def_id::DefId) -> DefKey {
    DefKey {
        path: normalized_def_path(cx, def_id),
    }
}

fn normalized_def_path<'tcx>(cx: &LateContext<'tcx>, def_id: rustc_span::def_id::DefId) -> String {
    let crate_name = cx.tcx.crate_name(def_id.krate).to_string();
    let path = cx.tcx.def_path_str(def_id);
    if path == crate_name || path.starts_with(&format!("{crate_name}::")) {
        path
    } else {
        format!("{crate_name}::{path}")
    }
}

fn span_location<'tcx>(cx: &LateContext<'tcx>, span: Span) -> Option<(String, u32, u32)> {
    let source_map = cx.tcx.sess.source_map();
    let location = source_map.lookup_char_pos(span.lo());
    let FileName::Real(real_file) = &location.file.name else {
        return None;
    };
    Some((
        real_file.local_path()?.display().to_string(),
        location.line.try_into().ok()?,
        (location.col.0 + 1).try_into().ok()?,
    ))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}
