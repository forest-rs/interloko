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

fn normalize_timing_nanos(output: &str) -> String {
    let mut normalized = String::new();
    for line in output.lines() {
        if let Some((prefix, _)) = line.split_once(" ns=") {
            normalized.push_str(prefix);
            normalized.push_str(" ns=<timing>");
        } else {
            normalized.push_str(line);
        }
        normalized.push('\n');
    }
    normalized
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

    assert!(english.contains("work="));
    assert!(english.contains("ticks="));
    assert!(english.contains("ns="));
    assert_eq!(
        normalize_timing_nanos(&english),
        normalize_timing_nanos(&pseudo)
    );
}

#[test]
fn run_executes_main() {
    let file = fixture("generic_area.loko");
    let out = run_lab(&["run", file.to_str().expect("fixture path should be utf8")]);

    assert_eq!(out.trim(), "100");
}

#[test]
fn run_and_trace_commands_expose_execution_trace() {
    let file = fixture("generic_area.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let run = run_lab(&["run", "--trace", path]);
    assert!(run.starts_with("100\nexecution trace:"));
    assert!(run.contains("run-start"));
    assert!(run.contains("run-end ok"));
    assert!(run.contains("call-frame"));
    assert!(run.contains("host-call"));

    let trace = run_lab(&["trace", path]);
    assert!(trace.starts_with("execution trace:"));
    assert!(trace.contains("run-start"));
    assert!(trace.contains("host-call"));
    assert!(!trace.contains("instr func="));

    let instruction_trace = run_lab(&["trace", "--instructions", path]);
    assert!(instruction_trace.contains("instr func="));
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
        "m0 specializers=(c1:<circle>) specializer-origins=(native) method-origin=source"
    ));

    let host = fixture("host_backed_area.loko");
    let host_out = run_lab(&["mop", host.to_str().expect("fixture path should be utf8")]);
    assert!(host_out.contains("<rust-circle> origin=host-backed(hc0)"));
    assert!(host_out.contains("<rust-square> origin=host-backed(hc1)"));
    assert!(host_out.contains("runtime=host-backed"));
    assert!(host_out.contains("slot s0"));
    assert!(host_out.contains("storage=host-backed"));
    assert!(host_out.contains(
        "m0 specializers=(c1:<rust-circle>) specializer-origins=(host-backed(hc0)) method-origin=source"
    ));
    assert!(host_out.contains(
        "m1 specializers=(c2:<rust-square>) specializer-origins=(host-backed(hc1)) method-origin=source"
    ));
}

#[test]
fn eop_reports_environment_objects_and_affordances() {
    let host = fixture("host_backed_area.loko");
    let out = run_lab(&["eop", host.to_str().expect("fixture path should be utf8")]);

    assert!(out.contains("object source:"));
    assert!(out.contains("kind=source-file"));
    assert!(out.contains("dump-eop"));
    assert!(out.contains("object grammar-registry:session"));
    assert!(out.contains("kind=grammar-registry"));
    assert!(out.contains("show-grammar-registry"));
    assert!(out.contains("object registered-grammar-environment:base-loko"));
    assert!(out.contains("object registered-grammar-environment:derived-host"));
    assert!(out.contains("object staged-grammar-adapter:calculator"));
    assert!(out.contains("object staged-grammar-adapter:records"));
    assert!(out.contains("object grammar-extension:calculator"));
    assert!(out.contains("object grammar-extension:records"));
    assert!(out.contains("show-grammar-extension"));
    assert!(out.contains("object language-environment:base-loko"));
    assert!(out.contains("kind=language-environment"));
    assert!(out.contains("object grammar:base-loko"));
    assert!(out.contains("kind=grammar"));
    assert!(out.contains("show-grammars"));
    assert!(out.contains("show-direct-rules"));
    assert!(out.contains("show-effective-rules"));
    assert!(out.contains("object grammar-rule:base-loko/function-definition"));
    assert!(out.contains("show-grammar-rule"));
    assert!(out.contains("object grammar-activation:base-loko/source-file"));
    assert!(out.contains("object class:c0"));
    assert!(out.contains("origin=compiler-generated storage=cached-artifact"));
    assert!(out.contains("object class:c1"));
    assert!(out.contains("origin=host-backed storage=host-registry"));
    assert!(out.contains("object slot:c1.s0"));
    assert!(out.contains("storage=host-adapter"));
    assert!(out.contains("object accessor:c1.s0"));
    assert!(out.contains("execution callable=known"));
    assert!(out.contains("object generic:g0"));
    assert!(out.contains("show-dispatch"));
    assert!(out.contains("object method:m0"));
    assert!(out.contains("participates-in=generic:g0"));
    assert!(out.contains("object function:f0"));
    assert!(out.contains("object dispatch:d0"));
    assert!(out.contains("kind=dispatch-site"));
    assert!(out.contains("object runtime-class:c0"));
    assert!(out.contains("object runtime-class:c1"));
    assert!(out.contains("object artifact:"));
}

