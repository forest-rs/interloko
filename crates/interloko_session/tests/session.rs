//! Embedding contracts across accepted edits and explicit host behavior.

use interloko_compiler::annotation::AnnotationTarget;
use interloko_compiler::claims::{ClaimDetail, ResidualCause};
use interloko_compiler::lower::CodegenPolicy;
use interloko_compiler::sem::{HostFunction, HostMetaRegistry, HostValueKind};
use interloko_compiler::session::{
    DumpStage, ExecutionTraceDetail, FunctionCallError, FunctionValue, HostFunctionFailure,
    HostFunctionHandler, SessionMutation,
};
use interloko_compiler::typing::{CheckOrigin, TypingConfig};
use interloko_session::{Session, SessionError};

const SOURCE: &str = "
define class <item> (<object>)
  slot value :: <integer>;
end;
define generic score(x);
define method score(x :: <object>) 0 end;
define function answer() score(make(<item>, value: 7)) end;
";

fn add_method(session: &Session, source: &str) -> SessionMutation {
    let generic = session
        .artifacts()
        .sem
        .generics
        .iter()
        .find(|generic| generic.name == "score")
        .unwrap()
        .id;
    SessionMutation::AddMethod {
        generic,
        method_source: source.into(),
    }
}

fn answer(session: &Session) -> FunctionValue {
    session.call("answer", &[], None).unwrap().value
}

#[test]
fn rejected_operations_retain_accepted_mutation_history_and_dispatch() {
    let mut session = Session::new("mutable.loko", SOURCE).unwrap();
    let request = add_method(&session, "define method score(x :: <item>) x.value end;");
    let mutation = session.mutate(0, request.clone()).unwrap();
    let accepted_source = session.artifacts().source.source.clone();
    assert_eq!(session.revision(), 1);
    assert_eq!(answer(&session), FunctionValue::Integer(7));

    assert!(matches!(
        session.mutate(0, request),
        Err(SessionError::StaleRevision {
            expected: 0,
            actual: 1
        })
    ));
    assert!(matches!(
        session.rewind(0, None),
        Err(SessionError::StaleRevision {
            expected: 0,
            actual: 1
        })
    ));
    let invalid = add_method(&session, "not a method");
    assert!(matches!(
        session.mutate(1, invalid),
        Err(SessionError::Mutation(_))
    ));
    assert!(matches!(
        session.rewind(1, Some(100)),
        Err(SessionError::Rewind(_))
    ));
    assert!(matches!(
        session.replace_source(1, "define function answer() missing end;"),
        Err(SessionError::Compilation { .. })
    ));
    assert_eq!(session.revision(), 1);
    assert_eq!(session.artifacts().source.source, accepted_source);
    assert_eq!(session.artifacts().mutations().len(), 1);
    assert_eq!(session.artifacts().mutations()[0].outcome, mutation);
    assert_eq!(answer(&session), FunctionValue::Integer(7));

    session.rewind(1, None).unwrap();
    assert_eq!(session.revision(), 2);
    assert!(session.artifacts().mutations().is_empty());
    assert_eq!(answer(&session), FunctionValue::Integer(0));
    let request = add_method(&session, "define method score(x :: <item>) 99 end;");
    let next = session.mutate(2, request).unwrap();
    assert!(next.mutation_id > mutation.mutation_id);
    assert_eq!(answer(&session), FunctionValue::Integer(99));
}

#[test]
fn accepted_replacement_starts_a_new_mutation_base() {
    let mut session = Session::new("mutable.loko", SOURCE).unwrap();
    let request = add_method(&session, "define method score(x :: <item>) x.value end;");
    session.mutate(0, request).unwrap();
    session
        .replace_source(1, "define function answer() 42 end;")
        .unwrap();
    assert_eq!(session.revision(), 2);
    assert!(session.artifacts().mutations().is_empty());
    assert_eq!(answer(&session), FunctionValue::Integer(42));
    session.rewind(2, None).unwrap();
    assert_eq!(session.revision(), 3);
    assert_eq!(answer(&session), FunctionValue::Integer(42));
}

struct Increment;
impl HostFunctionHandler for Increment {
    fn call(
        &mut self,
        function: &HostFunction,
        arguments: &[FunctionValue],
    ) -> Result<FunctionValue, HostFunctionFailure> {
        assert_eq!(function.name, "increment");
        let [FunctionValue::Integer(value)] = arguments else {
            return Err(HostFunctionFailure::new("expected an integer"));
        };
        Ok(FunctionValue::Integer(value + 1))
    }
}

