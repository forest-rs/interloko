//! Dynamic control bindings for one run: condition handlers and their search.
//!
//! Compiled code pushes a binding when it enters a dynamic extent and pops it
//! on every exit, normal or abnormal; protected regions in the emitted tape
//! guarantee the pop. The stack therefore mirrors the live guest extents
//! exactly, and handler search walks it on the signalling stack before any
//! frame unwinds. Selecting a handler becomes a guest exception raised at the
//! `signal` host call and addressed to the establishing activation by token.
//!
//! The VM owns frames, regions and unwinding. This module owns only the
//! language policy: which binding applies, and which faults end the run.

use alloc::vec::Vec;

use execution_tape::host::{HostError, HostSig};
use execution_tape::program::ValueType;
use execution_tape::value::{TagRef, Value};
use interloko_model::ClassId;

use crate::object_type;

/// Host symbol that pushes a condition handler binding and returns its token.
pub const CONDITION_PUSH_HANDLER: &str = "interloko.condition.push_handler";
/// Host symbol that signals a condition.
///
/// A selected handler's restart is raised at the call. Its results are the
/// values a resuming restart supplies; the synchronous host never resumes, so
/// an unhandled signal fails the run.
pub const CONDITION_SIGNAL: &str = "interloko.condition.signal";
/// Host symbol that pops the innermost dynamic binding by token.
pub const CONTROL_POP: &str = "interloko.control.pop";

/// Opaque identity of one dynamic binding.
///
/// Tokens are never reused within a run, so a token names exactly one
/// activation's extent even under recursion.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BindingToken(
    /// Raw token value carried in guest registers and exception payloads.
    pub u64,
);

/// What a dynamic binding establishes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BindingKind {
    /// A condition handler that, when selected, transfers to its restart.
    Handler {
        /// Conditions of this class or a subclass are handled.
        class: ClassId,
        /// VM tag raised at the signal site to reach the establishing
        /// activation, bound from the establishing program's local tag.
        restart: TagRef,
    },
}

/// One live dynamic binding.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DynamicBinding {
    /// Identity of this binding's extent.
    pub token: BindingToken,
    /// What this binding establishes.
    pub kind: BindingKind,
}

/// Language-level failure that ended a run.
///
/// The host records the fault before failing the host call, so the embedding
/// can report it precisely instead of as a raw trap.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ControlFault {
    /// No live handler applies to a signalled condition.
    UnhandledCondition {
        /// Exact runtime class of the signalled condition.
        class: ClassId,
    },
    /// Compiled code popped a binding that is not the innermost one.
    UnbalancedPop {
        /// Token supplied by the guest.
        token: BindingToken,
        /// Innermost live binding, if any.
        innermost: Option<BindingToken>,
    },
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
    fault: Option<ControlFault>,
    stats: ControlStats,
}

impl ControlState {
    /// Live bindings, outermost first.
    #[must_use]
    pub fn bindings(&self) -> &[DynamicBinding] {
        &self.bindings
    }

    /// The fault that ended the run, if any.
    #[must_use]
    pub const fn fault(&self) -> Option<ControlFault> {
        self.fault
    }

    /// Cumulative control work.
    #[must_use]
    pub const fn stats(&self) -> ControlStats {
        self.stats
    }

    pub(crate) fn push(&mut self, kind: BindingKind) -> BindingToken {
        self.next_token = self.next_token.saturating_add(1);
        let token = BindingToken(self.next_token);
        self.bindings.push(DynamicBinding { token, kind });
        self.stats.bindings_pushed = self.stats.bindings_pushed.saturating_add(1);
        self.stats.max_depth = self.stats.max_depth.max(self.bindings.len());
        token
    }

    pub(crate) fn pop(&mut self, token: BindingToken) -> Result<(), HostError> {
        let innermost = self.bindings.last().map(|binding| binding.token);
        if innermost != Some(token) {
            return Err(self.fail(ControlFault::UnbalancedPop { token, innermost }));
        }
        self.bindings.pop();
        Ok(())
    }

    /// Searches live bindings innermost-first for a handler accepting
    /// `class`, without changing any state.
    ///
    /// `applies(handled)` decides class applicability. Embeddings that drive
    /// the VM with yielded host calls can use this at a pending signal to
    /// decide between raising a restart and consulting a chooser while every
    /// frame is still live.
    #[must_use]
    pub fn find_handler(&self, applies: impl Fn(ClassId) -> bool) -> Option<DynamicBinding> {
        self.bindings
            .iter()
            .rev()
            .find(|binding| match binding.kind {
                BindingKind::Handler { class, .. } => applies(class),
            })
            .copied()
    }

