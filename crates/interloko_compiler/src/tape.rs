//! Adapter from lowered Interloko IR to `execution_tape`.
//!
//! This module owns only bytecode emission for already-lowered Interloko functions.
//! Generic dispatch, object construction, slot reads, and slot writes arrive
//! here as lowered instructions and are encoded with ordinary calls, branches,
//! constants, and host calls.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::asm::{Asm, FunctionSig, Label, ProgramBuilder};
use execution_tape::program::{ConstId, HostSigId, SpanId, ValueType};
use execution_tape::value::FuncId;
use execution_tape::verifier::VerifiedProgram;

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::lower;
use crate::sem;
use crate::syntax::Span;
use interloko_runtime::{self as runtime, RuntimeLayout};

/// A verified tape program emitted from lowered Interloko IR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TapeModule {
    /// Verified executable tape program.
    pub program: VerifiedProgram,
    /// Runtime layout required by the Interloko host.
    pub runtime: RuntimeLayout,
    /// `execution_tape` function ids in lowered function order.
    pub functions: Vec<FuncId>,
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

impl TapeError {
    /// Stable error code for this tape emission failure.
    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        match self {
            Self::Diagnostics { .. } => DiagnosticCode::TapeBlockedByDiagnostics,
            Self::InvalidLowered { .. } => DiagnosticCode::TapeInvalidLowered,
            Self::Build { .. } => DiagnosticCode::TapeBuildError,
        }
    }
}

/// Emits a verified `execution_tape` program for a lowered module.
///
/// The emitted tape uses normal `execution_tape` bytecode: calls, branches,
/// constants, arithmetic, and host calls. There are no Interloko-specific generic
/// dispatch opcodes.
pub fn emit(module: &lower::Module) -> Result<TapeModule, TapeError> {
    let mut builder = ProgramBuilder::new();
    builder.set_program_name("interloko");
    let host_sigs = HostSigs::new(&mut builder, module)?;

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
        functions: ids.functions,
        main,
    })
}

