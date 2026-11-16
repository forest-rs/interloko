//! Strict typing through the EOP: strict errors as diagnostic objects tied
//! to their decisions, their fixes as previewable and applicable
//! annotations, the dry-run strict preview, and the policy switch.

use interloko_compiler::diagnostic::{DiagnosticCode, Severity};
use interloko_compiler::session::{
    CompilerSession, EnvironmentAffordance, EnvironmentObject, RuntimeMode, SessionArtifacts,
    SessionMutation, TypingFacet,
};
use interloko_compiler::typing::{
    StrictFix, StrictSource, StrictViolation, TypingConfig, TypingSeverity,
};

const DIGITS: &str = "\
define function digit(n :: limited(<integer>, min: 0, max: 9)) => <integer>
  n
end;
define strict function twice(n :: <integer>) => <integer>
  digit(n) + digit(n)
end;
define function main() => <integer>
  twice(3)
end;
";

const SHAPES: &str = "\
define class <shape> (<object>)
end;
define class <circle> (<shape>)
end;
define class <square> (<shape>)
end;
define generic area(x);
define method area(c :: <circle>)
  7
end;
define method area(q :: <square>)
  4
end;
define function measure(s :: <shape>)
  area(s)
end;
define function main()
  measure(make(<circle>))
end;
";

fn compile(source: &str) -> SessionArtifacts {
    CompilerSession::compile_source("strict.loko", source, RuntimeMode::CompileOnly)
}

fn object(artifacts: &SessionArtifacts, id: &str) -> EnvironmentObject {
    artifacts
        .environment_objects()
        .into_iter()
        .find(|object| object.id.as_str() == id)
        .unwrap_or_else(|| panic!("no object {id}"))
}

#[test]
fn strict_errors_are_diagnostic_objects_tied_to_their_decisions() {
    let artifacts = compile(DIGITS);
    let codes = artifacts
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.severity))
        .collect::<Vec<_>>();
    assert_eq!(
        codes,
        [
            (DiagnosticCode::TypingUnprovedRange, Severity::Error),
            (DiagnosticCode::TypingUnprovedRange, Severity::Error),
        ]
    );
    assert!(artifacts.has_errors());
    // Strictness is reporting only: the program still lowers. Its errors
    // block running, as any error does, and the refusal names them.
    assert!(artifacts.tape.is_some());
    let blocked = CompilerSession::compile_source("strict.loko", DIGITS, RuntimeMode::RunMain);
    assert!(blocked.runtime_result.is_none());
    let refusal = blocked
        .call_function("main", &[], None)
        .expect_err("strict errors block calls")
        .to_string();
    assert!(
        refusal
            .starts_with("compilation reported 2 errors: diagnostic:d0 typing.unproved-range at "),
        "{refusal}"
    );

    let diagnostic = object(&artifacts, "diagnostic:d0");
    let decision = artifacts
        .finding_decision("diagnostic:d0")
        .expect("a typing finding")
        .clone();
    let decision_id = format!("typing:{}:{}", decision.owner, decision.site);
    let Some(TypingFacet::Finding {
        decision: linked,
        strict,
        violation,
        fixes,
    }) = &diagnostic.typing
    else {
        panic!("{:?}", diagnostic.typing);
    };
    assert_eq!(linked, &decision_id);
    assert_eq!(*strict, Some(StrictSource::Declared));
    assert_eq!(*violation, Some(StrictViolation::Unproved));
    assert!(matches!(fixes[0].fix, StrictFix::Annotate { .. }));
    let (target, type_source) = fixes[0].annotation.clone().expect("an annotation fix");
    assert_eq!(target.to_string(), format!("binding:{}:n", decision.owner));
    assert_eq!(type_source, "limited(<integer>, min: 0, max: 9)");
    assert!(matches!(fixes[1].fix, StrictFix::Explicit { .. }));
    assert_eq!(fixes[1].annotation, None);
    for affordance in [
        EnvironmentAffordance::ShowTyping,
        EnvironmentAffordance::PreviewFix,
        EnvironmentAffordance::ApplyFix,
    ] {
        assert!(
            diagnostic.affordances.contains(affordance),
            "{affordance:?}"
        );
    }
    assert!(
        diagnostic
            .inspection
            .detail
            .contains(&format!("decision={decision_id}")),
        "{}",
        diagnostic.inspection.detail
    );

    // The decision object says it is a strict error, and its explanation
    // carries the fixes.
    let decision_object = object(&artifacts, &decision_id);
    assert!(
        decision_object
            .inspection
            .detail
            .ends_with("origin=implicit strict=unproved"),
        "{}",
        decision_object.inspection.detail
    );
    let explained = artifacts
        .explain_typing("diagnostic:d0")
        .expect("explains the decision");
    assert!(
        explained.contains("strict: unproved (declared; must be proved)"),
        "{explained}"
    );
    assert!(
        explained.contains("fix: annotate parameter n as limited(<integer>, min: 0, max: 9)"),
        "{explained}"
    );
}

