use alloc::vec;

use interloko_model::types::ClassHierarchy;

use super::*;
use crate::grammar_frontend;
use crate::identity::SourceFileId;
use crate::{ast, sem};

fn module(source: &str) -> sem::Module {
    let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
    sem::lower(&ast::lower(&tree))
}

const SHAPES: &str = "define class <shape> (<object>)\nend;\n\
     define class <circle> (<shape>)\nend;\n\
     define class <square> (<shape>)\nend;\n\
     define class <label> (<object>)\nend;\n\
     define generic area(x);\n\
     define method area(c :: <circle>)\n  1\nend;\n\
     define method area(s :: <square>)\n  2\nend;\n";

fn decisions_for(source: &str) -> (sem::Module, TypingArtifact) {
    let module = module(source);
    assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
    let artifact = analyze(&module, TypingConfig::soft());
    (module, artifact)
}

fn main_decisions(module: &sem::Module, artifact: &TypingArtifact) -> Vec<TypingDecision> {
    let main = module
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("main");
    artifact
        .definitions
        .iter()
        .find(|definition| definition.definition == main.definition)
        .map(|definition| definition.decisions.clone())
        .unwrap_or_default()
}

#[test]
fn dispatch_on_a_covered_class_is_proved_and_selects_statically() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  area(make(<circle>))\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions.len(), 1);
    let decision = &decisions[0];
    assert_eq!(decision.check, TypingCheck::Dispatch);
    assert_eq!(decision.verdict, TypingVerdict::Proved);
    assert_eq!(decision.rule, TypingRule::DispatchCovered);
    assert_eq!(decision.selected_method, Some(module.methods[0].id));
    // `area` specializes only on source classes, so it never boxes.
    assert!(matches!(
        &decision.expected,
        TypingExpectation::Dispatch { boxed_params, .. } if boxed_params.is_empty()
    ));
    assert!(artifact.diagnostics.is_empty());
}

#[test]
fn dispatch_on_a_superclass_is_runtime_checked_and_silent_by_default() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function measure(s :: <shape>)\n  area(s)\nend;\n\
         define function main()\n  measure(make(<circle>))\nend;"
    ));
    let decision = artifact
        .decisions()
        .find(|decision| decision.check == TypingCheck::Dispatch)
        .expect("dispatch decision");
    assert_eq!(decision.verdict, TypingVerdict::Checked);
    assert_eq!(decision.rule, TypingRule::DispatchNeedsRuntimeClass);
    assert_eq!(decision.selected_method, None);
    assert!(artifact.diagnostics.is_empty());

    // Under the strict preset every definition must prove its sends.
    let strict = artifact.with_config(&module, TypingConfig::strict());
    assert_eq!(strict.diagnostics.len(), 1);
    assert_eq!(
        strict.diagnostics[0].code,
        DiagnosticCode::TypingUnprovedDispatch
    );
    assert_eq!(strict.diagnostics[0].severity, Severity::Error);
    assert_eq!(strict.definitions, artifact.definitions);
}

#[test]
fn dispatch_with_no_possible_method_is_refuted_and_warns() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  \
         if (1 < 2) area(make(<circle>)) else area(make(<shape>)) end\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    let verdicts = decisions
        .iter()
        .map(|decision| decision.verdict)
        .collect::<Vec<_>>();
    assert_eq!(verdicts, [TypingVerdict::Proved, TypingVerdict::Refuted]);
    assert_eq!(decisions[1].rule, TypingRule::DispatchNoApplicableMethod);
    assert_eq!(artifact.diagnostics.len(), 1);
    let diagnostic = &artifact.diagnostics[0];
    assert_eq!(
        diagnostic.code,
        DiagnosticCode::TypingDispatchNoApplicableMethod
    );
    assert_eq!(diagnostic.severity, Severity::Warning);
    assert_eq!(diagnostic.span, decisions[1].span);
    assert!(
        diagnostic
            .args
            .iter()
            .any(|arg| arg.name == "arguments" && arg.value.render() == "exactly <shape>")
    );
}

#[test]
fn a_refuted_send_still_yields_its_generic_result() {
    // No method accepts a `<label>`, so the send always fails. It signals a
    // `<no-applicable-method>` whose `use-value` restart supplies results
    // the dispatcher checks against the generic's, so the code after the
    // send is reachable and sees an `<integer>`, not bottom: the overflow
    // check after it is decided on that, not dropped as unreachable.
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function f(l :: <label>) => <integer>\n  area(l) + 1\nend;\n\
         define function main()\n  0\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "f", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(dispatch.verdict, TypingVerdict::Refuted);
    let [overflow] = checks_of(&module, &artifact, "f", TypingCheck::Overflow)
        .try_into()
        .expect("one overflow check");
    assert_eq!(overflow.verdict, TypingVerdict::Checked);
    assert_ne!(overflow.rule, TypingRule::Unreachable);
    assert_eq!(type_name(&module, &overflow.actual[0]), "<integer>");
}

#[test]
fn a_send_does_not_narrow_its_arguments() {
    // A send that finds no method continues with a `use-value`
    // replacement, so its arguments need not be instances of any method's
    // specializers afterwards: the test after it still depends on `x`.
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function f(x :: <object>) => <boolean>\n  \
         let a = area(x);\n  instance?(x, <shape>)\nend;\n\
         define function main()\n  0\nend;"
    ));
    let [test] = checks_of(&module, &artifact, "f", TypingCheck::Instance)
        .try_into()
        .expect("one instance test");
    assert_eq!(test.verdict, TypingVerdict::Checked);
}

#[test]
fn dispatch_on_an_unrelated_class_is_refuted() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function name(l :: <label>)\n  area(l)\nend;\n\
         define function main()\n  0\nend;"
    ));
    let decision = artifact.decisions().next().expect("decision");
    assert_eq!(decision.verdict, TypingVerdict::Refuted);
    assert_eq!(type_name(&module, &decision.actual[0]), "<label>");
}

#[test]
fn casts_are_classified_as_upcast_narrowing_or_disjoint() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function up()\n  as(<shape>, make(<circle>))\nend;\n\
         define function down(s :: <shape>)\n  as(<circle>, s)\nend;\n\
         define function never()\n  as(<circle>, make(<shape>))\nend;\n\
         define function main()\n  0\nend;"
    ));
    let verdict_of = |name: &str| {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function");
        artifact
            .decision(function.definition, SiteId(0))
            .map(|decision| (decision.verdict, decision.rule))
            .expect("cast decision")
    };
    assert_eq!(
        verdict_of("up"),
        (TypingVerdict::Proved, TypingRule::CastUpcast)
    );
    assert_eq!(
        verdict_of("down"),
        (TypingVerdict::Checked, TypingRule::CastNarrowing)
    );
    assert_eq!(
        verdict_of("never"),
        (TypingVerdict::Refuted, TypingRule::CastDisjoint)
    );
    let codes = artifact
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, [DiagnosticCode::TypingCastAlwaysFails]);
}

#[test]
fn checking_distributes_over_conditional_operands() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function pick(s :: <shape>)\n  \
         as(<circle>, if (1 < 2) make(<shape>) else s end)\nend;\n\
         define function main()\n  0\nend;"
    ));
    let pick = &module.functions[0];
    let decision = artifact
        .decision(pick.definition, SiteId(0))
        .expect("decision");
    assert_eq!(decision.verdict, TypingVerdict::Checked);
    assert_eq!(decision.rule, TypingRule::CastNarrowing);
}

#[test]
fn closure_bodies_are_checked_under_their_enclosing_definition() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  \
         let f :: <callable> = function () => <integer>\n    area(make(<shape>))\n  end;\n  0\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].verdict, TypingVerdict::Refuted);
}

#[test]
fn dump_lists_policy_and_decisions_by_definition() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  area(make(<shape>))\nend;"
    ));
    let dump = artifact.dump(&module);
    assert!(dump.starts_with(
        "typing config=soft status=checked decisions=1 proved=0 checked=0 refuted=1\n"
    ));
    assert!(dump.contains("  policy dispatch proved=silent checked=silent refuted=warning\n"));
    assert!(dump.contains("  policy instance proved=warning checked=silent refuted=warning\n"));
    assert!(dump.contains(" function main\n"));
    assert!(dump.contains("s0 dispatch refuted rule=dispatch-no-applicable-method span="));
    assert!(
        dump.contains("expected=dispatch area methods=2 actual=exactly <shape> severity=warning")
    );
}

const FLOATS: &str = "define generic size-of(x);\n\
     define method size-of(x :: <double-float>)\n  1\nend;\n\
     define method size-of(x :: <integer>)\n  2\nend;\n\
     define generic magnitude(x);\n\
     define method magnitude(x :: <float>)\n  5\nend;\n\
     define generic whole(x);\n\
     define method whole(x :: <integer>)\n  x\nend;\n";

/// Verdicts of `name`'s decisions other than overflow checks.
fn verdicts_of(module: &sem::Module, artifact: &TypingArtifact, name: &str) -> Vec<TypingVerdict> {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function");
    artifact
        .definitions
        .iter()
        .find(|definition| definition.definition == function.definition)
        .map(|definition| {
            definition
                .decisions
                .iter()
                .filter(|decision| decision.check != TypingCheck::Overflow)
                .map(|decision| decision.verdict)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn float_values_have_exact_double_float_class_under_abstract_float() {
    let (module, artifact) = decisions_for(&format!(
        "{FLOATS}define function main()\n  \
         if (1 < 2) size-of(2.5) + magnitude(2.5) else whole(2.5) end\nend;"
    ));
    let double_float = module
        .builtin_value_class(sem::BuiltinValueClass::DoubleFloat)
        .expect("double-float");
    let float = module
        .builtin_value_class(sem::BuiltinValueClass::Float)
        .expect("float");
    assert!(module.is_closed(double_float));
    assert!(!module.is_closed(float));
    let integer = module
        .builtin_value_class(sem::BuiltinValueClass::Integer)
        .expect("integer");
    let rational = module
        .builtin_value_class(sem::BuiltinValueClass::Rational)
        .expect("rational");
    assert!(module.is_closed(integer));
    assert!(!module.is_closed(rational));
    assert!(module.is_same_or_subclass(integer, rational));

    assert_eq!(
        verdicts_of(&module, &artifact, "main"),
        [
            TypingVerdict::Proved,
            TypingVerdict::Proved,
            TypingVerdict::Refuted
        ]
    );
    let refuted = artifact
        .decisions()
        .find(|decision| decision.verdict == TypingVerdict::Refuted)
        .expect("refuted dispatch");
    assert_eq!(refuted.actual[0].exact_class(&module), Some(double_float));
    // `whole` specializes on `<integer>`, so its parameter carries boxed
    // scalars; the decision records sem's boxing choice.
    assert!(matches!(
        &refuted.expected,
        TypingExpectation::Dispatch { boxed_params, .. } if boxed_params == &[0]
    ));
    assert_eq!(type_name(&module, &refuted.actual[0]), "<double-float>");
}

#[test]
fn float_casts_are_proved_checked_or_refuted() {
    let (module, artifact) = decisions_for(&format!(
        "{FLOATS}define function to-float(x :: <object>) => <double-float>\n  \
         as(<double-float>, x)\nend;\n\
         define function up(x :: <double-float>) => <object>\n  as(<float>, x)\nend;\n\
         define function never() => <double-float>\n  \
         as(<double-float>, as(<object>, \"text\"))\nend;\n\
         define function on-object(x :: <object>)\n  size-of(x)\nend;\n\
         define function main()\n  0\nend;"
    ));
    assert_eq!(
        verdicts_of(&module, &artifact, "to-float"),
        [TypingVerdict::Checked]
    );
    assert_eq!(
        verdicts_of(&module, &artifact, "up"),
        [TypingVerdict::Proved]
    );
    assert_eq!(
        verdicts_of(&module, &artifact, "never"),
        [TypingVerdict::Proved, TypingVerdict::Refuted]
    );
    assert_eq!(
        verdicts_of(&module, &artifact, "on-object"),
        [TypingVerdict::Checked]
    );
    let codes = artifact
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, [DiagnosticCode::TypingCastAlwaysFails]);
}

#[test]
fn limited_integer_operands_keep_their_range() {
    let (module, artifact) = decisions_for(
        "define generic weight(x);\n\
         define method weight(x :: <integer>) => <integer>\n  1\nend;\n\
         define function frames() => <u32>\n  5\nend;\n\
         define function main(step :: <i32>) => <integer>\n  \
         weight(step) + weight(frames()) + weight(7)\nend;",
    );
    let decisions = main_decisions(&module, &artifact)
        .into_iter()
        .filter(|decision| decision.check == TypingCheck::Dispatch)
        .collect::<Vec<_>>();
    let actual = decisions
        .iter()
        .map(|decision| type_name_list(&module, &decision.actual))
        .collect::<Vec<_>>();
    assert_eq!(actual, ["<i32>", "<u32>", "singleton(7)"]);
    assert!(
        decisions
            .iter()
            .all(|decision| decision.verdict == TypingVerdict::Proved)
    );
    let integer = module
        .builtin_value_class(sem::BuiltinValueClass::Integer)
        .expect("integer");
    assert!(
        decisions
            .iter()
            .all(|decision| decision.actual[0].exact_class(&module) == Some(integer))
    );
}

#[test]
fn a_cast_that_always_fails_still_yields_its_target() {
    // The operand is exactly `<shape>`, so the cast always fails. It
    // signals a `<type-error>` whose `use-value` restart may supply any
    // `<circle>`, which the cast checks again, so the dispatch after it
    // sees a `<circle>` rather than no value at all.
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  area(as(<circle>, make(<shape>)))\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0].check, TypingCheck::Cast);
    assert_eq!(decisions[0].verdict, TypingVerdict::Refuted);
    assert_eq!(decisions[1].check, TypingCheck::Dispatch);
    assert_eq!(type_name(&module, &decisions[1].actual[0]), "<circle>");
    assert_eq!(decisions[1].verdict, TypingVerdict::Proved);
    assert_eq!(decisions[1].rule, TypingRule::DispatchCovered);
    assert_eq!(artifact.diagnostics.len(), 1);
}

