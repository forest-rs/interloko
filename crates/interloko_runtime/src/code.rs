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
//! Generic sends are late-bound and dispatched by the runtime: compiled code
//! asks [`CODE_DISPATCH`] for the method its arguments select and calls it
//! indirectly. The driver chooses from the newest unit's [`GenericDispatch`]
//! table and caches the choice per argument classes until the next install,
//! so a frame running superseded code still sends to the newest methods.
//! When the table cannot decide (scalar dispatch, or a method that needs its
//! arguments converted), the answer is the newest unit's compiled dispatcher.
//! Direct calls to source functions stay within the caller's unit.
//!
//! A send whose argument classes typing proved skips the request: it calls
//! the selected method through an `execution_tape` link, as cheap as a
//! direct call. Every program declares one link per method of the runtime
//! layout, `LinkId(m)` for method `m`, so link ids depend on the layout alone
//! and an edit elsewhere renumbers none. The layout fixes which method
//! applies to given classes, so a link only has to follow its method's
//! newest code: whenever a run installs a unit, the driver repoints every
//! link a unit's sends use at the newest unit's version of the method, or at
//! its dispatcher when that version converts its arguments or results
//! ([`CodeUnit::link_target`]), exactly where a request would lead.
//! Activations already running a method keep it.
//!
//! # Contracts
//!
//! Compiled code trusts the types its compiler resolved: a send whose
//! generic was inferred to return `<circle>` reads the result's slots as a
//! `<circle>`'s, and a function whose parameter is declared `:: <circle>`
//! reads its argument the same way. A tape signature erases both to
//! `object`, so each unit also carries the [`Contract`] of every generic,
//! method and function it defines ([`Contracts`]), and newer code may take
//! over a definition from older code that is still running only when its
//! contract keeps the older one's promise ([`Contract::check_replaces`]):
//! parameters may widen and results may narrow. A callable parameter or
//! result also records the signature callers call it with, compared by the
//! same rule.
//!
//! The rule applies exactly where code compiled against one unit can reach
//! another unit's definition, which happens only within one run, after a
//! chooser installs a unit while frames are suspended:
//!
//! - a generic send is late-bound, dynamically or through a repointed link,
//!   so an old frame may send to the newest unit's methods;
//!   [`CodeUnit::check_replaces`] refuses a unit that changes a generic's
//!   contract incompatibly;
//! - a retried frame calls the newest version of its function or method,
//!   with the frame's own arguments, and returns to the frame's old caller;
//!   such a retry is refused ([`FrameCode::retry_refusal`]) when that
//!   definition's contract changed incompatibly.
//!
//! Direct function calls stay within their unit, so a unit that changes a
//! function's contract installs, and only retrying a frame of the old
//! function is refused. A run starts from one unit and checks nothing
//! against units from earlier runs, so recompiling and running again
//! accepts any change.
//!
//! The rule needs both contracts: a definition that either unit records no
//! contract for cannot take over or be taken over
//! ([`CodeMismatch::Uncontracted`]), since nothing says what code compiled
//! against it relies on. Compiled units record one for every definition.
//!
//! # Closed-world assumptions
//!
//! Code compiled under proved typing may also rely on the closed world:
//! that a sealed class has exactly its direct subclasses (so a cast past an
//! `instance?` test is dropped), or that a generic has exactly its methods
//! (so a send calls its method through a link). Each unit lists those
//! ([`CodeUnit::assumptions`]), and a unit takes over only when every
//! assumption of every unit whose code may still run holds in its layout
//! ([`CodeUnit::check_assumptions`]); a refusal names the assumption, as
//! [`CodeMismatch::Assumption`]. The layout rule refuses every such change
//! anyway, since classes and methods are part of the layout; the
//! assumption says which promise the change would break, and lets the rule
//! relax for changes nothing relies on.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use execution_tape::host::HostSig;
use execution_tape::program::{LinkId, ValueType};
use execution_tape::value::{FuncId, FuncRef, UnitId};
use execution_tape::verifier::VerifiedProgram;
use interloko_model::contract::{Contract, ContractBreak, ContractPosition, ValueContract};
use interloko_model::sealing::ClosedWorld;
use interloko_model::{ClassId, GenericFunctionId, MethodId};

