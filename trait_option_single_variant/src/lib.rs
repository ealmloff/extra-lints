#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::ty::is_type_diagnostic_item;
use rustc_hir::def::DefKind;
use rustc_hir::{Expr, ExprKind, ImplItem, ImplItemKind, TraitItem, TraitItemKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;
use rustc_span::{FileName, Span, def_id::LOCAL_CRATE, sym};
use serde::{Deserialize, Serialize};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when a trait-associated `Option` parameter or associated const is
    /// only ever used with one explicit variant (`Some` or `None`) across the
    /// current workspace.
    ///
    /// ### Why is this bad?
    ///
    /// If every call site passes `Some(...)`, or every implementation assigns
    /// only `None`, the `Option` in the trait API is carrying less information
    /// than it suggests. This usually means the trait contract can be made
    /// clearer with a non-optional value or a different API shape.
    ///
    /// ### Known problems
    ///
    /// This lint is intentionally conservative. Any non-literal `Option` value
    /// at a call site or impl assignment suppresses the warning rather than
    /// guessing through dataflow or constant evaluation.
    pub TRAIT_OPTION_SINGLE_VARIANT,
    Warn,
    "trait `Option` API only ever uses one variant across the workspace"
}

thread_local! {
    static STATE: RefCell<LintState> = RefCell::new(LintState::default());
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct DefKey {
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum CandidateKey {
    TraitMethodParam { trait_item: DefKey, index: usize },
    TraitConst { trait_item: DefKey },
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

#[derive(Debug, Clone)]
enum Mode {
    Collect {
        artifact_dir: PathBuf,
    },
    Emit {
        redundant: BTreeMap<CandidateKey, ReportEntry>,
    },
    Disabled,
}

#[derive(Debug, Clone)]
struct Candidate {
    key: CandidateKey,
    display_path: String,
    description: String,
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
    observations: Vec<ObservationRecord>,
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
            observations: Vec::new(),
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
            observations: Vec::new(),
        }
    }

    fn enabled(&self) -> bool {
        !matches!(self.mode, Mode::Disabled)
    }
}

impl Mode {
    fn from_env() -> Self {
        match env::var("TRAIT_OPTION_SINGLE_VARIANT_MODE").as_deref() {
            Ok("collect") => env::var_os("TRAIT_OPTION_SINGLE_VARIANT_DIR")
                .map(PathBuf::from)
                .map(|artifact_dir| Self::Collect { artifact_dir })
                .unwrap_or(Self::Disabled),
            Ok("emit") => {
                let Some(report_path) = env::var_os("TRAIT_OPTION_SINGLE_VARIANT_REPORT") else {
                    return Self::Disabled;
                };
                let Ok(bytes) = fs::read(report_path) else {
                    return Self::Disabled;
                };
                let Ok(report) = serde_json::from_slice::<AggregatedReport>(&bytes) else {
                    return Self::Disabled;
                };
                let redundant = report
                    .redundant
                    .into_iter()
                    .map(|entry| (entry.key.clone(), entry))
                    .collect();
                Self::Emit { redundant }
            }
            _ => Self::Disabled,
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for TraitOptionSingleVariant {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| *state.borrow_mut() = LintState::for_crate(cx));
    }

    fn check_trait_item(&mut self, cx: &LateContext<'tcx>, trait_item: &'tcx TraitItem<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_trait_candidate(cx, trait_item, &mut state);
        });
    }

    fn check_impl_item(&mut self, cx: &LateContext<'tcx>, impl_item: &'tcx ImplItem<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_trait_const_impl(cx, impl_item, &mut state);
        });
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_trait_method_call(cx, expr, &mut state);
        });
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| {
            let state = std::mem::take(&mut *state.borrow_mut());
            match state.mode {
                Mode::Collect { ref artifact_dir } => {
                    if let Err(error) = write_artifact(cx, &state, artifact_dir) {
                        cx.tcx.sess.dcx().warn(format!(
                            "trait_option_single_variant failed to write artifact: {error}"
                        ));
                    }
                }
                Mode::Emit { redundant } => {
                    for candidate in &state.candidates {
                        let Some(entry) = redundant.get(&candidate.key) else {
                            continue;
                        };
                        emit_lint(cx, candidate, entry.variant);
                    }
                }
                Mode::Disabled => {}
            }
        });
    }
}