#[test]
fn extensions_command_lists_descriptors_activations_emissions_and_provenance() {
    let file = fixture("staged_records.loko");
    let out = run_lab(&[
        "extensions",
        file.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(out.contains("grammar extensions"));
    assert!(out.contains("grammar-extension:records"));
    assert!(out.contains("capabilities=emit-frontend-forms"));
    assert!(out.contains("emits=frontend-class"));
    assert!(out.contains("activations:"));
    assert!(out.contains("grammar-extension-activation:records/r0"));
    assert!(out.contains("emissions:"));
    assert!(out.contains("grammar-extension-emission:records/r0/f0"));
    assert!(out.contains("provenance:"));
    assert!(out.contains("grammar-extension-provenance:records/r0/record-definition"));
}

#[test]
fn registry_command_lists_registered_frontend_entries() {
    let file = fixture("staged_records.loko");
    let out = run_lab(&[
        "registry",
        file.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(out.contains("grammar registry"));
    assert!(out.contains("environments:"));
    assert!(out.contains("base-loko BaseLokoEnvironment"));
    assert!(out.contains("derived-host DerivedHostEnvironment"));
    assert!(out.contains("staged adapters:"));
    assert!(out.contains("calculator CalculatorRegionGrammar"));
    assert!(out.contains("records RecordsRegionGrammar"));
}

#[test]
fn grammar_header_selects_derived_host_stack() {
    let file = fixture("derived_grammar.loko");
    let path = file.to_str().expect("fixture path should be utf8");
    let eop = run_lab(&["eop", path]);

    assert!(eop.contains("object language-environment:derived-host"));
    assert!(eop.contains("object grammar:derived-host"));
    assert!(eop.contains("object grammar-rule:derived-host/function-definition"));
    assert!(eop.contains("origin=host-provided storage=cached-artifact"));
    assert!(eop.contains("data=host-grammar-rule"));
    assert!(eop.contains("object effective-grammar-rule:derived-host/function-definition"));
    assert!(eop.contains("resolution=override"));
    assert!(eop.contains("object effective-grammar-rule:derived-host/class-definition"));
    assert!(eop.contains("resolution=inherited"));

    let stack = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:derived-host",
        "show-grammar-stack",
    ]);
    assert!(stack.contains("BaseLokoGrammar"));
    assert!(stack.contains("DerivedHostGrammar"));

    let rule = run_lab(&[
        "invoke-affordance",
        path,
        "effective-grammar-rule:derived-host/function-definition",
        "show-grammar-rule",
    ]);
    assert!(rule.contains("effective grammar rule"));
    assert!(rule.contains("resolution override"));
}

#[test]
fn staged_calculator_region_runs_and_traces_activation_boundary() {
    let file = fixture("staged_calculator.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let run = run_lab(&["run", path]);
    assert_eq!(run.trim(), "42");

    let eop = run_lab(&["eop", path]);
    assert!(eop.contains("object language-environment:staged-loko"));
    assert!(eop.contains("object grammar:calculator"));
    assert!(eop.contains("object grammar-activation:base-loko/source-file"));
    assert!(eop.contains("object grammar-activation:calculator/r0"));
    assert!(eop.contains("object grammar-activation:calculator/r1"));
    assert!(eop.contains("object staged-region:calculator/r1"));
    assert!(eop.contains("object grammar-extension-activation:calculator/r1"));
    assert!(eop.contains("object grammar-extension-emission:calculator/r1/f0"));
    assert!(eop.contains("object grammar-extension-provenance:calculator/r1/calculator-function"));
    assert!(eop.contains("adapter=calculator"));
    assert!(eop.contains("directive="));

    let trace = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:staged-loko",
        "show-parse-trace",
    ]);
    assert!(trace.contains("CalculatorRegionGrammar"));
    assert!(trace.contains("staged regions:"));
    assert!(trace.contains("extension activations:"));
    assert!(trace.contains("extension emissions:"));
    assert!(trace.contains("extension provenance:"));
    assert!(trace.contains("forms=[frontend-function(name: main"));
    assert!(trace.contains("calculator-function(name: main"));
    assert!(trace.contains("adapter=calculator"));
    assert!(trace.contains("directive="));
    assert!(trace.contains("define function main()"));
    assert!(
        trace.contains("calculator staged region emitted frontend forms for direct AST lowering")
    );
}

#[test]
fn staged_records_region_runs_and_projects_classes() {
    let file = fixture("staged_records.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let run = run_lab(&["run", path]);
    assert_eq!(run.trim(), "143");

    let eop = run_lab(&["eop", path]);
    assert!(eop.contains("object language-environment:staged-loko"));
    assert!(eop.contains("object grammar:records"));
    assert!(eop.contains("object staged-region:records/r0"));
    assert!(eop.contains("object grammar-extension-activation:records/r0"));
    assert!(eop.contains("object grammar-extension-emission:records/r0/f0"));
    assert!(eop.contains("object grammar-extension-provenance:records/r0/record-field"));
    assert!(eop.contains("generated=class:c"));
    assert!(eop.contains("frontend-class(name: <circle>"));
    assert!(eop.contains("object class:c"));
    assert!(eop.contains("presentation title=\"<circle>\""));
    assert!(eop.contains("slot radius"));

    let trace = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:staged-loko",
        "show-parse-trace",
    ]);
    assert!(trace.contains("RecordsRegionGrammar"));
    assert!(trace.contains("extension emissions:"));
    assert!(trace.contains("forms=[frontend-class(name: <circle>"));
    assert!(trace.contains("materialized=\"define class <circle>"));
    assert!(trace.contains("records-region(grammar: RecordsRegionGrammar"));
}

