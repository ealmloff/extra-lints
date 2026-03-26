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
