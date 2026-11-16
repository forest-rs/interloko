//! Adjusting typing through the EOP: annotate mutations, their previews,
//! their verdict deltas, and their place in the mutation log.

use interloko_compiler::annotation::{
    AnnotationRole, AnnotationTarget, ContractDirection, DecisionChange, DeclarationOrigin,
    ObligationEffect,
};
use interloko_compiler::lower::{CheckCodegen, CodegenPolicy};
use interloko_compiler::session::{
    AnnotationOutcome, CompilerSession, FunctionValue, MutationError, RuntimeMode,
    SessionArtifacts, SessionMutation, SourceFile,
};
use interloko_compiler::typing::{TypeSelector, TypingCheck, TypingVerdict};
use interloko_model::contract::ContractPosition;

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
  let bonus = if instance?(s, <circle>) then 1 else 0 end;
  area(s) + bonus
end;
define function main()
  measure(make(<circle>))
end;
";

const SQUARE_AREA: &str = "\
define method area(q :: <square>)
  4
end;
";

fn compile(source: &str) -> SessionArtifacts {
    CompilerSession::compile_source("shapes.loko", source, RuntimeMode::CompileOnly)
}

fn binding(artifacts: &SessionArtifacts, function: &str, name: &str) -> AnnotationTarget {
    let definition = artifacts
        .sem
        .functions
        .iter()
        .find(|candidate| candidate.name == function)
        .unwrap_or_else(|| panic!("no function {function}"))
        .definition;
    AnnotationTarget::Binding {
        definition,
        name: name.into(),
        occurrence: 0,
    }
}

fn run_main(artifacts: &SessionArtifacts) -> FunctionValue {
    artifacts
        .call_function("main", &[], None)
        .expect("main runs")
        .value
}

fn change(outcome: &AnnotationOutcome, check: TypingCheck) -> &DecisionChange {
    outcome
        .delta
        .changes
        .iter()
        .find(|change| {
            change
                .after
                .as_ref()
                .is_some_and(|after| after.check == check)
        })
        .unwrap_or_else(|| panic!("no {check:?} change in\n{}", outcome.delta.render()))
}

#[test]
fn annotating_a_parameter_proves_its_dispatch_and_elides_its_test() {
    let mut artifacts = compile(SHAPES);
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let before = run_main(&artifacts);
    let target = binding(&artifacts, "measure", "s");

    let outcome = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target: target.clone(),
            type_source: "<circle>".into(),
        })
        .expect("annotation compiles");
    let annotation = outcome.annotation().expect("an annotate outcome");
    assert_eq!(annotation.previous.as_deref(), Some("<shape>"));
    assert_eq!(annotation.annotated, target);

    let dispatch = change(annotation, TypingCheck::Dispatch);
    assert_eq!(
        dispatch.before.as_ref().map(|before| before.verdict),
        Some(TypingVerdict::Checked)
    );
    assert_eq!(
        dispatch.after.as_ref().map(|after| after.verdict),
        Some(TypingVerdict::Proved)
    );
    let instance = change(annotation, TypingCheck::Instance);
    assert_eq!(
        instance.before.as_ref().map(|before| before.codegen),
        Some(CheckCodegen::Kept)
    );
    assert_eq!(
        instance.after.as_ref().map(|after| after.codegen),
        Some(CheckCodegen::Elided)
    );
    assert!(annotation.delta.after.proved >= 2);
    assert!(annotation.delta.after.elided >= 1);
    let rendered = annotation.delta.render();
    assert!(
        rendered.contains("dispatch verdict=checked->proved codegen=kept"),
        "{rendered}"
    );
    assert!(
        rendered.contains("instance verdict=checked->proved codegen=kept->elided"),
        "{rendered}"
    );

    assert!(artifacts.source.source.contains("measure(s :: <circle>)"));
    assert_eq!(artifacts.mutations().len(), 1);
    assert_eq!(run_main(&artifacts), before);
}

