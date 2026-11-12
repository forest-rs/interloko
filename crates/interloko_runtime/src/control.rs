//! Dynamic control bindings for one run: condition handlers, block exit
//! points, and the restart descriptors that make them enumerable.
//!
//! Compiled code pushes a binding when it enters a dynamic extent and pops it
//! on every exit, normal or abnormal; protected regions in the emitted tape
//! guarantee the pop. The stack therefore mirrors the live guest extents
//! exactly. Each binding records the VM activation that established it, so
//! restarts can be attributed to frames.
//!
//! The VM owns frames, regions and unwinding. This module owns the language
//! data; [`crate::driver`] services the host requests that change it.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use execution_tape::host::HostSig;
use execution_tape::program::{TagId, ValueType};
use execution_tape::vm::ActivationId;
use interloko_model::ClassId;

use crate::object_type;

/// Host symbol that pushes a condition handler binding and returns its token.
pub const CONDITION_PUSH_HANDLER: &str = "interloko.condition.push_handler";
/// Host symbol that signals a condition.
///
/// A selected handler's restart is raised at the call. Its results are the
/// values a resuming choice supplies.
pub const CONDITION_SIGNAL: &str = "interloko.condition.signal";
/// Host symbol that pops the innermost dynamic binding by token.
pub const CONTROL_POP: &str = "interloko.control.pop";
/// Host symbol that pushes a block exit point and returns its token.
pub const CONTROL_PUSH_EXIT: &str = "interloko.control.push_exit";
/// Host symbol that checks an exit point is still live before exiting to it.
pub const CONTROL_CHECK_EXIT: &str = "interloko.control.check_exit";

/// Whether `symbol` is a control request serviced by [`crate::driver`]
/// rather than by a [`execution_tape::host::Host`].
#[must_use]
pub fn is_control_symbol(symbol: &str) -> bool {
    matches!(
        symbol,
        CONDITION_PUSH_HANDLER
            | CONDITION_SIGNAL
            | CONTROL_POP
            | CONTROL_PUSH_EXIT
            | CONTROL_CHECK_EXIT
    )
}

/// Opaque identity of one dynamic binding.
///
/// Tokens are never reused within a run, so a token names exactly one
/// activation's extent even under recursion.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BindingToken(
    /// Raw token value carried in guest registers and exception payloads.
    pub u64,
);

/// One typed parameter of a restart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestartParameter {
    /// Source name of the parameter.
    pub name: String,
    /// The parameter is a condition object of this class or a subclass.
    pub class: ClassId,
}

/// Static description of one `handle … with restart` site.
///
/// The site is both a handler, for conditions of [`Self::class`], and a
/// program-defined restart that a chooser may invoke for any condition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestartDescriptor {
    /// Restart name: the handled class name without angle brackets.
    pub name: String,
    /// Handled condition class.
    pub class: ClassId,
    /// Parameters the recovery expression binds: empty, or the condition.
    pub parameters: Vec<RestartParameter>,
    /// Source byte range of the `handle` expression.
    pub span: Range<usize>,
}

/// Tags the runtime raises to transfer control.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ControlTags {
    /// Raised at a signal to reach the selected handler's activation, with
    /// payload `(token, condition)`.
    pub restart: TagId,
}

/// Control metadata of one emitted program.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlLayout {
    /// Control tags, present when the program uses dynamic control.
    pub tags: Option<ControlTags>,
    /// Restart sites, indexed by the descriptor operand of
    /// [`CONDITION_PUSH_HANDLER`].
    pub restarts: Vec<RestartDescriptor>,
}

/// What a dynamic binding establishes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindingKind {
    /// A `handle` site's handler and restart.
    Handler {
        /// Index into [`ControlLayout::restarts`].
        descriptor: usize,
    },
    /// A `block` exit point, live until its block's extent ends.
    Exit,
}

/// One live dynamic binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicBinding {
    /// Identity of this binding's extent.
    pub token: BindingToken,
    /// What this binding establishes.
    pub kind: BindingKind,
    /// VM activation whose code pushed the binding.
    pub activation: ActivationId,
}