#[test]
fn an_annotation_fix_previews_and_applies_as_an_annotate_mutation() {
    let mut artifacts = compile(DIGITS);
    let (target, type_source) = artifacts
        .strict_fix_annotation("diagnostic:d0", 0)
        .expect("an annotation fix");
    assert_eq!(artifacts.strict_fix_annotation("diagnostic:d0", 1), None);
    assert_eq!(artifacts.strict_fix_annotation("diagnostic:d9", 0), None);

    let preview = artifacts
        .preview_annotation(&target, &type_source)
        .expect("the fix compiles");
    assert_eq!(preview.delta.before.errors, 2);
    assert_eq!(preview.delta.after.errors, 0);
    let resolved = preview
        .delta
        .changes
        .iter()
        .filter(|change| {
            change.before.as_ref().map(|before| before.severity) == Some(TypingSeverity::Error)
                && change.after.as_ref().map(|after| after.severity) == Some(TypingSeverity::Silent)
        })
        .count();
    assert_eq!(resolved, 2, "{}", preview.delta.render());
    assert!(preview.delta.render().contains("  errors 2->0\n"));
    // The errors go because the fix narrows `twice`'s contract: its range
    // checks are proved only by moving the obligation to `main`, which the
    // preview reports before the error counts.
    assert!(preview.delta.changes_specification());
    assert!(
        preview
            .delta
            .contracts
            .iter()
            .all(|change| change.moves_obligation_to_callers()),
        "{}",
        preview.delta.render()
    );
    let shift = preview.delta.obligations();
    assert_eq!(shift.removed_here, 2, "{}", preview.delta.render());
    assert_eq!(shift.moved_to_callers, 1, "{}", preview.delta.render());
    assert!(
        preview
            .delta
            .render()
            .starts_with("contract delta: SPECIFICATION CHANGED"),
        "{}",
        preview.delta.render()
    );
    // A preview changes nothing.
    assert!(artifacts.has_errors());

    artifacts
        .apply_mutation(SessionMutation::Annotate {
            target,
            type_source,
        })
        .expect("the fix applies");
    assert!(
        artifacts.diagnostics.is_empty(),
        "{:?}",
        artifacts.diagnostics
    );
    assert_eq!(artifacts.typing.strict_error_count(), 0);
}

#[test]
fn preview_strict_reports_what_would_fail_without_changing_anything() {
    let artifacts = compile(SHAPES);
    assert!(artifacts.diagnostics.is_empty());
    let measure = artifacts
        .sem
        .functions
        .iter()
        .find(|function| function.name == "measure")
        .expect("measure");
    let id = format!("function:f{}", measure.id.0);
    assert!(
        object(&artifacts, &id)
            .affordances
            .contains(EnvironmentAffordance::PreviewStrict)
    );
    let preview = artifacts.preview_strict(&id).expect("a function");
    assert!(
        preview.starts_with(
            "strict preview function measure (not applied) strict=none errors=1\n  \
             typing.unproved-dispatch"
        ),
        "{preview}"
    );
    assert!(
        preview.contains("    fix: declare <shape> sealed abstract\n"),
        "{preview}"
    );
    // A generic previews each of its methods.
    let area = artifacts
        .sem
        .generics
        .iter()
        .find(|generic| generic.name == "area")
        .expect("area");
    let preview = artifacts
        .preview_strict(&format!("generic:g{}", area.id.0))
        .expect("a generic");
    assert_eq!(preview.matches("strict preview method area").count(), 2);
    assert!(artifacts.diagnostics.is_empty());
    assert_eq!(artifacts.preview_strict("class:c0"), None);
}

#[test]
fn the_typing_policy_switches_strictness_for_every_definition() {
    let soft = compile(SHAPES);
    let config = soft
        .typing
        .config
        .apply_spec("strict.scope=all")
        .expect("valid");
    assert_eq!(config, TypingConfig::strict());
    let strict = soft.with_typing_config(config);
    assert_eq!(
        strict
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        [DiagnosticCode::TypingUnprovedDispatch]
    );
    let finding = strict.finding_decision("diagnostic:d0").expect("finding");
    assert_eq!(
        strict.typing.strictness(finding.owner),
        Some(StrictSource::Policy)
    );
    let delta = soft.typing_delta(&strict);
    assert_eq!((delta.before.errors, delta.after.errors), (0, 1));
    assert!(
        delta
            .render()
            .contains("dispatch verdict=checked codegen=kept severity=silent->error"),
        "{}",
        delta.render()
    );
    // Strictness changes reporting only: decisions and code are the same.
    assert_eq!(strict.typing.definitions, soft.typing.definitions);
    assert_eq!(
        strict.dump_stage(interloko_compiler::session::DumpStage::Tape),
        soft.dump_stage(interloko_compiler::session::DumpStage::Tape)
    );
}
