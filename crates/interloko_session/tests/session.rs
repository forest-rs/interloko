//! Embedding contracts across accepted edits and explicit host behavior.

use execution_tape::trace::TraceMask;
use interloko_compiler::sem::{HostFunction, HostMetaRegistry, HostValueKind};
use interloko_compiler::session::{
    FunctionCallError, FunctionValue, HostFunctionFailure, HostFunctionHandler, SessionMutation,
};
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
    session.call("answer", &[], TraceMask::NONE).unwrap().value
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
        session.call("answer", &[], TraceMask::NONE),
        Err(FunctionCallError::HostFunctionFailed { .. })
    ));
    let result = session
        .call_with_host("answer", &[], TraceMask::HOST, &mut Increment)
        .unwrap();
    assert_eq!(result.value, FunctionValue::Integer(42));
    assert!(!result.trace.unwrap().events.is_empty());
    assert_eq!(session.revision(), 0);

    session
        .replace_source(0, "define function answer() increment(99) end;")
        .unwrap();
    let result = session
        .call_with_host("answer", &[], TraceMask::NONE, &mut Increment)
        .unwrap();
    assert_eq!(result.value, FunctionValue::Integer(100));
    assert_eq!(session.revision(), 1);
}
