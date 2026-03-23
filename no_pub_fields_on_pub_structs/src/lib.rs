#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_middle;

use clippy_utils::diagnostics::span_lint_and_note;
use rustc_hir::{Item, ItemKind, VariantData};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::Visibility;

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when a `pub` struct has `pub` fields. Public structs should
    /// expose private fields with accessor methods to maintain API stability.
    ///
    /// ### Why is this bad?
    ///
    /// Public fields on public structs lock the struct's representation into
    /// the public API. Adding, removing, or retyping a field becomes a
    /// breaking change. Keeping fields private lets you refactor internals
    /// freely and control invariants through constructors and accessors.
    ///
    /// ### Exceptions
    ///
    /// - `#[repr(C)]` structs (field layout is part of the contract)
    /// - Tuple structs (e.g. newtype wrappers like `pub struct Foo(pub Bar)`)
    /// - Unit structs
    ///
    /// ### Example
    ///
    /// ```rust
    /// pub struct Config {
    ///     pub timeout: u64,  // warning: pub field on pub struct
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust
    /// pub struct Config {
    ///     timeout: u64,
    /// }
    ///
    /// impl Config {
    ///     pub fn timeout(&self) -> u64 { self.timeout }
    /// }
    /// ```
    pub NO_PUB_FIELDS_ON_PUB_STRUCTS,
    Warn,
    "`pub` structs should not have `pub` fields"
}

impl<'tcx> LateLintPass<'tcx> for NoPubFieldsOnPubStructs {
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        // Only check named-field structs (not tuple structs or unit structs)
        let ItemKind::Struct(_, _, VariantData::Struct { fields, .. }) = item.kind else {
            return;
        };

        // The struct itself must be pub
        if !cx.tcx.visibility(item.owner_id).is_public() {
            return;
        }

        // Skip #[repr(C)] — field layout is part of the FFI contract
        let adt_def = cx.tcx.adt_def(item.owner_id);
        if adt_def.repr().c() {
            return;
        }

        for field in fields {
            if let Visibility::Public = cx.tcx.visibility(field.def_id) {
                span_lint_and_note(
                    cx,
                    NO_PUB_FIELDS_ON_PUB_STRUCTS,
                    field.span,
                    "public field on a public struct",
                    None,
                    "consider making this field private and adding accessor methods",
                );
            }
        }
    }
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
