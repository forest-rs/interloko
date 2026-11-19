//! Claim reports through the EOP: what typing established about a
//! definition, what its generated code relies on, which checks stay at run
//! time and why, and what supports the result.

use interloko_compiler::claims::{
    ClaimDetail, ClaimReport, ResidualCause, SiteStatus, UnsupportedRelation, WideningLimit,
};
use interloko_compiler::lower::{CheckCodegen, CodegenPolicy};
use interloko_compiler::session::{
    CompilerSession, EnvironmentAffordance, EnvironmentKind, RuntimeMode, SessionArtifacts,
    TypingFacet,
};
use interloko_compiler::typing::{
    CheckOrigin, StrictSource, TypingCheck, TypingConfig, TypingDependency,
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
  let bonus = if (instance?(s, <circle>)) 1 else 0 end;
  area(s) + bonus
end;
define function exact(c :: <circle>)
  if (instance?(c, <circle>)) area(c) else 0 end
end;
define function narrow(s :: <shape>)
  area(as(<circle>, s))
end;
define function count(x)
  x + 1
end;
define function call-it(f :: <callable>) => <circle>
  let c :: <circle> = f();
  c
end;
define function main()
  measure(make(<circle>))
end;
";

fn compile(source: &str) -> SessionArtifacts {
    CompilerSession::compile_source("claims.loko", source, RuntimeMode::CompileOnly)
}

fn function_id(artifacts: &SessionArtifacts, name: &str) -> String {
    let function = artifacts
        .sem
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("no function {name}"));
    format!("function:f{}", function.id.0)
}

fn report(artifacts: &SessionArtifacts, name: &str, detail: ClaimDetail) -> ClaimReport {
    artifacts
        .claim_report(&function_id(artifacts, name), detail)
        .unwrap_or_else(|| panic!("no claim report for {name}"))
}

/// The cause of the one unproved check of `check` in `report`.
fn cause(report: &ClaimReport, check: TypingCheck) -> ResidualCause {
    report
        .residual_sites()
        .find(|site| site.check == check)
        .and_then(|site| site.residual.as_ref())
        .unwrap_or_else(|| panic!("no unproved {check:?} in\n{}", report.render()))
        .cause
}

#[test]
fn a_strict_report_states_its_guarantee_and_why_each_check_stays() {
    let artifacts = compile(DIGITS);
    let report = report(&artifacts, "twice", ClaimDetail::Summary);
    assert_eq!(report.revision, None);
    assert_eq!(report.basis, artifacts.claims_basis());
    assert_eq!(report.object, format!("claims:{}", report.definition));
    assert_eq!(report.label, "function twice");
    assert_eq!(report.codegen, CodegenPolicy::Proved);

    // Strictness is a stated guarantee, not "check-free": the overflow
    // check of `+` stays at run time and is exempt.
    assert_eq!(report.strict.source, Some(StrictSource::Declared));
    assert_eq!(report.strict.unmet, 2);
    assert_eq!(report.strict.exempt, 1);
    for kept in [
        "explicit as(...) casts",
        "instance? tests",
        "indirect-entry guards",
        "integer overflow (strict.overflow=runtime)",
    ] {
        assert!(
            report.strict.guarantee.contains(kept),
            "{}",
            report.strict.guarantee
        );
    }
    assert!(!report.strict.guarantee.contains("check-free"));

    let tally = report.tally;
    assert_eq!((tally.sites, tally.proved, tally.checked), (3, 0, 3));
    assert_eq!((tally.runtime, tally.relied, tally.errors), (3, 0, 2));

    let contract = report.contract.as_ref().expect("a function has a contract");
    assert_eq!(contract.params, ["n: <integer>"]);
    assert_eq!(contract.results, ["<integer>"]);

    // Both calls rely on `digit`'s contract.
    let [support] = report.support.as_slice() else {
        panic!("one support entry in\n{}", report.render());
    };
    assert!(matches!(support.dependency, TypingDependency::Contract(_)));
    assert_eq!(support.description, "contract of function digit");
    assert_eq!(support.relied, 0);

    let ranges = report
        .residual_sites()
        .filter(|site| site.check == TypingCheck::Range)
        .collect::<Vec<_>>();
    assert_eq!(ranges.len(), 2);
    for site in ranges {
        assert!(site.strict_required);
        assert_eq!(site.status, SiteStatus::Runtime);
        let residual = site.residual.as_ref().expect("unproved");
        assert_eq!(residual.cause, ResidualCause::Declared);
        // The annotation fix comes first, and says it narrows the
        // contract: the check moves to the callers.
        let fix = residual.fixes.first().expect("fixes");
        assert_eq!(fix.fix.kind(), "annotate");
        assert!(fix.fix.consequence().starts_with("narrows the contract"));
        assert!(fix.annotation.is_some());
    }
    let overflow = report
        .residual_sites()
        .find(|site| site.check == TypingCheck::Overflow)
        .expect("an overflow check");
    assert!(!overflow.strict_required);
    assert_eq!(
        overflow.residual.as_ref().map(|residual| residual.cause),
        Some(ResidualCause::Declared)
    );
}