fn maybe_record_trait_candidate<'tcx>(
    cx: &LateContext<'tcx>,
    trait_item: &'tcx TraitItem<'tcx>,
    state: &mut LintState,
) {
    if trait_item.span.from_expansion() {
        return;
    }

    let trait_item_def_id = trait_item.owner_id.to_def_id();
    let display_path = normalized_def_path(cx, trait_item_def_id);
    match trait_item.kind {
        TraitItemKind::Fn(hir_sig, _) => {
            let assoc_item = cx.tcx.associated_item(trait_item_def_id);
            let fn_sig = cx
                .tcx
                .fn_sig(trait_item_def_id)
                .instantiate_identity()
                .skip_binder();
            let skip = usize::from(assoc_item.is_method());
            let inputs = fn_sig.inputs();

            for (index, hir_ty) in hir_sig.decl.inputs.iter().enumerate() {
                let Some(param_ty) = inputs.get(index + skip).copied() else {
                    continue;
                };
                if !is_option_ty(cx, param_ty) {
                    continue;
                }

                state.candidates.push(Candidate {
                    key: CandidateKey::TraitMethodParam {
                        trait_item: def_key(cx, trait_item_def_id),
                        index,
                    },
                    display_path: display_path.clone(),
                    description: format!(
                        "parameter #{} of trait method `{display_path}`",
                        index + 1
                    ),
                    span: hir_ty.span,
                });
            }
        }
        TraitItemKind::Const(_, default) => {
            let ty = cx.tcx.type_of(trait_item_def_id).instantiate_identity();
            if !is_option_ty(cx, ty) {
                return;
            }

            let key = CandidateKey::TraitConst {
                trait_item: def_key(cx, trait_item_def_id),
            };
            state.candidates.push(Candidate {
                key: key.clone(),
                display_path: display_path.clone(),
                description: format!("associated const `{display_path}`"),
                span: trait_item.span,
            });

            if let Some(body_id) = default {
                let body = cx.tcx.hir_body(body_id);
                record_observation(state, key, classify_option_expr(cx, body.value));
            }
        }
        TraitItemKind::Type(..) => {}
    }
}

fn maybe_record_trait_const_impl<'tcx>(
    cx: &LateContext<'tcx>,
    impl_item: &'tcx ImplItem<'tcx>,
    state: &mut LintState,
) {
    let ImplItemKind::Const(_, body_id) = impl_item.kind else {
        return;
    };
    let rustc_hir::ImplItemImplKind::Trait {
        trait_item_def_id, ..
    } = impl_item.impl_kind
    else {
        return;
    };
    let Ok(trait_item_def_id) = trait_item_def_id else {
        return;
    };

    let key = CandidateKey::TraitConst {
        trait_item: def_key(cx, trait_item_def_id),
    };
    let body = cx.tcx.hir_body(body_id);
    record_observation(state, key, classify_option_expr(cx, body.value));
}

fn maybe_record_trait_method_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    state: &mut LintState,
) {
    match expr.kind {
        ExprKind::MethodCall(_, _, args, _) => {
            let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
                return;
            };
            let Some(trait_item_def_id) = trait_method_def_id(cx, def_id) else {
                return;
            };

            for (index, arg) in args.iter().enumerate() {
                record_observation(
                    state,
                    CandidateKey::TraitMethodParam {
                        trait_item: def_key(cx, trait_item_def_id),
                        index,
                    },
                    classify_option_expr(cx, arg),
                );
            }
        }
        ExprKind::Call(callee, args) => {
            let Some(def_id) = called_def_id(cx, expr, callee) else {
                return;
            };
            let Some(trait_item_def_id) = trait_method_def_id(cx, def_id) else {
                return;
            };
            let assoc_item = cx.tcx.associated_item(def_id);
            let skip = usize::from(assoc_item.is_method());

            for (index, arg) in args.iter().skip(skip).enumerate() {
                record_observation(
                    state,
                    CandidateKey::TraitMethodParam {
                        trait_item: def_key(cx, trait_item_def_id),
                        index,
                    },
                    classify_option_expr(cx, arg),
                );
            }
        }
        _ => {}
    }
}

