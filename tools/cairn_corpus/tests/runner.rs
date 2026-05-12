//! CLI tests for `cairn_corpus`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cairn_corpus")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn corpus_root() -> PathBuf {
    workspace_root().join("corpus")
}

fn corpus_file(relative: &str) -> PathBuf {
    corpus_root().join(relative)
}

fn run_corpus(args: &[&str]) -> String {
    let output = Command::new(bin())
        .args(args)
        .output()
        .expect("cairn_corpus should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf8")
}

#[test]
fn check_validates_all_corpus_cases() {
    let root = corpus_root();
    let out = run_corpus(&["check", root.to_str().expect("corpus path should be utf8")]);

    assert!(out.contains("pass/arithmetic/add.cairn"));
    assert!(out.contains("pass/control_flow/generic_guarded.cairn"));
    assert!(out.contains("pass/generics/two_class_dispatch.cairn"));
    assert!(out.contains("pass/restarts/recover_invalid_radius.cairn"));
    assert!(out.contains("fail/control_flow/invalid_if_condition.cairn"));
    assert!(out.contains("fail/diagnostics/unresolved_local.cairn"));
    assert!(out.contains("fail/restarts/signal_without_restart.cairn"));
    assert!(out.contains("corpus: 28 passed, 0 failed, 28 total"));
}

#[test]
fn list_reports_corpus_paths() {
    let root = corpus_root();
    let out = run_corpus(&["list", root.to_str().expect("corpus path should be utf8")]);

    assert!(out.contains("pass/functions/direct_call.cairn"));
    assert!(out.contains("fail/diagnostics/function_arity.cairn"));
}

#[test]
fn dump_prints_stage_text_for_one_case() {
    let file = corpus_file("pass/generics/single_method_area.cairn");
    let out = run_corpus(&[
        "dump",
        file.to_str().expect("corpus file path should be utf8"),
        "--stage",
        "lowered",
    ]);

    assert!(out.contains("lowered module"));
    assert!(out.contains("area#dispatch"));
}
