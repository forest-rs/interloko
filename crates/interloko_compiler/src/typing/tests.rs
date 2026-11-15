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

    let strict = artifact.with_config(&module, TypingConfig::strict());
    assert_eq!(strict.diagnostics.len(), 1);
    assert_eq!(
        strict.diagnostics[0].code,
        DiagnosticCode::TypingDispatchRuntimeChecked
    );
    assert_eq!(strict.diagnostics[0].severity, Severity::Warning);
    assert_eq!(strict.definitions, artifact.definitions);
}

#[test]
fn dispatch_with_no_possible_method_is_refuted_and_warns() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  \
         if 1 < 2 then area(make(<circle>)) else area(make(<shape>)) end\nend;"
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
         as(<circle>, if 1 < 2 then make(<shape>) else s end)\nend;\n\
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
         if 1 < 2 then size-of(2.5) + magnitude(2.5) else whole(2.5) end\nend;"
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
fn a_cast_that_always_fails_leaves_no_values_downstream() {
    // sem types the cast as `<circle>`; its operand is exactly
    // `<shape>`, so no value gets through and the dispatch on it is
    // unreachable rather than refuted.
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function main()\n  area(as(<circle>, make(<shape>)))\nend;"
    ));
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0].check, TypingCheck::Cast);
    assert_eq!(decisions[0].verdict, TypingVerdict::Refuted);
    assert_eq!(decisions[1].check, TypingCheck::Dispatch);
    assert!(decisions[1].actual[0].is_bottom());
    assert_eq!(decisions[1].verdict, TypingVerdict::Proved);
    assert_eq!(decisions[1].rule, TypingRule::Unreachable);
    assert_eq!(type_name(&module, &decisions[1].actual[0]), "type-union()");
    assert_eq!(artifact.diagnostics.len(), 1);
}

#[test]
fn synthesis_joins_branches_and_stays_within_sem() {
    let (module, artifact) = decisions_for(&format!(
        "{SHAPES}define function pick(s :: <shape>)\n  \
         as(<shape>, if 1 < 2 then as(<shape>, make(<circle>)) \
         else as(<shape>, make(<square>)) end)\nend;\n\
         define function same()\n  \
         as(<shape>, if 1 < 2 then make(<circle>) else make(<circle>) end)\nend;\n\
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
         if instance?(x, <integer>) then whole(x) else 0 end\nend;"
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
         let shape = if round? then c else s end;\n  \
         if instance?(shape, <circle>) then 0 else area(shape) end\nend;"
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
         if instance?(x, <i32>) then as(<u32>, as(<i32>, x)) else as(<u32>, x) end\nend;",
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
         if instance?(x, <i32>) then x + 1 else 0 end\nend;",
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
fn integer_comparisons_narrow_both_branches() {
    let (module, artifact) = decisions_for(
        "define function f(n :: <integer>) => <i32>\n  \
         if n < 0 then 0i32\n  \
         else if 100 >= n then as(<i32>, n) else as(<i32>, n - 100) end\n  end\nend;",
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
         if n = 0u32 then as(<i32>, n) else as(<i32>, n - 1u32) end\nend;",
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
         area(if round? then c else s end)\nend;\n\
         define function some(round? :: <boolean>) => <integer>\n  \
         let c :: <shape> = make(<circle>);\n  let s :: <shape> = make(<shape>);\n  \
         area(if round? then c else s end)\nend;"
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
         area(if instance?(c, <circle>) then d else as(<shape>, s) end)\nend;"
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
         let picked = if n > 10 then return(c) else s end;\n    picked\n  end;\n  \
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
         if #t then whole(7) else area(make(<shape>)) end\nend;"
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
fn certain_overflow_is_refuted_and_leaves_no_value() {
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
    assert_eq!(decisions[1].rule, TypingRule::Unreachable);
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
         if instance?(x, <integer>) then\n    block ()\n      \
         let f :: <callable> = function () => <integer>\n        whole(x)\n      end;\n      0\n    \
         end\n  else 0 end\nend;\n\
         define function inner(x :: <object>) => <integer>\n  \
         let f :: <callable> = function () => <integer>\n    \
         if instance?(x, <integer>) then whole(x) else 0 end\n  end;\n  0\nend;"
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
         if instance?(s, <circle>) then area(s) else 0 end\nend;"
    ));
    let dump = artifact.dump(&module);
    assert!(
        dump.contains("s1 dispatch proved rule=dispatch-covered span=",),
        "{dump}"
    );
    assert!(dump.contains("because=narrowed(s0,then)@"), "{dump}");
    assert!(dump.contains("<circle> <- declared@"), "{dump}");
    assert!(
        dump.contains(" assumes=closed-world(generic area)\n"),
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
    let module = module(&format!(
        "{SHAPES}define function main() => <integer>\n  \
         area(as(<circle>, make(<shape>)))\nend;"
    ));
    assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
    let artifact = analyze(&module, config);
    let decisions = main_decisions(&module, &artifact);
    assert_eq!(decisions[1].rule, TypingRule::Unreachable);
    assert_eq!(artifact.severity(&decisions[1]), TypingSeverity::Silent);
    let codes = artifact
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, [DiagnosticCode::TypingCastAlwaysFails]);
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
fn strict_preset_reports_certain_but_not_possible_overflow() {
    let strict = TypingConfig::strict();
    assert_eq!(
        strict.severity(TypingCheck::Overflow, TypingVerdict::Checked),
        TypingSeverity::Silent
    );
    assert_eq!(
        strict.severity(TypingCheck::Overflow, TypingVerdict::Refuted),
        TypingSeverity::Error
    );
    assert_eq!(
        strict.severity(TypingCheck::Range, TypingVerdict::Checked),
        TypingSeverity::Warning
    );
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
         if instance?(x, <integer>) then whole(x) else 0 end\nend;"
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
         let picked = if n > 10 then return(c) else s end;\n    picked\n  end;\n  \
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
         if instance?(x, <integer>) then\n    block ()\n      \
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
         if instance?(x, <integer>) then whole(x) else y end\nend;"
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
