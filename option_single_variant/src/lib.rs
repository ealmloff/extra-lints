#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use std::{cell::RefCell, collections::BTreeMap};

use agent_lint_utils::workspace_lint::{CrateInfo, LintEnvConfig, Mode, write_artifact_file};
use agent_lint_utils::{DefKey, def_key, normalized_def_path, span_location};
use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::ty::is_type_diagnostic_item;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{
    Expr, ExprKind, ImplItem, ImplItemKind, Item, ItemKind, Node, QPath, StructTailExpr,
    TraitItem, TraitItemKind, VariantData,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{self, Ty};
use rustc_span::{Span, Symbol, sym};
use serde::{Deserialize, Serialize};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when an `Option`-typed function parameter, trait-associated const,
    /// or struct field is only ever used with one explicit variant (`Some` or
    /// `None`) across the current workspace.
    ///
    /// ### Why is this bad?
    ///
    /// If every call site passes `Some(...)`, every implementation assigns only
    /// `None`, or every field write uses the same variant, the `Option` is
    /// carrying less information than it suggests. This usually means the API
    /// or data shape can be made clearer with a non-optional value or a more
    /// explicit representation of absence.
    ///
    /// ### Known problems
    ///
    /// This lint is intentionally conservative. Any non-literal `Option` value
    /// at a call site, const assignment, struct initialization, or field write
    /// suppresses the warning rather than guessing through dataflow or constant
    /// evaluation.
    pub OPTION_SINGLE_VARIANT,
    Warn,
    "`Option` API or field only ever uses one variant across the workspace"
}

const ENV_CONFIG: LintEnvConfig = LintEnvConfig {
    prefix: "OPTION_SINGLE_VARIANT",
};

thread_local! {
    static STATE: RefCell<LintState> = RefCell::new(LintState::default());
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

#[derive(Debug, Clone)]
struct Candidate {
    key: CandidateKey,
    display_path: String,
    description: String,
    span: Span,
}

#[derive(Debug, Clone)]
struct LintState {
    mode: Mode<BTreeMap<CandidateKey, ReportEntry>>,
    info: CrateInfo,
    candidates: Vec<Candidate>,
    observations: Vec<ObservationRecord>,
}

impl Default for LintState {
    fn default() -> Self {
        Self {
            mode: Mode::Disabled,
            info: CrateInfo::default(),
            candidates: Vec::new(),
            observations: Vec::new(),
        }
    }
}

impl LintState {
    fn for_crate<'tcx>(cx: &LateContext<'tcx>) -> Self {
        Self {
            mode: Mode::from_env(&ENV_CONFIG, |report: AggregatedReport| {
                report
                    .redundant
                    .into_iter()
                    .map(|entry| (entry.key.clone(), entry))
                    .collect()
            }),
            info: CrateInfo::for_current_crate(cx),
            candidates: Vec::new(),
            observations: Vec::new(),
        }
    }

    fn enabled(&self) -> bool {
        !self.mode.is_disabled()
    }
}

