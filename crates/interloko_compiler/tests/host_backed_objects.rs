//! Rust-backed objects reaching Loko code compiled through a session.
//!
//! `SessionArtifacts::call_function` only carries scalar values, so these
//! tests run the session's verified tape with an `InterlokoHost` that holds
//! the Rust objects, the same way an embedder passes host objects.

use execution_tape::value::{FuncRef, Value};
use execution_tape::vm::{InvocationOptions, Limits, Vm};
use interloko_compiler::sem::{
    HostClass, HostMetaRegistry, HostSlot, SlotId, ValueKind as SemValueKind,
};
use interloko_compiler::session::{CompilerSession, RuntimeMode};
use interloko_runtime::{
    HostObjectAdapter, InterlokoHost, RuntimeError, ValueKind as RuntimeValueKind,
};

#[derive(Debug)]
struct RustCircle {
    radius: i64,
}

impl HostObjectAdapter for RustCircle {
    fn read_slot(&self, slot: SlotId, kind: RuntimeValueKind) -> Result<Value, RuntimeError> {
        match (slot.0, kind) {
            (0, RuntimeValueKind::I64) => Ok(Value::I64(self.radius)),
            _ => Err(RuntimeError::HostAdapterReadFailed { slot }),
        }
    }
}

#[derive(Debug)]
struct RustSquare {
    side: i64,
}

impl HostObjectAdapter for RustSquare {
    fn read_slot(&self, slot: SlotId, kind: RuntimeValueKind) -> Result<Value, RuntimeError> {
        match (slot.0, kind) {
            (0, RuntimeValueKind::I64) => Ok(Value::I64(self.side)),
            _ => Err(RuntimeError::HostAdapterReadFailed { slot }),
        }
    }
}

fn host_registry() -> HostMetaRegistry {
    let mut registry = HostMetaRegistry::new();
    registry.add_class(HostClass::new("<rust-circle>").with_slot(HostSlot::new(
        "radius",
        SemValueKind::I64,
        "<integer>",
    )));
    registry.add_class(HostClass::new("<rust-square>").with_slot(HostSlot::new(
        "side",
        SemValueKind::I64,
        "<integer>",
    )));
    registry
}

/// How the Rust object is handed to the Loko function.
#[derive(Clone, Copy)]
enum Pass {
    /// As its own host-backed class.
    Typed,
    /// Boxed as `<object>`.
    Boxed,
}

/// Compiles `source`, installs `adapter` as an instance of `class_name`, and
/// returns the integer result of calling `function` with it.
fn call_with_host_object(
    source: &str,
    function: &str,
    class_name: &str,
    adapter: impl HostObjectAdapter + 'static,
    pass: Pass,
) -> i64 {
    let artifacts = CompilerSession::compile_source_with_host_registry(
        "host.loko",
        source,
        RuntimeMode::CompileOnly,
        &host_registry(),
    );
    assert!(!artifacts.has_errors(), "{:?}", artifacts.diagnostics);
    let tape = artifacts.tape.as_ref().expect("tape should emit");
    let entry = artifacts
        .sem
        .functions
        .iter()
        .find(|candidate| candidate.name == function)
        .and_then(|candidate| tape.functions.get(candidate.id.0 as usize))
        .copied()
        .expect("function should have a tape entry");

    let mut host = InterlokoHost::new(tape.runtime.clone());
    let class = tape
        .runtime
        .class_id(class_name)
        .expect("host class should be in runtime layout");
    let mut argument = host
        .push_host_object(class, adapter)
        .expect("host object should install");
    if matches!(pass, Pass::Boxed) {
        argument = host
            .box_top_value(argument)
            .expect("host object should box as <object>");
    }

    let mut vm = Vm::new(host, Limits::default());
    let unit = vm
        .install(std::sync::Arc::clone(&tape.program))
        .expect("program should install");
    let args = [argument];
    let outcome = interloko_runtime::driver::run(
        &mut vm,
        &tape.control,
        FuncRef::new(unit, entry),
        &args,
        InvocationOptions::default(),
    )
    .expect("function should run");
    match outcome.outputs.as_slice() {
        [Value::I64(value)] => *value,
        other => panic!("expected one integer result, got {other:?}"),
    }
}

#[test]
fn reads_host_backed_slots() {
    let value = call_with_host_object(
        "define function radius(c :: <rust-circle>) => <integer>\n  c.radius\nend;",
        "radius",
        "<rust-circle>",
        RustCircle { radius: 11 },
        Pass::Typed,
    );
    assert_eq!(value, 11);
}

#[test]
fn dispatches_on_host_backed_objects() {
    let value = call_with_host_object(
        "define generic area(x :: <object>) => <integer>;\n\
         define method area(c :: <rust-circle>) => <integer>\n  c.radius * c.radius\nend;\n\
         define function call-area(c :: <rust-circle>) => <integer>\n  area(c)\nend;",
        "call-area",
        "<rust-circle>",
        RustCircle { radius: 7 },
        Pass::Typed,
    );
    assert_eq!(value, 49);
}

#[test]
fn host_backed_objects_flow_through_object_top_tests_and_casts() {
    const GUARDED: &str = "define function guarded(x :: <object>) => <integer>\n\
                             if instance?(x, <rust-circle>) then as(<rust-circle>, x).radius else 0 end\n\
                           end;";

    let circle = call_with_host_object(
        GUARDED,
        "guarded",
        "<rust-circle>",
        RustCircle { radius: 5 },
        Pass::Boxed,
    );
    assert_eq!(circle, 5);

    let square = call_with_host_object(
        GUARDED,
        "guarded",
        "<rust-square>",
        RustSquare { side: 9 },
        Pass::Boxed,
    );
    assert_eq!(square, 0);
}
