//! Adapter from lowered Cairn IR to `execution_tape`.
//!
//! This module owns only bytecode emission for already-lowered Cairn functions.
//! Generic dispatch, object construction, and slot reads arrive here as lowered
//! instructions and are encoded with ordinary calls, branches, constants, and
//! host calls.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::asm::{Asm, FunctionSig, Label, ProgramBuilder};
use execution_tape::program::{HostSigId, SpanId, ValueType};
use execution_tape::value::FuncId;
use execution_tape::verifier::VerifiedProgram;

use crate::diagnostic::Diagnostic;
use crate::lower;
use crate::runtime::{self, RuntimeLayout};
use crate::sem;
use crate::syntax::Span;

/// A verified tape program emitted from lowered Cairn IR.
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
    /// Lowered IR contained an invalid reference.
    InvalidLowered {
        /// Error details.
        message: String,
    },
    /// `execution_tape` rejected the generated program.
    Build {
        /// Builder or verifier details.
        message: String,
    },
}

/// Emits a verified `execution_tape` program for a lowered module.
///
/// The emitted tape uses normal `execution_tape` bytecode: calls, branches,
/// constants, arithmetic, and host calls. There are no Cairn-specific generic
/// dispatch opcodes.
pub fn emit(module: &lower::Module) -> Result<TapeModule, TapeError> {
    let mut builder = ProgramBuilder::new();
    builder.set_program_name("cairn");
    let host_sigs = HostSigs::new(&mut builder, &module.runtime);

    let mut ids = TapeIds::default();
    let mut main = None;

    for function in &module.functions {
        if function.id.0 as usize != ids.functions.len() {
            return Err(invalid_lowered("lowered functions are not in id order"));
        }
        let func = builder.declare_function(FunctionSig {
            arg_types: function
                .params
                .iter()
                .map(|param| value_type(param.kind))
                .collect(),
            ret_types: vec![value_type(function.ret)],
        });
        builder
            .set_function_name(func, &function.name)
            .map_err(build_error)?;
        for (index, param) in function.params.iter().enumerate() {
            builder
                .set_function_input_name(
                    func,
                    u32::try_from(index).unwrap_or(u32::MAX),
                    &param.name,
                )
                .map_err(build_error)?;
        }
        builder
            .set_function_output_name(func, 0, "result")
            .map_err(build_error)?;
        if module.main == Some(function.id) {
            main = Some(func);
        }
        ids.functions.push(func);
    }

    for function in &module.functions {
        let func = ids
            .functions
            .get(function.id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_lowered("function id is out of range"))?;
        let asm = FunctionEmitter::new(function, &ids, &host_sigs).emit()?;
        builder.define_function(func, asm).map_err(build_error)?;
    }

    let program = builder.build_verified().map_err(build_error)?;
    Ok(TapeModule {
        program,
        runtime: module.runtime.clone(),
        main,
    })
}

fn value_type(kind: lower::ValueKind) -> ValueType {
    match kind {
        lower::ValueKind::I64 => ValueType::I64,
        lower::ValueKind::U64 => ValueType::U64,
        lower::ValueKind::Bool => ValueType::Bool,
        lower::ValueKind::Object(_) => runtime::object_type(),
    }
}

