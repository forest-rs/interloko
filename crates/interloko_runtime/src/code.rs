//! Compiled Interloko programs as code units a run installs and links.
//!
//! Each compile produces one verified program. [`CodeUnit`] carries it with
//! what the runtime needs beside the tape: the runtime layout its objects and
//! dispatch assume, its control metadata, and which tape function implements
//! each source definition. [`crate::driver::run`] installs the entry unit in
//! the VM, and a chooser may install a replacement while a condition is
//! unhandled. The VM keeps every unit, so a frame keeps running the code it
//! entered; definitions resolve to the newest unit that has them.
//!
//! Generic sends are late-bound: compiled code asks for a generic's current
//! dispatcher (see [`CODE_DISPATCHER`]) and calls it indirectly, so a frame
//! running superseded code still sends to the newest methods. Direct calls to
//! source functions stay within the caller's unit.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use execution_tape::host::HostSig;
use execution_tape::program::ValueType;
use execution_tape::value::{FuncId, FuncRef, UnitId};
use execution_tape::verifier::VerifiedProgram;
use interloko_model::{GenericFunctionId, MethodId};

use crate::RuntimeLayout;
use crate::control::ControlLayout;

/// Host symbol that resolves a generic function to its current dispatcher.
///
/// The argument is the generic's id in the shared runtime layout; the result
/// is a function value for `call.indirect`.
pub const CODE_DISPATCHER: &str = "interloko.code.dispatcher";

/// Host signature for [`CODE_DISPATCHER`]: `(generic) -> dispatcher`.
#[must_use]
pub fn dispatcher_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64],
        rets: alloc::vec![ValueType::Func],
    }
}

/// One compiled program and the metadata that links it into a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeUnit {
    /// Verified executable tape.
    pub program: Arc<VerifiedProgram>,
    /// Runtime layout the program's objects, slots and dispatch assume.
    pub layout: RuntimeLayout,
    /// Restart sites the driver needs for the program's handlers.
    pub control: ControlLayout,
    /// Dispatcher function of each generic, in layout generic order.
    pub generics: Vec<FuncId>,
    /// Function of each method, in layout method order.
    pub methods: Vec<FuncId>,
    /// Source functions by name.
    pub functions: Vec<(String, FuncId)>,
}

/// The source definition a tape function implements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Definition {
    /// A source function, identified by name.
    Function(String),
    /// A method, identified by its id in the shared runtime layout.
    Method(MethodId),
    /// A generic function's dispatcher.
    Generic(GenericFunctionId),
}

impl Definition {
    /// Whether frames of this definition can be retried: source functions
    /// and methods compile with a retry region, dispatchers do not.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Function(_) | Self::Method(_))
    }
}

/// Why a unit cannot replace the code a run already installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeMismatch {
    /// The units disagree on classes, slots, generics or methods, so objects
    /// and dispatch would change meaning under live frames.
    Layout,
    /// A generic's dispatcher takes or returns different kinds, so sends from
    /// older units could not call it.
    GenericSignature {
        /// The generic whose signature changed.
        generic: GenericFunctionId,
    },
}

impl core::fmt::Display for CodeMismatch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Layout => {
                formatter.write_str("the runtime layout changed (classes, slots or methods)")
            }
            Self::GenericSignature { generic } => {
                write!(formatter, "generic #{} changed its signature", generic.0)
            }
        }
    }
}

/// A function's argument and result kinds.
pub(crate) type Signature = (Vec<ValueType>, Vec<ValueType>);

impl CodeUnit {
    /// The source definition `func` implements, if any.
    #[must_use]
    pub fn definition(&self, func: FuncId) -> Option<Definition> {
        if let Some(index) = self
            .generics
            .iter()
            .position(|candidate| *candidate == func)
        {
            return Some(Definition::Generic(GenericFunctionId(index_u32(index))));
        }
        if let Some(index) = self.methods.iter().position(|candidate| *candidate == func) {
            return Some(Definition::Method(MethodId(index_u32(index))));
        }
        self.functions
            .iter()
            .find(|(_, candidate)| *candidate == func)
            .map(|(name, _)| Definition::Function(name.clone()))
    }