#[test]
fn a_cast_that_may_fail_forgets_its_operand() {
    // When the cast passes, its value is the operand's exact `<circle>`
    // (the join of an exact `<circle>` and an exact `<square>`); when it
    // fails, a `use-value` restart supplies some other `<circle>`.
    // Only the target is certain, so the dispatch is not narrowed to the
    // exact class.
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function pick(s :: <shape>)\n  \
         area(as(<circle>, if (instance?(s, <circle>)) as(<shape>, make(<circle>)) \
         else as(<shape>, make(<square>)) end))\nend;\n\
         define function main()\n  0\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "pick", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(type_name(&module, &dispatch.actual[0]), "<circle>");
    assert_eq!(dispatch.selected_method, None);
}

#[test]
fn synthesis_joins_branches_and_stays_within_sem() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function pick(s :: <shape>)\n  \
         as(<shape>, if (1 < 2) as(<shape>, make(<circle>)) \
         else as(<shape>, make(<square>)) end)\nend;\n\
         define function same()\n  \
         as(<shape>, if (1 < 2) make(<circle>) else make(<circle>) end)\nend;\n\
         define function main()\n  0\nend;"
    ));
    let actual = |name: &str| {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function");
        // The outer cast is checked after the casts inside it.
        let decision = artifact
            .definitions
            .iter()
            .find(|typed| typed.definition == function.definition)
            .and_then(|typed| typed.decisions.last())
            .expect("decision");
        type_name(&module, &decision.actual[0])
    };
    assert_eq!(
        actual("pick"),
        "type-union(exactly <circle>, exactly <square>)"
    );
    assert_eq!(actual("same"), "exactly <circle>");
    for function in &module.functions {
        let kind = module.expr_kind(&function.body.result);
        let ty = synthesize(&module, &function.body.result);
        assert!(ty.is_subtype(&module.kind_type(kind), &module));
    }
}

#[test]
fn semantic_errors_block_the_pass() {
    let module = module("define function main() => <integer>\n  #t\nend;");
    assert!(!module.diagnostics.is_empty());
    let artifact = analyze(&module, TypingConfig::soft());
    assert_eq!(
        artifact.status,
        TypingStatus::BlockedBySemanticDiagnostics {
            count: module.diagnostics.len()
        }
    );
    assert!(artifact.definitions.is_empty());
    assert!(artifact.diagnostics.is_empty());
}

#[test]
fn configuration_presets_and_policies() {
    assert_eq!(TypingConfig::default(), TypingConfig::soft());
    for name in TypingConfig::PRESETS {
        let config = TypingConfig::preset(name).expect("preset");
        assert_eq!(config.preset_name(), Some(*name));
    }
    assert_eq!(TypingConfig::preset("loud"), None);
    let soft = TypingConfig::soft();
    assert_eq!(
        soft.severity(TypingCheck::Dispatch, TypingVerdict::Refuted),
        TypingSeverity::Warning
    );
    assert_eq!(
        soft.severity(TypingCheck::Cast, TypingVerdict::Checked),
        TypingSeverity::Silent
    );
    let custom = soft.with_policy(
        TypingCheck::Cast,
        TypingPolicy::new(TypingSeverity::Warning, TypingSeverity::Error),
    );
    assert_eq!(custom.preset_name(), None);
    assert_eq!(custom.dispatch, soft.dispatch);
    assert_eq!(
        custom.severity(TypingCheck::Cast, TypingVerdict::Refuted),
        TypingSeverity::Error
    );
    for verdict in TypingVerdict::ALL {
        assert_eq!(
            TypingConfig::strict().severity(TypingCheck::Dispatch, TypingVerdict::Proved),
            TypingSeverity::Silent
        );
        assert_eq!(verdict.join(*verdict), *verdict);
    }
    assert_eq!(
        TypingVerdict::Proved.join(TypingVerdict::Refuted),
        TypingVerdict::Checked
    );
}

#[test]
fn decisions_are_independent_of_other_definitions() {
    let base = format!(
        "{SHAPES}define function measure(s :: <shape>)\n  area(s)\nend;\n\
         define function main()\n  area(make(<shape>))\nend;"
    );
    let (module, artifact) = decisions_for(&base);
    let measure = module.functions[0].definition;
    let alone = check_definition(&module, measure).expect("measure typing");
    assert_eq!(
        Some(&alone),
        artifact
            .definitions
            .iter()
            .find(|definition| definition.definition == measure)
    );
    assert_eq!(alone.kind, DefinitionKind::Function);
}

/// The typing of function `name`.
fn typed<'a>(
    module: &sem::Module,
    artifact: &'a TypingArtifact,
    name: &str,
) -> &'a DefinitionTyping {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function");
    artifact.definition(function.definition).expect("typed")
}

/// The decisions of function `name` for `check`, in site order.
fn checks_of(
    module: &sem::Module,
    artifact: &TypingArtifact,
    name: &str,
    check: TypingCheck,
) -> Vec<TypingDecision> {
    typed(module, artifact, name)
        .decisions
        .iter()
        .filter(|decision| decision.check == check)
        .cloned()
        .collect()
}

/// The rendered reason chain of `decision`'s operand `index`.
fn chain(
    module: &sem::Module,
    artifact: &TypingArtifact,
    decision: &TypingDecision,
    index: usize,
) -> String {
    artifact
        .definition(decision.owner)
        .expect("owner")
        .reason_chain(module, decision.because[index])
}

const WHOLE: &str = "define generic whole(x);\n\
     define method whole(x :: <integer>) => <integer>\n  x\nend;\n";

#[test]
fn instance_then_branch_meets_the_tested_class() {
    // Flow typing narrows `x` by the test that sem refines it by, so the
    // read sem gives as an unboxed `<integer>` has that type here too.
    let (module, artifact) = decisions_for(&format!(
        "{WHOLE}define function f(x :: <object>) => <integer>\n  \
         if (instance?(x, <integer>)) whole(x) else 0 end\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "f", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
    assert_eq!(type_name(&module, &dispatch.actual[0]), "<integer>");
    assert!(
        chain(&module, &artifact, &dispatch, 0).starts_with("narrowed(s0,then)@"),
        "{}",
        chain(&module, &artifact, &dispatch, 0)
    );
}

#[test]
fn instance_else_branch_removes_the_tested_class() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function f(round? :: <boolean>) => <integer>\n  \
         let c :: <shape> = make(<circle>);\n  let s :: <shape> = make(<square>);\n  \
         let shape = if (round?) c else s end;\n  \
         if (instance?(shape, <circle>)) 0 else area(shape) end\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "f", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(type_name(&module, &dispatch.actual[0]), "exactly <square>");
    assert_eq!(dispatch.selected_method, Some(module.methods[1].id));
    assert!(chain(&module, &artifact, &dispatch, 0).starts_with("narrowed(s0,else)@"));
    // The test itself is decided at runtime.
    let [instance] = checks_of(&module, &artifact, "f", TypingCheck::Instance)
        .try_into()
        .expect("one instance test");
    assert_eq!(instance.rule, TypingRule::InstanceSometimes);
}

#[test]
fn limited_instance_tests_narrow_ranges_in_both_branches() {
    let (module, artifact) = decisions_for(
        "define function f(x :: <integer>) => <u32>\n  \
         if (instance?(x, <i32>)) as(<u32>, as(<i32>, x)) else as(<u32>, x) end\nend;",
    );
    let ranges = checks_of(&module, &artifact, "f", TypingCheck::Range);
    let summary = ranges
        .iter()
        .map(|decision| (decision.rule, type_name(&module, &decision.actual[0])))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (TypingRule::RangeOverlaps, "<integer>".into()),
            (TypingRule::RangeWithin, "<i32>".into()),
            (TypingRule::RangeOverlaps, "<i32>".into()),
            (
                TypingRule::RangeOverlaps,
                "type-union(limited(<integer>, max: -2147483649), \
                 limited(<integer>, min: 2147483648))"
                    .into()
            ),
        ]
    );
}

#[test]
fn a_limited_test_bounds_arithmetic_on_the_unboxed_local() {
    // Sem reads `x` as an `<integer>` in the then branch, and flow typing
    // knows it is an `<i32>` there, so `x + 1` cannot overflow.
    let (module, artifact) = decisions_for(
        "define function f(x :: <object>) => <integer>\n  \
         if (instance?(x, <i32>)) x + 1 else 0 end\nend;",
    );
    let sem::Expr::If { refinement, .. } = &module.functions[0].body.result else {
        panic!("expected if");
    };
    assert_eq!(
        refinement.map(|refinement| refinement.kind),
        Some(sem::ValueKind::I64)
    );
    let [overflow] = checks_of(&module, &artifact, "f", TypingCheck::Overflow)
        .try_into()
        .expect("one overflow check");
    assert_eq!(overflow.rule, TypingRule::OverflowImpossible);
    assert_eq!(type_name(&module, &overflow.actual[0]), "<i32>");
    assert!(
        chain(&module, &artifact, &overflow, 0).starts_with("narrowed(s0,then)@"),
        "{}",
        chain(&module, &artifact, &overflow, 0)
    );
}

