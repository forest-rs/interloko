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

use execution_tape::asm::{
    Asm, Catch, ContractRef, FunctionAbi, FunctionSig, Label, ProgramBuilder, Reg,
};
use execution_tape::contract::{Contract, ContractId};
use execution_tape::host::HostSig;
use execution_tape::program::{
    CallSigId, ConstId, ElemTypeId, HostSigId, LinkId, SpanId, TagId, ValueType,
};
use execution_tape::value::FuncId;
use execution_tape::verifier::VerifiedProgram;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::lower;
use crate::sem;
use crate::syntax::Span;
use interloko_runtime::code::{CodeUnit, method_link};
use interloko_runtime::control::{ControlLayout, TypeErrorSite};
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

    /// Returns the internal-compiler-error diagnostic that reports this
    /// failure, or `None` when emission stopped for diagnostics that were
    /// already reported. Emission has no source position, so the
    /// diagnostic's span is empty.
    #[must_use]
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        match self {
            Self::Diagnostics { .. } => None,
            Self::InvalidLowered { message } | Self::Build { message } => Some(
                Diagnostic::error(Span::default(), self.code())
                    .with_arg(DiagnosticArg::text("detail", message.clone())),
            ),
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
        let mut signature = FunctionAbi {
            args: function
                .params
                .iter()
                .map(|param| abi_ref(param.kind, abi.callable))
                .collect(),
            rets: function
                .rets
                .iter()
                .map(|kind| abi_ref(*kind, abi.callable))
                .collect(),
        };
        if let Some(environment) = closure_env_contract(function, abi.callable, &mut builder)? {
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

    declare_links(module, &ids, &mut builder)?;

    let mut type_errors = vec![Vec::new(); ids.functions.len()];
    for function in &module.functions {
        let func = ids
            .functions
            .get(function.id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_lowered("function id is out of range"))?;
        let sites = type_errors
            .get_mut(func.0 as usize)
            .ok_or_else(|| invalid_lowered("function id is out of range"))?;
        let asm = FunctionEmitter::new(function, &ids, &abi, &mut builder, sites)
            .with_layout(&module.runtime)
            .emit()?;
        builder.define_function(func, asm).map_err(build_error)?;
    }

    // After the bodies, so nothing the sends need is interned before what
    // the bodies intern: a send added anywhere moves no other function's
    // host or call signature ids. Bodies name the links by generic alone.
    let (sites, miss_stubs) = declare_sites(module, &ids, &mut builder, &abi)?;

    let program = builder.build_verified().map_err(build_error)?;
    let mut code = CodeUnit {
        program: Arc::new(program),
        layout: module.runtime.clone(),
        control: ControlLayout {
            restarts: module.restarts.clone(),
            type_errors,
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
        links: linked_methods(module),
        contracts: module.contracts.clone(),
        assumptions: module.assumptions.clone(),
        sites,
        miss_stubs,
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
        lower::ValueKind::Char => ValueType::U32,
        lower::ValueKind::Position => ValueType::I64,
        lower::ValueKind::Function(_) => ValueType::Func,
        lower::ValueKind::Closure(_) => ValueType::Agg,
        lower::ValueKind::Entry => ValueType::Closure,
        lower::ValueKind::Aggregate => ValueType::Agg,
        lower::ValueKind::Exn => ValueType::Exn,
        lower::ValueKind::Object(_) => runtime::object_type(),
        lower::ValueKind::ClassObject(_)
        | lower::ValueKind::GenericFunctionObject(_)
        | lower::ValueKind::MethodObject(_)
        | lower::ValueKind::SlotDescriptorObject => runtime::object_type(),
    }
}

/// What a register of `kind` holds at a function or call boundary: its
/// kind, or for a callable value the `callable` contract, so the code that
/// receives one can open it ([`lower::FunctionKind::CallableParts`]).
fn abi_ref(kind: lower::ValueKind, callable: ContractId) -> ContractRef {
    match kind {
        lower::ValueKind::Closure(_) => callable.into(),
        kind => ContractRef::Kind(value_type(kind)),
    }
}

/// The contract of a callable value: a tuple of its typed entry, its boxed
/// entry and its shape ([`lower::Instr::CallableNew`]).
fn callable_contract(builder: &mut ProgramBuilder) -> Result<ContractId, TapeError> {
    let entry = builder
        .contract(Contract::Kind(ValueType::Closure))
        .map_err(build_error)?;
    let shape = builder
        .contract(Contract::Kind(ValueType::U64))
        .map_err(build_error)?;
    builder
        .contract(Contract::Tuple(&[Some(entry), Some(entry), Some(shape)]))
        .map_err(build_error)
}

fn closure_env_contract(
    function: &lower::Function,
    callable: ContractId,
    builder: &mut ProgramBuilder,
) -> Result<Option<ContractId>, TapeError> {
    if !matches!(
        function.kind,
        lower::FunctionKind::Closure(_)
            | lower::FunctionKind::ExitThunk { .. }
            | lower::FunctionKind::BoxedEntry { .. }
            | lower::FunctionKind::Adapter { .. }
    ) {
        return Ok(None);
    }

    let mut elems: Vec<Option<ContractRef>> = Vec::new();
    for instr in &function.instructions {
        let lower::Instr::CaptureGet { index, kind, .. } = instr else {
            continue;
        };
        if elems.len() <= *index {
            elems.resize(index.saturating_add(1), None);
        }
        let reference = abi_ref(*kind, callable);
        match elems[*index] {
            Some(existing) if existing != reference => {
                return Err(invalid_lowered("capture index has conflicting value kinds"));
            }
            _ => {
                elems[*index] = Some(reference);
            }
        }
    }

    if elems.is_empty() {
        Ok(None)
    } else {
        let children: Vec<_> = elems
            .into_iter()
            .map(|reference| {
                reference
                    .map(|reference| match reference {
                        ContractRef::Kind(kind) => builder.contract(Contract::Kind(kind)),
                        ContractRef::Id(id) => Ok(id),
                    })
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

/// Wraps an `execution_tape` builder or verifier error.
///
/// Uses the error's `Display`, which for a verifier rejection renders the
/// rejected program's function, pc, label, span and instruction, so the
/// internal-error message says where the emitted code went wrong.
fn build_error(error: impl core::fmt::Display) -> TapeError {
    TapeError::Build {
        message: format!("{error}"),
    }
}

/// The most classes an `instance?` test compares keys with before asking
/// the host instead.
const KEYED_INSTANCE_CLASSES: usize = 4;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TapeIds {
    functions: Vec<FuncId>,
}

/// Declares one tape link per method of the runtime layout, in method order,
/// so `LinkId(m)` names method `m` ([`method_link`]) and each starts at the
/// method's function.
///
/// Linked sends call through their method's link. Declaring every method's,
/// used or not, makes link ids depend on the layout alone: an edit that adds
/// or drops a send renumbers none, so unchanged functions keep their
/// bytecode. A link is named after its method's generic and specializers,
/// as `area(<square>)`, with `_` for an unspecialized parameter.
fn declare_links(
    module: &lower::Module,
    ids: &TapeIds,
    builder: &mut ProgramBuilder,
) -> Result<(), TapeError> {
    let mut methods = vec![None; module.runtime.methods.len()];
    for (function, func) in module.functions.iter().zip(&ids.functions) {
        if let lower::FunctionKind::Method(method) = function.kind {
            *methods
                .get_mut(method.0 as usize)
                .ok_or_else(|| invalid_lowered("method is not in the layout"))? = Some(*func);
        }
    }
    for (index, target) in methods.into_iter().enumerate() {
        let method = sem::MethodId(
            u32::try_from(index).map_err(|_| invalid_lowered("too many methods to link"))?,
        );
        let target = target.ok_or_else(|| invalid_lowered("a layout method has no function"))?;
        let name = method_link_name(&module.runtime, method);
        let link = builder.declare_link(&name, target).map_err(build_error)?;
        if link != method_link(method) {
            return Err(invalid_lowered("method links are not in method order"));
        }
    }
    Ok(())
}

/// Declares one link per generic of the runtime layout, after the method
/// links, so `LinkId(methods + g)` is generic `g`'s
/// ([`CodeUnit::send_link`]), and returns the [`runtime::code::SendSite`]s of
/// the generics `module` sends to through them, with the miss stubs.
///
/// Like method links, these depend on the layout alone, so a send added or
/// dropped anywhere renumbers no link and unchanged functions keep their
/// bytecode. Every guarded send to a generic shares its link, an inline
/// cache for the unit's sends to it. A generic whose dispatched arguments
/// are objects, and which the program sends to, gets a guarded link over its
/// dispatcher, whose fallback, its miss stub, takes the link index and keys
/// before the send's arguments, asks the driver for the method they select
/// ([`runtime::code::CODE_FILL`]), which also fills the guard, and calls it;
/// any other gets a plain link nothing calls.
fn declare_sites(
    module: &lower::Module,
    ids: &TapeIds,
    builder: &mut ProgramBuilder,
    abi: &AbiTables,
) -> Result<(Vec<runtime::code::SendSite>, Vec<FuncId>), TapeError> {
    let methods = module.runtime.methods.len();
    let mut sites = Vec::new();
    let mut stubs = Vec::new();
    for index in 0..module.runtime.generics.len() {
        let generic = sem::GenericFunctionId(
            u32::try_from(index).map_err(|_| invalid_lowered("too many generics to link"))?,
        );
        let dispatcher = module
            .functions
            .iter()
            .find(|function| function.kind == lower::FunctionKind::Dispatcher(generic))
            .ok_or_else(|| invalid_lowered("a layout generic has no dispatcher"))?;
        let target = ids.functions[dispatcher.id.0 as usize];
        // Only generics the program sends to need a guard and a miss stub;
        // a plain link keeps every other generic's id in place.
        let sent = module.functions.iter().any(|function| {
            function.instructions.iter().any(|instr| {
                matches!(instr, lower::Instr::GuardedSend { generic: sent, .. } if *sent == generic)
            })
        });
        let keys = guard_key_count(module, dispatcher, generic).filter(|_| sent);
        let name = format!("{}#send", dispatcher.name);
        let link = match keys {
            Some(keys) => {
                let stub = declare_miss_stub(builder, abi, dispatcher, generic, keys)?;
                stubs.push(stub);
                builder
                    .declare_guarded_link(&name, target, keys, stub)
                    .map_err(build_error)?
            }
            None => builder.declare_link(&name, target).map_err(build_error)?,
        };
        if link.0 as usize != methods + index {
            return Err(invalid_lowered("send links are not in generic order"));
        }
        // The guesses of the unit's sends to the generic, if they agree.
        let mut sends = module
            .functions
            .iter()
            .flat_map(|function| &function.instructions)
            .filter_map(|instr| match instr {
                lower::Instr::GuardedSend {
                    generic: sent,
                    keys: sent_keys,
                    guess,
                    ..
                } if *sent == generic => Some((sent_keys.len(), guess)),
                _ => None,
            })
            .peekable();
        let Some(&(_, first)) = sends.peek() else {
            continue;
        };
        let first = first.clone();
        let mut guess = first.clone();
        for (count, sent) in sends {
            if Some(count) != keys {
                return Err(invalid_lowered("a guarded send's keys are not its link's"));
            }
            if *sent != first {
                guess = None;
            }
        }
        sites.push(runtime::code::SendSite {
            link,
            generic,
            guess,
        });
    }
    Ok((sites, stubs))
}

/// How many keys a guarded send to `generic` passes: one per argument its
/// dispatch plan selects by, when there are 1 to
/// [`execution_tape::program::MAX_GUARD_KEYS`] and each is an object or a
/// boxed `<object>`, as lowering's guarded sends require.
fn guard_key_count(
    module: &lower::Module,
    dispatcher: &lower::Function,
    generic: sem::GenericFunctionId,
) -> Option<usize> {
    let plan = module.dispatch.get(generic.0 as usize)?.as_ref()?;
    let keyable = (1..=execution_tape::program::MAX_GUARD_KEYS).contains(&plan.positions.len())
        && plan.positions.iter().all(|&position| {
            matches!(
                dispatcher.params.get(position).map(|param| param.kind),
                Some(lower::ValueKind::Object(_) | lower::ValueKind::Any)
            )
        });
    keyable.then_some(plan.positions.len())
}

/// Declares and defines `generic`'s miss stub for sends keyed by `keys`
/// classes: `(link, keys..., args...) -> results`, which asks
/// [`runtime::code::CODE_FILL`] for the function to call and calls it.
fn declare_miss_stub(
    builder: &mut ProgramBuilder,
    abi: &AbiTables,
    dispatcher: &lower::Function,
    generic: sem::GenericFunctionId,
    keys: usize,
) -> Result<FuncId, TapeError> {
    let params = dispatcher
        .params
        .iter()
        .map(|param| param.kind)
        .collect::<Vec<_>>();
    let mut args = vec![ContractRef::Kind(ValueType::U64); 1 + keys];
    args.extend(params.iter().map(|kind| abi_ref(*kind, abi.callable)));
    let rets = dispatcher
        .rets
        .iter()
        .map(|kind| abi_ref(*kind, abi.callable))
        .collect::<Vec<_>>();
    let stub = builder
        .declare_function(FunctionAbi {
            args,
            rets: rets.clone(),
        })
        .map_err(build_error)?;
    builder
        .set_function_name(stub, &format!("{}#miss", dispatcher.name))
        .map_err(build_error)?;
    let call_sig = builder
        .call_sig(FunctionAbi {
            args: params
                .iter()
                .map(|kind| abi_ref(*kind, abi.callable))
                .collect(),
            rets,
        })
        .map_err(build_error)?;
    let param_types = params
        .iter()
        .map(|kind| value_type(*kind))
        .collect::<Vec<_>>();
    let fill = builder.host_sig_for(
        runtime::code::CODE_FILL,
        runtime::code::fill_signature(keys, &param_types),
    );
    let mut asm = builder.function_asm(stub).map_err(build_error)?;
    let generic_reg = asm.alloc_register(ValueType::U64);
    let callee = asm.alloc_register(ValueType::Func);
    let results = dispatcher
        .rets
        .iter()
        .map(|kind| asm.alloc_register(value_type(*kind)))
        .collect::<Vec<_>>();
    let link_reg = Reg(1);
    let key_regs = (0..keys).map(|index| Reg(u32::try_from(index + 2).unwrap_or(u32::MAX)));
    let arg_regs = (0..params.len())
        .map(|index| Reg(u32::try_from(index + 2 + keys).unwrap_or(u32::MAX)))
        .collect::<Vec<_>>();
    let mut fill_args = vec![link_reg, generic_reg];
    fill_args.extend(key_regs);
    fill_args.extend(&arg_regs);
    asm.const_u64(generic_reg, u64::from(generic.0))
        .host_call(Reg::EFFECT, fill, Reg::EFFECT, &fill_args, &[callee])
        .call_indirect(
            Reg::EFFECT,
            call_sig,
            callee,
            Reg::EFFECT,
            &arg_regs,
            &results,
        )
        .ret(Reg::EFFECT, &results);
    builder.define_function(stub, asm).map_err(build_error)?;
    Ok(stub)
}

/// The methods whose links `module`'s sends call, ascending.
fn linked_methods(module: &lower::Module) -> Vec<sem::MethodId> {
    let mut methods = module
        .functions
        .iter()
        .flat_map(|function| &function.instructions)
        .filter_map(|instr| match instr {
            lower::Instr::LinkedSend { method, .. } => Some(*method),
            _ => None,
        })
        .collect::<Vec<_>>();
    methods.sort_unstable_by_key(|method| method.0);
    methods.dedup();
    methods
}

/// A method's link name: its generic and specializers, as `area(<square>)`,
/// with `_` for a parameter it does not specialize and the method id after a
/// `#` should two methods ever print alike.
fn method_link_name(layout: &RuntimeLayout, method: sem::MethodId) -> String {
    let Some(entry) = layout.methods.get(method.0 as usize) else {
        return format!("m{}", method.0);
    };
    let generic = layout.generic(entry.generic).map_or_else(
        || format!("g{}", entry.generic.0),
        |generic| generic.name.clone(),
    );
    let specializers = entry
        .specializers
        .iter()
        .map(|specializer| match specializer {
            Some(class) => layout
                .class(*class)
                .map_or_else(|| format!("c{}", class.0), |class| class.name.clone()),
            None => "_".into(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let name = format!("{generic}({specializers})");
    let ambiguous = layout
        .methods
        .iter()
        .enumerate()
        .any(|(other, entry_other)| {
            other != method.0 as usize
                && entry_other.generic == entry.generic
                && entry_other.specializers == entry.specializers
        });
    if ambiguous {
        format!("{name}#m{}", method.0)
    } else {
        name
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AbiTables {
    /// The contract of every callable value register ([`callable_contract`]).
    callable: ContractId,
    make_object: HostSigId,
    class_count: usize,
    class_of: HostSigId,
    class_of_any: HostSigId,
    unbox_i64: HostSigId,
    unbox_f64: HostSigId,
    unbox_f32: HostSigId,
    unbox_bool: HostSigId,
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
    /// `any` and `object` input signatures for class object casts, when
    /// the module makes any.
    cast_class: Option<(HostSigId, HostSigId)>,
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

/// The value types a failing check reports, in the fixed order their
/// `<type-error>` request signatures are declared: a cast's operand (boxed
/// or an object), a conversion's or shift count's operand, and the operands
/// of an overflowing operation (two, or one for a negation).
fn type_error_reports() -> Vec<Vec<ValueType>> {
    let mut reports = vec![vec![runtime::any_type()], vec![runtime::object_type()]];
    for scalar in [
        ValueType::I64,
        ValueType::I32,
        ValueType::U32,
        ValueType::F64,
        ValueType::F32,
    ] {
        reports.push(vec![scalar]);
    }
    for integer in [ValueType::I64, ValueType::I32, ValueType::U32] {
        reports.push(vec![integer, integer]);
    }
    reports
}

/// The result types of the condition tags every program that signals
/// declares first, in a fixed order: what a failing check's `use-value`
/// replacement may be (an object, or a number), then what else a send that
/// finds no method may resume with: one boxed `<object>`, boolean or
/// callable, or nothing (when its results are known callables, which the
/// dispatcher supplies itself). A generic whose results have another shape,
/// such as several values, resumes through a tag declared in first-use
/// order with the program's own signals.
fn fixed_condition_results() -> Vec<Vec<ValueType>> {
    let mut results = [
        runtime::object_type(),
        ValueType::I64,
        ValueType::I32,
        ValueType::U32,
        ValueType::F64,
        ValueType::F32,
        runtime::any_type(),
        ValueType::Bool,
        ValueType::Closure,
        ValueType::Agg,
    ]
    .into_iter()
    .map(|result| vec![result])
    .collect::<Vec<_>>();
    results.push(Vec::new());
    results
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
    /// `<type-error>` requests by the types of the values they report.
    type_errors: Vec<(Vec<ValueType>, HostSigId)>,
    /// The `<no-applicable-method>` request a dispatcher makes when no
    /// method matches.
    no_applicable_method: HostSigId,
    /// The tag a checking host call raises when its check fails.
    check_failed: TagId,
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
                lower::Instr::CastObject { .. }
                | lower::Instr::CastClassObject { .. }
                | lower::Instr::UnboxValue { .. }
                | lower::Instr::SignalTypeError { .. }
                | lower::Instr::UnboxObject { .. }
                | lower::Instr::NoApplicableMethod { .. }
                | lower::Instr::CheckedRuntimeCall { .. }
                | lower::Instr::TextCall { check: Some(_), .. } => uses_control = true,
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
        // Failing checks and sends resume with these types. Their condition
        // tags and the check-failed tag come before every other control
        // tag, in a fixed order, and their request signatures after every
        // other control signature, so adding or removing a check or a
        // generic renumbers nothing: an unchanged function compiles to the
        // same bytecode in a newer unit. Tags for the other result types of
        // a program's own signals and its generics, and for its exit
        // signatures, follow in first-use order.
        let mut results = fixed_condition_results();
        for signal in signal_results {
            if !results.contains(&signal) {
                results.push(signal);
            }
        }
        let signal_results = results;
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
        // Raised by host calls only; declared with the fixed tags.
        let check_failed = builder
            .import_tag(runtime::control::CHECK_FAILED_TAG, &[])
            .map_err(build_error)?;
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
        let guard_call = builder
            .call_sig(FunctionSig {
                arg_types: vec![runtime::object_type()],
                ret_types: vec![ValueType::Bool],
            })
            .map_err(build_error)?;
        // Fields initialize in the order written, which is the order the
        // signatures are declared: the failing-check and failing-send
        // requests last.
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
            type_errors: type_error_reports()
                .into_iter()
                .map(|values| {
                    let sig = builder.host_sig_for(
                        runtime::control::CONDITION_TYPE_ERROR,
                        runtime::control::type_error_signature(&values),
                    );
                    (values, sig)
                })
                .collect(),
            no_applicable_method: builder.host_sig_for(
                runtime::control::CONDITION_NO_APPLICABLE_METHOD,
                runtime::control::no_applicable_method_signature(),
            ),
            check_failed,
        }))
    }

    fn condition_tag(&self, results: &[ValueType]) -> Result<TagId, TapeError> {
        self.conditions
            .iter()
            .find(|(candidate, _)| candidate == results)
            .map(|(_, tag)| *tag)
            .ok_or_else(|| invalid_lowered("condition tag is missing"))
    }

    fn type_error_sig(&self, values: &[ValueType]) -> Result<HostSigId, TapeError> {
        self.type_errors
            .iter()
            .find(|(candidate, _)| candidate == values)
            .map(|(_, sig)| *sig)
            .ok_or_else(|| invalid_lowered("type error signature is missing"))
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
        let unbox_bool = builder.host_sig_for(
            runtime::UNBOX_VALUE,
            runtime::unbox_value_signature(ValueType::Bool),
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
        let cast_class = module
            .functions
            .iter()
            .flat_map(|function| &function.instructions)
            .any(|instr| matches!(instr, lower::Instr::CastClassObject { .. }))
            .then(|| {
                (
                    builder.host_sig_for(
                        runtime::CAST_CLASS_OBJECT,
                        runtime::cast_object_signature(runtime::any_type()),
                    ),
                    builder.host_sig_for(
                        runtime::CAST_CLASS_OBJECT,
                        runtime::cast_object_signature(runtime::object_type()),
                    ),
                )
            });
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
            callable: callable_contract(builder)?,
            make_object,
            class_count: module.runtime.classes.len(),
            class_of,
            class_of_any,
            unbox_i64,
            unbox_f64,
            unbox_f32,
            unbox_bool,
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
            cast_class,
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
            RuntimeOp::ClassMetaId,
            runtime::META_CLASS_ID,
            runtime::meta_class_id_signature(),
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
            RuntimeOp::InstanceOfType,
            runtime::INSTANCE_OF_TYPE,
            runtime::instance_of_type_signature(),
        ),
        (
            RuntimeOp::LimitedIntegerType,
            runtime::LIMITED_INTEGER_TYPE,
            runtime::limited_integer_type_signature(),
        ),
        (
            RuntimeOp::LimitedIntegerBase,
            runtime::MOP_LIMITED_INTEGER_BASE,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::LimitedIntegerMin,
            runtime::MOP_LIMITED_INTEGER_MIN,
            runtime::meta_query_any_signature(),
        ),
        (
            RuntimeOp::LimitedIntegerMax,
            runtime::MOP_LIMITED_INTEGER_MAX,
            runtime::meta_query_any_signature(),
        ),
        (
            RuntimeOp::SubclassTypeClass,
            runtime::MOP_SUBCLASS_TYPE_CLASS,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::TypeName,
            runtime::MOP_TYPE_NAME,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::MethodSpecializers,
            runtime::MOP_METHOD_SPECIALIZERS,
            runtime::meta_query_object_signature(),
        ),
        (
            RuntimeOp::MakeVector,
            runtime::MAKE_VECTOR,
            runtime::make_vector_signature(),
        ),
        (
            RuntimeOp::MakeTable,
            runtime::MAKE_TABLE,
            runtime::make_table_signature(),
        ),
        (
            RuntimeOp::CollectionSize,
            runtime::COLLECTION_SIZE,
            runtime::collection_size_signature(),
        ),
        (
            RuntimeOp::VectorElement,
            runtime::VECTOR_ELEMENT,
            runtime::vector_element_signature(),
        ),
        (
            RuntimeOp::VectorElementOr,
            runtime::VECTOR_ELEMENT_OR,
            runtime::vector_element_or_signature(),
        ),
        (
            RuntimeOp::VectorElementSetter,
            runtime::VECTOR_ELEMENT_SETTER,
            runtime::vector_element_setter_signature(),
        ),
        (
            RuntimeOp::StretchyAdd,
            runtime::STRETCHY_ADD,
            runtime::stretchy_add_signature(),
        ),
        (
            RuntimeOp::TableElement,
            runtime::TABLE_ELEMENT,
            runtime::table_element_signature(),
        ),
        (
            RuntimeOp::TableElementOr,
            runtime::TABLE_ELEMENT_OR,
            runtime::table_element_or_signature(),
        ),
        (
            RuntimeOp::TableElementSetter,
            runtime::TABLE_ELEMENT_SETTER,
            runtime::table_element_setter_signature(),
        ),
        (
            RuntimeOp::TableKeyExists,
            runtime::TABLE_KEY_EXISTS,
            runtime::table_key_signature(),
        ),
        (
            RuntimeOp::TableVersion,
            runtime::TABLE_VERSION,
            runtime::collection_size_signature(),
        ),
        (
            RuntimeOp::TablePositions,
            runtime::TABLE_POSITIONS,
            runtime::collection_size_signature(),
        ),
        (
            RuntimeOp::TableNextPosition,
            runtime::TABLE_NEXT_POSITION,
            runtime::table_next_position_signature(),
        ),
        (
            RuntimeOp::TablePositionValue,
            runtime::TABLE_POSITION_VALUE,
            runtime::table_position_signature(),
        ),
        (
            RuntimeOp::TablePositionKey,
            runtime::TABLE_POSITION_KEY,
            runtime::table_position_signature(),
        ),
        (
            RuntimeOp::VectorConstant,
            runtime::VECTOR_CONSTANT,
            runtime::vector_constant_signature(),
        ),
        (
            RuntimeOp::TableRemoveKey,
            runtime::TABLE_REMOVE_KEY,
            runtime::table_key_signature(),
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

/// A checking host call: `sig(value, class)`, which raises the check-failed
/// tag when it rejects the value, and the call that checks a replacement
/// object again, when the result's register type does not establish the
/// check.
#[derive(Copy, Clone)]
struct CheckedCall {
    sig: HostSigId,
    recheck: Option<HostSigId>,
    class: u32,
}

struct FunctionEmitter<'a> {
    function: &'a lower::Function,
    ids: &'a TapeIds,
    abi: &'a AbiTables,
    builder: &'a mut ProgramBuilder,
    asm: Asm,
    labels: Vec<Label>,
    /// The function's `<type-error>` sites, which
    /// [`ControlLayout::type_errors`] carries: one per failing check, in
    /// emission order.
    type_errors: &'a mut Vec<TypeErrorSite>,
    /// Registers the function's signal paths share, by role and type.
    shared: Vec<(SharedTemp, ValueType, Reg)>,
    /// The runtime layout, whose class hierarchy `instance?` tests compare
    /// keys against.
    layout: Option<&'a RuntimeLayout>,
}

/// What a register shared by every signal path of a function holds.
///
/// One activation runs at most one signal path at a time (a guard runs in
/// its own activation), so the paths can share their scratch registers
/// rather than each adding its own to every call's frame.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SharedTemp {
    /// Whether the handler search waits on a guard.
    Pending,
    /// The guard to run.
    Guard,
    /// The guard's answer.
    Accepted,
    /// The exception a guard exited by.
    Escaped,
    /// The condition being signalled.
    Condition,
    /// A `<type-error>` site operand.
    Site,
    /// A `use-value` replacement to check again.
    Replacement,
}

impl<'a> FunctionEmitter<'a> {
    fn new(
        function: &'a lower::Function,
        ids: &'a TapeIds,
        abi: &'a AbiTables,
        builder: &'a mut ProgramBuilder,
        type_errors: &'a mut Vec<TypeErrorSite>,
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
            type_errors,
            shared: Vec::new(),
            layout: None,
        }
    }

    const fn with_layout(mut self, layout: &'a RuntimeLayout) -> Self {
        self.layout = Some(layout);
        self
    }

    /// The classes an `instance?` test of `class` accepts, when there are at
    /// most [`KEYED_INSTANCE_CLASSES`] of them, so the test compares an
    /// `obj.key` with each instead of asking the host.
    fn keyed_instance_classes(&self, class: sem::ClassId) -> Option<Vec<u32>> {
        let layout = self.layout?;
        let accepted = (0..layout.classes.len())
            .filter_map(|index| u32::try_from(index).ok())
            .filter(|&id| layout.is_same_or_subclass(sem::ClassId(id), class))
            .collect::<Vec<_>>();
        (1..=KEYED_INSTANCE_CLASSES)
            .contains(&accepted.len())
            .then_some(accepted)
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
                self.asm.const_i64(*dst, *value);
            }
            lower::Instr::ConstF64 { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_f64(*dst, value.to_f64());
            }
            lower::Instr::Const32 { dst, value, span } => {
                self.set_span(*span);
                match value {
                    sem::Number32::I32(value) => self.asm.const_i32(*dst, *value),
                    sem::Number32::U32(value) => self.asm.const_u32(*dst, *value),
                    sem::Number32::F32(bits) => self.asm.const_f32_bits(*dst, bits.to_bits()),
                };
            }
            lower::Instr::Num32Unary { dst, op, src, span } => {
                self.set_span(*span);
                let (dst, src) = (*dst, *src);
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
                    lower::Num32UnaryOp::I64ToI32Wrap => self.asm.i64_to_i32_wrap(dst, src),
                    lower::Num32UnaryOp::I32ToU32Wrap => self.asm.i32_to_u32_wrap(dst, src),
                    lower::Num32UnaryOp::U32ToI32Wrap => self.asm.u32_to_i32_wrap(dst, src),
                    lower::Num32UnaryOp::F32ToF64 => self.asm.f32_to_f64(dst, src),
                    lower::Num32UnaryOp::F64ToF32 => self.asm.f64_to_f32(dst, src),
                    lower::Num32UnaryOp::F32ToI32 => self.asm.f32_to_i32(dst, src),
                    lower::Num32UnaryOp::F32ToU32 => self.asm.f32_to_u32(dst, src),
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
                let (dst, a, b) = (*dst, *lhs, *rhs);
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
                let (dst, overflow, a, b) = (*dst, *overflow, *lhs, *rhs);
                match op {
                    lower::Num32CheckedOp::I32AddOvf => self.asm.i32_add_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::I32SubOvf => self.asm.i32_sub_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::I32MulOvf => self.asm.i32_mul_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::U32AddOvf => self.asm.u32_add_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::U32SubOvf => self.asm.u32_sub_ovf(dst, overflow, a, b),
                    lower::Num32CheckedOp::U32MulOvf => self.asm.u32_mul_ovf(dst, overflow, a, b),
                };
            }
            lower::Instr::ConvertChecked {
                dst,
                inexact,
                op,
                src,
                span,
            } => {
                self.set_span(*span);
                let (dst, inexact, a) = (*dst, *inexact, *src);
                match op {
                    lower::ConvertCheckedOp::I64ToI32Ovf => {
                        self.asm.i64_to_i32_ovf(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::I64ToU32Ovf => {
                        self.asm.i64_to_u32_ovf(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::I32ToU32Ovf => {
                        self.asm.i32_to_u32_ovf(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::U32ToI32Ovf => {
                        self.asm.u32_to_i32_ovf(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::F32ToI32Exact => {
                        self.asm.f32_to_i32_exact(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::F32ToU32Exact => {
                        self.asm.f32_to_u32_exact(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::F64ToI32Exact => {
                        self.asm.f64_to_i32_exact(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::F64ToU32Exact => {
                        self.asm.f64_to_u32_exact(dst, inexact, a)
                    }
                    lower::ConvertCheckedOp::F64ToI64Exact => {
                        self.asm.f64_to_i64_exact(dst, inexact, a)
                    }
                };
            }
            lower::Instr::ConstBool { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_bool(*dst, *value);
            }
            lower::Instr::StringLiteral {
                dst, value, span, ..
            } => {
                let (sig, value_const) = self.abi.string_literal(value)?;
                let value_reg = self.alloc_temp(ValueType::Str);
                self.set_span(*span);
                self.asm.const_pool(value_reg, value_const);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[value_reg], &[*dst]);
            }
            lower::Instr::SymbolLiteral {
                dst, name, span, ..
            } => {
                let (sig, name_const) = self.abi.symbol_literal(name)?;
                let name_reg = self.alloc_temp(ValueType::Str);
                self.set_span(*span);
                self.asm.const_pool(name_reg, name_const);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[name_reg], &[*dst]);
            }
            lower::Instr::ConstU64 { dst, value, span } => {
                self.set_span(*span);
                self.asm.const_u64(*dst, *value);
            }
            lower::Instr::ConstClass { dst, class, span } => {
                self.set_span(*span);
                self.asm.const_u64(*dst, u64::from(class.0));
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
                self.asm.const_func(*dst, function);
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
                self.asm.tuple_new(env_reg, captures);
                self.asm.closure_new(*dst, func_reg, env_reg);
            }
            lower::Instr::CallableNew {
                dst,
                typed,
                boxed,
                shape,
                captures,
                span,
            } => {
                let function = |id: &lower::FunctionId| {
                    self.ids
                        .functions
                        .get(id.0 as usize)
                        .copied()
                        .ok_or_else(|| {
                            invalid_lowered("callable entry function id is out of range")
                        })
                };
                let typed = function(typed)?;
                let func_reg = self.alloc_temp(ValueType::Func);
                let env_reg = self.alloc_temp(ValueType::Agg);
                let typed_reg = self.alloc_temp(ValueType::Closure);
                let shape_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.tuple_new(env_reg, captures);
                self.asm.const_func(func_reg, typed);
                self.asm.closure_new(typed_reg, func_reg, env_reg);
                let boxed_reg = match boxed {
                    lower::BoxedEntry::Function(boxed) => {
                        let boxed = function(boxed)?;
                        let boxed_func = self.alloc_temp(ValueType::Func);
                        let boxed_reg = self.alloc_temp(ValueType::Closure);
                        self.asm.const_func(boxed_func, boxed);
                        self.asm.closure_new(boxed_reg, boxed_func, env_reg);
                        boxed_reg
                    }
                    lower::BoxedEntry::Shared(entry) => *entry,
                };
                self.asm.const_u64(shape_reg, *shape);
                self.asm.tuple_new(*dst, &[typed_reg, boxed_reg, shape_reg]);
            }
            lower::Instr::OpenCallable {
                typed,
                boxed,
                shape,
                callable,
                span,
            } => {
                self.set_span(*span);
                for (index, part) in [typed, boxed, shape].into_iter().enumerate() {
                    if let Some(part) = part {
                        self.asm.tuple_get(
                            *part,
                            *callable,
                            u32::try_from(index).unwrap_or(u32::MAX),
                        );
                    }
                }
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
                    .tuple_get(*dst, *env, u32::try_from(*index).unwrap_or(u32::MAX));
            }
            lower::Instr::CellNew {
                dst,
                value,
                kind,
                span,
            } => {
                let sig = self.builder.host_sig_for(
                    runtime::CELL_NEW,
                    runtime::cell_new_signature(value_type(*kind)),
                );
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*value], &[*dst]);
            }
            lower::Instr::CellGet {
                dst,
                cell,
                kind,
                span,
            } => {
                let sig = self.builder.host_sig_for(
                    runtime::CELL_GET,
                    runtime::cell_get_signature(value_type(*kind)),
                );
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*cell], &[*dst]);
            }
            lower::Instr::CellSet {
                cell,
                value,
                kind,
                span,
            } => {
                let sig = self.builder.host_sig_for(
                    runtime::CELL_SET,
                    runtime::cell_set_signature(value_type(*kind)),
                );
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*cell, *value], &[]);
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
                    // Declared at its first use, as characters box only
                    // where they cross `<object>`.
                    lower::ValueKind::Char => self
                        .builder
                        .host_sig_for(runtime::BOX_CHARACTER, runtime::box_character_signature()),
                    lower::ValueKind::Position => self
                        .builder
                        .host_sig_for(runtime::BOX_POSITION, runtime::box_position_signature()),
                    // Declared at its first use, as callables box only
                    // where they cross `<object>`.
                    lower::ValueKind::Closure(_) => self.builder.host_sig_for(
                        runtime::BOX_VALUE,
                        runtime::box_value_signature(ValueType::Agg),
                    ),
                    // A runtime object boxes in its handle: `obj.convert`,
                    // no host call or allocation.
                    lower::ValueKind::Object(_)
                    | lower::ValueKind::ClassObject(_)
                    | lower::ValueKind::GenericFunctionObject(_)
                    | lower::ValueKind::MethodObject(_)
                    | lower::ValueKind::SlotDescriptorObject => {
                        self.set_span(*span);
                        self.asm.obj_convert(*dst, *src);
                        return Ok(());
                    }
                    // `<i32>` and `<u32>` widen to the `<integer>` they
                    // are before boxing.
                    lower::ValueKind::Any
                    | lower::ValueKind::I32
                    | lower::ValueKind::U32
                    | lower::ValueKind::U64
                    | lower::ValueKind::Function(_)
                    | lower::ValueKind::Entry
                    | lower::ValueKind::Aggregate
                    | lower::ValueKind::Exn => {
                        return Err(invalid_lowered("invalid boxed value input kind"));
                    }
                };
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*src], &[*dst]);
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
                let (dst, overflow, a, b) = (*dst, *overflow, *lhs, *rhs);
                match op {
                    lower::I64ArithOp::Add => self.asm.i64_add_ovf(dst, overflow, a, b),
                    lower::I64ArithOp::Sub => self.asm.i64_sub_ovf(dst, overflow, a, b),
                    lower::I64ArithOp::Mul => self.asm.i64_mul_ovf(dst, overflow, a, b),
                };
            }
            lower::Instr::I64Unchecked {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, a, b) = (*dst, *lhs, *rhs);
                match op {
                    lower::I64ArithOp::Add => self.asm.i64_add(dst, a, b),
                    lower::I64ArithOp::Sub => self.asm.i64_sub(dst, a, b),
                    lower::I64ArithOp::Mul => self.asm.i64_mul(dst, a, b),
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
                let (dst, a, b) = (*dst, *lhs, *rhs);
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
                let (dst, a, b) = (*dst, *lhs, *rhs);
                match op {
                    lower::I64BitOp::And => self.asm.i64_and(dst, a, b),
                    lower::I64BitOp::Or => self.asm.i64_or(dst, a, b),
                    lower::I64BitOp::Xor => self.asm.i64_xor(dst, a, b),
                };
            }
            lower::Instr::F64Unary { dst, op, src, span } => {
                self.set_span(*span);
                match op {
                    lower::F64UnaryOp::Neg => self.asm.f64_neg(*dst, *src),
                    lower::F64UnaryOp::Abs => self.asm.f64_abs(*dst, *src),
                    lower::F64UnaryOp::Floor => self.asm.f64_floor(*dst, *src),
                    lower::F64UnaryOp::Ceil => self.asm.f64_ceil(*dst, *src),
                    lower::F64UnaryOp::Trunc => self.asm.f64_trunc(*dst, *src),
                    lower::F64UnaryOp::Nearest => self.asm.f64_nearest(*dst, *src),
                    lower::F64UnaryOp::Sqrt => self.asm.f64_sqrt(*dst, *src),
                };
            }
            lower::Instr::FloatCall {
                dst,
                function,
                args,
                span,
            } => {
                let sig = self.abi.float_function(*function)?;
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, args, &[*dst]);
            }
            lower::Instr::TextCall {
                dst,
                function,
                args,
                check,
                span,
            } => {
                // Declared at its first use, as only text code calls it.
                let sig = self
                    .builder
                    .host_sig_for(function.symbol(), function.signature());
                self.set_span(*span);
                match (
                    check,
                    function.checked_arg().and_then(|index| args.get(index)),
                ) {
                    (Some(check), Some(checked)) => {
                        self.emit_checked_text_call(*dst, sig, args, *checked, check, *span)?;
                    }
                    (None, None) => {
                        self.asm
                            .host_call(Reg::EFFECT, sig, Reg::EFFECT, args, &[*dst]);
                    }
                    _ => return Err(invalid_lowered("a text call's check does not match it")),
                }
            }
            lower::Instr::F64NotEqual {
                dst,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                self.asm.f64_ne(*dst, *lhs, *rhs);
            }
            lower::Instr::I64ToF64 { dst, src, span } => {
                self.set_span(*span);
                self.asm.i64_to_f64(*dst, *src);
            }
            lower::Instr::F64ToI64 { dst, src, span } => {
                self.set_span(*span);
                self.asm.f64_to_i64(*dst, *src);
            }
            lower::Instr::F64Compare {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                let (dst, a, b) = (*dst, *lhs, *rhs);
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
                        self.asm.i64_eq(*dst, *lhs, *rhs);
                    }
                    sem::CompareOp::Lt => {
                        self.asm.i64_lt(*dst, *lhs, *rhs);
                    }
                    sem::CompareOp::Le => {
                        self.asm.i64_le(*dst, *lhs, *rhs);
                    }
                    sem::CompareOp::Gt => {
                        self.asm.i64_gt(*dst, *lhs, *rhs);
                    }
                    sem::CompareOp::Ge => {
                        self.asm.i64_ge(*dst, *lhs, *rhs);
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
                self.asm.host_call(
                    Reg::EFFECT,
                    self.abi.value_eq,
                    Reg::EFFECT,
                    &[*lhs, *rhs],
                    &[*dst],
                );
            }
            lower::Instr::ConstantRead {
                ready,
                value,
                key,
                span,
            } => {
                let mut rets = vec![ValueType::Bool];
                let mut dsts = vec![*ready];
                if let Some(value) = value {
                    let kind = self
                        .function
                        .register_kind(*value)
                        .ok_or_else(|| invalid_lowered("constant register has no kind"))?;
                    rets.push(value_type(kind));
                    dsts.push(*value);
                }
                let sig = self.builder.host_sig_for(
                    runtime::CONSTANT_READ,
                    HostSig {
                        args: vec![ValueType::U64],
                        rets,
                    },
                );
                let key_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(key_reg, *key);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[key_reg], &dsts);
            }
            lower::Instr::ConstantClaim { dst, key, span } => {
                let sig = self
                    .builder
                    .host_sig_for(runtime::CONSTANT_CLAIM, runtime::constant_claim_signature());
                let key_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(key_reg, *key);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[key_reg], &[*dst]);
            }
            lower::Instr::ConstantGet { dst, key, span } => {
                let kind = self
                    .function
                    .register_kind(*dst)
                    .ok_or_else(|| invalid_lowered("constant register has no kind"))?;
                let sig = self.builder.host_sig_for(
                    runtime::CONSTANT_GET,
                    HostSig {
                        args: vec![ValueType::U64],
                        rets: vec![value_type(kind)],
                    },
                );
                let key_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(key_reg, *key);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[key_reg], &[*dst]);
            }
            lower::Instr::ConstantSet { key, value, span } => {
                let kind = self
                    .function
                    .register_kind(*value)
                    .ok_or_else(|| invalid_lowered("constant register has no kind"))?;
                let sig = self.builder.host_sig_for(
                    runtime::CONSTANT_SET,
                    HostSig {
                        args: vec![ValueType::U64, value_type(kind)],
                        rets: vec![],
                    },
                );
                let key_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(key_reg, *key);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[key_reg, *value], &[]);
            }
            lower::Instr::ConstantAbandon { key, span } => {
                let sig = self.builder.host_sig_for(
                    runtime::CONSTANT_ABANDON,
                    runtime::constant_abandon_signature(),
                );
                let key_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(key_reg, *key);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[key_reg], &[]);
            }
            lower::Instr::Identical {
                dst,
                lhs,
                rhs,
                span,
            } => {
                // Declared at its first use, so a module that never compares
                // identities does not install it.
                let sig = self
                    .builder
                    .host_sig_for(runtime::IDENTICAL, runtime::identical_signature());
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*lhs, *rhs], &[*dst]);
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
                self.set_span(*span);
                self.asm.call(Reg::EFFECT, callee, Reg::EFFECT, args, dsts);
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
                operands.extend(args);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &operands, &[*dst]);
            }
            lower::Instr::LinkedSend {
                dsts,
                method,
                args,
                span,
                ..
            } => {
                self.set_span(*span);
                self.asm
                    .call_link(Reg::EFFECT, method_link(*method), Reg::EFFECT, args, dsts);
            }
            lower::Instr::GuardedSend {
                dsts,
                generic,
                keys,
                args,
                span,
                ..
            } => {
                let link = self
                    .layout
                    .and_then(|layout| u32::try_from(layout.methods.len()).ok())
                    .map(|methods| LinkId(methods + generic.0))
                    .ok_or_else(|| invalid_lowered("a guarded send has no layout"))?;
                self.set_span(*span);
                self.asm
                    .call_link_guard(Reg::EFFECT, link, Reg::EFFECT, keys, args, dsts);
            }
            lower::Instr::CallIndirect {
                dsts,
                callee,
                args,
                arg_kinds,
                rets,
                span,
            } => {
                let callable = self.abi.callable;
                let sig = self
                    .builder
                    .call_sig(FunctionAbi {
                        args: arg_kinds
                            .iter()
                            .map(|kind| abi_ref(*kind, callable))
                            .collect(),
                        rets: rets.iter().map(|kind| abi_ref(*kind, callable)).collect(),
                    })
                    .map_err(build_error)?;
                self.set_span(*span);
                self.asm
                    .call_indirect(Reg::EFFECT, sig, *callee, Reg::EFFECT, args, dsts);
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
                self.asm.tuple_new(fields_reg, fields);
                self.asm.host_call(
                    Reg::EFFECT,
                    self.abi.make_object,
                    Reg::EFFECT,
                    &[class_reg, fields_reg],
                    &[*dst],
                );
            }
            lower::Instr::VectorLiteral {
                dst,
                elements,
                span,
                ..
            } => {
                let (sig, elem) = self.abi.vector_literal()?;
                let aggregate = self.alloc_temp(ValueType::Agg);
                self.set_span(*span);
                self.asm.array_new(aggregate, elem, elements);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[aggregate], &[*dst]);
            }
            lower::Instr::RuntimeCall {
                dst,
                op,
                args,
                span,
                ..
            } => {
                let sig = self.abi.runtime_call(*op)?;
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, args, &[*dst]);
            }
            lower::Instr::HostCall {
                dst,
                function,
                args,
                span,
                ..
            } => {
                let sig = self.abi.host_function(*function)?;
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, args, &[*dst]);
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
                self.asm.host_call(
                    Reg::EFFECT,
                    sig,
                    Reg::EFFECT,
                    &[*receiver, slot_reg],
                    &[*dst],
                );
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
                self.asm.host_call(
                    Reg::EFFECT,
                    sig,
                    Reg::EFFECT,
                    &[*receiver, slot_reg, *value],
                    &[*dst],
                );
            }
            lower::Instr::ClassOf {
                dst,
                object,
                input,
                span,
            } => {
                // `obj.key` reads the class without a host call; a boxed
                // scalar's key also carries `SCALAR_KEY_BIT`, which a class
                // id does not.
                self.set_span(*span);
                if let ValueType::Obj(_) = self.register_type(*object)? {
                    self.asm.obj_key(*dst, *object);
                    if *input == lower::ValueKind::Any {
                        let mask = self.alloc_temp(ValueType::U64);
                        self.asm
                            .const_u64(mask, !runtime::SCALAR_KEY_BIT)
                            .u64_and(*dst, *dst, mask);
                    }
                } else {
                    let sig = if *input == lower::ValueKind::Any {
                        self.abi.class_of_any
                    } else {
                        self.abi.class_of
                    };
                    self.asm
                        .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*object], &[*dst]);
                }
            }
            lower::Instr::UnboxValue {
                dst,
                value,
                class,
                kind,
                type_error,
                span,
            } => {
                let sig = match kind {
                    lower::ValueKind::I64 => self.abi.unbox_i64,
                    lower::ValueKind::F64 => self.abi.unbox_f64,
                    lower::ValueKind::F32 => self.abi.unbox_f32,
                    lower::ValueKind::Bool => self.abi.unbox_bool,
                    lower::ValueKind::Char => self.builder.host_sig_for(
                        runtime::UNBOX_VALUE,
                        runtime::unbox_value_signature(ValueType::U32),
                    ),
                    lower::ValueKind::Position => self.abi.unbox_i64,
                    lower::ValueKind::Closure(None) => self.builder.host_sig_for(
                        runtime::UNBOX_VALUE,
                        runtime::unbox_value_signature(ValueType::Agg),
                    ),
                    _ => return Err(invalid_lowered("unbox result kind is not unboxable")),
                };
                // Every value of the result's register type is an instance
                // of the class, a scalar's or `<function>`, so a
                // replacement needs no check; except a character's, as a
                // `u32` register holds more than the scalar values.
                let recheck = match kind {
                    lower::ValueKind::Char => Some(self.builder.host_sig_for(
                        runtime::CHECK_CHARACTER,
                        runtime::check_character_signature(),
                    )),
                    lower::ValueKind::Position => Some(self.builder.host_sig_for(
                        runtime::CHECK_POSITION,
                        runtime::check_position_signature(),
                    )),
                    _ => None,
                };
                let check = CheckedCall {
                    sig,
                    recheck,
                    class: class.0,
                };
                self.emit_checked_call(*dst, *value, check, type_error, *span)?;
            }
            lower::Instr::InstanceOf {
                dst,
                value,
                input,
                class,
                span,
            } => {
                if let (ValueType::Obj(_), Some(accepted)) = (
                    self.register_type(*value)?,
                    self.keyed_instance_classes(*class),
                ) {
                    // `obj.key` and a compare per accepted class, without a
                    // host call; a boxed scalar's key is its class too.
                    let key = self.alloc_temp(ValueType::U64);
                    let expected = self.alloc_temp(ValueType::U64);
                    let (yes, done) = (self.asm.label(), self.asm.label());
                    self.set_span(*span);
                    self.asm.obj_key(key, *value);
                    if *input == lower::ValueKind::Any {
                        self.asm
                            .const_u64(expected, !runtime::SCALAR_KEY_BIT)
                            .u64_and(key, key, expected);
                    }
                    for id in accepted {
                        let no = self.asm.label();
                        self.asm
                            .const_u64(expected, u64::from(id))
                            .u64_eq(*dst, key, expected)
                            .br(*dst, yes, no);
                        self.asm
                            .place(no)
                            .map_err(|_| invalid_lowered("instance label is placed twice"))?;
                    }
                    self.asm.jmp(done);
                    self.asm
                        .place(yes)
                        .map_err(|_| invalid_lowered("instance label is placed twice"))?;
                    self.asm.const_bool(*dst, true).jmp(done);
                    self.asm
                        .place(done)
                        .map_err(|_| invalid_lowered("instance label is placed twice"))?;
                    return Ok(());
                }
                let sig = self.instance_sig(*input)?;
                let class_reg = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(class_reg, u64::from(class.0));
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*value, class_reg], &[*dst]);
            }
            lower::Instr::CastObject {
                dst,
                value,
                input,
                class,
                type_error,
                span,
            } => {
                let check = CheckedCall {
                    sig: self.cast_sig(*input)?,
                    recheck: Some(self.cast_sig(lower::ValueKind::Object(None))?),
                    class: class.0,
                };
                self.emit_checked_call(*dst, *value, check, type_error, *span)?;
            }
            lower::Instr::CastClassObject {
                dst,
                value,
                input,
                bound,
                type_error,
                span,
            } => {
                let check = CheckedCall {
                    sig: self.cast_class_sig(*input)?,
                    recheck: Some(self.cast_class_sig(lower::ValueKind::Object(None))?),
                    class: bound.0,
                };
                self.emit_checked_call(*dst, *value, check, type_error, *span)?;
            }
            lower::Instr::EqU64 {
                dst,
                lhs,
                rhs,
                span,
            } => {
                self.set_span(*span);
                self.asm.u64_eq(*dst, *lhs, *rhs);
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
                self.asm.br(*cond, then_label, else_label);
            }
            lower::Instr::Jump { label, span } => {
                let label = self.label(*label)?;
                self.set_span(*span);
                self.asm.jmp(label);
            }
            lower::Instr::Move { dst, src, span } => {
                self.set_span(*span);
                self.asm.mov(*dst, *src);
            }
            lower::Instr::Return { values, span } => {
                self.set_span(*span);
                self.asm.ret(Reg::EFFECT, values);
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
                        self.asm.host_call(
                            Reg::EFFECT,
                            guarded,
                            Reg::EFFECT,
                            &[descriptor_reg, *guard],
                            &[*dst],
                        );
                    }
                    None => {
                        self.asm.host_call(
                            Reg::EFFECT,
                            plain,
                            Reg::EFFECT,
                            &[descriptor_reg],
                            &[*dst],
                        );
                    }
                }
            }
            lower::Instr::PopBinding { token, span } => {
                let sig = self.abi.control()?.pop;
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*token], &[]);
            }
            lower::Instr::Signal {
                dsts,
                condition,
                span,
            } => {
                let results = dsts
                    .iter()
                    .map(|dst| self.register_type(*dst))
                    .collect::<Result<Vec<_>, _>>()?;
                self.emit_signal(&results, dsts, *condition, *span)?;
            }
            lower::Instr::UnboxObject {
                dst,
                value,
                otherwise,
                span,
            } => {
                // A boxed value's key carries `SCALAR_KEY_BIT` unless it
                // holds a runtime object, which `obj.convert` unboxes; a
                // scalar goes to `otherwise`. Neither is a host call.
                let otherwise = self.label(*otherwise)?;
                let key = self.alloc_temp(ValueType::U64);
                let bit = self.alloc_temp(ValueType::U64);
                let is_object = self.alloc_temp(ValueType::Bool);
                let object = self.asm.label();
                self.set_span(*span);
                self.asm
                    .obj_key(key, *value)
                    .const_u64(bit, runtime::SCALAR_KEY_BIT)
                    .u64_lt(is_object, key, bit)
                    .br(is_object, object, otherwise);
                self.asm
                    .place(object)
                    .map_err(|_| invalid_lowered("unbox label is placed twice"))?;
                self.asm.obj_convert(*dst, *value);
            }
            lower::Instr::CheckedRuntimeCall {
                dst,
                op,
                args,
                failed,
                span,
            } => {
                // The call raises the check-failed tag when it refuses its
                // arguments; the passing path is the call alone.
                let check_failed = self.abi.control()?.check_failed;
                let sig = self.abi.runtime_call(*op)?;
                let failed = self.label(*failed)?;
                self.set_span(*span);
                self.asm.begin_region(&[Catch::Tag(check_failed, failed)]);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, args, &[*dst]);
                self.asm
                    .end_region()
                    .ok_or_else(|| invalid_lowered("checked call region is not open"))?;
            }
            lower::Instr::NoApplicableMethod {
                dst,
                generic,
                arguments,
                span,
            } => {
                let sig = self.abi.control()?.no_applicable_method;
                let generic_id = self.alloc_temp(ValueType::U64);
                self.set_span(*span);
                self.asm.const_u64(generic_id, u64::from(generic.0));
                self.asm.host_call(
                    Reg::EFFECT,
                    sig,
                    Reg::EFFECT,
                    &[generic_id, *arguments],
                    &[*dst],
                );
            }
            lower::Instr::SignalTypeError {
                dst,
                values,
                type_error,
                resume,
                span,
            } => {
                let types = values
                    .iter()
                    .map(|value| self.register_type(*value))
                    .collect::<Result<Vec<_>, _>>()?;
                let site = self.type_error_site(type_error, *span)?;
                let condition = self.shared_temp(SharedTemp::Condition, runtime::object_type());
                let mut args = values.clone();
                args.push(site);
                let sig = self.abi.control()?.type_error_sig(&types)?;
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &args, &[condition]);
                let result = self.register_type(*dst)?;
                self.emit_signal(&[result], &[*dst], condition, *span)?;
                let resume = self.label(*resume)?;
                self.asm.jmp(resume);
            }
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
                self.asm.throw_ref(Reg::EFFECT, *exception);
            }
            lower::Instr::PushExit { dst, span } => {
                let sig = self.abi.control()?.push_exit;
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[], &[*dst]);
            }
            lower::Instr::CheckExit { token, span } => {
                let sig = self.abi.control()?.check_exit;
                self.set_span(*span);
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[*token], &[]);
            }
            lower::Instr::Throw { tag, payload, span } => {
                let tag = self.abi.tag(*tag)?;
                self.set_span(*span);
                self.builder
                    .emit_throw(&mut self.asm, tag, payload)
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
        results: &[ValueType],
        dsts: &[Reg],
        condition: Reg,
        span: Span,
    ) -> Result<(), TapeError> {
        let control = self.abi.control()?;
        let tag = control.condition_tag(results)?;
        let (signal, guard, guard_result, guard_unwound, guard_call) = (
            control.signal,
            control.guard,
            control.guard_result,
            control.guard_unwound,
            control.guard_call,
        );
        let pending = self.shared_temp(SharedTemp::Pending, ValueType::Bool);
        let closure = self.shared_temp(SharedTemp::Guard, ValueType::Closure);
        let accepted = self.shared_temp(SharedTemp::Accepted, ValueType::Bool);
        let escaped = self.shared_temp(SharedTemp::Escaped, ValueType::Exn);
        let (search, run_guard, none) = (self.asm.label(), self.asm.label(), self.asm.label());
        let unwound = self.asm.block(&[escaped]);
        self.set_span(span);
        self.asm
            .host_call(Reg::EFFECT, signal, Reg::EFFECT, &[condition], &[pending]);
        // Every block ends in a terminator: control never falls into a label.
        self.asm.jmp(search);
        self.asm.place(search).map_err(build_error)?;
        self.asm.br(pending, run_guard, none);
        self.asm.place(run_guard).map_err(build_error)?;
        self.asm
            .host_call(Reg::EFFECT, guard, Reg::EFFECT, &[], &[closure]);
        self.asm.begin_region(&[Catch::AllRef(unwound)]);
        self.asm.call_indirect(
            Reg::EFFECT,
            guard_call,
            closure,
            Reg::EFFECT,
            &[condition],
            &[accepted],
        );
        self.asm
            .end_region()
            .ok_or_else(|| invalid_lowered("guard region is not open"))?;
        self.asm.host_call(
            Reg::EFFECT,
            guard_result,
            Reg::EFFECT,
            &[accepted],
            &[pending],
        );
        self.asm.jmp(search);
        self.asm.place(unwound).map_err(build_error)?;
        self.asm
            .host_call(Reg::EFFECT, guard_unwound, Reg::EFFECT, &[], &[]);
        self.asm.throw_ref(Reg::EFFECT, escaped);
        self.asm.place(none).map_err(build_error)?;
        self.asm
            .suspend(Reg::EFFECT, tag, Reg::EFFECT, &[condition], dsts);
        Ok(())
    }

    /// Registers `type_error` in the function's sites and loads its index,
    /// the site operand of a `<type-error>` request, into a register.
    fn type_error_site(
        &mut self,
        type_error: &TypeErrorSite,
        span: Span,
    ) -> Result<Reg, TapeError> {
        let index = u64::try_from(self.type_errors.len())
            .map_err(|_| invalid_lowered("too many type error sites"))?;
        self.type_errors.push(type_error.clone());
        let site = self.shared_temp(SharedTemp::Site, ValueType::U64);
        self.set_span(span);
        self.asm.const_u64(site, index);
        Ok(site)
    }

    /// Emits a text call that can reject its argument `checked`, and what
    /// happens when it does ([`lower::TextCheck`]):
    ///
    /// ```text
    /// retry:  region (check-failed -> failed)
    ///           dst = call(args)
    ///         end region
    ///         jmp done
    /// failed: condition = type-error(boxed checked, site)
    ///         checked = signal(condition)    // use-value
    ///         jmp retry
    /// done:
    /// ```
    ///
    /// The operation checks every argument it is given, so a replacement
    /// is checked the same way when the call is made again.
    fn emit_checked_text_call(
        &mut self,
        dst: Reg,
        sig: HostSigId,
        args: &[Reg],
        checked: Reg,
        check: &lower::TextCheck,
        span: Span,
    ) -> Result<(), TapeError> {
        let control = self.abi.control()?;
        let check_failed = control.check_failed;
        let (retry, failed, done) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.asm.jmp(retry);
        self.asm.place(retry).map_err(build_error)?;
        self.set_span(span);
        self.asm.begin_region(&[Catch::Tag(check_failed, failed)]);
        self.asm
            .host_call(Reg::EFFECT, sig, Reg::EFFECT, args, &[dst]);
        self.asm
            .end_region()
            .ok_or_else(|| invalid_lowered("check region is not open"))?;
        self.asm.jmp(done);
        self.asm.place(failed).map_err(build_error)?;
        // The condition holds the rejected value as the class it is: a
        // character's `u32` register would box as an `<integer>`.
        let rejected_box = match check.kind {
            lower::ValueKind::Char => Some(self.builder.host_sig_for(
                runtime::BOX_REJECTED_CHARACTER,
                runtime::box_character_signature(),
            )),
            lower::ValueKind::Position => Some(self.builder.host_sig_for(
                runtime::BOX_REJECTED_POSITION,
                runtime::box_position_signature(),
            )),
            _ => None,
        };
        let (reported, reported_type) = match rejected_box {
            Some(sig) => {
                let boxed = self.alloc_temp(runtime::any_type());
                self.asm
                    .host_call(Reg::EFFECT, sig, Reg::EFFECT, &[checked], &[boxed]);
                (boxed, runtime::any_type())
            }
            None => (checked, self.register_type(checked)?),
        };
        let site = self.type_error_site(&check.type_error, span)?;
        let condition = self.shared_temp(SharedTemp::Condition, runtime::object_type());
        let report = self.abi.control()?.type_error_sig(&[reported_type])?;
        self.asm.host_call(
            Reg::EFFECT,
            report,
            Reg::EFFECT,
            &[reported, site],
            &[condition],
        );
        let replacement = self.register_type(checked)?;
        self.emit_signal(&[replacement], &[checked], condition, span)?;
        self.asm.jmp(retry);
        self.asm.place(done).map_err(build_error)?;
        Ok(())
    }

    /// Emits a checking host call, `dst = check(operand, class)`, and what
    /// happens when it fails.
    ///
    /// The call sits in a protected region: a check that rejects its value
    /// raises the check-failed tag, and the passing path is the call and a
    /// jump. A failure signals `type_error` for the operand. A `use-value`
    /// restart resumes it with a replacement in the result's register type;
    /// when that type does not establish the check (an object for a class),
    /// the replacement is checked again with `check.recheck`, signalling
    /// again for as long as replacements fail:
    ///
    /// ```text
    ///           region [check_failed -> failed]
    ///             dst = host_call check(operand, class)
    ///           jmp done
    /// failed:   condition = host_call type_error(operand, site)
    ///           jmp retry
    /// retry:    replacement = <signal condition>
    ///           region [check_failed -> refailed]
    ///             dst = host_call recheck(replacement, class)
    ///           jmp done
    /// refailed: condition = host_call type_error(replacement, site)
    ///           jmp retry
    /// done:
    /// ```
    ///
    /// Without a recheck, the signal resumes straight into `dst`.
    fn emit_checked_call(
        &mut self,
        dst: Reg,
        operand: Reg,
        check: CheckedCall,
        type_error: &TypeErrorSite,
        span: Span,
    ) -> Result<(), TapeError> {
        let control = self.abi.control()?;
        let check_failed = control.check_failed;
        let class = self.alloc_temp(ValueType::U64);
        let (failed, done) = (self.asm.label(), self.asm.label());
        self.set_span(span);
        self.asm.const_u64(class, u64::from(check.class));
        self.asm.begin_region(&[Catch::Tag(check_failed, failed)]);
        self.asm.host_call(
            Reg::EFFECT,
            check.sig,
            Reg::EFFECT,
            &[operand, class],
            &[dst],
        );
        self.asm
            .end_region()
            .ok_or_else(|| invalid_lowered("check region is not open"))?;
        self.asm.jmp(done);
        self.asm.place(failed).map_err(build_error)?;
        let site = self.type_error_site(type_error, span)?;
        let condition = self.shared_temp(SharedTemp::Condition, runtime::object_type());
        let operand_type = self.register_type(operand)?;
        let report = control.type_error_sig(&[operand_type])?;
        self.asm.host_call(
            Reg::EFFECT,
            report,
            Reg::EFFECT,
            &[operand, site],
            &[condition],
        );
        let result = self.register_type(dst)?;
        match check.recheck {
            None => {
                self.emit_signal(&[result], &[dst], condition, span)?;
                self.asm.jmp(done);
            }
            Some(recheck) => {
                let replacement = self.shared_temp(SharedTemp::Replacement, result);
                let report_again = control.type_error_sig(&[result])?;
                let (retry, refailed) = (self.asm.label(), self.asm.label());
                self.asm.jmp(retry);
                self.asm.place(retry).map_err(build_error)?;
                self.emit_signal(&[result], &[replacement], condition, span)?;
                self.asm.begin_region(&[Catch::Tag(check_failed, refailed)]);
                self.asm.host_call(
                    Reg::EFFECT,
                    recheck,
                    Reg::EFFECT,
                    &[replacement, class],
                    &[dst],
                );
                self.asm
                    .end_region()
                    .ok_or_else(|| invalid_lowered("recheck region is not open"))?;
                self.asm.jmp(done);
                self.asm.place(refailed).map_err(build_error)?;
                self.asm.host_call(
                    Reg::EFFECT,
                    report_again,
                    Reg::EFFECT,
                    &[replacement, site],
                    &[condition],
                );
                self.asm.jmp(retry);
            }
        }
        self.asm.place(done).map_err(build_error)?;
        Ok(())
    }

    fn label(&self, label: lower::LabelId) -> Result<Label, TapeError> {
        self.labels
            .get(label.0 as usize)
            .copied()
            .ok_or_else(|| invalid_lowered("label id is out of range"))
    }

    fn register_type(&self, register: Reg) -> Result<ValueType, TapeError> {
        register_type(self.function, register)
    }

    /// The function's register for `role` of type `kind`, allocated on
    /// first use.
    fn shared_temp(&mut self, role: SharedTemp, kind: ValueType) -> Reg {
        if let Some((_, _, register)) = self
            .shared
            .iter()
            .find(|(candidate, ty, _)| *candidate == role && *ty == kind)
        {
            return *register;
        }
        let register = self.alloc_temp(kind);
        self.shared.push((role, kind, register));
        register
    }

    fn alloc_temp(&mut self, kind: ValueType) -> Reg {
        self.asm.alloc_register(kind)
    }

    fn set_span(&mut self, span: Span) {
        self.asm.span(span_id(span));
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

    fn cast_class_sig(&self, kind: lower::ValueKind) -> Result<HostSigId, TapeError> {
        let (any, object) = self
            .abi
            .cast_class
            .ok_or_else(|| invalid_lowered("class object cast signature is missing"))?;
        match kind {
            lower::ValueKind::Any => Ok(any),
            lower::ValueKind::Object(_) | lower::ValueKind::ClassObject(_) => Ok(object),
            _ => Err(invalid_lowered("class object cast input kind is invalid")),
        }
    }
}