use crate::RuntimeLayout;
use crate::control::ControlLayout;

/// Host symbol that chooses the function a generic send calls.
///
/// The arguments are the generic's id in the shared runtime layout, then the
/// send's own arguments; the result is a function value taking those
/// arguments, for `call.indirect`: the selected method, or the generic's
/// dispatcher.
pub const CODE_DISPATCH: &str = "interloko.code.dispatch";

/// Host signature for [`CODE_DISPATCH`]: `(generic, args...) -> function`.
#[must_use]
pub fn dispatch_signature(args: &[ValueType]) -> HostSig {
    let mut all = alloc::vec![ValueType::U64];
    all.extend_from_slice(args);
    HostSig {
        args: all,
        rets: alloc::vec![ValueType::Func],
    }
}

/// How the runtime selects a generic's method, in the order its compiled
/// dispatcher tests them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenericDispatch {
    /// Methods, most specific first. The first whose specializers all
    /// accept the arguments' classes is selected.
    pub cases: Vec<DispatchCase>,
    /// Argument positions some case specializes, whose classes select.
    pub positions: Vec<usize>,
}

/// One method in a [`GenericDispatch`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchCase {
    /// The method, by its id in the shared runtime layout.
    pub method: MethodId,
    /// Class each argument must be an instance of; `None` accepts anything.
    pub specializers: Vec<Option<ClassId>>,
    /// Whether the method takes the send's arguments and returns its results
    /// unchanged, so the send can call it without the dispatcher.
    pub direct: bool,
}

impl GenericDispatch {
    /// The case selected for arguments of `class_of` each position, or
    /// `None` when no method applies.
    pub fn select(
        &self,
        class_of: impl Fn(usize) -> Option<ClassId>,
        is_subclass: impl Fn(ClassId, ClassId) -> bool,
    ) -> Option<&DispatchCase> {
        self.cases.iter().find(|case| {
            case.specializers
                .iter()
                .enumerate()
                .all(|(index, specializer)| match specializer {
                    None => true,
                    Some(specializer) => {
                        class_of(index).is_some_and(|class| is_subclass(class, *specializer))
                    }
                })
        })
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
    /// Runtime method selection of each generic, in layout generic order;
    /// `None` leaves the choice to the generic's dispatcher.
    pub dispatch: Vec<Option<GenericDispatch>>,
    /// The methods whose links the program's sends call, ascending. The
    /// program declares a link for every method ([`method_link`]); these are
    /// the ones a run must keep repointing.
    pub links: Vec<MethodId>,
    /// What code compiled against each definition relies on. A definition
    /// with no contract here (as in a unit built without any) cannot take
    /// over from another unit's or be taken over by one
    /// ([`CodeMismatch::Uncontracted`]).
    pub contracts: Contracts,
    /// The closed-world assumptions the program's code relies on: sealed
    /// classes whose direct subclasses it read, and generics whose method
    /// sets it read, with source names. A newer unit may take over only
    /// when every one still holds ([`CodeUnit::check_assumptions`]).
    pub assumptions: Vec<ClosedWorld>,
}

/// The [`Contract`] of each definition a [`CodeUnit`] implements, as its
/// compiler resolved them.
///
/// Types name classes by their ids in the unit's [`RuntimeLayout`], which is
/// the hierarchy they are compared in.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Contracts {
    /// Contract of each generic, in layout generic order.
    pub generics: Vec<Contract>,
    /// Contract of each method, in layout method order.
    pub methods: Vec<Contract>,
    /// Contract of each source function, by name.
    pub functions: Vec<(String, Contract)>,
}

impl Contracts {
    /// The contract of `definition`, if the unit records one.
    #[must_use]
    pub fn get(&self, definition: &Definition) -> Option<&Contract> {
        match definition {
            Definition::Generic(generic) => self.generics.get(generic.0 as usize),
            Definition::Method(method) => self.methods.get(method.0 as usize),
            Definition::Function(name) => self
                .functions
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, contract)| contract),
        }
    }
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