#[test]
fn a_proved_numeric_send_has_its_method_result() {
    let (module, artifact) = decisions_for(
        "define function main(d :: <integer>) => <integer>\n  \
         as(<integer>, (as(<object>, d) + 1) + 2)\nend;",
    );
    let [inner, outer] = checks_of(&module, &artifact, "main", TypingCheck::Dispatch)
        .try_into()
        .expect("two sends");
    assert_eq!(inner.verdict, TypingVerdict::Proved);
    // The second send sees the first's result as the `<integer>` its
    // method returns, not the generic's `<object>`, so it is proved too.
    assert_eq!(outer.verdict, TypingVerdict::Proved);
    assert_eq!(type_name(&module, &outer.actual[0]), "<integer>");
    assert!(
        chain(&module, &artifact, &outer, 0).starts_with(&format!("dispatched({})@", inner.site)),
        "{}",
        chain(&module, &artifact, &outer, 0)
    );
    // The cast relies on the method set the sends' results come from.
    let main = typed(&module, &artifact, "main");
    let [cast] = checks_of(&module, &artifact, "main", TypingCheck::Cast)
        .into_iter()
        .filter(|cast| {
            cast.expected
                == TypingExpectation::Class(
                    module
                        .builtin_value_class(sem::BuiltinValueClass::Integer)
                        .expect("integer"),
                )
        })
        .collect::<Vec<_>>()
        .try_into()
        .expect("one cast to <integer>");
    let TypingExpectation::Dispatch { generic, .. } = &inner.expected else {
        panic!("a dispatch expectation");
    };
    assert!(
        main.dependencies(&cast)
            .contains(&TypingDependency::Methods(*generic))
    );
}

#[test]
fn integer_comparisons_narrow_both_branches() {
    let (module, artifact) = decisions_for(
        "define function f(n :: <integer>) => <i32>\n  \
         if (n < 0) 0i32\n  \
         else if (100 >= n) as(<i32>, n) else as(<i32>, n - 100) end\n  end\nend;",
    );
    let ranges = checks_of(&module, &artifact, "f", TypingCheck::Range);
    let summary = ranges
        .iter()
        .map(|decision| (decision.rule, type_name(&module, &decision.actual[0])))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (
                TypingRule::RangeWithin,
                "limited(<integer>, min: 0, max: 100)".into()
            ),
            // `n - 100` may overflow, so it has the full `<integer>` range.
            (TypingRule::RangeOverlaps, "<integer>".into()),
        ]
    );
    assert!(chain(&module, &artifact, &ranges[0], 0).starts_with("narrowed(then)@"));
    // `n` is above 100 but unbounded, so `n - 100` may overflow.
    let [overflow] = checks_of(&module, &artifact, "f", TypingCheck::Overflow)
        .try_into()
        .expect("one overflow check");
    assert_eq!(overflow.rule, TypingRule::OverflowPossible);
}

#[test]
fn equality_narrows_to_the_point_and_removes_it_otherwise() {
    let (module, artifact) = decisions_for(
        "define function f(n :: <u32>) => <i32>\n  \
         if (n = 0u32) as(<i32>, n) else as(<i32>, n - 1u32) end\nend;",
    );
    let ranges = checks_of(&module, &artifact, "f", TypingCheck::Range);
    assert_eq!(ranges[0].rule, TypingRule::RangeWithin);
    assert_eq!(type_name(&module, &ranges[0].actual[0]), "singleton(0)");
    // Off zero, `n - 1` cannot go below zero.
    let [overflow] = checks_of(&module, &artifact, "f", TypingCheck::Overflow)
        .try_into()
        .expect("one overflow check");
    assert_eq!(overflow.rule, TypingRule::OverflowImpossible);
}

#[test]
fn handler_condition_local_has_the_handler_class() {
    let (module, artifact) = decisions_for(
        "define class <problem> (<object>)\nend;\n\
         define class <bad-radius> (<problem>)\nend;\n\
         define generic describe(p);\n\
         define method describe(p :: <problem>) => <integer>\n  7\nend;\n\
         define function main() => <integer>\n  \
         handle signal(make(<bad-radius>)) with restart e :: <problem> describe(e) end\nend;",
    );
    let [dispatch] = checks_of(&module, &artifact, "main", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
    assert!(chain(&module, &artifact, &dispatch, 0).starts_with("handler-condition(<problem>)@"));
}

#[test]
fn dispatch_distributes_over_union_members() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function both(round? :: <boolean>) => <integer>\n  \
         let c :: <shape> = make(<circle>);\n  let s :: <shape> = make(<square>);\n  \
         area(if (round?) c else s end)\nend;\n\
         define function some(round? :: <boolean>) => <integer>\n  \
         let c :: <shape> = make(<circle>);\n  let s :: <shape> = make(<shape>);\n  \
         area(if (round?) c else s end)\nend;"
    ));
    let [both] = checks_of(&module, &artifact, "both", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(
        (both.verdict, both.rule),
        (TypingVerdict::Proved, TypingRule::DispatchCovered)
    );
    assert_eq!(
        type_name(&module, &both.actual[0]),
        "type-union(exactly <circle>, exactly <square>)"
    );
    assert!(chain(&module, &artifact, &both, 0).starts_with("joined@"));
    let [some] = checks_of(&module, &artifact, "some", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(
        (some.verdict, some.rule),
        (
            TypingVerdict::Checked,
            TypingRule::DispatchNeedsRuntimeClass
        )
    );
}

#[test]
fn handle_joins_its_body_and_handler() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define class <problem> (<object>)\nend;\n\
         define function main() => <integer>\n  \
         let c :: <shape> = make(<circle>);\n  let s :: <shape> = make(<square>);\n  \
         area(handle c with restart <problem> s end)\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "main", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(
        type_name(&module, &dispatch.actual[0]),
        "type-union(exactly <circle>, exactly <square>)"
    );
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
}

#[test]
fn an_unreachable_branch_drops_out_of_the_join() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main() => <integer>\n  \
         let c :: <shape> = make(<circle>);\n  let d :: <shape> = make(<circle>);\n  \
         let s :: <shape> = make(<square>);\n  \
         area(if (instance?(c, <circle>)) d else as(<shape>, s) end)\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    // The test is always true, so the else branch's cast is unreachable and
    // the call sees only the then branch.
    let rules = decisions
        .iter()
        .map(|decision| (decision.check, decision.rule))
        .collect::<Vec<_>>();
    assert_eq!(
        rules,
        [
            (TypingCheck::Instance, TypingRule::InstanceAlways),
            (TypingCheck::Cast, TypingRule::Unreachable),
            (TypingCheck::Dispatch, TypingRule::DispatchCovered),
        ]
    );
    assert_eq!(
        type_name(&module, &decisions[2].actual[0]),
        "exactly <circle>"
    );
    assert_eq!(type_name(&module, &decisions[1].actual[0]), "type-union()");
    assert!(chain(&module, &artifact, &decisions[1], 0).starts_with("narrowed(s0,else)@"));
    // Only the decided test is reported.
    let codes = artifact
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, [DiagnosticCode::TypingInstanceAlwaysTrue]);
}

#[test]
fn block_exit_arguments_join_the_block_value() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function choose(n :: <integer>) => <integer>\n  \
         let s :: <shape> = make(<square>);\n  \
         let shape :: <shape> = block (return)\n    \
         let c :: <shape> = make(<circle>);\n    \
         let picked = if (n > 10) return(c) else s end;\n    picked\n  end;\n  \
         area(shape)\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "choose", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(
        type_name(&module, &dispatch.actual[0]),
        "type-union(exactly <circle>, exactly <square>)"
    );
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
}

#[test]
fn an_escaping_exit_joins_the_declared_block_type() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function keep(out :: <callable>) => <integer>\n  0\nend;\n\
         define function choose() => <integer>\n  \
         let s :: <shape> = make(<square>);\n  \
         let shape :: <shape> = block (return)\n    \
         let kept = keep(return);\n    s\n  end;\n  \
         area(shape)\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "choose", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(type_name(&module, &dispatch.actual[0]), "<shape>");
    assert_eq!(dispatch.verdict, TypingVerdict::Checked);
}

#[test]
fn literals_have_singleton_types_and_decide_conditions() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}{WHOLE}define function main() => <integer>\n  \
         if (#t) whole(7) else area(make(<shape>)) end\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(type_name(&module, &decisions[0].actual[0]), "singleton(7)");
    assert!(chain(&module, &artifact, &decisions[0], 0).starts_with("literal@"));
    // `#t` never takes the else branch, so its refuted call is unreachable
    // and not reported.
    assert_eq!(decisions[1].rule, TypingRule::Unreachable);
    assert!(artifact.diagnostics.is_empty());
}

#[test]
fn make_has_its_exact_class_and_casts_meet_their_target() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main() => <integer>\n  \
         let c :: <object> = make(<circle>);\n  area(as(<shape>, c))\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions[0].rule, TypingRule::CastUpcast);
    assert!(chain(&module, &artifact, &decisions[0], 0).starts_with("constructed@"));
    assert_eq!(
        type_name(&module, &decisions[1].actual[0]),
        "exactly <circle>"
    );
    assert!(chain(&module, &artifact, &decisions[1], 0).starts_with("check(s0)@"));
}

#[test]
fn integer_arithmetic_uses_intervals_and_overflow_widens_to_the_kind() {
    let (module, artifact) = decisions_for(&format!(
        "{WHOLE}define function main(n :: <i32>) => <integer>\n  \
         let a = 3;\n  let b = a * 4 + 1;\n  whole(b) + whole(n + 1i32)\nend;"
    ));
    let dispatches = checks_of(&module, &artifact, "main", TypingCheck::Dispatch);
    assert_eq!(
        type_name(&module, &dispatches[0].actual[0]),
        "singleton(13)"
    );
    assert!(chain(&module, &artifact, &dispatches[0], 0).starts_with("interval@"));
    // `n + 1` may overflow `<i32>`, so its result is any `<i32>`.
    assert_eq!(type_name(&module, &dispatches[1].actual[0]), "<i32>");
    assert!(chain(&module, &artifact, &dispatches[1], 0).starts_with("check(s"));
    let rules = checks_of(&module, &artifact, "main", TypingCheck::Overflow)
        .iter()
        .map(|decision| decision.rule)
        .collect::<Vec<_>>();
    assert_eq!(
        rules,
        [
            TypingRule::OverflowImpossible,
            TypingRule::OverflowImpossible,
            TypingRule::OverflowPossible,
            TypingRule::OverflowPossible,
        ]
    );
}

#[test]
fn certain_overflow_is_refuted_and_yields_its_kind() {
    let (module, artifact) = decisions_for(&format!(
        "{WHOLE}define function main() => <integer>\n  \
         let top :: <i32> = 2147483647;\n  whole(top + 1i32)\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(
        (decisions[0].check, decisions[0].verdict, decisions[0].rule),
        (
            TypingCheck::Overflow,
            TypingVerdict::Refuted,
            TypingRule::OverflowCertain
        )
    );
    // The overflow signals a `<type-error>`, and a `use-value` restart may
    // supply any `<i32>` for the sum, so the dispatch after it is reached.
    assert_eq!(type_name(&module, &decisions[1].actual[0]), "<i32>");
    assert_eq!(decisions[1].rule, TypingRule::DispatchCovered);
    let [diagnostic] = artifact.diagnostics.as_slice() else {
        panic!("{:?}", artifact.diagnostics);
    };
    assert_eq!(diagnostic.code, DiagnosticCode::TypingOverflowCertain);
    assert!(
        diagnostic
            .args
            .iter()
            .any(|arg| arg.name == "operation" && arg.value.render() == "+")
    );
}

#[test]
fn calls_take_their_callee_contract_not_its_body() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function declared() => <shape>\n  make(<circle>)\nend;\n\
         define function inferred()\n  make(<circle>)\nend;\n\
         define function main() => <integer>\n  area(declared()) + area(inferred())\nend;"
    ));
    let dispatches = checks_of(&module, &artifact, "main", TypingCheck::Dispatch);
    assert_eq!(type_name(&module, &dispatches[0].actual[0]), "<shape>");
    assert_eq!(dispatches[0].verdict, TypingVerdict::Checked);
    assert!(chain(&module, &artifact, &dispatches[0], 0).starts_with("contract(declared)@"));
    assert_eq!(type_name(&module, &dispatches[1].actual[0]), "<circle>");
    assert!(chain(&module, &artifact, &dispatches[1], 0).starts_with("contract(inferred)@"));
}