#[test]
fn report_generates_staged_records_html() {
    let file = fixture("staged_records.loko");
    let path = file.to_str().expect("fixture path should be utf8");
    let report = std::env::temp_dir().join(format!(
        "interloko-staged-records-report-{}.html",
        std::process::id()
    ));
    let report_path = report.to_str().expect("report path should be utf8");
    let _ = fs::remove_file(&report);

    let stdout = run_lab(&["report", path, "--output", report_path]);
    assert!(stdout.contains("wrote "));

    let html = fs::read_to_string(&report).expect("report should be written");
    let _ = fs::remove_file(&report);

    assert!(html.contains("Interloko compiler workbench report"));
    assert!(html.contains("staged_records.loko"));
    assert!(html.contains("This is the file the compiler saw."));
    assert!(html.contains("object-model view of what the compiler built"));
    assert!(html.contains("Runtime result"));
    assert!(html.contains("returned 143"));
    assert!(html.contains("Active Language Environment"));
    assert!(html.contains("language-environment:staged-loko"));
    assert!(html.contains("Host grammar catalog"));
    assert!(html.contains("id=\"artifact-pipeline\""));
    assert!(html.contains("Compiler Artifact Path"));
    assert!(html.contains("<dt>time</dt>"));
    assert!(html.contains("<dt>work</dt>"));
    assert!(html.contains("id=\"source-line-"));
    assert!(html.contains("id=\"object-class-c"));
    assert!(html.contains("href=\"#object-class-c"));
    assert!(html.contains("id=\"artifact-art"));
    assert!(html.contains("href=\"#artifact-art"));
    assert!(html.contains("id=\"interloko-report-data\""));
    assert!(html.contains("\"stagedRegions\""));
    assert!(html.contains("\"sourceSpan\""));
    assert!(html.contains("\"artifactKeys\""));
    assert!(html.contains("\"elapsedNanos\""));
    assert!(html.contains("\"executionTrace\""));
    assert!(html.contains("Expansion Chain"));
    assert!(html.contains("RecordsRegionGrammar"));
    assert!(html.contains("frontend-class"));
    assert!(html.contains("flow:records:r0:0"));
    assert!(html.contains("artifact-stage:lowered"));
    assert!(html.contains("artifact-stage:tape"));
    assert!(html.contains("define class &lt;circle&gt;"));
    assert!(html.contains("ast-class:"));
    assert!(html.contains("class:c"));
    assert!(html.contains("slot:c"));
    assert!(html.contains("class=\"meta-chip\" title=\"Class metaobject"));
    assert!(html.contains("<dt>superclass</dt><dd>&lt;object&gt;</dd>"));
    assert!(html.contains("<dt>init keys</dt><dd>radius:</dd>"));
    assert!(html.contains("Raw inspection facts"));
    assert!(html.contains("No diagnostics were emitted."));
    assert!(html.contains("Artifact Graph"));
    assert!(html.contains("Execution Timeline"));
    assert!(html.contains("Raw Execution Trace"));
    assert!(html.contains("host calls"));
    assert!(html.contains("Lowered IR"));
    assert!(html.contains("Tape Disassembly"));
    assert!(html.contains("source-mark--activation"));
    assert!(html.contains("activation directive"));
    assert!(html.contains("frontend form source"));
}

