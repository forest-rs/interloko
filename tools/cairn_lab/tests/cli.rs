//! CLI fixture tests for `cairn_lab`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cairn_lab")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name)
}

fn run_lab(args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .output()
        .expect("cairn_lab should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf8")
}

#[test]
fn run_executes_main() {
    let file = fixture("generic_area.cairn");
    let out = run_lab(&["run", file.to_str().expect("fixture path should be utf8")]);

    assert_eq!(out.trim(), "100");
}

#[test]
fn dump_exposes_stages() {
    let file = fixture("generic_area.cairn");
    let path = file.to_str().expect("fixture path should be utf8");

    let syntax = run_lab(&["dump", path, "--stage", "syntax"]);
    assert!(syntax.contains("syntax"));
    assert!(syntax.contains("Function"));

    let ast = run_lab(&["dump", path, "--stage", "ast"]);
    assert!(ast.contains("function"));
    assert!(ast.contains("def"));

    let sem = run_lab(&["dump", path, "--stage", "sem"]);
    assert!(sem.contains("generic-call"));

    let mop = run_lab(&["dump", path, "--stage", "mop"]);
    assert!(mop.contains("mop"));
    assert!(mop.contains("classes:"));
    assert!(mop.contains("dispatch table:"));

    let lowered = run_lab(&["dump", path, "--stage", "lowered"]);
    assert!(lowered.contains("area#dispatch"));

    let tape = run_lab(&["dump", path, "--stage", "tape"]);
    assert!(tape.contains("program=\"cairn\""));
}

#[test]
fn mop_reports_native_and_host_backed_metaobjects() {
    let native = fixture("generic_area.cairn");
    let native_out = run_lab(&["mop", native.to_str().expect("fixture path should be utf8")]);
    assert!(native_out.contains("c0"));
    assert!(native_out.contains("<circle>"));
    assert!(native_out.contains("origin=native runtime=native"));
    assert!(native_out.contains("accessor"));
    assert!(native_out.contains("g0 area"));
    assert!(native_out.contains("c0 <circle> origin=native -> m0"));

    let host = fixture("host_backed_area.cairn");
    let host_out = run_lab(&["mop", host.to_str().expect("fixture path should be utf8")]);
    assert!(host_out.contains("<rust-circle> origin=host-backed(hc0) runtime=host-backed"));
    assert!(host_out.contains("<rust-square> origin=host-backed(hc1) runtime=host-backed"));
    assert!(host_out.contains("slot s0"));
    assert!(host_out.contains("storage=host-backed"));
    assert!(host_out.contains("c0 <rust-circle> origin=host-backed(hc0) -> m0"));
    assert!(host_out.contains("c1 <rust-square> origin=host-backed(hc1) -> m1"));
}

#[test]
fn artifacts_lists_stable_keys_and_edges() {
    let file = fixture("generic_area.cairn");
    let out = run_lab(&[
        "artifacts",
        file.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(out.contains("artifact graph"));
    assert!(out.contains("art"));
    assert!(out.contains("method-lookup"));
    assert!(out.contains("dependencies:"));
}

#[test]
fn diff_reports_preserved_and_focused_method_changes() {
    let before = fixture("generic_area.cairn");
    let trivia = fixture("generic_area_trivia.cairn");
    let edited = fixture("generic_area_method_edit.cairn");

    let harmless = run_lab(&[
        "diff",
        before.to_str().expect("fixture path should be utf8"),
        trivia.to_str().expect("fixture path should be utf8"),
    ]);
    assert!(harmless.contains("incremental report"));
    assert!(harmless.contains("reused"));
    assert!(harmless.contains("recomputed"));
    assert!(harmless.contains("invalidated"));
    assert!(harmless.contains("affected"));
    assert!(harmless.contains("preserved definitions:"));
    assert!(harmless.contains("affected artifacts:"));
    assert!(!harmless.contains("Modified"));

    let changed = run_lab(&[
        "diff",
        before.to_str().expect("fixture path should be utf8"),
        edited.to_str().expect("fixture path should be utf8"),
    ]);
    assert!(changed.contains("Modified"));
    assert!(changed.contains("Method"));
    assert!(changed.contains("method-lookup"));
    assert!(changed.contains("tape-program"));
}
