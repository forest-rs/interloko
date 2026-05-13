//! End-to-end milestone-0 compiler driver helpers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use execution_tape::trace::TraceMask;
use execution_tape::value::FuncId;
use execution_tape::value::Value;
use execution_tape::vm::{Limits, Vm};

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::runtime::CairnHost;
use crate::sem::HostMetaRegistry;
use crate::{ast, lower, sem, syntax, tape};

/// End-to-end compiler or runtime error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    /// Compilation produced diagnostics.
    Diagnostics {
        /// Diagnostics to report.
        diagnostics: Vec<Diagnostic>,
    },
    /// Lowered IR emission failed.
    Lower {
        /// Lowering error.
        error: lower::LowerError,
    },
    /// Tape emission failed.
    Tape {
        /// Tape emission error.
        error: tape::TapeError,
    },
    /// No zero-argument `main` function was available to run.
    MissingMain,
    /// A requested function name was not found.
    MissingFunction {
        /// Requested function name.
        name: String,
    },
    /// The `main` function is present but expects arguments.
    MainHasParameters,
    /// Runtime execution trapped.
    Runtime {
        /// Trap details.
        message: String,
    },
    /// `main` returned a non-`I64` result.
    MainReturnedNonI64,
    /// Requested function returned a non-`I64` result.
    FunctionReturnedNonI64 {
        /// Requested function name.
        name: String,
    },
}

impl CompileError {
    /// Stable error code for this driver/runtime failure.
    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        match self {
            Self::Diagnostics { .. } => DiagnosticCode::Uncategorized,
            Self::Lower { error } => error.code(),
            Self::Tape { error } => error.code(),
            Self::MissingMain => DiagnosticCode::RuntimeMissingMain,
            Self::MissingFunction { .. } => DiagnosticCode::RuntimeMissingFunction,
            Self::MainHasParameters => DiagnosticCode::RuntimeMainHasParameters,
            Self::Runtime { .. } => DiagnosticCode::RuntimeTrap,
            Self::MainReturnedNonI64 => DiagnosticCode::RuntimeMainReturnedNonI64,
            Self::FunctionReturnedNonI64 { .. } => DiagnosticCode::RuntimeFunctionReturnedNonI64,
        }
    }
}

/// Fully compiled milestone-0 module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledModule {
    /// Parsed syntax tree.
    pub syntax: syntax::SyntaxTree,
    /// Surface AST.
    pub ast: ast::Module,
    /// Semantic IR.
    pub sem: sem::Module,
    /// Lowered executable IR.
    pub lowered: lower::Module,
    /// Verified tape program.
    pub tape: tape::TapeModule,
}

impl CompiledModule {
    /// Returns a stable, human-readable lowered IR dump.
    #[must_use]
    pub fn dump_lowered(&self) -> String {
        self.lowered.dump()
    }

    /// Returns a human-readable `execution_tape` disassembly.
    #[must_use]
    pub fn dump_tape(&self) -> String {
        format!(
            "{}",
            execution_tape::disasm::disassemble_verified(&self.tape.program)
        )
    }

    /// Returns the tape function id for a source direct function.
    #[must_use]
    pub fn tape_function(&self, name: &str) -> Option<FuncId> {
        let function = self
            .sem
            .functions
            .iter()
            .find(|function| function.name == name)?;
        self.tape.functions.get(function.id.0 as usize).copied()
    }

    /// Runs a source direct function with explicit arguments and a prepared host.
    pub fn run_function_i64(
        &self,
        name: &str,
        args: &[Value],
        host: CairnHost,
    ) -> Result<i64, CompileError> {
        let Some(function) = self.tape_function(name) else {
            return Err(CompileError::MissingFunction { name: name.into() });
        };
        run_tape_function_i64(&self.tape.program, function, args, host).map_err(|error| match error
        {
            CompileError::MainReturnedNonI64 => {
                CompileError::FunctionReturnedNonI64 { name: name.into() }
            }
            other => other,
        })
    }
}

/// Compiles source through syntax, AST, semantic IR, and tape emission.
pub fn compile(source: &str) -> Result<CompiledModule, CompileError> {
    compile_with_host_registry(source, &HostMetaRegistry::new())
}

