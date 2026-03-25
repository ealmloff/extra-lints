use std::{fs, process::Command};

use tempfile::TempDir;

#[test]
fn detects_trait_option_items_that_only_use_some_across_workspace() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["api", "impls", "client"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    fs::create_dir_all(workspace.path().join("api/src")).expect("api src dir");
    fs::write(
        workspace.path().join("api/Cargo.toml"),
        r#"[package]
name = "api"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("api manifest");
    fs::write(
        workspace.path().join("api/src/lib.rs"),
        r#"pub trait Api {
    fn always_some(&self, value: Option<u32>);
    fn mixed(&self, value: Option<u32>);

    const DEFAULT_VALUE: Option<&'static str>;
    const MIXED_VALUE: Option<&'static str>;
}
"#,
    )
    .expect("api source");

    fs::create_dir_all(workspace.path().join("impls/src")).expect("impls src dir");
    fs::write(
        workspace.path().join("impls/Cargo.toml"),
        r#"[package]
name = "impls"
version = "0.1.0"
edition = "2024"

[dependencies]
api = { path = "../api" }
"#,
    )
    .expect("impls manifest");
    fs::write(
        workspace.path().join("impls/src/lib.rs"),
        r#"use api::Api;

pub struct First;
pub struct Second;

impl Api for First {
    fn always_some(&self, _value: Option<u32>) {}
    fn mixed(&self, _value: Option<u32>) {}

    const DEFAULT_VALUE: Option<&'static str> = Some("first");
    const MIXED_VALUE: Option<&'static str> = Some("first");
}

impl Api for Second {
    fn always_some(&self, _value: Option<u32>) {}
    fn mixed(&self, _value: Option<u32>) {}

    const DEFAULT_VALUE: Option<&'static str> = Some("second");
    const MIXED_VALUE: Option<&'static str> = None;
}
"#,
    )
    .expect("impls source");

    fs::create_dir_all(workspace.path().join("client/src")).expect("client src dir");
    fs::write(
        workspace.path().join("client/Cargo.toml"),
        r#"[package]
name = "client"
version = "0.1.0"
edition = "2024"

[dependencies]
api = { path = "../api" }
impls = { path = "../impls" }
"#,
    )
    .expect("client manifest");
    fs::write(
        workspace.path().join("client/src/main.rs"),
        r#"use api::Api;
use impls::{First, Second};

fn main() {
    let first = First;
    let second = Second;

    first.always_some(Some(1));
    second.always_some(Some(2));
    Api::always_some(&first, Some(3));

    first.mixed(Some(4));
    second.mixed(None);
}
"#,
    )
    .expect("client source");

    let output = Command::new(env!("CARGO_BIN_EXE_trait_option_single_variant"))
        .arg("check")
        .current_dir(workspace.path())
        .output()
        .expect("run coordinator");
    assert!(output.status.success(), "{output:#?}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/trait_option_single_variant/report.json"),
    )
    .expect("read report");

    assert!(
        report.contains("api::Api::always_some"),
        "report was:\n{report}"
    );
    assert!(
        report.contains("api::Api::DEFAULT_VALUE"),
        "report was:\n{report}"
    );
    assert!(!report.contains("api::Api::mixed"), "report was:\n{report}");
    assert!(
        !report.contains("api::Api::MIXED_VALUE"),
        "report was:\n{report}"
    );

    assert!(
        stderr.contains("api::Api::always_some"),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("only ever passed `Some` across the workspace"),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("api::Api::DEFAULT_VALUE"),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("only ever assigned `Some` across the workspace"),
        "stderr was:\n{stderr}"
    );
    assert!(!stderr.contains("api::Api::mixed"), "stderr was:\n{stderr}");
    assert!(
        !stderr.contains("api::Api::MIXED_VALUE"),
        "stderr was:\n{stderr}"
    );
}