#[test]
fn report_handles_plain_loko_sources() {
    let file = fixture("generic_area.loko");
    let path = file.to_str().expect("fixture path should be utf8");
    let report = std::env::temp_dir().join(format!(
        "interloko-generic-area-report-{}.html",
        std::process::id()
    ));
    let report_path = report.to_str().expect("report path should be utf8");
    let _ = fs::remove_file(&report);

    let stdout = run_lab(&["report", path, "--output", report_path]);
    assert!(stdout.contains("wrote "));

    let html = fs::read_to_string(&report).expect("report should be written");
    let _ = fs::remove_file(&report);

    assert!(html.contains("generic_area.loko"));
    assert!(html.contains("returned 100"));
    assert!(html.contains("No staged regions were activated in this source."));
    assert!(html.contains("class:c0"));
    assert!(html.contains("&lt;object&gt;"));
    assert!(html.contains("class:c1"));
    assert!(html.contains("generic:g0"));
}

#[test]
fn parse_trace_reports_grammar_diagnostic_facts() {
    let file = fixture("derived_grammar_syntax_error.loko");
    let path = file.to_str().expect("fixture path should be utf8");
    let out = run_lab_success(&[
        "invoke-affordance",
        path,
        "language-environment:derived-host",
        "show-parse-trace",
    ])
    .0;

    assert!(out.contains("diagnostics:"));
    assert!(out.contains("code=syntax.expected-expression"));
    assert!(out.contains("rule-name=body"));
    assert!(out.contains("grammar=grm"));
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
        ("class:c0", "title: <object>"),
        ("class:c1", "kind: class"),
        ("slot:c1.s0", "kind: slot"),
        ("accessor:c1.s0", "kind: accessor"),
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
        ("runtime-class:c0", "title: runtime <object>"),
        ("runtime-class:c1", "kind: runtime-layout-row"),
        ("runtime-slot:c1.s0", "class: <runtime-slot>"),
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

    let out = run_lab(&["inspect", path, "slot:c1.s0"]);
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
    assert!(mop.contains("superclass=c1:<shape>"));
    assert!(mop.contains("chain=c2:<point> -> c1:<shape> -> c0:<object>"));
    assert!(mop.contains("effective-slots=2"));
    assert!(mop.contains("source=inherited c1.s0"));
    assert!(mop.contains("c2 <point> kind=native superclass=c1:<shape>"));
    assert!(mop.contains(
        "s0 x kind=integer storage=native readable=true writable=true write-rejection=none source=inherited source-slot=c1.s0"
    ));

    let class = run_lab(&["inspect", path, "class:c2"]);
    assert!(class.contains("superclass=c1:<shape>"));
    assert!(class.contains("chain=c2:<point> -> c1:<shape> -> c0:<object>"));
    assert!(class.contains("finalized=true"));
    assert!(class.contains("direct-slots=1"));
    assert!(class.contains("effective-slots=2"));
    assert!(class.contains("init-plan=[s0:x:inherited:required"));
    assert!(class.contains("accepted-keywords=[x:,y:]"));
    assert!(class.contains("initialize-methods=deferred"));
    assert!(class.contains("override=explicit-init-args-over-defaults"));

    let inherited = run_lab(&["inspect", path, "slot:c2.s0"]);
    assert!(inherited.contains("source=inherited"));
    assert!(inherited.contains("source-slot=class:c1.s0"));
    assert!(inherited.contains("init-keyword=x: required"));
}

