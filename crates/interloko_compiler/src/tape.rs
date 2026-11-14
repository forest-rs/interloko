//! Adapter from lowered Interloko IR to `execution_tape`.
//!
//! This module owns only bytecode emission for already-lowered Interloko functions.
//! Generic dispatch, object construction, slot reads, and slot writes arrive
//! here as lowered instructions and are encoded with ordinary calls, branches,
//! constants, and host calls.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::asm::{Asm, Catch, FunctionAbi, FunctionSig, Label, ProgramBuilder};
use execution_tape::contract::{Contract, ContractId};
use execution_tape::host::HostSig;
use execution_tape::program::{
    CallSigId, ConstId, ElemTypeId, HostSigId, SpanId, TagId, ValueType,
};
use execution_tape::value::FuncId;
use execution_tape::verifier::VerifiedProgram;

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::lower;
use crate::sem;
use crate::syntax::Span;
use interloko_runtime::code::CodeUnit;
use interloko_runtime::control::ControlLayout;
use interloko_runtime::{self as runtime, RuntimeLayout};

/// A verified tape program emitted from lowered Interloko IR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TapeModule {
    /// The verified program with its runtime layout and link metadata, as
    /// the runtime installs it.
    ///
    /// Shared so artifact snapshots, runs and execution recordings refer to
    /// one prepared program instead of copying it.
    pub code: Arc<CodeUnit>,
    /// `execution_tape` function ids in lowered function order.
    pub functions: Vec<FuncId>,
    /// `execution_tape` function id for `main`, if present.
    pub main: Option<FuncId>,
}

impl TapeModule {
    /// Verified executable tape program.
    #[must_use]
    pub fn program(&self) -> &Arc<VerifiedProgram> {
        &self.code.program
    }

    /// Runtime layout required by the Interloko host.
    #[must_use]
    pub fn runtime(&self) -> &RuntimeLayout {
        &self.code.layout
    }
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
    let mut code = CodeUnit {
        program: Arc::new(program),
        layout: module.runtime.clone(),
        control: ControlLayout {
            restarts: module.restarts.clone(),
            conditions: abi
                .control
                .as_ref()
                .map(|control| control.conditions.iter().map(|(_, tag)| *tag).collect())
                .unwrap_or_default(),
        },
        generics: vec![FuncId(0); module.runtime.generics.len()],
        methods: vec![FuncId(0); module.runtime.methods.len()],
        functions: Vec::new(),
        dispatch: module.dispatch.clone(),
    };
    for (function, func) in module.functions.iter().zip(&ids.functions) {
        let slot = match function.kind {
            lower::FunctionKind::Dispatcher(generic) => code.generics.get_mut(generic.0 as usize),
            lower::FunctionKind::Method(method) => code.methods.get_mut(method.0 as usize),
            lower::FunctionKind::Function(_) => {
                code.functions.push((function.name.clone(), *func));
                continue;
            }
            _ => continue,
        };
        *slot.ok_or_else(|| invalid_lowered("dispatcher or method is not in the layout"))? = *func;
    }
    Ok(TapeModule {
        code: Arc::new(code),
        functions: ids.functions,
        main,
    })
}

