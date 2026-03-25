#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::def::Res;
use rustc_hir::intravisit::FnKind;
use rustc_hir::{Body, Expr, ExprKind, Item, ItemKind, PatKind, QPath, UseKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Visibility;
use rustc_span::Span;

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns on two forms of trivial indirection:
    ///
    /// 1. **Trivial wrapper modules** — a `mod` whose only visible item is a
    ///    single `pub use` re-export.
    /// 2. **Trivial forwarding functions/methods** — a function or method whose
    ///    body is solely a call to another function/method, forwarding every
    ///    parameter in the same order.
    ///
    /// ### Why is this bad?
    ///
    /// Both patterns add a layer of indirection that contributes no logic,
    /// making the code harder to navigate without providing abstraction value.
    /// The wrapper module can be replaced by the `pub use` at the parent scope,
    /// and the forwarding function can be replaced by calling the target
    /// directly (or using a type alias / re-export).
    ///
    /// ### Example
    ///
    /// ```rust
    /// mod foo {
    ///     pub use bar::Baz;
    /// }
    ///
    /// fn add(x: i32, y: i32) -> i32 {
    ///     other::add(x, y)
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// pub use bar::Baz;
    ///
    /// // Call `other::add` directly at call sites.
    /// ```
    pub TRIVIAL_FORWARDING,
    Warn,
    "trivial wrapper module or forwarding function that adds no logic"
}

impl<'tcx> LateLintPass<'tcx> for TrivialForwarding {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        check_trivial_wrapper_module(cx, item);
    }

    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        kind: FnKind<'tcx>,
        _decl: &'tcx rustc_hir::FnDecl<'tcx>,
        body: &'tcx Body<'tcx>,
        span: Span,
        _def_id: rustc_hir::def_id::LocalDefId,
    ) {
        check_trivial_forwarding_fn(cx, kind, body, span);
    }
}

// ---------------------------------------------------------------------------
// Pattern 1: trivial wrapper modules
// ---------------------------------------------------------------------------

fn check_trivial_wrapper_module<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
    if item.span.from_expansion() {
        return;
    }

    let ItemKind::Mod(_, mod_) = item.kind else {
        return;
    };

    // Must contain exactly one item.
    if mod_.item_ids.len() != 1 {
        return;
    }

    let inner = cx.tcx.hir_item(mod_.item_ids[0]);

    // The single item must be a non-glob `use`.
    let ItemKind::Use(_, UseKind::Single(_)) = inner.kind else {
        return;
    };

    // The use must be `pub`.
    if !matches!(cx.tcx.visibility(inner.owner_id), Visibility::Public) {
        return;
    }

    span_lint_and_help(
        cx,
        TRIVIAL_FORWARDING,
        item.span,
        "module contains only a single `pub use` re-export",
        None,
        "consider replacing this module with the `pub use` at the parent scope",
    );
}

// ---------------------------------------------------------------------------
// Pattern 2: trivial forwarding functions / methods
// ---------------------------------------------------------------------------

fn check_trivial_forwarding_fn<'tcx>(
    cx: &LateContext<'tcx>,
    kind: FnKind<'tcx>,
    body: &'tcx Body<'tcx>,
    span: Span,
) {
    // Only named functions/methods.
    if matches!(kind, FnKind::Closure) {
        return;
    }
    if span.from_expansion() {
        return;
    }

    // The body must be a single tail expression with no statements.
    let expr = peel_blocks_checking_stmts(body.value);
    let Some(expr) = expr else {
        return;
    };

    // If the function is not unsafe but the body wraps the call in an unsafe
    // block, the wrapper is adding safety value — don't flag.
    if !fn_is_unsafe(&kind) && contains_unsafe_block(body.value) {
        return;
    }

    let params = body.params;

    match expr.kind {
        ExprKind::Call(_, call_args) => {
            // Free-function-style call: all params forwarded 1:1.
            if call_args.len() != params.len() {
                return;
            }
            for (param, arg) in params.iter().zip(call_args.iter()) {
                if !arg_matches_param(param, arg) {
                    return;
                }
            }
        }
        ExprKind::MethodCall(_, receiver, call_args, _) => {
            let is_method = matches!(kind, FnKind::Method(..));
            if is_method {
                // params[0] is self.
                if params.is_empty() {
                    return;
                }
                if !receiver_is_rooted_at_param(&params[0], receiver) {
                    return;
                }
                let non_self_params = &params[1..];
                if call_args.len() != non_self_params.len() {
                    return;
                }
                for (param, arg) in non_self_params.iter().zip(call_args.iter()) {
                    if !arg_matches_param(param, arg) {
                        return;
                    }
                }
            } else {
                // Free function whose body is a method call on the first param.
                if params.is_empty() {
                    return;
                }
                if !receiver_is_rooted_at_param(&params[0], receiver) {
                    return;
                }
                let remaining = &params[1..];
                if call_args.len() != remaining.len() {
                    return;
                }
                for (param, arg) in remaining.iter().zip(call_args.iter()) {
                    if !arg_matches_param(param, arg) {
                        return;
                    }
                }
            }
        }
        _ => return,
    }

    let name = fn_kind_name(&kind);
    span_lint_and_help(
        cx,
        TRIVIAL_FORWARDING,
        span,
        format!("{name} body is a trivial forwarding call"),
        None,
        "consider removing this wrapper and calling the target directly, or using a re-export",
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Peel nested blocks that have **no statements** and return the inner tail
/// expression. Returns `None` if any block along the way contains statements.
fn peel_blocks_checking_stmts<'tcx>(mut expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    loop {
        match expr.kind {
            ExprKind::Block(block, _) => {
                if !block.stmts.is_empty() {
                    return None;
                }
                match block.expr {
                    Some(inner) => expr = inner,
                    None => return None,
                }
            }
            _ => return Some(expr),
        }
    }
}

