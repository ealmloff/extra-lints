#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;
extern crate rustc_hir;
extern crate rustc_middle;
extern crate rustc_span;

use clippy_utils::diagnostics::span_lint_and_note;
use rustc_ast::ast::LitKind;
use rustc_hir::{Expr, ExprKind, MatchSource, PatExprKind, PatKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty;
use rustc_span::sym;

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when code uses strings as substitutes for structured types.
    /// Specifically detects:
    ///
    /// 1. `match` expressions on strings with 2+ string-literal arms
    /// 2. `if`/`else if` chains comparing a string to 2+ literals
    /// 3. `HashMap<String, _>` populated with 2+ string-literal keys
    ///
    /// ### Why is this bad?
    ///
    /// String-keyed dispatch and string-keyed maps are fragile: typos compile
    /// silently, exhaustiveness checking is lost, and refactoring becomes
    /// error-prone. An `enum` or a struct with named fields is almost always
    /// a better choice.
    ///
    /// ### Example
    ///
    /// ```rust
    /// match command {
    ///     "start" => start(),
    ///     "stop"  => stop(),
    ///     _       => unknown(),
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// enum Command { Start, Stop }
    ///
    /// match command {
    ///     Command::Start => start(),
    ///     Command::Stop  => stop(),
    /// }
    /// ```
    pub NO_STRINGLY_TYPED,
    Warn,
    "string literals used where an enum or struct would be more appropriate"
}

/// Returns `true` if the type is `&str` or `String`.
fn is_string_like<'tcx>(cx: &LateContext<'tcx>, expr: &Expr<'tcx>) -> bool {
    let ty = cx.typeck_results().expr_ty(expr).peel_refs();
    ty.is_str() || clippy_utils::ty::is_type_diagnostic_item(cx, ty, sym::String)
}

/// Returns `true` if `expr` is a string literal.
fn is_str_lit(expr: &Expr<'_>) -> bool {
    matches!(
        expr.kind,
        ExprKind::Lit(lit) if matches!(lit.node, LitKind::Str(..))
    )
}

/// Returns `true` if a pattern is (or contains) a string literal.
fn pat_is_str_lit(pat: &rustc_hir::Pat<'_>) -> bool {
    match pat.kind {
        PatKind::Expr(pat_expr) => matches!(
            pat_expr.kind,
            PatExprKind::Lit { lit, .. } if matches!(lit.node, LitKind::Str(..))
        ),
        PatKind::Or(pats) => pats.iter().any(|p| pat_is_str_lit(p)),
        _ => false,
    }
}

/// Check if a type is `HashMap<K, _>` where K is a string-like type.
fn is_hashmap_with_string_key<'tcx>(cx: &LateContext<'tcx>, ty: ty::Ty<'tcx>) -> bool {
    if !clippy_utils::ty::is_type_diagnostic_item(cx, ty, sym::HashMap) {
        return false;
    }
    // Get the key type parameter
    if let ty::Adt(_, args) = ty.kind() {
        if let Some(key_ty) = args.types().next() {
            let key_ty = key_ty.peel_refs();
            return key_ty.is_str()
                || clippy_utils::ty::is_type_diagnostic_item(cx, key_ty, sym::String);
        }
    }
    false
}

/// Extracts the `==` comparison of a string variable to a string literal
/// from an expression, returning the HirId of the variable if found.
fn extract_str_eq_comparison<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
) -> Option<rustc_hir::HirId> {
    // Look for calls to PartialEq::eq or the == operator desugared form
    match expr.kind {
        // Direct method call: s.eq("literal") or PartialEq::eq(&s, &"literal")
        ExprKind::MethodCall(_, receiver, args, _) => {
            if args.len() == 1 {
                if is_string_like(cx, receiver) && is_str_lit(&args[0]) {
                    return path_hir_id(receiver);
                }
                if is_str_lit(receiver) && is_string_like(cx, &args[0]) {
                    return path_hir_id(&args[0]);
                }
            }
            None
        }
        // Binary == operator
        ExprKind::Binary(op, lhs, rhs) if op.node == rustc_hir::BinOpKind::Eq => {
            if is_string_like(cx, lhs) && is_str_lit(rhs) {
                return path_hir_id(lhs);
            }
            if is_str_lit(lhs) && is_string_like(cx, rhs) {
                return path_hir_id(rhs);
            }
            None
        }
        _ => None,
    }
}

