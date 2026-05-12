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
    assert!(out.contains("fail/diagnostics/slot_initializer_type.cairn"));
    assert!(out.contains("fail/diagnostics/duplicate_slot_initializer.cairn"));
    assert!(out.contains("fail/diagnostics/unresolved_local.cairn"));
    assert!(out.contains("fail/restarts/signal_without_restart.cairn"));
    assert!(out.contains("pass/objects/init_keywords.cairn"));
    assert!(out.contains("pass/objects/accessor_call.cairn"));
    assert!(out.contains("pass/objects/class_object_make.cairn"));
    assert!(out.contains("pass/objects/class_object_function.cairn"));
    assert!(out.contains("pass/objects/class_of_make.cairn"));
    assert!(out.contains("pass/objects/object_top_slot.cairn"));
    assert!(out.contains("pass/objects/typed_slots.cairn"));
    assert!(out.contains("pass/functions/typed_boolean.cairn"));
    assert!(out.contains("pass/functions/typed_nominal_object.cairn"));
    assert!(out.contains("pass/functions/typed_class_object.cairn"));
    assert!(out.contains("pass/functions/typed_object_top.cairn"));
    assert!(out.contains("fail/diagnostics/function_argument_type.cairn"));
    assert!(out.contains("fail/diagnostics/function_return_type.cairn"));
    assert!(out.contains("pass/generics/typed_contract_area.cairn"));
    assert!(out.contains("pass/generics/typed_generic_object_result.cairn"));
    assert!(out.contains("fail/diagnostics/generic_argument_type.cairn"));
    assert!(out.contains("fail/diagnostics/generic_method_return_type.cairn"));
    assert!(out.contains("fail/diagnostics/generic_method_specializer_type.cairn"));
    assert!(out.contains("pass/functions/apply_known_direct.cairn"));
    assert!(out.contains("pass/generics/apply_known_generic.cairn"));
    assert!(out.contains("pass/objects/accessor_callable_identity.cairn"));
    assert!(out.contains("fail/diagnostics/apply_accessor_unsupported.cairn"));
    assert!(out.contains("fail/diagnostics/apply_non_callable.cairn"));
    assert!(out.contains("pass/objects/instance_refinement.cairn"));
    assert!(out.contains("pass/objects/cast_object_slot.cairn"));
    assert!(out.contains("pass/host/host_backed_slot.cairn"));
    assert!(out.contains("pass/host/host_backed_dispatch.cairn"));
    assert!(out.contains("fail/diagnostics/impossible_instance_test.cairn"));
    assert!(out.contains("fail/diagnostics/impossible_cast.cairn"));
    assert!(out.contains("fail/diagnostics/primitive_instance_test.cairn"));
    assert!(out.contains("fail/diagnostics/host_backed_make.cairn"));
    assert!(out.contains("fail/diagnostics/host_backed_bad_slot.cairn"));
    assert!(out.contains("fail/diagnostics/host_backed_bad_cast.cairn"));
    assert!(out.contains("corpus: 65 passed, 0 failed, 65 total"));
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
