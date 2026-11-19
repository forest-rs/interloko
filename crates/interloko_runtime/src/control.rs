//! Dynamic control bindings for one run: condition handlers, block exit
//! points, and the restart descriptors that make them enumerable.
//!
//! Compiled code pushes a binding when it enters a dynamic extent and pops it
//! on every exit, normal or abnormal; protected regions in the emitted tape
//! guarantee the pop. The stack therefore mirrors the live guest extents
//! exactly. Each binding records the VM activation that established it, so
//! restarts can be attributed to frames.
//!
//! A signal searches those bindings through host requests, running guards
//! as ordinary guest calls on the signalling stack. A condition no handler
//! accepts is then suspended on a condition tag ([`ControlLayout::conditions`])
//! and reaches the embedding as an `execution_tape` suspension, which the
//! driver answers with the chooser's restart.
//!
//! The VM owns frames, regions, unwinding and suspensions. This module owns
//! the language data; [`crate::driver`] services the host requests that change
//! it and answers the suspensions.

use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use execution_tape::host::HostSig;
use execution_tape::program::{TagId, ValueType};
use execution_tape::value::{FuncId, Obj, UnitId, Value};
use execution_tape::vm::ActivationId;
use interloko_model::ClassId;
use interloko_model::condition::TypeErrorKind;
use interloko_model::type_object::TypeObject;

use crate::object_type;

/// Host symbol that pushes a condition handler binding and returns its token.
pub const CONDITION_PUSH_HANDLER: &str = "interloko.condition.push_handler";
/// Host symbol that starts a handler search for a signalled condition.
///
/// A selected handler's restart is raised at the call. Otherwise it returns
/// `#t` when a guarded handler applies and its guard must run first
/// ([`CONDITION_GUARD`]), or `#f` when no handler applies and the signal
/// suspends on its condition tag.
pub const CONDITION_SIGNAL: &str = "interloko.condition.signal";
/// Host symbol that returns the guard the signalling activation's search is
/// waiting on. The guest calls it with the condition.
pub const CONDITION_GUARD: &str = "interloko.condition.guard";
/// Host symbol that hands the search the guard's answer and continues it,
/// with the same outcomes as [`CONDITION_SIGNAL`].
pub const CONDITION_GUARD_RESULT: &str = "interloko.condition.guard_result";
/// Host symbol that ends the signalling activation's search because its
/// guard exited by an exception, which the guest raises again afterwards.
pub const CONDITION_GUARD_UNWOUND: &str = "interloko.condition.guard_unwound";
/// Host symbol that makes the `<type-error>` a failed runtime check signals:
/// `(values..., site) -> condition`, where `site` indexes the calling
/// function's sites in its unit's [`ControlLayout::type_errors`]. One value
/// becomes the condition's value; several (the operands of an overflowing
/// operation) a `<vector>` of them.
pub const CONDITION_TYPE_ERROR: &str = "interloko.condition.type_error";
/// Host symbol that makes the `<no-applicable-method>` a send that found no
/// method signals: `(generic, arguments) -> condition`, where `generic` is
/// the generic's id in the runtime layout and `arguments` a `<vector>` of
/// the send's arguments. A generic's dispatcher, which every such send
/// reaches, makes the request.
pub const CONDITION_NO_APPLICABLE_METHOD: &str = "interloko.condition.no_applicable_method";
/// Host symbol that pops the innermost dynamic binding by token.
pub const CONTROL_POP: &str = "interloko.control.pop";
/// Host symbol that pushes a block exit point and returns its token.
pub const CONTROL_PUSH_EXIT: &str = "interloko.control.push_exit";
/// Host symbol that checks an exit point is still live before exiting to it.
pub const CONTROL_CHECK_EXIT: &str = "interloko.control.check_exit";

/// VM tag raised at a signal to reach the selected handler's activation,
/// with payload [`restart_payload`]: `(token, condition)`.
pub const RESTART_TAG: &str = "interloko.restart";
/// VM tag raised to unwind the whole run; only cleanups catch it.
pub const ABORT_TAG: &str = "interloko.abort";
/// VM tag raised to retry a frame, with payload [`retry_payload`]:
/// `(retry frames still to cross, function to call instead)`.
pub const RETRY_TAG: &str = "interloko.retry";
/// VM tag, with no payload, that a checking host call raises instead of
/// failing when its check rejects the value and the calling program
/// imports the tag: [`crate::CAST_OBJECT`], [`crate::CAST_CLASS_OBJECT`]
/// and [`crate::UNBOX_VALUE`]. The program catches it around the call and
/// signals a `<type-error>` ([`CONDITION_TYPE_ERROR`]); a passing check
/// costs what it did.
pub const CHECK_FAILED_TAG: &str = "interloko.check_failed";

