//! The one way to run Interloko tape.
//!
//! [`run`] starts an `execution_tape` invocation that yields its host
//! requests and its unhandled suspensions, and services both. Control
//! requests (handler and exit bindings, handler searches) are language policy
//! and are handled here, where the VM's frames are visible: a signal searches
//! the dynamic bindings on the signalling stack before anything unwinds, and
//! guards run there as ordinary guest calls. Every other request dispatches to
//! the embedding's [`Host`] unchanged.
//!
//! A condition no handler accepts suspends on its condition tag. No guest
//! code handles those tags, so the suspension reaches the driver with every
//! frame still live, and the driver answers it with the chooser's choice:
//! resuming the signal with values, or raising a restart, retry or abort at
//! it so that every crossed cleanup runs.
//!
//! A run links [`CodeUnit`]s: it starts from an entry unit installed in the
//! VM, either by [`run`] or once by [`install`] for any number of
//! [`run_installed`] calls, and a chooser may install replacements while a
//! condition is unhandled. Generic sends ask
//! the driver which method to call ([`crate::code::CODE_DISPATCH`]) or, when
//! typing selected the method, call it through a link that every install
//! repoints ([`crate::code::CodeUnit::link_target`]),
//! and a retried frame calls the newest version of its function, while every
//! frame keeps running the unit it entered.
//!
//! Code older than the newest unit may reach it only where the newest unit
//! keeps what that code relies on ([`crate::code::CodeUnit::check_replaces`]):
//! a chooser's install is checked against every unit whose code may still
//! run, and so is a run's entry unit against those that earlier runs of the
//! same [`Installed`] code put in place, since values those runs made may
//! still call their code.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use execution_tape::host::{Host, HostError, ValueRef};
use execution_tape::program::LinkId;
use execution_tape::program::ValueType;
use execution_tape::value::{FuncId, FuncRef, Obj, TagRef, UnitId, Value};
use execution_tape::vm::{
    ActivationId, InstallError, InvocationInspection, InvocationOptions, PendingHostCall,
    PendingSuspension, RelinkError, RunStatus, TagError, Trap, TrapInfo, Units, Vm,
};
use interloko_model::{ClassId, GenericFunctionId};

use crate::InterlokoHost;
use crate::chooser::{
    Choice, ConditionChooser, FrameSnapshot, LinkSnapshot, OfferedRestart, RegisterSnapshot,
    UnhandledCondition,
};
use crate::code::{CODE_DISPATCH, CodeMismatch, CodeUnit, Definition, UnitTable, method_link};
use crate::control::{
    ABORT_TAG, BindingKind, BindingToken, CHECK_FAILED_TAG, CONDITION_GUARD,
    CONDITION_GUARD_RESULT, CONDITION_GUARD_UNWOUND, CONDITION_NO_APPLICABLE_METHOD,
    CONDITION_PUSH_HANDLER, CONDITION_SIGNAL, CONDITION_TYPE_ERROR, CONTROL_CHECK_EXIT,
    CONTROL_POP, CONTROL_PUSH_EXIT, ControlFault, ControlState, ControlStats, RESTART_TAG,
    RETRY_TAG, RestartDescriptor, is_control_symbol, restart_payload, retry_payload,
};

/// A [`Host`] backed by an [`InterlokoHost`], which the driver consults for
/// condition classes and the class hierarchy.
pub trait RuntimeHost: Host {
    /// Runs `f` with the Interloko runtime behind this host.
    fn with_runtime<R>(&mut self, f: impl FnOnce(&mut InterlokoHost) -> R) -> R;
}

impl RuntimeHost for InterlokoHost {
    fn with_runtime<R>(&mut self, f: impl FnOnce(&mut InterlokoHost) -> R) -> R {
        f(self)
    }
}

/// A completed run.
#[derive(Debug)]
pub struct RunOutcome {
    /// Values returned by the entry function. Aggregate handles belong to the
    /// VM that ran the invocation.
    pub outputs: Vec<Value>,
    /// Control work performed by the run.
    pub stats: ControlStats,
}

/// Why a run did not complete.
#[derive(Debug)]
pub enum RunFailure {
    /// The VM trapped, or the host failed a request.
    Trap(Box<TrapInfo>),
    /// The language runtime ended the run.
    Control {
        /// The language-level fault.
        fault: ControlFault,
        /// The trap that ended the VM invocation.
        trap: Box<TrapInfo>,
    },
    /// A chooser aborted the run; every crossed cleanup ran.
    Aborted {
        /// The uncaught abort that ended the VM invocation.
        trap: Box<TrapInfo>,
    },
    /// The entry unit could not be linked into the VM; nothing ran.
    Link(LinkError),
    /// The VM paused with a run status this driver does not service, so the
    /// invocation was cancelled and rolled back.
    UnsupportedStatus {
        /// The status's `Debug` text.
        status: String,
    },
}

impl RunFailure {
    /// The trap that ended the VM invocation, if one started.
    #[must_use]
    pub const fn trap(&self) -> Option<&TrapInfo> {
        match self {
            Self::Trap(trap) | Self::Control { trap, .. } | Self::Aborted { trap } => Some(trap),
            Self::Link(_) | Self::UnsupportedStatus { .. } => None,
        }
    }
}