impl<'tcx> LateLintPass<'tcx> for OptionSingleVariant {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| *state.borrow_mut() = LintState::for_crate(cx));
    }

    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_item_candidates(cx, item, &mut state);
        });
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
            maybe_record_impl_item_candidate(cx, impl_item, &mut state);
            maybe_record_trait_const_impl(cx, impl_item, &mut state);
        });
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_expr_observation(cx, expr, &mut state);
        });
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| {
            let state = std::mem::take(&mut *state.borrow_mut());
            match state.mode {
                Mode::Collect { ref artifact_dir } => {
                    if let Err(error) = write_artifact(cx, &state, artifact_dir) {
                        cx.tcx.sess.dcx().warn(format!(
                            "option_single_variant failed to write artifact: {error}"
                        ));
                    }
                }
                Mode::Emit { data: ref redundant } => {
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

fn maybe_record_item_candidates<'tcx>(
    cx: &LateContext<'tcx>,
    item: &'tcx Item<'tcx>,
    state: &mut LintState,
) {
    if item.span.from_expansion() {
        return;
    }

    match item.kind {
        ItemKind::Fn { sig, .. } => {
            register_function_param_candidates(
                cx,
                item.owner_id.to_def_id(),
                sig.decl.inputs,
                "function",
                state,
            );
        }
        ItemKind::Struct(_, _, VariantData::Struct { fields, .. }) => {
            register_struct_field_candidates(cx, item.owner_id.to_def_id(), fields, false, state);
        }
        ItemKind::Struct(_, _, VariantData::Tuple(fields, ..)) => {
            register_struct_field_candidates(cx, item.owner_id.to_def_id(), fields, true, state);
        }
        _ => {}
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
    match trait_item.kind {
        TraitItemKind::Fn(hir_sig, _) => {
            register_function_param_candidates(
                cx,
                trait_item_def_id,
                hir_sig.decl.inputs,
                "trait method",
                state,
            );
        }
        TraitItemKind::Const(_, default) => {
            let display_path = normalized_def_path(cx, trait_item_def_id);
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

fn maybe_record_impl_item_candidate<'tcx>(
    cx: &LateContext<'tcx>,
    impl_item: &'tcx ImplItem<'tcx>,
    state: &mut LintState,
) {
    let ImplItemKind::Fn(sig, _) = impl_item.kind else {
        return;
    };
    if impl_item.span.from_expansion() {
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

    let assoc_item = cx.tcx.associated_item(impl_item.owner_id.to_def_id());
    let kind = if assoc_item.is_method() {
        "method"
    } else {
        "associated function"
    };
    register_function_param_candidates(cx, impl_item.owner_id.to_def_id(), sig.decl.inputs, kind, state);
}

fn register_function_param_candidates<'tcx>(
    cx: &LateContext<'tcx>,
    function_def_id: rustc_span::def_id::DefId,
    hir_inputs: &'tcx [rustc_hir::Ty<'tcx>],
    kind: &str,
    state: &mut LintState,
) {
    let display_path = normalized_def_path(cx, function_def_id);
    let fn_sig = cx
        .tcx
        .fn_sig(function_def_id)
        .instantiate_identity()
        .skip_binder();
    let skip = cx
        .tcx
        .opt_associated_item(function_def_id)
        .map(|assoc_item| usize::from(assoc_item.is_method()))
        .unwrap_or(0);
    let inputs = fn_sig.inputs();

    for (index, hir_ty) in hir_inputs.iter().skip(skip).enumerate() {
        let Some(param_ty) = inputs.get(index + skip).copied() else {
            continue;
        };
        if !is_option_ty(cx, param_ty) {
            continue;
        }

        state.candidates.push(Candidate {
            key: CandidateKey::FunctionParam {
                function: def_key(cx, function_def_id),
                index,
            },
            display_path: display_path.clone(),
            description: format!("parameter #{} of {kind} `{display_path}`", index + 1),
            span: hir_ty.span,
        });
    }
}

fn register_struct_field_candidates<'tcx>(
    cx: &LateContext<'tcx>,
    struct_def_id: rustc_span::def_id::DefId,
    fields: &'tcx [rustc_hir::FieldDef<'tcx>],
    tuple: bool,
    state: &mut LintState,
) {
    let struct_path = normalized_def_path(cx, struct_def_id);
    let struct_key = def_key(cx, struct_def_id);

    for (index, field) in fields.iter().enumerate() {
        let field_ty = cx.tcx.type_of(field.def_id.to_def_id()).instantiate_identity();
        if !is_option_ty(cx, field_ty) {
            continue;
        }

        let (display_path, description) = if tuple {
            (
                format!("{struct_path}::{index}"),
                format!("field #{} of tuple struct `{struct_path}`", index + 1),
            )
        } else {
            let field_name = field.ident.name.to_string();
            (
                format!("{struct_path}::{field_name}"),
                format!("field `{struct_path}::{field_name}`"),
            )
        };

        state.candidates.push(Candidate {
            key: CandidateKey::StructField {
                struct_item: struct_key.clone(),
                index,
            },
            display_path,
            description,
            span: field.span,
        });
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

fn maybe_record_expr_observation<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    state: &mut LintState,
) {
    match expr.kind {
        ExprKind::MethodCall(_, _, args, _) => {
            maybe_record_method_call(cx, expr, args, state);
        }
        ExprKind::Call(callee, args) => {
            if maybe_record_tuple_struct_ctor(cx, expr, callee, args, state) {
                return;
            }
            maybe_record_function_call(cx, expr, callee, args, state);
        }
        ExprKind::Struct(qpath, fields, base) => {
            maybe_record_struct_literal(cx, expr, qpath, fields, base, state);
        }
        ExprKind::Assign(lhs, rhs, _) => {
            maybe_record_struct_field_assignment(cx, lhs, rhs, state);
        }
        _ => {}
    }
}

fn maybe_record_method_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    args: &'tcx [Expr<'tcx>],
    state: &mut LintState,
) {
    let Some(def_id) = cx.typeck_results().type_dependent_def_id(expr.hir_id) else {
        return;
    };
    let Some(function_def_id) = canonical_function_def_id(cx, def_id) else {
        return;
    };

    for (index, arg) in args.iter().enumerate() {
        record_observation(
            state,
            CandidateKey::FunctionParam {
                function: def_key(cx, function_def_id),
                index,
            },
            classify_option_expr(cx, arg),
        );
    }
}

fn maybe_record_function_call<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    callee: &'tcx Expr<'tcx>,
    args: &'tcx [Expr<'tcx>],
    state: &mut LintState,
) {
    let Some(def_id) = called_def_id(cx, expr, callee) else {
        return;
    };
    let Some(function_def_id) = canonical_function_def_id(cx, def_id) else {
        return;
    };
    let skip = cx
        .tcx
        .opt_associated_item(def_id)
        .map(|assoc_item| usize::from(assoc_item.is_method()))
        .unwrap_or(0);

    for (index, arg) in args.iter().skip(skip).enumerate() {
        record_observation(
            state,
            CandidateKey::FunctionParam {
                function: def_key(cx, function_def_id),
                index,
            },
            classify_option_expr(cx, arg),
        );
    }
}

