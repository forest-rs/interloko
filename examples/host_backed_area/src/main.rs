//! Host-backed object Interloko example.
//!
//! Rust structs stand behind the Loko classes `<rust-circle>` and
//! `<rust-square>`. The compiler session learns their shape from a
//! [`HostMetaRegistry`]; at run time each object is installed in the
//! `InterlokoHost` through a [`HostObjectAdapter`] that answers slot reads.
//!
//! `SessionArtifacts::call_function` only passes scalar values, so calling a
//! function with host objects runs the session's verified tape directly.

use std::error::Error;

use execution_tape::trace::TraceMask;
use execution_tape::value::Value;
use execution_tape::vm::{Limits, Vm};
use interloko_compiler::sem::{
    HostClass, HostMetaRegistry, HostSlot, SlotId, ValueKind as SemValueKind,
};
use interloko_compiler::session::{CompilerSession, DumpStage, RuntimeMode, SessionArtifacts};
use interloko_runtime::{
    HostObjectAdapter, InterlokoHost, RuntimeError, ValueKind as RuntimeValueKind,
};

const NAME: &str = "host_backed_area.loko";
const SOURCE: &str = include_str!("../programs/host_backed_area.loko");

fn main() -> Result<(), Box<dyn Error>> {
    let artifacts = CompilerSession::compile_source_with_host_registry(
        NAME,
        SOURCE,
        RuntimeMode::CompileOnly,
        &host_registry(),
    );
    if artifacts.has_errors() {
        let codes = artifacts
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .map(|diagnostic| diagnostic.code.to_string())
            .collect::<Vec<_>>();
        return Err(format!("{NAME} did not compile: {}", codes.join(", ")).into());
    }

    match dump_mode().as_deref() {
        None => {
            let value = sum_areas(&artifacts, RustCircle { radius: 7 }, RustSquare { side: 5 })?;
            assert_eq!(value, 74);
            println!("{value}");
        }
        Some("--dump-lowered") => print!("{}", artifacts.dump_stage(DumpStage::Lowered)),
        Some("--dump-tape") => print!("{}", artifacts.dump_stage(DumpStage::Tape)),
        Some("--dump-all") => {
            print!("{}", artifacts.dump_stage(DumpStage::Lowered));
            println!();
            print!("{}", artifacts.dump_stage(DumpStage::Tape));
        }
        Some(_) => {
            eprintln!(
                "usage: cargo run -p interloko_host_backed_area_example -- [--dump-lowered|--dump-tape|--dump-all]"
            );
            std::process::exit(2);
        }
    }

    Ok(())
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

/// Runs Loko `sum-areas` on two Rust-backed objects.
fn sum_areas(
    artifacts: &SessionArtifacts,
    circle: RustCircle,
    square: RustSquare,
) -> Result<i64, Box<dyn Error>> {
    let tape = artifacts.tape.as_ref().ok_or("tape unavailable")?;
    let entry = artifacts
        .sem
        .functions
        .iter()
        .find(|function| function.name == "sum-areas")
        .and_then(|function| tape.functions.get(function.id.0 as usize))
        .ok_or("missing function `sum-areas`")?;

    let layout = &tape.runtime;
    let circle_class = layout
        .class_id("<rust-circle>")
        .ok_or("missing `<rust-circle>`")?;
    let square_class = layout
        .class_id("<rust-square>")
        .ok_or("missing `<rust-square>`")?;
    let mut host = InterlokoHost::new(layout.clone());
    let circle = host
        .push_host_object(circle_class, circle)
        .map_err(|error| format!("cannot install `<rust-circle>`: {error:?}"))?;
    let square = host
        .push_host_object(square_class, square)
        .map_err(|error| format!("cannot install `<rust-square>`: {error:?}"))?;

    let mut vm = Vm::new(host, Limits::default());
    match vm
        .run(
            &tape.program,
            *entry,
            &[circle, square],
            TraceMask::NONE,
            None,
        )?
        .as_slice()
    {
        [Value::I64(value)] => Ok(*value),
        other => Err(format!("`sum-areas` returned {other:?}").into()),
    }
}

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

fn dump_mode() -> Option<String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    if args.next().is_some() {
        return Some(String::new());
    }
    mode
}