#[test]
fn annotating_an_inferred_binding_inserts_the_annotation() {
    // An unannotated parameter defaults to `<integer>`, so writing that
    // type changes no decision: the delta says the annotation buys nothing.
    let source = format!("{SHAPES}define function twice(s)\n  s * 2\nend;\n");
    let mut artifacts = compile(&source);
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let target = binding(&artifacts, "twice", "s");
    let site = artifacts
        .annotation_sites()
        .into_iter()
        .find(|site| site.target == target)
        .expect("s is a site");
    assert_eq!(site.role, AnnotationRole::Param);
    assert_eq!(site.declaration(), DeclarationOrigin::Inferred);

    let outcome = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target: target.clone(),
            type_source: "<integer>".into(),
        })
        .expect("annotation compiles");
    let annotation = outcome.annotation().expect("annotate");
    assert_eq!(annotation.previous, None);
    assert!(annotation.delta.is_empty(), "{}", annotation.delta.render());
    assert!(artifacts.source.source.contains("twice(s :: <integer>)"));
    let site = artifacts
        .annotation_sites()
        .into_iter()
        .find(|site| site.target == target)
        .expect("s is still a site");
    assert_eq!(site.declaration(), DeclarationOrigin::Written);
}

#[test]
fn type_reports_say_whether_a_binding_type_is_written() {
    let artifacts = compile(SHAPES);
    let sites = artifacts.annotation_sites();
    let span_of = |name: &str| {
        sites
            .iter()
            .find(|site| matches!(&site.target, AnnotationTarget::Binding { name: n, .. } if n == name))
            .map(|site| site.span)
            .expect("site")
    };
    let param = artifacts
        .type_of(&TypeSelector::Span(span_of("s")))
        .expect("typed");
    assert_eq!(param.declaration, Some(DeclarationOrigin::Written));
    let local = artifacts
        .type_of(&TypeSelector::Span(span_of("bonus")))
        .expect("typed");
    assert_eq!(local.declaration, Some(DeclarationOrigin::Inferred));
    let types = artifacts
        .explain_types(&binding(&artifacts, "measure", "bonus").to_string())
        .expect("binding types");
    assert!(types.contains("declared: <integer> inferred"), "{types}");
}

#[test]
fn preview_reports_the_delta_without_swapping() {
    let artifacts = compile(SHAPES);
    let target = binding(&artifacts, "measure", "s");
    let source = artifacts.source.source.clone();

    let preview = artifacts
        .preview_annotation(&target, "<circle>")
        .expect("preview compiles");
    assert_eq!(artifacts.source.source, source);
    assert!(artifacts.mutations().is_empty());
    assert_eq!(artifacts.next_mutation_id(), 0);

    let mut applied = artifacts.clone();
    let outcome = applied
        .apply_mutation(SessionMutation::Annotate {
            target,
            type_source: "<circle>".into(),
        })
        .expect("apply compiles");
    assert_eq!(outcome.annotation(), Some(&preview));
}

#[test]
fn wrong_annotations_are_rejected_and_leave_the_session_alone() {
    let mut artifacts = compile(SHAPES);
    let target = binding(&artifacts, "measure", "s");
    let source = artifacts.source.source.clone();

    for (type_source, expected) in [
        ("<integer>", "semantic"),
        ("<nowhere>", "semantic"),
        ("<integer> = 3", "type-source"),
        ("bonus", "type-source"),
    ] {
        let error = artifacts
            .apply_mutation(SessionMutation::Annotate {
                target: target.clone(),
                type_source: type_source.into(),
            })
            .expect_err(type_source);
        match (expected, &error) {
            ("semantic", MutationError::SemanticFailed { diagnostics }) => {
                assert!(!diagnostics.is_empty());
            }
            ("type-source", MutationError::InvalidTypeSource { .. }) => {}
            _ => panic!("{type_source}: unexpected {error:?}"),
        }
        assert!(artifacts.preview_annotation(&target, type_source).is_err());
    }
    let missing = AnnotationTarget::Binding {
        definition: target.definition(),
        name: "nothing".into(),
        occurrence: 0,
    };
    assert!(matches!(
        artifacts.apply_mutation(SessionMutation::Annotate {
            target: missing,
            type_source: "<circle>".into(),
        }),
        Err(MutationError::UnknownAnnotationTarget { .. })
    ));
    assert_eq!(artifacts.source.source, source);
    assert!(artifacts.mutations().is_empty());
    assert_eq!(artifacts.next_mutation_id(), 0);
}

