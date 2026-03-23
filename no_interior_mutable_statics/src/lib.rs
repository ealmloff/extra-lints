#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_span;

use clippy_utils::diagnostics::span_lint;
use clippy_utils::ty::is_type_diagnostic_item;
use rustc_hir::{Item, ItemKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::sym;

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Denies `static` items whose type contains interior mutability
    /// (e.g., `Mutex`, `RwLock`, `RefCell`, `Cell`, `UnsafeCell`).
    ///
    /// ### Why is this bad?
    ///
    /// `static` items with interior-mutable types like `Mutex` or `RwLock`
    /// can lead to subtle concurrency bugs and are often a sign that a
    /// `lazy_static!` or `OnceLock`-based pattern should be used instead.
    /// Interior mutability in a `static` bypasses the compiler's normal
    /// guarantees about shared references being immutable.
    ///
    /// ### Example
    ///
    /// ```rust
    /// use std::sync::Mutex;
    /// static SHARED: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// use std::sync::{Mutex, LazyLock};
    /// static SHARED: LazyLock<Mutex<Vec<i32>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    /// ```
    pub NO_INTERIOR_MUTABLE_STATICS,
    Warn,
    "`static` items should not use interior-mutable types"
}

/// Types with interior mutability that we want to ban in `static` items.
const BANNED_DIAG_ITEMS: &[rustc_span::Symbol] = &[
    sym::Mutex,
    sym::RwLock,
    sym::RefCell,
    sym::Cell,
    sym::unsafe_cell,
];

impl<'tcx> LateLintPass<'tcx> for NoInteriorMutableStatics {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        if !matches!(item.kind, ItemKind::Static(..)) {
            return;
        }

        let ty = cx.tcx.type_of(item.owner_id).instantiate_identity();

        // Walk the full type tree to catch nested cases like Arc<Mutex<T>>
        for generic_arg in ty.walk() {
            let Some(inner_ty) = generic_arg.as_type() else {
                continue;
            };
            for &diag_item in BANNED_DIAG_ITEMS {
                if is_type_diagnostic_item(cx, inner_ty, diag_item) {
                    span_lint(
                        cx,
                        NO_INTERIOR_MUTABLE_STATICS,
                        item.span,
                        format!(
                            "`static` item uses interior-mutable type `{}`",
                            diag_item,
                        ),
                    );
                    return;
                }
            }
        }
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