fn maybe_record_tuple_struct_ctor<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    callee: &'tcx Expr<'tcx>,
    args: &'tcx [Expr<'tcx>],
    state: &mut LintState,
) -> bool {
    if expr.span.from_expansion() {
        return false;
    }

    let Some(def_id) = called_def_id(cx, expr, callee) else {
        return false;
    };
    let Some(struct_def_id) = tuple_struct_def_id(cx, def_id) else {
        return false;
    };
    let struct_key = def_key(cx, struct_def_id);

    for (index, arg) in args.iter().enumerate() {
        record_observation(
            state,
            CandidateKey::StructField {
                struct_item: struct_key.clone(),
                index,
            },
            classify_option_expr(cx, arg),
        );
    }

    true
}

fn maybe_record_struct_literal<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    qpath: &QPath<'tcx>,
    fields: &'tcx [rustc_hir::ExprField<'tcx>],
    base: StructTailExpr<'tcx>,
    state: &mut LintState,
) {
    if !matches!(base, StructTailExpr::None) || expr.span.from_expansion() {
        return;
    }

    let Some(struct_def_id) = resolve_struct_def_id(cx, qpath, expr) else {
        return;
    };
    let struct_key = def_key(cx, struct_def_id);

    for field in fields {
        let Some(index) = field_index_by_name(cx, struct_def_id, field.ident.name) else {
            continue;
        };
        record_observation(
            state,
            CandidateKey::StructField {
                struct_item: struct_key.clone(),
                index,
            },
            classify_option_expr(cx, field.expr),
        );
    }
}