#[test]
fn unrefined_expressions_have_their_sem_category() {
    let (module, artifact) = decisions_for(&format!(
        "{WHOLE}define class <disc> (<object>)\n  slot radius :: <i32>, init-value: 3i32;\nend;\n\
         define function main() => <integer>\n  whole(make(<disc>).radius)\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "main", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(type_name(&module, &dispatch.actual[0]), "<i32>");
    assert!(chain(&module, &artifact, &dispatch, 0).starts_with("contract(declared)@"));
}

#[test]
fn captures_carry_and_narrow_the_facts_of_their_sources() {
    let (module, artifact) = decisions_for(&format!(
        "{WHOLE}define function outer(x :: <object>) => <integer>\n  \
         if (instance?(x, <integer>))\n    block ()\n      \
         let f :: <callable> = function () => <integer>\n        whole(x)\n      end;\n      0\n    \
         end\n  else 0 end\nend;\n\
         define function inner(x :: <object>) => <integer>\n  \
         let f :: <callable> = function () => <integer>\n    \
         if (instance?(x, <integer>)) whole(x) else 0 end\n  end;\n  0\nend;"
    ));
    for name in ["outer", "inner"] {
        let [dispatch] = checks_of(&module, &artifact, name, TypingCheck::Dispatch)
            .try_into()
            .expect("one dispatch");
        assert_eq!(dispatch.verdict, TypingVerdict::Proved, "{name}");
        assert_eq!(type_name(&module, &dispatch.actual[0]), "<integer>");
    }
}

#[test]
fn range_checks_classify_conversions() {
    let (module, artifact) = decisions_for(
        "define function main(n :: <integer>, x :: <double-float>, w :: <i32>) => <integer>\n  \
         let narrow = as(<i32>, n);\n  let float = as(<integer>, x);\n  \
         let wide = as(<integer>, w);\n  let negative = -1;\n  as(<u32>, negative)\nend;",
    );
    let rules = checks_of(&module, &artifact, "main", TypingCheck::Range)
        .iter()
        .map(|decision| (decision.verdict, decision.rule))
        .collect::<Vec<_>>();
    assert_eq!(
        rules,
        [
            (TypingVerdict::Checked, TypingRule::RangeOverlaps),
            (TypingVerdict::Checked, TypingRule::FloatToInteger),
            (TypingVerdict::Proved, TypingRule::CannotTrap),
            (TypingVerdict::Refuted, TypingRule::RangeDisjoint),
        ]
    );
    let codes = artifact
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, [DiagnosticCode::TypingRangeAlwaysOutside]);
}

#[test]
fn shifts_check_their_count_and_bit_operations_cannot_trap() {
    let (module, artifact) = decisions_for(
        "define function main(x :: <u32>, n :: <u32>) => <u32>\n  \
         let a = shift-left(x, n);\n  let b = shift-left(x, 3u32);\n  \
         let c = x +% 1u32;\n  logand(a, logior(b, c))\nend;",
    );
    let rules = checks_of(&module, &artifact, "main", TypingCheck::Overflow)
        .iter()
        .map(|decision| (decision.verdict, decision.rule))
        .collect::<Vec<_>>();
    assert_eq!(
        rules,
        [
            (TypingVerdict::Checked, TypingRule::ShiftCountUnchecked),
            (TypingVerdict::Proved, TypingRule::ShiftCountInRange),
            (TypingVerdict::Proved, TypingRule::CannotTrap),
            (TypingVerdict::Proved, TypingRule::CannotTrap),
            (TypingVerdict::Proved, TypingRule::CannotTrap),
        ]
    );
}

#[test]
fn refutations_label_the_sources_of_their_operands() {
    let source = format!(
        "{SHAPES}define function main() => <integer>\n  \
         let s = make(<shape>);\n  area(s)\nend;"
    );
    let (module, artifact) = decisions_for(&source);
    let [diagnostic] = artifact.diagnostics.as_slice() else {
        panic!("{:?}", artifact.diagnostics);
    };
    let make = source.find("make(<shape>)").expect("make");
    let secondary = diagnostic
        .labels
        .iter()
        .filter(|label| label.style == crate::diagnostic::DiagnosticLabelStyle::Secondary)
        .collect::<Vec<_>>();
    assert_eq!(secondary.len(), 1, "{secondary:?}");
    assert_eq!(secondary[0].span.start, make);
    assert_eq!(
        secondary[0].message.as_deref(),
        Some("constructed as exactly <shape> here")
    );
    assert!(artifact.dump(&module).contains("because=constructed@"));
}

#[test]
fn dump_renders_reason_chains_and_assumptions() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function measure(s :: <shape>) => <integer>\n  \
         if (instance?(s, <circle>)) area(s) else 0 end\nend;"
    ));
    let dump = artifact.dump(&module);
    assert!(
        dump.contains("s1 dispatch proved rule=dispatch-covered span=",),
        "{dump}"
    );
    assert!(dump.contains("because=narrowed(s0,then)@"), "{dump}");
    assert!(dump.contains("<circle> <- declared@"), "{dump}");
    assert!(
        dump.contains(
            " assumes=closed-world(generic area has exactly methods {area(<circle>), area(<square>)})\n"
        ),
        "{dump}"
    );
}

#[test]
fn unreachable_checks_are_never_reported() {
    let config = TypingConfig::soft().with_policy(
        TypingCheck::Dispatch,
        TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Warning)
            .with_proved(TypingSeverity::Warning),
    );
    // The literal condition takes only its else branch, so the dispatch in
    // the then branch is never reached.
    let module = module(&format!(
        "{SHAPES}define function main() => <integer>\n  \
         if (#f) area(make(<circle>)) else 0 end\nend;"
    ));
    assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
    let artifact = analyze(&module, config);
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions[0].rule, TypingRule::Unreachable);
    assert_eq!(artifact.severity(&decisions[0]), TypingSeverity::Silent);
    assert_eq!(artifact.diagnostics, Vec::new());
}

#[test]
fn every_check_and_verdict_has_a_diagnostic_code() {
    let mut codes = Vec::new();
    for check in TypingCheck::ALL {
        for verdict in TypingVerdict::ALL {
            let code = diagnostic_code(*check, *verdict);
            assert!(code.as_str().starts_with("typing."));
            assert!(!codes.contains(&code));
            codes.push(code);
        }
    }
}

#[test]
fn loop_heads_widen_to_a_fixpoint() {
    let module = module("define function main() => <integer>\n  0\nend;");
    let range = |min, max| range_type(&module, IntRange { min, max });
    // `let i = 0; loop i := i + 1`: the head widens through the `<i32>`,
    // `<u32>`, and 64-bit thresholds before the increment may overflow and
    // the head covers every `<integer>`.
    let mut steps = 0;
    let head = widen_to_fixpoint(&module, vec![range(Some(0), Some(0))], |head| {
        steps += 1;
        let next = match head[0].atoms() {
            Some(
                [
                    Atom::Integer(IntRange {
                        min,
                        max: Some(max),
                    }),
                ],
            ) => match max.checked_add(1) {
                Some(max) => IntRange {
                    min: min.map(|min| min.saturating_add(1)),
                    max: Some(max),
                },
                None => IntRange::INTEGER,
            },
            _ => IntRange::INTEGER,
        };
        vec![range_type(&module, next)]
    });
    assert_eq!(head, vec![range(None, None)]);
    assert!(steps <= 6, "{steps}");
}

#[test]
fn strict_preset_is_soft_reporting_with_every_definition_strict() {
    let strict = TypingConfig::strict();
    for check in TypingCheck::ALL {
        assert_eq!(strict.policy(*check), TypingConfig::soft().policy(*check));
    }
    assert_eq!(strict.strict.scope, StrictScope::All);
    assert!(!strict.strict.overflow);
    assert_eq!(TypingConfig::soft().strict, StrictPolicy::DECLARED);
    assert_eq!(TypingConfig::silent().strict, StrictPolicy::DECLARED);
    assert_eq!(
        TypingConfig::soft().severity(TypingCheck::Instance, TypingVerdict::Proved),
        TypingSeverity::Warning
    );
}

/// The definition of function `name`.
fn function_definition(module: &sem::Module, name: &str) -> DefinitionId {
    module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function")
        .definition
}

/// The local of function `name` called `local`.
fn local_of(module: &sem::Module, name: &str, local: &str) -> Binding {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function");
    Binding::Local(
        function
            .locals
            .iter()
            .find(|candidate| candidate.name == local)
            .expect("local")
            .id,
    )
}

/// The span of the `occurrence`th match of `needle` in `source`.
fn span_of(source: &str, needle: &str, occurrence: usize) -> Span {
    let start = source
        .match_indices(needle)
        .nth(occurrence)
        .expect("needle")
        .0;
    Span::new(start, start + needle.len())
}

#[test]
fn type_of_a_narrowed_local_lists_its_narrowed_uses() {
    let source = format!(
        "{WHOLE}define function f(x :: <object>) => <integer>\n  \
         if (instance?(x, <integer>)) whole(x) else 0 end\nend;"
    );
    let (module, artifact) = decisions_for(&source);
    let f = function_definition(&module, "f");
    let report = artifact
        .type_of(
            &module,
            &TypeSelector::Binding {
                definition: f,
                binding: local_of(&module, "f", "x"),
            },
        )
        .expect("binding report");
    assert_eq!(report.definition, f);
    assert_eq!(type_name(&module, &report.ty), "<object>");
    assert_eq!(report.declared, report.ty);
    assert!(report.reason_chain(&module).starts_with("declared@"));
    // Only the read in the then branch is narrower than the binding.
    let [narrowed] = report.uses.as_slice() else {
        panic!("one narrowed use: {:?}", report.uses);
    };
    let call = span_of(&source, "whole(x)", 1);
    assert_eq!(narrowed.span, Span::new(call.start + 6, call.start + 7));
    assert_eq!(type_name(&module, &narrowed.ty), "<integer>");
    assert!(
        report
            .chain(&module, narrowed.reason)
            .starts_with("narrowed(s0,then)@"),
        "{}",
        report.chain(&module, narrowed.reason)
    );

    // The same read, selected by its span, is an expression of that type.
    // Sem refines the local there too, so the read is an unboxed `i64`.
    let at_use = artifact
        .type_of(&module, &TypeSelector::Span(narrowed.span))
        .expect("span report");
    assert_eq!(at_use.subject, TypeSubject::Expression { site: None });
    assert_eq!(at_use.ty, narrowed.ty);
    assert_eq!(at_use.rep, sem::ValueKind::I64);
    assert_eq!(type_name(&module, &at_use.declared), "<integer>");
}

#[test]
fn type_of_a_handler_condition_is_the_handler_class() {
    let source = "define class <problem> (<object>)\nend;\n\
         define class <bad-radius> (<problem>)\nend;\n\
         define generic describe(p);\n\
         define method describe(p :: <problem>) => <integer>\n  7\nend;\n\
         define function main() => <integer>\n  \
         handle signal(make(<bad-radius>)) with restart e :: <problem> describe(e) end\nend;";
    let (module, artifact) = decisions_for(source);
    let main = function_definition(&module, "main");
    let report = artifact
        .type_of(
            &module,
            &TypeSelector::Binding {
                definition: main,
                binding: local_of(&module, "main", "e"),
            },
        )
        .expect("binding report");
    assert_eq!(type_name(&module, &report.ty), "<problem>");
    assert!(
        report
            .reason_chain(&module)
            .starts_with("handler-condition(<problem>)@")
    );
    // Selecting the binding's name finds the binding, not an expression.
    let name = span_of(source, "e ::", 0);
    let by_name = artifact
        .type_of(
            &module,
            &TypeSelector::Span(Span::new(name.start, name.start + 1)),
        )
        .expect("span report");
    assert_eq!(by_name.subject, report.subject);
}

