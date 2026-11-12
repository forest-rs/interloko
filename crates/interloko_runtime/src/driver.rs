//! The one way to run Interloko tape.
//!
//! [`run`] starts a yielded `execution_tape` invocation and services every
//! host request it admits. Control requests (handler and exit bindings,
//! signals) are language policy and are handled here, where the VM's frames
//! are visible: a signal searches the dynamic bindings on the signalling
//! stack before anything unwinds. Every other request dispatches to the
//! embedding's [`Host`] unchanged.

use alloc::boxed::Box;
use alloc::vec::Vec;

use execution_tape::host::{Host, HostError, ValueRef};
use execution_tape::program::ValueType;
use execution_tape::value::{FuncRef, Obj, TagRef, Value};
use execution_tape::vm::{
    ActivationId, Invocation, InvocationOptions, PendingHostCall, RunStatus, Trap, TrapInfo, Vm,
};
use interloko_model::ClassId;

use crate::InterlokoHost;
use crate::chooser::{
    Choice, ConditionChooser, FrameSnapshot, OfferedRestart, RegisterSnapshot, UnhandledCondition,
};
use crate::control::{
    BindingKind, BindingToken, CONDITION_PUSH_HANDLER, CONDITION_SIGNAL, CONTROL_CHECK_EXIT,
    CONTROL_POP, CONTROL_PUSH_EXIT, ControlFault, ControlLayout, ControlState, ControlStats,
    is_control_symbol,
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
}

impl RunFailure {
    /// The trap that ended the VM invocation.
    #[must_use]
    pub const fn trap(&self) -> &TrapInfo {
        match self {
            Self::Trap(trap) | Self::Control { trap, .. } | Self::Aborted { trap } => trap,
        }
    }
}

/// Runs `entry` with `args` to completion, servicing host requests.
///
/// `entry` names a function of a unit installed from the program `control`
/// describes. `options` may carry trace and access observers; the driver
/// opts into yielded host calls itself. Hard VM limits apply to the whole
/// run, including guest code the driver starts on the signalling stack.
///
/// A condition that no live handler accepts goes to `chooser` before anything
/// unwinds.
pub fn run<'a, H: RuntimeHost>(
    vm: &'a mut Vm<H>,
    control: &ControlLayout,
    entry: FuncRef,
    args: &'a [Value],
    options: InvocationOptions<'a>,
    chooser: &mut dyn ConditionChooser,
) -> Result<RunOutcome, RunFailure> {
    // The program names its tags locally; the driver raises the VM tags they
    // were bound to when the entry unit was installed.
    let tags = control.tags.and_then(|tags| {
        let units = vm.units();
        Some(VmTags {
            restart: units.tag(entry.unit, tags.restart)?,
            abort: units.tag(entry.unit, tags.abort)?,
        })
    });
    let mut invocation = vm
        .start(entry, args, options.yield_host_calls())
        .map_err(|trap| RunFailure::Trap(Box::new(trap)))?;
    let mut driver = Driver {
        control,
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
        match status {
            RunStatus::Complete => {
                return Ok(RunOutcome {
                    outputs: invocation.take_outputs().unwrap_or_default(),
                    stats: driver.state.stats(),
                });
            }
            RunStatus::Suspended => {}
            RunStatus::AwaitingHost | RunStatus::CallbackReturned => {
                if let Err(trap) = driver.service(&mut invocation, status, chooser) {
                    return Err(driver.failure(trap));
                }
            }
        }
    }
}

/// Copies the live frames and their initialized registers, innermost first.
fn snapshot_frames<H: RuntimeHost>(pending: &PendingHostCall<'_, '_, H>) -> Vec<FrameSnapshot> {
    let mut inspection = pending.inspect();
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
            FrameSnapshot { frame, registers }
        })
        .collect()
}

struct Driver<'c> {
    control: &'c ControlLayout,
    /// The VM tags bound to [`ControlLayout::tags`] in the entry unit.
    tags: Option<VmTags>,
    state: ControlState,
    searches: Vec<Search>,
    fault: Option<ControlFault>,
}