fn emit_lint<'tcx>(cx: &LateContext<'tcx>, candidate: &Candidate, variant: OptionVariant) {
    let action = match candidate.key {
        CandidateKey::TraitMethodParam { .. } => "passed",
        CandidateKey::TraitConst { .. } => "assigned",
    };
    let help = match (candidate.key.clone(), variant) {
        (CandidateKey::TraitMethodParam { .. }, OptionVariant::Some) => {
            "consider making this parameter non-optional"
        }
        (CandidateKey::TraitMethodParam { .. }, OptionVariant::None) => {
            "consider removing this parameter or using a clearer API for absence"
        }
        (CandidateKey::TraitConst { .. }, OptionVariant::Some) => {
            "consider making this associated const non-optional"
        }
        (CandidateKey::TraitConst { .. }, OptionVariant::None) => {
            "consider removing this associated const or using a clearer sentinel"
        }
    };

    span_lint_and_help(
        cx,
        TRAIT_OPTION_SINGLE_VARIANT,
        candidate.span,
        format!(
            "{} has type `Option<_>` but is only ever {action} `{}` across the workspace",
            candidate.description,
            variant.as_str(),
        ),
        None,
        help,
    );
}

fn called_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    callee: &'tcx Expr<'tcx>,
) -> Option<rustc_span::def_id::DefId> {
    cx.typeck_results()
        .type_dependent_def_id(expr.hir_id)
        .or_else(|| match callee.kind {
            ExprKind::Path(qpath) => cx.qpath_res(&qpath, callee.hir_id).opt_def_id(),
            _ => None,
        })
}

fn trait_method_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> Option<rustc_span::def_id::DefId> {
    let assoc_item = cx.tcx.opt_associated_item(def_id)?;
    if !assoc_item.is_fn() {
        return None;
    }
    if assoc_item.trait_container(cx.tcx).is_none() && assoc_item.trait_item_def_id().is_none() {
        return None;
    }
    assoc_item.trait_item_or_self().ok()
}

fn classify_option_expr<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> ObservedValue {
    let expr = peel_blocks(expr);
    match expr.kind {
        ExprKind::Call(callee, args) if args.len() == 1 => {
            let ExprKind::Path(qpath) = callee.kind else {
                return ObservedValue::Other;
            };
            let Some(def_id) = cx.qpath_res(&qpath, callee.hir_id).opt_def_id() else {
                return ObservedValue::Other;
            };
            match option_variant_for_def_id(cx, def_id) {
                Some(OptionVariant::Some) => ObservedValue::Some,
                Some(OptionVariant::None) | None => ObservedValue::Other,
            }
        }
        ExprKind::Path(qpath) => {
            let Some(def_id) = cx.qpath_res(&qpath, expr.hir_id).opt_def_id() else {
                return ObservedValue::Other;
            };
            match option_variant_for_def_id(cx, def_id) {
                Some(OptionVariant::None) => ObservedValue::None,
                Some(OptionVariant::Some) | None => ObservedValue::Other,
            }
        }
        _ => ObservedValue::Other,
    }
}

fn peel_blocks<'tcx>(mut expr: &'tcx Expr<'tcx>) -> &'tcx Expr<'tcx> {
    loop {
        match expr.kind {
            ExprKind::Block(block, _) if block.stmts.is_empty() => {
                let Some(inner) = block.expr else {
                    return expr;
                };
                expr = inner;
            }
            _ => return expr,
        }
    }
}

fn option_variant_for_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> Option<OptionVariant> {
    let variant_def_id = match cx.tcx.def_kind(def_id) {
        DefKind::Ctor(..) => cx.tcx.parent(def_id),
        _ => def_id,
    };
    let parent = cx.tcx.parent(variant_def_id);
    if !cx.tcx.is_diagnostic_item(sym::Option, parent) {
        return None;
    }

    match cx.tcx.item_name(variant_def_id) {
        sym::Some => Some(OptionVariant::Some),
        sym::None => Some(OptionVariant::None),
        _ => None,
    }
}

fn is_option_ty<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    is_type_diagnostic_item(cx, ty, sym::Option)
}

fn record_observation(state: &mut LintState, key: CandidateKey, value: ObservedValue) {
    state.observations.push(ObservationRecord { key, value });
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
                key: candidate.key.clone(),
                display_path: candidate.display_path.clone(),
                description: candidate.description.clone(),
                file,
                line,
                column,
            })
        })
        .collect::<Vec<_>>();

    let artifact = TargetArtifact {
        crate_stable_id: state.crate_stable_id.clone(),
        crate_name: state.crate_name.clone(),
        target_name: state.target_name.clone(),
        target_kind: state.target_kind.clone(),
        candidates,
        observations: state.observations.clone(),
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

impl OptionVariant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Some => "Some",
            Self::None => "None",
        }
    }
}