#[test]
fn type_of_a_block_is_the_join_of_its_exits() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function choose(n :: <integer>) => <integer>\n  \
         let s :: <shape> = make(<square>);\n  \
         let shape :: <shape> = block (return)\n    \
         let c :: <shape> = make(<circle>);\n    \
         let picked = if (n > 10) return(c) else s end;\n    picked\n  end;\n  \
         area(shape)\nend;"
    ));
    let choose = function_definition(&module, "choose");
    let report = artifact
        .type_of(
            &module,
            &TypeSelector::Binding {
                definition: choose,
                binding: local_of(&module, "choose", "shape"),
            },
        )
        .expect("binding report");
    assert_eq!(
        type_name(&module, &report.ty),
        "type-union(exactly <circle>, exactly <square>)"
    );
    assert_eq!(type_name(&module, &report.declared), "<shape>");
    let chain = report.reason_chain(&module);
    assert!(chain.starts_with("joined@"), "{chain}");
    assert!(chain.contains("constructed@"), "{chain}");
    // The report owns a re-indexed provenance graph rooted at its reason.
    assert_eq!(report.reason, ReasonId(0));
    assert!(report.reasons.iter().all(|reason| {
        reason
            .from
            .iter()
            .all(|from| (from.0 as usize) < report.reasons.len())
    }));

    // The call's site reports the generic's result contract.
    let call = artifact
        .type_of(
            &module,
            &TypeSelector::Site {
                definition: choose,
                site: SiteId(0),
            },
        )
        .expect("site report");
    assert_eq!(
        call.subject,
        TypeSubject::Expression {
            site: Some(SiteId(0))
        }
    );
    assert!(
        call.reason_chain(&module)
            .starts_with("contract(inferred)@")
    );
}

#[test]
fn type_of_a_capture_is_its_source_fact_where_the_closure_is_made() {
    let source = format!(
        "{WHOLE}define function outer(x :: <object>) => <integer>\n  \
         if (instance?(x, <integer>))\n    block ()\n      \
         let f :: <callable> = function () => <integer>\n        whole(x)\n      end;\n      0\n    \
         end\n  else 0 end\nend;"
    );
    let (module, artifact) = decisions_for(&source);
    let outer = function_definition(&module, "outer");
    let closure = module.closures[0].id;
    let report = artifact
        .type_of(
            &module,
            &TypeSelector::Binding {
                definition: outer,
                binding: Binding::Capture {
                    closure,
                    capture: module.closures[0].captures[0].id,
                },
            },
        )
        .expect("capture report");
    assert_eq!(type_name(&module, &report.ty), "<integer>");
    assert!(
        report
            .reason_chain(&module)
            .starts_with("narrowed(s0,then)@")
    );
    let dump = artifact.dump_definition_types(&module, outer);
    assert!(dump.contains("closure#0 capture x@"), "{dump}");
    assert!(dump.contains("param x@"), "{dump}");
    assert!(dump.contains("local f@"), "{dump}");
}

#[test]
fn type_queries_explain_what_they_cannot_answer() {
    let source = format!("{SHAPES}define function main()\n  area(make(<circle>))\nend;");
    let (module, artifact) = decisions_for(&source);
    let main = function_definition(&module, "main");
    assert_eq!(
        artifact.type_of(
            &module,
            &TypeSelector::Site {
                definition: main,
                site: SiteId(9)
            }
        ),
        Err(TypeQueryError::UnknownSite(SiteId(9)))
    );
    assert_eq!(
        artifact.type_of(
            &module,
            &TypeSelector::Binding {
                definition: main,
                binding: Binding::Local(LocalId(4)),
            }
        ),
        Err(TypeQueryError::UnknownBinding(Binding::Local(LocalId(4))))
    );
    let outside = Span::new(0, 3);
    assert_eq!(
        artifact.type_of(&module, &TypeSelector::Span(outside)),
        Err(TypeQueryError::NothingAtSpan(outside))
    );
    let blocked = module_with_errors();
    let artifact = analyze(&blocked, TypingConfig::soft());
    assert!(matches!(
        artifact.type_of(&blocked, &TypeSelector::Span(Span::new(0, 1))),
        Err(TypeQueryError::NotChecked(_))
    ));
}

fn module_with_errors() -> sem::Module {
    let module = module("define function main()\n  missing\nend;");
    assert!(!module.diagnostics.is_empty());
    module
}

#[test]
fn decisions_depend_on_cited_contracts_and_assumptions() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function pick() => <shape>\n  make(<circle>)\nend;\n\
         define function main()\n  area(pick())\nend;"
    ));
    let pick = function_definition(&module, "pick");
    let main = typed(&module, &artifact, "main");
    let [dispatch] = main.decisions.as_slice() else {
        panic!("one decision");
    };
    let dependencies = main.dependencies(dispatch);
    assert_eq!(
        dependencies,
        [
            TypingDependency::Contract(pick),
            TypingDependency::Methods(module.generics[0].id),
        ]
    );
    let dependents = artifact.contract_dependents(pick);
    assert_eq!(dependents, [dispatch]);
    assert!(
        artifact
            .contract_dependents(function_definition(&module, "main"))
            .is_empty()
    );
}

#[test]
fn dump_types_lists_bindings_uses_and_results() {
    let (module, artifact) = decisions_for(&format!(
        "{WHOLE}define function f(x :: <object>) => <integer>\n  \
         let y :: <integer> = 5;\n  \
         if (instance?(x, <integer>)) whole(x) else y end\nend;"
    ));
    let dump = artifact.dump_types(&module);
    assert!(
        dump.starts_with("types status=checked definitions="),
        "{dump}"
    );
    assert!(dump.contains("narrowed-uses=1"), "{dump}");
    assert!(dump.contains("    param x@"), "{dump}");
    assert!(
        dump.contains(" singleton(5) declared=<integer> because=literal@"),
        "{dump}"
    );
    assert!(dump.contains("      use @"), "{dump}");
    assert!(dump.contains("    result "), "{dump}");
}

/// [`SHAPES`] with `<shape>` sealed and abstract and its two subclasses
/// sealed, so `<shape>` is exactly a `<circle>` or a `<square>`.
const SEALED_SHAPES: &str = "define sealed abstract class <shape> (<object>)\nend;\n\
     define sealed class <circle> (<shape>)\nend;\n\
     define sealed class <square> (<shape>)\nend;\n\
     define class <label> (<object>)\nend;\n\
     define sealed generic area(x);\n\
     define method area(c :: <circle>)\n  1\nend;\n\
     define method area(s :: <square>)\n  2\nend;\n";

/// The decisions of the function named `name`, with its typing.
fn function_typing<'a>(
    module: &sem::Module,
    artifact: &'a TypingArtifact,
    name: &str,
) -> &'a DefinitionTyping {
    let function = module
        .functions
        .iter()
        .find(|function| function.name == name)
        .expect("function");
    artifact
        .definition(function.definition)
        .expect("typed function")
}

/// The assumptions `decision` lists, as their labels.
fn assumption_labels(
    module: &sem::Module,
    typed: &DefinitionTyping,
    decision: &TypingDecision,
) -> Vec<String> {
    decision
        .assumes
        .iter()
        .filter_map(|id| match typed.reason(*id)?.kind {
            ReasonKind::ClosedWorld(assumption) => Some(assumption_label(module, assumption)),
            _ => None,
        })
        .collect()
}

#[test]
fn dispatch_over_a_sealed_cone_is_covered_by_its_pieces() {
    let measure = "define function measure(s :: <shape>)\n  area(s)\nend;";
    // Open, a `<shape>` may be a subclass `area` has no method for.
    let (module, artifact) = decisions_for(&format!("{SHAPES}{measure}"));
    let typed = function_typing(&module, &artifact, "measure");
    assert_eq!(typed.decisions[0].verdict, TypingVerdict::Checked);

    let (module, artifact) = decisions_for(&format!("{SEALED_SHAPES}{measure}"));
    let typed = function_typing(&module, &artifact, "measure");
    let decision = &typed.decisions[0];
    assert_eq!(
        (decision.verdict, decision.rule),
        (TypingVerdict::Proved, TypingRule::DispatchCovered)
    );
    // Two methods apply, so none is selected.
    assert_eq!(decision.selected_method, None);
    assert_eq!(
        assumption_labels(&module, typed, decision),
        [
            "sealed generic area has exactly methods {area(<circle>), area(<square>)}",
            "sealed abstract class <shape> has exactly direct subclasses {<circle>, <square>}",
            "sealed class <circle> has no subclasses",
            "sealed class <square> has no subclasses",
        ]
    );
    let shape = module.classes[1].id;
    assert!(
        typed
            .dependencies(decision)
            .contains(&TypingDependency::Sealed(shape))
    );
    assert_eq!(
        typed.inputs().sealed,
        [shape, module.classes[2].id, module.classes[3].id]
    );
}

#[test]
fn a_send_whose_sealed_pieces_select_one_method_selects_it() {
    let source = "define sealed abstract class <shape> (<object>)\nend;\n\
         define sealed class <circle> (<shape>)\nend;\n\
         define sealed class <square> (<shape>)\nend;\n\
         define generic describe(s);\n\
         define method describe(s :: <shape>)\n  1\nend;\n\
         define function measure(s :: <shape>)\n  describe(s)\nend;";
    let (module, artifact) = decisions_for(source);
    let typed = function_typing(&module, &artifact, "measure");
    let decision = &typed.decisions[0];
    assert_eq!(decision.verdict, TypingVerdict::Proved);
    assert_eq!(decision.selected_method, Some(module.methods[0].id));
    // Covering needs no sealing; selecting does, including that each leaf
    // piece has no subclasses of its own.
    assert_eq!(
        assumption_labels(&module, typed, decision),
        [
            "generic describe has exactly methods {describe(<shape>)}",
            "sealed abstract class <shape> has exactly direct subclasses {<circle>, <square>}",
            "sealed class <square> has no subclasses",
            "sealed class <circle> has no subclasses",
        ]
    );
    // Without sealing the subclasses, a piece is a cone and no method is
    // selected.
    let (module, artifact) =
        decisions_for(&source.replace("define sealed class <circle>", "define class <circle>"));
    let typed = function_typing(&module, &artifact, "measure");
    assert_eq!(typed.decisions[0].verdict, TypingVerdict::Proved);
    assert_eq!(typed.decisions[0].selected_method, None);
}

#[test]
fn a_sealed_leaf_is_exact_so_its_sends_select_their_method() {
    let source = format!("{SEALED_SHAPES}define function measure(c :: <circle>)\n  area(c)\nend;");
    let (module, artifact) = decisions_for(&source);
    let typed = function_typing(&module, &artifact, "measure");
    let decision = &typed.decisions[0];
    assert_eq!(decision.selected_method, Some(module.methods[0].id));
    assert_eq!(type_name(&module, &decision.actual[0]), "<circle>");
    assert!(
        assumption_labels(&module, typed, decision)
            .contains(&"sealed class <circle> has no subclasses".into())
    );
}

#[test]
fn an_instance_test_narrows_a_sealed_cone_to_its_other_pieces() {
    let source = format!(
        "{SEALED_SHAPES}define function side(s :: <shape>)\n  \
         if (instance?(s, <circle>)) 0 \
         else if (instance?(s, <square>)) 1 else area(s) end end\nend;"
    );
    let (module, artifact) = decisions_for(&source);
    let typed = function_typing(&module, &artifact, "side");
    let rules = typed
        .decisions
        .iter()
        .map(|decision| decision.rule)
        .collect::<Vec<_>>();
    // The second test is always true where it runs, and the send after it
    // is unreachable.
    assert_eq!(
        rules,
        [
            TypingRule::InstanceSometimes,
            TypingRule::InstanceAlways,
            TypingRule::Unreachable
        ]
    );
    let shape_sealing =
        "sealed abstract class <shape> has exactly direct subclasses {<circle>, <square>}";
    assert!(assumption_labels(&module, typed, &typed.decisions[1]).contains(&shape_sealing.into()));
    assert!(
        typed
            .reason_chain(&module, typed.decisions[1].because[0])
            .starts_with("narrowed(s0,else)@")
    );
    // Open, the else branch keeps the whole cone.
    let (module, artifact) = decisions_for(&source.replace("define sealed abstract", "define"));
    let typed = function_typing(&module, &artifact, "side");
    assert_eq!(typed.decisions[1].rule, TypingRule::InstanceSometimes);
}