#[test]
fn a_report_separates_proved_checks_from_those_generated_code_relies_on() {
    let artifacts = compile(SHAPES);
    let report = report(&artifacts, "exact", ClaimDetail::Full);
    // `instance?(c, <circle>)` is always true and the dead branch goes;
    // the send on an exact `<circle>` is proved.
    let instance = report
        .sites
        .iter()
        .find(|site| site.check == TypingCheck::Instance)
        .expect("an instance? site");
    assert_eq!(instance.status, SiteStatus::Relied);
    assert_eq!(instance.codegen, CheckCodegen::Elided);
    assert!(instance.residual.is_none());
    assert!(!instance.reasons.is_empty(), "full detail renders reasons");
    assert!(report.tally.relied >= 1, "{}", report.render());
    let methods = report
        .support
        .iter()
        .find(|entry| matches!(entry.dependency, TypingDependency::Methods(_)))
        .expect("the send relies on area's method set");
    assert!(
        methods
            .description
            .starts_with("generic area has exactly methods")
    );

    // Under `off` the same proofs are made but nothing relies on them.
    let off = artifacts.with_codegen_policy(CodegenPolicy::Off);
    let off_report = off
        .claim_report(&function_id(&off, "exact"), ClaimDetail::Full)
        .expect("a report");
    assert_eq!(off_report.tally.proved, report.tally.proved);
    assert_eq!(off_report.tally.relied, 0);
    assert_eq!(off_report.tally.proved_kept, report.tally.proved);
    assert_ne!(off_report.basis, report.basis);

    // A summary lists only what typing did not prove.
    let summary = artifacts
        .claim_report(&report.object, ClaimDetail::Summary)
        .expect("claims: ids select reports");
    assert!(summary.sites.is_empty(), "{}", summary.render());
    assert!(summary.render().contains("  unproved checks: 0\n"));
}

#[test]
fn residual_checks_name_why_typing_could_not_prove_them() {
    let artifacts = compile(SHAPES);
    let measure = report(&artifacts, "measure", ClaimDetail::Summary);
    assert_eq!(
        cause(&measure, TypingCheck::Instance),
        ResidualCause::Requested(CheckOrigin::Test)
    );
    // `<shape>` is open; its current subclasses would cover the send.
    let shape = artifacts
        .sem
        .classes
        .iter()
        .find(|class| class.name == "<shape>")
        .expect("<shape>")
        .id;
    assert_eq!(
        cause(&measure, TypingCheck::Dispatch),
        ResidualCause::OpenClass(shape)
    );
    let send = measure
        .residual_sites()
        .find(|site| site.check == TypingCheck::Dispatch)
        .and_then(|site| site.residual.as_ref())
        .expect("send");
    assert!(
        send.fixes.iter().any(|offer| offer.fix.kind() == "seal"
            && offer
                .fix
                .consequence()
                .starts_with("relies on a closed world")),
        "{}",
        measure.render()
    );
    // `area`'s result is inferred, so the `+` has no written range.
    assert_eq!(
        cause(&measure, TypingCheck::Overflow),
        ResidualCause::UnannotatedInput
    );

    let narrow = report(&artifacts, "narrow", ClaimDetail::Summary);
    assert_eq!(
        cause(&narrow, TypingCheck::Cast),
        ResidualCause::Requested(CheckOrigin::Explicit)
    );

    let count = report(&artifacts, "count", ClaimDetail::Summary);
    assert_eq!(
        cause(&count, TypingCheck::Overflow),
        ResidualCause::UnannotatedInput
    );
    let input = count
        .residual_sites()
        .find_map(|site| site.residual.as_ref()?.input.clone())
        .expect("the unannotated parameter's target");
    assert_eq!(input.to_string(), format!("binding:{}:x", count.definition));

    let call = report(&artifacts, "call-it", ClaimDetail::Summary);
    assert_eq!(
        cause(&call, TypingCheck::Cast),
        ResidualCause::Unsupported(UnsupportedRelation::UnrefinedExpression)
    );
    assert!(cause(&call, TypingCheck::Cast).is_precision_loss());
}

