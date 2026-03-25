use std::{fs, process::Command};

use tempfile::TempDir;

#[test]
fn detects_overlapping_struct_fields_across_workspace() {
    let workspace = TempDir::new().expect("temp workspace");

    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["crate_a", "crate_b"]
resolver = "2"
"#,
    )
    .expect("workspace manifest");

    // crate_a defines two structs that share 3 fields (name + type match)
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
        r#"pub struct User {
    pub name: String,
    pub email: String,
    pub age: u32,
    pub active: bool,
}

pub struct Admin {
    pub name: String,
    pub email: String,
    pub age: u32,
    pub role: String,
}
"#,
    )
    .expect("crate_a source");

    // crate_b defines a struct in a different crate that also overlaps
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
        workspace.path().join("crate_b/src/lib.rs"),
        r#"pub struct Profile {
    pub name: String,
    pub email: String,
    pub age: u32,
    pub avatar_url: String,
}

// This struct only shares 2 fields — below threshold, should NOT be flagged
pub struct Config {
    pub name: String,
    pub email: String,
    pub timeout: u64,
}
"#,
    )
    .expect("crate_b source");

    let output = Command::new(env!("CARGO_BIN_EXE_overlapping_struct_fields"))
        .arg("check")
        .current_dir(workspace.path())
        .output()
        .expect("run coordinator");
    assert!(
        output.status.success(),
        "status: {:#?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let report =
        fs::read_to_string(workspace.path().join("target/osf/report.json")).expect("read report");

    // User, Admin, and Profile all share 3 fields (name: String, email: String, age: u32)
    // so all three should appear in the report
    assert!(
        report.contains("crate_a::User"),
        "report should flag User, got:\n{report}"
    );
    assert!(
        report.contains("crate_a::Admin"),
        "report should flag Admin, got:\n{report}"
    );
    assert!(
        report.contains("crate_b::Profile"),
        "report should flag Profile, got:\n{report}"
    );

    // Config only shares 2 fields — below threshold
    assert!(
        !report.contains("crate_b::Config"),
        "report should NOT flag Config (only 2 shared fields), got:\n{report}"
    );

    // The shared fields should be listed
    assert!(
        report.contains("name"),
        "report should mention shared field 'name', got:\n{report}"
    );
    assert!(
        report.contains("email"),
        "report should mention shared field 'email', got:\n{report}"
    );
    assert!(
        report.contains("age"),
        "report should mention shared field 'age', got:\n{report}"
    );

    // Verify emit-phase diagnostics appear on stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("shares"),
        "stderr should contain overlap warning, got:\n{stderr}"
    );
    assert!(
        stderr.contains("consider extracting the shared fields"),
        "stderr should contain help message, got:\n{stderr}"
    );
}