/// Whether `symbol` is a control request serviced by [`crate::driver`]
/// rather than by a [`execution_tape::host::Host`].
///
/// [`crate::code::CODE_DISPATCH`] is not one: the host answers it, inline
/// unless [`crate::InterlokoHost`]'s `is_synchronous` says otherwise, which
/// a host wrapper should forward rather than derive from this.
#[must_use]
pub fn is_control_symbol(symbol: &str) -> bool {
    matches!(
        symbol,
        CONDITION_PUSH_HANDLER
            | CONDITION_SIGNAL
            | CONDITION_GUARD
            | CONDITION_GUARD_RESULT
            | CONDITION_GUARD_UNWOUND
            | CONDITION_TYPE_ERROR
            | CONDITION_NO_APPLICABLE_METHOD
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

/// Static description of one runtime check that signals a `<type-error>`
/// when it fails.
///
/// The condition records the site's kind as its stable code, its expected
/// type as a type object, and its site, so a handler, a chooser or an agent can tie a
/// failure at run time to the check, and to the typing decision when soft
/// typing classified the check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeErrorSite {
    /// What kind of check failed; its stable code.
    pub kind: TypeErrorKind,
    /// The expected type, which the condition holds as a type object:
    /// a class such as `<circle>`, a limited integer type such as `<i32>`,
    /// or a subclass type such as `subclass(<shape>)`.
    pub expected: TypeObject,
    /// Where the check is, in one of these forms:
    ///
    /// - `typing:<definition>:s<n>`, the EOP id of the typing decision that
    ///   classified the check;
    /// - `entry:<function>:<index>`, source parameter `index` of the lowered
    ///   function `function` (such as `exit#3`, `closure#0` or a callable
    ///   thunk), for an entry check typing does not classify;
    /// - `check:<definition>@<start>..<end>`, a check typing does not
    ///   classify, by the definition whose code makes it and its span;
    /// - `check@<start>..<end>`, the same in generated code no definition
    ///   owns.
    pub site: String,
    /// Source byte range of the checked expression, empty when the check
    /// has no source of its own.
    pub span: Range<usize>,
}

/// Control metadata of one emitted program.
///
/// Programs import the shared control tags ([`RESTART_TAG`], [`RETRY_TAG`],
/// and [`CHECK_FAILED_TAG`] when they check) by name, so every unit of a
/// run throws and catches the same exceptions. The driver defines them,
/// with [`ABORT_TAG`], before it installs a unit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControlLayout {
    /// Restart sites, indexed by the descriptor operand of
    /// [`CONDITION_PUSH_HANDLER`].
    pub restarts: Vec<RestartDescriptor>,
    /// Checks that signal a `<type-error>` when they fail: per function, by
    /// function id, the sites the site operand of [`CONDITION_TYPE_ERROR`]
    /// indexes. Numbering them per function keeps a function's code
    /// independent of how many checks other functions have, so an edit
    /// elsewhere leaves it byte-for-byte the same.
    pub type_errors: Vec<Vec<TypeErrorSite>>,
    /// The program's condition tags: an unhandled signal suspends on one,
    /// with payload `(condition)` and its own result types as the tag's
    /// results. No guest code handles them, so each suspension reaches the
    /// driver, which answers it with the chooser's restart.
    pub conditions: Vec<TagId>,
}

impl ControlLayout {
    /// The `<type-error>` site `index` of function `func`.
    #[must_use]
    pub fn type_error(&self, func: FuncId, index: usize) -> Option<&TypeErrorSite> {
        self.type_errors.get(func.0 as usize)?.get(index)
    }
}

/// What a dynamic binding establishes.
#[derive(Clone, Debug, PartialEq)]
pub enum BindingKind {
    /// A `handle` site's handler and restart.
    Handler {
        /// Unit whose code established the handler.
        unit: UnitId,
        /// Index into that unit's [`ControlLayout::restarts`].
        descriptor: usize,
        /// Guard closure, in the run's own heap, that may decline.
        guard: Option<Value>,
    },
    /// A `block` exit point, live until its block's extent ends.
    Exit,
}

