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
//! A run links [`CodeUnit`]s: it installs the entry unit, and a chooser may
//! install replacements while a condition is unhandled. Generic sends ask
//! the driver which method to call ([`crate::code::CODE_DISPATCH`]),
//! and a retried frame calls the newest version of its function, while every
//! frame keeps running the unit it entered.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use execution_tape::host::{Host, HostError, ValueRef};
use execution_tape::program::ValueType;
use execution_tape::value::{FuncId, FuncRef, Obj, TagRef, Value};
use execution_tape::vm::{
    ActivationId, InstallError, InvocationInspection, InvocationOptions, PendingHostCall,
    PendingSuspension, RunStatus, TagError, Trap, TrapInfo, Vm,
};
use interloko_model::{ClassId, GenericFunctionId};

use crate::InterlokoHost;
use crate::chooser::{
    Choice, ConditionChooser, FrameSnapshot, OfferedRestart, RegisterSnapshot, UnhandledCondition,
};
use crate::code::{CODE_DISPATCH, CodeMismatch, CodeUnit, Definition, UnitTable};
use crate::control::{
    ABORT_TAG, BindingKind, BindingToken, CONDITION_GUARD, CONDITION_GUARD_RESULT,
    CONDITION_GUARD_UNWOUND, CONDITION_PUSH_HANDLER, CONDITION_SIGNAL, CONTROL_CHECK_EXIT,
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
}

impl RunFailure {
    /// The trap that ended the VM invocation, if one started.
    #[must_use]
    pub const fn trap(&self) -> Option<&TrapInfo> {
        match self {
            Self::Trap(trap) | Self::Control { trap, .. } | Self::Aborted { trap } => Some(trap),
            Self::Link(_) => None,
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
}

impl core::fmt::Display for LinkError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Tag(error) => write!(formatter, "control tags: {error:?}"),
            Self::Install(error) => write!(formatter, "install refused: {error}"),
            Self::Mismatch(mismatch) => {
                write!(formatter, "cannot replace running code: {mismatch}")
            }
        }
    }
}

/// Installs `unit` in `vm` and runs its function `entry` with `args` to
/// completion, servicing host requests.
///
/// The driver defines the control tags the unit imports, unless the VM
/// already has them, and installs the unit as a new VM unit; installing is
/// explicit and never cached, so every run adds one unit. `options` may carry
/// trace and access observers; the driver opts into yielded host calls and
/// suspensions itself. Hard VM limits apply to the whole run.
///
/// A condition that no live handler accepts goes to `chooser` before anything
/// unwinds.
pub fn run<'a, H: RuntimeHost>(
    vm: &'a mut Vm<H>,
    unit: &Arc<CodeUnit>,
    entry: FuncId,
    args: &'a [Value],
    options: InvocationOptions<'a>,
    chooser: &mut dyn ConditionChooser,
) -> Result<RunOutcome, RunFailure> {
    let tags = define_tags(vm).map_err(RunFailure::Link)?;
    let installed = vm
        .install(Arc::clone(&unit.program))
        .map_err(|error| RunFailure::Link(LinkError::Install(error)))?;
    let mut units = UnitTable::default();
    units.push(installed, Arc::clone(unit));
    let mut invocation = vm
        .start(
            FuncRef::new(installed, entry),
            args,
            options.yield_host_calls().yield_suspensions(),
        )
        .map_err(|trap| RunFailure::Trap(Box::new(trap)))?;
    let mut driver = Driver {
        units,
        sends: BTreeMap::new(),
        tags,
        state: ControlState::default(),
        searches: Vec::new(),
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
    Ok(VmTags {
        restart: define(RESTART_TAG, &restart_payload())?,
        abort: define(ABORT_TAG, &[])?,
        retry: define(RETRY_TAG, &retry_payload())?,
    })
}

/// Copies the live frames and their initialized registers, innermost first.
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

struct Driver {
    units: UnitTable,
    /// Functions sends selected, by generic and the classes of the argument
    /// positions its methods specialize. Cleared by every install.
    sends: BTreeMap<SendKey, FuncRef>,
    tags: VmTags,
    state: ControlState,
    searches: Vec<Search>,
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

impl Driver {
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
                self.unhandled(&mut suspension, condition, class, &resume_types, chooser)
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
    /// unit's dispatch table cannot decide, so the dispatcher's own checks
    /// and traps apply.
    fn dispatch<H: RuntimeHost>(
        &mut self,
        mut pending: PendingHostCall<'_, '_, H>,
    ) -> Result<(), TrapInfo> {
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
                });
                self.step(pending)
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
        self.searches.pop();
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
        condition: Obj,
        class: ClassId,
        resume_types: &[ValueType],
        chooser: &mut dyn ConditionChooser,
    ) -> Action {
        let mut refused = None;
        loop {
            let frames = snapshot_frames(pending.inspect(), &self.units);
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
                        runtime,
                        frames: &frames,
                        restarts: &restarts,
                        refused: refused.as_ref(),
                    })
                })
            };
            match choice {
                Choice::Install(code) => refused = self.install(pending, code).err(),
                Choice::Retry { frame } => return self.retry(&frames, &frame),
                Choice::Unhandled => {
                    return Action::Fail(ControlFault::UnhandledCondition { class });
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
    /// over from the newest unit.
    fn install<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingSuspension<'_, '_, H>,
        code: Arc<CodeUnit>,
    ) -> Result<(), LinkError> {
        if let Some(newest) = self.units.newest() {
            code.check_replaces(newest).map_err(LinkError::Mismatch)?;
        }
        let unit = pending
            .install(Arc::clone(&code.program))
            .map_err(LinkError::Install)?;
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
        let code = &frames[index].code;
        let Some(current) = code.current.filter(|_| code.is_retryable()) else {
            return Action::Fail(ControlFault::InvalidChoice);
        };
        if self.units.signature(current) != self.units.signature(code.func) {
            return Action::Fail(ControlFault::InvalidChoice);
        }
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