impl core::fmt::Display for RunFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Trap(trap) => write!(formatter, "{trap}"),
            Self::Control { fault, trap } => write!(formatter, "{fault:?}: {trap}"),
            Self::Aborted { .. } => formatter.write_str("aborted by the condition chooser"),
            Self::Link(error) => write!(formatter, "{error}"),
            Self::UnsupportedStatus { status } => {
                write!(
                    formatter,
                    "the VM paused with unsupported run status {status}"
                )
            }
        }
    }
}

/// Why a code unit could not be linked into a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkError {
    /// The VM already defines a control tag with a different payload.
    Tag(TagError),
    /// The VM refused the program.
    Install(InstallError),
    /// The unit cannot replace the code the run already installed.
    Mismatch(CodeMismatch),
    /// A run's entry unit cannot take over from a unit an earlier run of
    /// the same [`Installed`] code installed, whose code values from that
    /// run, such as returned closures, may still call; nothing ran.
    ///
    /// Such a VM can no longer run its installed code: the values must be
    /// dropped with it, and the code installed in a new VM.
    EarlierCode {
        /// The unit the earlier run installed.
        unit: UnitId,
        /// What the entry unit would break.
        mismatch: CodeMismatch,
    },
    /// The VM asked to run an [`Installed`] unit did not install it.
    NotInstalled,
    /// The VM refused to repoint a linked send's link at the newest code.
    ///
    /// Units that took over from one another always accept each other's
    /// targets, so this reports a broken invariant: links repointed before
    /// the refusal stay repointed, and a refused install does not take over.
    Relink(RelinkError),
}

impl core::fmt::Display for LinkError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Tag(error) => write!(formatter, "control tags: {error:?}"),
            Self::Install(error) => write!(formatter, "install refused: {error}"),
            Self::Mismatch(mismatch) => {
                write!(formatter, "cannot replace running code: {mismatch}")
            }
            Self::EarlierCode { mismatch, .. } => write!(
                formatter,
                "cannot run over code an earlier run installed, which values from that run \
                 may still call: {mismatch}"
            ),
            Self::NotInstalled => formatter.write_str("the code is not installed in this VM"),
            Self::Relink(error) => write!(formatter, "cannot repoint a linked send: {error}"),
        }
    }
}

/// A code unit installed in one VM, to run there any number of times with
/// [`run_installed`].
///
/// It also remembers the units choosers install during its runs, for the
/// VM's lifetime: their linked sends must keep following the code each run
/// selects from, even when a value made by such a unit is called in a later
/// run, and each run's entry must keep the promises their code relies on.
#[derive(Clone, Debug)]
pub struct Installed {
    unit: UnitId,
    code: Arc<CodeUnit>,
    /// This unit and every unit installed during its runs, oldest first.
    linked: UnitTable,
}

impl Installed {
    /// The VM unit the code was installed as.
    #[must_use]
    pub const fn unit(&self) -> UnitId {
        self.unit
    }

    /// The installed code.
    #[must_use]
    pub const fn code(&self) -> &Arc<CodeUnit> {
        &self.code
    }
}

/// Points every link the sends of `units` use where a dynamic send would go
/// with `newest`, installed as `unit`, as a run's newest code: see
/// [`CodeUnit::link_target`].
///
/// The VM accepts every target. `newest` took over from every unit
/// ([`CodeUnit::check_replaces`]), so they all share the layout and each
/// generic's tape signature; a link's signature is its method's, which a
/// unit only calls directly when it is its generic's; and the target is the
/// newest unit's version of the method only when that has the generic's
/// signature too, and its dispatcher otherwise. A refusal or a missing method
/// therefore breaks that invariant, and is reported without undoing the
/// links already repointed.
fn relink_all<'u>(
    units: impl IntoIterator<Item = (UnitId, &'u CodeUnit)>,
    unit: UnitId,
    newest: &CodeUnit,
    mut relink: impl FnMut(UnitId, LinkId, FuncRef) -> Result<FuncRef, RelinkError>,
) -> Result<(), LinkError> {
    for (owner, code) in units {
        for method in code.linked_methods() {
            let target = newest
                .link_target(unit, method)
                .ok_or(LinkError::Mismatch(CodeMismatch::Layout))?;
            let relinked = relink(owner, method_link(method), target);
            debug_assert!(
                relinked.is_ok(),
                "units that took over from each other accept each other's targets: {relinked:?}"
            );
            relinked.map_err(LinkError::Relink)?;
        }
    }
    Ok(())
}

/// Defines the control tags `code` imports, unless `vm` already has them,
/// and installs `code` as a new VM unit.
///
/// Installing is explicit and never cached: every call adds a unit, which
/// the VM keeps until it is dropped.
pub fn install<H: Host>(vm: &mut Vm<H>, code: &Arc<CodeUnit>) -> Result<Installed, LinkError> {
    define_tags(vm)?;
    let unit = vm
        .install(Arc::clone(&code.program))
        .map_err(LinkError::Install)?;
    let mut linked = UnitTable::default();
    linked.push(unit, Arc::clone(code));
    Ok(Installed {
        unit,
        code: Arc::clone(code),
        linked,
    })
}

