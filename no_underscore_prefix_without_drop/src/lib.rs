#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use clippy_utils::diagnostics::span_lint_and_note;
use rustc_hir::{Item, ItemKind, VariantData};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Ty;

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when a binding is prefixed with `_` (e.g. `_value`) but its type
    /// does not implement `Drop`. The `_` prefix on a named binding is
    /// conventionally used to suppress "unused variable" warnings while still
    /// keeping the value alive until end-of-scope — which only matters when
    /// the type has a `Drop` implementation (e.g. lock guards, file handles).
    ///
    /// ### Why is this bad?
    ///
    /// If the type does not implement `Drop`, prefixing with `_` has no
    /// semantic effect beyond silencing the unused-variable warning. This can
    /// mislead readers into thinking the binding is kept alive for its
    /// destructor side-effects. Prefer removing the binding entirely or
    /// removing the `_` prefix and using the value.
    ///
    /// ### Example
    ///
    /// ```rust
    /// fn process(_count: u32) {
    ///     // _count has no Drop impl — the `_` prefix is misleading
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// fn process(count: u32) {
    ///     // use count, or remove the parameter
    /// }
    /// ```
    pub NO_UNDERSCORE_PREFIX_WITHOUT_DROP,
    Warn,
    "`_`-prefixed binding on a type that does not implement `Drop`"
}

/// Returns `true` if `ty` has a significant (non-dealloc) `Drop` impl.
///
/// This uses rustc's built-in `has_significant_drop` query, which returns
/// `false` for types marked `#[rustc_insignificant_dtor]` (Vec, String,
/// Box, HashMap, etc.) and `true` for types with meaningful destructors
/// (MutexGuard, file handles, etc.).
fn has_significant_drop<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> bool {
    ty.has_significant_drop(cx.tcx, cx.typing_env())
}

/// Returns `true` if the name starts with `_` followed by at least one more character.
/// A bare `_` is the wildcard pattern and is not flagged.
fn is_underscore_prefixed(name: &str) -> bool {
    name.starts_with('_') && name.len() > 1
}

impl<'tcx> LateLintPass<'tcx> for NoUnderscorePrefixWithoutDrop {
    // Check function / method parameters
    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        _kind: rustc_hir::intravisit::FnKind<'tcx>,
        _decl: &'tcx rustc_hir::FnDecl<'tcx>,
        body: &'tcx rustc_hir::Body<'tcx>,
        _span: rustc_span::Span,
        _def_id: rustc_hir::def_id::LocalDefId,
    ) {
        for param in body.params.iter() {
            // Only look at simple identifier patterns
            if let rustc_hir::PatKind::Binding(_, hir_id, ident, _) = param.pat.kind {
                let name = ident.name.as_str();
                if is_underscore_prefixed(name) {
                    let ty = cx.typeck_results().node_type(hir_id);
                    if !has_significant_drop(cx, ty) {
                        span_lint_and_note(
                            cx,
                            NO_UNDERSCORE_PREFIX_WITHOUT_DROP,
                            param.pat.span,
                            format!(
                                "`_`-prefixed binding `{}` has type `{}` which does not implement `Drop`",
                                name, ty,
                            ),
                            None,
                            "the `_` prefix is conventionally used to keep a value alive for its \
                             destructor; consider removing the prefix or the binding",
                        );
                    }
                }
            }
        }
    }

    // Check struct fields
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        let fields = match item.kind {
            ItemKind::Struct(_, _, VariantData::Struct { fields, .. }) => fields,
            ItemKind::Struct(_, _, VariantData::Tuple(fields, ..)) => fields,
            _ => return,
        };

        for field in fields {
            let name = field.ident.name.as_str();
            if is_underscore_prefixed(name) {
                let ty = cx.tcx.type_of(field.def_id).instantiate_identity();
                if !has_significant_drop(cx, ty) {
                    span_lint_and_note(
                        cx,
                        NO_UNDERSCORE_PREFIX_WITHOUT_DROP,
                        field.span,
                        format!(
                            "`_`-prefixed field `{}` has type `{}` which does not implement `Drop`",
                            name, ty,
                        ),
                        None,
                        "the `_` prefix is conventionally used to keep a value alive for its \
                         destructor; consider removing the prefix or the field",
                    );
                }
            }
        }
    }

    // Check trait method signatures (handled by check_fn for provided methods)
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