fn maybe_record_struct_field_assignment<'tcx>(
    cx: &LateContext<'tcx>,
    lhs: &'tcx Expr<'tcx>,
    rhs: &'tcx Expr<'tcx>,
    state: &mut LintState,
) {
    let lhs = peel_blocks(lhs);
    let ExprKind::Field(base, ident) = lhs.kind else {
        return;
    };
    let Some(struct_def_id) = expr_struct_def_id(cx, base) else {
        return;
    };
    let Some(index) = field_index_by_name(cx, struct_def_id, ident.name) else {
        return;
    };

    record_observation(
        state,
        CandidateKey::StructField {
            struct_item: def_key(cx, struct_def_id),
            index,
        },
        classify_option_expr(cx, rhs),
    );
}

fn emit_lint<'tcx>(cx: &LateContext<'tcx>, candidate: &Candidate, variant: OptionVariant) {
    let action = match candidate.key {
        CandidateKey::FunctionParam { .. } => "passed",
        CandidateKey::TraitConst { .. } => "assigned",
        CandidateKey::StructField { .. } => "set",
    };
    let help = match (candidate.key.clone(), variant) {
        (CandidateKey::FunctionParam { .. }, OptionVariant::Some) => {
            "consider making this parameter non-optional"
        }
        (CandidateKey::FunctionParam { .. }, OptionVariant::None) => {
            "consider removing this parameter or using a clearer API for absence"
        }
        (CandidateKey::TraitConst { .. }, OptionVariant::Some) => {
            "consider making this associated const non-optional"
        }
        (CandidateKey::TraitConst { .. }, OptionVariant::None) => {
            "consider removing this associated const or using a clearer sentinel"
        }
        (CandidateKey::StructField { .. }, OptionVariant::Some) => {
            "consider making this field non-optional"
        }
        (CandidateKey::StructField { .. }, OptionVariant::None) => {
            "consider removing this field or using a clearer sentinel"
        }
    };

    span_lint_and_help(
        cx,
        OPTION_SINGLE_VARIANT,
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

fn canonical_function_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> Option<rustc_span::def_id::DefId> {
    if let Some(assoc_item) = cx.tcx.opt_associated_item(def_id) {
        if !assoc_item.is_fn() {
            return None;
        }
        return assoc_item.trait_item_or_self().ok();
    }

    match cx.tcx.def_kind(def_id) {
        DefKind::Fn => Some(def_id),
        _ => None,
    }
}

fn tuple_struct_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> Option<rustc_span::def_id::DefId> {
    let DefKind::Ctor(..) = cx.tcx.def_kind(def_id) else {
        return None;
    };
    let struct_def_id = cx.tcx.parent(def_id);
    if matches!(cx.tcx.def_kind(struct_def_id), DefKind::Struct) {
        Some(struct_def_id)
    } else {
        None
    }
}

fn resolve_struct_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    qpath: &QPath<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<rustc_span::def_id::DefId> {
    match cx.qpath_res(qpath, expr.hir_id) {
        Res::Def(DefKind::Struct, def_id) => Some(def_id),
        _ => None,
    }
}

fn expr_struct_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<rustc_span::def_id::DefId> {
    match cx.typeck_results().expr_ty(expr).peel_refs().kind() {
        ty::Adt(adt_def, _) if adt_def.is_struct() => Some(adt_def.did()),
        _ => None,
    }
}

fn field_index_by_name<'tcx>(
    cx: &LateContext<'tcx>,
    struct_def_id: rustc_span::def_id::DefId,
    field_name: Symbol,
) -> Option<usize> {
    let adt_def = cx.tcx.adt_def(struct_def_id);
    adt_def
        .non_enum_variant()
        .fields
        .iter()
        .position(|field| field.name == field_name)
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
    artifact_dir: &std::path::Path,
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
        crate_stable_id: state.info.crate_stable_id.clone(),
        crate_name: state.info.crate_name.clone(),
        target_name: state.info.target_name.clone(),
        target_kind: state.info.target_kind.clone(),
        candidates,
        observations: state.observations.clone(),
    };

    write_artifact_file(
        &artifact,
        &state.info.crate_name,
        &state.info.target_name,
        artifact_dir,
    )
}

impl OptionVariant {
    fn as_str(self) -> &'static str {
        match self {
            Self::Some => "Some",
            Self::None => "None",
        }
    }
}
