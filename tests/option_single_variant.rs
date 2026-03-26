use std::{fs, process::Command};

use tempfile::TempDir;

#[test]
fn detects_option_items_that_only_use_one_variant_across_workspace() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["api", "impls", "functions", "models", "client"]
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

    fs::create_dir_all(workspace.path().join("functions/src")).expect("functions src dir");
    fs::write(
        workspace.path().join("functions/Cargo.toml"),
        r#"[package]
name = "functions"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("functions manifest");
    fs::write(
        workspace.path().join("functions/src/lib.rs"),
        r#"pub fn always_some(value: Option<u32>) {
    let _ = value;
}

pub fn mixed(value: Option<u32>) {
    let _ = value;
}

pub struct Handler;

impl Handler {
    pub fn always_none(&self, value: Option<&'static str>) {
        let _ = value;
    }

    pub fn method_mixed(&self, value: Option<&'static str>) {
        let _ = value;
    }
}
"#,
    )
    .expect("functions source");

    fs::create_dir_all(workspace.path().join("models/src")).expect("models src dir");
    fs::write(
        workspace.path().join("models/Cargo.toml"),
        r#"[package]
name = "models"
version = "0.1.0"
edition = "2024"

[lib]
path = "src/lib.rs"
"#,
    )
    .expect("models manifest");
    fs::write(
        workspace.path().join("models/src/lib.rs"),
        r#"pub struct Config {
    pub always_some: Option<u32>,
    pub mixed: Option<u32>,
    pub always_none: Option<&'static str>,
}

pub struct Pair(pub Option<u32>, pub Option<u32>);
"#,
    )
    .expect("models source");

    fs::create_dir_all(workspace.path().join("client/src")).expect("client src dir");
    fs::write(
        workspace.path().join("client/Cargo.toml"),
        r#"[package]
name = "client"
version = "0.1.0"
edition = "2024"

[dependencies]
api = { path = "../api" }
functions = { path = "../functions" }
impls = { path = "../impls" }
models = { path = "../models" }
"#,
    )
    .expect("client manifest");
    fs::write(
        workspace.path().join("client/src/main.rs"),
        r#"use api::Api;
use functions::{always_some, mixed, Handler};
use impls::{First, Second};
use models::{Config, Pair};

fn main() {
    let first = First;
    let second = Second;

    first.always_some(Some(1));
    second.always_some(Some(2));
    Api::always_some(&first, Some(3));

    first.mixed(Some(4));
    second.mixed(None);

    always_some(Some(10));
    always_some(Some(11));
    mixed(Some(12));
    mixed(None);

    let handler = Handler;
    handler.always_none(None);
    Handler::always_none(&handler, None);
    handler.method_mixed(Some("value"));
    handler.method_mixed(None);

    let mut config = Config {
        always_some: Some(20),
        mixed: Some(21),
        always_none: None,
    };
    config.mixed = None;
    config.always_some = Some(22);
    config.always_none = None;

    let _pair_one = Pair(Some(30), None);
    let _pair_two = Pair(Some(31), Some(32));
}
"#,
    )
    .expect("client source");

    let output = Command::new(env!("CARGO_BIN_EXE_option_single_variant"))
        .arg("check")
        .current_dir(workspace.path())
        .output()
        .expect("run coordinator");
    assert!(output.status.success(), "{output:#?}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let report = fs::read_to_string(
        workspace
            .path()
            .join("target/option_single_variant/report.json"),
    )
    .expect("read report");

    for expected in [
        "api::Api::always_some",
        "api::Api::DEFAULT_VALUE",
        "functions::always_some",
        "functions::Handler::always_none",
        "models::Config::always_some",
        "models::Config::always_none",
        "models::Pair::0",
    ] {
        assert!(
            report.contains(expected),
            "missing `{expected}` in report:\n{report}"
        );
    }

    for expected in [
        "api::Api::always_some",
        "api::Api::DEFAULT_VALUE",
        "functions::always_some",
        "functions::Handler::always_none",
        "models::Config::always_some",
        "models::Config::always_none",
        "field #1 of tuple struct `models::Pair`",
    ] {
        assert!(
            stderr.contains(expected),
            "missing `{expected}` in stderr:\n{stderr}"
        );
    }

    for unexpected in [
        "api::Api::mixed",
        "api::Api::MIXED_VALUE",
        "functions::mixed",
        "functions::Handler::method_mixed",
        "models::Config::mixed",
        "models::Pair::1",
    ] {
        assert!(
            !report.contains(unexpected),
            "unexpected `{unexpected}` in report:\n{report}"
        );
        assert!(
            !stderr.contains(unexpected),
            "unexpected `{unexpected}` in stderr:\n{stderr}"
        );
    }

    for expected_message in [
        "only ever passed `Some` across the workspace",
        "only ever passed `None` across the workspace",
        "only ever assigned `Some` across the workspace",
        "only ever set `Some` across the workspace",
        "only ever set `None` across the workspace",
    ] {
        assert!(
            stderr.contains(expected_message),
            "missing message `{expected_message}` in stderr:\n{stderr}"
        );
    }
}