#[test]
fn mop_reports_dispatch_applicability_for_inheritance() {
    let file = fixture("inherited_dispatch.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let mop = run_lab(&["mop", path]);
    assert!(mop.contains("method m0"));
    assert!(mop.contains("applicable=([c1:<shape>@0,c2:<circle>@1])"));
    assert!(mop.contains("method m1"));
    assert!(mop.contains("applicable=([c2:<circle>@0])"));
    assert!(mop.contains(
        "m1 specializers=(c2:<circle>) specializer-origins=(native) method-origin=source applies-to=([c2:<circle>@0])"
    ));
    assert!(mop.contains(
        "m0 specializers=(c1:<shape>) specializer-origins=(native) method-origin=source applies-to=([c1:<shape>@0,c2:<circle>@1])"
    ));

    let generic = run_lab(&["inspect", path, "generic:g0"]);
    assert!(generic.contains("dispatch=m1=>([c2:<circle>@0]);m0=>([c1:<shape>@0,c2:<circle>@1])"));

    let method = run_lab(&["inspect", path, "method:m0"]);
    assert!(method.contains("applies-to=([c1:<shape>@0,c2:<circle>@1])"));
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
    assert!(list.contains("selected=m1:score(c2:<circle>)"));

    let explanation = run_lab(&["dispatch", path, "dispatch:d0"]);
    assert!(explanation.contains("dispatch dispatch:d0"));
    assert!(explanation.contains("owner: function main"));
    assert!(explanation.contains("contract=score(x:object(<shape>)) -> integer"));
    assert!(explanation.contains("static-kinds: (object(<circle>))"));
    assert!(explanation.contains("m1:score(c2:<circle>)"));
    assert!(explanation.contains("selected"));
    assert!(explanation.contains("m0:score(c1:<shape>)"));
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
    assert!(list.contains("selected=m1:collide(c2:<circle>, c3:<square>)"));

    let explanation = run_lab(&["dispatch", path, "dispatch:d0"]);
    assert!(explanation.contains("static-kinds: (object(<circle>), object(<square>))"));
    assert!(explanation.contains("exact-classes: (c2:<circle>, c3:<square>)"));
    assert!(explanation.contains("specializers=(c2:<circle>, c3:<square>)"));
    assert!(explanation.contains("distances=(0,0) selected"));
    assert!(explanation.contains("specializers=(c1:<shape>, c1:<shape>)"));
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

    let accessor = run_lab(&["invoke-affordance", path, "slot:c1.s0", "show-accessor"]);
    assert!(accessor.contains("object accessor:c1.s0"));
    assert!(accessor.contains("reads=slot:c1.s0"));

    let slot = run_lab(&["invoke-affordance", path, "accessor:c1.s0", "show-slot"]);
    assert!(slot.contains("object slot:c1.s0"));
    assert!(slot.contains("show-accessor"));

    let dispatch = run_lab(&["invoke-affordance", path, "generic:g0", "show-dispatch"]);
    assert!(dispatch.contains("dispatch table:"));
    assert!(dispatch.contains("g0 area"));

    let dispatch_site = run_lab(&["invoke-affordance", path, "dispatch:d0", "show-dispatch"]);
    assert!(dispatch_site.contains("dispatch dispatch:d0"));
    assert!(dispatch_site.contains("dispatcher: area#dispatch"));

    let runtime = run_lab(&["invoke-affordance", path, "class:c1", "show-runtime-layout"]);
    assert!(runtime.contains("runtime layout:"));
    assert!(runtime.contains("c1 <circle>"));

    let deps = run_lab(&["invoke-affordance", path, "method:m0", "show-dependencies"]);
    assert!(deps.contains("artifact graph"));
    assert!(deps.contains("dependencies:"));

    let sem = run_lab(&["invoke-affordance", path, "method:m0", "dump-sem"]);
    assert!(sem.contains("sem"));
    assert!(sem.contains("sem object method:m0"));

    let grammar_stack = run_lab(&[
        "invoke-affordance",
        path,
        "grammar:base-loko",
        "show-grammar-stack",
    ]);
    assert!(grammar_stack.contains("grammar stack"));
    assert!(grammar_stack.contains("BaseLokoGrammar"));

    let grammars = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:base-loko",
        "show-grammars",
    ]);
    assert!(grammars.contains("language environment grammars"));
    assert!(grammars.contains("grammar:base-loko"));

    let direct_rules = run_lab(&[
        "invoke-affordance",
        path,
        "grammar:base-loko",
        "show-direct-rules",
    ]);
    assert!(direct_rules.contains("direct grammar rules grammar:base-loko"));
    assert!(direct_rules.contains("grammar-rule:base-loko/function-definition"));

    let effective_rules = run_lab(&[
        "invoke-affordance",
        path,
        "grammar:base-loko",
        "show-effective-rules",
    ]);
    assert!(effective_rules.contains("effective grammar rules grammar:base-loko"));
    assert!(effective_rules.contains("effective-grammar-rule:base-loko/function-definition"));

    let activations = run_lab(&[
        "invoke-affordance",
        path,
        "grammar:base-loko",
        "show-activations",
    ]);
    assert!(activations.contains("grammar activations grammar:base-loko"));
    assert!(activations.contains("grammar-activation:base-loko/source-file"));

    let grammar_rule = run_lab(&[
        "invoke-affordance",
        path,
        "grammar-rule:base-loko/function-definition",
        "show-grammar-rule",
    ]);
    assert!(grammar_rule.contains("grammar rule grammar-rule:base-loko/function-definition"));
    assert!(grammar_rule.contains("syntax.rs:Parser::parse_function"));

    let expression = run_lab(&[
        "invoke-affordance",
        path,
        "grammar-rule:base-loko/function-definition",
        "show-expression",
    ]);
    assert!(expression.contains("grammar expression grammar-rule:base-loko/function-definition"));
    assert!(expression.contains("data sequence("));

    let parse_trace = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:base-loko",
        "show-parse-trace",
    ]);
    assert!(parse_trace.contains("parse trace"));
    assert!(parse_trace.contains("rule provenance:"));

    let registry = run_lab(&[
        "invoke-affordance",
        path,
        "grammar-registry:session",
        "show-grammar-registry",
    ]);
    assert!(registry.contains("grammar registry"));
    assert!(registry.contains("staged adapters:"));

    let extension = run_lab(&[
        "invoke-affordance",
        path,
        "grammar-extension:records",
        "show-grammar-extension",
    ]);
    assert!(extension.contains("grammar extension grammar-extension:records"));
    assert!(extension.contains("capabilities emit-frontend-forms"));
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

