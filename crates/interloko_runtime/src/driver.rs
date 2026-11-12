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

use execution_tape::host::{Host, HostError};
use execution_tape::value::{FuncRef, TagRef, Value};
use execution_tape::vm::{Invocation, InvocationOptions, PendingHostCall, RunStatus, TrapInfo, Vm};
use interloko_model::ClassId;

use crate::InterlokoHost;
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
}

impl RunFailure {
    /// The trap that ended the VM invocation.
    #[must_use]
    pub const fn trap(&self) -> &TrapInfo {
        match self {
            Self::Trap(trap) | Self::Control { trap, .. } => trap,
        }
    }
}

/// Runs `entry` with `args` to completion, servicing host requests.
///
/// `entry` names a function of a unit installed from the program `control`
/// describes. `options` may carry trace and access observers; the driver
/// opts into yielded host calls itself. Hard VM limits apply to the whole
/// run, including guest code the driver starts on the signalling stack.
pub fn run<'a, H: RuntimeHost>(
    vm: &'a mut Vm<H>,
    control: &ControlLayout,
    entry: FuncRef,
    args: &'a [Value],
    options: InvocationOptions<'a>,
) -> Result<RunOutcome, RunFailure> {
    // The program names its tags locally; the driver raises the VM tags they
    // were bound to when the entry unit was installed.
    let restart = control
        .tags
        .and_then(|tags| vm.units().tag(entry.unit, tags.restart));
    let mut invocation = vm
        .start(entry, args, options.yield_host_calls())
        .map_err(|trap| RunFailure::Trap(Box::new(trap)))?;
    let mut driver = Driver {
        control,
        restart,
        state: ControlState::default(),
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
            RunStatus::Suspended | RunStatus::CallbackReturned => {}
            RunStatus::AwaitingHost => {
                if let Err(trap) = driver.service(&mut invocation) {
                    return Err(driver.failure(trap));
                }
            }
        }
    }
}

struct Driver<'c> {
    control: &'c ControlLayout,
    /// The VM tag of [`ControlLayout::tags`]' restart tag.
    restart: Option<TagRef>,
    state: ControlState,
    fault: Option<ControlFault>,
}

/// How to finish a pending request.
enum Action {
    Complete(Vec<Value>),
    Throw(TagRef, Vec<Value>),
    Fail(ControlFault),
}

impl Driver<'_> {
    fn failure(&self, trap: TrapInfo) -> RunFailure {
        match self.fault {
            Some(fault) => RunFailure::Control {
                fault,
                trap: Box::new(trap),
            },
            None => RunFailure::Trap(Box::new(trap)),
        }
    }

    fn service<H: RuntimeHost>(
        &mut self,
        invocation: &mut Invocation<'_, H>,
    ) -> Result<(), TrapInfo> {
        let Some(mut pending) = invocation.pending_host_call() else {
            return Ok(());
        };
        if !is_control_symbol(pending.symbol()) {
            return pending.dispatch();
        }
        match self.decide(&mut pending) {
            Action::Complete(mut values) => pending.complete(&mut values, Ok(0)),
            Action::Throw(tag, payload) => pending.throw(tag, payload),
            Action::Fail(fault) => {
                self.fault.get_or_insert(fault);
                pending.complete(&mut [], Err(HostError::Failed))
            }
        }
    }

    /// Decides a control request while it waits, with the signalling frames
    /// and the host both reachable through `pending`.
    fn decide<H: RuntimeHost>(&mut self, pending: &mut PendingHostCall<'_, '_, H>) -> Action {
        let Some(activation) = pending
            .inspect()
            .frames()
            .next()
            .map(|frame| frame.activation)
        else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        let args = pending
            .arguments()
            .map(|value| value.to_value())
            .collect::<Vec<_>>();
        match (pending.symbol(), args.as_slice()) {
            (CONDITION_PUSH_HANDLER, [Value::U64(descriptor)]) => {
                let Some(descriptor) = usize::try_from(*descriptor)
                    .ok()
                    .filter(|index| *index < self.control.restarts.len())
                else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                let token = self
                    .state
                    .push(BindingKind::Handler { descriptor }, activation);
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
                let applicable = pending.host_mut().with_runtime(|runtime| {
                    let class = runtime.object(condition).ok()?.class;
                    Some((class, self.applicable(runtime, class)))
                });
                let Some((class, applicable)) = applicable else {
                    return Action::Fail(ControlFault::MalformedRequest);
                };
                self.search(class, Value::Obj(condition), &applicable)
            }
            _ => Action::Fail(ControlFault::MalformedRequest),
        }
    }

    /// Which live bindings, outermost first, are handlers accepting `class`.
    fn applicable(&self, runtime: &InterlokoHost, class: ClassId) -> Vec<bool> {
        self.state
            .bindings()
            .iter()
            .map(|binding| match binding.kind {
                BindingKind::Handler { descriptor } => self
                    .control
                    .restarts
                    .get(descriptor)
                    .is_some_and(|site| runtime.layout.is_same_or_subclass(class, site.class)),
                BindingKind::Exit => false,
            })
            .collect()
    }

    /// Selects the innermost applicable handler, still on the signalling
    /// stack, and raises its restart.
    fn search(&mut self, class: ClassId, condition: Value, applicable: &[bool]) -> Action {
        let Some(restart) = self.restart else {
            return Action::Fail(ControlFault::MalformedRequest);
        };
        for index in (0..applicable.len()).rev() {
            self.state.note_searched();
            if applicable[index] {
                let token = self.state.bindings()[index].token;
                return Action::Throw(restart, alloc::vec![Value::U64(token.0), condition]);
            }
        }
        Action::Fail(ControlFault::UnhandledCondition { class })
    }
}