/// One signal's handler search, alive while its `signal` request is pending.
///
/// While a guard runs, the bindings from the guarded one up to those live
/// when this search began are masked: a signal from inside the guard sees
/// only outer bindings and bindings the guard itself establishes.
struct Search {
    /// Activation that issued the `signal`; the search dies with it.
    activation: ActivationId,
    condition: Obj,
    class: ClassId,
    resume_types: Vec<ValueType>,
    /// Bindings below this index are still candidates.
    next: usize,
    /// Bindings live when the search began.
    stack_len: usize,
    /// Binding whose guard is running on the signalling stack.
    guarding: Option<usize>,
}

/// Control tags as the VM raises and compares them.
#[derive(Copy, Clone)]
struct VmTags {
    restart: TagRef,
    abort: TagRef,
}

/// How to finish a pending request.
enum Action {
    Complete(Vec<Value>),
    Throw(TagRef, Vec<Value>),
    CallGuest(Value, Vec<Value>),
    Fail(ControlFault),
}

impl Driver<'_> {
    fn failure(&self, trap: TrapInfo) -> RunFailure {
        if let (Trap::Uncaught(exception), Some(tags)) = (&trap.trap, self.tags)
            && exception.tag() == tags.abort
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

    /// Services the waiting request: a new one, or a `signal` whose guard
    /// just returned to it.
    fn service<H: RuntimeHost>(
        &mut self,
        invocation: &mut Invocation<'_, H>,
        status: RunStatus,
        chooser: &mut dyn ConditionChooser,
    ) -> Result<(), TrapInfo> {
        let Some(mut pending) = invocation.pending_host_call() else {
            return Ok(());
        };
        let guard_returned = status == RunStatus::CallbackReturned;
        if !guard_returned && !is_control_symbol(pending.symbol()) {
            return pending.dispatch();
        }
        // A search whose signalling activation has unwound is over.
        let activations = pending
            .inspect()
            .frames()
            .map(|frame| frame.activation)
            .collect::<Vec<_>>();
        self.searches
            .retain(|search| activations.contains(&search.activation));
        let action = if guard_returned {
            let results = pending.take_callback_results().unwrap_or_default();
            self.guard_returned(&mut pending, &results, chooser)
        } else {
            self.decide(&mut pending, activations, chooser)
        };
        match action {
            Action::Complete(mut values) => pending.complete(&mut values, Ok(0)),
            Action::Throw(tag, payload) => pending.throw(tag, payload),
            Action::CallGuest(callee, args) => pending.call_guest(callee, args),
            Action::Fail(fault) => {
                self.fault.get_or_insert(fault);
                pending.complete(&mut [], Err(HostError::Failed))
            }
        }
    }

    /// Decides a new control request while it waits, with the signalling
    /// frames and the host both reachable through `pending`. `activations`
    /// are the live ones, innermost first.
    fn decide<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingHostCall<'_, '_, H>,
        activations: Vec<ActivationId>,
        chooser: &mut dyn ConditionChooser,
    ) -> Action {
        let Some(activation) = activations.into_iter().next() else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let args = pending
            .arguments()
            .map(|value| value.to_value())
            .collect::<Vec<_>>();
        match (pending.symbol(), args.as_slice()) {
            (CONDITION_PUSH_HANDLER, [Value::U64(descriptor), guard @ ..]) => {
                let Some(descriptor) = usize::try_from(*descriptor)
                    .ok()
                    .filter(|index| *index < self.control.restarts.len())
                else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                let guard = match guard {
                    [] => None,
                    [guard @ Value::Closure(_)] => Some(guard.clone()),
                    _ => return Action::Fail(ControlFault::MalformedRequest),
                };
                let token = self
                    .state
                    .push(BindingKind::Handler { descriptor, guard }, activation);
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
                    resume_types: pending.return_types().to_vec(),
                    next: live,
                    stack_len: live,
                    guarding: None,
                });
                self.step(pending, chooser)
            }
            _ => Action::Fail(ControlFault::MalformedRequest),
        }
    }

    /// Continues the innermost search after its guard returned.
    fn guard_returned<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingHostCall<'_, '_, H>,
        results: &[Value],
        chooser: &mut dyn ConditionChooser,
    ) -> Action {
        let Some(search) = self.searches.last_mut() else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let Some(index) = search.guarding.take() else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        match results {
            [Value::Bool(true)] => self.accept(index),
            [Value::Bool(false)] => self.step(pending, chooser),
            _ => Action::Fail(ControlFault::MalformedRequest),
        }
    }

    /// Advances the innermost search to its next applicable handler, still
    /// on the signalling stack. A guarded handler's guard runs first as a
    /// guest call; an unguarded one is taken at once.
    fn step<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingHostCall<'_, '_, H>,
        chooser: &mut dyn ConditionChooser,
    ) -> Action {
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
                BindingKind::Handler { guard, .. } => guard.clone(),
                BindingKind::Exit => None,
            };
            let Some(search) = self.searches.last_mut() else {
                return Action::Fail(ControlFault::MalformedRequest);
            };
            search.next = index;
            match guard {
                Some(guard) => {
                    search.guarding = Some(index);
                    return Action::CallGuest(guard, alloc::vec![Value::Obj(search.condition)]);
                }
                None => return self.accept(index),
            }
        }
        let Some(search) = self.searches.pop() else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        self.unhandled(
            pending,
            search.condition,
            search.class,
            &search.resume_types,
            chooser,
        )
    }

    /// Takes the handler at `index` for the innermost search: its restart is
    /// raised at the signal.
    fn accept(&mut self, index: usize) -> Action {
        let (Some(search), Some(tags)) = (self.searches.pop(), self.tags) else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let token = self.state.bindings()[index].token;
        Action::Throw(
            tags.restart,
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

    /// Which live bindings, outermost first, are handlers accepting `class`.
    fn applicable(&self, runtime: &InterlokoHost, class: ClassId) -> Vec<bool> {
        self.state
            .bindings()
            .iter()
            .map(|binding| match binding.kind {
                BindingKind::Handler { descriptor, .. } => self
                    .control
                    .restarts
                    .get(descriptor)
                    .is_some_and(|site| runtime.layout.is_same_or_subclass(class, site.class)),
                BindingKind::Exit => false,
            })
            .collect()
    }

    /// Hands an unhandled condition to the chooser while every frame is
    /// still live, then carries out its validated choice.
    fn unhandled<H: RuntimeHost>(
        &mut self,
        pending: &mut PendingHostCall<'_, '_, H>,
        condition: Obj,
        class: ClassId,
        resume_types: &[ValueType],
        chooser: &mut dyn ConditionChooser,
    ) -> Action {
        let Some(tags) = self.tags else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let frames = snapshot_frames(pending);
        let mut restarts = Vec::new();
        for binding in self.state.bindings().iter().rev() {
            if let BindingKind::Handler { descriptor, .. } = binding.kind
                && let Some(descriptor) = self.control.restarts.get(descriptor)
            {
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
        restarts.push(OfferedRestart::Abort);
        let state = &self.state;
        let control = self.control;
        pending.host_mut().with_runtime(|runtime| {
            let choice = chooser.choose(&UnhandledCondition {
                condition,
                class,
                runtime,
                frames: &frames,
                restarts: &restarts,
            });
            match choice {
                Choice::Unhandled => Action::Fail(ControlFault::UnhandledCondition { class }),
                Choice::Abort => Action::Throw(tags.abort, Vec::new()),
                Choice::UseValue(values) => {
                    let fits = values.len() == resume_types.len()
                        && values.iter().zip(resume_types).all(|(value, expected)| {
                            ValueRef::from_value(value).value_type() == *expected
                        });
                    if fits {
                        Action::Complete(values)
                    } else {
                        Action::Fail(ControlFault::InvalidChoice)
                    }
                }
                Choice::Restart { token, arguments } => {
                    let site = state.position(token).and_then(|index| {
                        match state.bindings()[index].kind {
                            BindingKind::Handler { descriptor, .. } => {
                                control.restarts.get(descriptor)
                            }
                            BindingKind::Exit => None,
                        }
                    });
                    let Some(site) = site else {
                        return Action::Fail(ControlFault::InvalidChoice);
                    };
                    let payload_condition = match (site.parameters.as_slice(), arguments.as_slice())
                    {
                        ([], []) => Value::Obj(condition),
                        ([parameter], [argument @ Value::Obj(_)])
                            if runtime.value_is_instance(
                                ValueRef::from_value(argument),
                                parameter.class,
                            ) =>
                        {
                            argument.clone()
                        }
                        _ => return Action::Fail(ControlFault::InvalidChoice),
                    };
                    Action::Throw(
                        tags.restart,
                        alloc::vec![Value::U64(token.0), payload_condition],
                    )
                }
            }
        })
    }
}
