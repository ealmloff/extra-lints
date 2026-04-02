#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_span;

use std::{cell::RefCell, collections::BTreeSet};

use agent_lint_utils::workspace_lint::{CrateInfo, LintEnvConfig, Mode, write_artifact_file};
use agent_lint_utils::{DefKey, def_key, normalized_def_path, span_location};
use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{
    Expr, ExprKind, FnDecl, FnRetTy, GenericArg, GenericBound, ImplItem, ImplItemKind, Item,
    ItemKind, MutTy, Node, Path, PolyTraitRef, QPath, TraitRef, Ty, TyKind,
};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;
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

const ENV_CONFIG: LintEnvConfig = LintEnvConfig {
    prefix: "UNUSED_PUBLIC_ITEMS",
};

thread_local! {
    static STATE: RefCell<LintState> = RefCell::new(LintState::default());
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

#[derive(Debug, Clone)]
struct Candidate {
    def_key: DefKey,
    kind: &'static str,
    display_path: String,
    span: Span,
    required_defs: BTreeSet<DefKey>,
}

#[derive(Debug, Clone)]
struct LintState {
    mode: Mode<BTreeSet<DefKey>>,
    info: CrateInfo,
    candidates: Vec<Candidate>,
    uses: BTreeSet<DefKey>,
}

impl Default for LintState {
    fn default() -> Self {
        Self {
            mode: Mode::Disabled,
            info: CrateInfo::default(),
            candidates: Vec::new(),
            uses: BTreeSet::new(),
        }
    }
}

impl LintState {
    fn for_crate<'tcx>(cx: &LateContext<'tcx>) -> Self {
        Self {
            mode: Mode::from_env(&ENV_CONFIG, |report: AggregatedReport| {
                report.unused.into_iter().map(|entry| entry.def_key).collect()
            }),
            info: CrateInfo::for_current_crate(cx),
            candidates: Vec::new(),
            uses: BTreeSet::new(),
        }
    }

    fn enabled(&self) -> bool {
        !self.mode.is_disabled()
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
        let maybe_def_id = match expr.kind {
            ExprKind::MethodCall(..) | ExprKind::Field(..) => {
                cx.typeck_results().type_dependent_def_id(expr.hir_id)
            }
            ExprKind::Call(callee, ..) => match callee.kind {
                ExprKind::Path(QPath::TypeRelative(..)) => {
                    cx.typeck_results().type_dependent_def_id(callee.hir_id)
                }
                _ => None,
            },
            ExprKind::Path(QPath::TypeRelative(..)) => {
                cx.typeck_results().type_dependent_def_id(expr.hir_id)
            }
            _ => None,
        };

        let Some(def_id) = maybe_def_id else {
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
                    if let Err(error) = write_artifact(cx, &state, artifact_dir) {
                        cx.tcx.sess.dcx().warn(format!(
                            "unused_public_items_in_workspace failed to write artifact: {error}"
                        ));
                    }
                }
                Mode::Emit { data: ref unused } => {
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
        _ => None,
    };

    if let Some(kind) = kind {
        state.candidates.push(Candidate {
            def_key: def_key(cx, item.owner_id.to_def_id()),
            kind,
            display_path: normalized_def_path(cx, item.owner_id.to_def_id()),
            span: item.span,
            required_defs: required_defs_for_item(cx, item),
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
        let mut required_defs = BTreeSet::new();
        required_defs.insert(def_key(cx, self_def_id));
        required_defs.extend(required_defs_for_impl_item(cx, impl_item));

        state.candidates.push(Candidate {
            def_key: def_key(cx, impl_item.owner_id.to_def_id()),
            kind,
            display_path: normalized_def_path(cx, impl_item.owner_id.to_def_id()),
            span: impl_item.span,
            required_defs,
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
    artifact_dir: &std::path::Path,
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
                required_defs: candidate.required_defs.iter().cloned().collect(),
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
        crate_stable_id: state.info.crate_stable_id.clone(),
        crate_name: state.info.crate_name.clone(),
        target_name: state.info.target_name.clone(),
        target_kind: state.info.target_kind.clone(),
        candidates,
        uses,
    };

    write_artifact_file(
        &artifact,
        &state.info.crate_name,
        &state.info.target_name,
        artifact_dir,
    )
}

fn required_defs_for_item<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) -> BTreeSet<DefKey> {
    let mut required_defs = BTreeSet::new();

    match item.kind {
        ItemKind::Fn { sig, .. } => collect_fn_sig_required_defs(cx, sig.decl, &mut required_defs),
        ItemKind::Const(_, _, ty, ..) => {
            collect_ty_required_defs(cx, ty, &mut required_defs);
        }
        ItemKind::Static(_, _, ty, ..) | ItemKind::TyAlias(_, _, ty) => {
            collect_ty_required_defs(cx, ty, &mut required_defs);
        }
        _ => {}
    }

    required_defs
}

fn required_defs_for_impl_item<'tcx>(
    cx: &LateContext<'tcx>,
    impl_item: &'tcx ImplItem<'tcx>,
) -> BTreeSet<DefKey> {
    let mut required_defs = BTreeSet::new();

    match impl_item.kind {
        ImplItemKind::Fn(sig, ..) => collect_fn_sig_required_defs(cx, sig.decl, &mut required_defs),
        ImplItemKind::Const(ty, ..) | ImplItemKind::Type(ty) => {
            collect_ty_required_defs(cx, ty, &mut required_defs);
        }
    }

    required_defs
}

fn collect_fn_sig_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    decl: &'tcx FnDecl<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    for input in decl.inputs {
        collect_ty_required_defs(cx, input, required_defs);
    }

    if let FnRetTy::Return(output) = decl.output {
        collect_ty_required_defs(cx, output, required_defs);
    }
}

fn collect_ty_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    ty: &'tcx Ty<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    match ty.kind {
        TyKind::Slice(inner) | TyKind::Array(inner, _) => {
            collect_ty_required_defs(cx, inner, required_defs);
        }
        TyKind::Ptr(MutTy { ty: inner, .. }) | TyKind::Ref(_, MutTy { ty: inner, .. }) => {
            collect_ty_required_defs(cx, inner, required_defs);
        }
        TyKind::Tup(tys) => {
            for ty in tys {
                collect_ty_required_defs(cx, ty, required_defs);
            }
        }
        TyKind::Path(qpath) => collect_qpath_required_defs(cx, qpath, required_defs),
        TyKind::TraitObject(bounds, ..) => {
            for bound in bounds {
                collect_poly_trait_ref_required_defs(cx, bound, required_defs);
            }
        }
        TyKind::FnPtr(fn_ptr) => collect_fn_sig_required_defs(cx, fn_ptr.decl, required_defs),
        TyKind::OpaqueDef(opaque) => {
            for bound in opaque.bounds {
                collect_bound_required_defs(cx, bound, required_defs);
            }
        }
        TyKind::TraitAscription(bounds) => {
            for bound in bounds {
                collect_bound_required_defs(cx, bound, required_defs);
            }
        }
        TyKind::UnsafeBinder(unsafe_binder) => {
            collect_ty_required_defs(cx, unsafe_binder.inner_ty, required_defs);
        }
        TyKind::Never
        | TyKind::Infer(_)
        | TyKind::InferDelegation(_, _)
        | TyKind::Err(_)
        | TyKind::Typeof(_)
        | TyKind::Pat(_, _)
        => {}
    }
}