#[test]
fn annotating_a_let_reports_the_cast_it_adds() {
    let source = SHAPES.replace(
        "define function main()\n  measure(make(<circle>))",
        "define function main()\n  let s = make(<circle>);\n  measure(s)",
    );
    let mut artifacts = compile(&source);
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let target = binding(&artifacts, "main", "s");
    let outcome = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target,
            type_source: "<shape>".into(),
        })
        .expect("annotation compiles");
    assert!(artifacts.source.source.contains("let s :: <shape> = make"));
    let delta = &outcome.annotation().expect("annotate").delta;
    assert!(
        delta
            .changes
            .iter()
            .all(|change| change.before.is_none() || change.after.is_some()),
        "{}",
        delta.render()
    );
}

#[test]
fn annotating_a_result_inserts_an_arrow() {
    let mut artifacts = compile(SHAPES);
    let definition = binding(&artifacts, "measure", "s").definition();
    let target = AnnotationTarget::Result { definition };
    artifacts
        .apply_mutation(SessionMutation::Annotate {
            target,
            type_source: "<integer>".into(),
        })
        .expect("annotation compiles");
    assert!(
        artifacts
            .source
            .source
            .contains("measure(s :: <shape>) => <integer>\n")
    );
    let function = artifacts
        .sem
        .functions
        .iter()
        .find(|function| function.name == "measure")
        .expect("measure");
    assert!(function.return_annotation.is_some());
}

#[test]
fn annotating_a_method_parameter_respecializes_the_method() {
    let source = SHAPES.replace(
        "define method area(q :: <square>)",
        "define method area(q :: <shape>)",
    );
    let mut artifacts = compile(&source);
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let method = &artifacts.ast.methods[1];
    let target = AnnotationTarget::Binding {
        definition: method.definition,
        name: "q".into(),
        occurrence: 0,
    };
    let outcome = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target: target.clone(),
            type_source: "<square>".into(),
        })
        .expect("annotation compiles");
    let annotation = outcome.annotation().expect("annotate");
    assert_ne!(annotation.annotated, target);
    assert_eq!(
        annotation.annotated.definition(),
        artifacts.ast.methods[1].definition
    );
    assert!(artifacts.source.source.contains("area(q :: <square>)"));

    assert!(matches!(
        artifacts.clone().apply_mutation(SessionMutation::Annotate {
            target: annotation.annotated.clone(),
            type_source: "<circle>".into(),
        }),
        Err(MutationError::SemanticFailed { .. })
    ));
}

#[test]
fn rewind_and_replay_carry_annotations() {
    let base = SHAPES.replace(SQUARE_AREA, "");
    let mut artifacts = compile(&base);
    let area = artifacts
        .sem
        .generics
        .iter()
        .find(|generic| generic.name == "area")
        .expect("area")
        .id;
    let added = artifacts
        .apply_mutation(SessionMutation::AddMethod {
            generic: area,
            method_source: SQUARE_AREA.into(),
        })
        .expect("add-method");
    let target = binding(&artifacts, "measure", "s");
    let annotated = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target: target.clone(),
            type_source: "<circle>".into(),
        })
        .expect("annotate");
    assert_eq!(annotated.mutation_id, 1);
    let annotated_source = artifacts.source.source.clone();
    assert!(annotated_source.contains("s :: <circle>"));
    assert!(annotated_source.contains("area(q :: <square>)"));

    // Replay against a base that gained an unrelated definition: the
    // target resolves by name, not by position.
    let new_base = format!("define function unrelated()\n  1\nend;\n{base}");
    let replay = artifacts.replay_against(&SourceFile::named("shapes.loko", &new_base));
    assert!(replay.failed.is_none(), "{:?}", replay.failed);
    assert_eq!(replay.applied.len(), 2);
    assert!(replay.artifacts.source.source.contains("s :: <circle>"));
    assert_eq!(run_main(&replay.artifacts), run_main(&artifacts));

    let rewound = artifacts
        .rewind_to(Some(added.mutation_id))
        .expect("rewind to add-method");
    assert_eq!(rewound.dropped.len(), 1);
    assert!(artifacts.source.source.contains("s :: <shape>"));
    assert!(artifacts.source.source.contains("area(q :: <square>)"));

    // Re-applying gets a fresh id, and the session rebuilds the same text.
    let again = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target,
            type_source: "<circle>".into(),
        })
        .expect("annotate again");
    assert_eq!(again.mutation_id, 2);
    assert_eq!(artifacts.source.source, annotated_source);
    let added_again = artifacts.mutations()[0].mutation_id;
    artifacts
        .rewind_to(Some(added_again))
        .expect("rewind past the annotation");
    artifacts.rewind_to(None).expect("rewind to pristine");
    assert_eq!(artifacts.source.source, base);
}