/// Compiles source with host-backed metaobject declarations.
pub fn compile_with_host_registry(
    source: &str,
    registry: &HostMetaRegistry,
) -> Result<CompiledModule, CompileError> {
    let syntax = syntax::parse(source);
    let ast = ast::lower(&syntax);
    let sem = sem::lower_with_host_registry(&ast, registry);
    if !sem.diagnostics.is_empty() {
        return Err(CompileError::Diagnostics {
            diagnostics: sem.diagnostics.clone(),
        });
    }
    let lowered = lower::lower(&sem).map_err(|error| CompileError::Lower { error })?;
    let tape = tape::emit(&lowered).map_err(|error| CompileError::Tape { error })?;
    Ok(CompiledModule {
        syntax,
        ast,
        sem,
        lowered,
        tape,
    })
}

/// Compiles source and runs the zero-argument `main` function.
pub fn compile_and_run_main(source: &str) -> Result<i64, CompileError> {
    let compiled = compile(source)?;
    let Some(main) = compiled.tape.main else {
        return Err(CompileError::MissingMain);
    };
    let Some(main_sem) = compiled
        .sem
        .functions
        .iter()
        .find(|function| function.name == "main")
    else {
        return Err(CompileError::MissingMain);
    };
    if !main_sem.params.is_empty() {
        return Err(CompileError::MainHasParameters);
    }

    let host = CairnHost::new(compiled.tape.runtime.clone());
    run_tape_function_i64(&compiled.tape.program, main, &[], host)
}

