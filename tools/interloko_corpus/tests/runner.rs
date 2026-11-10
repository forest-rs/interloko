//! CLI tests for `interloko_corpus`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_interloko_corpus")
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
        .expect("interloko_corpus should run");
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

    assert!(out.contains("pass/arithmetic/add.loko"));
    assert!(out.contains("pass/control_flow/generic_guarded.loko"));
    assert!(out.contains("pass/generics/two_class_dispatch.loko"));
    assert!(out.contains("pass/restarts/recover_invalid_radius.loko"));
    assert!(out.contains("fail/control_flow/invalid_if_condition.loko"));
    assert!(out.contains("fail/diagnostics/slot_initializer_type.loko"));
    assert!(out.contains("fail/diagnostics/constant_slot_assignment.loko"));
    assert!(out.contains("fail/diagnostics/constant_type_annotation.loko"));
    assert!(out.contains("fail/diagnostics/unknown_slot_assignment.loko"));
    assert!(out.contains("fail/diagnostics/slot_assignment_type.loko"));
    assert!(out.contains("fail/diagnostics/host_backed_slot_assignment.loko"));
    assert!(out.contains("fail/diagnostics/non_object_slot_assignment.loko"));
    assert!(out.contains("fail/diagnostics/duplicate_slot_initializer.loko"));
    assert!(out.contains("fail/diagnostics/mop_legacy_alias.loko"));
    assert!(out.contains("pass/mop/staged_records.loko"));
    assert!(out.contains("fail/diagnostics/unresolved_local.loko"));
    assert!(out.contains("pass/values/top_level_constants.loko"));
    assert!(out.contains("fail/diagnostics/duplicate_constant.loko"));
    assert!(out.contains("fail/diagnostics/invalid_constant_initializer.loko"));
    assert!(out.contains("fail/diagnostics/local_type_annotation.loko"));
    assert!(out.contains("fail/diagnostics/unresolved_constant.loko"));
    assert!(out.contains("fail/restarts/signal_without_restart.loko"));
    assert!(out.contains("pass/objects/init_keywords.loko"));
    assert!(out.contains("pass/objects/accessor_call.loko"));
    assert!(out.contains("pass/objects/class_object_make.loko"));
    assert!(out.contains("pass/objects/class_object_function.loko"));
    assert!(out.contains("pass/objects/class_of_make.loko"));
    assert!(out.contains("pass/objects/object_top_slot.loko"));
    assert!(out.contains("pass/objects/typed_slots.loko"));
    assert!(out.contains("pass/objects/slot_assignment_write_read.loko"));
    assert!(out.contains("pass/objects/slot_assignment_returns_value.loko"));
    assert!(out.contains("pass/objects/slot_setter_call.loko"));
    assert!(out.contains("pass/objects/inherited_slot_read.loko"));
    assert!(out.contains("pass/objects/inherited_parent_child_slots.loko"));
    assert!(out.contains("pass/objects/subtype_instance_cast.loko"));
    assert!(out.contains("pass/objects/parent_typed_slot_child.loko"));
    assert!(out.contains("pass/functions/typed_boolean.loko"));
    assert!(out.contains("pass/functions/typed_nominal_object.loko"));
    assert!(out.contains("pass/functions/typed_class_object.loko"));
    assert!(out.contains("pass/functions/typed_object_top.loko"));
    assert!(out.contains("pass/functions/subtype_direct_call.loko"));
    assert!(out.contains("fail/diagnostics/function_argument_type.loko"));
    assert!(out.contains("fail/diagnostics/function_return_type.loko"));
    assert!(out.contains("pass/generics/typed_contract_area.loko"));
    assert!(out.contains("pass/generics/typed_generic_object_result.loko"));
    assert!(out.contains("pass/generics/parent_method_dispatch_child.loko"));
    assert!(out.contains("pass/generics/most_specific_subclass_dispatch.loko"));
    assert!(out.contains("fail/diagnostics/generic_argument_type.loko"));
    assert!(out.contains("fail/diagnostics/generic_method_return_type.loko"));
    assert!(out.contains("fail/diagnostics/generic_method_specializer_type.loko"));
    assert!(out.contains("pass/functions/apply_known_direct.loko"));
    assert!(out.contains("pass/generics/apply_known_generic.loko"));
    assert!(out.contains("pass/objects/accessor_callable_identity.loko"));
    assert!(out.contains("pass/objects/apply_accessor_callable.loko"));
    assert!(out.contains("pass/functions/apply_runtime_callable_parameter.loko"));
    assert!(out.contains("pass/functions/closure_capture.loko"));
    assert!(out.contains("fail/diagnostics/apply_non_callable.loko"));
    assert!(out.contains("pass/objects/instance_refinement.loko"));
    assert!(out.contains("pass/objects/cast_object_slot.loko"));
    assert!(out.contains("pass/host/host_backed_slot.loko"));
    assert!(out.contains("pass/host/host_backed_dispatch.loko"));
    assert!(out.contains("pass/restarts/subclass_condition_parent_restart.loko"));
    assert!(out.contains("fail/diagnostics/impossible_instance_test.loko"));
    assert!(out.contains("fail/diagnostics/impossible_cast.loko"));
    assert!(out.contains("fail/diagnostics/sibling_subtype_instance_test.loko"));
    assert!(out.contains("fail/diagnostics/sibling_subtype_cast.loko"));
    assert!(out.contains("fail/diagnostics/primitive_instance_test.loko"));
    assert!(out.contains("fail/diagnostics/host_backed_make.loko"));
    assert!(out.contains("fail/diagnostics/host_backed_bad_slot.loko"));
    assert!(out.contains("fail/diagnostics/host_backed_bad_cast.loko"));
    assert!(out.contains("fail/diagnostics/unknown_superclass.loko"));
    assert!(out.contains("fail/diagnostics/duplicate_inherited_slot.loko"));
    assert!(out.contains("fail/diagnostics/signal_arity.loko"));
    assert!(out.contains("pass/values/typed_slots_and_dispatch.loko"));
    assert!(out.contains("fail/values/malformed_string.loko"));
    assert!(out.contains("fail/syntax/expected_expression.loko"));
    assert!(out.contains("pass/syntax/comments_and_doc_comments.loko"));
    assert!(out.contains("pass/values/typed_run_results.loko"));
    assert!(out.contains("pass/values/boolean_run_result.loko"));
    assert!(out.contains("corpus: 171 passed, 0 failed, 171 total"));
}

#[test]
fn list_reports_corpus_paths() {
    let root = corpus_root();
    let out = run_corpus(&["list", root.to_str().expect("corpus path should be utf8")]);

    assert!(out.contains("pass/functions/direct_call.loko"));
    assert!(out.contains("fail/diagnostics/function_arity.loko"));
}

#[test]
fn dump_prints_stage_text_for_one_case() {
    let file = corpus_file("pass/generics/single_method_area.loko");
    let out = run_corpus(&[
        "dump",
        file.to_str().expect("corpus file path should be utf8"),
        "--stage",
        "lowered",
    ]);

    assert!(out.contains("lowered module"));
    assert!(out.contains("area#dispatch"));
}
