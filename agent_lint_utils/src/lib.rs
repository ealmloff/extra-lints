#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_span;

pub mod workspace_lint;

use rustc_hir::def::DefKind;
use rustc_lint::LateContext;
use rustc_span::{FileName, Span};
use serde::{Deserialize, Serialize};

/// A stable, serializable identifier for a definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DefKey {
    pub path: String,
}

/// Returns a stable string identifier for a crate.
pub fn stable_crate_id<'tcx>(
    cx: &LateContext<'tcx>,
    crate_num: rustc_span::def_id::CrateNum,
) -> String {
    format!("{:?}", cx.tcx.stable_crate_id(crate_num))
}

/// Builds a `DefKey` from a `DefId` using the item's structural definition
/// path rather than rustc's user-facing path string.
///
/// This keeps the key stable across `lib`/`lib test` compilations and avoids
/// re-export spellings like `crate::foo` comparing differently from the
/// original definition path like `crate::module::foo`.
pub fn def_key<'tcx>(cx: &LateContext<'tcx>, def_id: rustc_span::def_id::DefId) -> DefKey {
    let def_id = canonical_def_id(cx, def_id);
    DefKey {
        path: structural_def_path(cx, def_id),
    }
}

fn canonical_def_id<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> rustc_span::def_id::DefId {
    match cx.tcx.def_kind(def_id) {
        DefKind::Ctor(..) => cx.tcx.opt_parent(def_id).unwrap_or(def_id),
        _ => def_id,
    }
}

fn structural_def_path<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> String {
    let crate_name = cx.tcx.crate_name(def_id.krate).to_string();
    let path = cx.tcx.def_path(def_id).to_string_no_crate_verbose();
    if path.is_empty() {
        crate_name
    } else {
        format!("{crate_name}::{path}")
    }
}

/// Returns a fully-qualified, crate-prefixed path string for a `DefId`.
pub fn normalized_def_path<'tcx>(
    cx: &LateContext<'tcx>,
    def_id: rustc_span::def_id::DefId,
) -> String {
    let crate_name = cx.tcx.crate_name(def_id.krate).to_string();
    let path = cx.tcx.def_path_str(def_id);
    if path == crate_name || path.starts_with(&format!("{crate_name}::")) {
        path
    } else {
        format!("{crate_name}::{path}")
    }
}

/// Resolves a `Span` to `(file_path, line, column)`, or `None` for non-real files.
pub fn span_location<'tcx>(
    cx: &LateContext<'tcx>,
    span: Span,
) -> Option<(String, u32, u32)> {
    let source_map = cx.tcx.sess.source_map();
    let location = source_map.lookup_char_pos(span.lo());
    let FileName::Real(real_file) = &location.file.name else {
        return None;
    };
    Some((
        real_file.local_path()?.display().to_string(),
        location.line.try_into().ok()?,
        (location.col.0 + 1).try_into().ok()?,
    ))
}

/// Sanitizes a string for use in file names: only keeps `[a-zA-Z0-9_-]`.
pub fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}