fn build_error(error: impl core::fmt::Debug) -> TapeError {
    TapeError::Build {
        message: format!("{error:?}"),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TapeIds {
    functions: Vec<FuncId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostSigs {
    make_object: Vec<HostSigId>,
    class_of: HostSigId,
    slot_get: HostSigId,
}

impl HostSigs {
    fn new(builder: &mut ProgramBuilder, runtime: &RuntimeLayout) -> Self {
        let make_object = runtime
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

struct FunctionEmitter<'a> {
    function: &'a lower::Function,
    ids: &'a TapeIds,
    host_sigs: &'a HostSigs,
    asm: Asm,
    labels: Vec<Label>,
    next_temp: u32,
}

impl<'a> FunctionEmitter<'a> {
    fn new(function: &'a lower::Function, ids: &'a TapeIds, host_sigs: &'a HostSigs) -> Self {
        let mut asm = Asm::new();
        let labels = labels_for(function, &mut asm);
        Self {
            function,
            ids,
            host_sigs,
            asm,
            labels,
            next_temp: max_register(function).saturating_add(1),
        }
    }

    fn emit(mut self) -> Result<Asm, TapeError> {
        for instr in &self.function.instructions {
            self.emit_instr(instr)?;
        }
        Ok(self.asm)
    }

    fn emit_instr(&mut self, instr: &lower::Instr) -> Result<(), TapeError> {
        match instr {
            lower::Instr::Label { label } => {
                let label = self.label(*label)?;
                self.asm.place(label).map_err(build_error)?;
            }
            lower::Instr::ConstI64 { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_i64(dst.0, *value);
            }
            lower::Instr::ConstBool { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_bool(dst.0, *value);
            }
            lower::Instr::ConstU64 { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_u64(dst.0, *value);
            }
            lower::Instr::I64Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                match op {
                    sem::BinaryOp::Add => {
                        self.asm.i64_add(dst.0, lhs.0, rhs.0);
                    }
                    sem::BinaryOp::Sub => {
                        self.asm.i64_sub(dst.0, lhs.0, rhs.0);
                    }
                    sem::BinaryOp::Mul => {
                        self.asm.i64_mul(dst.0, lhs.0, rhs.0);
                    }
                }
            }
            lower::Instr::I64Compare {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                match op {
                    sem::CompareOp::Eq => {
                        self.asm.i64_eq(dst.0, lhs.0, rhs.0);
                    }
                    sem::CompareOp::Lt => {
                        self.asm.i64_lt(dst.0, lhs.0, rhs.0);
                    }
                    sem::CompareOp::Le => {
                        self.asm.i64_le(dst.0, lhs.0, rhs.0);
                    }
                    sem::CompareOp::Gt => {
                        self.asm.i64_gt(dst.0, lhs.0, rhs.0);
                    }
                    sem::CompareOp::Ge => {
                        self.asm.i64_ge(dst.0, lhs.0, rhs.0);
                    }
                }
            }
            lower::Instr::Call {
                dst,
                callee,
                args,
                span,
            } => {
                let callee = self
                    .ids
                    .functions
                    .get(callee.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_lowered("callee id is out of range"))?;
                let args = reg_slice(args);
                self.set_span(*span);
                self.asm.call(0, callee, 0, &args, &[dst.0]);
            }
            lower::Instr::MakeObject {
                dst,
                class,
                fields,
                span,
            } => {
                let sig = self
                    .host_sigs
                    .make_object
                    .get(class.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_lowered("class id is out of range"))?;
                let class_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                let mut args = Vec::with_capacity(fields.len() + 1);
                args.push(class_reg);
                args.extend(reg_slice(fields));
                self.asm.host_call(0, sig, 0, &args, &[dst.0]);
            }
            lower::Instr::ReadSlot {
                dst,
                receiver,
                slot,
                span,
                ..
            } => {
                let slot_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_u64(slot_reg, u64::from(slot.0));
                self.asm.host_call(
                    0,
                    self.host_sigs.slot_get,
                    0,
                    &[receiver.0, slot_reg],
                    &[dst.0],
                );
            }
            lower::Instr::ClassOf { dst, object, span } => {
                self.set_span(*span);
                self.asm
                    .host_call(0, self.host_sigs.class_of, 0, &[object.0], &[dst.0]);
            }
            lower::Instr::EqU64 {
                dst,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                self.asm.u64_eq(dst.0, lhs.0, rhs.0);
            }
            lower::Instr::Branch {
                cond,
                then_label,
                else_label,
                span,
            } => {
                let then_label = self.label(*then_label)?;
                let else_label = self.label(*else_label)?;
                self.set_span(*span);
                self.asm.br(cond.0, then_label, else_label);
            }
            lower::Instr::Jump { label, span } => {
                let label = self.label(*label)?;
                self.set_span(*span);
                self.asm.jmp(label);
            }
            lower::Instr::Move { dst, src, span } => {
                self.set_span(*span);
                self.asm.mov(dst.0, src.0);
            }
            lower::Instr::Return { value, span } => {
                self.set_span(*span);
                self.asm.ret(0, &[value.0]);
            }
            lower::Instr::Trap { code, span } => {
                self.set_span(*span);
                self.asm.trap(*code);
            }
        }
        Ok(())
    }

    fn label(&self, label: lower::LabelId) -> Result<Label, TapeError> {
        self.labels
            .get(label.0 as usize)
            .copied()
            .ok_or_else(|| invalid_lowered("label id is out of range"))
    }

    fn alloc_temp(&mut self) -> u32 {
        let reg = self.next_temp;
        self.next_temp = self.next_temp.saturating_add(1);
        reg
    }

    fn set_span(&mut self, span: Span) {
        if let Some(span_id) = span_id(span) {
            self.asm.span(span_id);
        }
    }
}

fn labels_for(function: &lower::Function, asm: &mut Asm) -> Vec<Label> {
    let mut count = 0_usize;
    for instr in &function.instructions {
        match instr {
            lower::Instr::Label { label } => {
                count = count.max(label.0 as usize + 1);
            }
            lower::Instr::Branch {
                then_label,
                else_label,
                ..
            } => {
                count = count.max(then_label.0 as usize + 1);
                count = count.max(else_label.0 as usize + 1);
            }
            lower::Instr::Jump { label, .. } => {
                count = count.max(label.0 as usize + 1);
            }
            _ => {}
        }
    }
    (0..count).map(|_| asm.label()).collect()
}

fn max_register(function: &lower::Function) -> u32 {
    let mut max = 0;
    for param in &function.params {
        max = max.max(param.reg.0);
    }
    for local in &function.locals {
        max = max.max(local.reg.0);
    }
    for instr in &function.instructions {
        note_instr_registers(instr, &mut max);
    }
    max
}

fn note_instr_registers(instr: &lower::Instr, max: &mut u32) {
    match instr {
        lower::Instr::Label { .. } | lower::Instr::Jump { .. } | lower::Instr::Trap { .. } => {}
        lower::Instr::ConstI64 { dst, .. }
        | lower::Instr::ConstBool { dst, .. }
        | lower::Instr::ConstU64 { dst, .. } => {
            note_register(*dst, max);
        }
        lower::Instr::I64Binary { dst, lhs, rhs, .. }
        | lower::Instr::I64Compare { dst, lhs, rhs, .. }
        | lower::Instr::EqU64 { dst, lhs, rhs, .. } => {
            note_register(*dst, max);
            note_register(*lhs, max);
            note_register(*rhs, max);
        }
        lower::Instr::Call { dst, args, .. } => {
            note_register(*dst, max);
            note_registers(args, max);
        }
        lower::Instr::MakeObject { dst, fields, .. } => {
            note_register(*dst, max);
            note_registers(fields, max);
        }
        lower::Instr::ReadSlot { dst, receiver, .. } => {
            note_register(*dst, max);
            note_register(*receiver, max);
        }
        lower::Instr::ClassOf { dst, object, .. } => {
            note_register(*dst, max);
            note_register(*object, max);
        }
        lower::Instr::Branch { cond, .. } => {
            note_register(*cond, max);
        }
        lower::Instr::Move { dst, src, .. } => {
            note_register(*dst, max);
            note_register(*src, max);
        }
        lower::Instr::Return { value, .. } => {
            note_register(*value, max);
        }
    }
}

fn note_register(reg: lower::Register, max: &mut u32) {
    *max = (*max).max(reg.0);
}

fn note_registers(regs: &[lower::Register], max: &mut u32) {
    for reg in regs {
        note_register(*reg, max);
    }
}

fn reg_slice(regs: &[lower::Register]) -> Vec<u32> {
    regs.iter().map(|reg| reg.0).collect()
}

fn invalid_lowered(message: &str) -> TapeError {
    TapeError::InvalidLowered {
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

    fn lower_source(source: &str) -> lower::Module {
        let tree = syntax::parse(source);
        let ast = ast::lower(&tree);
        let sem = sem::lower(&ast);
        lower::lower(&sem).expect("lowering should succeed")
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

    #[test]
    fn emits_verified_tape_for_boolean_control_flow() {
        let module = lower_source("define function main()\n  if 10 >= 5 then 42 else 0 end\nend;");
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program.program().functions.len(), 1);
        assert_eq!(tape.main, Some(FuncId(0)));
    }

    #[test]
    fn emits_verified_tape_for_signal_handle_restart() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program.program().functions.len(), 1);
        assert_eq!(tape.main, Some(FuncId(0)));
    }
}
