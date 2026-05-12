//! Lowering from semantic Cairn IR to `execution_tape`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::asm::{Asm, FunctionSig, ProgramBuilder};
use execution_tape::program::{HostSigId, SpanId, ValueType};
use execution_tape::value::FuncId;
use execution_tape::verifier::VerifiedProgram;

use crate::diagnostic::Diagnostic;
use crate::runtime::{self, RuntimeLayout};
use crate::sem;
use crate::syntax::Span;

/// A verified tape program emitted from a semantic module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TapeModule {
    /// Verified executable tape program.
    pub program: VerifiedProgram,
    /// Runtime layout required by the Cairn host.
    pub runtime: RuntimeLayout,
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
/// Direct functions and methods return one `I64`. Generic calls lower to
/// generated dispatcher functions that use ordinary branch/call bytecode.
pub fn emit(module: &sem::Module) -> Result<TapeModule, TapeError> {
    if !module.diagnostics.is_empty() {
        return Err(TapeError::Diagnostics {
            diagnostics: module.diagnostics.clone(),
        });
    }

    let mut builder = ProgramBuilder::new();
    builder.set_program_name("cairn");
    let runtime = RuntimeLayout::from_sem(module);
    let host_sigs = HostSigs::new(&mut builder, module);

    let mut ids = TapeIds::default();
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
        ids.functions.push(func);
    }

    for method in &module.methods {
        let func = builder.declare_function(FunctionSig {
            arg_types: vec![runtime::object_type()],
            ret_types: vec![ValueType::I64],
        });
        builder
            .set_function_name(
                func,
                &format!(
                    "{}#method{}",
                    module.generics[method.generic.0 as usize].name, method.id.0
                ),
            )
            .map_err(build_error)?;
        builder
            .set_function_input_name(func, 0, "self")
            .map_err(build_error)?;
        builder
            .set_function_output_name(func, 0, "result")
            .map_err(build_error)?;
        ids.methods.push(func);
    }

    for generic in &module.generics {
        let func = builder.declare_function(FunctionSig {
            arg_types: vec![runtime::object_type()],
            ret_types: vec![ValueType::I64],
        });
        builder
            .set_function_name(func, &format!("{}#dispatch", generic.name))
            .map_err(build_error)?;
        builder
            .set_function_input_name(func, 0, "object")
            .map_err(build_error)?;
        builder
            .set_function_output_name(func, 0, "result")
            .map_err(build_error)?;
        ids.generics.push(func);
    }

    for function in &module.functions {
        let mut emitter = FunctionEmitter::for_function(function, module, &ids, &host_sigs)?;
        emitter.emit_block(&function.body)?;
        let func = ids.functions[function.id.0 as usize];
        builder
            .define_function(func, emitter.finish())
            .map_err(build_error)?;
    }

    for method in &module.methods {
        let mut emitter = FunctionEmitter::for_method(method, module, &ids, &host_sigs)?;
        emitter.emit_block(&method.body)?;
        let func = ids.methods[method.id.0 as usize];
        builder
            .define_function(func, emitter.finish())
            .map_err(build_error)?;
    }

    for generic in &module.generics {
        let mut asm = Asm::new();
        emit_dispatcher(&mut asm, module, &ids, &host_sigs, generic)?;
        let func = ids.generics[generic.id.0 as usize];
        builder.define_function(func, asm).map_err(build_error)?;
    }

    let program = builder.build_verified().map_err(build_error)?;
    Ok(TapeModule {
        program,
        runtime,
        main,
    })
}