/// Installs `unit` in `vm` and runs its function `entry` with `args` to
/// completion, servicing host requests.
///
/// This is [`install`] followed by [`run_installed`], so every run adds one
/// unit to `vm`.
pub fn run<'a, H: RuntimeHost>(
    vm: &'a mut Vm<H>,
    unit: &Arc<CodeUnit>,
    entry: FuncId,
    args: &'a [Value],
    options: InvocationOptions<'a>,
    chooser: &mut dyn ConditionChooser,
) -> Result<RunOutcome, RunFailure> {
    let mut installed = install(vm, unit).map_err(RunFailure::Link)?;
    run_installed(vm, &mut installed, entry, args, options, chooser)
}

/// Runs function `entry` of code that [`install`] installed in `vm`, with
/// `args`, to completion, servicing host requests.
///
/// `options` may carry trace and access observers; the driver opts into
/// yielded host calls and suspensions itself. Hard VM limits apply to the
/// whole run. Nothing of the run's control state outlives it: handler and
/// exit bindings, handler searches and selected sends start empty every run.
///
/// A condition that no live handler accepts goes to `chooser` before anything
/// unwinds. Code it installs stays in `vm` after the run, and `installed`
/// remembers it: every run starts by pointing the linked sends of all those
/// units back at the entry unit's methods, which is where their dynamic
/// sends go too, since a run selects from its entry unit and what it
/// installs. Values a run made, such as returned closures, may run such a
/// unit's code in a later run, so the entry unit must keep every promise
/// that code relies on, as a chooser's install must
/// ([`CodeUnit::check_replaces`]); otherwise the run is refused with
/// [`LinkError::EarlierCode`] before anything runs. A frame of such code
/// may be retried like any other.
pub fn run_installed<'a, H: RuntimeHost>(
    vm: &'a mut Vm<H>,
    installed: &mut Installed,
    entry: FuncId,
    args: &'a [Value],
    options: InvocationOptions<'a>,
    chooser: &mut dyn ConditionChooser,
) -> Result<RunOutcome, RunFailure> {
    let unit = installed.unit;
    if !vm
        .units()
        .program(unit)
        .is_some_and(|program| Arc::ptr_eq(program, &installed.code.program))
    {
        return Err(RunFailure::Link(LinkError::NotInstalled));
    }
    // Refusing is the only sound answer to a broken promise: leaving the
    // older units' links on their own code would still send their dynamic
    // sends to the entry unit's methods, and quietly run stale code.
    installed
        .linked
        .check_takes_over(&installed.code, Some(unit))
        .map_err(|(unit, mismatch)| RunFailure::Link(LinkError::EarlierCode { unit, mismatch }))?;
    let tags = define_tags(vm).map_err(RunFailure::Link)?;
    // A run starts from its entry unit, so every linked send, including
    // those of units an earlier run installed, follows it; their frames
    // resolve against it as superseded code.
    let mut units = UnitTable::default();
    for (owner, code) in installed.linked.iter().filter(|(owner, _)| *owner != unit) {
        units.push(owner, Arc::clone(code));
    }
    relink_all(
        installed
            .linked
            .iter()
            .map(|(owner, code)| (owner, &**code)),
        unit,
        &installed.code,
        |owner, link, target| vm.relink(owner, link, target),
    )
    .map_err(RunFailure::Link)?;
    units.push(unit, Arc::clone(&installed.code));
    let mut invocation = vm
        .start(
            FuncRef::new(unit, entry),
            args,
            options.yield_host_calls().yield_suspensions(),
        )
        .map_err(|trap| RunFailure::Trap(Box::new(trap)))?;
    let mut driver = Driver {
        units,
        linked: &mut installed.linked,
        sends: BTreeMap::new(),
        tags,
        state: ControlState::default(),
        searches: Vec::new(),
        made_check_error: None,
        unhandled_check: None,
        fault: None,
    };
    loop {
        let status = match invocation.resume(u64::MAX) {
            Ok(status) => status,
            Err(trap) => return Err(driver.failure(trap)),
        };
        let serviced = match status {
            RunStatus::Complete => {
                return Ok(RunOutcome {
                    outputs: invocation.take_outputs().unwrap_or_default(),
                    stats: driver.state.stats(),
                });
            }
            RunStatus::Suspended => Ok(()),
            RunStatus::AwaitingHost | RunStatus::CallbackReturned => invocation
                .pending_host_call()
                .map_or(Ok(()), |pending| driver.service(pending, status)),
            RunStatus::AwaitingHandler => invocation
                .pending_suspension()
                .map_or(Ok(()), |suspension| driver.answer(suspension, chooser)),
            // A status from a newer VM that this driver cannot service.
            // Returning drops the invocation, which cancels and rolls it back.
            status => {
                return Err(RunFailure::UnsupportedStatus {
                    status: format!("{status:?}"),
                });
            }
        };
        if let Err(trap) = serviced {
            return Err(driver.failure(trap));
        }
    }
}

