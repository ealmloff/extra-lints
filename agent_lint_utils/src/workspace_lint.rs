extern crate rustc_lint;
extern crate rustc_span;

use std::{env, fs, path::{Path, PathBuf}};

use rustc_lint::LateContext;
use rustc_span::def_id::LOCAL_CRATE;
use serde::{Serialize, de::DeserializeOwned};

/// Configuration for a workspace lint's environment variable names.
pub struct LintEnvConfig {
    /// Environment variable prefix, e.g. `"OPTION_SINGLE_VARIANT"`.
    ///
    /// Used to derive `{prefix}_MODE`, `{prefix}_DIR`, and `{prefix}_REPORT`.
    pub prefix: &'static str,
}

impl LintEnvConfig {
    fn mode_var(&self) -> String {
        format!("{}_MODE", self.prefix)
    }
    fn dir_var(&self) -> String {
        format!("{}_DIR", self.prefix)
    }
    fn report_var(&self) -> String {
        format!("{}_REPORT", self.prefix)
    }
}

/// The two-phase mode for workspace-wide lints, generic over emit-phase data.
#[derive(Debug, Clone)]
pub enum Mode<E> {
    /// Collect phase: write artifacts to the given directory.
    Collect { artifact_dir: PathBuf },
    /// Emit phase: use pre-aggregated data to fire diagnostics.
    Emit { data: E },
    /// Lint is not active (env vars not set).
    Disabled,
}

impl<E> Mode<E> {
    /// Parse mode from environment variables.
    ///
    /// - `config`: which env var prefix to read
    /// - `transform_report`: converts the deserialized report into emit-phase data
    pub fn from_env<R: DeserializeOwned>(
        config: &LintEnvConfig,
        transform_report: impl FnOnce(R) -> E,
    ) -> Self {
        match env::var(config.mode_var()).as_deref() {
            Ok("collect") => env::var_os(config.dir_var())
                .map(PathBuf::from)
                .map(|artifact_dir| Self::Collect { artifact_dir })
                .unwrap_or(Self::Disabled),
            Ok("emit") => {
                let Some(report_path) = env::var_os(config.report_var()) else {
                    return Self::Disabled;
                };
                let Ok(bytes) = fs::read(report_path) else {
                    return Self::Disabled;
                };
                let Ok(report) = serde_json::from_slice::<R>(&bytes) else {
                    return Self::Disabled;
                };
                Self::Emit {
                    data: transform_report(report),
                }
            }
            _ => Self::Disabled,
        }
    }

    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// Common crate-level metadata populated at `check_crate` time.
#[derive(Debug, Clone, Default)]
pub struct CrateInfo {
    pub crate_stable_id: String,
    pub crate_name: String,
    pub target_name: String,
    pub target_kind: String,
}

impl CrateInfo {
    /// Build `CrateInfo` for the crate currently being compiled.
    pub fn for_current_crate<'tcx>(cx: &LateContext<'tcx>) -> Self {
        Self {
            crate_stable_id: crate::stable_crate_id(cx, LOCAL_CRATE),
            crate_name: cx.tcx.crate_name(LOCAL_CRATE).to_string(),
            target_name: env::var("CARGO_CRATE_NAME")
                .unwrap_or_else(|_| "unknown".to_owned()),
            target_kind: env::var("CARGO_BIN_NAME")
                .map(|_| "bin".to_owned())
                .unwrap_or_else(|_| "lib".to_owned()),
        }
    }
}

/// Write a JSON artifact file atomically (write to `.tmp`, then rename).
///
/// The file name is derived from `crate_name`, `target_name`, and PID.
pub fn write_artifact_file<A: Serialize>(
    artifact: &A,
    crate_name: &str,
    target_name: &str,
    artifact_dir: &Path,
) -> Result<(), String> {
    let file_name = format!(
        "{}-{}-{}.json",
        crate::sanitize(crate_name),
        crate::sanitize(target_name),
        std::process::id(),
    );
    let final_path = artifact_dir.join(file_name);
    let temp_path = final_path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(artifact).map_err(|e| e.to_string())?;
    fs::write(&temp_path, json).map_err(|e| e.to_string())?;
    fs::rename(&temp_path, &final_path).map_err(|e| e.to_string())?;
    Ok(())
}