#[test]
fn binding_ids_survive_unrelated_edits() {
    let source = "\
define function first(n :: <integer>)
  let x = n + 1;
  let y = function (x :: <integer>) x end;
  y(x)
end;
define function main()
  first(1)
end;
";
    let ids = |source: &str| -> Vec<String> {
        compile(source)
            .annotation_sites()
            .into_iter()
            .filter(|site| site.target.to_string().contains(":x"))
            .map(|site| site.target.to_string())
            .collect()
    };
    let before = ids(source);
    assert_eq!(before.len(), 2, "{before:?}");
    assert!(before[0].ends_with(":x"));
    assert!(before[1].ends_with(":x#1"));

    // A new definition ahead of `first`, and a new binding of another name
    // inside it, renumber locals and shift spans but leave the ids alone.
    let edited = format!(
        "define function ahead()\n  2\nend;\n{}",
        source.replace("let x = n + 1;", "let z = n;\n  let x = z + 1;")
    );
    assert_eq!(ids(&edited), before);
}

#[test]
fn policy_changes_report_codegen_deltas_without_touching_the_log() {
    let artifacts = compile(&SHAPES.replace("measure(s :: <shape>)", "measure(s :: <circle>)"));
    let off = artifacts.with_codegen_policy(CodegenPolicy::Off);
    let delta = artifacts.typing_delta(&off);
    assert!(
        delta.changes.iter().any(|change| {
            change.before.as_ref().map(|before| before.codegen) == Some(CheckCodegen::Elided)
                && change.after.as_ref().map(|after| after.codegen) == Some(CheckCodegen::Kept)
        }),
        "{}",
        delta.render()
    );
    assert_eq!(delta.before.proved, delta.after.proved);
    assert!(off.mutations().is_empty());
}

#[test]
fn annotating_a_limited_parameter_proves_its_conversion_and_checks_its_callers() {
    // `n` defaults to `<integer>`, so `as(<u32>, n)` is tested. Limiting it
    // to a digit proves the conversion inside `digit` and moves the range
    // check to `main`, where the 7 it passes is proved too.
    let source = "\
define function digit(n)
  as(<u32>, n)
end;
define function main()
  as(<integer>, digit(7))
end;
";
    let mut artifacts = compile(source);
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let before = run_main(&artifacts);
    let target = binding(&artifacts, "digit", "n");
    let digit = "limited(<integer>, min: 0, max: 9)";

    let outcome = artifacts
        .apply_mutation(SessionMutation::Annotate {
            target: target.clone(),
            type_source: digit.into(),
        })
        .expect("annotation compiles");
    let annotation = outcome.annotation().expect("annotate");
    assert_eq!(annotation.previous, None);
    let conversion = change(annotation, TypingCheck::Range);
    assert_eq!(
        conversion.before.as_ref().map(|before| before.verdict),
        Some(TypingVerdict::Checked)
    );
    assert_eq!(
        conversion.after.as_ref().map(|after| after.verdict),
        Some(TypingVerdict::Proved)
    );
    assert_eq!(
        conversion.after.as_ref().map(|after| after.codegen),
        Some(CheckCodegen::Specialized)
    );
    let rendered = annotation.delta.render();
    assert!(rendered.contains("added=1"), "{rendered}");
    assert!(
        artifacts
            .source
            .source
            .contains("define function digit(n :: limited(<integer>, min: 0, max: 9))")
    );

    let site = artifacts
        .annotation_sites()
        .into_iter()
        .find(|site| site.target == target)
        .expect("n is a site");
    let report = artifacts
        .type_of(&TypeSelector::Span(site.span))
        .expect("n is typed");
    assert_eq!(report.declaration, Some(DeclarationOrigin::Written));
    let types = artifacts
        .explain_types(&target.to_string())
        .expect("binding types");
    assert!(
        types.contains(&format!("declared: {digit} written")),
        "{types}"
    );
    assert_eq!(run_main(&artifacts), before);
}

