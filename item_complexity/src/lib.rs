#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_span;

use clippy_utils::diagnostics::span_lint;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Body, Expr, Pat, Stmt};
use rustc_lint::{LateContext, LateLintPass};

/// Default threshold for the number of HIR nodes in a function body before
/// the lint fires.
const DEFAULT_THRESHOLD: usize = 250;

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when a function or method body contains more than a configurable
    /// number of HIR (High-level Intermediate Representation) nodes, indicating
    /// excessive complexity.
    ///
    /// ### Why is this bad?
    ///
    /// Functions with too many IR nodes are hard to read, test, and maintain.
    /// Large function bodies suggest the code should be broken into smaller,
    /// more focused helper functions.
    ///
    /// ### Example
    ///
    /// ```rust
    /// fn do_everything() {
    ///     // hundreds of lines of deeply nested logic...
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// fn do_everything() {
    ///     step_one();
    ///     step_two();
    ///     step_three();
    /// }
    /// ```
    pub ITEM_COMPLEXITY,
    Warn,
    "function body contains too many HIR nodes, indicating excessive complexity"
}

impl<'tcx> LateLintPass<'tcx> for ItemComplexity {
    fn check_fn(
        &mut self,
        cx: &LateContext<'tcx>,
        kind: rustc_hir::intravisit::FnKind<'tcx>,
        _decl: &'tcx rustc_hir::FnDecl<'tcx>,
        body: &'tcx Body<'tcx>,
        span: rustc_span::Span,
        _def_id: rustc_hir::def_id::LocalDefId,
    ) {
        // Don't lint generated/macro code
        if span.from_expansion() {
            return;
        }

        let mut counter = NodeCounter { count: 0 };
        counter.visit_body(body);

        if counter.count > DEFAULT_THRESHOLD {
            let name = match kind {
                rustc_hir::intravisit::FnKind::ItemFn(ident, ..) => {
                    format!("function `{}`", ident.name)
                }
                rustc_hir::intravisit::FnKind::Method(ident, ..) => {
                    format!("method `{}`", ident.name)
                }
                rustc_hir::intravisit::FnKind::Closure => "closure".to_string(),
            };

            span_lint(
                cx,
                ITEM_COMPLEXITY,
                span,
                format!(
                    "{name} has {count} HIR nodes (threshold: {DEFAULT_THRESHOLD}); \
                     consider breaking it into smaller functions",
                    count = counter.count,
                ),
            );
        }
    }
}

struct NodeCounter {
    count: usize,
}

impl<'hir> Visitor<'hir> for NodeCounter {
    fn visit_expr(&mut self, expr: &'hir Expr<'hir>) {
        if expr.span.from_expansion() {
            return;
        }
        self.count += 1;
        intravisit::walk_expr(self, expr);
    }

    fn visit_stmt(&mut self, stmt: &'hir Stmt<'hir>) {
        if stmt.span.from_expansion() {
            return;
        }
        self.count += 1;
        intravisit::walk_stmt(self, stmt);
    }

    fn visit_pat(&mut self, pat: &'hir Pat<'hir>) {
        if pat.span.from_expansion() {
            return;
        }
        self.count += 1;
        intravisit::walk_pat(self, pat);
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