/// Defines the control tags units import, or finds them when the VM already
/// has them.
fn define_tags<H: Host>(vm: &mut Vm<H>) -> Result<VmTags, LinkError> {
    let mut define = |name: &str, payload: &[ValueType]| match vm.units().defined_tag(name) {
        Some(tag) => Ok(tag),
        None => vm.define_tag(name, payload).map_err(LinkError::Tag),
    };
    // Only checking host calls raise it, and only the guest catches it.
    define(CHECK_FAILED_TAG, &[])?;
    Ok(VmTags {
        restart: define(RESTART_TAG, &restart_payload())?,
        abort: define(ABORT_TAG, &[])?,
        retry: define(RETRY_TAG, &retry_payload())?,
    })
}

/// Copies the live frames and their initialized registers, innermost first.
/// Every link the sends of `units` use, with its current target.
///
/// Each unit was installed in this VM, and its program declares a link per
/// method of its layout.
fn snapshot_links(vm: Units<'_>, units: &UnitTable) -> Vec<LinkSnapshot> {
    let mut snapshots = Vec::new();
    for (unit, code) in units.iter() {
        let targets = vm
            .links(unit)
            .expect("linked units are installed in this VM");
        for method in code.linked_methods() {
            let link = method_link(method);
            let target = targets[link.0 as usize];
            let name = code
                .program
                .program()
                .link_name(link)
                .expect("a program names the link of each method");
            snapshots.push(LinkSnapshot {
                unit,
                link,
                name: String::from(name),
                method,
                target,
                definition: units
                    .get(target.unit)
                    .and_then(|code| code.definition(target.func)),
            });
        }
    }
    snapshots
}