fn value_type(kind: lower::ValueKind) -> ValueType {
    match kind {
        lower::ValueKind::Any => runtime::any_type(),
        lower::ValueKind::I64 => ValueType::I64,
        lower::ValueKind::U64 => ValueType::U64,
        lower::ValueKind::Bool => ValueType::Bool,
        lower::ValueKind::Object(_) => runtime::object_type(),
        lower::ValueKind::ClassObject(_)
        | lower::ValueKind::GenericFunctionObject(_)
        | lower::ValueKind::MethodObject(_)
        | lower::ValueKind::SlotDescriptorObject => runtime::object_type(),
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
    slot_get: Vec<Vec<HostSigId>>,
    slot_set: Vec<Vec<HostSigId>>,
    string_literal: Option<HostSigId>,
    string_literal_consts: Vec<(String, ConstId)>,
    symbol_literal: Option<HostSigId>,
    symbol_literal_consts: Vec<(String, ConstId)>,
    vector_literals: Vec<(usize, HostSigId)>,
    value_eq: HostSigId,
    runtime_calls: Vec<(lower::RuntimeOp, HostSigId)>,
    box_i64: HostSigId,
    box_bool: HostSigId,
    box_object: HostSigId,
    instance_any: HostSigId,
    instance_object: HostSigId,
    cast_any: HostSigId,
    cast_object: HostSigId,
}

impl HostSigs {
    fn new(builder: &mut ProgramBuilder, module: &lower::Module) -> Result<Self, TapeError> {
        let make_object = module
            .runtime
            .classes
            .iter()
            .map(|class| {
                Ok(builder.host_sig_for(
                    runtime::MAKE_OBJECT,
                    runtime::make_object_signature(class)
                        .ok_or_else(|| invalid_lowered("runtime class has invalid slot kind"))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let slot_get = module
            .runtime
            .classes
            .iter()
            .map(|class| {
                class
                    .slots
                    .iter()
                    .map(|slot| {
                        Ok(builder.host_sig_for(
                            runtime::SLOT_GET,
                            runtime::slot_get_signature(slot.kind)
                                .ok_or_else(|| invalid_lowered("runtime slot has invalid kind"))?,
                        ))
                    })
                    .collect::<Result<Vec<_>, TapeError>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let slot_set = module
            .runtime
            .classes
            .iter()
            .map(|class| {
                class
                    .slots
                    .iter()
                    .map(|slot| {
                        Ok(builder.host_sig_for(
                            runtime::SLOT_SET,
                            runtime::slot_set_signature(slot.kind)
                                .ok_or_else(|| invalid_lowered("runtime slot has invalid kind"))?,
                        ))
                    })
                    .collect::<Result<Vec<_>, TapeError>>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        let class_of = builder.host_sig_for(runtime::CLASS_OF, runtime::class_of_signature());
        let box_i64 = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(ValueType::I64),
        );
        let box_bool = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(ValueType::Bool),
        );
        let box_object = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(runtime::object_type()),
        );
        let instance_any = builder.host_sig_for(
            runtime::INSTANCE_OF,
            runtime::instance_of_signature(runtime::any_type()),
        );
        let instance_object = builder.host_sig_for(
            runtime::INSTANCE_OF,
            runtime::instance_of_signature(runtime::object_type()),
        );
        let cast_any = builder.host_sig_for(
            runtime::CAST_OBJECT,
            runtime::cast_object_signature(runtime::any_type()),
        );
        let cast_object = builder.host_sig_for(
            runtime::CAST_OBJECT,
            runtime::cast_object_signature(runtime::object_type()),
        );
        let string_literal_consts = literal_consts(builder, module, |instr| match instr {
            lower::Instr::StringLiteral { value, .. } => Some(value.as_str()),
            _ => None,
        });
        let string_literal = (!string_literal_consts.is_empty()).then(|| {
            builder.host_sig_for(runtime::STRING_LITERAL, runtime::string_literal_signature())
        });
        let symbol_literal_consts = literal_consts(builder, module, |instr| match instr {
            lower::Instr::SymbolLiteral { name, .. } => Some(name.as_str()),
            _ => None,
        });
        let symbol_literal = (!symbol_literal_consts.is_empty()).then(|| {
            builder.host_sig_for(runtime::SYMBOL_LITERAL, runtime::symbol_literal_signature())
        });
        let vector_literals = vector_literal_sigs(builder, module);
        let value_eq = builder.host_sig_for(runtime::VALUE_EQ, runtime::value_eq_signature());
        let runtime_calls = runtime_call_sigs(builder);
        Ok(Self {
            make_object,
            class_of,
            slot_get,
            slot_set,
            string_literal,
            string_literal_consts,
            symbol_literal,
            symbol_literal_consts,
            vector_literals,
            value_eq,
            runtime_calls,
            box_i64,
            box_bool,
            box_object,
            instance_any,
            instance_object,
            cast_any,
            cast_object,
        })
    }

    fn string_literal(&self, value: &str) -> Result<(HostSigId, ConstId), TapeError> {
        let sig = self
            .string_literal
            .ok_or_else(|| invalid_lowered("string literal signature is missing"))?;
        self.string_literal_consts
            .iter()
            .find(|(literal, _)| literal == value)
            .map(|(_, const_id)| (sig, *const_id))
            .ok_or_else(|| invalid_lowered("string literal constant is missing"))
    }

    fn symbol_literal(&self, name: &str) -> Result<(HostSigId, ConstId), TapeError> {
        let sig = self
            .symbol_literal
            .ok_or_else(|| invalid_lowered("symbol literal signature is missing"))?;
        self.symbol_literal_consts
            .iter()
            .find(|(literal, _)| literal == name)
            .map(|(_, const_id)| (sig, *const_id))
            .ok_or_else(|| invalid_lowered("symbol literal constant is missing"))
    }

    fn vector_literal(&self, arity: usize) -> Result<HostSigId, TapeError> {
        self.vector_literals
            .iter()
            .find(|(literal_arity, _)| *literal_arity == arity)
            .map(|(_, sig)| *sig)
            .ok_or_else(|| invalid_lowered("vector literal signature is missing"))
    }

    fn runtime_call(&self, op: lower::RuntimeOp) -> Result<HostSigId, TapeError> {
        self.runtime_calls
            .iter()
            .find(|(candidate, _)| *candidate == op)
            .map(|(_, sig)| *sig)
            .ok_or_else(|| invalid_lowered("runtime call signature is missing"))
    }
}

fn runtime_call_sigs(builder: &mut ProgramBuilder) -> Vec<(lower::RuntimeOp, HostSigId)> {
    use lower::RuntimeOp;
    [
        (
            RuntimeOp::ClassMetaObject,
            runtime::META_CLASS_OBJECT,
            runtime::meta_class_object_signature(),
        ),
        (
            RuntimeOp::GenericMetaObject,
            runtime::META_GENERIC_OBJECT,
            runtime::meta_generic_object_signature(),
        ),
        (
            RuntimeOp::MethodMetaObject,
            runtime::META_METHOD_OBJECT,
            runtime::meta_method_object_signature(),
        ),
        (
            RuntimeOp::SlotDescriptorObject,
            runtime::META_SLOT_DESCRIPTOR_OBJECT,
            runtime::meta_slot_descriptor_object_signature(),
        ),
        (
            RuntimeOp::ObjectClass,
            runtime::MOP_OBJECT_CLASS,
            runtime::meta_query_any_signature(),
        ),
        (
            RuntimeOp::ClassName,
            runtime::MOP_CLASS_NAME,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::ClassDirectSuperclass,
            runtime::MOP_CLASS_DIRECT_SUPERCLASS,
            runtime::meta_query_any_signature(),
        ),
        (
            RuntimeOp::ClassDirectSlots,
            runtime::MOP_CLASS_DIRECT_SLOTS,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::ClassEffectiveSlots,
            runtime::MOP_CLASS_EFFECTIVE_SLOTS,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::SlotName,
            runtime::MOP_SLOT_NAME,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::SlotType,
            runtime::MOP_SLOT_TYPE,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::SlotInitKeyword,
            runtime::MOP_SLOT_INIT_KEYWORD,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::SlotMutable,
            runtime::MOP_SLOT_MUTABLE,
            runtime::meta_query_bool_signature(),
        ),
        (
            RuntimeOp::SlotGetter,
            runtime::MOP_SLOT_GETTER,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::SlotSetter,
            runtime::MOP_SLOT_SETTER,
            runtime::meta_query_any_signature(),
        ),
        (
            RuntimeOp::GenericFunctionName,
            runtime::MOP_GENERIC_FUNCTION_NAME,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::GenericFunctionMethods,
            runtime::MOP_GENERIC_FUNCTION_METHODS,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::MethodGenericFunction,
            runtime::MOP_METHOD_GENERIC_FUNCTION,
            runtime::meta_query_any_signature(),
        ),
        (
            RuntimeOp::MethodSpecializers,
            runtime::MOP_METHOD_SPECIALIZERS,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::VectorSize,
            runtime::VECTOR_SIZE,
            runtime::vector_size_signature(),
        ),
        (
            RuntimeOp::VectorRef,
            runtime::VECTOR_REF,
            runtime::vector_ref_signature(),
        ),
    ]
    .into_iter()
    .map(|(op, symbol, sig)| (op, builder.host_sig_for(symbol, sig)))
    .collect()
}

fn literal_consts(
    builder: &mut ProgramBuilder,
    module: &lower::Module,
    select: fn(&lower::Instr) -> Option<&str>,
) -> Vec<(String, ConstId)> {
    let mut literals: Vec<(String, ConstId)> = Vec::new();
    for function in &module.functions {
        for instr in &function.instructions {
            let Some(value) = select(instr) else {
                continue;
            };
            if literals.iter().any(|(literal, _)| literal == value) {
                continue;
            }
            let const_id = builder.const_str(value);
            literals.push((value.into(), const_id));
        }
    }
    literals
}

fn vector_literal_sigs(
    builder: &mut ProgramBuilder,
    module: &lower::Module,
) -> Vec<(usize, HostSigId)> {
    let mut arities = Vec::new();
    for function in &module.functions {
        for instr in &function.instructions {
            let lower::Instr::VectorLiteral { elements, .. } = instr else {
                continue;
            };
            let arity = elements.len();
            if arities.iter().any(|(existing, _)| *existing == arity) {
                continue;
            }
            let sig = builder.host_sig_for(
                runtime::VECTOR_LITERAL,
                runtime::vector_literal_signature(arity),
            );
            arities.push((arity, sig));
        }
    }
    arities
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
            lower::Instr::StringLiteral {
                dst, value, span, ..
            } => {
                let (sig, value_const) = self.host_sigs.string_literal(value)?;
                let value_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_pool(value_reg, value_const);
                self.asm.host_call(0, sig, 0, &[value_reg], &[dst.0]);
            }
            lower::Instr::SymbolLiteral {
                dst, name, span, ..
            } => {
                let (sig, name_const) = self.host_sigs.symbol_literal(name)?;
                let name_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_pool(name_reg, name_const);
                self.asm.host_call(0, sig, 0, &[name_reg], &[dst.0]);
            }
            lower::Instr::ConstU64 { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_u64(dst.0, *value);
            }
            lower::Instr::ConstClass { dst, class, span } => {
                self.set_span(*span);
                self.asm.const_u64(dst.0, u64::from(class.0));
            }
            lower::Instr::BoxValue {
                dst,
                src,
                from,
                span,
            } => {
                let sig = match from {
                    lower::ValueKind::I64 => self.host_sigs.box_i64,
                    lower::ValueKind::Bool => self.host_sigs.box_bool,
                    lower::ValueKind::Object(_)
                    | lower::ValueKind::ClassObject(_)
                    | lower::ValueKind::GenericFunctionObject(_)
                    | lower::ValueKind::MethodObject(_)
                    | lower::ValueKind::SlotDescriptorObject => self.host_sigs.box_object,
                    lower::ValueKind::Any | lower::ValueKind::U64 => {
                        return Err(invalid_lowered("invalid boxed value input kind"));
                    }
                };
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &[src.0], &[dst.0]);
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
            lower::Instr::ValueEqual {
                dst,
                lhs,
                rhs,
                span,
                ..
            } => {
                self.set_span(*span);
                self.asm
                    .host_call(0, self.host_sigs.value_eq, 0, &[lhs.0, rhs.0], &[dst.0]);
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
            lower::Instr::VectorLiteral {
                dst,
                elements,
                span,
                ..
            } => {
                let sig = self.host_sigs.vector_literal(elements.len())?;
                let elements = reg_slice(elements);
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &elements, &[dst.0]);
            }
            lower::Instr::RuntimeCall {
                dst,
                op,
                args,
                span,
                ..
            } => {
                let sig = self.host_sigs.runtime_call(*op)?;
                let args = reg_slice(args);
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &args, &[dst.0]);
            }
            lower::Instr::ReadSlot {
                dst,
                receiver,
                class,
                slot,
                span,
                ..
            } => {
                let sig = self
                    .host_sigs
                    .slot_get
                    .get(class.0 as usize)
                    .and_then(|slots| slots.get(slot.0 as usize))
                    .copied()
                    .ok_or_else(|| invalid_lowered("slot_get signature is out of range"))?;
                let slot_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_u64(slot_reg, u64::from(slot.0));
                self.asm
                    .host_call(0, sig, 0, &[receiver.0, slot_reg], &[dst.0]);
            }
            lower::Instr::WriteSlot {
                dst,
                receiver,
                value,
                class,
                slot,
                span,
                ..
            } => {
                let sig = self
                    .host_sigs
                    .slot_set
                    .get(class.0 as usize)
                    .and_then(|slots| slots.get(slot.0 as usize))
                    .copied()
                    .ok_or_else(|| invalid_lowered("slot_set signature is out of range"))?;
                let slot_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_u64(slot_reg, u64::from(slot.0));
                self.asm
                    .host_call(0, sig, 0, &[receiver.0, slot_reg, value.0], &[dst.0]);
            }
            lower::Instr::ClassOf { dst, object, span } => {
                self.set_span(*span);
                self.asm
                    .host_call(0, self.host_sigs.class_of, 0, &[object.0], &[dst.0]);
            }
            lower::Instr::InstanceOf {
                dst,
                value,
                input,
                class,
                span,
            } => {
                let sig = self.instance_sig(*input)?;
                let class_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                self.asm
                    .host_call(0, sig, 0, &[value.0, class_reg], &[dst.0]);
            }
            lower::Instr::CastObject {
                dst,
                value,
                input,
                class,
                span,
            } => {
                let sig = self.cast_sig(*input)?;
                let class_reg = self.alloc_temp();
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                self.asm
                    .host_call(0, sig, 0, &[value.0, class_reg], &[dst.0]);
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

    fn instance_sig(&self, kind: lower::ValueKind) -> Result<HostSigId, TapeError> {
        match kind {
            lower::ValueKind::Any => Ok(self.host_sigs.instance_any),
            lower::ValueKind::Object(_) => Ok(self.host_sigs.instance_object),
            _ => Err(invalid_lowered("instance? input kind is invalid")),
        }
    }

    fn cast_sig(&self, kind: lower::ValueKind) -> Result<HostSigId, TapeError> {
        match kind {
            lower::ValueKind::Any => Ok(self.host_sigs.cast_any),
            lower::ValueKind::Object(_) => Ok(self.host_sigs.cast_object),
            _ => Err(invalid_lowered("cast input kind is invalid")),
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
        | lower::Instr::StringLiteral { dst, .. }
        | lower::Instr::SymbolLiteral { dst, .. }
        | lower::Instr::ConstU64 { dst, .. }
        | lower::Instr::ConstClass { dst, .. } => {
            note_register(*dst, max);
        }
        lower::Instr::BoxValue { dst, src, .. } => {
            note_register(*dst, max);
            note_register(*src, max);
        }
        lower::Instr::I64Binary { dst, lhs, rhs, .. }
        | lower::Instr::I64Compare { dst, lhs, rhs, .. }
        | lower::Instr::ValueEqual { dst, lhs, rhs, .. }
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
        lower::Instr::VectorLiteral { dst, elements, .. } => {
            note_register(*dst, max);
            note_registers(elements, max);
        }
        lower::Instr::RuntimeCall { dst, args, .. } => {
            note_register(*dst, max);
            note_registers(args, max);
        }
        lower::Instr::ReadSlot { dst, receiver, .. } => {
            note_register(*dst, max);
            note_register(*receiver, max);
        }
        lower::Instr::WriteSlot {
            dst,
            receiver,
            value,
            ..
        } => {
            note_register(*dst, max);
            note_register(*receiver, max);
            note_register(*value, max);
        }
        lower::Instr::ClassOf { dst, object, .. } => {
            note_register(*dst, max);
            note_register(*object, max);
        }
        lower::Instr::InstanceOf { dst, value, .. }
        | lower::Instr::CastObject { dst, value, .. } => {
            note_register(*dst, max);
            note_register(*value, max);
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
    use crate::{ast, grammar_frontend, identity::SourceFileId, sem};

    fn lower_source(source: &str) -> lower::Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let ast = ast::lower(&tree);
        let sem = sem::lower(&ast);
        lower::lower(&sem).expect("lowering should succeed")
    }

    fn runtime_class<'a>(tape: &'a TapeModule, name: &str) -> &'a runtime::RuntimeClass {
        tape.runtime
            .classes
            .iter()
            .find(|class| class.name == name)
            .unwrap_or_else(|| panic!("missing runtime class {name}"))
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
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        assert_eq!(tape.program.program().functions.len(), 5);
        assert_eq!(tape.main, Some(FuncId(0)));
        let circle = runtime_class(&tape, "<circle>");
        assert_eq!(circle.slots[0].name, "radius");
        assert_eq!(circle.slots[0].kind, runtime::ValueKind::I64);
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
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program.program().functions.len(), 3);
        assert_eq!(tape.main, Some(FuncId(0)));
    }

    #[test]
    fn emits_verified_tape_for_typed_runtime_slots() {
        let module = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<flag>, valid?: #t).valid? then 1 else 0 end\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program.program().functions.len(), 3);
        let flag = runtime_class(&tape, "<flag>");
        assert_eq!(flag.slots[0].name, "valid?");
        assert_eq!(flag.slots[0].kind, runtime::ValueKind::Bool);
    }

    #[test]
    fn emits_verified_tape_for_object_top_slots() {
        let module = lower_source(
            "define class <box> (<object>)\n  slot value;\nend;\n\
             define function main()\n  make(<box>, value: 7).value\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program.program().functions.len(), 3);
        let box_class = runtime_class(&tape, "<box>");
        assert_eq!(box_class.slots[0].name, "value");
        assert_eq!(box_class.slots[0].kind, runtime::ValueKind::Any);
    }
}