/// Check whether a call argument is the exact same binding as the parameter.
fn arg_matches_param(param: &rustc_hir::Param<'_>, arg: &Expr<'_>) -> bool {
    let PatKind::Binding(_, param_hir_id, _, _) = param.pat.kind else {
        return false;
    };
    let arg = peel_borrows(arg);
    match arg.kind {
        ExprKind::Path(QPath::Resolved(_, path)) => {
            matches!(path.res, Res::Local(hir_id) if hir_id == param_hir_id)
        }
        _ => false,
    }
}

/// Check whether `receiver` is rooted at the given parameter, possibly through
/// a chain of field accesses (e.g. `self.inner.field`).
fn receiver_is_rooted_at_param(param: &rustc_hir::Param<'_>, receiver: &Expr<'_>) -> bool {
    let PatKind::Binding(_, param_hir_id, _, _) = param.pat.kind else {
        return false;
    };
    let root = peel_field_accesses(receiver);
    let root = peel_borrows(root);
    match root.kind {
        ExprKind::Path(QPath::Resolved(_, path)) => {
            matches!(path.res, Res::Local(hir_id) if hir_id == param_hir_id)
        }
        _ => false,
    }
}

/// Strip `&`, `&mut`, and `*` wrappers from an expression.
fn peel_borrows<'tcx>(mut expr: &'tcx Expr<'tcx>) -> &'tcx Expr<'tcx> {
    loop {
        match expr.kind {
            ExprKind::AddrOf(_, _, inner) | ExprKind::Unary(rustc_hir::UnOp::Deref, inner) => {
                expr = inner;
            }
            _ => return expr,
        }
    }
}

/// Walk through `.field` chains to find the root expression.
fn peel_field_accesses<'tcx>(mut expr: &'tcx Expr<'tcx>) -> &'tcx Expr<'tcx> {
    loop {
        match expr.kind {
            ExprKind::Field(base, _) => expr = base,
            _ => return expr,
        }
    }
}

/// Check if the function kind is `unsafe`.
fn fn_is_unsafe(kind: &FnKind<'_>) -> bool {
    kind.header().map_or(false, |h| {
        matches!(
            h.safety,
            rustc_hir::HeaderSafety::Normal(rustc_hir::Safety::Unsafe)
        )
    })
}

/// Check if any block in the expression chain is an `unsafe` block.
fn contains_unsafe_block(mut expr: &Expr<'_>) -> bool {
    loop {
        match expr.kind {
            ExprKind::Block(block, _) => {
                if matches!(block.rules, rustc_hir::BlockCheckMode::UnsafeBlock(_)) {
                    return true;
                }
                match block.expr {
                    Some(inner) if block.stmts.is_empty() => expr = inner,
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
}

fn fn_kind_name(kind: &FnKind<'_>) -> String {
    match kind {
        FnKind::ItemFn(ident, ..) => format!("function `{}`", ident.name),
        FnKind::Method(ident, ..) => format!("method `{}`", ident.name),
        FnKind::Closure => "closure".to_string(),
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
