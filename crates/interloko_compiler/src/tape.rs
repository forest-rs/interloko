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

use execution_tape::asm::{Asm, FunctionAbi, FunctionSig, Label, ProgramBuilder};
use execution_tape::contract::{Contract, ContractId};
use execution_tape::host::HostSig;
use execution_tape::program::{ConstId, ElemTypeId, HostSigId, SpanId, ValueType};
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
    let abi = AbiTables::new(&mut builder, module)?;

    let mut ids = TapeIds::default();
    let mut main = None;

    for function in &module.functions {
        if function.id.0 as usize != ids.functions.len() {
            return Err(invalid_lowered("lowered functions are not in id order"));
        }
        let sig = FunctionSig {
            arg_types: function
                .params
                .iter()
                .map(|param| value_type(param.kind))
                .collect(),
            ret_types: function.rets.iter().copied().map(value_type).collect(),
        };
        let mut signature = FunctionAbi::from(sig);
        if let Some(environment) = closure_env_contract(function, &mut builder)? {
            signature.args[0] = environment.into();
        }
        let func = builder.declare_function(signature).map_err(build_error)?;
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
        for index in 0..function.rets.len() {
            builder
                .set_function_output_name(
                    func,
                    u32::try_from(index).unwrap_or(u32::MAX),
                    &format!("result{index}"),
                )
                .map_err(build_error)?;
        }
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
        let asm = FunctionEmitter::new(function, &ids, &abi, &mut builder).emit()?;
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
        lower::ValueKind::F64 => ValueType::F64,
        lower::ValueKind::U64 => ValueType::U64,
        lower::ValueKind::Bool => ValueType::Bool,
        lower::ValueKind::Function(_) => ValueType::Func,
        lower::ValueKind::Closure(_) => ValueType::Closure,
        lower::ValueKind::Aggregate => ValueType::Agg,
        lower::ValueKind::Object(_) => runtime::object_type(),
        lower::ValueKind::ClassObject(_)
        | lower::ValueKind::GenericFunctionObject(_)
        | lower::ValueKind::MethodObject(_)
        | lower::ValueKind::SlotDescriptorObject => runtime::object_type(),
    }
}