fn build_error(error: impl core::fmt::Debug) -> TapeError {
    TapeError::Build {
        message: format!("{error:?}"),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TapeIds {
    functions: Vec<FuncId>,
    methods: Vec<FuncId>,
    generics: Vec<FuncId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostSigs {
    make_object: Vec<HostSigId>,
    class_of: HostSigId,
    slot_get: HostSigId,
}

impl HostSigs {
    fn new(builder: &mut ProgramBuilder, module: &sem::Module) -> Self {
        let make_object = module
            .classes
            .iter()
            .map(|class| {
                builder.host_sig_for(
                    runtime::MAKE_OBJECT,
                    runtime::make_object_signature(class.slots.len()),
                )
            })
            .collect();
        let class_of = builder.host_sig_for(runtime::CLASS_OF, runtime::class_of_signature());
        let slot_get = builder.host_sig_for(runtime::SLOT_GET, runtime::slot_get_signature());
        Self {
            make_object,
            class_of,
            slot_get,
        }
    }
}

fn emit_dispatcher(
    asm: &mut Asm,
    module: &sem::Module,
    ids: &TapeIds,
    host_sigs: &HostSigs,
    generic: &sem::GenericFunction,
) -> Result<(), TapeError> {
    let class_reg = 2;
    asm.host_call(0, host_sigs.class_of, 0, &[1], &[class_reg]);
    let mut next_reg = 3;

    for method in module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
    {
        let class_const = next_reg;
        next_reg += 1;
        let cond = next_reg;
        next_reg += 1;
        let result = next_reg;
        next_reg += 1;

        let matched = asm.label();
        let next = asm.label();
        asm.const_u64(class_const, u64::from(method.specializer.0));
        asm.u64_eq(cond, class_reg, class_const);
        asm.br(cond, matched, next);
        asm.place(matched).map_err(build_error)?;
        let method_func = ids.methods[method.id.0 as usize];
        asm.call(0, method_func, 0, &[1], &[result]);
        asm.ret(0, &[result]);
        asm.place(next).map_err(build_error)?;
    }

    asm.trap(1);
    Ok(())
}

struct FunctionEmitter<'module> {
    asm: Asm,
    local_regs: Vec<Option<u32>>,
    module: &'module sem::Module,
    ids: &'module TapeIds,
    host_sigs: &'module HostSigs,
    next_reg: u32,
}

impl<'module> FunctionEmitter<'module> {
    fn for_function(
        function: &sem::Function,
        module: &'module sem::Module,
        ids: &'module TapeIds,
        host_sigs: &'module HostSigs,
    ) -> Result<Self, TapeError> {
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
            module,
            ids,
            host_sigs,
            next_reg,
        })
    }

    fn for_method(
        method: &sem::Method,
        module: &'module sem::Module,
        ids: &'module TapeIds,
        host_sigs: &'module HostSigs,
    ) -> Result<Self, TapeError> {
        let mut local_regs = vec![None; method.locals.len()];
        if let Some(slot) = local_regs.get_mut(method.param.0 as usize) {
            *slot = Some(1);
        }
        Ok(Self {
            asm: Asm::new(),
            local_regs,
            module,
            ids,
            host_sigs,
            next_reg: 2,
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
                let Some(func) = self.ids.functions.get(function.0 as usize).copied() else {
                    return Err(invalid_semantic("function id is out of range"));
                };
                let result = self.alloc_reg();
                self.set_span(*span);
                self.asm.call(0, func, 0, &arg_regs, &[result]);
                Ok(result)
            }
            sem::Expr::GenericCall {
                generic,
                args,
                span,
            } => {
                let mut arg_regs = Vec::with_capacity(args.len());
                for arg in args {
                    arg_regs.push(self.emit_expr(arg)?);
                }
                let Some(func) = self.ids.generics.get(generic.0 as usize).copied() else {
                    return Err(invalid_semantic("generic id is out of range"));
                };
                let result = self.alloc_reg();
                self.set_span(*span);
                self.asm.call(0, func, 0, &arg_regs, &[result]);
                Ok(result)
            }
            sem::Expr::MakeObject {
                class,
                fields,
                span,
            } => {
                let class_info = self
                    .module
                    .classes
                    .get(class.0 as usize)
                    .ok_or_else(|| invalid_semantic("class id is out of range"))?;
                let class_reg = self.alloc_reg();
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                let mut args = Vec::with_capacity(class_info.slots.len() + 1);
                args.push(class_reg);
                for slot in &class_info.slots {
                    let Some(field) = fields.iter().find(|field| field.slot == slot.id) else {
                        return Err(invalid_semantic("missing object field"));
                    };
                    args.push(self.emit_expr(&field.value)?);
                }
                let result = self.alloc_reg();
                let sig = self.host_sigs.make_object[class.0 as usize];
                self.asm.host_call(0, sig, 0, &args, &[result]);
                Ok(result)
            }
            sem::Expr::SlotRead {
                receiver,
                slot,
                span,
                ..
            } => {
                let receiver = self.emit_expr(receiver)?;
                let slot_reg = self.alloc_reg();
                self.set_span(*span);
                self.asm.const_u64(slot_reg, u64::from(slot.0));
                let result = self.alloc_reg();
                self.asm.host_call(
                    0,
                    self.host_sigs.slot_get,
                    0,
                    &[receiver, slot_reg],
                    &[result],
                );
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

    #[test]
    fn emits_verified_tape_with_generic_dispatcher() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        assert_eq!(tape.program.program().functions.len(), 3);
        assert_eq!(tape.main, Some(FuncId(0)));
        assert_eq!(tape.runtime.classes[0].slots, ["radius"]);
    }
}