/// One live dynamic binding.
#[derive(Clone, Debug, PartialEq)]
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
        /// The condition object. It belongs to the run's turn, so it stays
        /// readable until the embedding finishes or aborts that turn, as
        /// [`crate::InterlokoHost::type_error_report`] does for a
        /// `<type-error>` and
        /// [`crate::InterlokoHost::no_applicable_method_report`] for a
        /// `<no-applicable-method>`.
        condition: Obj,
        /// Whether the runtime signalled the condition for a check that
        /// failed, a runtime type check or a send that found no applicable
        /// method, rather than the program's own `signal`.
        failed_check: bool,
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
    /// A chooser's answer did not match the restarts on offer or their
    /// parameter types.
    InvalidChoice,
    /// Code another install put in the VM, such as a closure passed in as
    /// an argument, sent a generic: nothing compared what that code relies
    /// on with the run's methods, so the send is refused.
    ForeignCode {
        /// The sending code's unit.
        unit: UnitId,
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
    /// Signalled conditions no handler accepted, answered by the chooser.
    pub unhandled: u64,
    /// Bindings inspected while searching for handlers.
    pub bindings_searched: u64,
    /// Generic sends the runtime selected a method for by their argument
    /// classes, consulting its dispatch cache
    /// ([`crate::InterlokoHost::send_cache`]).
    pub sends: u64,
    /// Those sends the dispatch cache answered.
    pub send_cache_hits: u64,
    /// Sends that found no applicable method and signalled
    /// `<no-applicable-method>`.
    pub failed_sends: u64,
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

    pub(crate) fn note_unhandled(&mut self) {
        self.stats.unhandled = self.stats.unhandled.saturating_add(1);
    }

    pub(crate) fn note_failed_send(&mut self) {
        self.stats.failed_sends = self.stats.failed_sends.saturating_add(1);
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

/// Host signature for a guarded [`CONDITION_PUSH_HANDLER`]:
/// `(descriptor, guard) -> token`. The guard takes the condition and returns
/// `<boolean>`.
#[must_use]
pub fn push_guarded_handler_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64, ValueType::Closure],
        rets: alloc::vec![ValueType::U64],
    }
}

/// Host signature for [`CONDITION_SIGNAL`]: `(condition) -> guard pending`.
#[must_use]
pub fn signal_signature() -> HostSig {
    HostSig {
        args: alloc::vec![object_type()],
        rets: alloc::vec![ValueType::Bool],
    }
}

/// Host signature for [`CONDITION_GUARD`]: `() -> guard`. The guard takes
/// the condition and returns `<boolean>`.
#[must_use]
pub fn guard_signature() -> HostSig {
    HostSig {
        args: alloc::vec![],
        rets: alloc::vec![ValueType::Closure],
    }
}

/// Host signature for [`CONDITION_GUARD_RESULT`]: `(accepted) -> guard
/// pending`.
#[must_use]
pub fn guard_result_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::Bool],
        rets: alloc::vec![ValueType::Bool],
    }
}

/// Host signature for [`CONDITION_GUARD_UNWOUND`]: `() -> ()`.
#[must_use]
pub fn guard_unwound_signature() -> HostSig {
    HostSig {
        args: alloc::vec![],
        rets: alloc::vec![],
    }
}

/// Payload types of a condition tag: `(condition)`. Its results are the
/// signal expression's result types, which a resuming choice supplies.
#[must_use]
pub fn condition_payload() -> [ValueType; 1] {
    [object_type()]
}

/// Host signature for [`CONDITION_TYPE_ERROR`] with offending values of
/// types `values`: `(values..., site) -> condition`.
#[must_use]
pub fn type_error_signature(values: &[ValueType]) -> HostSig {
    let mut args = values.to_vec();
    args.push(ValueType::U64);
    HostSig {
        args,
        rets: alloc::vec![object_type()],
    }
}

/// Host signature for [`CONDITION_NO_APPLICABLE_METHOD`]: `(generic,
/// arguments) -> condition`.
#[must_use]
pub fn no_applicable_method_signature() -> HostSig {
    HostSig {
        args: alloc::vec![ValueType::U64, object_type()],
        rets: alloc::vec![object_type()],
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

/// Payload types of the retry tag: `(i64 frames to cross, function)`.
#[must_use]
pub fn retry_payload() -> [ValueType; 2] {
    [ValueType::I64, ValueType::Func]
}