#[test]
fn a_cast_against_a_sealed_hierarchy_is_proved_or_refuted_by_its_pieces() {
    let source = format!(
        "{SEALED_SHAPES}define function circle(s :: <shape>)\n  \
         if (instance?(s, <square>)) 0 else area(as(<circle>, s)) end\nend;\n\
         define function square(s :: <shape>)\n  \
         if (instance?(s, <square>)) 0 else area(as(<square>, s)) end\nend;"
    );
    let (module, artifact) = decisions_for(&source);
    let cast = |name: &str| {
        let typed = function_typing(&module, &artifact, name);
        let decision = typed
            .decisions
            .iter()
            .find(|decision| decision.check == TypingCheck::Cast)
            .expect("cast")
            .clone();
        (decision.rule, assumption_labels(&module, typed, &decision))
    };
    let shape_sealing = String::from(
        "sealed abstract class <shape> has exactly direct subclasses {<circle>, <square>}",
    );
    // Past the test, a `<shape>` can only be a `<circle>`.
    let (rule, assumes) = cast("circle");
    assert_eq!(rule, TypingRule::CastUpcast);
    assert!(assumes.contains(&shape_sealing));
    let (rule, assumes) = cast("square");
    assert_eq!(rule, TypingRule::CastDisjoint);
    assert!(assumes.contains(&shape_sealing));
}

#[test]
fn a_sealed_hierarchy_too_large_to_distribute_is_left_to_the_runtime() {
    let mut source = String::from("define sealed abstract class <shape> (<object>)\nend;\n");
    let mut methods = String::new();
    for index in 0..=MAX_DISPATCH_TUPLES {
        let _ = write!(source, "define sealed class <s{index}> (<shape>)\nend;\n");
        let _ = write!(
            methods,
            "define method area(s :: <s{index}>)\n  {index}\nend;\n"
        );
    }
    source.push_str("define generic area(s);\n");
    source.push_str(&methods);
    source.push_str("define function measure(s :: <shape>)\n  area(s)\nend;");
    let (module, artifact) = decisions_for(&source);
    let typed = function_typing(&module, &artifact, "measure");
    let decision = &typed.decisions[0];
    assert_eq!(decision.verdict, TypingVerdict::Checked);
    assert!(
        assumption_labels(&module, typed, decision)
            .iter()
            .all(|label| !label.starts_with("sealed abstract class"))
    );
}

#[test]
fn decisions_record_who_asked_for_each_check() {
    let (module, artifact) = decisions_for(
        "define class <shape> (<object>)\nend;\n\
         define class <circle> (<shape>)\nend;\n\
         define function small(n :: limited(<integer>, min: 0, max: 9)) => <integer>\n  n\nend;\n\
         define function keep(s :: <circle>) => <circle>\n  s\nend;\n\
         define function main(s :: <shape>, n :: <integer>, f :: <callable>) => <integer>\n  \
           let c = as(<circle>, s);\n  \
           let k :: <circle> = f(c);\n  \
           let g = keep;\n  \
           if (instance?(s, <circle>)) small(n)\n  \
           else small(as(limited(<integer>, min: 0, max: 9), n)) end\nend;",
    );
    let origins = |name: &str| {
        artifact
            .definition(function_definition(&module, name))
            .expect("typed")
            .decisions
            .iter()
            .map(|decision| (decision.check, decision.origin))
            .collect::<Vec<_>>()
    };
    // `keep` is used as a value, so its indirect entry checks `s`.
    assert_eq!(origins("keep"), [(TypingCheck::Cast, CheckOrigin::Entry)]);
    assert_eq!(
        origins("main"),
        [
            // `as(<circle>, s)`.
            (TypingCheck::Cast, CheckOrigin::Explicit),
            // The `<callable>`'s result reaching `k :: <circle>`.
            (TypingCheck::Cast, CheckOrigin::Implicit),
            (TypingCheck::Instance, CheckOrigin::Test),
            // `n` reaching `small`'s limited parameter.
            (TypingCheck::Range, CheckOrigin::Implicit),
            // `as(limited(...), n)`, then its value reaching the parameter.
            (TypingCheck::Range, CheckOrigin::Explicit),
            (TypingCheck::Range, CheckOrigin::Implicit),
        ]
    );
    assert!(
        artifact
            .dump(&module)
            .contains("severity=silent origin=explicit")
    );
}

/// The strict errors of `artifact`, as (code, definition label) pairs.
fn strict_errors(artifact: &TypingArtifact) -> Vec<(DiagnosticCode, String)> {
    artifact
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| {
            let definition = diagnostic
                .args
                .iter()
                .find(|arg| arg.name == "definition")
                .map(|arg| arg.value.render())
                .unwrap_or_default();
            (diagnostic.code, definition)
        })
        .collect()
}

fn arg<'a>(diagnostic: &'a Diagnostic, name: &str) -> &'a str {
    diagnostic
        .args
        .iter()
        .find(|arg| arg.name == name)
        .map_or("", |arg| match &arg.value {
            crate::diagnostic::DiagnosticArgValue::Text(text)
            | crate::diagnostic::DiagnosticArgValue::Id(text) => text.as_str(),
            _ => "",
        })
}

#[test]
fn a_strict_function_whose_checks_are_proved_has_no_errors() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define strict function round(c :: <circle>)\n  area(c)\nend;\n\
         define function main()\n  round(make(<circle>))\nend;"
    ));
    let round = function_definition(&module, "round");
    assert_eq!(artifact.strictness(round), Some(StrictSource::Declared));
    assert_eq!(
        artifact.strictness(function_definition(&module, "main")),
        None
    );
    assert!(
        artifact.diagnostics.is_empty(),
        "{:?}",
        artifact.diagnostics
    );
    assert_eq!(artifact.strict_error_count(), 0);
    assert!(
        artifact
            .dump(&module)
            .contains("strict scope=declared overflow=runtime definitions=1 errors=0")
    );
}

#[test]
fn strict_reports_each_unproved_check_with_its_code_reasons_and_fixes() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function small(n :: limited(<integer>, min: 0, max: 9)) => <integer>\n  \
         n\nend;\n\
         define strict function measure(s :: <shape>, n :: <integer>, f :: <callable>)\n  \
         let k :: <circle> = f(s);\n  \
         small(n) + area(s)\nend;\n\
         define function main()\n  0\nend;"
    ));
    let measure = function_definition(&module, "measure");
    let codes = strict_errors(&artifact);
    assert_eq!(
        codes,
        [
            (
                DiagnosticCode::TypingUnprovedCast,
                "function measure".into()
            ),
            (
                DiagnosticCode::TypingUnprovedRange,
                "function measure".into()
            ),
            (
                DiagnosticCode::TypingUnprovedDispatch,
                "function measure".into()
            ),
        ]
    );
    // Strict errors are errors whatever the soft policy says.
    assert_eq!(
        strict_errors(&artifact.with_config(&module, TypingConfig::silent())),
        codes
    );

    let typed = artifact.definition(measure).expect("typed");
    let range = artifact
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::TypingUnprovedRange)
        .expect("range error");
    let decision = typed
        .decisions
        .iter()
        .find(|decision| decision.check == TypingCheck::Range)
        .expect("range decision");
    assert_eq!(arg(range, "decision"), decision_id(decision));
    assert_eq!(arg(range, "strict"), "declared");
    assert_eq!(arg(range, "actual"), "<integer>");
    assert_eq!(arg(range, "expected"), "limited(<integer>, min: 0, max: 9)");
    assert_eq!(artifact.severity(decision), TypingSeverity::Error);
    assert_eq!(
        artifact.strict_violation(decision),
        Some(StrictViolation::Unproved)
    );
    let notes = range
        .notes
        .iter()
        .map(|note| note.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        notes
            .iter()
            .any(|note| note.starts_with("because declared@")),
        "{notes:?}"
    );
    assert!(
        notes.contains(&"fix: annotate parameter n as limited(<integer>, min: 0, max: 9)"),
        "{notes:?}"
    );
    assert!(
        notes.contains(
            &"fix: write as(limited(<integer>, min: 0, max: 9), ...) to check deliberately"
        ),
        "{notes:?}"
    );

    let dispatch = artifact
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::TypingUnprovedDispatch)
        .expect("dispatch error");
    assert_eq!(arg(dispatch, "generic"), "area");
    assert_eq!(arg(dispatch, "arguments"), "<shape>");
    let notes = dispatch
        .notes
        .iter()
        .map(|note| note.message.as_str())
        .collect::<Vec<_>>();
    assert!(
        notes.contains(&"fix: test s with instance? against <circle> or <square> first"),
        "{notes:?}"
    );
    assert!(
        notes.contains(&"fix: declare <shape> sealed abstract"),
        "{notes:?}"
    );
    // The secondary labels point at where the operand types came from.
    assert!(dispatch.labels.len() > 1, "{:?}", dispatch.labels);
}

#[test]
fn explicit_casts_tests_and_indirect_entries_are_not_strict_obligations() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define strict function keep(s :: <circle>) => <circle>\n  s\nend;\n\
         define strict function main(s :: <shape>, n :: <integer>)\n  \
         let g = keep;\n  \
         let c = as(<circle>, s);\n  \
         let m = as(limited(<integer>, min: 0), n);\n  \
         if (instance?(s, <square>)) 1 else 2 end\nend;"
    ));
    let origins = artifact
        .decisions()
        .filter(|decision| decision.verdict == TypingVerdict::Checked)
        .map(|decision| decision.origin)
        .collect::<Vec<_>>();
    assert!(origins.contains(&CheckOrigin::Entry), "{origins:?}");
    assert!(origins.contains(&CheckOrigin::Explicit), "{origins:?}");
    assert!(origins.contains(&CheckOrigin::Test), "{origins:?}");
    assert!(
        artifact.diagnostics.is_empty(),
        "{:?}",
        artifact.diagnostics
    );
    assert_eq!(artifact.strict.len(), 2);
    let _ = module;
}

#[test]
fn checks_that_always_fail_are_errors_in_strict_code_and_warnings_in_soft_code() {
    let source = |adjective: &str| {
        format!(
            "{SHAPES}define {adjective}function main()\n  \
             as(<circle>, as(<object>, make(<label>)))\nend;"
        )
    };
    let (_, soft) = decisions_for(&source(""));
    assert_eq!(soft.diagnostics.len(), 1);
    assert_eq!(
        soft.diagnostics[0].code,
        DiagnosticCode::TypingCastAlwaysFails
    );
    assert_eq!(soft.diagnostics[0].severity, Severity::Warning);

    let (_, strict) = decisions_for(&source("strict "));
    assert_eq!(
        strict_errors(&strict),
        [(
            DiagnosticCode::TypingCastAlwaysFails,
            "function main".into()
        )]
    );
    let decision = strict
        .decisions()
        .find(|decision| decision.verdict == TypingVerdict::Refuted)
        .expect("cast");
    assert_eq!(decision.origin, CheckOrigin::Explicit);
    assert_eq!(
        strict.strict_violation(decision),
        Some(StrictViolation::Refuted)
    );
}