fn snapshot_frames(
    mut inspection: InvocationInspection<'_>,
    units: &UnitTable,
) -> Vec<FrameSnapshot> {
    let frames = inspection.frames().collect::<Vec<_>>();
    frames
        .into_iter()
        .map(|frame| {
            let registers = inspection
                .registers(&frame.activation)
                .map(|registers| {
                    registers
                        .filter_map(|register| {
                            register.value.map(|value| RegisterSnapshot {
                                register: register.register,
                                value: value.to_value(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let code = units.frame(FuncRef::new(frame.location.unit, frame.location.func));
            FrameSnapshot {
                frame,
                registers,
                code,
            }
        })
        .collect()
}

struct Driver<'a> {
    /// The units whose code the run may run, as [`Self::linked`] has them
    /// but with the entry unit after those earlier runs installed, so the
    /// newest is the one the run selects from.
    units: UnitTable,
    /// Every unit installed through the run's [`Installed`], this run's
    /// included, whose linked sends follow the run's newest code.
    linked: &'a mut UnitTable,
    /// Functions sends selected, by generic and the classes of the argument
    /// positions its methods specialize. Cleared by every install.
    sends: BTreeMap<SendKey, FuncRef>,
    tags: VmTags,
    state: ControlState,
    searches: Vec<Search>,
    /// The `<type-error>` or `<no-applicable-method>` the last request
    /// made for a failed check or send, which the code that asked signals
    /// next.
    made_check_error: Option<Obj>,
    /// The condition of a failed check's signal that found no handler, which
    /// suspends next.
    unhandled_check: Option<Obj>,
    fault: Option<ControlFault>,
}

/// One signal's handler search, alive from its `signal` request until it
/// selects a handler, finds none, or its guard exits by an exception. Each of
/// those is a request from the signalling activation, so the search never
/// outlives what it describes.
///
/// While a guard runs, the bindings from the guarded one up to those live
/// when this search began are masked: a signal from inside the guard sees
/// only outer bindings and bindings the guard itself establishes.
struct Search {
    /// Activation that issued the `signal` and makes the search's requests.
    activation: ActivationId,
    condition: Obj,
    class: ClassId,
    /// Bindings below this index are still candidates.
    next: usize,
    /// Bindings live when the search began.
    stack_len: usize,
    /// Binding whose guard is running on the signalling stack.
    guarding: Option<usize>,
    /// Whether a failed runtime check or send signalled the condition,
    /// rather than the program.
    failed_check: bool,
}

/// The classes that select a send, for up to [`SendKey::POSITIONS`]
/// specialized positions; wider sends are not cached.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SendKey {
    generic: u32,
    classes: [u32; SendKey::POSITIONS],
}

impl SendKey {
    const POSITIONS: usize = 4;

    fn new(
        generic: GenericFunctionId,
        classes: impl ExactSizeIterator<Item = Option<ClassId>>,
    ) -> Option<Self> {
        if classes.len() > Self::POSITIONS {
            return None;
        }
        let mut key = [u32::MAX; Self::POSITIONS];
        for (slot, class) in key.iter_mut().zip(classes) {
            *slot = class?.0;
        }
        Some(Self {
            generic: generic.0,
            classes: key,
        })
    }
}

/// A suspended signal that no handler accepted.
#[derive(Copy, Clone)]
struct Signalled {
    condition: Obj,
    class: ClassId,
    /// Whether a failed runtime check or send signalled it.
    failed_check: bool,
}

/// Control tags as the VM raises and compares them.
#[derive(Copy, Clone)]
struct VmTags {
    restart: TagRef,
    abort: TagRef,
    retry: TagRef,
}

/// How to finish a pending request or answer a suspended signal.
enum Action {
    /// Return these values where the guest waits.
    Complete(Vec<Value>),
    /// Raise this exception where the guest waits.
    Throw(TagRef, Vec<Value>),
    /// End the run with this fault.
    Fail(ControlFault),
}

impl Driver<'_> {
    fn failure(&self, trap: TrapInfo) -> RunFailure {
        if let Trap::Uncaught(exception) = &trap.trap
            && exception.tag() == self.tags.abort
        {
            return RunFailure::Aborted {
                trap: Box::new(trap),
            };
        }
        match self.fault {
            Some(fault) => RunFailure::Control {
                fault,
                trap: Box::new(trap),
            },
            None => RunFailure::Trap(Box::new(trap)),
        }
    }

    /// Services a waiting host request.
    ///
    /// The driver never calls back into the guest, so a returned callback
    /// is a protocol fault.
    fn service<H: RuntimeHost>(
        &mut self,
        mut pending: PendingHostCall<'_, '_, H>,
        status: RunStatus,
    ) -> Result<(), TrapInfo> {
        let action = if status == RunStatus::CallbackReturned {
            Action::Fail(ControlFault::MalformedRequest)
        } else if pending.symbol() == CODE_DISPATCH {
            return self.dispatch(pending);
        } else if !is_control_symbol(pending.symbol()) {
            return pending.dispatch();
        } else {
            self.decide(&mut pending)
        };
        match action {
            Action::Complete(mut values) => pending.complete(&mut values, Ok(0)),
            Action::Throw(tag, payload) => pending.throw(tag, payload),
            Action::Fail(fault) => {
                self.fault.get_or_insert(fault);
                pending.complete(&mut [], Err(HostError::Failed))
            }
        }
    }

    /// Answers a suspension: a signal that no handler accepted waits on its
    /// condition tag, and the chooser decides how it continues.
    fn answer<H: RuntimeHost>(
        &mut self,
        mut suspension: PendingSuspension<'_, '_, H>,
        chooser: &mut dyn ConditionChooser,
    ) -> Result<(), TrapInfo> {
        let action = match self.suspended_condition(&mut suspension) {
            Some((condition, class)) => {
                self.state.note_unhandled();
                let resume_types = suspension.result_types().to_vec();
                // The guest suspends right after its search finds no
                // handler, so this is that search's signal.
                let failed_check = self.unhandled_check.take() == Some(condition);
                let signal = Signalled {
                    condition,
                    class,
                    failed_check,
                };
                self.unhandled(&mut suspension, signal, &resume_types, chooser)
            }
            None => Action::Fail(ControlFault::MalformedRequest),
        };
        match action {
            Action::Complete(values) => suspension.resume(values),
            Action::Throw(tag, payload) => suspension.resume_throw(tag, payload),
            Action::Fail(fault) => {
                self.fault.get_or_insert(fault);
                Err(suspension.discard())
            }
        }
    }

    /// The condition and its class, when `suspension` waits on one of its
    /// unit's condition tags with a condition object.
    fn suspended_condition<H: RuntimeHost>(
        &self,
        suspension: &mut PendingSuspension<'_, '_, H>,
    ) -> Option<(Obj, ClassId)> {
        let code = self.units.get(suspension.location().unit)?;
        if !code.control.conditions.contains(&suspension.local_tag()) {
            return None;
        }
        let payload = suspension
            .payload()
            .map(|value| value.to_value())
            .collect::<Vec<_>>();
        let [Value::Obj(condition)] = payload.as_slice() else {
            return None;
        };
        let condition = *condition;
        let class = suspension
            .host_mut()
            .with_runtime(|runtime| runtime.object(condition).ok().map(|object| object.class))?;
        Some((condition, class))
    }

    /// Answers a generic send with the function its arguments select from
    /// the newest unit: a method, or the generic's dispatcher when the
    /// unit's dispatch table cannot decide or selects nothing, so the
    /// dispatcher's own checks apply and a send that finds no method
    /// signals its `<no-applicable-method>` there.
    ///
    /// Only code the run's [`Installed`] knows may send: nothing compared
    /// another install's code with the newest unit.
    fn dispatch<H: RuntimeHost>(
        &mut self,
        mut pending: PendingHostCall<'_, '_, H>,
    ) -> Result<(), TrapInfo> {
        let sender = pending.unit();
        if self.units.get(sender).is_none() {
            self.fault
                .get_or_insert(ControlFault::ForeignCode { unit: sender });
            return pending.complete(&mut [], Err(HostError::Failed));
        }
        let args = pending
            .arguments()
            .map(|value| value.to_value())
            .collect::<Vec<_>>();
        let generic = match args.first() {
            Some(Value::U64(generic)) => u32::try_from(*generic).ok().map(GenericFunctionId),
            _ => None,
        };
        let target = generic.and_then(|generic| {
            self.select(&mut pending, generic, &args[1..])
                .or_else(|| self.units.current(&Definition::Generic(generic)))
        });
        match target {
            Some(target) => pending.complete(&mut [Value::Func(target)], Ok(0)),
            None => {
                self.fault.get_or_insert(ControlFault::MalformedRequest);
                pending.complete(&mut [], Err(HostError::Failed))
            }
        }
    }

    /// Selects a directly callable method of the newest unit, consulting and
    /// filling the dispatch cache.
    fn select<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingHostCall<'_, '_, H>,
        generic: GenericFunctionId,
        args: &[Value],
    ) -> Option<FuncRef> {
        let unit = self.units.newest_unit()?;
        let code = Arc::clone(self.units.newest()?);
        let plan = code.dispatch.get(generic.0 as usize)?.as_ref()?;
        let mut classes = alloc::vec![None; args.len()];
        let found = pending.host_mut().with_runtime(|runtime| {
            for &position in &plan.positions {
                let value = args.get(position)?;
                classes[position] = Some(runtime.class_of_any(ValueRef::from_value(value)).ok()?);
            }
            Some(())
        });
        found?;
        let key = SendKey::new(generic, plan.positions.iter().map(|&index| classes[index]));
        if let Some(target) = key.and_then(|key| self.sends.get(&key)) {
            self.state.note_send(true);
            return Some(*target);
        }
        self.state.note_send(false);
        let case = pending.host_mut().with_runtime(|runtime| {
            plan.select(
                |index| classes.get(index).copied().flatten(),
                |class, specializer| runtime.layout().is_same_or_subclass(class, specializer),
            )
            .cloned()
        })?;
        let method = code.methods.get(case.method.0 as usize)?;
        let target = case.direct.then(|| FuncRef::new(unit, *method))?;
        if let Some(key) = key {
            self.sends.insert(key, target);
        }
        Some(target)
    }

    /// Decides a control request while it waits, with the requesting
    /// frames and the host both reachable through `pending`.
    fn decide<H: RuntimeHost>(&mut self, pending: &mut PendingHostCall<'_, '_, H>) -> Action {
        let Some(activation) = pending
            .inspect()
            .frames()
            .next()
            .map(|frame| frame.activation)
        else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let symbol = pending.symbol();
        let args = pending
            .arguments()
            .map(|value| value.to_value())
            .collect::<Vec<_>>();
        match (symbol, args.as_slice()) {
            (CONDITION_PUSH_HANDLER, [Value::U64(descriptor), guard @ ..]) => {
                let unit = pending.unit();
                let Some(descriptor) = usize::try_from(*descriptor).ok().filter(|index| {
                    self.units
                        .get(unit)
                        .is_some_and(|code| *index < code.control.restarts.len())
                }) else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                let guard = match guard {
                    [] => None,
                    [guard @ Value::Closure(_)] => Some(guard.clone()),
                    _ => return Action::Fail(ControlFault::MalformedRequest),
                };
                let token = self.state.push(
                    BindingKind::Handler {
                        unit,
                        descriptor,
                        guard,
                    },
                    activation,
                );
                Action::Complete(alloc::vec![Value::U64(token.0)])
            }
            (CONTROL_PUSH_EXIT, []) => {
                let token = self.state.push(BindingKind::Exit, activation);
                Action::Complete(alloc::vec![Value::U64(token.0)])
            }
            (CONTROL_POP, [Value::U64(token)]) => match self.state.pop(BindingToken(*token)) {
                Ok(()) => Action::Complete(Vec::new()),
                Err(fault) => Action::Fail(fault),
            },
            (CONTROL_CHECK_EXIT, [Value::U64(token)]) => {
                let token = BindingToken(*token);
                if self.state.exit_is_live(token) {
                    Action::Complete(Vec::new())
                } else {
                    Action::Fail(ControlFault::ExpiredExit { token })
                }
            }
            (CONDITION_SIGNAL, [Value::Obj(condition)]) => {
                self.state.note_signal();
                let condition = *condition;
                let class = pending.host_mut().with_runtime(|runtime| {
                    runtime.object(condition).ok().map(|object| object.class)
                });
                let Some(class) = class else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                let live = self.state.bindings().len();
                self.searches.push(Search {
                    activation,
                    condition,
                    class,
                    next: live,
                    stack_len: live,
                    guarding: None,
                    failed_check: self.made_check_error.take() == Some(condition),
                });
                self.step(pending)
            }
            (CONDITION_TYPE_ERROR, [values @ .., Value::U64(site)]) if !values.is_empty() => {
                let location = pending.location();
                let site = usize::try_from(*site).ok().and_then(|index| {
                    self.units
                        .get(location.unit)?
                        .control
                        .type_error(location.func, index)
                        .cloned()
                });
                let Some(site) = site else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                match pending
                    .host_mut()
                    .with_runtime(|runtime| runtime.make_type_error(&site, values))
                {
                    Ok(condition) => {
                        self.made_check_error = Some(condition);
                        Action::Complete(alloc::vec![Value::Obj(condition)])
                    }
                    Err(_) => Action::Fail(ControlFault::MalformedRequest),
                }
            }
            (CONDITION_NO_APPLICABLE_METHOD, [Value::U64(generic), arguments @ Value::Obj(_)]) => {
                let Some(generic) = u32::try_from(*generic).ok().map(GenericFunctionId) else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                match pending.host_mut().with_runtime(|runtime| {
                    runtime.make_no_applicable_method(generic, arguments.clone())
                }) {
                    Ok(condition) => {
                        self.state.note_failed_send();
                        self.made_check_error = Some(condition);
                        Action::Complete(alloc::vec![Value::Obj(condition)])
                    }
                    Err(_) => Action::Fail(ControlFault::MalformedRequest),
                }
            }
            (CONDITION_GUARD, []) => match self.guarding(&activation) {
                Some(index) => match &self.state.bindings()[index].kind {
                    BindingKind::Handler {
                        guard: Some(guard), ..
                    } => Action::Complete(alloc::vec![guard.clone()]),
                    _ => Action::Fail(ControlFault::MalformedRequest),
                },
                None => Action::Fail(ControlFault::MalformedRequest),
            },
            (CONDITION_GUARD_UNWOUND, []) => {
                if self.guarding(&activation).is_none() {
                    return Action::Fail(ControlFault::MalformedRequest);
                }
                self.searches.pop();
                Action::Complete(Vec::new())
            }
            (CONDITION_GUARD_RESULT, [Value::Bool(accepted)]) => {
                let Some(index) = self.guarding(&activation) else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                if let Some(search) = self.searches.last_mut() {
                    search.guarding = None;
                }
                if *accepted {
                    self.accept(index)
                } else {
                    self.step(pending)
                }
            }
            _ => Action::Fail(ControlFault::MalformedRequest),
        }
    }

    /// The binding whose guard the innermost search waits on, when that
    /// search belongs to `activation`.
    fn guarding(&self, activation: &ActivationId) -> Option<usize> {
        self.searches
            .last()
            .filter(|search| search.activation == *activation)
            .and_then(|search| search.guarding)
    }

    /// Advances the innermost search to its next applicable handler, still
    /// on the signalling stack. An unguarded handler is taken at once; for a
    /// guarded one the request returns `#t`, and the guest runs the guard and
    /// reports its answer. With no handler left the request returns `#f`,
    /// and the guest suspends the signal for the chooser.
    fn step<H: RuntimeHost>(&mut self, pending: &mut PendingHostCall<'_, '_, H>) -> Action {
        let Some(search) = self.searches.last() else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let (class, next) = (search.class, search.next);
        let masked = self.masked();
        let applicable = pending
            .host_mut()
            .with_runtime(|runtime| self.applicable(runtime, class));
        for index in (0..next).rev() {
            self.state.note_searched();
            if masked[index] || !applicable[index] {
                continue;
            }
            let guard = match &self.state.bindings()[index].kind {
                BindingKind::Handler { guard, .. } => guard.as_ref(),
                BindingKind::Exit => None,
            };
            let Some(search) = self.searches.last_mut() else {
                return Action::Fail(ControlFault::MalformedRequest);
            };
            search.next = index;
            if guard.is_some() {
                search.guarding = Some(index);
                return Action::Complete(alloc::vec![Value::Bool(true)]);
            }
            return self.accept(index);
        }
        if let Some(search) = self.searches.pop() {
            self.unhandled_check = search.failed_check.then_some(search.condition);
        }
        Action::Complete(alloc::vec![Value::Bool(false)])
    }

    /// Takes the handler at `index` for the innermost search: its restart is
    /// raised at the signal.
    fn accept(&mut self, index: usize) -> Action {
        let Some(search) = self.searches.pop() else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let token = self.state.bindings()[index].token;
        Action::Throw(
            self.tags.restart,
            alloc::vec![Value::U64(token.0), Value::Obj(search.condition)],
        )
    }

    /// Bindings hidden from the innermost search by guards running in the
    /// searches that enclose it.
    fn masked(&self) -> Vec<bool> {
        let mut masked = alloc::vec![false; self.state.bindings().len()];
        let enclosing = self.searches.len().saturating_sub(1);
        for search in &self.searches[..enclosing] {
            if let Some(guarded) = search.guarding {
                for slot in masked.iter_mut().take(search.stack_len).skip(guarded) {
                    *slot = true;
                }
            }
        }
        masked
    }

    /// The restart site a handler binding established, from its unit.
    fn site(&self, kind: &BindingKind) -> Option<&RestartDescriptor> {
        match kind {
            BindingKind::Handler {
                unit, descriptor, ..
            } => self.units.get(*unit)?.control.restarts.get(*descriptor),
            BindingKind::Exit => None,
        }
    }

    /// Which live bindings, outermost first, are handlers accepting `class`.
    fn applicable(&self, runtime: &InterlokoHost, class: ClassId) -> Vec<bool> {
        self.state
            .bindings()
            .iter()
            .map(|binding| {
                self.site(&binding.kind)
                    .is_some_and(|site| runtime.layout.is_same_or_subclass(class, site.class))
            })
            .collect()
    }

    /// Hands an unhandled condition to the chooser while every frame is
    /// still live, then carries out its validated choice.
    ///
    /// A chooser may first install replacement code; the driver links it
    /// and asks again, with the frames showing what it superseded.
    fn unhandled<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingSuspension<'_, '_, H>,
        signal: Signalled,
        resume_types: &[ValueType],
        chooser: &mut dyn ConditionChooser,
    ) -> Action {
        let Signalled {
            condition,
            class,
            failed_check,
        } = signal;
        let mut refused = None;
        loop {
            let frames = snapshot_frames(pending.inspect(), &self.units);
            let links = snapshot_links(pending.units(), self.linked);
            let choice = {
                let mut restarts = Vec::new();
                for binding in self.state.bindings().iter().rev() {
                    if let Some(descriptor) = self.site(&binding.kind) {
                        restarts.push(OfferedRestart::Handler {
                            token: binding.token,
                            descriptor,
                            activation: &binding.activation,
                        });
                    }
                }
                restarts.push(OfferedRestart::UseValue {
                    types: resume_types,
                });
                restarts.push(OfferedRestart::Retry);
                restarts.push(OfferedRestart::Abort);
                pending.host_mut().with_runtime(|runtime| {
                    chooser.choose(&UnhandledCondition {
                        condition,
                        class,
                        failed_check,
                        runtime,
                        frames: &frames,
                        links: &links,
                        restarts: &restarts,
                        refused: refused.as_ref(),
                    })
                })
            };
            match choice {
                Choice::Install(code) => refused = self.install(pending, code).err(),
                Choice::Retry { frame } => return self.retry(&frames, &frame),
                Choice::Unhandled => {
                    return Action::Fail(ControlFault::UnhandledCondition {
                        class,
                        condition,
                        failed_check,
                    });
                }
                Choice::Abort => return Action::Throw(self.tags.abort, Vec::new()),
                Choice::UseValue(values) => {
                    let fits = values.len() == resume_types.len()
                        && values.iter().zip(resume_types).all(|(value, expected)| {
                            ValueRef::from_value(value).value_type() == *expected
                        });
                    return if fits {
                        Action::Complete(values)
                    } else {
                        Action::Fail(ControlFault::InvalidChoice)
                    };
                }
                Choice::Restart { token, arguments } => {
                    return self.restart(pending, condition, token, &arguments);
                }
            }
        }
    }

    /// Installs `code` while the signal waits, after checking it can take
    /// over from every unit whose code may still run: this run's and those
    /// earlier runs of the same installed code put in place.
    fn install<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingSuspension<'_, '_, H>,
        code: Arc<CodeUnit>,
    ) -> Result<(), LinkError> {
        // The run's units are every unit through its `Installed`, the run's
        // newest last, so a refusal names what that newest one promised.
        self.units
            .check_takes_over(&code, None)
            .map_err(|(_, mismatch)| LinkError::Mismatch(mismatch))?;
        let unit = pending
            .install(Arc::clone(&code.program))
            .map_err(LinkError::Install)?;
        // Linked sends of every unit, suspended frames' and earlier runs'
        // included, now follow the new code, as dynamic sends do.
        relink_all(
            self.linked
                .iter()
                .map(|(owner, code)| (owner, &**code))
                .chain(core::iter::once((unit, &*code))),
            unit,
            &code,
            |owner, link, target| pending.relink(owner, link, target),
        )?;
        self.linked.push(unit, Arc::clone(&code));
        self.units.push(unit, code);
        // Selections name the previous newest unit's methods.
        self.sends.clear();
        Ok(())
    }

    /// Unwinds to `target` and calls the newest version of its function
    /// there with the frame's own arguments.
    ///
    /// Every function and method body is a retry region: each one the
    /// exception crosses counts it down and raises it again, and the target,
    /// reached at zero, makes the call and returns its results.
    fn retry(&self, frames: &[FrameSnapshot], target: &ActivationId) -> Action {
        let Some(index) = frames
            .iter()
            .position(|frame| frame.frame.activation == *target)
        else {
            return Action::Fail(ControlFault::InvalidChoice);
        };
        // `is_retryable` holds only when the newest code keeps the frame's
        // signature and contract (`FrameCode::retry_refusal`): the frame's
        // arguments go to it and its results return to the frame's caller.
        let code = &frames[index].code;
        let Some(current) = code.current.filter(|_| code.is_retryable()) else {
            return Action::Fail(ControlFault::InvalidChoice);
        };
        let crossed = frames[..index]
            .iter()
            .filter(|frame| {
                frame
                    .code
                    .definition
                    .as_ref()
                    .is_some_and(Definition::is_retryable)
            })
            .count();
        Action::Throw(
            self.tags.retry,
            alloc::vec![
                Value::I64(i64::try_from(crossed).unwrap_or(i64::MAX)),
                Value::Func(current),
            ],
        )
    }

    /// Invokes an offered `handle` site's restart with checked arguments.
    fn restart<H: RuntimeHost>(
        &self,
        pending: &mut PendingSuspension<'_, '_, H>,
        condition: Obj,
        token: BindingToken,
        arguments: &[Value],
    ) -> Action {
        let site = self
            .state
            .position(token)
            .and_then(|index| self.site(&self.state.bindings()[index].kind));
        let Some(site) = site else {
            return Action::Fail(ControlFault::InvalidChoice);
        };
        let payload_condition = match (site.parameters.as_slice(), arguments) {
            ([], []) => Some(Value::Obj(condition)),
            ([parameter], [argument @ Value::Obj(_)]) => pending
                .host_mut()
                .with_runtime(|runtime| {
                    runtime.value_is_instance(ValueRef::from_value(argument), parameter.class)
                })
                .then(|| argument.clone()),
            _ => None,
        };
        match payload_condition {
            Some(payload) => {
                Action::Throw(self.tags.restart, alloc::vec![Value::U64(token.0), payload])
            }
            None => Action::Fail(ControlFault::InvalidChoice),
        }
    }
}
