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
fn run_prints_typed_scalar_results() {
    let file = fixture("string_main.loko");
    let out = run_lab(&["run", file.to_str().expect("fixture path should be utf8")]);

    assert_eq!(out.trim(), "\"glasshouse\"");
}

#[test]
fn run_and_trace_commands_expose_execution_trace() {
    let file = fixture("generic_area.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let run = run_lab(&["run", "--trace", path]);
    assert!(run.starts_with("100\nexecution trace: boundaries\n"));
    assert!(run.contains("capture: complete"));
    assert!(run.contains("invocation: main [function:f0]"));
    assert!(run.contains("activations: 2 entered"));
    assert!(run.contains("host-call interloko.slot_get"));
    assert!(run.contains("at area#method0@"));
    assert!(run.contains(" from main@"));
    assert!(run.contains("outcome: returned (100) at main@"));

    let trace = run_lab(&["trace", path]);
    assert!(trace.starts_with("execution trace: boundaries\n"));
    assert!(trace.contains("host-call"));
    assert!(!trace.contains(" instr "));

    let instruction_trace = run_lab(&["trace", "--instructions", path]);
    assert!(instruction_trace.starts_with("execution trace: instructions\n"));
    assert!(instruction_trace.contains("instructions: "));
    assert!(instruction_trace.contains(" instr main@0 "));
    // Typing proves `area`'s argument class, so the send calls the method
    // through its link.
    assert!(instruction_trace.contains(" call.link -> area#method0@0"));
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
    // derived_host.grammar currently has no rule overrides, so every
    // derived-host rule is inherited from base-loko. EOP records must
    // not advertise direct overrides that the parser does not use.
    let file = fixture("derived_grammar.loko");
    let path = file.to_str().expect("fixture path should be utf8");
    let eop = run_lab(&["eop", path]);

    assert!(eop.contains("object language-environment:derived-host"));
    assert!(eop.contains("object grammar:derived-host"));
    assert!(
        !eop.contains("object grammar-rule:derived-host/"),
        "derived-host must not emit direct grammar-rule records until the source declares overrides",
    );
    assert!(eop.contains("object effective-grammar-rule:derived-host/function-definition"));
    assert!(eop.contains("object effective-grammar-rule:derived-host/class-definition"));
    assert!(!eop.contains("resolution=override"));
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
    assert!(rule.contains("resolution inherited"));
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
    assert!(eop.contains("driver=interpreted-grammar"));
    assert!(eop.contains("oracle=none"));
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
    assert!(trace.contains("grammar-action-output"));
    assert!(trace.contains("frontend-function(name: main"));
    assert!(trace.contains("adapter=calculator"));
    assert!(trace.contains("driver=interpreted-grammar"));
    assert!(trace.contains("oracle=none"));
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
    assert!(trace.contains("driver=interpreted-grammar"));
    assert!(trace.contains("oracle=none"));
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
    assert!(html.contains("source-authored"));
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
    assert!(html.contains("id=\"parse-provenance\""));
    assert!(html.contains("Source To Parse Rules"));
    assert!(html.contains("parse-card"));
    assert!(html.contains("parse-rule-ladder"));
    assert!(html.contains("parse-token-strip"));
    assert!(html.contains("function-definition"));
    assert!(html.contains("parse-card--staged"));
    assert!(html.contains("staged-form:r0/f0"));
    assert!(html.contains("frontend class &lt;circle&gt;"));
    assert!(html.contains("parse-materialized"));
    assert!(html.contains("\"parseViews\""));
    assert!(html.contains("\"stagedRegions\""));
    assert!(html.contains("\"driver\":\"interpreted-grammar\""));
    assert!(html.contains("\"oracle\":\"none\""));
    assert!(html.contains("\"sourceSpan\""));
    assert!(html.contains("\"artifactKeys\""));
    assert!(html.contains("\"elapsedNanos\""));
    assert!(html.contains("\"executionTrace\""));
    assert!(html.contains("Expansion Chain"));
    assert!(html.contains("driver interpreted-grammar · oracle none"));
    assert!(html.contains("RecordsRegionGrammar"));
    assert!(html.contains("Grammar package, rules, and actions"));
    assert!(html.contains("grammar-package:records"));
    assert!(html.contains("grammar-resource:records/grammars/records.grammar"));
    assert!(html.contains("grammar-rule:records/record-definition"));
    assert!(html.contains("effective-grammar-rule:records/record-definition"));
    assert!(html.contains("grammar-action:records/record-definition"));
    assert!(html.contains("action output"));
    assert!(html.contains("grammar-extension-emission:records/r0/f0"));
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

    let stdout = run_lab(&["report", path, "--output", report_path, "--codegen", "off"]);
    assert!(stdout.contains("wrote "));

    let html = fs::read_to_string(&report).expect("report should be written");
    let _ = fs::remove_file(&report);

    assert!(html.contains("generic_area.loko"));
    assert!(html.contains("<span>Codegen</span><strong>off</strong>"));
    assert!(html.contains("returned 100"));
    assert!(html.contains("No staged regions were activated in this source."));
    assert!(html.contains("class:c0"));
    assert!(html.contains("&lt;object&gt;"));
    assert!(html.contains("class:c1"));
    assert!(html.contains("generic:g0"));
    assert!(html.contains("id=\"parse-form-form-f0\""));
    assert!(html.contains("rule stack"));
    assert!(html.contains("tokens"));
    let timeline = html
        .split("<ol class=\"trace-timeline\">")
        .nth(1)
        .unwrap()
        .split("</ol>")
        .next()
        .unwrap();
    assert!(timeline.contains("<strong>run starts at main</strong>"));
    assert!(timeline.contains("<strong>host interloko.slot_get · returned</strong>"));
    assert!(timeline.contains(" · from main@"));
    assert!(timeline.contains("<strong>run returns</strong>"));
}

#[test]
fn doc_comments_are_exposed_by_lab_and_report() {
    let file = fixture("doc_comments.loko");
    let path = file.to_str().expect("fixture path should be utf8");

    let affordances = run_lab(&["affordances", path, "function:f1"]);
    assert!(affordances.contains("show-documentation"));

    let docs = run_lab(&[
        "invoke-affordance",
        path,
        "function:f1",
        "show-documentation",
    ]);
    assert!(docs.contains("documentation function:f1"));
    assert!(docs.contains("Main docs."));
    assert!(docs.contains("Returns three."));
    assert!(!docs.contains("Body docs"));

    let eop = run_lab(&["eop", path]);
    assert!(eop.contains("documentation [Main docs. | Returns three.]"));

    let report = std::env::temp_dir().join(format!(
        "interloko-doc-comments-report-{}.html",
        std::process::id()
    ));
    let report_path = report.to_str().expect("report path should be utf8");
    let _ = fs::remove_file(&report);

    let stdout = run_lab(&["report", path, "--output", report_path]);
    assert!(stdout.contains("wrote "));
    let html = fs::read_to_string(&report).expect("report should be written");
    let _ = fs::remove_file(&report);

    assert!(html.contains("source documentation"));
    assert!(html.contains("Main docs."));
    assert!(html.contains("Returns three."));
    assert!(html.contains("show-documentation"));
    assert!(html.contains("\"docComments\":[\"Main docs.\",\"Returns three.\"]"));
    assert!(html.contains("\"documentation\":[\"Main docs.\",\"Returns three.\"]"));
}

#[test]
fn parse_trace_reports_grammar_diagnostic_facts() {
    let file = fixture("derived_grammar_syntax_error.loko");
    let path = file.to_str().expect("fixture path should be utf8");
    let (out, err) = run_lab_success(&[
        "invoke-affordance",
        path,
        "language-environment:derived-host",
        "show-parse-trace",
    ]);

    assert!(out.contains("diagnostics:"));
    assert!(err.contains("Error syntax.expected-expression"));
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

    let shadow = run_lab(&[
        "invoke-affordance",
        path,
        "language-environment:base-loko",
        "show-parser-shadow",
    ]);
    assert!(shadow.contains("parser shadow status=oracle-unavailable"));
    assert!(shadow.contains("implementation source-authored-base-loko-grammar"));

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
fn invoke_affordance_add_method_refuses_a_method_with_strict_errors() {
    let target = fixture("annotate_shapes.loko");
    let strict = fixture("add_method_strict_unproved.loko");

    let stderr = run_lab_failure(&[
        "invoke-affordance",
        target.to_str().expect("fixture path should be utf8"),
        "generic:g0",
        "add-method",
        "--source",
        strict.to_str().expect("fixture path should be utf8"),
    ]);

    // The refusal prints the strict error it would add, with its decision
    // and fix, then names its code.
    assert!(stderr.contains("Error typing.unproved-range"), "{stderr}");
    assert!(stderr.contains("(typing:def"), "{stderr}");
    assert!(
        stderr.contains("    fix: write as(limited(<integer>, min: 0, max: 9), ...)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "add-method mutation failed: semantic failure (1 new diagnostics: typing.unproved-range)"
        ),
        "{stderr}"
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

#[test]
fn soft_typing_warns_runs_and_explains_decisions() {
    let source = fixture("soft_typing.loko");
    let path = source.to_str().expect("fixture path should be utf8");

    let (stdout, stderr) = run_lab_success(&["run", path]);
    assert_eq!(stdout.trim(), "7");
    assert!(stderr.contains(
        "Warning typing.dispatch-no-applicable-method 256..275 no method of `area` can apply to exactly <shape>"
    ));
    assert!(stderr.contains("Warning typing.cast-always-fails"));

    let typing = run_lab(&["dump", path, "--stage", "typing"]);
    assert!(typing.starts_with(
        "typing config=soft status=checked decisions=4 proved=1 checked=1 refuted=2\n"
    ));

    let eop = run_lab(&["eop", path]);
    assert!(eop.contains("object typing-config:session"));
    let config = run_lab(&[
        "invoke-affordance",
        path,
        "typing-config:session",
        "show-typing",
    ]);
    assert_eq!(config, typing);

    let decision = object_id_with_prefix(&eop, "typing:");
    let explanation = run_lab(&["invoke-affordance", path, &decision, "show-typing"]);
    assert!(explanation.starts_with(&format!("typing decision {decision}\n")));
    assert!(explanation.contains("owner: function measure"));
    assert!(explanation.contains("verdict: checked"));
    assert!(explanation.contains("dispatch-site: dispatch:d0"));
    assert!(explanation.contains("contract: advisory; the send is guarded (observed)"));
}

/// The id of the object whose presentation title is `title`.
fn object_id_with_title(eop: &str, title: &str) -> String {
    let mut current = None;
    for line in eop.lines() {
        if let Some(id) = line.trim().strip_prefix("object ") {
            current = Some(id);
        } else if line.trim() == format!("presentation title=\"{title}\"") {
            return current.expect("title follows an object line").to_string();
        }
    }
    panic!("no object titled {title}");
}

#[test]
fn show_types_and_contract_dependents_explain_flow_types() {
    let source = fixture("typed_contracts.loko");
    let path = source.to_str().expect("fixture path should be utf8");

    let (stdout, _stderr) = run_lab_success(&["run", path]);
    assert_eq!(stdout.trim(), "7");

    let types = run_lab(&["dump", path, "--stage", "types"]);
    assert!(types.starts_with("types status=checked definitions=4 "));

    let measure = run_lab(&["invoke-affordance", path, "function:f1", "show-types"]);
    assert!(measure.contains("function measure"), "{measure}");
    assert!(measure.contains("    param s@"), "{measure}");
    assert!(
        measure.contains(" <circle> because=narrowed(s0,then)@"),
        "{measure}"
    );
    assert!(measure.contains("    result <integer>"), "{measure}");

    let eop = run_lab(&["eop", path]);
    let contract = object_id_with_title(&eop, "contract of function pick");
    let affordances = run_lab(&["affordances", path, &contract]);
    assert!(affordances.contains("show-dependents"), "{affordances}");
    let dependents = run_lab(&["invoke-affordance", path, &contract, "show-dependents"]);
    assert!(
        dependents.starts_with(&format!("contract {contract} of function pick\n")),
        "{dependents}"
    );
    assert!(dependents.contains("results: (<shape>)\n"), "{dependents}");
    assert!(dependents.contains("dependents: 1\n"), "{dependents}");
    let decision = dependents
        .lines()
        .find_map(|line| line.trim().strip_prefix("typing:"))
        .map(|rest| format!("typing:{}", rest.split(' ').next().expect("id")))
        .expect("a dependent decision");

    let explanation = run_lab(&["invoke-affordance", path, &decision, "show-typing"]);
    assert!(
        explanation.contains(&format!("depends-on: {contract}\n")),
        "{explanation}"
    );
    assert!(
        explanation.contains("depends-on: generic:g0\n"),
        "{explanation}"
    );
    assert!(
        explanation.contains("codegen: guarded (policy proved)\n"),
        "{explanation}"
    );
    let record = run_lab(&["inspect", path, &decision]);
    assert!(record.contains("codegen=guarded"), "{record}");
}

#[test]
fn codegen_option_selects_the_policy_for_every_command() {
    let source = fixture("inlined_overflow.loko");
    let path = source.to_str().expect("fixture path should be utf8");

    // `off` keeps the send; the overflow signals inside the `+` method.
    let signal = "host-call interloko.condition.type_error args=(9223372036854775807, 1, ";
    let off = run_lab(&["trace", "--codegen", "off", path]);
    assert!(off.contains("+#dispatch"), "{off}");
    assert!(
        off.lines()
            .any(|line| line.contains(signal) && line.contains(" at +#method0@")),
        "{off}"
    );

    // `proved` runs the method inline, so the signal comes from `main` at
    // the call's source position instead of the built-in method.
    let proved = run_lab(&["trace", "--codegen=proved", path]);
    assert!(!proved.contains("+#dispatch"), "{proved}");
    assert!(
        proved.lines().any(|line| line.contains(signal)
            && line.contains(" at main@")
            && line.ends_with(" (8:17)")),
        "{proved}"
    );

    // Both runs end in the same outcome.
    for policy in ["off", "proved"] {
        let stderr = run_lab_failure(&["--codegen", policy, "run", path]);
        assert!(
            stderr.starts_with(
                "runtime failed: unhandled condition <type-error>: type-error.overflow: \
                 <integer> arithmetic on #[9223372036854775807, 1] overflows at typing:def"
            ),
            "{policy}: {stderr}"
        );
    }

    // The option may come before the command or among its arguments, and
    // the session's typing configuration object reports it.
    let lowered = run_lab(&["--codegen", "off", "dump", path, "--stage", "lowered"]);
    assert!(lowered.contains("guarded-send"), "{lowered}");
    let lowered = run_lab(&["dump", path, "--stage", "lowered", "--codegen", "proved"]);
    assert!(!lowered.contains("guarded-send"), "{lowered}");
    // `proved` is the default.
    assert_eq!(run_lab(&["dump", path, "--stage", "lowered"]), lowered);
    for policy in ["off", "proved"] {
        let config = run_lab(&[
            "inspect",
            "--codegen",
            policy,
            path,
            "typing-config:session",
        ]);
        assert!(
            config.contains(&format!("  config=soft codegen={policy} ")),
            "{config}"
        );
    }
}

#[test]
fn codegen_option_rejects_unknown_missing_and_repeated_policies() {
    let source = fixture("inlined_overflow.loko");
    let path = source.to_str().expect("fixture path should be utf8");

    let unknown = run_lab_failure(&["run", "--codegen", "fast", path]);
    assert!(
        unknown.contains("unknown codegen policy `fast`; expected off|proved"),
        "{unknown}"
    );
    let missing = run_lab_failure(&["run", path, "--codegen"]);
    assert!(
        missing.contains("option `--codegen` requires a policy: off|proved"),
        "{missing}"
    );
    let repeated = run_lab_failure(&["run", "--codegen", "off", "--codegen=proved", path]);
    assert!(
        repeated.contains("option `--codegen` may be given only once"),
        "{repeated}"
    );
}

/// The `binding:` id of parameter `s`, discovered the way an agent would:
/// from the `eop` listing, since definition ids depend on the file path.
fn measure_param_id(path: &str) -> String {
    let eop = run_lab(&["eop", path]);
    eop.lines()
        .filter_map(|line| line.trim().strip_prefix("object "))
        .find(|id| id.starts_with("binding:") && id.ends_with(":s"))
        .expect("binding object for s")
        .to_string()
}

#[test]
fn eop_projects_annotation_targets_with_their_declarations() {
    let path = fixture("annotate_shapes.loko");
    let path = path.to_str().expect("fixture path should be utf8");
    let id = measure_param_id(path);
    let inspect = run_lab(&["inspect", path, &id]);
    assert!(
        inspect.contains("role=param declared=written annotation=<shape>"),
        "{inspect}"
    );
    let affordances = run_lab(&["affordances", path, &id]);
    for name in ["show-types", "annotate", "preview-annotation"] {
        assert!(affordances.contains(name), "{affordances}");
    }
    let types = run_lab(&["invoke-affordance", path, &id, "show-types"]);
    assert!(types.contains("declared: <shape> written"), "{types}");
    let local = id.replace(":s", ":bonus");
    let types = run_lab(&["invoke-affordance", path, &local, "show-types"]);
    assert!(types.contains("declared: <integer> inferred"), "{types}");
    let dump = run_lab(&["dump", path, "--stage", "types"]);
    assert!(
        dump.contains(&format!("annotation=written id={id}")),
        "{dump}"
    );
}

#[test]
fn annotate_reports_the_verdict_delta_it_buys() {
    let path = fixture("annotate_shapes.loko");
    let path = path.to_str().expect("fixture path should be utf8");
    let id = measure_param_id(path);

    let preview = run_lab(&["annotate", path, &id, "<circle>", "--dry-run"]);
    assert!(
        preview.starts_with(&format!(
            "preview annotate target={id} annotation=<circle> previous=<shape> (not applied)"
        )),
        "{preview}"
    );
    assert!(
        preview.contains("instance verdict=checked->proved codegen=kept->elided"),
        "{preview}"
    );
    assert!(
        preview.contains("dispatch verdict=checked->proved codegen=guarded"),
        "{preview}"
    );

    let applied = run_lab(&["annotate", path, &id, "<circle>"]);
    let first = applied.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("applied annotate mutation_id=0")
            && first.contains("log_size=1")
            && first.contains("strategy=full-candidate-rebuild"),
        "{applied}"
    );
    // The applied delta is the preview's.
    assert_eq!(
        applied.lines().skip(1).collect::<Vec<_>>(),
        preview.lines().skip(1).collect::<Vec<_>>()
    );

    let invoked = run_lab(&[
        "invoke-affordance",
        path,
        &id,
        "preview-annotation",
        "--type",
        "<circle>",
    ]);
    assert_eq!(invoked, preview);
}

#[test]
fn annotate_rejects_wrong_annotations() {
    let path = fixture("annotate_shapes.loko");
    let path = path.to_str().expect("fixture path should be utf8");
    let id = measure_param_id(path);
    let stderr = run_lab_failure(&["annotate", path, &id, "<integer>"]);
    assert!(
        stderr.contains("annotate mutation failed: semantic failure"),
        "{stderr}"
    );
    let stderr = run_lab_failure(&["annotate", path, &id, "<integer>", "=", "3"]);
    assert!(stderr.contains("is not a type annotation"), "{stderr}");
    let stderr = run_lab_failure(&[
        "annotate",
        path,
        "binding:def0000000000000000:s",
        "<circle>",
    ]);
    assert!(
        stderr.contains("unknown object") || stderr.contains("binding:def0000000000000000:s"),
        "{stderr}"
    );
}

#[test]
fn typing_config_affordances_report_policy_deltas() {
    let path = fixture("annotate_shapes.loko");
    let path = path.to_str().expect("fixture path should be utf8");
    let affordances = run_lab(&["affordances", path, "typing-config:session"]);
    assert!(affordances.contains("set-typing-policy"), "{affordances}");
    assert!(affordances.contains("set-codegen-policy"), "{affordances}");

    let (stdout, stderr) = run_lab_success(&[
        "invoke-affordance",
        path,
        "typing-config:session",
        "set-typing-policy",
        "soft",
        "dispatch.checked=warning",
    ]);
    assert!(
        stdout.starts_with("typing policy soft -> custom"),
        "{stdout}"
    );
    assert!(
        stdout.contains("dispatch verdict=checked codegen=guarded severity=silent->warning"),
        "{stdout}"
    );
    assert!(
        stderr.contains("typing.dispatch-runtime-checked"),
        "{stderr}"
    );

    let stderr = run_lab_failure(&[
        "invoke-affordance",
        path,
        "typing-config:session",
        "set-typing-policy",
        "loud",
    ]);
    assert!(
        stderr.contains("`loud` is neither a typing preset"),
        "{stderr}"
    );

    let proved = fixture("inlined_overflow.loko");
    let stdout = run_lab(&[
        "invoke-affordance",
        proved.to_str().expect("fixture path should be utf8"),
        "typing-config:session",
        "set-codegen-policy",
        "off",
    ]);
    assert!(
        stdout.starts_with("codegen policy proved -> off"),
        "{stdout}"
    );
    // Off, the numeric send is no longer inlined; it sends through a guard.
    assert!(stdout.contains("codegen=inlined->guarded"), "{stdout}");
}

#[test]
fn strict_errors_preview_strictness_and_fixes_through_affordances() {
    let path = fixture("strict_digits.loko");
    let path = path.to_str().expect("fixture path should be utf8");

    // Strict errors block running like any compile error: the diagnostics
    // carry the reasons and fixes, and the refusal names each error's
    // diagnostic object, place and typing decision.
    let stderr = run_lab_failure(&["run", path]);
    assert!(stderr.contains("Error typing.unproved-range"), "{stderr}");
    assert!(
        stderr.contains("    fix: annotate parameter n as limited(<integer>, min: 0, max: 9)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "main did not run: compilation reported 2 errors: \
             diagnostic:d0 typing.unproved-range at 302..303 (typing:def"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("; diagnostic:d1 typing.unproved-range at 313..314 (typing:def"),
        "{stderr}"
    );
    let stderr = run_lab_failure(&["invoke-affordance", path, "function:f2", "run"]);
    assert!(
        stderr.contains("main did not run: compilation reported 2 errors"),
        "{stderr}"
    );
    let (stdout, _) = run_lab_success(&["trace", path]);
    assert!(
        stdout.starts_with(
            "execution trace unavailable: main did not run: compilation reported 2 errors"
        ),
        "{stdout}"
    );

    let affordances = run_lab(&["affordances", path, "diagnostic:d0"]);
    for name in ["show-typing", "preview-fix", "apply-fix"] {
        assert!(affordances.contains(name), "{affordances}");
    }
    let preview = run_lab(&["invoke-affordance", path, "function:f0", "preview-strict"]);
    assert_eq!(
        preview,
        "strict preview function small (not applied) strict=none errors=0\n"
    );

    let (stdout, _) = run_lab_success(&["invoke-affordance", path, "diagnostic:d0", "preview-fix"]);
    assert!(stdout.contains("(not applied)"), "{stdout}");
    assert!(stdout.contains("  errors 2->0\n"), "{stdout}");
    // The fix clears the errors by narrowing `twice`'s contract, which the
    // preview reports before the counts: the checks moved to `main`.
    assert!(
        stdout.contains("contract delta: SPECIFICATION CHANGED changes=1 narrows-inputs=1\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "<integer> -> limited(<integer>, min: 0, max: 9); callers now carry the obligation"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("obligations: removed-here=2 added-here=0 moved-to-callers=1 (proved=1"),
        "{stdout}"
    );
    assert!(
        stdout.contains("range added verdict=proved codegen=elided severity=silent in=caller\n"),
        "{stdout}"
    );
    assert!(
        stdout
            .contains("range verdict=checked->proved codegen=kept->elided severity=error->silent"),
        "{stdout}"
    );
    let (stdout, _) =
        run_lab_success(&["invoke-affordance", path, "diagnostic:d0", "apply-fix", "0"]);
    assert!(stdout.contains("applied annotate"), "{stdout}");

    let stderr = run_lab_failure(&["invoke-affordance", path, "diagnostic:d0", "apply-fix", "1"]);
    assert!(
        stderr.contains("fix 1 of `diagnostic:d0` is not one an annotation makes"),
        "{stderr}"
    );
}

#[test]
fn claims_report_what_typing_established_and_why_checks_stay() {
    let fixture = fixture("strict_digits.loko");
    let path = fixture.to_str().expect("utf8 path");

    let affordances = run_lab(&["affordances", path, "function:f1"]);
    assert!(affordances.contains("show-claims"), "{affordances}");
    let eop = run_lab(&["eop", path]);
    assert!(eop.contains("  object claims:def"), "{eop}");
    assert!(
        eop.contains("presentation title=\"claims of function twice\""),
        "{eop}"
    );

    let (stdout, _) = run_lab_success(&["claims", path, "function:f1"]);
    assert!(stdout.starts_with("claims claims:def"), "{stdout}");
    assert!(stdout.contains("  revision=none basis="), "{stdout}");
    assert!(
        stdout.contains(
            "  strict (declared): every check the compiler inserts is proved (send dispatch, implicit casts, range checks); \
             explicit as(...) casts and conversions, instance? tests, indirect-entry guards and integer overflow (strict.overflow=runtime) stay runtime checks; \
             unmet=2 exempt-runtime=1\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("  established: sites=3 proved=0 checked=3 refuted=0 unreachable=0\n"),
        "{stdout}"
    );
    assert!(stdout.contains("  unproved checks: 3\n"), "{stdout}");
    assert!(
        stdout.contains("cause=declared: the declared type <integer> admits values outside limited(<integer>, min: 0, max: 9)"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "fix: annotate parameter n as limited(<integer>, min: 0, max: 9) [binding:def"
        ) && stdout.contains("; narrows the contract: callers carry the obligation"),
        "{stdout}"
    );

    let (full, _) = run_lab_success(&[
        "invoke-affordance",
        path,
        "function:f1",
        "show-claims",
        "full",
    ]);
    assert!(full.contains("detail=full"), "{full}");
    assert!(full.contains("  sites: 3 (3 unproved)\n"), "{full}");
    assert!(full.contains("      because declared@"), "{full}");

    let stderr = run_lab_failure(&["claims", path, "function:f1", "everything"]);
    assert!(stderr.contains("takes `summary` or `full`"), "{stderr}");
}
