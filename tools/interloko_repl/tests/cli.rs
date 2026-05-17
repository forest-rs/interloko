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
    assert!(stdout.contains("source repl.loko (1 forms, 4 lines, unsaved)"));
    assert!(stdout.contains("   1 | define function answer()"));
    assert!(stdout.lines().any(|line| line == "42"));
    assert!(stdout.contains("reset"));
    assert!(stdout.contains("(no accepted forms)"));
    assert!(stdout.contains("(empty)"));
}

#[test]
fn source_forms_are_inspectable_and_show_source_affordance_objects() {
    let (stdout, stderr) = run_repl(
        "define function answer()\n  42\nend;\n\
         :forms\n\
         :inspect form:f0\n\
         :affordances form:f0\n\
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
    assert!(stdout.contains("affordances form:f0"));
    assert!(stdout.contains("show-source"));
    assert!(stdout.contains("source form:f0 function answer"));
    assert!(stdout.contains("   1 | define function answer()"));
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
    assert!(stdout.contains("source repl.loko (1 forms, 4 lines, clean)"));
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
    assert!(stdout.contains("form:f1 loaded function second lines 8..10"));
    assert!(stdout.contains("// explain second"));
    assert!(stdout.contains("   8 | define function second()"));
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
    assert!(stdout.contains("form:f1 loaded function main lines 3..5"));
    assert!(stdout.lines().any(|line| line == "1"));
}

#[test]
fn malformed_pending_definition_reports_immediately() {
    let (stdout, stderr) = run_repl(
        "define function broken()\n\
           let x = ;\n\
         1 + 2\n\
         :quit\n",
    );

    assert!(stderr.contains("diagnostics:"), "{stderr}");
    assert!(stderr.contains("syntax.expected-expression"), "{stderr}");
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
    assert!(stdout.contains("source repl.loko (1 forms, 4 lines, clean)"));
    assert_eq!(written, "define function answer()\n  42\nend;\n\n");
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
    assert!(stdout.contains("source repl.loko (2 forms, 8 lines, dirty since load)"));
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
    assert!(stdout.contains("source repl.loko (2 forms, 8 lines, dirty since load)"));
    assert!(stdout.contains("reloaded "));
    assert!(stdout.contains("discarded 1 typed definitions"));
    assert!(stdout.contains("form:f0 loaded function loaded-answer lines 1..3"));
    assert!(stdout.contains("source repl.loko (1 forms, 4 lines, clean)"));
    assert!(stdout.lines().any(|line| line == "10"));
}