fn register_type(function: &lower::Function, register: Reg) -> Result<ValueType, TapeError> {
    function
        .register_kind(register)
        .map(value_type)
        .ok_or_else(|| invalid_lowered("register is out of range"))
}

fn labels_for(function: &lower::Function, asm: &mut Asm) -> Vec<Label> {
    let mut count = 0_usize;
    let mut catch_params: Vec<(usize, &[Reg])> = Vec::new();
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
                Some((_, params)) => asm.block(params),
                None => asm.label(),
            },
        )
        .collect()
}

fn invalid_lowered(message: &str) -> TapeError {
    TapeError::InvalidLowered {
        message: message.into(),
    }
}

/// Encodes a source span as its start offset plus one; span ids are non-zero.
fn span_id(span: Span) -> SpanId {
    let start = u64::try_from(span.start).unwrap_or(u64::MAX - 1);
    SpanId::new(start.saturating_add(1))
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
        let typing = crate::typing::analyze(&sem, crate::typing::TypingConfig::default());
        lower::lower(&sem, &typing, lower::CodegenPolicy::Off).expect("lowering should succeed")
    }

    fn runtime_class<'a>(tape: &'a TapeModule, name: &str) -> &'a runtime::RuntimeClass {
        tape.runtime()
            .classes
            .iter()
            .find(|class| class.name == name)
            .unwrap_or_else(|| panic!("missing runtime class {name}"))
    }

    #[test]
    fn verifier_rejection_names_the_function_and_pc() {
        let mut module = lower_source("define function main()\n  42\nend;");
        let main = &mut module.functions[0];
        // Declare a float result for a body that returns an integer, so the
        // emitted program reaches the verifier and is rejected there.
        main.ret = lower::ValueKind::F64;
        main.rets = vec![lower::ValueKind::F64];
        let Err(TapeError::Build { message }) = emit(&module) else {
            panic!("a mistyped return should fail verification");
        };
        assert!(
            message.starts_with("verification failed: function #0 \"main\", pc "),
            "{message}"
        );
        assert!(message.contains("declared I64, required F64"), "{message}");
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
                .function_arg_types(program.function(FuncId(1)).expect("callit"))
                .expect("callit args should decode"),
            &[ValueType::Agg],
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
        // The four lowered functions and the generic's miss stub.
        assert_eq!(tape.program().program().functions.len(), 6);
        assert_eq!(tape.main, Some(FuncId(0)));
        let circle = runtime_class(&tape, "<circle>");
        assert_eq!(circle.slots[0].name, "radius");
        assert_eq!(circle.slots[0].kind, runtime::ValueKind::I64);
    }

    #[test]
    fn emits_verified_tape_for_boolean_control_flow() {
        let module = lower_source("define function main()\n  if (10 >= 5) 42 else 0 end\nend;");
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
             define function main()\n  if (make(<flag>, valid?: #t).valid?) 1 else 0 end\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");

        assert_eq!(tape.program().program().functions.len(), 3);
        let flag = runtime_class(&tape, "<flag>");
        assert_eq!(flag.slots[0].name, "valid?");
        assert_eq!(flag.slots[0].kind, runtime::ValueKind::Bool);
    }

    #[test]
    fn instance_tests_read_keys_unless_too_many_classes_apply() {
        // `<circle>` has no subclasses, so `instance?` compares one key;
        // `<object>` covers every class, so it still asks the host.
        let module = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\nend;\n\
             define function round?(x :: <object>) => <boolean>\n  instance?(x, <circle>)\nend;\n\
             define function any?(x :: <object>) => <boolean>\n  instance?(x, <object>)\nend;\n\
             define class <four> (<object>)\nend;\n\
             define class <f1> (<four>)\nend;\ndefine class <f2> (<four>)\nend;\n\
             define class <f3> (<four>)\nend;\n\
             define class <five> (<object>)\nend;\n\
             define class <v1> (<five>)\nend;\ndefine class <v2> (<five>)\nend;\n\
             define class <v3> (<five>)\nend;\ndefine class <v4> (<five>)\nend;\n\
             define function four?(x :: <object>) => <boolean>\n  instance?(x, <four>)\nend;\n\
             define function five?(x :: <object>) => <boolean>\n  instance?(x, <five>)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let text = |name: &str| {
            let function = module
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("function");
            execution_tape::disasm::disassemble_function(
                tape.program().program(),
                FuncId(function.id.0),
            )
            .expect("disassembles")
            .instrs()
            .map(|instr| instr.to_string())
            .collect::<Vec<_>>()
            .join("\n")
        };
        let round = text("round?");
        assert!(round.contains("obj.key"), "{round}");
        assert!(!round.contains("interloko.instance_of"), "{round}");
        let any = text("any?");
        assert!(any.contains("interloko.instance_of"), "{any}");
        // Up to four accepted classes compare keys; five ask the host.
        let four = text("four?");
        assert!(!four.contains("interloko.instance_of"), "{four}");
        let five = text("five?");
        assert!(five.contains("interloko.instance_of"), "{five}");
    }

    #[test]
    fn untyped_receivers_unbox_and_objects_box_without_host_calls() {
        // `describe` sends to an `<object>`, which the send unboxes to the
        // object it holds; `wrap` passes a `<shape>` as an `<object>`.
        let module = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define generic name(s) => <integer>;\n\
             define method name(s :: <shape>) => <integer>\n  1\nend;\n\
             define function describe(x :: <object>) => <integer>\n  name(x)\nend;\n\
             define function wrap(s :: <shape>) => <integer>\n  describe(s)\nend;",
        );
        let tape = emit(&module).expect("tape emission should succeed");
        let text = |name: &str| {
            let function = module
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("function");
            execution_tape::disasm::disassemble_function(
                tape.program().program(),
                FuncId(function.id.0),
            )
            .expect("disassembles")
            .instrs()
            .map(|instr| instr.to_string())
            .collect::<Vec<_>>()
            .join("\n")
        };
        let describe = text("describe");
        assert!(describe.contains("obj.convert"), "{describe}");
        assert!(!describe.contains("interloko.cast_object"), "{describe}");
        let wrap = text("wrap");
        assert!(wrap.contains("obj.convert"), "{wrap}");
        assert!(!wrap.contains("interloko.box_value"), "{wrap}");
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