#[test]
fn a_strict_generic_holds_each_of_its_methods_to_strict_typing() {
    let (module, artifact) = decisions_for(
        "define class <shape> (<object>)\nend;\n\
         define class <circle> (<shape>)\nend;\n\
         define generic describe(x);\n\
         define method describe(c :: <circle>)\n  1\nend;\n\
         define strict generic measure(s :: <shape>);\n\
         define method measure(s :: <shape>)\n  describe(s)\nend;\n\
         define generic loose(s :: <shape>);\n\
         define method loose(s :: <shape>)\n  describe(s)\nend;\n\
         define function main()\n  0\nend;",
    );
    let method_of = |name: &str| {
        let generic = module
            .generics
            .iter()
            .find(|generic| generic.name == name)
            .expect("generic")
            .id;
        module
            .methods
            .iter()
            .find(|method| method.generic == generic)
            .expect("method")
            .definition
    };
    assert_eq!(
        artifact.strictness(method_of("measure")),
        Some(StrictSource::Generic(
            module
                .generics
                .iter()
                .find(|generic| generic.name == "measure")
                .expect("generic")
                .id
        ))
    );
    assert_eq!(artifact.strictness(method_of("loose")), None);
    assert_eq!(
        strict_errors(&artifact),
        [(
            DiagnosticCode::TypingUnprovedDispatch,
            "method measure(<shape>)".into()
        )]
    );
    assert_eq!(
        arg(&artifact.diagnostics[0], "strict"),
        "generic measure is strict"
    );
    assert!(
        artifact
            .dump(&module)
            .contains("method measure(<shape>) strict=generic(measure)")
    );
}

#[test]
fn the_strict_preset_holds_every_definition_and_overflow_is_opt_in() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function measure(s :: <shape>, n :: <integer>)\n  \
         area(s) + n\nend;\n\
         define function main()\n  measure(make(<circle>), 1)\nend;"
    ));
    assert!(artifact.diagnostics.is_empty());
    assert!(artifact.strict.is_empty());

    let strict = artifact.with_config(&module, TypingConfig::strict());
    assert_eq!(strict.definitions, artifact.definitions);
    assert_eq!(
        strict.strictness(function_definition(&module, "main")),
        Some(StrictSource::Policy)
    );
    // `area` returns an unannotated `<object>`, so `+` dispatches too.
    let codes = strict_errors(&strict)
        .into_iter()
        .map(|(code, _)| code)
        .collect::<Vec<_>>();
    assert!(codes.contains(&DiagnosticCode::TypingUnprovedDispatch));
    assert!(!codes.contains(&DiagnosticCode::TypingUnprovedOverflow));

    let (module, artifact) = decisions_for(
        "define function add(a :: <integer>, b :: <integer>) => <integer>\n  a + b\nend;\n\
         define function main()\n  add(1, 2)\nend;",
    );
    let strict = artifact.with_config(&module, TypingConfig::strict());
    assert!(strict.diagnostics.is_empty(), "{:?}", strict.diagnostics);
    let overflow = TypingConfig::strict()
        .apply_spec("strict.overflow=proved")
        .expect("valid");
    let strict = artifact.with_config(&module, overflow);
    assert_eq!(
        strict_errors(&strict),
        [(
            DiagnosticCode::TypingUnprovedOverflow,
            "function add".into()
        )]
    );
    assert!(
        strict
            .dump(&module)
            .contains("strict scope=all overflow=proved definitions=2 errors=1")
    );
}

#[test]
fn strict_settings_parse_and_compose_with_presets() {
    let config = TypingConfig::soft()
        .apply_spec("strict.scope=all strict.overflow=proved")
        .expect("valid");
    assert_eq!(
        config.strict,
        StrictPolicy {
            scope: StrictScope::All,
            overflow: true
        }
    );
    assert_eq!(config_label(config), "custom");
    assert_eq!(
        TypingConfig::soft().apply_spec("strict.scope=all"),
        Ok(TypingConfig::strict())
    );
    assert_eq!(
        TypingConfig::strict().apply_spec("soft"),
        Ok(TypingConfig::soft())
    );
    assert_eq!(
        config.strict.label(),
        "strict.scope=all strict.overflow=proved"
    );
    for bad in [
        "strict.scope=some",
        "strict.overflow=yes",
        "strict.depth=1",
        "strict.",
    ] {
        assert_eq!(
            TypingConfig::soft().apply_spec(bad),
            Err(TypingConfigError::InvalidWord(bad.into())),
            "{bad}"
        );
    }
}

#[test]
fn sealing_makes_strict_dispatch_over_a_hierarchy_provable() {
    let source = |adjectives: &str| {
        format!(
            "define {adjectives}class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\nend;\n\
             define class <square> (<shape>)\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  1\nend;\n\
             define method area(s :: <square>)\n  2\nend;\n\
             define strict function measure(s :: <shape>)\n  area(s)\nend;\n\
             define function main()\n  measure(make(<circle>))\nend;"
        )
    };
    let (module, open) = decisions_for(&source(""));
    assert_eq!(
        strict_errors(&open),
        [(
            DiagnosticCode::TypingUnprovedDispatch,
            "function measure".into()
        )]
    );
    let measure = function_definition(&module, "measure");
    let typed = open.definition(measure).expect("typed");
    let fixes = strict_fixes(&module, typed, &typed.decisions[0]);
    let shape = module
        .classes
        .iter()
        .find(|class| class.name == "<shape>")
        .expect("class")
        .id;
    assert!(fixes.contains(&StrictFix::Seal {
        class: shape,
        make_abstract: true
    }));

    let (module, sealed) = decisions_for(&source("sealed abstract "));
    assert!(sealed.diagnostics.is_empty(), "{:?}", sealed.diagnostics);
    let decision = sealed
        .definition(function_definition(&module, "measure"))
        .expect("typed")
        .decisions[0]
        .clone();
    assert_eq!(decision.verdict, TypingVerdict::Proved);
    assert!(!decision.assumes.is_empty());
}

#[test]
fn limited_ranges_prove_strict_range_checks() {
    let source = |param: &str| {
        format!(
            "define function small(n :: limited(<integer>, min: 0, max: 9)) => <integer>\n  \
             n\nend;\n\
             define strict function twice(n :: {param}) => <integer>\n  small(n) + small(n)\nend;\n\
             define function main()\n  twice(3)\nend;"
        )
    };
    let (module, loose) = decisions_for(&source("<integer>"));
    assert_eq!(
        strict_errors(&loose),
        [
            (DiagnosticCode::TypingUnprovedRange, "function twice".into()),
            (DiagnosticCode::TypingUnprovedRange, "function twice".into()),
        ]
    );
    let twice = function_definition(&module, "twice");
    let typed = loose.definition(twice).expect("typed");
    let fixes = strict_fixes(&module, typed, &typed.decisions[0]);
    let StrictFix::Annotate { name, ty, .. } = &fixes[0] else {
        panic!("{fixes:?}");
    };
    assert_eq!(name, "n");
    assert_eq!(
        type_source(&module, ty).as_deref(),
        Some("limited(<integer>, min: 0, max: 9)")
    );

    let (_, narrow) = decisions_for(&source("limited(<integer>, min: 2, max: 5)"));
    assert!(narrow.diagnostics.is_empty(), "{:?}", narrow.diagnostics);
}

#[test]
fn strict_preview_reports_what_would_fail_without_changing_anything() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function measure(s :: <shape>)\n  area(s)\nend;\n\
         define function main()\n  measure(make(<circle>))\nend;"
    ));
    let measure = function_definition(&module, "measure");
    assert_eq!(artifact.strictness(measure), None);
    let preview = artifact.strict_preview(&module, measure);
    assert_eq!(
        preview
            .iter()
            .map(|diagnostic| (diagnostic.code, arg(diagnostic, "strict")))
            .collect::<Vec<_>>(),
        [(DiagnosticCode::TypingUnprovedDispatch, "declared")]
    );
    assert!(artifact.diagnostics.is_empty());
    assert!(
        artifact
            .strict_preview(&module, function_definition(&module, "main"))
            .is_empty()
    );
}

#[test]
fn loop_decisions_hold_on_every_iteration() {
    // `s` is a `<circle>` on the first iteration and a `<square>` on the
    // rest, so the send is covered by both methods and selects neither. The
    // trial passes that reach the loop head's fixpoint record nothing: the
    // send has one decision.
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main() => <integer>\n  \
         let s :: <shape> = make(<circle>);\n  \
         let n = 0;\n  \
         while (n < 2)\n    area(s);\n    s := make(<square>);\n    n := n + 1\n  end;\n  \
         n\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "main", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch decision");
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
    assert_eq!(dispatch.selected_method, None);
    assert_eq!(
        type_name(&module, &dispatch.actual[0]),
        "type-union(exactly <circle>, exactly <square>)"
    );
    // The head is exactly the join of the entry and the back edge.
    assert!(
        chain(&module, &artifact, &dispatch, 0).starts_with("joined@"),
        "{}",
        chain(&module, &artifact, &dispatch, 0)
    );
    // The test bounds `n` in the body, so `n + 1` cannot overflow; the
    // head's integer bound was widened to reach the fixpoint.
    let [overflow] = checks_of(&module, &artifact, "main", TypingCheck::Overflow)
        .try_into()
        .expect("one overflow check");
    assert_eq!(overflow.rule, TypingRule::OverflowImpossible);
    assert!(chain(&module, &artifact, &overflow, 0).contains("widened@"));
}

#[test]
fn an_assigned_local_is_bound_at_its_declared_type() {
    // Each read sees the last value assigned, while the binding's fact is
    // what holds over its whole scope: the declared type.
    let source = "define function main() => <integer>\n  \
                  let n :: <integer> = 0;\n  n := 5;\n  n\nend;";
    let (module, artifact) = decisions_for(source);
    let typed = typed(&module, &artifact, "main");
    let binding = Binding::Local(LocalId(0));
    let bound = typed.binding_type(binding).expect("bound");
    assert_eq!(type_name(&module, bound), "<integer>");
    let read = source.rfind("n\nend").expect("the read");
    let span = Span::new(read, read + 1);
    let read = typed.read_type(binding, span).expect("read");
    assert_eq!(type_name(&module, read), "singleton(5)");
}

#[test]
fn an_exit_resumes_with_what_the_body_may_have_assigned() {
    // The exit escapes to `call`, which may call it before or after the
    // assignment, so after the block `n` may hold either value: the
    // conversion cannot rely on the `0` it was bound to.
    let (module, artifact) = decisions_for(
        "define function call(f :: <callable>) => <integer>\n  apply(f, 1)\nend;\n\
         define function main() => <i32>\n  \
         let n = 0;\n  \
         let ignored = block (out)\n    n := 5000000000;\n    call(out)\n  end;\n  \
         as(<i32>, n)\nend;",
    );
    let [range] = checks_of(&module, &artifact, "main", TypingCheck::Range)
        .try_into()
        .expect("one range check");
    assert_eq!(range.verdict, TypingVerdict::Checked);
    assert_eq!(type_name(&module, &range.actual[0]), "<integer>");
}

#[test]
fn a_test_that_assigns_its_operand_narrows_nothing() {
    // `n < (n := 3)` compares the old `n`; narrowing the new one by it
    // would make the then branch unreachable.
    let (module, artifact) = decisions_for(
        "define function f(m :: <integer>) => <i32>\n  \
         let n = m;\n  \
         if (n < (n := 3)) as(<i32>, n) else 0i32 end\nend;",
    );
    let [range] = checks_of(&module, &artifact, "f", TypingCheck::Range)
        .try_into()
        .expect("one range check");
    assert_eq!(range.rule, TypingRule::RangeWithin);
    assert_eq!(type_name(&module, &range.actual[0]), "singleton(3)");
}