    /// Handles one synchronous signal of a condition whose class is `class`.
    ///
    /// The returned error raises the selected handler's restart tag with its
    /// token and the condition. With no applicable handler the run fails with
    /// [`ControlFault::UnhandledCondition`].
    pub(crate) fn signal(
        &mut self,
        class: ClassId,
        condition: Value,
        applies: impl Fn(ClassId) -> bool,
    ) -> HostError {
        self.stats.signals = self.stats.signals.saturating_add(1);
        let selected = self.find_handler(applies);
        let searched = match selected {
            Some(binding) => self
                .bindings
                .iter()
                .rev()
                .position(|candidate| candidate.token == binding.token)
                .map_or(0, |index| index + 1),
            None => self.bindings.len(),
        };
        self.stats.bindings_searched = self
            .stats
            .bindings_searched
            .saturating_add(u64::try_from(searched).unwrap_or(u64::MAX));
        match selected {
            Some(DynamicBinding {
                token,
                kind: BindingKind::Handler { restart, .. },
            }) => HostError::throw(restart, alloc::vec![Value::U64(token.0), condition]),
            None => self.fail(ControlFault::UnhandledCondition { class }),
        }
    }

    fn fail(&mut self, fault: ControlFault) -> HostError {
        self.fault.get_or_insert(fault);
        HostError::Failed
    }
}

/// Host signature for [`CONDITION_PUSH_HANDLER`]: `(class, restart tag) -> token`.
#[must_use]
pub fn push_handler_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64, ValueType::U64],
        rets: alloc::vec![ValueType::U64],
    }
}

/// Host signature for [`CONDITION_SIGNAL`]: `(condition) -> resumption values`.
///
/// `rets` are the signal expression's result types, which is also the
/// signature a resuming restart must supply.
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

/// Payload types of a restart tag: `(token, condition)`.
#[must_use]
pub fn restart_payload() -> [ValueType; 2] {
    [ValueType::U64, object_type()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(class: u32) -> BindingKind {
        BindingKind::Handler {
            class: ClassId(class),
            restart: TagRef(0),
        }
    }

    #[test]
    fn search_is_innermost_first_and_leaves_state_unchanged() {
        let mut control = ControlState::default();
        let outer = control.push(handler(1));
        let inner = control.push(handler(2));
        assert_ne!(outer, inner);
        let found = control.find_handler(|class| class == ClassId(1));
        assert_eq!(found.map(|binding| binding.token), Some(outer));
        let found = control.find_handler(|_| true);
        assert_eq!(found.map(|binding| binding.token), Some(inner));
        assert_eq!(control.bindings().len(), 2);
        assert_eq!(control.stats().max_depth, 2);
    }

    #[test]
    fn signal_raises_the_selected_restart_or_records_unhandled() {
        let mut control = ControlState::default();
        let token = control.push(handler(1));
        let raised = control.signal(ClassId(1), Value::Bool(true), |class| class == ClassId(1));
        let HostError::Throw(thrown) = raised else {
            panic!("expected a restart to be raised, got {raised:?}");
        };
        assert_eq!(thrown.payload[0], Value::U64(token.0));
        assert_eq!(control.fault(), None);

        let failed = control.signal(ClassId(9), Value::Bool(true), |_| false);
        assert!(matches!(failed, HostError::Failed));
        assert_eq!(
            control.fault(),
            Some(ControlFault::UnhandledCondition { class: ClassId(9) })
        );
        assert_eq!(control.stats().signals, 2);
    }

    #[test]
    fn pops_must_match_the_innermost_binding() {
        let mut control = ControlState::default();
        let outer = control.push(handler(1));
        let inner = control.push(handler(1));
        assert!(control.pop(outer).is_err());
        assert_eq!(
            control.fault(),
            Some(ControlFault::UnbalancedPop {
                token: outer,
                innermost: Some(inner),
            })
        );
        assert!(control.pop(inner).is_ok());
        assert!(control.pop(outer).is_ok());
        assert!(control.bindings().is_empty());
    }
}