fn collect_bound_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    bound: &'tcx GenericBound<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    if let GenericBound::Trait(poly_trait_ref) = bound {
        collect_poly_trait_ref_required_defs(cx, poly_trait_ref, required_defs);
    }
}

fn collect_poly_trait_ref_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    poly_trait_ref: &'tcx PolyTraitRef<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    collect_trait_ref_required_defs(cx, poly_trait_ref.trait_ref, required_defs);
}

fn collect_trait_ref_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    trait_ref: TraitRef<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    collect_path_required_defs(cx, trait_ref.path, required_defs);
}

fn collect_qpath_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    qpath: QPath<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    match qpath {
        QPath::Resolved(_, path) => collect_path_required_defs(cx, path, required_defs),
        QPath::TypeRelative(ty, segment) => {
            collect_ty_required_defs(cx, ty, required_defs);
            if let Some(args) = segment.args {
                collect_generic_args_required_defs(cx, args.args, required_defs);
            }
        }
        QPath::LangItem(..) => {}
    }
}

fn collect_path_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    path: &Path<'tcx>,
    required_defs: &mut BTreeSet<DefKey>,
) {
    if let Some(def_id) = path.res.opt_def_id()
        && local_public_signature_def(cx, def_id)
    {
        required_defs.insert(def_key(cx, def_id));
    }

    for segment in path.segments {
        if let Some(args) = segment.args {
            collect_generic_args_required_defs(cx, args.args, required_defs);
        }
    }
}

fn local_public_signature_def<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> bool {
    if !def_id.is_local() {
        return false;
    }

    let visibility_supported = matches!(
        cx.tcx.def_kind(def_id),
        rustc_hir::def::DefKind::Struct
            | rustc_hir::def::DefKind::Union
            | rustc_hir::def::DefKind::Enum
            | rustc_hir::def::DefKind::Trait
            | rustc_hir::def::DefKind::TraitAlias
            | rustc_hir::def::DefKind::TyAlias
            | rustc_hir::def::DefKind::AssocTy
    );

    visibility_supported && cx.tcx.visibility(def_id).is_public()
}

fn collect_generic_args_required_defs<'tcx>(
    cx: &LateContext<'tcx>,
    args: &'tcx [GenericArg<'tcx>],
    required_defs: &mut BTreeSet<DefKey>,
) {
    for arg in args {
        if let GenericArg::Type(ty) = arg {
            collect_ty_required_defs(cx, ty.as_unambig_ty(), required_defs);
        }
    }
}