#[test]
fn invoke_affordance_add_method_inserts_method_and_reports_outcome() {
    let target = fixture("add_method_target.loko");
    let source = fixture("add_method_square.loko");

    let output = run_lab(&[
        "invoke-affordance",
        target.to_str().expect("fixture path should be utf8"),
        "generic:g0",
        "add-method",
        "--source",
        source.to_str().expect("fixture path should be utf8"),
    ]);

    let trimmed = output.trim();
    assert!(
        trimmed.starts_with("applied add-method"),
        "unexpected outcome line: {trimmed}"
    );
    assert!(trimmed.contains("mutation_id=0"));
    assert!(trimmed.contains("inserted_method=m1"));
    assert!(trimmed.contains("strategy=full-candidate-rebuild"));
    // The session log gained one entry; the lab is stateless per-invocation,
    // so log_size is always 1 for a successful add-method today. The field
    // is reported anyway to make the log visible from the CLI.
    assert!(
        trimmed.contains("log_size=1"),
        "outcome line should report the post-mutation log size: {trimmed}"
    );
    // The reused word would be a leak from a future cache-reuse slice.
    assert!(
        !trimmed.contains("reused"),
        "outcome wording must not claim cache reuse yet: {trimmed}"
    );
}

#[test]
fn invoke_affordance_add_method_reports_parse_failure() {
    let target = fixture("add_method_target.loko");
    let garbage = fixture("add_method_garbage.loko");

    let stderr = run_lab_failure(&[
        "invoke-affordance",
        target.to_str().expect("fixture path should be utf8"),
        "generic:g0",
        "add-method",
        "--source",
        garbage.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(
        stderr.contains("add-method mutation failed"),
        "stderr should explain mutation failure: {stderr}"
    );
    assert!(
        stderr.contains("parse failure") || stderr.contains("not a single"),
        "stderr should mention the parse-time failure shape: {stderr}"
    );
}

#[test]
fn invoke_affordance_add_method_reports_duplicate_specializer() {
    let target = fixture("add_method_target.loko");
    let duplicate = fixture("add_method_duplicate_circle.loko");

    let stderr = run_lab_failure(&[
        "invoke-affordance",
        target.to_str().expect("fixture path should be utf8"),
        "generic:g0",
        "add-method",
        "--source",
        duplicate.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(stderr.contains("add-method mutation failed"));
    assert!(
        stderr.contains("duplicates existing method"),
        "stderr should explain duplicate-specializer rejection: {stderr}"
    );
}

#[test]
fn invoke_affordance_add_method_rejects_unknown_generic_object_id() {
    let target = fixture("add_method_target.loko");
    let source = fixture("add_method_square.loko");

    let stderr = run_lab_failure(&[
        "invoke-affordance",
        target.to_str().expect("fixture path should be utf8"),
        "generic:g99",
        "add-method",
        "--source",
        source.to_str().expect("fixture path should be utf8"),
    ]);

    assert!(
        stderr.contains("unknown object id `generic:g99`"),
        "stderr should report the unknown object id: {stderr}"
    );
}

#[test]
fn invoke_affordance_add_method_requires_source_flag() {
    let target = fixture("add_method_target.loko");

    let stderr = run_lab_failure(&[
        "invoke-affordance",
        target.to_str().expect("fixture path should be utf8"),
        "generic:g0",
        "add-method",
    ]);

    assert!(
        stderr.contains("`--source <path>`"),
        "stderr should explain that --source is required: {stderr}"
    );
}
