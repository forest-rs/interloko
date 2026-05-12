//! End-to-end milestone-0 compiler driver helpers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use execution_tape::host::{AccessSink, Host, HostError, SigHash, ValueRef};
use execution_tape::trace::TraceMask;
use execution_tape::value::Value;
use execution_tape::vm::{Limits, Vm};

use crate::diagnostic::Diagnostic;
use crate::{ast, sem, syntax, tape};

/// End-to-end compiler or runtime error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    /// Compilation produced diagnostics.
    Diagnostics {
        /// Diagnostics to report.
        diagnostics: Vec<Diagnostic>,
    },
    /// Tape emission failed.
    Tape {
        /// Tape emission error.
        error: tape::TapeError,
    },
    /// No zero-argument `main` function was available to run.
    MissingMain,
    /// The `main` function is present but expects arguments.
    MainHasParameters,
    /// Runtime execution trapped.
    Runtime {
        /// Trap details.
        message: String,
    },
    /// `main` returned a non-`I64` result.
    MainReturnedNonI64,
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
    /// Verified tape program.
    pub tape: tape::TapeModule,
}

/// Compiles source through syntax, AST, semantic IR, and tape emission.
pub fn compile(source: &str) -> Result<CompiledModule, CompileError> {
    let syntax = syntax::parse(source);
    let ast = ast::lower(&syntax);
    let sem = sem::lower(&ast);
    if !sem.diagnostics.is_empty() {
        return Err(CompileError::Diagnostics {
            diagnostics: sem.diagnostics.clone(),
        });
    }
    let tape = tape::emit(&sem).map_err(|error| CompileError::Tape { error })?;
    Ok(CompiledModule {
        syntax,
        ast,
        sem,
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

    let mut vm = Vm::new(NoHost, Limits::default());
    let output = vm
        .run(&compiled.tape.program, main, &[], TraceMask::NONE, None)
        .map_err(|trap| CompileError::Runtime {
            message: format!("{trap:?}"),
        })?;

    match output.as_slice() {
        [Value::I64(value)] => Ok(*value),
        _ => Err(CompileError::MainReturnedNonI64),
    }
}

struct NoHost;

impl Host for NoHost {
    fn call(
        &mut self,
        _symbol: &str,
        _sig_hash: SigHash,
        _args: &[ValueRef<'_>],
        _rets: &mut [Value],
        _access: Option<&mut dyn AccessSink>,
    ) -> Result<u64, HostError> {
        Err(HostError::UnknownSymbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_runs_main_returning_42() {
        let value = compile_and_run_main(
            "define function forty-two()\n  40 + 2\nend;\n\
             define function main()\n  forty-two()\nend;",
        )
        .expect("program should compile and run");
        assert_eq!(value, 42);
    }
}