/// Why a unit cannot replace the code a run already installed, or why a
/// frame cannot be retried with the newest code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodeMismatch {
    /// The units disagree on classes, slots, generics or methods, so objects
    /// and dispatch would change meaning under live frames.
    Layout,
    /// A definition takes or returns different value kinds, so code
    /// compiled against the older one could not call the newer one.
    Signature {
        /// The definition whose signature changed.
        definition: Definition,
        /// Its name, for messages.
        name: String,
    },
    /// A definition's contract changed in a way code compiled against the
    /// older one could observe.
    Contract(Box<ContractMismatch>),
    /// One of the units records no contract for a definition, so nothing
    /// says what code compiled against the older one relies on, or what the
    /// newer one promises.
    Uncontracted {
        /// The definition without a contract.
        definition: Definition,
        /// Its name, for messages.
        name: String,
    },
    /// A closed-world assumption code compiled against the older unit relies
    /// on does not hold in the newer one: a sealed class gained or lost a
    /// direct subclass, changed its abstractness or was unsealed, or a
    /// generic gained or lost a method or was unsealed.
    Assumption(Box<AssumptionMismatch>),
}

/// A closed-world assumption a newer unit would break; see
/// [`CodeUnit::check_assumptions`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssumptionMismatch {
    /// What the older unit's code relies on.
    pub assumption: ClosedWorld,
    /// What the newer unit's layout has instead for the same class or
    /// generic: the record it would make, or `None` when the class or
    /// generic is missing or, for a sealed class, open.
    pub now: Option<ClosedWorld>,
}

impl core::fmt::Display for AssumptionMismatch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "running code assumes {}", self.assumption)?;
        match (&self.assumption, &self.now) {
            (_, Some(now)) => write!(formatter, ", but the new code has {now}"),
            (ClosedWorld::SealedClass { class, .. }, None) => {
                write!(
                    formatter,
                    ", but the new code leaves {class} open or removes it"
                )
            }
            (ClosedWorld::Methods { generic, .. }, None) => {
                write!(formatter, ", but the new code removes generic {generic}")
            }
        }
    }
}

/// A contract change that code compiled against the older contract could
/// observe; see [`Contract::check_replaces`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractMismatch {
    /// The definition whose contract changed.
    pub definition: Definition,
    /// Its name, for messages.
    pub name: String,
    /// The incompatible change.
    pub change: ContractBreak,
    /// The old and new values of `change` written with the run's class
    /// names, such as `<circle>` or `callable(<integer>) => <circle>`;
    /// empty for [`ContractBreak::Arity`].
    pub types: (String, String),
}

impl core::fmt::Display for CodeMismatch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Layout => {
                formatter.write_str("the runtime layout changed (classes, slots or methods)")
            }
            Self::Signature { name, .. } => {
                write!(
                    formatter,
                    "{name} changed the value kinds it takes or returns"
                )
            }
            Self::Contract(mismatch) => write!(formatter, "{mismatch}"),
            Self::Uncontracted { name, .. } => write!(
                formatter,
                "{name} has no contract to compare, so what code compiled against it \
                 relies on is unknown"
            ),
            Self::Assumption(mismatch) => write!(formatter, "{mismatch}"),
        }
    }
}

impl core::fmt::Display for ContractMismatch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (old, new) = &self.types;
        let name = &self.name;
        match self.change {
            ContractBreak::Arity => {
                write!(
                    formatter,
                    "{name} changed how many values it takes or returns"
                )
            }
            ContractBreak::Value {
                position: ContractPosition::Parameter(index),
                ..
            } => write!(
                formatter,
                "{name} would take {new} as argument {} where callers pass {old}",
                index + 1
            ),
            ContractBreak::Value {
                position: ContractPosition::Result(index),
                ..
            } => write!(
                formatter,
                "{name} would return {new} as result {} where callers expect {old}",
                index + 1
            ),
        }
    }
}

/// A function's argument and result kinds.
pub(crate) type Signature = (Vec<ValueType>, Vec<ValueType>);

/// The link a program declares for `method`: link ids are method ids.
#[must_use]
pub const fn method_link(method: MethodId) -> LinkId {
    LinkId(method.0)
}

