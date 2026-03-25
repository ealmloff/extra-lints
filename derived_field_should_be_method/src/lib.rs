#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_hir;
extern crate rustc_span;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use agent_lint_utils::workspace_lint::{CrateInfo, LintEnvConfig, Mode, write_artifact_file};
use agent_lint_utils::{DefKey, def_key, normalized_def_path, span_location};
use clippy_utils::diagnostics::span_lint_and_help;
use rustc_ast::LitKind;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{
    Expr, ExprKind, Item, ItemKind, QPath, StructTailExpr, VariantData,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;
use serde::{Deserialize, Serialize};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when a struct field is always initialized to the same expression
    /// across all construction sites in the workspace. The expression may be a
    /// constant literal or a computation derived from other fields in the same
    /// struct literal.
    ///
    /// ### Why is this bad?
    ///
    /// A field that is always derived from other fields adds redundant storage
    /// and a maintenance burden: every constructor must remember to compute it
    /// correctly. Such a field is better expressed as a method (for derived
    /// values) or a constant (for fixed values).
    ///
    /// ### Known problems
    ///
    /// This lint is intentionally conservative. Construction sites using struct
    /// update syntax (`..default`), macro-generated code, or expressions too
    /// complex to normalize are silently ignored rather than producing false
    /// positives. Fields with only a single observed construction site are not
    /// flagged.
    pub DERIVED_FIELD_SHOULD_BE_METHOD,
    Warn,
    "struct field is always initialized to the same expression and could be a method"
}

const ENV_CONFIG: LintEnvConfig = LintEnvConfig {
    prefix: "DERIVED_FIELD_SHOULD_BE_METHOD",
};

thread_local! {
    static STATE: RefCell<LintState> = RefCell::new(LintState::default());
}

// ---------------------------------------------------------------------------
// Serializable data types (shared with the CLI aggregation binary)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FieldKey {
    struct_def: DefKey,
    field_name: String,
}

/// A normalized, serializable representation of an expression where references
/// to sibling struct fields have been replaced with symbolic names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NormalizedExpr {
    /// Reference to another field in the same struct literal.
    SiblingField(String),
    /// Integer literal.
    LitInt(i128),
    /// Floating-point literal (string representation for equality).
    LitFloat(String),
    /// Boolean literal.
    LitBool(bool),
    /// String literal.
    LitStr(String),
    /// Char literal.
    LitChar(char),
    /// Binary operation (operator as Debug string).
    BinOp(String, Box<NormalizedExpr>, Box<NormalizedExpr>),
    /// Unary operation (operator as Debug string).
    UnaryOp(String, Box<NormalizedExpr>),
    /// Method call: first element is receiver, rest are arguments.
    MethodCall(String, Vec<NormalizedExpr>),
    /// Free function / path call.
    FnCall(String, Vec<NormalizedExpr>),
    /// Field access on an expression.
    FieldAccess(Box<NormalizedExpr>, String),
    /// Tuple literal.
    Tuple(Vec<NormalizedExpr>),
    /// Cannot be normalized — causes the whole observation to be skipped.
    Opaque,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateRecord {
    key: FieldKey,
    display_path: String,
    file: String,
    line: u32,
    column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservationRecord {
    key: FieldKey,
    expr: NormalizedExpr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetArtifact {
    crate_stable_id: String,
    crate_name: String,
    target_name: String,
    target_kind: String,
    candidates: Vec<CandidateRecord>,
    observations: Vec<ObservationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportEntry {
    pub key: FieldKey,
    pub display_path: String,
    pub expr: NormalizedExpr,
    pub construction_site_count: usize,
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedReport {
    pub redundant_fields: Vec<ReportEntry>,
}

// ---------------------------------------------------------------------------
// In-memory lint state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Candidate {
    key: FieldKey,
    display_path: String,
    span: Span,
}

#[derive(Debug, Clone)]
struct LintState {
    mode: Mode<BTreeMap<FieldKey, ReportEntry>>,
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
                    .redundant_fields
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

// ---------------------------------------------------------------------------
// LateLintPass implementation
// ---------------------------------------------------------------------------

impl<'tcx> LateLintPass<'tcx> for DerivedFieldShouldBeMethod {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| *state.borrow_mut() = LintState::for_crate(cx));
    }

    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            register_struct_field_candidates(cx, item, &mut state);
        });
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            maybe_record_struct_literal(cx, expr, &mut state);
        });
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| {
            let state = std::mem::take(&mut *state.borrow_mut());
            match state.mode {
                Mode::Collect { ref artifact_dir } => {
                    if let Err(error) = write_artifact(cx, &state, artifact_dir) {
                        cx.tcx.sess.dcx().warn(format!(
                            "derived_field_should_be_method failed to write artifact: {error}"
                        ));
                    }
                }
                Mode::Emit { data: ref flagged } => {
                    for candidate in &state.candidates {
                        let Some(entry) = flagged.get(&candidate.key) else {
                            continue;
                        };
                        emit_lint(cx, candidate, entry);
                    }
                }
                Mode::Disabled => {}
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Candidate registration: record struct field definitions
// ---------------------------------------------------------------------------

fn register_struct_field_candidates<'tcx>(
    cx: &LateContext<'tcx>,
    item: &'tcx Item<'tcx>,
    state: &mut LintState,
) {
    let ItemKind::Struct(_, _, variant_data) = item.kind else {
        return;
    };
    let VariantData::Struct { fields, .. } = variant_data else {
        return;
    };
    if item.span.from_expansion() {
        return;
    }

    let struct_def_id = item.owner_id.to_def_id();
    let struct_key = def_key(cx, struct_def_id);
    let struct_path = normalized_def_path(cx, struct_def_id);

    for field in fields {
        let field_name = field.ident.name.to_string();
        let key = FieldKey {
            struct_def: struct_key.clone(),
            field_name: field_name.clone(),
        };
        state.candidates.push(Candidate {
            key,
            display_path: format!("{struct_path}::{field_name}"),
            span: field.span,
        });
    }
}

// ---------------------------------------------------------------------------
// Observation recording: analyze struct literal expressions
// ---------------------------------------------------------------------------

fn maybe_record_struct_literal<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    state: &mut LintState,
) {
    let ExprKind::Struct(qpath, fields, base) = expr.kind else {
        return;
    };

    // Skip struct update syntax — can't observe all fields.
    if !matches!(base, StructTailExpr::None) {
        return;
    }

    // Skip macro-expanded code.
    if expr.span.from_expansion() {
        return;
    }

    // Resolve the struct DefId.
    let Some(struct_def_id) = resolve_struct_def_id(cx, qpath, expr) else {
        return;
    };

    let struct_key = def_key(cx, struct_def_id);

    // Build local-variable → field-name mapping.
    let local_to_field = build_local_to_field_map(fields);

    for field in fields {
        let field_name = field.ident.name.to_string();
        let key = FieldKey {
            struct_def: struct_key.clone(),
            field_name,
        };
        let normalized = normalize_expr(cx, field.expr, &local_to_field);
        state.observations.push(ObservationRecord {
            key,
            expr: normalized,
        });
    }
}

