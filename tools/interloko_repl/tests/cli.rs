//! CLI transcript tests for `interloko_repl`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn run_repl(input: &str) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_interloko_repl"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn interloko_repl");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for interloko_repl");
    assert!(output.status.success());
    (
        String::from_utf8(output.stdout).expect("stdout utf8"),
        String::from_utf8(output.stderr).expect("stderr utf8"),
    )
}

fn temp_loko_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "interloko_repl_{}_{}_{}",
        std::process::id(),
        name,
        line!()
    ))
}

#[test]
fn expressions_print_typed_scalar_results() {
    let (stdout, stderr) = run_repl("1 < 2\n\"glass\\nhouse\"\n:quit\n");

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.lines().any(|line| line == "#t"), "{stdout}");
    assert!(
        stdout.lines().any(|line| line == "\"glass\\nhouse\""),
        "{stdout}"
    );
}

#[test]
fn transcript_definitions_expressions_and_inspection_commands() {
    let (stdout, stderr) = run_repl(
        "define function square(x)\n  x * x\nend;\n\
         square(5)\n\
         :source\n\
         :dump sem\n\
         :mop\n\
         :eop\n\
         :artifacts\n\
         :inspect function:f0\n\
         :affordances function:f0\n\
         :clear\n\
         :source\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("ok"));
    assert!(stdout.lines().any(|line| line == "25"));
    assert!(stdout.contains("define function square"));
    assert!(stdout.contains("sem "));
    assert!(stdout.contains("mop "));
    assert!(stdout.contains("eop "));
    assert!(stdout.contains("artifact graph"));
    assert!(stdout.contains("object function:f0"));
    assert!(stdout.contains("bind, source="));
    assert!(stdout.contains("runtime-result, source="));
    assert!(stdout.contains("affordances function:f0"));
    assert!(stdout.contains("cleared"));
    assert!(stdout.contains("(empty)"));
    assert!(!stdout.contains("\u{1b}["));
    assert!(!stderr.contains("\u{1b}["));
}

#[test]
fn diagnostics_are_rendered_and_repl_continues() {
    let (stdout, stderr) = run_repl("missing\n1 + 2\n:quit\n");

    assert!(stdout.lines().any(|line| line == "3"));
    assert!(stderr.contains("diagnostics:"));
    assert!(stderr.contains("sem.unresolved-local"));
    assert!(stderr.contains("unresolved local `missing`"));
}

#[test]
fn malformed_definitions_are_not_committed_to_the_source_buffer() {
    let (stdout, stderr) = run_repl("define function broken(\nend;\n1 + 2\n:source\n:quit\n");

    assert!(stdout.lines().any(|line| line == "3"));
    assert!(stdout.contains("(empty)"));
    assert!(!stdout.contains("define function broken"));
    assert!(stderr.contains("diagnostics:"));
    assert!(stderr.contains("syntax.expected-token"));
}

#[test]
fn dispatch_sites_can_be_listed_and_explained() {
    let (stdout, stderr) = run_repl(
        "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
         define generic area(shape :: <object>) => <integer>;\n\
         define method area(shape :: <circle>) => <integer>\n  shape.radius * shape.radius\nend;\n\
         area(make(<circle>, radius: 3))\n\
         :dispatch\n\
         :dispatch dispatch:d0\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.lines().any(|line| line == "9"));
    assert!(stdout.contains("dispatch sites for repl.loko"));
    assert!(stdout.contains("dispatch dispatch:d0"));
    assert!(stdout.contains("generic: g0:area"));
}

#[test]
fn rich_values_work_in_expressions_and_dumps() {
    let (stdout, stderr) = run_repl(
        "define function same-message(a :: <string>, b :: <string>) => <boolean>\n  a = b\nend;\n\
         if same-message(\"hello\", \"hello\") then 7 else 0 end\n\
         :dump sem\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.lines().any(|line| line == "7"));
    assert!(stdout.contains("literal.string"));
    assert!(stdout.contains("value-eq"));
}

#[test]
fn session_commands_list_forms_run_functions_and_reset() {
    let (stdout, stderr) = run_repl(
        "define function answer()\n  42\nend;\n\
         :forms\n\
         :source\n\
         :run answer\n\
         :reset\n\
         :forms\n\
         :source\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("accepted forms"));
    assert!(stdout.contains("form:f0 typed function answer lines 1..3"));
    assert!(stdout.contains("source repl.loko (1 forms, 3 lines, unsaved)"));
    assert!(stdout.contains("   1 | define function answer()"));
    assert!(stdout.lines().any(|line| line == "42"));
    assert!(stdout.contains("reset"));
    assert!(stdout.contains("(no accepted forms)"));
    assert!(stdout.contains("(empty)"));
}

#[test]
fn source_forms_are_inspectable_and_invokable() {
    let (stdout, stderr) = run_repl(
        "define function answer()\n  42\nend;\n\
         :forms\n\
         :inspect form:f0\n\
         :affordances form:f0\n\
         :invoke form:f0 inspect\n\
         :invoke form:f0 show-defined-objects\n\
         :invoke form:f0 show-grammar\n\
         :invoke form:f0 show-parse\n\
         :invoke form:f0 show-cst\n\
         :invoke form:f0 show-tokens\n\
         :invoke form:f0 show-rule-applications\n\
         :invoke form:f0 show-diagnostics\n\
         :invoke form:f0 show-source\n\
         :show-form f0\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("form:f0 typed function answer lines 1..3"));
    assert!(stdout.contains("object form:f0"));
    assert!(stdout.contains("kind: repl-source-form"));
    assert!(stdout.contains("category: editor-state"));
    assert!(stdout.contains("origin: typed"));
    assert!(stdout.contains("form-kind: function"));
    assert!(stdout.contains("span: "));
    assert!(stdout.contains("defines:"));
    assert!(stdout.contains("function:f0 answer() -> integer primary"));
    assert!(stdout.contains("grammar:"));
    assert!(stdout.contains("environment: language-environment:base-loko"));
    assert!(stdout.contains("activation: grammar-activation:base-loko/source-file"));
    assert!(stdout.contains("grammar: grammar:base-loko"));
    assert!(
        stdout.contains("effective-rule: effective-grammar-rule:base-loko/function-definition")
    );
    assert!(stdout.contains("affordances form:f0"));
    assert!(stdout.contains("inspect"));
    assert!(stdout.contains("show-source"));
    assert!(stdout.contains("show-defined-objects"));
    assert!(stdout.contains("show-grammar"));
    assert!(stdout.contains("show-parse"));
    assert!(stdout.contains("show-cst"));
    assert!(stdout.contains("show-tokens"));
    assert!(stdout.contains("show-rule-applications"));
    assert!(stdout.contains("show-diagnostics"));
    assert!(stdout.contains("defined objects form:f0"));
    assert!(stdout.contains("grammar form:f0"));
    assert!(stdout.contains("parse form:f0"));
    assert!(stdout.contains("cst:"));
    assert!(stdout.contains("tokens:"));
    assert!(stdout.contains("rule-applications:"));
    assert!(stdout.contains("diagnostics:"));
    assert!(stdout.contains("Function"));
    assert!(stdout.contains("Define"));
    assert!(stdout.contains("function-definition"));
    assert!(stdout.contains("<none>"));
    assert!(stdout.contains("source form:f0 function answer"));
    assert!(stdout.contains("   1 | define function answer()"));
}

#[test]
fn doc_comments_are_exposed_by_repl_affordances() {
    let (stdout, stderr) = run_repl(
        "/// Main docs.\n\
         /// Returns three.\n\
         define function main() => <integer>\n\
           /// Body docs stay trivia.\n\
           3\n\
         end;\n\
         :affordances function:f0\n\
         :invoke function:f0 show-documentation\n\
         :invoke form:f0 show-documentation\n\
         :inspect function:f0\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("show-documentation"));
    assert!(stdout.contains("documentation function:f0"));
    assert!(stdout.contains("documentation form:f0"));
    assert!(stdout.contains("Main docs."));
    assert!(stdout.contains("Returns three."));
    assert!(!stdout.contains("Body docs"));
}

#[test]
fn source_forms_report_defined_environment_objects() {
    let (stdout, stderr) = run_repl(
        "define class <point> slot x; end;\n\
         define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
         define generic area(shape :: <object>) => <integer>;\n\
         define method area(c :: <circle>) => <integer>\n  c.radius * c.radius\nend;\n\
         define function main()\n  area(make(<circle>, radius: 10))\nend;\n\
         :invoke form:f0 show-defined-objects\n\
         :invoke form:f1 show-defined-objects\n\
         :invoke form:f2 show-defined-objects\n\
         :invoke form:f3 show-defined-objects\n\
         :invoke form:f4 show-defined-objects\n\
         :invoke form:f0 show-grammar\n\
         :invoke form:f1 show-grammar\n\
         :invoke form:f2 show-grammar\n\
         :invoke form:f3 show-grammar\n\
         :invoke form:f4 show-grammar\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("defined objects form:f0"));
    assert!(stdout.contains("class:c1 <point> primary"));
    assert!(stdout.contains("slot:c1.s0"));
    assert!(stdout.contains("accessor:c1.s0"));
    assert!(stdout.contains("defined objects form:f1"));
    assert!(stdout.contains("class:c2 <circle> primary"));
    assert!(stdout.contains("slot:c2.s0"));
    assert!(stdout.contains("accessor:c2.s0"));
    assert!(stdout.contains("defined objects form:f2"));
    assert!(stdout.contains("generic:g0 area(shape:object-top) -> integer primary"));
    assert!(stdout.contains("defined objects form:f3"));
    assert!(stdout.contains("method:m0 area(c2:<circle>) -> integer primary"));
    assert!(stdout.contains("defined objects form:f4"));
    assert!(stdout.contains("function:f0 main() -> integer primary"));
    assert!(stdout.contains("effective-rule: effective-grammar-rule:base-loko/class-definition"));
    assert!(stdout.contains("effective-rule: effective-grammar-rule:base-loko/generic-definition"));
    assert!(stdout.contains("effective-rule: effective-grammar-rule:base-loko/method-definition"));
    assert!(
        stdout.contains("effective-rule: effective-grammar-rule:base-loko/function-definition")
    );
}

#[test]
fn inspect_and_affordances_resolve_language_names() {
    let (stdout, stderr) = run_repl(
        "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
         define generic area(shape :: <object>) => <integer>;\n\
         define method area(c :: <circle>) => <integer>\n  c.radius * c.radius\nend;\n\
         define function main()\n  area(make(<circle>, radius: 10))\nend;\n\
         :explain <circle>\n\
         :explain form:f0\n\
         :inspect <circle>\n\
         :affordances <circle>\n\
         :invoke <circle> show-slots\n\
         :inspect class:c1\n\
         :inspect main\n\
         :inspect area\n\
         :inspect radius\n\
         :inspect missing\n\
         :quit\n",
    );

    assert!(stdout.contains("object class:c1"));
    assert!(stdout.contains("explain class:c1"));
    assert!(stdout.contains("defined by:"));
    assert!(stdout.contains("form:f0 class definition"));
    assert!(stdout.contains(":invoke class:c1 show-slots"));
    assert!(stdout.contains("explain form:f0"));
    assert!(stdout.contains("summary: typed class `<circle>`"));
    assert!(stdout.contains("defined objects:"));
    assert!(stdout.contains(":invoke form:f0 show-grammar"));
    assert!(stdout.contains("title: <circle>"));
    assert!(stdout.contains("affordances class:c1"));
    assert!(stdout.contains("show-slots"));
    assert!(stdout.contains("slots class:c1 <circle>"));
    assert!(stdout.contains("object function:f0"));
    assert!(stdout.contains("title: main() -> integer"));
    assert!(
        stderr.contains("error: ambiguous object reference `area`"),
        "{stderr}"
    );
    assert!(stderr.contains("generic:g0 area"), "{stderr}");
    assert!(stderr.contains("method:m0 area"), "{stderr}");
    assert!(
        stderr.contains("error: ambiguous object reference `radius`"),
        "{stderr}"
    );
    assert!(stderr.contains("accessor:c1.s0 radius"), "{stderr}");
    assert!(stderr.contains("slot:c1.s0 <circle>.radius"), "{stderr}");
    assert!(
        stderr.contains("error: unknown object `missing`"),
        "{stderr}"
    );
}

#[test]
fn grammar_names_resolve_to_active_environment_objects() {
    let path = temp_loko_path("staged_records_lookup.loko");
    fs::write(
        &path,
        "// grammar-activate: records\n\
         // staged-loko: records\n\
         record circle\n\
           radius: <integer>\n\
         end\n\
         // end-staged-loko\n\
         define function main()\n\
           10\n\
         end;\n",
    )
    .expect("write staged fixture");

    let (stdout, stderr) = run_repl(&format!(
        ":load {}\n\
         :inspect records\n\
         :invoke records show-direct-rules\n\
         :invoke records show-effective-rules\n\
         :invoke staged-loko show-grammars\n\
         :quit\n",
        path.display()
    ));
    let _ = fs::remove_file(path);

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("object grammar:records"), "{stdout}");
    assert!(
        stdout.contains("direct grammar rules grammar:records"),
        "{stdout}"
    );
    assert!(
        stdout.contains("effective grammar rules grammar:records"),
        "{stdout}"
    );
    assert!(
        stdout.contains("language environment grammars language-environment:staged-loko"),
        "{stdout}"
    );
}

#[test]
fn unknown_source_form_ids_report_unknown_form() {
    let (stdout, stderr) = run_repl(
        "define function answer()\n  42\nend;\n\
         :inspect form:f99\n\
         :affordances f99\n\
         :quit\n",
    );

    assert!(stdout.contains("ok"));
    assert!(
        stderr.contains("error: unknown form `form:f99`"),
        "{stderr}"
    );
    assert!(stderr.contains("error: unknown form `f99`"), "{stderr}");
}

#[test]
fn invoke_runs_supported_environment_object_affordances() {
    let (stdout, stderr) = run_repl(
        "define function answer()\n  42\nend;\n\
         define function other()\n  7\nend;\n\
         :invoke function:f0 inspect\n\
         :invoke function:f0 dump-sem\n\
         :invoke function:f0 show-dependencies\n\
         :invoke function:f0 run\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("object function:f0"));
    assert!(stdout.contains("sem object function:f0"));
    assert!(stdout.contains("name: answer"));
    assert!(!stdout.contains("name: other"), "{stdout}");
    assert!(stdout.contains("artifact graph"));
    assert!(stdout.lines().any(|line| line == "42"));
}

#[test]
fn invoke_runs_linked_object_and_dispatch_affordances() {
    let (stdout, stderr) = run_repl(
        "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
         define generic area(shape :: <object>) => <integer>;\n\
         define method area(shape :: <circle>) => <integer>\n  shape.radius * shape.radius\nend;\n\
         define function main()\n  area(make(<circle>, radius: 3))\nend;\n\
         :run main\n\
         :invoke slot:c1.s0 show-accessor\n\
         :invoke accessor:c1.s0 show-slot\n\
         :invoke generic:g0 show-dispatch\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.lines().any(|line| line == "9"));
    assert!(stdout.contains("object accessor:c1.s0"));
    assert!(stdout.contains("object slot:c1.s0"));
    assert!(stdout.contains("dispatch table:"));
}

#[test]
fn invoke_handles_supported_and_rejected_environment_affordances() {
    let (stdout, stderr) = run_repl(
        "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
         define function answer()\n  42\nend;\n\
         define generic area(shape :: <object>) => <integer>;\n\
         define method area(shape :: <circle>) => <integer>\n  shape.radius\nend;\n\
         define function main()\n  area(make(<circle>, radius: 3))\nend;\n\
         :invoke generic:g0 add-method\n\
         :invoke generic:g0 bogus-affordance\n\
         :invoke generic:g0 show-slot\n\
         :invoke dispatch:d0 open-source\n\
         :invoke class:c1 show-slots\n\
         :quit\n",
    );

    assert!(stdout.contains("ok"));
    assert!(
        stderr.contains("mutating affordance `add-method` is not executable by interloko_repl"),
        "{stderr}"
    );
    assert!(
        stderr.contains("error: unknown affordance `bogus-affordance`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("object `generic:g0` does not expose affordance `show-slot`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("affordance `open-source` needs editor integration"),
        "{stderr}"
    );
    assert!(stdout.contains("slots class:c1 <circle>"), "{stdout}");
    assert!(
        stdout.contains("slot:c1.s0 radius :: <integer>"),
        "{stdout}"
    );
}

#[test]
fn drop_form_affordance_removes_an_unused_form() {
    let (stdout, stderr) = run_repl(
        "define function unused()\n  1\nend;\n\
         define function answer()\n  42\nend;\n\
         :invoke form:f0 drop\n\
         :forms\n\
         :source\n\
         :run answer\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("dropped form:f0"));
    assert!(stdout.contains("form:f0 typed function answer lines 1..3"));
    assert!(!stdout.contains("define function unused()"));
    assert!(stdout.contains("source repl.loko (1 forms, 3 lines, unsaved)"));
    assert!(stdout.lines().any(|line| line == "42"));
}

#[test]
fn drop_form_can_remove_the_only_accepted_form() {
    let (stdout, stderr) = run_repl(
        "define function answer()\n  42\nend;\n\
         :drop-form form:f0\n\
         :forms\n\
         :source\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("dropped form:f0"));
    assert!(stdout.contains("(no accepted forms)"));
    assert!(stdout.contains("(empty)"));
}

#[test]
fn drop_form_rolls_back_when_candidate_source_does_not_compile() {
    let (stdout, stderr) = run_repl(
        "define function helper()\n  41\nend;\n\
         define function answer()\n  helper() + 1\nend;\n\
         :drop-form f0\n\
         :forms\n\
         :source\n\
         :run answer\n\
         :quit\n",
    );

    assert!(stderr.contains("diagnostics:"), "{stderr}");
    assert!(stdout.contains("form:f0 typed function helper lines 1..3"));
    assert!(stdout.contains("form:f1 typed function answer lines 5..7"));
    assert!(stdout.contains("define function helper()"));
    assert!(stdout.contains("define function answer()"));
    assert!(stdout.lines().any(|line| line == "42"));
}

#[test]
fn pending_definition_can_be_inspected_and_cancelled() {
    let (stdout, stderr) = run_repl(
        "define function broken()\n\
         :pending\n\
         :cancel\n\
         :forms\n\
         :source\n\
         1 + 2\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("pending definition"));
    assert!(stdout.contains("   1 | define function broken()"));
    assert!(stdout.contains("cancelled pending definition"));
    assert!(stdout.contains("(no accepted forms)"));
    assert!(stdout.contains("(empty)"));
    assert!(stdout.lines().any(|line| line == "3"));
}

#[test]
fn multiline_definitions_finish_from_parser_state_without_semicolons() {
    let (stdout, stderr) = run_repl(
        "define class <circle> (<object>)\n\
           slot radius :: <integer>;\n\
         end\n\
         define generic area(x :: <object>) => <integer>;\n\
         define method area(c :: <circle>) => <integer>\n\
           c.radius * c.radius\n\
         end\n\
         define function main()\n\
           area(make(<circle>, radius: 3))\n\
         end\n\
         :run main\n\
         :quit\n",
    );

    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.lines().any(|line| line == "9"));
}

#[test]
fn quit_discards_pending_definition_and_exits() {
    let (stdout, stderr) = run_repl("define function broken()\n:quit\n");

    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn load_replaces_the_source_buffer_with_loaded_forms() {
    let path = temp_loko_path("load_replace.loko");
    fs::write(&path, "define function loaded-answer()\n  42\nend;\n").expect("write source");

    let (stdout, stderr) = run_repl(&format!(
        "define function typed-answer()\n  1\nend;\n\
         :load {}\n\
         :forms\n\
         :source\n\
         :run loaded-answer\n\
         :quit\n",
        path.display()
    ));

    let _ = fs::remove_file(&path);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("loaded "));
    assert!(stdout.contains("form:f0 loaded function loaded-answer lines 1..3"));
    assert!(stdout.contains("source repl.loko (1 forms, 3 lines, clean)"));
    assert!(stdout.contains("define function loaded-answer()"));
    assert!(!stdout.contains("define function typed-answer()"));
    assert!(stdout.lines().any(|line| line == "42"));
}

#[test]
fn failed_load_preserves_the_existing_source_buffer() {
    let path = temp_loko_path("bad_load.loko");
    fs::write(&path, "define function broken(\nend;\n").expect("write bad source");

    let (stdout, stderr) = run_repl(&format!(
        "define function answer()\n  42\nend;\n\
         :load {}\n\
         :run answer\n\
         :source\n\
         :quit\n",
        path.display()
    ));

    let _ = fs::remove_file(&path);
    assert!(stderr.contains("diagnostics:"), "{stderr}");
    assert!(stderr.contains("syntax.expected-token"), "{stderr}");
    assert!(stdout.lines().any(|line| line == "42"));
    assert!(stdout.contains("define function answer()"));
    assert!(!stdout.contains("define function broken("));
}

#[test]
fn load_preserves_inter_form_comments_with_parser_line_ranges() {
    let path = temp_loko_path("comments_load.loko");
    fs::write(
        &path,
        "define function first()\n  1\nend;\n\
         \n\
         // explain second\n\
         define function second()\n  2\nend;\n",
    )
    .expect("write source");

    let (stdout, stderr) = run_repl(&format!(
        ":load {}\n\
         :forms\n\
         :source\n\
         :quit\n",
        path.display()
    ));

    let _ = fs::remove_file(&path);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("form:f0 loaded function first lines 1..3"));
    assert!(stdout.contains("form:f1 loaded function second lines 6..8"));
    assert!(stdout.contains("   5 | // explain second"));
    assert!(stdout.contains("   6 | define function second()"));
}

#[test]
fn loaded_buffer_keeps_file_layout_and_line_numbers() {
    // Several blank lines, a doc comment, a trailing line comment, and no
    // final newline: every reported line must match the file itself.
    let source = "define function first()\n  1\nend; // trailing\n\
                  \n\n\n\
                  /// Second's doc.\n\
                  define function second()\n  2\nend;\n\
                  define function third()\n  3\nend;";
    let path = temp_loko_path("layout_load.loko");
    let written = temp_loko_path("layout_write.loko");
    fs::write(&path, source).expect("write source");

    let (stdout, stderr) = run_repl(&format!(
        ":load {path}\n\
         :forms\n\
         :source\n\
         :show-form f1\n\
         :write {written}\n\
         :reload\n\
         :forms\n\
         define function fourth()\n  4\nend;\n\
         :forms\n\
         :quit\n",
        path = path.display(),
        written = written.display(),
    ));

    let round_trip = fs::read_to_string(&written).expect("read written source");
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&written);
    assert!(stderr.is_empty(), "{stderr}");
    assert_eq!(round_trip, source);
    assert_eq!(
        stdout
            .matches("form:f0 loaded function first lines 1..3")
            .count(),
        3,
        "{stdout}"
    );
    assert_eq!(
        stdout
            .matches("form:f1 loaded function second lines 8..10")
            .count(),
        3,
        "{stdout}"
    );
    assert_eq!(
        stdout
            .matches("form:f2 loaded function third lines 11..13")
            .count(),
        3,
        "{stdout}"
    );
    assert!(
        stdout.contains("form:f3 typed function fourth lines 15..17"),
        "{stdout}"
    );
    assert!(
        stdout.contains("source repl.loko (3 forms, 13 lines, clean)"),
        "{stdout}"
    );
    for (index, line) in source.lines().enumerate() {
        let numbered = format!("{:>4} | {line}", index + 1);
        assert!(
            stdout.lines().any(|out| out == numbered),
            "missing `{numbered}`\n{stdout}"
        );
    }
    assert!(stdout.contains("source form:f1 function second\n   7 | /// Second's doc.\n   8 | define function second()"), "{stdout}");
}

#[test]
fn load_records_semicolon_terminated_generic_forms() {
    let path = temp_loko_path("generic_load.loko");
    fs::write(
        &path,
        "define generic score(x :: <object>) => <integer>;\n\
         define function main()\n  1\nend;\n",
    )
    .expect("write source");

    let (stdout, stderr) = run_repl(&format!(
        ":load {}\n\
         :forms\n\
         :run main\n\
         :quit\n",
        path.display()
    ));

    let _ = fs::remove_file(&path);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("form:f0 loaded generic score lines 1..1"));
    assert!(stdout.contains("form:f1 loaded function main lines 2..4"));
    assert!(stdout.lines().any(|line| line == "1"));
}

#[test]
fn malformed_completed_definition_reports_and_repl_continues() {
    let (stdout, stderr) = run_repl(
        "define function broken()\n\
           let x = ;\n\
         end;\n\
         1 + 2\n\
         :quit\n",
    );

    assert!(stderr.contains("diagnostics:"), "{stderr}");
    assert!(stderr.contains("syntax.expected-token"), "{stderr}");
    assert!(stdout.lines().any(|line| line == "3"));
}

#[test]
fn write_emits_the_rendered_source_buffer() {
    let path = temp_loko_path("write_source.loko");
    let _ = fs::remove_file(&path);

    let (stdout, stderr) = run_repl(&format!(
        "define function answer()\n  42\nend;\n\
         :write {}\n\
         :source\n\
         :quit\n",
        path.display()
    ));

    let written = fs::read_to_string(&path).expect("read written source");
    let _ = fs::remove_file(&path);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("wrote "));
    assert!(stdout.contains("source repl.loko (1 forms, 3 lines, clean)"));
    assert_eq!(written, "define function answer()\n  42\nend;\n");
}

#[test]
fn failed_write_preserves_dirty_state() {
    let path = temp_loko_path("failed_write_source.loko");
    let rejected_path = path.join("missing-directory").join("out.loko");
    fs::write(&path, "define function loaded-answer()\n  10\nend;\n").expect("write source");

    let (stdout, stderr) = run_repl(&format!(
        ":load {}\n\
         define function typed-answer()\n  99\nend;\n\
         :write {}\n\
         :source\n\
         :quit\n",
        path.display(),
        rejected_path.display()
    ));

    let _ = fs::remove_file(&path);
    assert!(stderr.contains("error:"), "{stderr}");
    assert!(stdout.contains("source repl.loko (2 forms, 7 lines, dirty since load)"));
    assert!(stdout.contains("define function loaded-answer()"));
    assert!(stdout.contains("define function typed-answer()"));
}

#[test]
fn reload_discards_typed_definitions_after_the_last_load() {
    let path = temp_loko_path("reload.loko");
    fs::write(&path, "define function loaded-answer()\n  10\nend;\n").expect("write source");

    let (stdout, stderr) = run_repl(&format!(
        ":load {}\n\
         define function typed-answer()\n  99\nend;\n\
         :source\n\
         :reload\n\
         :forms\n\
         :source\n\
         :run loaded-answer\n\
         :quit\n",
        path.display()
    ));

    let _ = fs::remove_file(&path);
    assert!(stderr.is_empty(), "{stderr}");
    assert!(stdout.contains("source repl.loko (2 forms, 7 lines, dirty since load)"));
    assert!(stdout.contains("reloaded "));
    assert!(stdout.contains("discarded 1 typed definitions"));
    assert!(stdout.contains("form:f0 loaded function loaded-answer lines 1..3"));
    assert!(stdout.contains("source repl.loko (1 forms, 3 lines, clean)"));
    assert!(stdout.lines().any(|line| line == "10"));
}

#[test]
fn repl_turns_keep_the_runtime_image_flat() {
    let mut input = String::from(
        "define function score(text :: <string>) => <integer>\n  \
         if text = \"abcd\" then 1 else 0 end\nend;\n\
         define function churn(n :: <integer>) => <integer>\n  \
         if n = 0 then 0 else churn(n - 1) + score(concatenate(\"ab\", \"cd\")) end\nend;\n",
    );
    for _ in 0..10 {
        input.push_str("churn(40)\n:heap\n");
    }
    input.push_str(":quit\n");
    let (stdout, stderr) = run_repl(&input);

    assert!(stderr.is_empty(), "{stderr}");
    let images: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("image: "))
        .collect();
    assert_eq!(images.len(), 10, "{stdout}");
    assert!(images.iter().all(|line| *line == images[0]), "{stdout}");
    assert_eq!(images[0], "image: 0 objects, 0 boxed values, 0 bytes");
    assert!(
        stdout.contains("last turn: allocated 160 objects"),
        "{stdout}"
    );
}