#[test]
fn source_replacement_keeps_host_declarations_and_requires_an_explicit_handler() {
    let mut registry = HostMetaRegistry::new();
    registry.add_function(HostFunction::new(
        "increment",
        vec![HostValueKind::Integer],
        HostValueKind::Integer,
    ));
    let mut session = Session::with_host_registry(
        "host.loko",
        "define function answer() increment(41) end;",
        registry,
    )
    .unwrap();
    assert!(session.inspect("host-function:increment").is_some());
    assert!(matches!(
        session.call("answer", &[], None),
        Err(FunctionCallError::HostFunctionFailed { .. })
    ));
    let result = session
        .call_with_host(
            "answer",
            &[],
            Some(ExecutionTraceDetail::Boundaries),
            &mut Increment,
        )
        .unwrap();
    assert_eq!(result.value, FunctionValue::Integer(42));
    let trace = result.trace.expect("traced call records a trace");
    let view = session.artifacts().execution_trace_view(&trace);
    assert_eq!(view.host_calls().len(), 1);
    assert_eq!(session.revision(), 0);

    session
        .replace_source(0, "define function answer() increment(99) end;")
        .unwrap();
    let result = session
        .call_with_host("answer", &[], None, &mut Increment)
        .unwrap();
    assert_eq!(result.value, FunctionValue::Integer(100));
    assert_eq!(session.revision(), 1);
}

#[test]
fn codegen_policy_is_explicit_kept_by_replacement_and_changed_by_revision() {
    // `step(i) + 2` may overflow as far as typing knows; `i + 1` cannot.
    const SUM: &str = "define function step(i :: <integer>) => <integer>
  if i < 1000 then if i > 0 then i + 1 else 0 end else 0 end
end;
define function answer() => <integer> step(999) + 2 end;";
    let checked_adds = |session: &Session| {
        session
            .dump(DumpStage::Lowered)
            .matches("i64.add_ovf")
            .count()
    };

    assert_eq!(
        Session::new("sum.loko", SUM).unwrap().codegen_policy(),
        CodegenPolicy::Proved
    );
    let mut session =
        Session::with_codegen_policy("sum.loko", SUM, HostMetaRegistry::new(), CodegenPolicy::Off)
            .unwrap();
    assert_eq!(session.codegen_policy(), CodegenPolicy::Off);
    assert_eq!(checked_adds(&session), 2);

    assert!(matches!(
        session.set_codegen_policy(1, CodegenPolicy::Proved),
        Err(SessionError::StaleRevision {
            expected: 1,
            actual: 0
        })
    ));
    assert_eq!(
        session.set_codegen_policy(0, CodegenPolicy::Off).unwrap(),
        0
    );
    assert_eq!(
        session
            .set_codegen_policy(0, CodegenPolicy::Proved)
            .unwrap(),
        1
    );
    assert_eq!(session.codegen_policy(), CodegenPolicy::Proved);
    assert_eq!(checked_adds(&session), 1);
    assert_eq!(
        session.call("answer", &[], None).unwrap().value,
        FunctionValue::Integer(1002)
    );

    // Replacement compiles the candidate under the session's policy.
    session.replace_source(1, SUM).unwrap();
    assert_eq!(session.codegen_policy(), CodegenPolicy::Proved);
    assert_eq!(checked_adds(&session), 1);
}

#[test]
fn annotations_are_previewed_then_applied_as_revisioned_mutations() {
    let mut session = Session::new("typed.loko", SOURCE).unwrap();
    let definition = session
        .artifacts()
        .sem
        .functions
        .iter()
        .find(|function| function.name == "answer")
        .unwrap()
        .definition;
    let target = AnnotationTarget::Result { definition };

    let preview = session.preview_annotation(&target, "<integer>").unwrap();
    assert_eq!(preview.previous, None);
    assert_eq!(session.revision(), 0);
    assert!(session.artifacts().mutations().is_empty());

    let outcome = session
        .mutate(
            0,
            SessionMutation::Annotate {
                target: target.clone(),
                type_source: "<integer>".into(),
            },
        )
        .unwrap();
    assert_eq!(outcome.annotation(), Some(&preview));
    assert_eq!(session.revision(), 1);
    assert!(
        session
            .artifacts()
            .source
            .source
            .contains("answer() => <integer>")
    );
    assert_eq!(answer(&session), FunctionValue::Integer(0));

    assert!(matches!(
        session.preview_annotation(&target, "<nowhere>"),
        Err(SessionError::Mutation(_))
    ));
}

