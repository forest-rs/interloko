//! CLI fixture tests for `interloko_lab`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_interloko_lab")
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
        .expect("interloko_lab should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf8")
}

fn run_lab_success(args: &[&str]) -> (String, String) {
    let output = Command::new(bin())
        .args(args)
        .output()
        .expect("interloko_lab should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf8"),
        String::from_utf8(output.stderr).expect("stderr should be utf8"),
    )
}

fn run_lab_success_with_env(args: &[&str], envs: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(bin());
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let output = command.output().expect("interloko_lab should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf8"),
        String::from_utf8(output.stderr).expect("stderr should be utf8"),
    )
}

fn run_lab_failure(args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .output()
        .expect("interloko_lab should run");
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stderr).expect("stderr should be utf8")
}

fn run_lab_failure_with_env(args: &[&str], envs: &[(&str, &str)]) -> String {
    let mut command = Command::new(bin());
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let output = command.output().expect("interloko_lab should run");
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
fn diagnostics_are_rendered_through_message_format_catalog() {
    let diagnostic = fixture("diagnostic_unknown_local.loko");
    let path = diagnostic.to_str().expect("fixture path should be utf8");
    let (_stdout, stderr) = run_lab_success(&["dump", path, "--stage", "sem"]);

    assert!(stderr.contains("sem.unresolved-local"));
    assert!(stderr.contains("unresolved local `missing_value`"));
}

#[test]
fn diagnostics_can_use_pseudo_catalog() {
    let diagnostic = fixture("diagnostic_unknown_local.loko");
    let path = diagnostic.to_str().expect("fixture path should be utf8");
    let (_stdout, stderr) = run_lab_success_with_env(
        &["dump", path, "--stage", "sem"],
        &[("INTERLOKO_DIAGNOSTIC_LOCALE", "pseudo")],
    );

    assert!(stderr.contains("sem.unresolved-local"));
    assert!(stderr.contains("[ps] local not found: missing"));
    assert!(!stderr.contains("unresolved local `missing`"));
}

#[test]
fn diagnostics_fail_for_missing_catalog_entries() {
    let catalog_path = std::env::temp_dir().join(format!(
        "interloko-partial-diagnostics-{}.mf2.toml",
        std::process::id()
    ));
    fs::write(
        &catalog_path,
        "[messages]\n\"sem.unresolved-class\" = \"class { $class }\"\n",
    )
    .expect("partial catalog should be written");
    let diagnostic = fixture("diagnostic_unknown_local.loko");
    let path = diagnostic.to_str().expect("fixture path should be utf8");
    let catalog = catalog_path.to_str().expect("catalog path should be utf8");

    let stderr = run_lab_failure_with_env(
        &["dump", path, "--stage", "sem"],
        &[("INTERLOKO_DIAGNOSTIC_CATALOG", catalog)],
    );
    let _ = fs::remove_file(catalog_path);

    assert!(stderr.contains("diagnostic catalog failed"));
    assert!(stderr.contains("sem.unresolved-local"));
}

#[test]
fn artifact_output_is_independent_of_diagnostic_locale() {
    let diagnostic = fixture("diagnostic_unknown_local.loko");
    let path = diagnostic.to_str().expect("fixture path should be utf8");
    let english = run_lab(&["artifacts", path]);
    let (pseudo, _stderr) = run_lab_success_with_env(
        &["artifacts", path],
        &[("INTERLOKO_DIAGNOSTIC_LOCALE", "pseudo")],
    );

    assert_eq!(english, pseudo);
}

#[test]
fn run_executes_main() {
    let file = fixture("generic_area.loko");
    let out = run_lab(&["run", file.to_str().expect("fixture path should be utf8")]);

    assert_eq!(out.trim(), "100");
}

#[test]
fn rich_values_run_and_dump_through_lab() {
    let file = fixture("rich_values.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let out = run_lab(&["run", path]);
    assert_eq!(out.trim(), "10");

    let sem = run_lab(&["dump", path, "--stage", "sem"]);
    assert!(sem.contains("literal.string"));
    assert!(sem.contains("literal.symbol"));
    assert!(sem.contains("literal.vector"));

    let lowered = run_lab(&["dump", path, "--stage", "lowered"]);
    assert!(lowered.contains("value.eq"));
    assert!(lowered.contains("literal.vector"));

    let eop = run_lab(&["dump", path, "--stage", "eop"]);
    assert!(eop.contains("<string>"));
    assert!(eop.contains("builtin-value"));
}

#[test]
fn dump_exposes_stages() {
    let file = fixture("generic_area.loko");
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
    assert!(tape.contains("program=\"interloko\""));
}

#[test]
fn mop_reports_native_and_host_backed_metaobjects() {
    let native = fixture("generic_area.loko");
    let native_out = run_lab(&["mop", native.to_str().expect("fixture path should be utf8")]);
    assert!(native_out.contains("c0"));
    assert!(native_out.contains("<circle>"));
    assert!(native_out.contains("origin=native"));
    assert!(native_out.contains("runtime=native"));
    assert!(native_out.contains("accessor"));
    assert!(native_out.contains("initialization-plan:"));
    assert!(native_out.contains("initialize-methods=deferred"));
    assert!(native_out.contains("g0 area"));
    assert!(native_out.contains(
        "m0 specializers=(c0:<circle>) specializer-origins=(native) method-origin=source"
    ));

    let host = fixture("host_backed_area.loko");
    let host_out = run_lab(&["mop", host.to_str().expect("fixture path should be utf8")]);
    assert!(host_out.contains("<rust-circle> origin=host-backed(hc0)"));
    assert!(host_out.contains("<rust-square> origin=host-backed(hc1)"));
    assert!(host_out.contains("runtime=host-backed"));
    assert!(host_out.contains("slot s0"));
    assert!(host_out.contains("storage=host-backed"));
    assert!(host_out.contains(
        "m0 specializers=(c0:<rust-circle>) specializer-origins=(host-backed(hc0)) method-origin=source"
    ));
    assert!(host_out.contains(
        "m1 specializers=(c1:<rust-square>) specializer-origins=(host-backed(hc1)) method-origin=source"
    ));
}

#[test]
fn eop_reports_environment_objects_and_affordances() {
    let host = fixture("host_backed_area.loko");
    let out = run_lab(&["eop", host.to_str().expect("fixture path should be utf8")]);

    assert!(out.contains("object source:"));
    assert!(out.contains("kind=source-file"));
    assert!(out.contains("dump-eop"));
    assert!(out.contains("object language-environment:base-loko"));
    assert!(out.contains("kind=language-environment"));
    assert!(out.contains("object grammar:base-loko"));
    assert!(out.contains("kind=grammar"));
    assert!(out.contains("object grammar-rule:base-loko/function-definition"));
    assert!(out.contains("show-grammar-rule"));
    assert!(out.contains("object grammar-activation:base-loko/source-file"));
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
    assert!(out.contains("object dispatch:d0"));
    assert!(out.contains("kind=dispatch-site"));
    assert!(out.contains("object runtime-class:c0"));
    assert!(out.contains("object artifact:"));
}

#[test]
fn inspect_reports_structured_environment_objects() {
    let file = fixture("generic_area.loko");
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
        ("dispatch:d0", "kind: dispatch-site"),
        (
            "language-environment:base-loko",
            "kind: language-environment",
        ),
        ("grammar:base-loko", "kind: grammar"),
        (
            "grammar-rule:base-loko/function-definition",
            "kind: grammar-rule",
        ),
        (
            "grammar-activation:base-loko/source-file",
            "kind: grammar-activation",
        ),
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

    let diagnostic = fixture("diagnostic_unknown_local.loko");
    let diagnostic_path = diagnostic.to_str().expect("fixture path should be utf8");
    let out = run_lab(&["inspect", diagnostic_path, "diagnostic:d0"]);
    assert!(out.contains("object diagnostic:d0"));
    assert!(out.contains("kind: diagnostic"));
    assert!(out.contains("sem.unresolved-local"));
    assert!(out.contains("args=[local=missing_value]"));
    assert!(!out.contains("rendered="));
}

#[test]
fn inspect_reports_richer_slot_metadata() {
    let file = fixture("slot_defaults.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let out = run_lab(&["inspect", path, "slot:c0.s0"]);
    assert!(out.contains("kind: slot"));
    assert!(out.contains("type=<integer>"));
    assert!(out.contains("init-keyword=r: required"));
    assert!(out.contains("init-value=yes"));
    assert!(out.contains("allocation=instance"));
    assert!(out.contains("mutability=constant"));
}

#[test]
fn inspect_reports_inherited_effective_slots() {
    let file = fixture("inherited_point.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let mop = run_lab(&["dump", path, "--stage", "mop"]);
    assert!(mop.contains("<point>"));
    assert!(mop.contains("superclass=c0:<shape>"));
    assert!(mop.contains("chain=c1:<point> -> c0:<shape> -> <object>"));
    assert!(mop.contains("effective-slots=2"));
    assert!(mop.contains("source=inherited c0.s0"));
    assert!(mop.contains("c1 <point> kind=native superclass=c0:<shape>"));
    assert!(mop.contains("s0 x kind=integer storage=native source=inherited source-slot=c0.s0"));

    let class = run_lab(&["inspect", path, "class:c1"]);
    assert!(class.contains("superclass=c0:<shape>"));
    assert!(class.contains("chain=c1:<point> -> c0:<shape> -> <object>"));
    assert!(class.contains("finalized=true"));
    assert!(class.contains("direct-slots=1"));
    assert!(class.contains("effective-slots=2"));
    assert!(class.contains("init-plan=[s0:x:inherited:required"));
    assert!(class.contains("accepted-keywords=[x:,y:]"));
    assert!(class.contains("initialize-methods=deferred"));
    assert!(class.contains("override=explicit-init-args-over-defaults"));

    let inherited = run_lab(&["inspect", path, "slot:c1.s0"]);
    assert!(inherited.contains("source=inherited"));
    assert!(inherited.contains("source-slot=class:c0.s0"));
    assert!(inherited.contains("init-keyword=x: required"));
}

#[test]
fn mop_reports_dispatch_applicability_for_inheritance() {
    let file = fixture("inherited_dispatch.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let mop = run_lab(&["mop", path]);
    assert!(mop.contains("method m0"));
    assert!(mop.contains("applicable=([c0:<shape>@0,c1:<circle>@1])"));
    assert!(mop.contains("method m1"));
    assert!(mop.contains("applicable=([c1:<circle>@0])"));
    assert!(mop.contains(
        "m1 specializers=(c1:<circle>) specializer-origins=(native) method-origin=source applies-to=([c1:<circle>@0])"
    ));
    assert!(mop.contains(
        "m0 specializers=(c0:<shape>) specializer-origins=(native) method-origin=source applies-to=([c0:<shape>@0,c1:<circle>@1])"
    ));

    let generic = run_lab(&["inspect", path, "generic:g0"]);
    assert!(generic.contains("dispatch=m1=>([c1:<circle>@0]);m0=>([c0:<shape>@0,c1:<circle>@1])"));

    let method = run_lab(&["inspect", path, "method:m0"]);
    assert!(method.contains("applies-to=([c0:<shape>@0,c1:<circle>@1])"));
}

#[test]
fn dispatch_command_lists_and_explains_dispatch_sites() {
    let file = fixture("inherited_dispatch.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let list = run_lab(&["dispatch", path]);
    assert!(list.contains("dispatch sites for"));
    assert!(list.contains("dispatch:d0"));
    assert!(list.contains("owner=function main"));
    assert!(list.contains("generic=g0:score"));
    assert!(list.contains("static-args=(object(<circle>))"));
    assert!(list.contains("selected=m1:score(c1:<circle>)"));

    let explanation = run_lab(&["dispatch", path, "dispatch:d0"]);
    assert!(explanation.contains("dispatch dispatch:d0"));
    assert!(explanation.contains("owner: function main"));
    assert!(explanation.contains("contract=score(x:object(<shape>)) -> integer"));
    assert!(explanation.contains("static-kinds: (object(<circle>))"));
    assert!(explanation.contains("m1:score(c1:<circle>)"));
    assert!(explanation.contains("selected"));
    assert!(explanation.contains("m0:score(c0:<shape>)"));
    assert!(explanation.contains("shadowed"));
    assert!(explanation.contains("dispatcher: score#dispatch"));
    assert!(explanation.contains("generic-dispatcher"));
    assert!(explanation.contains("tape-program"));

    let err = run_lab_failure(&["dispatch", path, "dispatch:missing"]);
    assert!(err.contains("unknown dispatch site `dispatch:missing`"));
    assert!(err.contains("available dispatch sites: dispatch:d0"));
}

#[test]
fn dispatch_command_shows_multi_argument_vectors() {
    let file = fixture("multi_dispatch.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let list = run_lab(&["dispatch", path]);
    assert!(list.contains("generic=g0:collide"));
    assert!(list.contains("static-args=(object(<circle>), object(<square>))"));
    assert!(list.contains("selected=m1:collide(c1:<circle>, c2:<square>)"));

    let explanation = run_lab(&["dispatch", path, "dispatch:d0"]);
    assert!(explanation.contains("static-kinds: (object(<circle>), object(<square>))"));
    assert!(explanation.contains("exact-classes: (c1:<circle>, c2:<square>)"));
    assert!(explanation.contains("specializers=(c1:<circle>, c2:<square>)"));
    assert!(explanation.contains("distances=(0,0) selected"));
    assert!(explanation.contains("specializers=(c0:<shape>, c0:<shape>)"));
    assert!(explanation.contains("distances=(1,1) shadowed"));
}

#[test]
fn affordances_lists_stable_object_actions_and_unknown_ids_fail() {
    let file = fixture("generic_area.loko");
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
    let file = fixture("generic_area.loko");
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

    let dispatch_site = run_lab(&["invoke-affordance", path, "dispatch:d0", "show-dispatch"]);
    assert!(dispatch_site.contains("dispatch dispatch:d0"));
    assert!(dispatch_site.contains("dispatcher: area#dispatch"));

    let runtime = run_lab(&["invoke-affordance", path, "class:c0", "show-runtime-layout"]);
    assert!(runtime.contains("runtime layout:"));
    assert!(runtime.contains("c0 <circle>"));

    let deps = run_lab(&["invoke-affordance", path, "method:m0", "show-dependencies"]);
    assert!(deps.contains("artifact graph"));
    assert!(deps.contains("dependencies:"));

    let sem = run_lab(&["invoke-affordance", path, "method:m0", "dump-sem"]);
    assert!(sem.contains("sem"));
    assert!(sem.contains("method m0"));

    let grammar_stack = run_lab(&[
        "invoke-affordance",
        path,
        "grammar:base-loko",
        "show-grammar-stack",
    ]);
    assert!(grammar_stack.contains("grammar stack"));
    assert!(grammar_stack.contains("BaseLokoGrammar"));

    let grammar_rule = run_lab(&[
        "invoke-affordance",
        path,
        "grammar-rule:base-loko/function-definition",
        "show-grammar-rule",
    ]);
    assert!(grammar_rule.contains("grammar rule grammar-rule:base-loko/function-definition"));
    assert!(grammar_rule.contains("syntax.rs:Parser::parse_function"));

    let parse_trace = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:base-loko",
        "show-parse-trace",
    ]);
    assert!(parse_trace.contains("parse trace"));
    assert!(parse_trace.contains("rule provenance:"));
}

#[test]
fn invoke_affordance_runs_dump_and_run_actions() {
    let file = fixture("generic_area.loko");
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
        ("dump-tape", "program=\"interloko\""),
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
    let plain = fixture("generic_area.loko");
    let plain_out = run_lab(&["eop", plain.to_str().expect("fixture path should be utf8")]);
    assert!(!plain_out.contains("host-class:rust-shape"));
    assert!(!plain_out.contains("host-instance:circle-10"));

    let projected = fixture("host_projected_environment.loko");
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
    let file = fixture("generic_area.loko");
    let out = run_lab(&[
        "artifacts",
        file.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(out.contains("artifact graph"));
    assert!(out.contains("art"));
    assert!(out.contains("environment-resolution"));
    assert!(out.contains("grammar-stack"));
    assert!(out.contains("grammar-parse"));
    assert!(out.contains("parse <- grammar-parse"));
    assert!(out.contains("method-lookup"));
    assert!(out.contains("dependencies:"));
}

#[test]
fn diff_reports_preserved_and_focused_method_changes() {
    let before = fixture("generic_area.loko");
    let trivia = fixture("generic_area_trivia.loko");
    let edited = fixture("generic_area_method_edit.loko");

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

#[test]
fn diff_reports_parent_slot_edit_affecting_child_artifacts() {
    let before = fixture("inherited_point_parent_before.loko");
    let after = fixture("inherited_point_parent_after.loko");

    let changed = run_lab(&[
        "diff",
        before.to_str().expect("fixture path should be utf8"),
        after.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(changed.contains("Modified"));
    assert!(changed.contains("Slot <shape>.x"));
    assert!(changed.contains("Class <point>"));
    assert!(changed.contains("lowered-module"));
    assert!(changed.contains("tape-program"));
}