fn run_tape_function_i64(
    program: &execution_tape::verifier::VerifiedProgram,
    function: FuncId,
    args: &[Value],
    host: CairnHost,
) -> Result<i64, CompileError> {
    let mut vm = Vm::new(host, Limits::default());
    let output = vm
        .run(program, function, args, TraceMask::NONE, None)
        .map_err(|trap| CompileError::Runtime {
            message: format!("{trap:?}"),
        })?;

    match output.as_slice() {
        [Value::I64(value)] => Ok(*value),
        _ => Err(CompileError::MainReturnedNonI64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::HostObjectAdapter;
    use crate::sem::{HostClass, HostMetaRegistry, HostSlot, SlotId, ValueKind as SemValueKind};
    use execution_tape::host::HostError;

    #[derive(Debug)]
    struct RustCircle {
        radius: i64,
    }

    impl HostObjectAdapter for RustCircle {
        fn read_slot(&self, slot: SlotId, kind: SemValueKind) -> Result<Value, HostError> {
            match (slot.0, kind) {
                (0, SemValueKind::I64) => Ok(Value::I64(self.radius)),
                _ => Err(HostError::Failed),
            }
        }
    }

    #[derive(Debug)]
    struct RustSquare {
        side: i64,
    }

    impl HostObjectAdapter for RustSquare {
        fn read_slot(&self, slot: SlotId, kind: SemValueKind) -> Result<Value, HostError> {
            match (slot.0, kind) {
                (0, SemValueKind::I64) => Ok(Value::I64(self.side)),
                _ => Err(HostError::Failed),
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

    #[test]
    fn compiles_and_runs_main_returning_42() {
        let value = compile_and_run_main(
            "define function forty-two()\n  40 + 2\nend;\n\
             define function main()\n  forty-two()\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 42);
    }

    #[test]
    fn compiles_and_runs_generic_area_dispatch() {
        let value = compile_and_run_main(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 100);
    }

    #[test]
    fn compiles_and_runs_slot_init_value_defaults_and_overrides() {
        let value = compile_and_run_main(
            "define class <circle> (<object>)\n  constant slot radius :: <integer>, required-init-keyword: r:, init-value: 10;\nend;\n\
             define function main()\n  make(<circle>).radius + make(<circle>, r: 2).radius\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 12);
    }

    #[test]
    fn compiles_and_runs_recoverable_restart() {
        let value = compile_and_run_main(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 42);
    }

    #[test]
    fn compiles_and_runs_mixed_typed_slots() {
        let value = compile_and_run_main(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <box> (<object>)\n  slot item :: <circle>;\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  let b = make(<box>, item: make(<circle>, radius: 7), valid?: #t);\n  if b.valid? then b.item.radius else 0 end\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 7);
    }

    #[test]
    fn compiles_and_runs_nominal_subtype_flow() {
        let value = compile_and_run_main(
            "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 5;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 7;\nend;\n\
             define class <holder> (<object>)\n  slot item :: <shape>;\nend;\n\
             define function hold(s :: <shape>) => <shape>\n  s\nend;\n\
             define function build() => <shape>\n  make(<circle>)\nend;\n\
             define generic describe(x :: <shape>) => <integer>;\n\
             define method describe(s :: <shape>) => <integer>\n  s.size\nend;\n\
             define function main()\n  hold(make(<circle>)).size + build().size + make(<holder>, item: make(<circle>)).item.size + describe(make(<circle>))\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 20);
    }

    #[test]
    fn compiles_and_runs_most_specific_subclass_dispatch() {
        let value = compile_and_run_main(
            "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
             define generic score(x :: <shape>) => <integer>;\n\
             define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
             define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
             define function main()\n  score(make(<circle>))\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 2);
    }

    #[test]
    fn compiles_and_runs_subclass_conditions_for_parent_restarts() {
        let value = compile_and_run_main(
            "define class <problem> (<object>)\n  slot fallback :: <integer>, init-value: 11;\nend;\n\
             define class <invalid-radius> (<problem>)\nend;\n\
             define function main()\n  handle signal(make(<invalid-radius>)) with restart e :: <problem> e.fallback end\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 11);
    }

    #[test]
    fn compiles_and_runs_object_top_slot_reads() {
        let error = compile_and_run_main(
            "define class <box> (<object>)\n  slot value;\nend;\n\
             define function main()\n  make(<box>, value: 7).value\nend;",
        )
        .expect_err("main returns a boxed <object>, not an integer");
        assert_eq!(error, CompileError::MainReturnedNonI64);
    }

    #[test]
    fn compiles_and_runs_instance_refinement_and_casts() {
        let value = compile_and_run_main(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function radius-or-zero(x :: <object>) => <integer>\n  if instance?(x, <circle>) then x.radius else 0 end\nend;\n\
             define function radius(x :: <object>) => <integer>\n  as(<circle>, x).radius\nend;\n\
             define function main()\n  radius-or-zero(make(<circle>, radius: 8)) + radius(make(<circle>, radius: 9))\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 17);
    }

    #[test]
    fn compiles_and_runs_known_callable_apply() {
        let value = compile_and_run_main(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function inc(x)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  let g = area;\n  apply(f, 2) + apply(g, make(<circle>, radius: 4))\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 19);
    }

    #[test]
    fn compiles_and_reads_host_backed_slots() {
        let registry = host_registry();
        let compiled = compile_with_host_registry(
            "define function radius(c :: <rust-circle>) => <integer>\n  c.radius\nend;",
            &registry,
        )
        .expect("program should compile");
        let mut host = CairnHost::new(compiled.tape.runtime.clone());
        let class = compiled
            .tape
            .runtime
            .class_id("<rust-circle>")
            .expect("host class should be in runtime layout");
        let circle = host
            .push_host_object(class, RustCircle { radius: 11 })
            .expect("host object should install");

        let value = compiled
            .run_function_i64("radius", &[circle], host)
            .expect("host-backed slot read should run");
        assert_eq!(value, 11);
    }

    #[test]
    fn compiles_and_dispatches_on_host_backed_objects() {
        let registry = host_registry();
        let compiled = compile_with_host_registry(
            "define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <rust-circle>) => <integer>\n  c.radius * c.radius\nend;\n\
             define function call-area(c :: <rust-circle>) => <integer>\n  area(c)\nend;",
            &registry,
        )
        .expect("program should compile");
        let mut host = CairnHost::new(compiled.tape.runtime.clone());
        let class = compiled
            .tape
            .runtime
            .class_id("<rust-circle>")
            .expect("host class should be in runtime layout");
        let circle = host
            .push_host_object(class, RustCircle { radius: 7 })
            .expect("host object should install");

        let value = compiled
            .run_function_i64("call-area", &[circle], host)
            .expect("host-backed generic dispatch should run");
        assert_eq!(value, 49);
    }

    #[test]
    fn host_backed_objects_work_through_object_top_tests_and_casts() {
        let registry = host_registry();
        let compiled = compile_with_host_registry(
            "define function guarded(x :: <object>) => <integer>\n\
               if instance?(x, <rust-circle>) then as(<rust-circle>, x).radius else 0 end\n\
             end;",
            &registry,
        )
        .expect("program should compile");
        let mut host = CairnHost::new(compiled.tape.runtime.clone());
        let class = compiled
            .tape
            .runtime
            .class_id("<rust-circle>")
            .expect("host class should be in runtime layout");
        let circle = host
            .push_host_object(class, RustCircle { radius: 5 })
            .expect("host object should install");
        let boxed = host
            .box_top_value(circle)
            .expect("host object should box as <object>");

        let value = compiled
            .run_function_i64("guarded", &[boxed], host)
            .expect("host-backed object-top flow should run");
        assert_eq!(value, 5);

        let mut host = CairnHost::new(compiled.tape.runtime.clone());
        let class = compiled
            .tape
            .runtime
            .class_id("<rust-square>")
            .expect("host class should be in runtime layout");
        let square = host
            .push_host_object(class, RustSquare { side: 9 })
            .expect("host object should install");
        let boxed = host
            .box_top_value(square)
            .expect("host object should box as <object>");
        let value = compiled
            .run_function_i64("guarded", &[boxed], host)
            .expect("host-backed negative instance? should run");
        assert_eq!(value, 0);
    }

    #[test]
    fn reports_invalid_host_backed_slot_reads_and_casts() {
        let registry = host_registry();
        let bad_slot = compile_with_host_registry(
            "define function bad(c :: <rust-circle>) => <integer>\n  c.side\nend;",
            &registry,
        )
        .expect_err("unknown host-backed slot should be rejected");
        assert!(matches!(
            bad_slot,
            CompileError::Diagnostics { diagnostics }
                if diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == DiagnosticCode::UnknownSlot
                        && diagnostic.args.iter().any(|arg| {
                            arg.name == "class" && arg.value.render() == "<rust-circle>"
                        })
                        && diagnostic.args.iter().any(|arg| {
                            arg.name == "slot" && arg.value.render() == "side"
                        })
                })
        ));

        let bad_cast = compile_with_host_registry(
            "define function bad(s :: <rust-square>) => <integer>\n  as(<rust-circle>, s).radius\nend;",
            &registry,
        )
        .expect_err("impossible host-backed cast should be rejected");
        assert!(matches!(
            bad_cast,
            CompileError::Diagnostics { diagnostics }
                if diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == DiagnosticCode::CastImpossible
                        && diagnostic.args.iter().any(|arg| {
                            arg.name == "actual" && arg.value.render() == "<rust-square>"
                        })
                        && diagnostic.args.iter().any(|arg| {
                            arg.name == "expected" && arg.value.render() == "<rust-circle>"
                        })
                })
        ));
    }

    #[test]
    fn compiles_and_runs_rich_value_literals_slots_and_dispatch() {
        let value = compile_and_run_main(
            "define class <sample> (<object>)\n\
               slot label :: <string>;\n\
               slot tag :: <symbol>;\n\
               slot option :: <symbol>;\n\
               slot items :: <vector>;\n\
             end;\n\
             define generic score(x :: <object>) => <integer>;\n\
             define method score(x :: <string>) => <integer>\n\
               if x = \"direct\" then 4 else 0 end\n\
             end;\n\
             define method score(x :: <sample>) => <integer>\n\
               let label-score = if x.label = \"box\" then 3 else 0 end;\n\
               let tag-score = if x.tag = #ready then 2 else 0 end;\n\
               let option-score = if x.option = fallback: then 1 else 0 end;\n\
               label-score + tag-score + option-score\n\
             end;\n\
             define function main()\n\
               let sample = make(<sample>, label: \"box\", tag: #ready, option: fallback:, items: #[1, 2]);\n\
               score(\"direct\") + score(sample)\n\
             end;",
        )
        .expect("rich value program should run");

        assert_eq!(value, 10);
    }

    #[test]
    fn compiles_and_runs_integer_generic_dispatch() {
        let value = compile_and_run_main(
            "define generic score(x :: <integer>) => <integer>;\n\
             define method score(x :: <integer>) => <integer>\n\
               x + 1\n\
             end;\n\
             define function main()\n\
               score(41)\n\
             end;",
        )
        .expect("integer generic program should run");

        assert_eq!(value, 42);
    }

    #[test]
    fn reports_malformed_strings_and_unsupported_vector_equality() {
        let malformed = compile("define function main()\n  \"unterminated\nend;")
            .expect_err("malformed string should fail");
        assert!(matches!(
            malformed,
            CompileError::Diagnostics { diagnostics }
                if diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == DiagnosticCode::AstMalformedStringLiteral)
        ));

        let vector_eq = compile("define function main()\n  #[1, 2] = #[1, 2]\nend;")
            .expect_err("vector equality should be rejected");
        assert!(matches!(
            vector_eq,
            CompileError::Diagnostics { diagnostics }
                if diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == DiagnosticCode::ComparisonOperandTypeMismatch
                })
        ));
    }

    #[test]
    fn compiled_module_exposes_lowered_and_tape_dumps() {
        let compiled = compile(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        )
        .expect("program should compile");

        let lowered = compiled.dump_lowered();
        assert!(lowered.contains("area#dispatch"));
        assert!(lowered.contains("slot.read"));

        let tape = compiled.dump_tape();
        assert!(tape.contains("; program=\"cairn\""));
        assert!(tape.contains("name=\"area#dispatch\""));
    }
}