#[test]
fn narrowing_a_parameter_reports_the_specification_change_and_the_moved_checks() {
    // Limiting `n` proves the conversion in `digit` only by rejecting
    // inputs `digit` accepted before: the range check moves to `main`.
    let source = "\
define function digit(n)
  as(<u32>, n)
end;
define function main()
  as(<integer>, digit(7))
end;
";
    let artifacts = compile(source);
    let target = binding(&artifacts, "digit", "n");
    let preview = artifacts
        .preview_annotation(&target, "limited(<integer>, min: 0, max: 9)")
        .expect("annotation compiles");
    let delta = &preview.delta;
    assert!(delta.changes_specification());
    let [contract] = delta.contracts.as_slice() else {
        panic!("one contract change in\n{}", delta.render());
    };
    assert_eq!(contract.direction, ContractDirection::Narrowed);
    assert_eq!(contract.position, ContractPosition::Parameter(0));
    assert_eq!(contract.name.as_deref(), Some("n"));
    assert_eq!(contract.before, "<integer>");
    assert_eq!(contract.after, "limited(<integer>, min: 0, max: 9)");
    assert!(contract.moves_obligation_to_callers());

    let shift = delta.obligations();
    assert_eq!(shift.removed_here, 1, "{}", delta.render());
    assert_eq!(shift.moved_to_callers, 1, "{}", delta.render());
    assert_eq!(shift.callers_proved, 1, "{}", delta.render());
    assert_eq!(shift.callers_runtime, 0);
    let moved = delta
        .changes
        .iter()
        .find(|change| delta.in_caller(change))
        .expect("a caller change");
    assert_eq!(moved.effect(), ObligationEffect::Imposed);
    assert!(moved.before.is_none());

    let rendered = delta.render();
    assert!(
        rendered.starts_with("contract delta: SPECIFICATION CHANGED changes=1 narrows-inputs=1\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains("narrowed param n of function digit (contract:def")
            && rendered.contains(
                "<integer> -> limited(<integer>, min: 0, max: 9); callers now carry the obligation"
            ),
        "{rendered}"
    );
    assert!(
        rendered.contains("obligations: removed-here=1 added-here=0 moved-to-callers=1 (proved=1"),
        "{rendered}"
    );
    assert!(rendered.contains(" in=caller\n"), "{rendered}");
}

#[test]
fn widening_a_parameter_and_keeping_contracts_are_reported_as_such() {
    let artifacts = compile(SHAPES);
    let target = binding(&artifacts, "measure", "s");
    let widened = artifacts
        .preview_annotation(&target, "<object>")
        .expect("annotation compiles");
    let [contract] = widened.delta.contracts.as_slice() else {
        panic!("one contract change in\n{}", widened.delta.render());
    };
    assert_eq!(contract.direction, ContractDirection::Widened);
    assert!(!contract.moves_obligation_to_callers());
    assert!(
        widened
            .delta
            .render()
            .contains("contract delta: SPECIFICATION CHANGED changes=1 narrows-inputs=0"),
        "{}",
        widened.delta.render()
    );

    // A `let` annotation and a result that writes the inferred type keep
    // every contract.
    let bonus = binding(&artifacts, "measure", "bonus");
    let kept = artifacts
        .preview_annotation(&bonus, "<integer>")
        .expect("annotation compiles");
    assert!(!kept.delta.changes_specification());
    assert!(
        kept.delta
            .render()
            .starts_with("contract delta: none (every contract kept)\n"),
        "{}",
        kept.delta.render()
    );
    let result = AnnotationTarget::Result {
        definition: target.definition(),
    };
    let same = artifacts
        .preview_annotation(&result, "<integer>")
        .expect("annotation compiles");
    assert!(same.delta.contracts.is_empty(), "{}", same.delta.render());

    // A policy change edits no source, so it reports no contract delta.
    let off = artifacts.with_codegen_policy(CodegenPolicy::Off);
    let delta = artifacts.typing_delta(&off);
    assert!(delta.edited.is_empty() && delta.contracts.is_empty());
    assert!(delta.render().starts_with("typing delta:"));
}
