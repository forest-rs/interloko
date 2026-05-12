//! Lowering from semantic Cairn IR to `execution_tape`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::asm::{Asm, FunctionSig, ProgramBuilder};
use execution_tape::program::{SpanId, ValueType};
use execution_tape::value::FuncId;
use execution_tape::verifier::VerifiedProgram;

use crate::diagnostic::Diagnostic;
use crate::sem;
use crate::syntax::Span;

/// A verified tape program emitted from a semantic module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TapeModule {
    /// Verified executable tape program.
    pub program: VerifiedProgram,
    /// `execution_tape` function id for `main`, if present.
    pub main: Option<FuncId>,
}

/// Tape emission error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TapeError {
    /// Earlier compiler diagnostics prevented emission.
    Diagnostics {
        /// Diagnostics that must be reported to the caller.
        diagnostics: Vec<Diagnostic>,
    },
    /// Semantic IR contained an invalid placeholder.
    InvalidSemantic {
        /// Error details.
        message: String,
    },
    /// `execution_tape` rejected the generated program.
    Build {
        /// Builder or verifier details.
        message: String,
    },
}

/// Emits a verified `execution_tape` program for a semantic module.
///
/// All milestone-0 values are currently `I64`; every function returns one `I64`.
pub fn emit(module: &sem::Module) -> Result<TapeModule, TapeError> {
    if !module.diagnostics.is_empty() {
        return Err(TapeError::Diagnostics {
            diagnostics: module.diagnostics.clone(),
        });
    }

    let mut builder = ProgramBuilder::new();
    builder.set_program_name("cairn");

    let mut tape_functions = Vec::new();
    let mut main = None;
    for function in &module.functions {
        let func = builder.declare_function(FunctionSig {
            arg_types: vec![ValueType::I64; function.params.len()],
            ret_types: vec![ValueType::I64],
        });
        builder
            .set_function_name(func, &function.name)
            .map_err(build_error)?;
        for (index, local) in function
            .params
            .iter()
            .filter_map(|id| function.locals.get(id.0 as usize))
            .enumerate()
        {
            builder
                .set_function_input_name(
                    func,
                    u32::try_from(index).unwrap_or(u32::MAX),
                    &local.name,
                )
                .map_err(build_error)?;
        }
        builder
            .set_function_output_name(func, 0, "result")
            .map_err(build_error)?;
        if function.name == "main" {
            main = Some(func);
        }
        tape_functions.push(func);
    }

    for function in &module.functions {
        let mut emitter = FunctionEmitter::new(function, &tape_functions)?;
        emitter.emit_block(&function.body)?;
        let func = tape_functions[function.id.0 as usize];
        builder
            .define_function(func, emitter.finish())
            .map_err(build_error)?;
    }

    let program = builder.build_verified().map_err(build_error)?;
    Ok(TapeModule { program, main })
}

fn build_error(error: impl core::fmt::Debug) -> TapeError {
    TapeError::Build {
        message: format!("{error:?}"),
    }
}

struct FunctionEmitter<'module> {
    asm: Asm,
    local_regs: Vec<Option<u32>>,
    functions: &'module [FuncId],
    next_reg: u32,
}

impl<'module> FunctionEmitter<'module> {
    fn new(function: &sem::Function, functions: &'module [FuncId]) -> Result<Self, TapeError> {
        let mut local_regs = vec![None; function.locals.len()];
        for (index, local) in function.params.iter().enumerate() {
            let reg = u32::try_from(index + 1).unwrap_or(u32::MAX);
            if let Some(slot) = local_regs.get_mut(local.0 as usize) {
                *slot = Some(reg);
            }
        }
        let next_reg = u32::try_from(function.params.len() + 1).unwrap_or(u32::MAX);
        Ok(Self {
            asm: Asm::new(),
            local_regs,
            functions,
            next_reg,
        })
    }

    fn finish(self) -> Asm {
        self.asm
    }

    fn emit_block(&mut self, block: &sem::Block) -> Result<(), TapeError> {
        for let_binding in &block.lets {
            let value = self.emit_expr(&let_binding.value)?;
            let Some(slot) = self.local_regs.get_mut(let_binding.local.0 as usize) else {
                return Err(invalid_semantic("local id is out of range"));
            };
            *slot = Some(value);
        }

        let result = self.emit_expr(&block.result)?;
        self.set_span(block.result.span());
        self.asm.ret(0, &[result]);
        Ok(())
    }

    fn emit_expr(&mut self, expr: &sem::Expr) -> Result<u32, TapeError> {
        match expr {
            sem::Expr::ConstI64 { value, span } => {
                let reg = self.alloc_reg();
                self.set_span(*span);
                self.asm.const_i64(reg, *value);
                Ok(reg)
            }
            sem::Expr::Local { local, .. } => self
                .local_regs
                .get(local.0 as usize)
                .and_then(|reg| *reg)
                .ok_or_else(|| invalid_semantic("local was read before assignment")),
            sem::Expr::DirectCall {
                function,
                args,
                span,
            } => {
                let mut arg_regs = Vec::with_capacity(args.len());
                for arg in args {
                    arg_regs.push(self.emit_expr(arg)?);
                }
                let Some(func) = self.functions.get(function.0 as usize).copied() else {
                    return Err(invalid_semantic("function id is out of range"));
                };
                let result = self.alloc_reg();
                self.set_span(*span);
                self.asm.call(0, func, 0, &arg_regs, &[result]);
                Ok(result)
            }
            sem::Expr::Binary { op, lhs, rhs, span } => {
                let lhs = self.emit_expr(lhs)?;
                let rhs = self.emit_expr(rhs)?;
                let result = self.alloc_reg();
                self.set_span(*span);
                match op {
                    sem::BinaryOp::Add => {
                        self.asm.i64_add(result, lhs, rhs);
                    }
                    sem::BinaryOp::Sub => {
                        self.asm.i64_sub(result, lhs, rhs);
                    }
                    sem::BinaryOp::Mul => {
                        self.asm.i64_mul(result, lhs, rhs);
                    }
                }
                Ok(result)
            }
            sem::Expr::Error { .. } => Err(invalid_semantic("semantic expression is invalid")),
        }
    }

    fn alloc_reg(&mut self) -> u32 {
        let reg = self.next_reg;
        self.next_reg = self.next_reg.saturating_add(1);
        reg
    }

    fn set_span(&mut self, span: Span) {
        if let Some(span_id) = span_id(span) {
            self.asm.span(span_id);
        }
    }
}

fn invalid_semantic(message: &str) -> TapeError {
    TapeError::InvalidSemantic {
        message: message.into(),
    }
}

fn span_id(span: Span) -> Option<SpanId> {
    let start = u64::try_from(span.start).unwrap_or(u64::MAX - 1);
    SpanId::try_from(start.saturating_add(1)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, sem, syntax};

    fn lower_source(source: &str) -> sem::Module {
        let tree = syntax::parse(source);
        let ast = ast::lower(&tree);
        sem::lower(&ast)
    }

    #[test]
    fn emits_verified_tape_for_direct_calls_and_arithmetic() {
        let module = lower_source(
            "define function add(a, b)\n  a + b\nend;\n\
             define function main()\n  add(40, 2)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        assert_eq!(tape.program.program().functions.len(), 2);
        assert_eq!(tape.main, Some(FuncId(1)));
    }
}