fn resolve_struct_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    qpath: &QPath<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<rustc_span::def_id::DefId> {
    let res = cx.qpath_res(qpath, expr.hir_id);
    match res {
        Res::Def(DefKind::Struct, def_id) => Some(def_id),
        _ => None,
    }
}

/// Build a mapping from local variable HirId to the struct field name it
/// directly initializes.
///
/// For `Foo { width: w, height: h, area: w * h }`:
///   HirId(w) → "width", HirId(h) → "height"
fn build_local_to_field_map<'tcx>(
    fields: &[rustc_hir::ExprField<'tcx>],
) -> HashMap<rustc_hir::HirId, String> {
    let mut map = HashMap::new();
    for field in fields {
        let init = peel_blocks(field.expr);
        if let ExprKind::Path(QPath::Resolved(_, path)) = init.kind {
            if let Res::Local(hir_id) = path.res {
                map.insert(hir_id, field.ident.name.to_string());
            }
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Expression normalization
// ---------------------------------------------------------------------------

fn normalize_expr<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    local_to_field: &HashMap<rustc_hir::HirId, String>,
) -> NormalizedExpr {
    let expr = peel_blocks(expr);

    match expr.kind {
        // Literals
        ExprKind::Lit(ref lit) => normalize_lit(lit),

        // Local variable reference
        ExprKind::Path(QPath::Resolved(_, path)) => {
            if let Res::Local(hir_id) = path.res {
                if let Some(field_name) = local_to_field.get(&hir_id) {
                    return NormalizedExpr::SiblingField(field_name.clone());
                }
            }
            NormalizedExpr::Opaque
        }

        // Non-local paths (e.g., constants, statics, functions used as values)
        ExprKind::Path(_) => NormalizedExpr::Opaque,

        // Binary operation
        ExprKind::Binary(op, lhs, rhs) => {
            let l = normalize_expr(cx, lhs, local_to_field);
            let r = normalize_expr(cx, rhs, local_to_field);
            if l == NormalizedExpr::Opaque || r == NormalizedExpr::Opaque {
                return NormalizedExpr::Opaque;
            }
            NormalizedExpr::BinOp(format!("{:?}", op.node), Box::new(l), Box::new(r))
        }

        // Unary operation
        ExprKind::Unary(op, inner) => {
            let n = normalize_expr(cx, inner, local_to_field);
            if n == NormalizedExpr::Opaque {
                return NormalizedExpr::Opaque;
            }
            NormalizedExpr::UnaryOp(format!("{op:?}"), Box::new(n))
        }

        // Method call: receiver.method(args...)
        ExprKind::MethodCall(method, receiver, args, _) => {
            let recv = normalize_expr(cx, receiver, local_to_field);
            if recv == NormalizedExpr::Opaque {
                return NormalizedExpr::Opaque;
            }
            let mut normalized_args = vec![recv];
            for arg in args {
                let n = normalize_expr(cx, arg, local_to_field);
                if n == NormalizedExpr::Opaque {
                    return NormalizedExpr::Opaque;
                }
                normalized_args.push(n);
            }
            NormalizedExpr::MethodCall(method.ident.name.to_string(), normalized_args)
        }

        // Function call: path(args...)
        ExprKind::Call(callee, args) => {
            let callee_name = match callee.kind {
                ExprKind::Path(ref qpath) => cx
                    .qpath_res(qpath, callee.hir_id)
                    .opt_def_id()
                    .map(|did| cx.tcx.def_path_str(did)),
                _ => None,
            };
            let Some(name) = callee_name else {
                return NormalizedExpr::Opaque;
            };
            let mut normalized_args = Vec::with_capacity(args.len());
            for arg in args {
                let n = normalize_expr(cx, arg, local_to_field);
                if n == NormalizedExpr::Opaque {
                    return NormalizedExpr::Opaque;
                }
                normalized_args.push(n);
            }
            NormalizedExpr::FnCall(name, normalized_args)
        }

        // Field access: expr.field
        ExprKind::Field(base_expr, ident) => {
            let base = normalize_expr(cx, base_expr, local_to_field);
            if base == NormalizedExpr::Opaque {
                return NormalizedExpr::Opaque;
            }
            NormalizedExpr::FieldAccess(Box::new(base), ident.name.to_string())
        }

        // Tuple
        ExprKind::Tup(exprs) => {
            let mut items = Vec::with_capacity(exprs.len());
            for e in exprs {
                let n = normalize_expr(cx, e, local_to_field);
                if n == NormalizedExpr::Opaque {
                    return NormalizedExpr::Opaque;
                }
                items.push(n);
            }
            NormalizedExpr::Tuple(items)
        }

        // Cast — treat as transparent
        ExprKind::Cast(inner, _) => normalize_expr(cx, inner, local_to_field),

        // Everything else
        _ => NormalizedExpr::Opaque,
    }
}

fn normalize_lit(lit: &rustc_span::source_map::Spanned<LitKind>) -> NormalizedExpr {
    match lit.node {
        LitKind::Int(value, _) => NormalizedExpr::LitInt(value.get() as i128),
        LitKind::Float(symbol, _) => NormalizedExpr::LitFloat(symbol.to_string()),
        LitKind::Bool(b) => NormalizedExpr::LitBool(b),
        LitKind::Str(symbol, _) => NormalizedExpr::LitStr(symbol.to_string()),
        LitKind::Char(c) => NormalizedExpr::LitChar(c),
        _ => NormalizedExpr::Opaque,
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

// ---------------------------------------------------------------------------
// Emit diagnostics
// ---------------------------------------------------------------------------

fn emit_lint<'tcx>(cx: &LateContext<'tcx>, candidate: &Candidate, entry: &ReportEntry) {
    let field_name = &candidate.key.field_name;
    let (kind, help) = classify_redundancy(&entry.expr, field_name);

    span_lint_and_help(
        cx,
        DERIVED_FIELD_SHOULD_BE_METHOD,
        candidate.span,
        format!(
            "field `{}` is always initialized to {} across {} construction sites",
            field_name, kind, entry.construction_site_count,
        ),
        None,
        help,
    );
}

fn classify_redundancy<'a>(
    expr: &NormalizedExpr,
    self_field: &str,
) -> (&'a str, &'a str) {
    if references_other_sibling(expr, self_field) {
        (
            "an expression derived from other fields",
            "consider making this a method instead of a stored field",
        )
    } else {
        (
            "the same constant value",
            "consider making this a const or removing it from the struct",
        )
    }
}

/// Returns `true` if the expression references at least one sibling field
/// *other than* `self_field`. A bare `SiblingField("x")` for field `x` is
/// just field shorthand, not a derivation.
fn references_other_sibling(expr: &NormalizedExpr, self_field: &str) -> bool {
    match expr {
        NormalizedExpr::SiblingField(name) => name != self_field,
        NormalizedExpr::BinOp(_, l, r) => {
            references_other_sibling(l, self_field)
                || references_other_sibling(r, self_field)
        }
        NormalizedExpr::UnaryOp(_, inner) => references_other_sibling(inner, self_field),
        NormalizedExpr::MethodCall(_, args)
        | NormalizedExpr::FnCall(_, args)
        | NormalizedExpr::Tuple(args) => {
            args.iter().any(|a| references_other_sibling(a, self_field))
        }
        NormalizedExpr::FieldAccess(base, _) => references_other_sibling(base, self_field),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Artifact writing
// ---------------------------------------------------------------------------

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

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
