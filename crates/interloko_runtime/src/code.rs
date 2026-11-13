//! Compiled Interloko programs as code units a run installs and links.
//!
//! Each compile produces one verified program. [`CodeUnit`] carries it with
//! what the runtime needs beside the tape: the runtime layout its objects and
//! dispatch assume, its control metadata, and which tape function implements
//! each source definition. [`crate::driver::run`] installs the entry unit in
//! the VM. The VM keeps every unit, so a frame keeps running the code it
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
}

fn index_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}
