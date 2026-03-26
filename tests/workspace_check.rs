use std::{fs, process::Command};

use tempfile::TempDir;

#[test]
fn detects_unused_public_items_across_workspace_targets() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"pub fn used() {}

pub fn unused() {}

pub struct PublicType;

impl PublicType {
    pub fn method(&self) {}
}

fn internal() {
    used();
    unused();
}
"#,
    )
    .expect("crate_a source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::{used, PublicType};

fn main() {
    used();
    PublicType.method();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/unused_public_items/report.json"),
    )
    .expect("read report");

    assert!(report.contains("crate_a::unused"));
    assert!(!report.contains("crate_a::used"));
    assert!(!report.contains("crate_a::PublicType"));
    assert!(!report.contains("crate_a::PublicType::method"));
}

#[test]
fn enables_all_features_for_workspace_analysis() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[features]
extra = []

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"#[cfg(feature = "extra")]
pub fn only_with_feature() {}
"#,
    )
    .expect("crate_a source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a", features = ["extra"] }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::only_with_feature;

fn main() {
    only_with_feature();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/unused_public_items/report.json"),
    )
    .expect("read report");

    assert!(!report.contains("crate_a::only_with_feature"));
}

#[test]
fn treats_reexports_as_uses_of_the_original_public_item() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"mod fixtures {
    pub fn used_via_reexport() {}
}

pub use fixtures::used_via_reexport;

pub fn unused() {}
"#,
    )
    .expect("crate_a source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::used_via_reexport;

fn main() {
    used_via_reexport();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/unused_public_items/report.json"),
    )
    .expect("read report");

    assert!(!report.contains("crate_a::fixtures::used_via_reexport"));
    assert!(!report.contains("crate_a::used_via_reexport"));
    assert!(report.contains("crate_a::unused"));
}

#[test]
fn treats_public_return_types_of_used_functions_as_used() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"pub struct Needed;

pub struct Unused;

pub fn setup_lazy_boot() -> Needed {
    Needed
}
"#,
    )
    .expect("crate_a source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::setup_lazy_boot;

fn main() {
    let _boot = setup_lazy_boot();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/unused_public_items/report.json"),
    )
    .expect("read report");

    assert!(!report.contains("crate_a::Needed"));
    assert!(!report.contains("crate_a::setup_lazy_boot"));
    assert!(report.contains("crate_a::Unused"));
}

#[test]
fn treats_cross_crate_associated_function_calls_as_uses() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"pub struct Builder;

impl Builder {
    pub fn new() -> Self {
        Self
    }
}

pub fn unused() {}
"#,
    )
    .expect("crate_a source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::Builder;

fn main() {
    let _builder = Builder::new();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/unused_public_items/report.json"),
    )
    .expect("read report");

    assert!(!report.contains("crate_a::Builder::new"));
    assert!(!report.contains("crate_a::Builder"));
    assert!(report.contains("crate_a::unused"));
}

#[test]
fn treats_cross_crate_associated_function_items_as_uses() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"pub struct Item(pub Option<u32>);

impl Item {
    pub fn value(self) -> Option<u32> {
        self.0
    }
}

pub fn unused() {}
"#,
    )
    .expect("crate_a source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::Item;

fn main() {
    let _values = [Item(Some(1)), Item(None)]
        .into_iter()
        .filter_map(Item::value)
        .collect::<Vec<_>>();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/unused_public_items/report.json"),
    )
    .expect("read report");

    assert!(!report.contains("crate_a::Item::value"));
    assert!(!report.contains("crate_a::Item"));
    assert!(report.contains("crate_a::unused"));
}

#[test]
fn fix_makes_matching_pub_use_reexports_crate_private() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"mod inner;

pub use inner::unused_helper;
"#,
    )
    .expect("crate_a lib source");
    fs::write(
        workspace.path().join("crate_a/src/inner.rs"),
        r#"pub fn unused_helper() {}
"#,
    )
    .expect("crate_a inner source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("fix")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let lib_source = fs::read_to_string(workspace.path().join("crate_a/src/lib.rs"))
        .expect("read fixed lib source");
    let inner_source = fs::read_to_string(workspace.path().join("crate_a/src/inner.rs"))
        .expect("read fixed inner source");

    assert!(lib_source.contains("pub(crate) use inner::unused_helper;"));
    assert!(inner_source.contains("pub(crate) fn unused_helper() {}"));

    let status = Command::new("cargo")
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("cargo check");
    assert!(status.success());
}

#[test]
fn fix_keeps_used_members_public_in_mixed_pub_use_groups() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"mod inner;