impl CodeUnit {
    /// The methods whose links this unit's sends call ([`Self::links`]).
    pub fn linked_methods(&self) -> impl Iterator<Item = MethodId> + '_ {
        self.links.iter().copied()
    }

    /// Where a send through the link for `method` must go once this unit,
    /// installed as `unit`, is a run's newest code: this unit's version of
    /// the method when its table calls it directly, as a request would, and
    /// otherwise the generic's dispatcher, which converts and checks as a
    /// dynamic send does.
    ///
    /// `None` when this unit's layout lacks the method, which units of one
    /// run never do.
    #[must_use]
    pub fn link_target(&self, unit: UnitId, method: MethodId) -> Option<FuncRef> {
        let generic = self.layout.methods.get(method.0 as usize)?.generic;
        let direct = self
            .dispatch
            .get(generic.0 as usize)?
            .as_ref()
            .is_some_and(|plan| {
                plan.cases
                    .iter()
                    .any(|case| case.method == method && case.direct)
            });
        let func = if direct {
            *self.methods.get(method.0 as usize)?
        } else {
            *self.generics.get(generic.0 as usize)?
        };
        Some(FuncRef::new(unit, func))
    }

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

    /// Checks that this unit can take over from `older` in one run, while
    /// frames running `older` (or units it took over from) are suspended.
    ///
    /// Every closed-world assumption `older`'s code relies on must still
    /// hold ([`Self::check_assumptions`]), and the layouts must be equal; a
    /// layout change that breaks an assumption is reported as the
    /// assumption it breaks. Every generic must keep its signature and keep
    /// its contract's promise, because sends from older frames reach this
    /// unit's methods. Methods and functions are not checked here: older
    /// frames reach them only by a retry, which the run checks then.
    pub fn check_replaces(&self, older: &Self) -> Result<(), CodeMismatch> {
        self.check_assumptions(older)?;
        if self.layout != older.layout || self.generics.len() != older.generics.len() {
            return Err(CodeMismatch::Layout);
        }
        for index in 0..self.generics.len() {
            let definition = Definition::Generic(GenericFunctionId(index_u32(index)));
            self.check_takes_over(&definition, older)?;
        }
        Ok(())
    }

    /// Checks that every closed-world assumption `older`'s code relies on
    /// holds in this unit's layout: each sealed class it read is still
    /// sealed with the same direct subclasses and abstractness, and each
    /// generic whose method set it read has the same methods (and is still
    /// sealed, if it was). Assumptions are compared by source name, so a
    /// layout that numbers classes differently is judged by what it means.
    ///
    /// Code that relies on nothing, such as code lowered without typing
    /// facts, accepts any closed world; the layout rule in
    /// [`Self::check_replaces`] still applies.
    pub fn check_assumptions(&self, older: &Self) -> Result<(), CodeMismatch> {
        for assumption in &older.assumptions {
            let now = self.layout.closed_world_like(assumption);
            if !now.as_ref().is_some_and(|now| keeps(now, assumption)) {
                return Err(CodeMismatch::Assumption(Box::new(AssumptionMismatch {
                    assumption: assumption.clone(),
                    now,
                })));
            }
        }
        Ok(())
    }

    /// Checks that this unit's `definition` can be called by code compiled
    /// against `older`'s: the same signature and a contract that keeps the
    /// older one's promise. Both units must record a contract for it.
    pub fn check_takes_over(
        &self,
        definition: &Definition,
        older: &Self,
    ) -> Result<(), CodeMismatch> {
        let name = || self.definition_name(definition);
        let (Some(new_func), Some(old_func)) =
            (self.function(definition), older.function(definition))
        else {
            return Err(CodeMismatch::Layout);
        };
        if self.signature(new_func) != older.signature(old_func) {
            return Err(CodeMismatch::Signature {
                definition: definition.clone(),
                name: name(),
            });
        }
        let (Some(new), Some(old)) = (
            self.contracts.get(definition),
            older.contracts.get(definition),
        ) else {
            // Code compiled against `older` may rely on more than the tape
            // signature, and nothing says what.
            return Err(CodeMismatch::Uncontracted {
                definition: definition.clone(),
                name: name(),
            });
        };
        new.check_replaces(old, &self.layout).map_err(|change| {
            let types = match &change {
                ContractBreak::Arity => (String::new(), String::new()),
                ContractBreak::Value { old, new, .. } => {
                    (self.value_name(old), self.value_name(new))
                }
            };
            CodeMismatch::Contract(Box::new(ContractMismatch {
                definition: definition.clone(),
                name: name(),
                change,
                types,
            }))
        })
    }

    /// The source name of `definition`: a generic's or function's name, or
    /// a method's tape function name such as `area#method0`.
    #[must_use]
    pub fn definition_name(&self, definition: &Definition) -> String {
        let program_name = || {
            self.function(definition)
                .and_then(|func| self.program.program().function_name(func))
                .map(String::from)
        };
        match definition {
            Definition::Generic(generic) => self
                .layout
                .generic(*generic)
                .map(|generic| generic.name.clone())
                .unwrap_or_else(|| alloc::format!("generic#{}", generic.0)),
            Definition::Method(method) => {
                program_name().unwrap_or_else(|| alloc::format!("method#{}", method.0))
            }
            Definition::Function(name) => name.clone(),
        }
    }

    /// The declared result types of the generic named `generic`, written
    /// with this unit's class names, such as `<circle>` or
    /// `limited(<integer>, min: 0, max: 9)`: what a `use-value` standing
    /// in for the results of a send to it that found no applicable method
    /// must supply, since its dispatcher checks each one against them.
    ///
    /// `None` when the unit has no such generic or records no contract for
    /// it.
    #[must_use]
    pub fn generic_result_types(&self, generic: &str) -> Option<Vec<String>> {
        let index = self
            .layout
            .generics
            .iter()
            .position(|candidate| candidate.name == generic)?;
        let contract = self.contracts.generics.get(index)?;
        Some(
            contract
                .results
                .iter()
                .map(|result| self.value_name(result))
                .collect(),
        )
    }

    fn value_name(&self, value: &ValueContract) -> String {
        alloc::format!("{}", value.display(&self.layout))
    }

    /// Whether `func` here and `other_func` in `other` have the same
    /// signature and bytecode. Unchanged definitions recompile identically
    /// unless an edit renumbered what their code refers to.
    pub(crate) fn same_code(&self, func: FuncId, other: &Self, other_func: FuncId) -> bool {
        self.signature(func) == other.signature(other_func)
            && self.bytecode(func).is_some()
            && self.bytecode(func) == other.bytecode(other_func)
    }

    fn bytecode(&self, func: FuncId) -> Option<&[u8]> {
        Some(self.program.function(func)?.bytecode())
    }

    /// Argument and result kinds of `func`.
    pub(crate) fn signature(&self, func: FuncId) -> Option<Signature> {
        let function = self.program.function(func)?;
        Some((
            function.arg_types().iter().cloned().collect(),
            function.ret_types().iter().cloned().collect(),
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
    /// Why the frame cannot be retried with `current`, when a newer unit
    /// changed the definition's signature or broke its contract: the retry
    /// would pass the frame's arguments to the new code and return its
    /// results to the frame's caller, both compiled against the old one.
    pub retry_refusal: Option<CodeMismatch>,
}

impl FrameCode {
    /// Whether the frame can be retried with its current definition.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.current.is_some()
            && self.retry_refusal.is_none()
            && self
                .definition
                .as_ref()
                .is_some_and(Definition::is_retryable)
    }
}

/// Code units installed in one VM, oldest first: the units of one run, or
/// every unit installed through one [`crate::driver::Installed`].
#[derive(Clone, Debug, Default)]
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

    pub(crate) fn newest_unit(&self) -> Option<UnitId> {
        self.units.last().map(|(unit, _)| *unit)
    }

    /// Every unit, oldest first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (UnitId, &Arc<CodeUnit>)> {
        self.units.iter().map(|(unit, code)| (*unit, code))
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
                .function_name(func.func)
                .map(String::from)
        });
        let retry_refusal = match (&definition, current) {
            (Some(definition), Some(current)) if current != func && definition.is_retryable() => {
                match (self.get(current.unit), self.get(func.unit)) {
                    (Some(new), Some(old)) => new.check_takes_over(definition, old).err(),
                    _ => Some(CodeMismatch::Layout),
                }
            }
            _ => None,
        };
        FrameCode {
            func,
            name,
            definition,
            current,
            superseded,
            retry_refusal,
        }
    }
}