    /// The function implementing `definition` in this unit, if it has one.
    #[must_use]
    pub fn function(&self, definition: &Definition) -> Option<FuncId> {
        match definition {
            Definition::Generic(generic) => self.generics.get(generic.0 as usize).copied(),
            Definition::Method(method) => self.methods.get(method.0 as usize).copied(),
            Definition::Function(name) => self
                .functions
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, func)| *func),
        }
    }

    /// Checks that this unit can take over from `older` in one run.
    pub fn check_replaces(&self, older: &Self) -> Result<(), CodeMismatch> {
        if self.layout != older.layout || self.generics.len() != older.generics.len() {
            return Err(CodeMismatch::Layout);
        }
        for (index, (new, old)) in self.generics.iter().zip(&older.generics).enumerate() {
            if self.signature(*new) != older.signature(*old) {
                return Err(CodeMismatch::GenericSignature {
                    generic: GenericFunctionId(index_u32(index)),
                });
            }
        }
        Ok(())
    }

    /// Argument and result kinds of `func`.
    /// Whether `func` here and `other_func` in `other` have the same
    /// signature and bytecode. Unchanged definitions recompile identically
    /// unless an edit renumbered what their code refers to.
    pub(crate) fn same_code(&self, func: FuncId, other: &Self, other_func: FuncId) -> bool {
        self.signature(func) == other.signature(other_func)
            && self.bytecode(func).is_some()
            && self.bytecode(func) == other.bytecode(other_func)
    }

    fn bytecode(&self, func: FuncId) -> Option<&[u8]> {
        let program = self.program.program();
        program
            .functions
            .get(func.0 as usize)?
            .bytecode(program)
            .ok()
    }

    pub(crate) fn signature(&self, func: FuncId) -> Option<Signature> {
        let program = self.program.program();
        let function = program.functions.get(func.0 as usize)?;
        let args = program.function_arg_types(function).ok()?;
        let rets = program.function_ret_types(function).ok()?;
        Some((
            args.iter().cloned().collect(),
            rets.iter().cloned().collect(),
        ))
    }
}

/// Where a suspended frame's code stands among a run's installed units.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameCode {
    /// The function the frame runs.
    pub func: FuncRef,
    /// That function's name in its unit's program, when the program names it.
    pub name: Option<String>,
    /// The source definition it implements, if it implements one.
    pub definition: Option<Definition>,
    /// The function a new call of that definition would run.
    pub current: Option<FuncRef>,
    /// Whether that function's code differs from the code this frame runs:
    /// a newer unit changed the definition under the frame.
    pub superseded: bool,
}

impl FrameCode {
    /// Whether the frame can be retried with its current definition.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.current.is_some()
            && self
                .definition
                .as_ref()
                .is_some_and(Definition::is_retryable)
    }
}

/// The code units one run installed, oldest first.
#[derive(Debug, Default)]
pub(crate) struct UnitTable {
    units: Vec<(UnitId, Arc<CodeUnit>)>,
}

impl UnitTable {
    pub(crate) fn push(&mut self, unit: UnitId, code: Arc<CodeUnit>) {
        self.units.push((unit, code));
    }

    pub(crate) fn newest(&self) -> Option<&Arc<CodeUnit>> {
        self.units.last().map(|(_, code)| code)
    }

    pub(crate) fn get(&self, unit: UnitId) -> Option<&CodeUnit> {
        self.units
            .iter()
            .find(|(candidate, _)| *candidate == unit)
            .map(|(_, code)| code.as_ref())
    }

    /// The newest function implementing `definition`.
    pub(crate) fn current(&self, definition: &Definition) -> Option<FuncRef> {
        self.units
            .iter()
            .rev()
            .find_map(|(unit, code)| Some(FuncRef::new(*unit, code.function(definition)?)))
    }

    pub(crate) fn signature(&self, func: FuncRef) -> Option<Signature> {
        self.get(func.unit)?.signature(func.func)
    }

    pub(crate) fn frame(&self, func: FuncRef) -> FrameCode {
        let definition = self
            .get(func.unit)
            .and_then(|code| code.definition(func.func));
        let current = definition
            .as_ref()
            .and_then(|definition| self.current(definition));
        let superseded = current.is_some_and(|current| {
            current != func
                && match (self.get(func.unit), self.get(current.unit)) {
                    (Some(old), Some(new)) => !old.same_code(func.func, new, current.func),
                    _ => true,
                }
        });
        let name = self.get(func.unit).and_then(|code| {
            code.program
                .program()
                .function_name(func.func.0)
                .map(String::from)
        });
        FrameCode {
            func,
            name,
            definition,
            current,
            superseded,
        }
    }
}

fn index_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}