pub use inner::{unused_helper, used_helper};
"#,
    )
    .expect("crate_a lib source");
    fs::write(
        workspace.path().join("crate_a/src/inner.rs"),
        r#"pub fn unused_helper() {}

pub fn used_helper() {}
"#,
    )
    .expect("crate_a inner source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::used_helper;

fn main() {
    used_helper();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("fix")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let lib_source = fs::read_to_string(workspace.path().join("crate_a/src/lib.rs"))
        .expect("read fixed lib source");
    let inner_source = fs::read_to_string(workspace.path().join("crate_a/src/inner.rs"))
        .expect("read fixed inner source");

    assert!(lib_source.contains("pub use inner::{used_helper};"));
    assert!(lib_source.contains("pub(crate) use inner::{unused_helper};"));
    assert!(inner_source.contains("pub fn used_helper() {}"));
    assert!(inner_source.contains("pub(crate) fn unused_helper() {}"));

    let status = Command::new("cargo")
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("cargo check");
    assert!(status.success());
}

#[test]
fn fix_keeps_used_members_public_in_nested_mixed_pub_use_groups() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src/outer")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"pub mod outer;
"#,
    )
    .expect("crate_a lib source");
    fs::write(
        workspace.path().join("crate_a/src/outer/mod.rs"),
        r#"mod inner;

pub use inner::{unused_helper, used_helper};
"#,
    )
    .expect("crate_a outer source");
    fs::write(
        workspace.path().join("crate_a/src/outer/inner.rs"),
        r#"pub fn unused_helper() {}

pub fn used_helper() {}
"#,
    )
    .expect("crate_a inner source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::outer::used_helper;

fn main() {
    used_helper();
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("fix")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let outer_source = fs::read_to_string(workspace.path().join("crate_a/src/outer/mod.rs"))
        .expect("read fixed outer source");
    let inner_source = fs::read_to_string(workspace.path().join("crate_a/src/outer/inner.rs"))
        .expect("read fixed inner source");

    assert!(outer_source.contains("pub use inner::{used_helper};"));
    assert!(outer_source.contains("pub(crate) use inner::{unused_helper};"));
    assert!(inner_source.contains("pub fn used_helper() {}"));
    assert!(inner_source.contains("pub(crate) fn unused_helper() {}"));

    let status = Command::new("cargo")
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("cargo check");
    assert!(status.success());
}

#[test]
fn fix_handles_multiline_reexports_through_intermediate_aliases() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("crate_a/src/engine")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"mod engine;

pub use engine::{
    StepStatus,
    Used,
};
"#,
    )
    .expect("crate_a lib source");
    fs::write(
        workspace.path().join("crate_a/src/engine/mod.rs"),
        r#"mod inner;

pub use inner::StepStatus;
pub use inner::Used;
"#,
    )
    .expect("crate_a engine source");
    fs::write(
        workspace.path().join("crate_a/src/engine/inner.rs"),
        r#"pub enum StepStatus {
    Ready,
}

pub struct Used;
"#,
    )
    .expect("crate_a inner source");

    fs::create_dir_all(workspace.path().join("crate_b/src")).expect("crate_b src dir");
    fs::write(
        workspace.path().join("crate_b/Cargo.toml"),
        r#"[package]
name = "crate_b"
version = "0.1.0"
edition = "2024"

[dependencies]
crate_a = { path = "../crate_a" }
"#,
    )
    .expect("crate_b manifest");
    fs::write(
        workspace.path().join("crate_b/src/main.rs"),
        r#"use crate_a::Used;

fn main() {
    let _used = Used;
}
"#,
    )
    .expect("crate_b source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("fix")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let lib_source = fs::read_to_string(workspace.path().join("crate_a/src/lib.rs"))
        .expect("read fixed lib source");
    let engine_source = fs::read_to_string(workspace.path().join("crate_a/src/engine/mod.rs"))
        .expect("read fixed engine source");
    let inner_source = fs::read_to_string(workspace.path().join("crate_a/src/engine/inner.rs"))
        .expect("read fixed inner source");

    assert!(lib_source.contains("pub use engine::{Used};"));
    assert!(lib_source.contains("pub(crate) use engine::{StepStatus};"));
    assert!(engine_source.contains("pub(crate) use inner::StepStatus;"));
    assert!(inner_source.contains("pub(crate) enum StepStatus"));
    assert!(inner_source.contains("pub struct Used;"));

    let status = Command::new("cargo")
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("cargo check");
    assert!(status.success());
}

#[test]
fn fix_skips_items_with_visibility_sensitive_derives() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["derive_macro", "crate_a"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("derive_macro/src")).expect("derive src dir");
    fs::write(
        workspace.path().join("derive_macro/Cargo.toml"),
        r#"[package]
name = "derive_macro"
version = "0.1.0"
edition = "2024"

[lib]
proc-macro = true
"#,
    )
    .expect("derive manifest");
    fs::write(
        workspace.path().join("derive_macro/src/lib.rs"),
        r#"use proc_macro::TokenStream;

#[proc_macro_derive(PublicFactory)]
pub fn public_factory(input: TokenStream) -> TokenStream {
    let input = input.to_string();
    let name = input
        .split_whitespace()
        .skip_while(|part| *part != "struct" && *part != "enum")
        .nth(1)
        .unwrap_or("Model")
        .trim_matches('{')
        .trim_matches(';')
        .trim_matches('(');

    format!("pub fn make_public() -> {name} {{ panic!() }}")
        .parse()
        .expect("generated tokens")
}
"#,
    )
    .expect("derive source");

    fs::create_dir_all(workspace.path().join("crate_a/src")).expect("crate_a src dir");
    fs::write(
        workspace.path().join("crate_a/Cargo.toml"),
        r#"[package]
name = "crate_a"
version = "0.1.0"
edition = "2024"

[dependencies]
derive_macro = { path = "../derive_macro" }

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("crate_a manifest");
    fs::write(
        workspace.path().join("crate_a/src/lib.rs"),
        r#"use derive_macro::PublicFactory;

#[derive(Clone, PublicFactory)]
pub struct Model;
"#,
    )
    .expect("crate_a source");

    let status = Command::new(env!("CARGO_BIN_EXE_pubprune"))
        .arg("fix")
        .current_dir(workspace.path())
        .status()
        .expect("run coordinator");
    assert!(status.success());

    let lib_source = fs::read_to_string(workspace.path().join("crate_a/src/lib.rs"))
        .expect("read fixed lib source");
    assert!(lib_source.contains("pub struct Model;"));

    let status = Command::new("cargo")
        .arg("check")
        .current_dir(workspace.path())
        .status()
        .expect("cargo check");
    assert!(status.success());
}
