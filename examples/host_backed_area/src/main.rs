//! Host-backed object Cairn example.

use execution_tape::host::HostError;
use execution_tape::value::Value;
use interloko_compiler::driver::{CompileError, compile_with_host_registry};
use interloko_compiler::runtime::{
    HostObjectAdapter, InterlokoHost, ValueKind as RuntimeValueKind,
};
use interloko_compiler::sem::{
    HostClass, HostMetaRegistry, HostSlot, SlotId, ValueKind as SemValueKind,
};

fn main() -> Result<(), CompileError> {
    let source = include_str!("../programs/host_backed_area.cairn");
    let registry = host_registry();
    let compiled = compile_with_host_registry(source, &registry)?;

    match dump_mode().as_deref() {
        None => {
            let mut host = InterlokoHost::new(compiled.tape.runtime.clone());
            let circle = host_object(
                &compiled,
                &mut host,
                "<rust-circle>",
                RustCircle { radius: 7 },
            );
            let square = host_object(
                &compiled,
                &mut host,
                "<rust-square>",
                RustSquare { side: 5 },
            );
            let value = compiled.run_function_i64("sum-areas", &[circle, square], host)?;
            assert_eq!(value, 74);
            println!("{value}");
        }
        Some("--dump-lowered") => {
            print!("{}", compiled.dump_lowered());
        }
        Some("--dump-tape") => {
            print!("{}", compiled.dump_tape());
        }
        Some("--dump-all") => {
            print!("{}", compiled.dump_lowered());
            println!();
            print!("{}", compiled.dump_tape());
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

fn host_object(
    compiled: &interloko_compiler::driver::CompiledModule,
    host: &mut InterlokoHost,
    class_name: &str,
    adapter: impl HostObjectAdapter + 'static,
) -> Value {
    let class = compiled
        .tape
        .runtime
        .class_id(class_name)
        .expect("host-backed class should be in runtime layout");
    host.push_host_object(class, adapter)
        .expect("host-backed object should install")
}

#[derive(Debug)]
struct RustCircle {
    radius: i64,
}

impl HostObjectAdapter for RustCircle {
    fn read_slot(&self, slot: SlotId, kind: RuntimeValueKind) -> Result<Value, HostError> {
        match (slot.0, kind) {
            (0, RuntimeValueKind::I64) => Ok(Value::I64(self.radius)),
            _ => Err(HostError::Failed),
        }
    }
}

#[derive(Debug)]
struct RustSquare {
    side: i64,
}

impl HostObjectAdapter for RustSquare {
    fn read_slot(&self, slot: SlotId, kind: RuntimeValueKind) -> Result<Value, HostError> {
        match (slot.0, kind) {
            (0, RuntimeValueKind::I64) => Ok(Value::I64(self.side)),
            _ => Err(HostError::Failed),
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