/// Extract the HirId from a path expression (possibly behind references).
fn path_hir_id(expr: &Expr<'_>) -> Option<rustc_hir::HirId> {
    let expr = peel_refs_and_borrows(expr);
    match expr.kind {
        ExprKind::Path(rustc_hir::QPath::Resolved(_, path)) => {
            if let rustc_hir::def::Res::Local(hir_id) = path.res {
                Some(hir_id)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Peel `&` and `*` from an expression.
fn peel_refs_and_borrows<'a>(mut expr: &'a Expr<'a>) -> &'a Expr<'a> {
    loop {
        match expr.kind {
            ExprKind::AddrOf(_, _, inner) | ExprKind::Unary(rustc_hir::UnOp::Deref, inner) => {
                expr = inner;
            }
            _ => return expr,
        }
    }
}

/// Count string-literal comparisons in an if/else-if chain on the same variable.
fn count_str_eq_chain<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    target_id: rustc_hir::HirId,
) -> usize {
    let mut count = 0;
    let mut current = Some(expr);

    while let Some(e) = current {
        match e.kind {
            ExprKind::If(cond, _then, else_opt) => {
                if let Some(id) = extract_str_eq_comparison(cx, cond) {
                    if id == target_id {
                        count += 1;
                    }
                }
                current = else_opt.map(|e| peel_block(e));
            }
            _ => break,
        }
    }

    count
}

/// Peel a block expression to get the inner expression.
fn peel_block<'a>(expr: &'a Expr<'a>) -> &'a Expr<'a> {
    match expr.kind {
        ExprKind::Block(block, _) => {
            if block.stmts.is_empty() {
                if let Some(inner) = block.expr {
                    return inner;
                }
            } else if block.stmts.len() == 1 && block.expr.is_none() {
                if let rustc_hir::StmtKind::Expr(inner) | rustc_hir::StmtKind::Semi(inner) =
                    block.stmts[0].kind
                {
                    return inner;
                }
            }
            expr
        }
        _ => expr,
    }
}

/// Returns `true` if this `if` expression is the else branch of a parent `if`
/// (i.e., this is an `else if`, not the outermost `if`).
fn is_else_if<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> bool {
    let parent_iter = cx.tcx.hir_parent_iter(expr.hir_id);
    for (_id, node) in parent_iter {
        match node {
            // Direct parent is the else branch of an if
            rustc_hir::Node::Expr(parent_expr) => {
                if let ExprKind::If(_, _, Some(else_expr)) = parent_expr.kind {
                    if else_expr.hir_id == expr.hir_id {
                        return true;
                    }
                }
                // If parent is a Block wrapping our if, continue looking up
                if let ExprKind::Block(..) = parent_expr.kind {
                    continue;
                }
                return false;
            }
            // Skip blocks
            rustc_hir::Node::Block(_) => continue,
            rustc_hir::Node::Stmt(_) => continue,
            _ => return false,
        }
    }
    false
}

impl<'tcx> LateLintPass<'tcx> for NoStringlyTyped {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        // Pattern 1: match on string with 2+ literal arms
        if let ExprKind::Match(scrutinee, arms, MatchSource::Normal) = expr.kind {
            if is_string_like(cx, scrutinee) {
                let str_lit_arms = arms.iter().filter(|arm| pat_is_str_lit(arm.pat)).count();
                if str_lit_arms >= 2 {
                    span_lint_and_note(
                        cx,
                        NO_STRINGLY_TYPED,
                        expr.span,
                        format!(
                            "match on string with {str_lit_arms} literal arms suggests a missing enum"
                        ),
                        None,
                        "consider defining an enum for these variants and parsing the string into it",
                    );
                    return;
                }
            }
        }

        // Pattern 2: if/else-if chain comparing string to 2+ literals
        if let ExprKind::If(cond, _then, _else_opt) = expr.kind {
            if let Some(target_id) = extract_str_eq_comparison(cx, cond) {
                // Skip if this if-expression is an inner else-if:
                // check whether our parent is the else branch of another if.
                if is_else_if(cx, expr) {
                    // The outermost if will handle this chain.
                } else {
                    let chain_len = count_str_eq_chain(cx, expr, target_id);
                    if chain_len >= 2 {
                        span_lint_and_note(
                            cx,
                            NO_STRINGLY_TYPED,
                            expr.span,
                            format!(
                                "if/else chain compares string to {chain_len} literals, suggesting a missing enum"
                            ),
                            None,
                            "consider defining an enum and matching on it instead of string comparisons",
                        );
                        return;
                    }
                }
            }
        }

        // Pattern 3: HashMap with string-literal keys (.insert("key", val))
        if let ExprKind::MethodCall(method, receiver, args, _) = expr.kind {
            if method.ident.name.as_str() == "insert" && !args.is_empty() && is_str_lit(&args[0]) {
                let recv_ty = cx.typeck_results().expr_ty(receiver).peel_refs();
                if is_hashmap_with_string_key(cx, recv_ty) {
                    // Walk the enclosing block to count how many literal-key
                    // inserts target the same receiver.
                    if let Some(recv_id) = path_hir_id(receiver) {
                        let count = count_literal_inserts_in_parent(cx, expr, recv_id);
                        if count >= 2 {
                            span_lint_and_note(
                                cx,
                                NO_STRINGLY_TYPED,
                                expr.span,
                                "inserting string-literal key into HashMap suggests a struct or enum would be more appropriate",
                                None,
                                "consider using a struct with named fields or an enum-keyed map",
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Count how many `.insert("literal", _)` calls target the same HashMap
/// variable in the enclosing block.
fn count_literal_inserts_in_parent<'tcx>(
    cx: &LateContext<'tcx>,
    _trigger: &'tcx Expr<'tcx>,
    target_id: rustc_hir::HirId,
) -> usize {
    // Walk up to find the enclosing block
    let parent_body = cx.enclosing_body.map(|id| cx.tcx.hir_body(id));

    let Some(body) = parent_body else {
        return 1;
    };

    count_inserts_in_expr(cx, body.value, target_id)
}

/// Recursively count `.insert("literal", _)` calls on `target_id` within an expression.
fn count_inserts_in_expr<'tcx>(
    cx: &LateContext<'tcx>,
    expr: &'tcx Expr<'tcx>,
    target_id: rustc_hir::HirId,
) -> usize {
    match expr.kind {
        ExprKind::Block(block, _) => {
            let mut count = 0;
            for stmt in block.stmts {
                match stmt.kind {
                    rustc_hir::StmtKind::Expr(e) | rustc_hir::StmtKind::Semi(e) => {
                        count += count_inserts_in_expr(cx, e, target_id);
                    }
                    rustc_hir::StmtKind::Let(local) => {
                        if let Some(init) = local.init {
                            count += count_inserts_in_expr(cx, init, target_id);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(tail) = block.expr {
                count += count_inserts_in_expr(cx, tail, target_id);
            }
            count
        }
        ExprKind::MethodCall(method, receiver, args, _)
            if method.ident.name.as_str() == "insert" && !args.is_empty() =>
        {
            if is_str_lit(&args[0]) {
                if let Some(id) = path_hir_id(receiver) {
                    if id == target_id {
                        return 1;
                    }
                }
            }
            0
        }
        _ => 0,
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