/// Whether `now` keeps the promise of `assumed`, the same class or
/// generic: the same direct subclasses or methods, whatever their order,
/// the same abstractness, and still sealed where `assumed` is.
fn keeps(now: &ClosedWorld, assumed: &ClosedWorld) -> bool {
    let same = |left: &[String], right: &[String]| {
        let mut left = left.to_vec();
        let mut right = right.to_vec();
        left.sort();
        right.sort();
        left == right
    };
    match (now, assumed) {
        (
            ClosedWorld::SealedClass {
                class,
                subclasses,
                is_abstract,
            },
            ClosedWorld::SealedClass {
                class: assumed_class,
                subclasses: assumed_subclasses,
                is_abstract: assumed_abstract,
            },
        ) => {
            class == assumed_class
                && is_abstract == assumed_abstract
                && same(subclasses, assumed_subclasses)
        }
        (
            ClosedWorld::Methods {
                generic,
                sealing,
                methods,
            },
            ClosedWorld::Methods {
                generic: assumed_generic,
                sealing: assumed_sealing,
                methods: assumed_methods,
            },
        ) => {
            generic == assumed_generic
                && (sealing.is_sealed() || !assumed_sealing.is_sealed())
                && same(methods, assumed_methods)
        }
        _ => false,
    }
}