fn closure_env_contract(
    function: &lower::Function,
    builder: &mut ProgramBuilder,
) -> Result<Option<ContractId>, TapeError> {
    if !matches!(function.kind, lower::FunctionKind::Closure(_)) {
        return Ok(None);
    }

    let mut elems: Vec<Option<ValueType>> = Vec::new();
    for instr in &function.instructions {
        let lower::Instr::CaptureGet { index, kind, .. } = instr else {
            continue;
        };
        if elems.len() <= *index {
            elems.resize(index.saturating_add(1), None);
        }
        let ty = value_type(*kind);
        match elems[*index] {
            Some(existing) if existing != ty => {
                return Err(invalid_lowered("capture index has conflicting value kinds"));
            }
            _ => {
                elems[*index] = Some(ty);
            }
        }
    }

    if elems.is_empty() {
        Ok(None)
    } else {
        let children: Vec<_> = elems
            .into_iter()
            .map(|kind| {
                kind.map(|kind| builder.contract(Contract::Kind(kind)))
                    .transpose()
            })
            .collect::<Result<_, _>>()
            .map_err(build_error)?;
        Ok(Some(
            builder
                .contract(Contract::Tuple(&children))
                .map_err(build_error)?,
        ))
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
struct AbiTables {
    make_object: HostSigId,
    class_count: usize,
    class_of: HostSigId,
    slot_get: Vec<Vec<HostSigId>>,
    slot_set: Vec<Vec<HostSigId>>,
    string_literal: Option<HostSigId>,
    string_literal_consts: Vec<(String, ConstId)>,
    symbol_literal: Option<HostSigId>,
    symbol_literal_consts: Vec<(String, ConstId)>,
    vector_literal: Option<(HostSigId, ElemTypeId)>,
    value_eq: HostSigId,
    runtime_calls: Vec<(lower::RuntimeOp, HostSigId)>,
    host_functions: Vec<HostSigId>,
    float_functions: Vec<(sem::FloatFunction, HostSigId)>,
    box_i64: HostSigId,
    box_f64: HostSigId,
    box_bool: HostSigId,
    box_object: HostSigId,
    instance_any: HostSigId,
    instance_object: HostSigId,
    cast_any: HostSigId,
    cast_object: HostSigId,
}

impl AbiTables {
    fn new(builder: &mut ProgramBuilder, module: &lower::Module) -> Result<Self, TapeError> {
        let make_object =
            builder.host_sig_for(runtime::MAKE_OBJECT, runtime::make_object_signature());
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
        let box_f64 = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(ValueType::F64),
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
        let vector_literal = vector_literal_sig(builder, module);
        let value_eq = builder.host_sig_for(runtime::VALUE_EQ, runtime::value_eq_signature());
        let runtime_calls = runtime_call_sigs(builder);
        let float_functions = float_function_sigs(builder, module);
        let host_functions = module
            .host_functions
            .iter()
            .map(|function| {
                builder.host_sig_for(
                    &function.name,
                    HostSig {
                        args: function.params.iter().copied().map(value_type).collect(),
                        rets: vec![value_type(function.ret)],
                    },
                )
            })
            .collect();
        Ok(Self {
            make_object,
            class_count: module.runtime.classes.len(),
            class_of,
            slot_get,
            slot_set,
            string_literal,
            string_literal_consts,
            symbol_literal,
            symbol_literal_consts,
            vector_literal,
            value_eq,
            runtime_calls,
            host_functions,
            float_functions,
            box_i64,
            box_f64,
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

    fn vector_literal(&self) -> Result<(HostSigId, ElemTypeId), TapeError> {
        self.vector_literal
            .ok_or_else(|| invalid_lowered("vector literal signature is missing"))
    }

    fn runtime_call(&self, op: lower::RuntimeOp) -> Result<HostSigId, TapeError> {
        self.runtime_calls
            .iter()
            .find(|(candidate, _)| *candidate == op)
            .map(|(_, sig)| *sig)
            .ok_or_else(|| invalid_lowered("runtime call signature is missing"))
    }

    fn float_function(&self, function: sem::FloatFunction) -> Result<HostSigId, TapeError> {
        self.float_functions
            .iter()
            .find(|(candidate, _)| *candidate == function)
            .map(|(_, sig)| *sig)
            .ok_or_else(|| invalid_lowered("float function signature is missing"))
    }

    fn host_function(&self, id: sem::HostFunctionId) -> Result<HostSigId, TapeError> {
        self.host_functions
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_lowered("host function signature is missing"))
    }
}

/// Declares host signatures for exactly the float library functions the
/// module calls, so programs without float math carry no extra host table
/// entries.
fn float_function_sigs(
    builder: &mut ProgramBuilder,
    module: &lower::Module,
) -> Vec<(sem::FloatFunction, HostSigId)> {
    let mut sigs: Vec<(sem::FloatFunction, HostSigId)> = Vec::new();
    for instr in module
        .functions
        .iter()
        .flat_map(|function| &function.instructions)
    {
        if let lower::Instr::FloatCall { function, .. } = instr
            && !sigs.iter().any(|(candidate, _)| candidate == function)
        {
            let sig = builder.host_sig_for(function.symbol(), function.signature());
            sigs.push((*function, sig));
        }
    }
    sigs
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

fn vector_literal_sig(
    builder: &mut ProgramBuilder,
    module: &lower::Module,
) -> Option<(HostSigId, ElemTypeId)> {
    for function in &module.functions {
        for instr in &function.instructions {
            if matches!(instr, lower::Instr::VectorLiteral { .. }) {
                let sig = builder
                    .host_sig_for(runtime::VECTOR_LITERAL, runtime::vector_literal_signature());
                let elem = builder.array_elem(runtime::any_type());
                return Some((sig, elem));
            }
        }
    }
    None
}

struct FunctionEmitter<'a> {
    function: &'a lower::Function,
    ids: &'a TapeIds,
    abi: &'a AbiTables,
    builder: &'a mut ProgramBuilder,
    asm: Asm,
    labels: Vec<Label>,
}

impl<'a> FunctionEmitter<'a> {
    fn new(
        function: &'a lower::Function,
        ids: &'a TapeIds,
        abi: &'a AbiTables,
        builder: &'a mut ProgramBuilder,
    ) -> Self {
        let mut asm = builder
            .function_asm(ids.functions[function.id.0 as usize])
            .expect("function was declared before body emission");
        for kind in function.register_kinds.iter().skip(function.params.len()) {
            asm.alloc_register(value_type(*kind));
        }
        let labels = labels_for(function, &mut asm);
        Self {
            function,
            ids,
            abi,
            builder,
            asm,
            labels,
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
            lower::Instr::ConstF64 { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_f64(dst.0, value.to_f64());
            }
            lower::Instr::ConstBool { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_bool(dst.0, *value);
            }
            lower::Instr::StringLiteral {
                dst, value, span, ..
            } => {
                let (sig, value_const) = self.abi.string_literal(value)?;
                let value_reg = self.alloc_temp(ValueType::Str);
                self.set_span(*span);
                self.asm.const_pool(value_reg, value_const);
                self.asm.host_call(0, sig, 0, &[value_reg], &[dst.0]);
            }
            lower::Instr::SymbolLiteral {
                dst, name, span, ..
            } => {
                let (sig, name_const) = self.abi.symbol_literal(name)?;
                let name_reg = self.alloc_temp(ValueType::Str);
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
            lower::Instr::ConstFunction {
                dst,
                function,
                span,
            } => {
                let function = self
                    .ids
                    .functions
                    .get(function.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_lowered("function id is out of range"))?;
                self.set_span(*span);
                self.asm.const_func(dst.0, function);
            }
            lower::Instr::ClosureNew {
                dst,
                function,
                captures,
                span,
            } => {
                let function = self
                    .ids
                    .functions
                    .get(function.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_lowered("closure function id is out of range"))?;
                let func_reg = self.alloc_temp(ValueType::Func);
                let env_reg = self.alloc_temp(ValueType::Agg);
                self.set_span(*span);
                self.asm.const_func(func_reg, function);
                self.asm.tuple_new(env_reg, &reg_slice(captures));
                self.asm.closure_new(dst.0, func_reg, env_reg);
            }
            lower::Instr::CaptureGet {
                dst,
                env,
                index,
                span,
                ..
            } => {
                self.set_span(*span);
                self.asm
                    .tuple_get(dst.0, env.0, u32::try_from(*index).unwrap_or(u32::MAX));
            }
            lower::Instr::BoxValue {
                dst,
                src,
                from,
                span,
            } => {
                let sig = match from {
                    lower::ValueKind::I64 => self.abi.box_i64,
                    lower::ValueKind::F64 => self.abi.box_f64,
                    lower::ValueKind::Bool => self.abi.box_bool,
                    lower::ValueKind::Object(_)
                    | lower::ValueKind::ClassObject(_)
                    | lower::ValueKind::GenericFunctionObject(_)
                    | lower::ValueKind::MethodObject(_)
                    | lower::ValueKind::SlotDescriptorObject => self.abi.box_object,
                    lower::ValueKind::Any
                    | lower::ValueKind::U64
                    | lower::ValueKind::Function(_)
                    | lower::ValueKind::Closure(_)
                    | lower::ValueKind::Aggregate => {
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
                    sem::BinaryOp::Div => {
                        return Err(invalid_lowered("integer division has no tape lowering"));
                    }
                }
            }
            lower::Instr::F64Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, a, b) = (dst.0, lhs.0, rhs.0);
                match op {
                    lower::F64BinaryOp::Add => self.asm.f64_add(dst, a, b),
                    lower::F64BinaryOp::Sub => self.asm.f64_sub(dst, a, b),
                    lower::F64BinaryOp::Mul => self.asm.f64_mul(dst, a, b),
                    lower::F64BinaryOp::Div => self.asm.f64_div(dst, a, b),
                    lower::F64BinaryOp::Rem => self.asm.f64_rem(dst, a, b),
                    lower::F64BinaryOp::Min => self.asm.f64_min(dst, a, b),
                    lower::F64BinaryOp::Max => self.asm.f64_max(dst, a, b),
                };
            }
            lower::Instr::F64Unary { dst, op, src, span } => {
                self.set_span(*span);
                match op {
                    lower::F64UnaryOp::Neg => self.asm.f64_neg(dst.0, src.0),
                    lower::F64UnaryOp::Abs => self.asm.f64_abs(dst.0, src.0),
                };
            }
            lower::Instr::FloatCall {
                dst,
                function,
                args,
                span,
            } => {
                let sig = self.abi.float_function(*function)?;
                let args = reg_slice(args);
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &args, &[dst.0]);
            }
            lower::Instr::BoolNot { dst, src, span } => {
                self.set_span(*span);
                self.asm.bool_not(dst.0, src.0);
            }
            lower::Instr::I64ToF64 { dst, src, span } => {
                self.set_span(*span);
                self.asm.i64_to_f64(dst.0, src.0);
            }
            lower::Instr::F64ToI64 { dst, src, span } => {
                self.set_span(*span);
                self.asm.f64_to_i64(dst.0, src.0);
            }
            lower::Instr::F64Compare {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, a, b) = (dst.0, lhs.0, rhs.0);
                match op {
                    sem::CompareOp::Eq => self.asm.f64_eq(dst, a, b),
                    sem::CompareOp::Lt => self.asm.f64_lt(dst, a, b),
                    sem::CompareOp::Le => self.asm.f64_le(dst, a, b),
                    sem::CompareOp::Gt => self.asm.f64_gt(dst, a, b),
                    sem::CompareOp::Ge => self.asm.f64_ge(dst, a, b),
                };
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
                    .host_call(0, self.abi.value_eq, 0, &[lhs.0, rhs.0], &[dst.0]);
            }
            lower::Instr::Call {
                dsts,
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
                let dsts = reg_slice(dsts);
                self.set_span(*span);
                self.asm.call(0, callee, 0, &args, &dsts);
            }
            lower::Instr::CallIndirect {
                dsts,
                callee,
                args,
                arg_kinds,
                rets,
                span,
            } => {
                let args_types = arg_kinds
                    .iter()
                    .map(|kind| value_type(*kind))
                    .collect::<Vec<_>>();
                let ret_types = rets.iter().copied().map(value_type).collect::<Vec<_>>();
                let sig = self
                    .builder
                    .call_sig(FunctionSig {
                        arg_types: args_types,
                        ret_types,
                    })
                    .map_err(build_error)?;
                let args = reg_slice(args);
                let dsts = reg_slice(dsts);
                self.set_span(*span);
                self.asm.call_indirect(0, sig, callee.0, 0, &args, &dsts);
            }
            lower::Instr::MakeObject {
                dst,
                class,
                fields,
                span,
            } => {
                if class.0 as usize >= self.abi.class_count {
                    return Err(invalid_lowered("class id is out of range"));
                }
                let class_reg = self.alloc_temp(ValueType::U64);
                let fields_reg = self.alloc_temp(ValueType::Agg);
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                self.asm.tuple_new(fields_reg, &reg_slice(fields));
                self.asm.host_call(
                    0,
                    self.abi.make_object,
                    0,
                    &[class_reg, fields_reg],
                    &[dst.0],
                );
            }
            lower::Instr::VectorLiteral {
                dst,
                elements,
                span,
                ..
            } => {
                let (sig, elem) = self.abi.vector_literal()?;
                let elements = reg_slice(elements);
                let aggregate = self.alloc_temp(ValueType::Agg);
                self.set_span(*span);
                self.asm.array_new(aggregate, elem, &elements);
                self.asm.host_call(0, sig, 0, &[aggregate], &[dst.0]);
            }
            lower::Instr::RuntimeCall {
                dst,
                op,
                args,
                span,
                ..
            } => {
                let sig = self.abi.runtime_call(*op)?;
                let args = reg_slice(args);
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &args, &[dst.0]);
            }
            lower::Instr::HostCall {
                dst,
                function,
                args,
                span,
                ..
            } => {
                let sig = self.abi.host_function(*function)?;
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
                    .abi
                    .slot_get
                    .get(class.0 as usize)
                    .and_then(|slots| slots.get(slot.0 as usize))
                    .copied()
                    .ok_or_else(|| invalid_lowered("slot_get signature is out of range"))?;
                let slot_reg = self.alloc_temp(ValueType::U64);
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
                    .abi
                    .slot_set
                    .get(class.0 as usize)
                    .and_then(|slots| slots.get(slot.0 as usize))
                    .copied()
                    .ok_or_else(|| invalid_lowered("slot_set signature is out of range"))?;
                let slot_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(slot_reg, u64::from(slot.0));
                self.asm
                    .host_call(0, sig, 0, &[receiver.0, slot_reg, value.0], &[dst.0]);
            }
            lower::Instr::ClassOf { dst, object, span } => {
                self.set_span(*span);
                self.asm
                    .host_call(0, self.abi.class_of, 0, &[object.0], &[dst.0]);
            }
            lower::Instr::InstanceOf {
                dst,
                value,
                input,
                class,
                span,
            } => {
                let sig = self.instance_sig(*input)?;
                let class_reg = self.alloc_temp(ValueType::U64);
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
                let class_reg = self.alloc_temp(ValueType::U64);
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
            lower::Instr::Return { values, span } => {
                let values = reg_slice(values);
                self.set_span(*span);
                self.asm.ret(0, &values);
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

    fn alloc_temp(&mut self, kind: ValueType) -> u32 {
        self.asm.alloc_register(kind)
    }

    fn set_span(&mut self, span: Span) {
        if let Some(span_id) = span_id(span) {
            self.asm.span(span_id);
        }
    }

    fn instance_sig(&self, kind: lower::ValueKind) -> Result<HostSigId, TapeError> {
        match kind {
            lower::ValueKind::Any => Ok(self.abi.instance_any),
            lower::ValueKind::Object(_) => Ok(self.abi.instance_object),
            _ => Err(invalid_lowered("instance? input kind is invalid")),
        }
    }

    fn cast_sig(&self, kind: lower::ValueKind) -> Result<HostSigId, TapeError> {
        match kind {
            lower::ValueKind::Any => Ok(self.abi.cast_any),
            lower::ValueKind::Object(_) => Ok(self.abi.cast_object),
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
    use alloc::string::ToString;

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
    fn emits_known_callable_apply_through_indirect_call_signature() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  apply(f, 2)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let program = tape.program.program();

        assert_eq!(program.call_sigs.len(), 1);
        assert_eq!(
            program
                .call_sig_args(&program.call_sigs[0])
                .expect("indirect call args should decode"),
            &[ValueType::I64],
        );
        assert_eq!(
            program
                .call_sig_rets(&program.call_sigs[0])
                .expect("indirect call rets should decode"),
            &[ValueType::I64],
        );

        let disassembly = execution_tape::disasm::disassemble_verified(&tape.program).to_string();
        assert!(disassembly.contains("const.func"));
        assert!(disassembly.contains("call.indirect"));
    }

    #[test]
    fn emits_generic_callable_apply_through_indirect_dispatcher() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  let g = area;\n  apply(g, make(<circle>, radius: 5))\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let program = tape.program.program();

        assert_eq!(program.call_sigs.len(), 1);
        assert_eq!(
            program
                .call_sig_args(&program.call_sigs[0])
                .expect("indirect generic args should decode"),
            &[runtime::object_type()],
        );
        assert_eq!(
            program
                .call_sig_rets(&program.call_sigs[0])
                .expect("indirect generic rets should decode"),
            &[ValueType::I64],
        );

        let disassembly = execution_tape::disasm::disassemble_verified(&tape.program).to_string();
        assert!(disassembly.contains("name=\"area#dispatch\""));
        assert!(disassembly.contains("const.func"));
        assert!(disassembly.contains("call.indirect"));
    }

    #[test]
    fn emits_runtime_callable_parameter_apply_through_indirect_call() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function callit(f :: <callable>) => <integer>\n  apply(f, 41)\nend;\n\
             define function main()\n  callit(inc)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let program = tape.program.program();

        assert_eq!(
            program
                .function_arg_types(&program.functions[1])
                .expect("callit args should decode"),
            &[ValueType::Closure],
        );
        let disassembly = execution_tape::disasm::disassemble_verified(&tape.program).to_string();
        assert!(disassembly.contains("name=\"callit\""));
        assert!(disassembly.contains("const.func"));
        assert!(disassembly.contains("closure.new"));
        assert!(disassembly.contains("call.indirect"));
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

    #[test]
    fn emits_object_construction_through_one_aggregate_host_signature() {
        let module = lower_source(
            "define class <point> (<object>)\n  slot x :: <integer>;\n  slot y :: <integer>;\nend;\n\
             define class <box> (<object>)\n  slot value;\nend;\n\
             define function point()\n  make(<point>, x: 1, y: 2)\nend;\n\
             define function box()\n  make(<box>, value: 3)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let program = tape.program.program();
        let make_sigs: Vec<_> = program
            .host_sigs
            .iter()
            .filter(|sig| {
                program
                    .symbol_str(sig.symbol)
                    .is_ok_and(|symbol| symbol == runtime::MAKE_OBJECT)
            })
            .collect();

        assert_eq!(make_sigs.len(), 1);
        assert_eq!(
            program
                .host_sig_args(make_sigs[0])
                .expect("make_object host args should decode"),
            &[ValueType::U64, ValueType::Agg],
        );
        assert_eq!(
            program
                .host_sig_rets(make_sigs[0])
                .expect("make_object host rets should decode"),
            &[runtime::object_type()],
        );
    }

    #[test]
    fn emits_vector_literals_through_one_aggregate_host_signature() {
        let module = lower_source(
            "define function first()\n  #[1, 2]\nend;\n\
             define function second()\n  #[3]\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let program = tape.program.program();
        let vector_sigs: Vec<_> = program
            .host_sigs
            .iter()
            .filter(|sig| {
                program
                    .symbol_str(sig.symbol)
                    .is_ok_and(|symbol| symbol == runtime::VECTOR_LITERAL)
            })
            .collect();

        assert_eq!(vector_sigs.len(), 1);
        assert_eq!(
            program
                .host_sig_args(vector_sigs[0])
                .expect("vector host args should decode"),
            &[ValueType::Agg],
        );
        assert_eq!(
            program
                .host_sig_rets(vector_sigs[0])
                .expect("vector host rets should decode"),
            &[runtime::object_type()],
        );
    }
}
