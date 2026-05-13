//! CLI transcript tests for `cairn_repl`.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_repl(input: &str) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cairn_repl"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cairn_repl");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for cairn_repl");
    assert!(output.status.success());
    (
        String::from_utf8(output.stdout).expect("stdout utf8"),
        String::from_utf8(output.stderr).expect("stderr utf8"),
    )
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
fn malformed_definitions_are_not_committed_to_the_durable_source() {
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
    assert!(stdout.contains("dispatch sites for repl.cairn"));
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
