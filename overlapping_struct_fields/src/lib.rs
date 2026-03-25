#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_span;

use std::cell::RefCell;
use std::collections::BTreeMap;

use agent_lint_utils::workspace_lint::{CrateInfo, LintEnvConfig, Mode, write_artifact_file};
use agent_lint_utils::{DefKey, def_key, normalized_def_path, span_location};
use clippy_utils::diagnostics::span_lint_and_help;
use rustc_hir::{Item, ItemKind, VariantData};
use rustc_lint::{LateContext, LateLintPass};
use rustc_span::Span;
use serde::{Deserialize, Serialize};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Warns when two or more structs in the workspace share 3 or more fields
    /// with the same name and type, suggesting they should extract a common
    /// struct.
    ///
    /// ### Why is this bad?
    ///
    /// Duplicated field sets across structs indicate a missing abstraction.
    /// Extracting the shared fields into a common struct improves
    /// maintainability and reduces the risk of the definitions drifting apart.
    ///
    /// ### Known problems
    ///
    /// Type comparison uses string representations, so type aliases or
    /// re-exports may cause false negatives. `#[repr(C)]` structs are skipped
    /// because their field layout is semantically significant.
    pub OVERLAPPING_STRUCT_FIELDS,
    Warn,
    "two or more structs share many fields and should extract a common struct"
}

const ENV_CONFIG: LintEnvConfig = LintEnvConfig {
    prefix: "OVERLAPPING_STRUCT_FIELDS",
};

thread_local! {
    static STATE: RefCell<LintState> = RefCell::new(LintState::default());
}

// ---------------------------------------------------------------------------
// Serializable data types (shared with the CLI aggregation binary)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StructField {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructRecord {
    pub def_key: DefKey,
    pub display_path: String,
    pub fields: Vec<StructField>,
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetArtifact {
    pub crate_stable_id: String,
    pub crate_name: String,
    pub target_name: String,
    pub target_kind: String,
    pub structs: Vec<StructRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlapEntry {
    pub struct_a: DefKey,
    pub struct_b: DefKey,
    pub display_path_a: String,
    pub display_path_b: String,
    pub shared_fields: Vec<StructField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedReport {
    pub overlaps: Vec<OverlapEntry>,
}

// ---------------------------------------------------------------------------
// In-memory lint state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct LocalStruct {
    def_key: DefKey,
    display_path: String,
    span: Span,
}

#[derive(Debug, Clone)]
struct LintState {
    mode: Mode<BTreeMap<DefKey, Vec<OverlapEntry>>>,
    info: CrateInfo,
    /// Struct definitions in the current crate (for artifact writing).
    struct_records: Vec<StructRecord>,
    /// Struct definitions with their spans (for emit-phase matching).
    local_structs: Vec<LocalStruct>,
}

impl Default for LintState {
    fn default() -> Self {
        Self {
            mode: Mode::Disabled,
            info: CrateInfo::default(),
            struct_records: Vec::new(),
            local_structs: Vec::new(),
        }
    }
}

impl LintState {
    fn for_crate<'tcx>(cx: &LateContext<'tcx>) -> Self {
        Self {
            mode: Mode::from_env(&ENV_CONFIG, |report: AggregatedReport| {
                let mut map = BTreeMap::<DefKey, Vec<OverlapEntry>>::new();
                for entry in report.overlaps {
                    map.entry(entry.struct_a.clone())
                        .or_default()
                        .push(entry);
                }
                map
            }),
            info: CrateInfo::for_current_crate(cx),
            struct_records: Vec::new(),
            local_structs: Vec::new(),
        }
    }

    fn enabled(&self) -> bool {
        !self.mode.is_disabled()
    }
}

// ---------------------------------------------------------------------------
// LateLintPass implementation
// ---------------------------------------------------------------------------