fn index_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(method: u32, specializers: &[Option<u32>]) -> DispatchCase {
        DispatchCase {
            method: MethodId(method),
            specializers: specializers
                .iter()
                .map(|class| class.map(ClassId))
                .collect(),
            direct: true,
        }
    }

    #[test]
    fn an_assumption_is_kept_by_the_same_members_in_any_order() {
        use alloc::string::ToString;
        use interloko_model::sealing::Sealing;

        let names = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
        let shapes = |subclasses: &[&str], is_abstract| ClosedWorld::SealedClass {
            class: "<shape>".into(),
            subclasses: names(subclasses),
            is_abstract,
        };
        let assumed = shapes(&["<circle>", "<square>"], true);
        assert!(keeps(&shapes(&["<square>", "<circle>"], true), &assumed));
        assert!(!keeps(&shapes(&["<circle>"], true), &assumed));
        assert!(!keeps(&shapes(&["<circle>", "<square>"], false), &assumed));

        let area = |sealing, methods: &[&str]| ClosedWorld::Methods {
            generic: "area".into(),
            sealing,
            methods: names(methods),
        };
        let open = area(Sealing::Open, &["area(<circle>)"]);
        let sealed = area(Sealing::Sealed, &["area(<circle>)"]);
        // Sealing an open generic keeps what code assumed of it; unsealing
        // a sealed one does not.
        assert!(keeps(&sealed, &open));
        assert!(!keeps(&open, &sealed));
        assert!(!keeps(&area(Sealing::Open, &[]), &open));
        assert!(!keeps(&open, &assumed));
    }

    #[test]
    fn selection_takes_the_first_case_whose_specializers_all_accept() {
        // Classes 2 and 3 are subclasses of 1; class 4 is unrelated.
        let is_subclass =
            |class: ClassId, of: ClassId| class == of || (of == ClassId(1) && class.0 <= 3);
        let plan = GenericDispatch {
            cases: alloc::vec![
                case(0, &[Some(2), Some(3)]),
                case(1, &[Some(2), None]),
                case(2, &[Some(1), Some(1)]),
            ],
            positions: alloc::vec![0, 1],
        };
        let select = |classes: [u32; 2]| {
            plan.select(
                |index| classes.get(index).copied().map(ClassId),
                is_subclass,
            )
            .map(|case| case.method.0)
        };
        assert_eq!(select([2, 3]), Some(0));
        assert_eq!(select([2, 2]), Some(1));
        assert_eq!(select([3, 2]), Some(2));
        assert_eq!(
            select([4, 2]),
            None,
            "no case applies: the dispatcher decides"
        );
    }
}