#[test]
fn widened_analysis_is_named_as_the_cause() {
    // Ten leaves under a sealed root: an `instance?` test's else branch
    // would leave nine pieces, more than a union holds, and a send on two
    // such arguments splits into 100 combinations.
    let mut source = String::from("define sealed abstract class <big> (<object>) end;\n");
    for index in 0..10 {
        source.push_str(&format!("define class <k{index}> (<big>) end;\n"));
    }
    source.push_str("define generic name(x);\n");
    for index in 1..10 {
        source.push_str(&format!(
            "define method name(x :: <k{index}>) {index} end;\n"
        ));
    }
    source.push_str("define generic mix(x, y);\n");
    for index in 0..10 {
        source.push_str(&format!(
            "define method mix(x :: <k{index}>, y :: <big>) {index} end;\n"
        ));
    }
    source.push_str(
        "define function pick(b :: <big>)
  if (instance?(b, <k0>)) 0 else name(b) end
end;
define function both(a :: <big>, b :: <big>)
  mix(a, b)
end;
",
    );
    let artifacts = compile(&source);
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let pick = report(&artifacts, "pick", ClaimDetail::Summary);
    assert_eq!(
        cause(&pick, TypingCheck::Dispatch),
        ResidualCause::Widened(WideningLimit::Atoms)
    );
    let both = report(&artifacts, "both", ClaimDetail::Summary);
    assert_eq!(
        cause(&both, TypingCheck::Dispatch),
        ResidualCause::Widened(WideningLimit::DispatchTuples)
    );
    assert!(
        both.render()
            .contains("cause=widened(dispatch-tuples): the arguments split into 100 combinations"),
        "{}",
        both.render()
    );
}

#[test]
fn claims_are_environment_objects_of_every_source_definition() {
    let artifacts = compile(DIGITS);
    let objects = artifacts.environment_objects();
    let claims = objects
        .iter()
        .filter(|object| object.kind == EnvironmentKind::Claims)
        .collect::<Vec<_>>();
    assert_eq!(claims.len(), 3);
    let twice = report(&artifacts, "twice", ClaimDetail::Summary);
    let object = claims
        .iter()
        .find(|object| object.id.as_str() == twice.object)
        .expect("twice's claims object");
    assert!(
        object
            .affordances
            .contains(EnvironmentAffordance::ShowClaims)
    );
    assert_eq!(object.source.definition, Some(twice.definition));
    assert!(
        object
            .inspection
            .detail
            .contains("strict=declared sites=3 proved=0 relied=0 runtime=3"),
        "{}",
        object.inspection.detail
    );
    let Some(TypingFacet::Claims {
        definition,
        tally,
        strict_unmet,
    }) = &object.typing
    else {
        panic!("a claims facet");
    };
    assert_eq!(*definition, twice.definition);
    assert_eq!(*tally, twice.tally);
    assert_eq!(*strict_unmet, 2);

    let function = objects
        .iter()
        .find(|object| object.id.as_str() == function_id(&artifacts, "twice"))
        .expect("function object");
    assert!(
        function
            .affordances
            .contains(EnvironmentAffordance::ShowClaims)
    );
    assert_eq!(
        artifacts.explain_claims(&function_id(&artifacts, "twice"), ClaimDetail::Summary),
        Some(twice.render())
    );
    assert_eq!(
        artifacts.claim_report(&twice.definition.to_string(), ClaimDetail::Summary),
        Some(twice.clone())
    );
    assert_eq!(
        artifacts.claim_report("generic:g0", ClaimDetail::Summary),
        None
    );
    assert_eq!(
        artifacts.claim_report("claims:nothing", ClaimDetail::Summary),
        None
    );

    // The basis follows the configuration the report was derived under.
    let strict = artifacts.with_typing_config(TypingConfig::strict());
    assert_ne!(strict.claims_basis(), artifacts.claims_basis());
    let digit = strict
        .claim_report(&function_id(&strict, "digit"), ClaimDetail::Summary)
        .expect("digit");
    assert_eq!(digit.strict.source, Some(StrictSource::Policy));
}