fn value_type(kind: lower::ValueKind) -> ValueType {
    match kind {
        lower::ValueKind::Any => runtime::any_type(),
        lower::ValueKind::I64 => ValueType::I64,
        lower::ValueKind::F64 => ValueType::F64,
        lower::ValueKind::I32 => ValueType::I32,
        lower::ValueKind::U32 => ValueType::U32,
        lower::ValueKind::F32 => ValueType::F32,
        lower::ValueKind::U64 => ValueType::U64,
        lower::ValueKind::Bool => ValueType::Bool,
        lower::ValueKind::Function(_) => ValueType::Func,
        lower::ValueKind::Closure(_) => ValueType::Closure,
        lower::ValueKind::Aggregate => ValueType::Agg,
        lower::ValueKind::Exn => ValueType::Exn,
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
    if !matches!(
        function.kind,
        lower::FunctionKind::Closure(_) | lower::FunctionKind::ExitThunk { .. }
    ) {
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
    class_of_any: HostSigId,
    unbox_i64: HostSigId,
    unbox_f64: HostSigId,
    unbox_f32: HostSigId,
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
    box_f32: HostSigId,
    box_bool: HostSigId,
    box_object: HostSigId,
    instance_any: HostSigId,
    instance_object: HostSigId,
    cast_any: HostSigId,
    cast_object: HostSigId,
    tags: SharedTags,
    control: Option<ControlAbi>,
}

/// Control tags every program imports by name, so that code units installed
/// in one VM throw and catch the same exceptions.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedTags {
    restart: TagId,
    retry: TagId,
}

impl SharedTags {
    fn import(builder: &mut ProgramBuilder) -> Result<Self, TapeError> {
        let restart = builder
            .import_tag(
                runtime::control::RESTART_TAG,
                &runtime::control::restart_payload().map(Into::into),
            )
            .map_err(build_error)?;
        let retry = builder
            .import_tag(
                runtime::control::RETRY_TAG,
                &runtime::control::retry_payload().map(Into::into),
            )
            .map_err(build_error)?;
        Ok(Self { restart, retry })
    }
}

/// Host signatures and tags for dynamic control, declared only when a module
/// establishes handlers, signals or exit points. Cleanup-only regions need
/// neither.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlAbi {
    push_handler: HostSigId,
    push_guarded_handler: HostSigId,
    pop: HostSigId,
    push_exit: HostSigId,
    check_exit: HostSigId,
    signal: HostSigId,
    guard: HostSigId,
    guard_result: HostSigId,
    guard_unwound: HostSigId,
    /// A guard's call: `(condition) -> <boolean>`.
    guard_call: CallSigId,
    exits: Vec<(lower::CallableSigId, TagId)>,
    /// Condition tags by the result types of the signals that suspend on
    /// them.
    conditions: Vec<(Vec<ValueType>, TagId)>,
}

impl ControlAbi {
    fn declare(
        builder: &mut ProgramBuilder,
        module: &lower::Module,
    ) -> Result<Option<Self>, TapeError> {
        let mut uses_control = false;
        let mut exit_sigs = Vec::new();
        let mut signal_results = Vec::<Vec<ValueType>>::new();
        let mut note_tag = |tag: lower::ControlTag| {
            if let lower::ControlTag::Exit(signature) = tag
                && !exit_sigs.contains(&signature)
            {
                exit_sigs.push(signature);
            }
        };
        for (function, instr) in module.functions.iter().flat_map(|function| {
            function
                .instructions
                .iter()
                .map(move |instr| (function, instr))
        }) {
            match instr {
                lower::Instr::Signal { dsts, .. } => {
                    uses_control = true;
                    let results = dsts
                        .iter()
                        .map(|dst| register_type(function, *dst))
                        .collect::<Result<Vec<_>, _>>()?;
                    if !signal_results.contains(&results) {
                        signal_results.push(results);
                    }
                }
                lower::Instr::PushHandler { .. }
                | lower::Instr::PopBinding { .. }
                | lower::Instr::PushExit { .. }
                | lower::Instr::CheckExit { .. } => uses_control = true,
                lower::Instr::Throw { tag, .. } if *tag != lower::ControlTag::Retry => {
                    uses_control = true;
                    note_tag(*tag);
                }
                lower::Instr::EnterRegion { catches } => {
                    for catch in catches {
                        // Every function body catches retries; that alone
                        // needs no control host calls.
                        if let lower::Catch::Tag { tag, .. } = catch
                            && *tag != lower::ControlTag::Retry
                        {
                            uses_control = true;
                            note_tag(*tag);
                        }
                    }
                }
                _ => {}
            }
        }
        if !uses_control {
            return Ok(None);
        }
        let exits = exit_sigs
            .into_iter()
            .map(|signature| {
                let params = module
                    .callable_signature(signature)
                    .ok_or_else(|| invalid_lowered("exit signature is missing"))?
                    .params
                    .iter()
                    .map(|kind| value_type(*kind).into());
                let payload = core::iter::once(ValueType::U64.into())
                    .chain(params)
                    .collect::<Vec<_>>();
                let tag = builder
                    .declare_tag(&format!("interloko.exit#{}", signature.0), &payload)
                    .map_err(build_error)?;
                Ok((signature, tag))
            })
            .collect::<Result<Vec<_>, TapeError>>()?;
        let payload = runtime::control::condition_payload().map(Into::into);
        let conditions = signal_results
            .into_iter()
            .enumerate()
            .map(|(index, results)| {
                let result_abi = results.iter().cloned().map(Into::into).collect::<Vec<_>>();
                let tag = builder
                    .declare_tag_with_results(
                        &format!("interloko.condition#{index}"),
                        &payload,
                        &result_abi,
                    )
                    .map_err(build_error)?;
                Ok((results, tag))
            })
            .collect::<Result<Vec<_>, TapeError>>()?;
        let guard_call = builder
            .call_sig(FunctionSig {
                arg_types: vec![runtime::object_type()],
                ret_types: vec![ValueType::Bool],
            })
            .map_err(build_error)?;
        Ok(Some(Self {
            signal: builder.host_sig_for(
                runtime::control::CONDITION_SIGNAL,
                runtime::control::signal_signature(),
            ),
            guard: builder.host_sig_for(
                runtime::control::CONDITION_GUARD,
                runtime::control::guard_signature(),
            ),
            guard_result: builder.host_sig_for(
                runtime::control::CONDITION_GUARD_RESULT,
                runtime::control::guard_result_signature(),
            ),
            guard_unwound: builder.host_sig_for(
                runtime::control::CONDITION_GUARD_UNWOUND,
                runtime::control::guard_unwound_signature(),
            ),
            guard_call,
            conditions,
            push_handler: builder.host_sig_for(
                runtime::control::CONDITION_PUSH_HANDLER,
                runtime::control::push_handler_signature(),
            ),
            push_guarded_handler: builder.host_sig_for(
                runtime::control::CONDITION_PUSH_HANDLER,
                runtime::control::push_guarded_handler_signature(),
            ),
            pop: builder.host_sig_for(
                runtime::control::CONTROL_POP,
                runtime::control::pop_signature(),
            ),
            push_exit: builder.host_sig_for(
                runtime::control::CONTROL_PUSH_EXIT,
                runtime::control::push_exit_signature(),
            ),
            check_exit: builder.host_sig_for(
                runtime::control::CONTROL_CHECK_EXIT,
                runtime::control::check_exit_signature(),
            ),
            exits,
        }))
    }