#[test]
fn a_join_relies_on_what_each_path_relied_on() {
    // After the `if`, `x` is a `<square>` on both paths: the first path's
    // test says so directly, but the second's says it only because
    // `<shape>` is sealed with exactly two subclasses. The send relies on
    // that sealing, whichever path's fact the join keeps.
    let (module, artifact) = decisions_for(
        "define sealed abstract class <shape> (<object>)\nend;\n\
         define sealed class <circle> (<shape>)\nend;\n\
         define sealed class <square> (<shape>)\nend;\n\
         define generic measure(s :: <shape>) => <integer>;\n\
         define method measure(s :: <square>) => <integer>\n  4\nend;\n\
         define function f(x :: <shape>, flag :: <boolean>) => <integer>\n  \
         block (out)\n    \
         let checked = if (flag)\n      \
         if (instance?(x, <square>)) 0 else out(0) end\n    \
         else\n      if (instance?(x, <circle>)) out(1) else 0 end\n    end;\n    \
         measure(x) + checked\n  end\nend;",
    );
    let shape = module
        .classes
        .iter()
        .position(|class| class.name == "<shape>")
        .map(|index| sem::ClassId(u32::try_from(index).expect("class index")))
        .expect("<shape>");
    let [dispatch] = checks_of(&module, &artifact, "f", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
    let typed = typed(&module, &artifact, "f");
    assert!(
        typed
            .dependencies(&dispatch)
            .contains(&TypingDependency::Sealed(shape)),
        "{:?}",
        typed.dependencies(&dispatch)
    );
}

/// The class `name` of `module`.
fn class_named(module: &sem::Module, name: &str) -> sem::ClassId {
    module
        .classes
        .iter()
        .position(|class| class.name == name)
        .map(|index| sem::ClassId(u32::try_from(index).expect("class index")))
        .expect("class")
}

const MEASURED_SHAPES: &str = "define sealed abstract class <shape> (<object>)\nend;\n\
     define sealed class <circle> (<shape>)\nend;\n\
     define sealed class <square> (<shape>)\nend;\n\
     define generic measure(s :: <object>) => <integer>;\n\
     define method measure(s :: <square>) => <integer>\n  4\nend;\n";

#[test]
fn a_join_relies_on_what_made_a_path_dead() {
    // The last branch is dead only because `<shape>` is sealed with two
    // subclasses, so `y` never reaches the send: proving it relies on that
    // sealing, though only the live branches' values reach it.
    let (module, artifact) = decisions_for(&format!(
        "{MEASURED_SHAPES}define function g(x :: <shape>, y :: <object>) => <integer>\n  \
         let s = if (instance?(x, <circle>)) make(<square>)\n    \
         elseif (instance?(x, <square>)) make(<square>) else y end;\n  \
         measure(s)\nend;"
    ));
    let [dispatch] = checks_of(&module, &artifact, "g", TypingCheck::Dispatch)
        .try_into()
        .expect("one dispatch");
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
    let shape = class_named(&module, "<shape>");
    let dependencies = typed(&module, &artifact, "g").dependencies(&dispatch);
    assert!(
        dependencies.contains(&TypingDependency::Sealed(shape)),
        "{dependencies:?}"
    );
}

#[test]
fn a_comparison_narrowing_relies_on_what_it_compared_against() {
    // `n < m` bounds `n` by `m`'s range, which `limit`'s contract gives, so
    // the overflow proof relies on that contract.
    let (module, artifact) = decisions_for(
        "define function limit() => limited(<integer>, min: 0, max: 10)\n  10\nend;\n\
         define function h(n :: limited(<integer>, min: 0, max: 4294967296)) => <integer>\n  \
         let m = limit();\n  \
         if (n < m) n * n else 0 end\nend;",
    );
    let [overflow] = checks_of(&module, &artifact, "h", TypingCheck::Overflow)
        .try_into()
        .expect("one overflow check");
    assert_eq!(overflow.rule, TypingRule::OverflowImpossible);
    let limit = module
        .functions
        .iter()
        .find(|function| function.name == "limit")
        .expect("limit")
        .definition;
    let dependencies = typed(&module, &artifact, "h").dependencies(&overflow);
    assert!(
        dependencies.contains(&TypingDependency::Contract(limit)),
        "{dependencies:?}"
    );
}

#[test]
fn a_loop_head_relies_on_what_its_back_edge_relied_on() {
    // `s` is a `<square>` at the head whichever iteration it is: on entry
    // because `p` is one, and around the back edge only because the
    // `unless` test, with `<shape>` sealed, leaves `q` a `<square>`.
    let (module, artifact) = decisions_for(&format!(
        "{MEASURED_SHAPES}define function f(p :: <square>, q :: <shape>, n :: <integer>) => <integer>\n  \
         let s :: <shape> = p;\n  \
         let total = 0;\n  \
         let i = 0;\n  \
         while (i < n)\n    \
         total := total + measure(s);\n    \
         unless (instance?(q, <circle>)) s := q end;\n    \
         i := i + 1\n  end;\n  \
         total\nend;"
    ));
    let dispatch = checks_of(&module, &artifact, "f", TypingCheck::Dispatch)
        .into_iter()
        .next()
        .expect("a dispatch");
    assert_eq!(dispatch.verdict, TypingVerdict::Proved);
    let shape = class_named(&module, "<shape>");
    let dependencies = typed(&module, &artifact, "f").dependencies(&dispatch);
    assert!(
        dependencies.contains(&TypingDependency::Sealed(shape)),
        "{dependencies:?}"
    );
}

/// Whether the one proved dispatch of `name` relies on `<shape>`'s sealing.
fn dispatch_relies_on_shape_sealing(source: &str, name: &str) -> bool {
    let (module, artifact) = decisions_for(&format!("{MEASURED_SHAPES}{source}"));
    let dispatch = checks_of(&module, &artifact, name, TypingCheck::Dispatch)
        .into_iter()
        .find(|decision| decision.verdict == TypingVerdict::Proved)
        .expect("a proved dispatch");
    typed(&module, &artifact, name)
        .dependencies(&dispatch)
        .contains(&TypingDependency::Sealed(class_named(&module, "<shape>")))
}

#[test]
fn code_in_a_dead_path_is_dead_for_the_path_reason() {
    // The exit call is in an arm the sealing rules out; the arm is dead
    // because of that, not because the call would leave.
    assert!(dispatch_relies_on_shape_sealing(
        "define function f(x :: <shape>, y :: <object>) => <integer>\n  \
         let sq :: <object> = make(<square>);\n  \
         let s :: <object> = block (out)\n    \
         if (instance?(x, <circle>)) sq elseif (instance?(x, <square>)) sq else out(y) end\n  \
         end;\n  \
         measure(s)\nend;",
        "f",
    ));
}

#[test]
fn dead_paths_that_meet_keep_both_reasons() {
    // Each arm of the inner test is dead for its own reason: the first two
    // because a literal is `#f`, the last because of the sealing.
    assert!(dispatch_relies_on_shape_sealing(
        "define function f(x :: <shape>, y :: <object>) => <integer>\n  \
         let t :: <object> = if (if (instance?(x, <circle>)) #f \
         elseif (instance?(x, <square>)) #f else #t end) y else make(<square>) end;\n  \
         measure(t)\nend;",
        "f",
    ));
    // Two arms leave through exits, and the third is dead by the sealing.
    assert!(dispatch_relies_on_shape_sealing(
        "define function f(x :: <shape>, flag :: <boolean>, y :: <object>) => <integer>\n  \
         let s :: <object> = make(<square>);\n  \
         block (out)\n    \
         if (flag)\n      \
         if (instance?(x, <circle>)) out(0) elseif (instance?(x, <square>)) out(1) \
         else s := y end\n    \
         else 0 end;\n    \
         measure(s)\n  end\nend;",
        "f",
    ));
}

#[test]
fn a_loop_whose_back_edge_is_dead_relies_on_why() {
    // No iteration comes back, because `limit`'s contract keeps `m` at most
    // 10; if it did, `s` would be `y` at the send.
    let (module, artifact) = decisions_for(&format!(
        "{MEASURED_SHAPES}define function limit() => limited(<integer>, min: 0, max: 10)\n  10\nend;\n\
         define function h(n :: <integer>, y :: <object>) => <integer>\n  \
         let s :: <object> = make(<square>);\n  \
         let m = limit();\n  \
         let i = 0;\n  \
         block (out)\n    \
         while (i < n)\n      i := i + 1;\n      s := y;\n      \
         if (m > 20) 0 else out(0) end\n    end;\n    \
         measure(s)\n  end\nend;"
    ));
    let dispatch = checks_of(&module, &artifact, "h", TypingCheck::Dispatch)
        .into_iter()
        .find(|decision| decision.verdict == TypingVerdict::Proved)
        .expect("a proved dispatch");
    let limit = module
        .functions
        .iter()
        .find(|function| function.name == "limit")
        .expect("limit")
        .definition;
    let dependencies = typed(&module, &artifact, "h").dependencies(&dispatch);
    assert!(
        dependencies.contains(&TypingDependency::Contract(limit)),
        "{dependencies:?}"
    );
}

#[test]
fn a_dead_path_is_cited_only_for_the_bindings_it_changed() {
    // The dead arm narrows `s`, not `x`: the send on `x` relies on nothing
    // about `<shape>`, and its strict fixes still offer to test `x`.
    let source = "define function r(x :: <object>, s :: <shape>) => <integer>\n  \
                  if (instance?(x, <shape>))\n    \
                  let k = if (instance?(s, <circle>)) 1 elseif (instance?(s, <square>)) 2 else 3 end;\n    \
                  measure(x) + k\n  \
                  else 0 end\nend;";
    let (module, artifact) = decisions_for(&format!("{MEASURED_SHAPES}{source}"));
    let typed = typed(&module, &artifact, "r");
    let dispatch = typed
        .decisions
        .iter()
        .find(|decision| decision.check == TypingCheck::Dispatch)
        .expect("a dispatch");
    let shape = class_named(&module, "<shape>");
    assert!(
        !typed
            .dependencies(dispatch)
            .contains(&TypingDependency::Sealed(shape))
    );
    let fixes = strict_fixes(&module, typed, dispatch);
    assert!(
        fixes
            .iter()
            .any(|fix| matches!(fix, StrictFix::Narrow { name, .. } if name == "x")),
        "{fixes:?}"
    );
}

#[test]
fn a_refutation_labels_only_the_paths_that_gave_the_value() {
    // `y` is `v` whichever live arm runs; the dead arm's narrowing is cited
    // for what it relies on, but gave no value, so it labels nothing.
    let (_, artifact) = decisions_for(&format!(
        "{MEASURED_SHAPES}define function p(s :: <shape>) => <integer>\n  \
         let v :: <object> = 1;\n  \
         let y = if (instance?(s, <circle>)) v\n    \
         elseif (instance?(s, <square>)) v else make(<square>) end;\n  \
         measure(y)\nend;"
    ));
    let refuted = artifact
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == DiagnosticCode::TypingDispatchNoApplicableMethod)
        .expect("the refuted send");
    let labels = refuted
        .labels
        .iter()
        .filter_map(|label| label.message.as_deref())
        .collect::<Vec<_>>();
    assert!(
        labels.iter().all(|label| !label.contains("narrowed")),
        "{labels:?}"
    );
}

#[test]
fn a_select_arm_ruled_out_by_a_contract_relies_on_it() {
    // `bit`'s contract keeps `b` to 0 or 1, so the `==` tests leave the
    // `otherwise` arm dead and `y` never reaches the send.
    let (module, artifact) = decisions_for(&format!(
        "{MEASURED_SHAPES}define function bit() => limited(<integer>, min: 0, max: 1)\n  1\nend;\n\
         define function f(y :: <object>) => <integer>\n  \
         let sq :: <object> = make(<square>);\n  \
         let b = bit();\n  \
         let s = select (b) 0 => sq; 1 => sq; otherwise => y; end;\n  \
         measure(s)\nend;"
    ));
    let dispatch = checks_of(&module, &artifact, "f", TypingCheck::Dispatch)
        .into_iter()
        .find(|decision| decision.verdict == TypingVerdict::Proved)
        .expect("a proved dispatch");
    let bit = module
        .functions
        .iter()
        .find(|function| function.name == "bit")
        .expect("bit")
        .definition;
    let dependencies = typed(&module, &artifact, "f").dependencies(&dispatch);
    assert!(
        dependencies.contains(&TypingDependency::Contract(bit)),
        "{dependencies:?}"
    );
}