#[test]
fn typing_config_is_a_revisioned_session_input_kept_by_replacement() {
    let mut session = Session::new("typed.loko", SOURCE).unwrap();
    assert_eq!(session.typing_config(), TypingConfig::soft());
    assert_eq!(
        session.set_typing_config(0, TypingConfig::soft()).unwrap(),
        0
    );
    assert_eq!(
        session
            .set_typing_config(0, TypingConfig::silent())
            .unwrap(),
        1
    );
    assert_eq!(session.typing_config(), TypingConfig::silent());

    // Reporting proved dispatches turns the one in `answer` into a warning,
    // which the session does not accept.
    let loud = TypingConfig::silent()
        .apply_spec("dispatch.proved=warning")
        .unwrap();
    assert!(matches!(
        session.set_typing_config(1, loud),
        Err(SessionError::Compilation { .. })
    ));
    assert_eq!(session.typing_config(), TypingConfig::silent());

    session
        .replace_source(1, "define function answer() 1 end;")
        .unwrap();
    assert_eq!(session.typing_config(), TypingConfig::silent());
}

#[test]
fn strict_errors_are_refused_like_any_compile_error_and_keep_the_accepted_code() {
    let mut session = Session::new("strict.loko", SOURCE).unwrap();
    // `answer`'s send is on an open class: proved by its one `<object>`
    // method. A strict function whose range check typing cannot prove is
    // refused, with the strict error and its fixes as the reason, and the
    // accepted code keeps running.
    let strict = "
define function digit(d :: limited(<integer>, min: 0, max: 9)) => <integer>
  d
end;
define strict function answer() => <integer>
  digit(7 + 0)
end;
define strict function twice(n :: <integer>) => <integer>
  digit(n)
end;
";
    let Err(SessionError::Compilation {
        diagnostics,
        emission_failed,
    }) = session.replace_source(0, strict)
    else {
        panic!("a strict error should reject the source");
    };
    assert!(!emission_failed);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let error = &diagnostics[0];
    assert!(error.is_error());
    assert_eq!(error.code.as_str(), "typing.unproved-range");
    assert!(error.args.iter().any(|arg| arg.name == "decision"));
    assert!(
        error
            .notes
            .iter()
            .any(|note| note.message.starts_with("fix: annotate parameter n as")),
        "{error:?}"
    );
    assert_eq!(session.revision(), 0);
    assert_eq!(answer(&session), FunctionValue::Integer(0));

    // Without `strict` on `twice` its range check stays at runtime and the
    // source is accepted; holding every definition to strict typing is then
    // refused the same way.
    session
        .replace_source(
            0,
            &strict.replace("strict function twice", "function twice"),
        )
        .unwrap();
    assert_eq!(answer(&session), FunctionValue::Integer(7));
    assert!(matches!(
        session.set_typing_config(1, TypingConfig::strict()),
        Err(SessionError::Compilation { .. })
    ));
    assert_eq!(session.revision(), 1);
    assert_eq!(session.typing_config(), TypingConfig::soft());
    assert!(session.artifacts().check_runnable().is_ok());
    assert_eq!(answer(&session), FunctionValue::Integer(7));
}

#[test]
fn claim_reports_carry_the_revision_and_show_contract_narrowing_moving_checks() {
    let source = "
define function digit(n)
  as(<u32>, n)
end;
define function answer()
  as(<integer>, digit(7))
end;
";
    let mut session = Session::new("claims.loko", source).unwrap();
    let definition = session
        .artifacts()
        .sem
        .functions
        .iter()
        .find(|function| function.name == "digit")
        .unwrap()
        .definition;
    let id = format!("claims:{definition}");
    let before = session.claims(&id, ClaimDetail::Summary).unwrap();
    assert_eq!(before.revision, Some(0));
    assert!(session.is_current(&before));
    assert_eq!(before.tally.checked, 1);
    // The program asked for the conversion with `as`, so it is not lost
    // precision, even though `n` has no written type.
    let site = before.residual_sites().next().unwrap();
    assert_eq!(
        site.residual.as_ref().map(|residual| residual.cause),
        Some(ResidualCause::Requested(CheckOrigin::Explicit))
    );

    let target = AnnotationTarget::Binding {
        definition,
        name: "n".into(),
        occurrence: 0,
    };
    let digit = "limited(<integer>, min: 0, max: 9)";
    let preview = session.preview_annotation(&target, digit).unwrap();
    assert!(preview.delta.changes_specification());
    assert_eq!(preview.delta.obligations().moved_to_callers, 1);
    session
        .mutate(
            0,
            SessionMutation::Annotate {
                target,
                type_source: digit.into(),
            },
        )
        .unwrap();

    // The edit makes the held report stale; the new one shows the check
    // proved in `digit` and the contract callers now satisfy.
    assert!(!session.is_current(&before));
    let after = session.claims(&id, ClaimDetail::Summary).unwrap();
    assert_eq!(after.revision, Some(1));
    assert_eq!((after.tally.checked, after.tally.relied), (0, 1));
    assert_eq!(
        after
            .contract
            .as_ref()
            .map(|contract| contract.params.clone()),
        Some(vec![format!("n: {digit}")])
    );
    assert_eq!(session.claims("generic:g99", ClaimDetail::Summary), None);
}