    fn condition_tag(&self, results: &[ValueType]) -> Result<TagId, TapeError> {
        self.conditions
            .iter()
            .find(|(candidate, _)| candidate == results)
            .map(|(_, tag)| *tag)
            .ok_or_else(|| invalid_lowered("condition tag is missing"))
    }

    fn exit_tag(&self, signature: lower::CallableSigId) -> Result<TagId, TapeError> {
        self.exits
            .iter()
            .find(|(candidate, _)| *candidate == signature)
            .map(|(_, tag)| *tag)
            .ok_or_else(|| invalid_lowered("exit tag is missing"))
    }
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
        let class_of_any =
            builder.host_sig_for(runtime::CLASS_OF, runtime::class_of_any_signature());
        let unbox_i64 = builder.host_sig_for(
            runtime::UNBOX_VALUE,
            runtime::unbox_value_signature(ValueType::I64),
        );
        let unbox_f64 = builder.host_sig_for(
            runtime::UNBOX_VALUE,
            runtime::unbox_value_signature(ValueType::F64),
        );
        let unbox_f32 = builder.host_sig_for(
            runtime::UNBOX_VALUE,
            runtime::unbox_value_signature(ValueType::F32),
        );
        let box_i64 = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(ValueType::I64),
        );
        let box_f64 = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(ValueType::F64),
        );
        let box_f32 = builder.host_sig_for(
            runtime::BOX_VALUE,
            runtime::box_value_signature(ValueType::F32),
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
        let tags = SharedTags::import(builder)?;
        let control = ControlAbi::declare(builder, module)?;
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
            class_of_any,
            unbox_i64,
            unbox_f64,
            unbox_f32,
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
            box_f32,
            box_bool,
            box_object,
            instance_any,
            instance_object,
            cast_any,
            cast_object,
            tags,
            control,
        })
    }

    fn tag(&self, tag: lower::ControlTag) -> Result<TagId, TapeError> {
        match tag {
            lower::ControlTag::Restart => Ok(self.tags.restart),
            lower::ControlTag::Retry => Ok(self.tags.retry),
            lower::ControlTag::Exit(signature) => self.control()?.exit_tag(signature),
        }
    }

    fn control(&self) -> Result<&ControlAbi, TapeError> {
        self.control
            .as_ref()
            .ok_or_else(|| invalid_lowered("control signatures are missing"))
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
            RuntimeOp::FloatToString,
            runtime::FLOAT_TO_STRING,
            runtime::float_to_string_signature(),
        ),
        (
            RuntimeOp::SingleFloatToString,
            runtime::FLOAT_TO_STRING,
            runtime::single_float_to_string_signature(),
        ),
        (
            RuntimeOp::IntegerToString,
            runtime::INTEGER_TO_STRING,
            runtime::integer_to_string_signature(),
        ),
        (
            RuntimeOp::Concatenate,
            runtime::STRING_CONCATENATE,
            runtime::string_concatenate_signature(),
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
            lower::Instr::Const32 { dst, value, span } => {
                self.set_span(*span);
                match value {
                    sem::Number32::I32(value) => self.asm.const_i32(dst.0, *value),
                    sem::Number32::U32(value) => self.asm.const_u32(dst.0, *value),
                    sem::Number32::F32(bits) => self.asm.const_f32_bits(dst.0, bits.to_bits()),
                };
            }
            lower::Instr::Num32Unary { dst, op, src, span } => {
                self.set_span(*span);
                let (dst, src) = (dst.0, src.0);
                match op {
                    lower::Num32UnaryOp::F32Neg => self.asm.f32_neg(dst, src),
                    lower::Num32UnaryOp::F32Abs => self.asm.f32_abs(dst, src),
                    lower::Num32UnaryOp::F32Floor => self.asm.f32_floor(dst, src),
                    lower::Num32UnaryOp::F32Ceil => self.asm.f32_ceil(dst, src),
                    lower::Num32UnaryOp::F32Trunc => self.asm.f32_trunc(dst, src),
                    lower::Num32UnaryOp::F32Nearest => self.asm.f32_nearest(dst, src),
                    lower::Num32UnaryOp::F32Sqrt => self.asm.f32_sqrt(dst, src),
                    lower::Num32UnaryOp::I32ToI64 => self.asm.i32_to_i64(dst, src),
                    lower::Num32UnaryOp::U32ToI64 => self.asm.u32_to_i64(dst, src),
                    lower::Num32UnaryOp::I64ToI32 => self.asm.i64_to_i32(dst, src),
                    lower::Num32UnaryOp::I64ToU64Wrap => self.asm.i64_to_u64_wrap(dst, src),
                    lower::Num32UnaryOp::U64ToU32 => self.asm.u64_to_u32(dst, src),
                    lower::Num32UnaryOp::I32ToU32 => self.asm.i32_to_u32(dst, src),
                    lower::Num32UnaryOp::U32ToI32 => self.asm.u32_to_i32(dst, src),
                    lower::Num32UnaryOp::F32ToF64 => self.asm.f32_to_f64(dst, src),
                    lower::Num32UnaryOp::F64ToF32 => self.asm.f64_to_f32(dst, src),
                    lower::Num32UnaryOp::F32ToI32 => self.asm.f32_to_i32(dst, src),
                    lower::Num32UnaryOp::F32ToU32 => self.asm.f32_to_u32(dst, src),
                    lower::Num32UnaryOp::F64ToI32 => self.asm.f64_to_i32(dst, src),
                    lower::Num32UnaryOp::F64ToU32 => self.asm.f64_to_u32(dst, src),
                    lower::Num32UnaryOp::I32ToF32 => self.asm.i32_to_f32(dst, src),
                    lower::Num32UnaryOp::U32ToF32 => self.asm.u32_to_f32(dst, src),
                    lower::Num32UnaryOp::I64ToF32 => self.asm.i64_to_f32(dst, src),
                    lower::Num32UnaryOp::I32ToF64 => self.asm.i32_to_f64(dst, src),
                    lower::Num32UnaryOp::U32ToF64 => self.asm.u32_to_f64(dst, src),
                };
            }
            lower::Instr::Num32Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, a, b) = (dst.0, lhs.0, rhs.0);
                match op {
                    lower::Num32BinaryOp::I32Add => self.asm.i32_add(dst, a, b),
                    lower::Num32BinaryOp::I32Sub => self.asm.i32_sub(dst, a, b),
                    lower::Num32BinaryOp::I32Mul => self.asm.i32_mul(dst, a, b),
                    lower::Num32BinaryOp::I32And => self.asm.i32_and(dst, a, b),
                    lower::Num32BinaryOp::I32Or => self.asm.i32_or(dst, a, b),
                    lower::Num32BinaryOp::I32Xor => self.asm.i32_xor(dst, a, b),
                    lower::Num32BinaryOp::I32Shl => self.asm.i32_shl(dst, a, b),
                    lower::Num32BinaryOp::I32Shr => self.asm.i32_shr(dst, a, b),
                    lower::Num32BinaryOp::I32Eq => self.asm.i32_eq(dst, a, b),
                    lower::Num32BinaryOp::I32Lt => self.asm.i32_lt(dst, a, b),
                    lower::Num32BinaryOp::I32Gt => self.asm.i32_gt(dst, a, b),
                    lower::Num32BinaryOp::I32Le => self.asm.i32_le(dst, a, b),
                    lower::Num32BinaryOp::I32Ge => self.asm.i32_ge(dst, a, b),
                    lower::Num32BinaryOp::U32Add => self.asm.u32_add(dst, a, b),
                    lower::Num32BinaryOp::U32Sub => self.asm.u32_sub(dst, a, b),
                    lower::Num32BinaryOp::U32Mul => self.asm.u32_mul(dst, a, b),
                    lower::Num32BinaryOp::U32And => self.asm.u32_and(dst, a, b),
                    lower::Num32BinaryOp::U32Or => self.asm.u32_or(dst, a, b),
                    lower::Num32BinaryOp::U32Xor => self.asm.u32_xor(dst, a, b),
                    lower::Num32BinaryOp::U32Shl => self.asm.u32_shl(dst, a, b),
                    lower::Num32BinaryOp::U32Shr => self.asm.u32_shr(dst, a, b),
                    lower::Num32BinaryOp::U32Eq => self.asm.u32_eq(dst, a, b),
                    lower::Num32BinaryOp::U32Lt => self.asm.u32_lt(dst, a, b),
                    lower::Num32BinaryOp::U32Gt => self.asm.u32_gt(dst, a, b),
                    lower::Num32BinaryOp::U32Le => self.asm.u32_le(dst, a, b),
                    lower::Num32BinaryOp::U32Ge => self.asm.u32_ge(dst, a, b),
                    lower::Num32BinaryOp::F32Add => self.asm.f32_add(dst, a, b),
                    lower::Num32BinaryOp::F32Sub => self.asm.f32_sub(dst, a, b),
                    lower::Num32BinaryOp::F32Mul => self.asm.f32_mul(dst, a, b),
                    lower::Num32BinaryOp::F32Div => self.asm.f32_div(dst, a, b),
                    lower::Num32BinaryOp::F32Rem => self.asm.f32_rem(dst, a, b),
                    lower::Num32BinaryOp::F32Min => self.asm.f32_min(dst, a, b),
                    lower::Num32BinaryOp::F32Max => self.asm.f32_max(dst, a, b),
                    lower::Num32BinaryOp::F32Eq => self.asm.f32_eq(dst, a, b),
                    lower::Num32BinaryOp::F32Ne => self.asm.f32_ne(dst, a, b),
                    lower::Num32BinaryOp::F32Lt => self.asm.f32_lt(dst, a, b),
                    lower::Num32BinaryOp::F32Gt => self.asm.f32_gt(dst, a, b),
                    lower::Num32BinaryOp::F32Le => self.asm.f32_le(dst, a, b),
                    lower::Num32BinaryOp::F32Ge => self.asm.f32_ge(dst, a, b),
                };
            }
            lower::Instr::Num32Checked {
                dst,
                overflow,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, overflow, a, b) = (dst.0, overflow.0, lhs.0, rhs.0);
                match op {
                    lower::Num32CheckedOp::I32AddOvf => self.asm.i32_add_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::I32SubOvf => self.asm.i32_sub_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::I32MulOvf => self.asm.i32_mul_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::U32AddOvf => self.asm.u32_add_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::U32SubOvf => self.asm.u32_sub_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::U32MulOvf => self.asm.u32_mul_ovf(dst, overflow, a, b),
                };
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
                    lower::ValueKind::F32 => self.abi.box_f32,
                    lower::ValueKind::Bool => self.abi.box_bool,
                    lower::ValueKind::Object(_)
                    | lower::ValueKind::ClassObject(_)
                    | lower::ValueKind::GenericFunctionObject(_)
                    | lower::ValueKind::MethodObject(_)
                    | lower::ValueKind::SlotDescriptorObject => self.abi.box_object,
                    // `<i32>` and `<u32>` widen to the `<integer>` they
                    // are before boxing.
                    lower::ValueKind::Any
                    | lower::ValueKind::I32
                    | lower::ValueKind::U32
                    | lower::ValueKind::U64
                    | lower::ValueKind::Function(_)
                    | lower::ValueKind::Closure(_)
                    | lower::ValueKind::Aggregate
                    | lower::ValueKind::Exn => {
                        return Err(invalid_lowered("invalid boxed value input kind"));
                    }
                };
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &[src.0], &[dst.0]);
            }
            lower::Instr::I64Binary {
                dst,
                overflow,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, overflow, a, b) = (dst.0, overflow.0, lhs.0, rhs.0);
                match op {
                    lower::I64ArithOp::Add => self.asm.i64_add_ovf(dst, overflow, a, b),
                    lower::I64ArithOp::Sub => self.asm.i64_sub_ovf(dst, overflow, a, b),
                    lower::I64ArithOp::Mul => self.asm.i64_mul_ovf(dst, overflow, a, b),
                };
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
            lower::Instr::I64Bitwise {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, a, b) = (dst.0, lhs.0, rhs.0);
                match op {
                    lower::I64BitOp::And => self.asm.i64_and(dst, a, b),
                    lower::I64BitOp::Or => self.asm.i64_or(dst, a, b),
                    lower::I64BitOp::Xor => self.asm.i64_xor(dst, a, b),
                };
            }
            lower::Instr::F64Unary { dst, op, src, span } => {
                self.set_span(*span);
                match op {
                    lower::F64UnaryOp::Neg => self.asm.f64_neg(dst.0, src.0),
                    lower::F64UnaryOp::Abs => self.asm.f64_abs(dst.0, src.0),
                    lower::F64UnaryOp::Floor => self.asm.f64_floor(dst.0, src.0),
                    lower::F64UnaryOp::Ceil => self.asm.f64_ceil(dst.0, src.0),
                    lower::F64UnaryOp::Trunc => self.asm.f64_trunc(dst.0, src.0),
                    lower::F64UnaryOp::Nearest => self.asm.f64_nearest(dst.0, src.0),
                    lower::F64UnaryOp::Sqrt => self.asm.f64_sqrt(dst.0, src.0),
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
            lower::Instr::F64NotEqual {
                dst,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                self.asm.f64_ne(dst.0, lhs.0, rhs.0);
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
            lower::Instr::SelectMethod {
                dst,
                generic,
                args,
                span,
            } => {
                let arg_types = args
                    .iter()
                    .map(|arg| self.register_type(*arg))
                    .collect::<Result<Vec<_>, _>>()?;
                let sig = self.builder.host_sig_for(
                    runtime::code::CODE_DISPATCH,
                    runtime::code::dispatch_signature(&arg_types),
                );
                let generic_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(generic_reg, u64::from(generic.0));
                let mut operands = Vec::with_capacity(args.len() + 1);
                operands.push(generic_reg);
                operands.extend(args.iter().map(|arg| arg.0));
                self.asm.host_call(0, sig, 0, &operands, &[dst.0]);
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
            lower::Instr::ClassOf {
                dst,
                object,
                input,
                span,
            } => {
                let sig = if *input == lower::ValueKind::Any {
                    self.abi.class_of_any
                } else {
                    self.abi.class_of
                };
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &[object.0], &[dst.0]);
            }
            lower::Instr::UnboxValue {
                dst,
                value,
                class,
                kind,
                span,
            } => {
                let sig = match kind {
                    lower::ValueKind::I64 => self.abi.unbox_i64,
                    lower::ValueKind::F64 => self.abi.unbox_f64,
                    lower::ValueKind::F32 => self.abi.unbox_f32,
                    _ => return Err(invalid_lowered("unbox result kind is not a scalar")),
                };
                let class_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                self.asm
                    .host_call(0, sig, 0, &[value.0, class_reg], &[dst.0]);
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
            lower::Instr::Trap { trap, span } => {
                self.set_span(*span);
                self.asm.trap(trap.code());
            }
            lower::Instr::PushHandler {
                dst,
                descriptor,
                guard,
                span,
            } => {
                let control = self.abi.control()?;
                let (plain, guarded) = (control.push_handler, control.push_guarded_handler);
                let descriptor_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(
                    descriptor_reg,
                    u64::try_from(*descriptor).unwrap_or(u64::MAX),
                );
                match guard {
                    Some(guard) => {
                        self.asm
                            .host_call(0, guarded, 0, &[descriptor_reg, guard.0], &[dst.0]);
                    }
                    None => {
                        self.asm.host_call(0, plain, 0, &[descriptor_reg], &[dst.0]);
                    }
                }
            }
            lower::Instr::PopBinding { token, span } => {
                let sig = self.abi.control()?.pop;
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &[token.0], &[]);
            }
            lower::Instr::Signal {
                dsts,
                condition,
                span,
            } => self.emit_signal(dsts, *condition, *span)?,
            lower::Instr::EnterRegion { catches } => {
                let clauses = catches
                    .iter()
                    .map(|catch| {
                        Ok(match catch {
                            lower::Catch::Tag { tag, target } => {
                                Catch::TagRef(self.abi.tag(*tag)?, self.label(*target)?)
                            }
                            lower::Catch::All { target } => Catch::AllRef(self.label(*target)?),
                        })
                    })
                    .collect::<Result<Vec<_>, TapeError>>()?;
                self.asm.begin_region(&clauses);
            }
            lower::Instr::LeaveRegion => {
                self.asm
                    .end_region()
                    .ok_or_else(|| invalid_lowered("region.leave has no open region"))?;
            }
            lower::Instr::CatchTarget { label, .. } => {
                let label = self.label(*label)?;
                self.asm.place(label).map_err(build_error)?;
            }
            lower::Instr::Rethrow { exception, span } => {
                self.set_span(*span);
                self.asm.throw_ref(0, exception.0);
            }
            lower::Instr::PushExit { dst, span } => {
                let sig = self.abi.control()?.push_exit;
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &[], &[dst.0]);
            }
            lower::Instr::CheckExit { token, span } => {
                let sig = self.abi.control()?.check_exit;
                self.set_span(*span);
                self.asm.host_call(0, sig, 0, &[token.0], &[]);
            }
            lower::Instr::Throw { tag, payload, span } => {
                let tag = self.abi.tag(*tag)?;
                self.set_span(*span);
                self.builder
                    .emit_throw(&mut self.asm, tag, &reg_slice(payload))
                    .map_err(build_error)?;
            }
        }
        Ok(())
    }

    /// Emits a signal: a handler search that runs each applicable guard
    /// here on the signalling stack, then a suspension on the condition tag
    /// when no handler accepts. A selected handler's restart is raised at the
    /// search request that selects it, and a guard that exits by an exception
    /// ends the search before the exception continues.
    ///
    /// ```text
    ///           pending = host_call signal(condition)
    ///           jmp search
    /// search:   br pending, guard, none
    /// guard:    closure = host_call guard()
    ///           region [catch_all_ref -> unwound]
    ///             accepted = call.indirect closure(condition)
    ///           pending = host_call guard_result(accepted)
    ///           jmp search
    /// unwound(exn):
    ///           host_call guard_unwound()
    ///           throw_ref exn
    /// none:     dsts = suspend condition#n(condition)
    /// ```
    fn emit_signal(
        &mut self,
        dsts: &[lower::Register],
        condition: lower::Register,
        span: Span,
    ) -> Result<(), TapeError> {
        let control = self.abi.control()?;
        let results = dsts
            .iter()
            .map(|dst| self.register_type(*dst))
            .collect::<Result<Vec<_>, _>>()?;
        let tag = control.condition_tag(&results)?;
        let (signal, guard, guard_result, guard_unwound, guard_call) = (
            control.signal,
            control.guard,
            control.guard_result,
            control.guard_unwound,
            control.guard_call,
        );
        let pending = self.alloc_temp(ValueType::Bool);
        let closure = self.alloc_temp(ValueType::Closure);
        let accepted = self.alloc_temp(ValueType::Bool);
        let escaped = self.alloc_temp(ValueType::Exn);
        let (search, run_guard, none) = (self.asm.label(), self.asm.label(), self.asm.label());
        let unwound = self.asm.block(&[escaped]);
        self.set_span(span);
        self.asm.host_call(0, signal, 0, &[condition.0], &[pending]);
        // Every block ends in a terminator: control never falls into a label.
        self.asm.jmp(search);
        self.asm.place(search).map_err(build_error)?;
        self.asm.br(pending, run_guard, none);
        self.asm.place(run_guard).map_err(build_error)?;
        self.asm.host_call(0, guard, 0, &[], &[closure]);
        self.asm.begin_region(&[Catch::AllRef(unwound)]);
        self.asm
            .call_indirect(0, guard_call, closure, 0, &[condition.0], &[accepted]);
        self.asm
            .end_region()
            .ok_or_else(|| invalid_lowered("guard region is not open"))?;
        self.asm
            .host_call(0, guard_result, 0, &[accepted], &[pending]);
        self.asm.jmp(search);
        self.asm.place(unwound).map_err(build_error)?;
        self.asm.host_call(0, guard_unwound, 0, &[], &[]);
        self.asm.throw_ref(0, escaped);
        self.asm.place(none).map_err(build_error)?;
        self.asm
            .suspend(0, tag, 0, &[condition.0], &reg_slice(dsts));
        Ok(())
    }

    fn label(&self, label: lower::LabelId) -> Result<Label, TapeError> {
        self.labels
            .get(label.0 as usize)
            .copied()
            .ok_or_else(|| invalid_lowered("label id is out of range"))
    }

    fn register_type(&self, register: lower::Register) -> Result<ValueType, TapeError> {
        register_type(self.function, register)
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

fn register_type(
    function: &lower::Function,
    register: lower::Register,
) -> Result<ValueType, TapeError> {
    register
        .0
        .checked_sub(1)
        .and_then(|index| function.register_kinds.get(index as usize))
        .copied()
        .map(value_type)
        .ok_or_else(|| invalid_lowered("register is out of range"))
}

fn labels_for(function: &lower::Function, asm: &mut Asm) -> Vec<Label> {
    let mut count = 0_usize;
    let mut catch_params: Vec<(usize, &[lower::Register])> = Vec::new();
    for instr in &function.instructions {
        match instr {
            lower::Instr::Label { label } => {
                count = count.max(label.0 as usize + 1);
            }
            lower::Instr::CatchTarget { label, params } => {
                count = count.max(label.0 as usize + 1);
                catch_params.push((label.0 as usize, params));
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
            lower::Instr::EnterRegion { catches } => {
                for catch in catches {
                    let (lower::Catch::Tag { target, .. } | lower::Catch::All { target }) = catch;
                    count = count.max(target.0 as usize + 1);
                }
            }
            _ => {}
        }
    }
    (0..count)
        .map(
            |index| match catch_params.iter().find(|(label, _)| *label == index) {
                Some((_, params)) => asm.block(&reg_slice(params)),
                None => asm.label(),
            },
        )
        .collect()
}

fn reg_slice(regs: &[lower::Register]) -> Vec<u32> {
    regs.iter().map(|reg| reg.0).collect()
}

fn invalid_lowered(message: &str) -> TapeError {
    TapeError::InvalidLowered {
        message: message.into(),
    }
}

/// Encodes a source span as its start offset plus one; span ids are non-zero.
fn span_id(span: Span) -> Option<SpanId> {
    let start = u64::try_from(span.start).unwrap_or(u64::MAX - 1);
    SpanId::try_from(start.saturating_add(1)).ok()
}

/// Recovers the source start offset encoded by [`span_id`].
pub(crate) fn span_start(span_id: SpanId) -> Option<usize> {
    usize::try_from(span_id.get() - 1).ok()
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
        tape.runtime()
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
        assert_eq!(tape.program().program().functions.len(), 2);
        assert_eq!(tape.main, Some(FuncId(1)));
    }

    fn has_call_sig(
        program: &execution_tape::program::Program,
        args: &[ValueType],
        rets: &[ValueType],
    ) -> bool {
        program.call_sigs.iter().any(|sig| {
            program
                .call_sig_args(sig)
                .is_ok_and(|kinds| kinds.iter().eq(args.iter()))
                && program
                    .call_sig_rets(sig)
                    .is_ok_and(|kinds| kinds.iter().eq(rets.iter()))
        })
    }

    #[test]
    fn emits_known_callable_apply_through_indirect_call_signature() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  apply(f, 2)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let program = tape.program().program();

        // Retry regions add one indirect signature per function signature.
        assert!(has_call_sig(program, &[ValueType::I64], &[ValueType::I64]));

        let disassembly = execution_tape::disasm::disassemble_verified(tape.program()).to_string();
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
        let program = tape.program().program();

        // Retry regions add one indirect signature per function signature.
        assert!(has_call_sig(
            program,
            &[runtime::object_type()],
            &[ValueType::I64]
        ));

        let disassembly = execution_tape::disasm::disassemble_verified(tape.program()).to_string();
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
        let program = tape.program().program();

        assert_eq!(
            program
                .function_arg_types(&program.functions[1])
                .expect("callit args should decode"),
            &[ValueType::Closure],
        );
        let disassembly = execution_tape::disasm::disassemble_verified(tape.program()).to_string();
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
        assert_eq!(tape.program().program().functions.len(), 5);
        assert_eq!(tape.main, Some(FuncId(0)));
        let circle = runtime_class(&tape, "<circle>");
        assert_eq!(circle.slots[0].name, "radius");
        assert_eq!(circle.slots[0].kind, runtime::ValueKind::I64);
    }

    #[test]
    fn emits_verified_tape_for_boolean_control_flow() {
        let module = lower_source("define function main()\n  if 10 >= 5 then 42 else 0 end\nend;");
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program().program().functions.len(), 1);
        assert_eq!(tape.main, Some(FuncId(0)));
    }

    #[test]
    fn emits_verified_tape_for_signal_handle_restart() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program().program().functions.len(), 3);
        assert_eq!(tape.main, Some(FuncId(0)));
    }

    #[test]
    fn emits_verified_tape_for_typed_runtime_slots() {
        let module = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<flag>, valid?: #t).valid? then 1 else 0 end\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program().program().functions.len(), 3);
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

        assert_eq!(tape.program().program().functions.len(), 3);
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
        let program = tape.program().program();
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
        let program = tape.program().program();
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