/// Language-level failure that ended a run.
///
/// The driver records the fault before failing the pending request, so the
/// embedding can report it precisely instead of as a raw trap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ControlFault {
    /// No live handler applies to a signalled condition.
    UnhandledCondition {
        /// Exact runtime class of the signalled condition.
        class: ClassId,
    },
    /// A block exit was invoked after its block's extent ended.
    ExpiredExit {
        /// Token of the expired exit point.
        token: BindingToken,
    },
    /// Compiled code popped a binding that is not the innermost one.
    UnbalancedPop {
        /// Token supplied by the guest.
        token: BindingToken,
        /// Innermost live binding, if any.
        innermost: Option<BindingToken>,
    },
    /// A control request did not match its protocol.
    MalformedRequest,
}

/// Cumulative control work performed in one run.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlStats {
    /// Bindings pushed.
    pub bindings_pushed: u64,
    /// Deepest binding stack observed.
    pub max_depth: usize,
    /// Conditions signalled.
    pub signals: u64,
    /// Bindings inspected while searching for handlers.
    pub bindings_searched: u64,
}

/// The dynamic binding stack of one run.
#[derive(Clone, Debug, Default)]
pub struct ControlState {
    bindings: Vec<DynamicBinding>,
    next_token: u64,
    stats: ControlStats,
}

impl ControlState {
    /// Live bindings, outermost first.
    #[must_use]
    pub fn bindings(&self) -> &[DynamicBinding] {
        &self.bindings
    }

    /// Cumulative control work.
    #[must_use]
    pub const fn stats(&self) -> ControlStats {
        self.stats
    }

    /// Index of the live binding named by `token`.
    #[must_use]
    pub fn position(&self, token: BindingToken) -> Option<usize> {
        self.bindings
            .iter()
            .position(|binding| binding.token == token)
    }

    pub(crate) fn push(&mut self, kind: BindingKind, activation: ActivationId) -> BindingToken {
        self.next_token = self.next_token.saturating_add(1);
        let token = BindingToken(self.next_token);
        self.bindings.push(DynamicBinding {
            token,
            kind,
            activation,
        });
        self.stats.bindings_pushed = self.stats.bindings_pushed.saturating_add(1);
        self.stats.max_depth = self.stats.max_depth.max(self.bindings.len());
        token
    }

    pub(crate) fn pop(&mut self, token: BindingToken) -> Result<(), ControlFault> {
        let innermost = self.bindings.last().map(|binding| binding.token);
        if innermost != Some(token) {
            return Err(ControlFault::UnbalancedPop { token, innermost });
        }
        self.bindings.pop();
        Ok(())
    }

    /// Whether the exit point `token` is still live.
    #[must_use]
    pub fn exit_is_live(&self, token: BindingToken) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.token == token && binding.kind == BindingKind::Exit)
    }

    pub(crate) fn note_signal(&mut self) {
        self.stats.signals = self.stats.signals.saturating_add(1);
    }

    pub(crate) fn note_searched(&mut self) {
        self.stats.bindings_searched = self.stats.bindings_searched.saturating_add(1);
    }
}

/// Host signature for [`CONDITION_PUSH_HANDLER`]: `(descriptor) -> token`.
#[must_use]
pub fn push_handler_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64],
        rets: alloc::vec![ValueType::U64],
    }
}

/// Host signature for [`CONDITION_SIGNAL`]: `(condition) -> resumption values`.
///
/// `rets` are the signal expression's result types, which is also the
/// signature a resuming choice must supply.
#[must_use]
pub fn signal_signature(rets: Vec<ValueType>) -> HostSig {
    HostSig {
        args: alloc::vec![object_type()],
        rets,
    }
}

/// Host signature for [`CONTROL_POP`]: `(token) -> ()`.
#[must_use]
pub fn pop_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64],
        rets: alloc::vec![],
    }
}

/// Host signature for [`CONTROL_PUSH_EXIT`]: `() -> token`.
#[must_use]
pub fn push_exit_signature() -> HostSig {
    HostSig {
        args: alloc::vec![],
        rets: alloc::vec![ValueType::U64],
    }
}

/// Host signature for [`CONTROL_CHECK_EXIT`]: `(token) -> ()`.
#[must_use]
pub fn check_exit_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64],
        rets: alloc::vec![],
    }
}

/// Payload types of a restart tag: `(token, condition)`.
#[must_use]
pub fn restart_payload() -> [ValueType; 2] {
    [ValueType::U64, object_type()]
}