impl<'tcx> LateLintPass<'tcx> for OverlappingStructFields {
    fn check_crate(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| *state.borrow_mut() = LintState::for_crate(cx));
    }

    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.enabled() {
                return;
            }
            register_struct(cx, item, &mut state);
        });
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        STATE.with(|state| {
            let state = std::mem::take(&mut *state.borrow_mut());
            match state.mode {
                Mode::Collect { ref artifact_dir } => {
                    if let Err(error) = write_artifact(cx, &state, artifact_dir) {
                        cx.tcx.sess.dcx().warn(format!(
                            "overlapping_struct_fields failed to write artifact: {error}"
                        ));
                    }
                }
                Mode::Emit { data: ref flagged } => {
                    for local_struct in &state.local_structs {
                        let Some(entries) = flagged.get(&local_struct.def_key) else {
                            continue;
                        };
                        for entry in entries {
                            emit_lint(cx, local_struct, entry);
                        }
                    }
                }
                Mode::Disabled => {}
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Candidate registration: record struct field definitions
// ---------------------------------------------------------------------------

fn register_struct<'tcx>(cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>, state: &mut LintState) {
    let ItemKind::Struct(_, _, variant_data) = item.kind else {
        return;
    };
    let VariantData::Struct { .. } = variant_data else {
        return;
    };
    if item.span.from_expansion() {
        return;
    }

    let struct_def_id = item.owner_id.to_def_id();
    let adt_def = cx.tcx.adt_def(struct_def_id);

    // Skip #[repr(C)] structs — field layout is part of the contract.
    if adt_def.repr().c() {
        return;
    }

    let struct_key = def_key(cx, struct_def_id);
    let struct_path = normalized_def_path(cx, struct_def_id);

    let mut struct_fields = Vec::new();
    let variant = adt_def.non_enum_variant();

    for field_def in variant.fields.iter() {
        let field_name = field_def.name.to_string();
        let field_ty = cx.tcx.type_of(field_def.did).instantiate_identity();
        let ty_str = format!("{field_ty}");
        struct_fields.push(StructField {
            name: field_name,
            ty: ty_str,
        });
    }

    if struct_fields.is_empty() {
        return;
    }

    state.local_structs.push(LocalStruct {
        def_key: struct_key.clone(),
        display_path: struct_path.clone(),
        span: item.span,
    });

    if let Some((file, line, column)) = span_location(cx, item.span) {
        state.struct_records.push(StructRecord {
            def_key: struct_key,
            display_path: struct_path,
            fields: struct_fields,
            file,
            line,
            column,
        });
    }
}

// ---------------------------------------------------------------------------
// Emit diagnostics
// ---------------------------------------------------------------------------

fn emit_lint<'tcx>(cx: &LateContext<'tcx>, local_struct: &LocalStruct, entry: &OverlapEntry) {
    let field_list = entry
        .shared_fields
        .iter()
        .map(|f| format!("{}: {}", f.name, f.ty))
        .collect::<Vec<_>>()
        .join(", ");

    span_lint_and_help(
        cx,
        OVERLAPPING_STRUCT_FIELDS,
        local_struct.span,
        format!(
            "struct `{}` shares {} fields with `{}`: [{}]",
            local_struct.display_path,
            entry.shared_fields.len(),
            entry.display_path_b,
            field_list,
        ),
        None,
        "consider extracting the shared fields into a common struct and composing it",
    );
}

// ---------------------------------------------------------------------------
// Artifact writing
// ---------------------------------------------------------------------------

fn write_artifact<'tcx>(
    cx: &LateContext<'tcx>,
    state: &LintState,
    artifact_dir: &std::path::Path,
) -> Result<(), String> {
    let _ = cx;
    let artifact = TargetArtifact {
        crate_stable_id: state.info.crate_stable_id.clone(),
        crate_name: state.info.crate_name.clone(),
        target_name: state.info.target_name.clone(),
        target_kind: state.info.target_kind.clone(),
        structs: state.struct_records.clone(),
    };

    write_artifact_file(
        &artifact,
        &state.info.crate_name,
        &state.info.target_name,
        artifact_dir,
    )
}

#[test]
fn ui() {
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "ui");
}
