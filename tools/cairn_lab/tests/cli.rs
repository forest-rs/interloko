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

fn run_lab_failure(args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .output()
        .expect("cairn_lab should run");
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stderr).expect("stderr should be utf8")
}

fn object_id_with_prefix(eop: &str, prefix: &str) -> String {
    eop.lines()
        .filter_map(|line| line.trim().strip_prefix("object "))
        .find(|id| id.starts_with(prefix))
        .expect("object id should exist")
        .to_string()
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

    let eop = run_lab(&["dump", path, "--stage", "eop"]);
    assert!(eop.contains("eop"));
    assert!(eop.contains("objects:"));
    assert!(eop.contains("object source:"));

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
fn eop_reports_environment_objects_and_affordances() {
    let host = fixture("host_backed_area.cairn");
    let out = run_lab(&["eop", host.to_str().expect("fixture path should be utf8")]);

    assert!(out.contains("object source:"));
    assert!(out.contains("kind=source-file"));
    assert!(out.contains("dump-eop"));
    assert!(out.contains("object class:c0"));
    assert!(out.contains("origin=host-backed storage=host-registry"));
    assert!(out.contains("object slot:c0.s0"));
    assert!(out.contains("storage=host-adapter"));
    assert!(out.contains("object accessor:c0.s0"));
    assert!(out.contains("execution callable=known"));
    assert!(out.contains("object generic:g0"));
    assert!(out.contains("show-dispatch"));
    assert!(out.contains("object method:m0"));
    assert!(out.contains("participates-in=generic:g0"));
    assert!(out.contains("object function:f0"));
    assert!(out.contains("object runtime-class:c0"));
    assert!(out.contains("object artifact:"));
}

#[test]
fn inspect_reports_structured_environment_objects() {
    let file = fixture("generic_area.cairn");
    let path = file.to_str().expect("fixture path should be utf8");
    let eop = run_lab(&["eop", path]);
    let source_id = object_id_with_prefix(&eop, "source:");
    let artifact_id = object_id_with_prefix(&eop, "artifact:");

    for (object_id, expected) in [
        (source_id.as_str(), "kind: source-file"),
        ("class:c0", "kind: class"),
        ("slot:c0.s0", "kind: slot"),
        ("accessor:c0.s0", "kind: accessor"),
        ("generic:g0", "kind: generic"),
        ("method:m0", "kind: method"),
        ("function:f0", "kind: function"),
        ("runtime-class:c0", "kind: runtime-layout-row"),
        ("runtime-slot:c0.s0", "class: <runtime-slot>"),
        (artifact_id.as_str(), "kind: artifact"),
    ] {
        let out = run_lab(&["inspect", path, object_id]);
        assert!(out.contains(&format!("object {object_id}")));
        assert!(out.contains(expected));
        assert!(out.contains("presentation:"));
        assert!(out.contains("affordances:"));
        assert!(out.contains("inspection:"));
    }

    let diagnostic = fixture("diagnostic_unknown_local.cairn");
    let diagnostic_path = diagnostic.to_str().expect("fixture path should be utf8");
    let out = run_lab(&["inspect", diagnostic_path, "diagnostic:d0"]);
    assert!(out.contains("object diagnostic:d0"));
    assert!(out.contains("kind: diagnostic"));
    assert!(out.contains("unresolved local"));
}

#[test]
fn affordances_lists_stable_object_actions_and_unknown_ids_fail() {
    let file = fixture("generic_area.cairn");
    let path = file.to_str().expect("fixture path should be utf8");

    let out = run_lab(&["affordances", path, "generic:g0"]);
    assert!(out.contains("affordances generic:g0"));
    assert!(out.contains("inspect"));
    assert!(out.contains("show-dispatch"));
    assert!(out.contains("add-method"));

    let err = run_lab_failure(&["affordances", path, "missing:object"]);
    assert!(err.contains("unknown object id `missing:object`"));
    assert!(err.contains("available object ids:"));
}

#[test]
fn invoke_affordance_runs_read_only_views() {
    let file = fixture("generic_area.cairn");
    let path = file.to_str().expect("fixture path should be utf8");

    let accessor = run_lab(&["invoke-affordance", path, "slot:c0.s0", "show-accessor"]);
    assert!(accessor.contains("object accessor:c0.s0"));
    assert!(accessor.contains("reads=slot:c0.s0"));

    let slot = run_lab(&["invoke-affordance", path, "accessor:c0.s0", "show-slot"]);
    assert!(slot.contains("object slot:c0.s0"));
    assert!(slot.contains("show-accessor"));

    let dispatch = run_lab(&["invoke-affordance", path, "generic:g0", "show-dispatch"]);
    assert!(dispatch.contains("dispatch table:"));
    assert!(dispatch.contains("g0 area"));

    let runtime = run_lab(&["invoke-affordance", path, "class:c0", "show-runtime-layout"]);
    assert!(runtime.contains("runtime layout:"));
    assert!(runtime.contains("c0 <circle>"));

    let deps = run_lab(&["invoke-affordance", path, "method:m0", "show-dependencies"]);
    assert!(deps.contains("artifact graph"));
    assert!(deps.contains("dependencies:"));

    let sem = run_lab(&["invoke-affordance", path, "method:m0", "dump-sem"]);
    assert!(sem.contains("sem"));
    assert!(sem.contains("method m0"));
}

#[test]
fn invoke_affordance_runs_dump_and_run_actions() {
    let file = fixture("generic_area.cairn");
    let path = file.to_str().expect("fixture path should be utf8");
    let eop = run_lab(&["eop", path]);
    let source_id = object_id_with_prefix(&eop, "source:");

    for (affordance, expected) in [
        ("dump-syntax", "syntax"),
        ("dump-ast", "ast"),
        ("dump-sem", "sem"),
        ("dump-mop", "mop"),
        ("dump-eop", "objects:"),
        ("dump-lowered", "area#dispatch"),
        ("dump-tape", "program=\"cairn\""),
    ] {
        let out = run_lab(&["invoke-affordance", path, source_id.as_str(), affordance]);
        assert!(out.contains(expected), "{affordance} output was {out}");
    }

    let run = run_lab(&["invoke-affordance", path, "function:f0", "run"]);
    assert_eq!(run.trim(), "100");

    let unavailable = run_lab_failure(&["invoke-affordance", path, "class:c0", "run"]);
    assert!(unavailable.contains("object `class:c0` does not expose affordance `run`"));

    let unsupported = run_lab_failure(&["invoke-affordance", path, "method:m0", "edit-body"]);
    assert!(unsupported.contains("affordance `edit-body` is not executable"));
}

#[test]
fn host_projection_records_are_explicit_and_inspectable() {
    let plain = fixture("generic_area.cairn");
    let plain_out = run_lab(&["eop", plain.to_str().expect("fixture path should be utf8")]);
    assert!(!plain_out.contains("host-class:rust-shape"));
    assert!(!plain_out.contains("host-instance:circle-10"));

    let projected = fixture("host_projected_environment.cairn");
    let path = projected.to_str().expect("fixture path should be utf8");
    let eop = run_lab(&["eop", path]);
    assert!(eop.contains("object host-class:rust-shape"));
    assert!(eop.contains("class=<host-class> origin=host-provided storage=host-owned"));
    assert!(eop.contains("object host-instance:circle-10"));
    assert!(eop.contains("class=<host-instance> origin=host-provided storage=host-owned"));

    let class = run_lab(&["inspect", path, "host-class:rust-shape"]);
    assert!(class.contains("kind: host-object"));
    assert!(class.contains("class: <host-class>"));
    assert!(class.contains("provider=fixture"));

    let instance = run_lab(&["inspect", path, "host-instance:circle-10"]);
    assert!(instance.contains("class: <host-instance>"));
    assert!(instance.contains("facts=radius=10"));

    let affordances = run_lab(&["affordances", path, "host-instance:circle-10"]);
    assert!(affordances.contains("inspect"));
    assert!(affordances.contains("dump-eop"));
    assert!(affordances.contains("show-runtime-layout"));
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
