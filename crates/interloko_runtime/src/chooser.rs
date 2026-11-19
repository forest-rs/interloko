//! The seam where an unhandled condition meets its embedding.
//!
//! When a signal finds no applicable handler, [`crate::driver`] stops before
//! anything unwinds and hands a [`ConditionChooser`] the condition, the live
//! frames and the restarts on offer, all as data. The chooser answers with a
//! [`Choice`]; the driver validates it against the restart's typed signature
//! and carries it out. A REPL break loop, a scripted chooser or a recorded
//! oracle are all choosers; [`ReportUnhandled`] is the default.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use execution_tape::program::{LinkId, Reg, ValueType};
use execution_tape::value::{FuncRef, Obj, UnitId, Value};
use execution_tape::vm::{ActivationId, PausedFrame};
use interloko_model::{ClassId, MethodId};

use crate::code::{CodeUnit, Definition, FrameCode};
use crate::control::{BindingToken, RestartDescriptor};
use crate::driver::LinkError;
use crate::{InterlokoHost, NoApplicableMethodReport, TypeErrorReport};

/// One initialized register of a suspended frame.
#[derive(Clone, Debug)]
pub struct RegisterSnapshot {
    /// Function-local register.
    pub register: Reg,
    /// Its value. Aggregate handles belong to the suspended invocation.
    pub value: Value,
}

/// One suspended activation, innermost first.
#[derive(Clone, Debug)]
pub struct FrameSnapshot {
    /// VM frame metadata: function, location and activation identity.
    pub frame: PausedFrame,
    /// Registers definitely initialized at the frame's location.
    pub registers: Vec<RegisterSnapshot>,
    /// The code unit the frame runs and whether newer code replaced it.
    pub code: FrameCode,
}

/// Where one devirtualized send of one installed unit points now.
///
/// Installing code repoints every unit's links at the newest code's
/// selection; this is how a chooser sees the result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkSnapshot {
    /// The unit whose code makes the send.
    pub unit: UnitId,
    /// The link within that unit's program.
    pub link: LinkId,
    /// The link's name in the program, such as `area(<circle>)`.
    pub name: String,
    /// The method the send selected, whose link this is.
    pub method: MethodId,
    /// The function the send calls now.
    pub target: FuncRef,
    /// What that function implements: the selected method, or the generic's
    /// dispatcher when the newest method needs its arguments converted.
    pub definition: Option<Definition>,
}

/// What one guarded send of one installed unit has cached so far.
///
/// A guarded send calls its method through a link whose guard holds the
/// classes of the arguments last selected ([`crate::code::SendSite`]): an
/// observation the runtime keeps current, not a proof, so it is reported
/// apart from [`LinkSnapshot`]s.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SiteSnapshot {
    /// The unit whose code makes the send.
    pub unit: UnitId,
    /// The guarded link within that unit's program.
    pub link: LinkId,
    /// The generic sent to.
    pub generic: interloko_model::GenericFunctionId,
    /// The classes the guard admits, or that it admits any or none.
    pub guard: execution_tape::vm::LinkGuard,
    /// The function sends the guard admits call now.
    pub target: FuncRef,
    /// Sends that missed the guard since the unit was installed.
    pub misses: u64,
}

/// A restart a chooser may select.
#[derive(Clone, Debug)]
pub enum OfferedRestart<'a> {
    /// A live `handle` site. It did not apply to this condition, but it can
    /// still be invoked: control unwinds to the activation that established
    /// it and its recovery expression runs there.
    Handler {
        /// Binding to name in [`Choice::Restart`].
        token: BindingToken,
        /// The site's name and typed parameters.
        descriptor: &'a RestartDescriptor,
        /// Activation that established the site.
        activation: &'a ActivationId,
    },
    /// Resume the signal: it returns values of these types where it was
    /// called.
    UseValue {
        /// The signal expression's result types.
        types: &'a [ValueType],
    },
    /// Unwind to a frame whose [`FrameCode::is_retryable`] holds and call
    /// the newest version of its function there, with the frame's own
    /// arguments; its result returns to the frame's caller. A frame whose
    /// definition's newest code changed its signature or broke its contract
    /// is not retryable; [`FrameCode::retry_refusal`] says why.
    Retry,
    /// Unwind the whole run, running every crossed cleanup.
    Abort,
}

/// A condition no live handler accepted, with the suspended computation.
pub struct UnhandledCondition<'a> {
    /// The signalled condition object.
    pub condition: Obj,
    /// Its exact class.
    pub class: ClassId,
    /// Whether the runtime signalled the condition for a check that failed,
    /// rather than the program's own `signal`, whose `use-value` is what the
    /// `signal` expression returns. It is then a `<type-error>` whose
    /// [`OfferedRestart::UseValue`] continues the failed type check
    /// ([`Self::type_error`]), or a `<no-applicable-method>` whose
    /// `use-value` is the result of the send that found no method
    /// ([`Self::no_applicable_method`]).
    pub failed_check: bool,
    /// Runtime objects, classes and slots, for presenting the condition.
    pub runtime: &'a InterlokoHost,
    /// Live frames, innermost (the signaller) first. Nothing has unwound.
    pub frames: &'a [FrameSnapshot],
    /// Every link the sends of the run's units, and of units earlier runs
    /// installed, use, oldest unit first, with the function it calls now.
    pub links: &'a [LinkSnapshot],
    /// Every guarded send of the same units, with what its guard holds.
    pub sites: &'a [SiteSnapshot],
    /// Restarts on offer, innermost handler sites first, then the built-ins.
    pub restarts: &'a [OfferedRestart<'a>],
    /// Why the chooser's last [`Choice::Install`] was refused, if it was.
    pub refused: Option<&'a LinkError>,
}

impl UnhandledCondition<'_> {
    /// What a failed runtime check reports, when one signalled the
    /// condition: the stable code, the expected type, the rejected value
    /// and the check's site. A `<type-error>` the program signalled itself
    /// is an ordinary condition and reports nothing here.
    ///
    /// [`OfferedRestart::UseValue`] supplies what the checked operation
    /// produces instead: a number of the operation's result type, which is
    /// all such a check establishes, or, for a cast, an object that the
    /// cast checks again.
    #[must_use]
    pub fn type_error(&self) -> Option<TypeErrorReport> {
        if !self.failed_check {
            return None;
        }
        self.runtime.type_error_report(self.condition)
    }

    /// What a send that found no applicable method reports, when one
    /// signalled the condition: the stable code, the generic, and the
    /// arguments with their classes. A `<no-applicable-method>` the program
    /// signalled itself is an ordinary condition and reports nothing here.
    ///
    /// [`OfferedRestart::UseValue`] supplies the send's results instead, of
    /// the generic's result kinds. The generic's dispatcher checks each one
    /// against the generic's declared result type (its class, or its
    /// `limited` range) as an entry check: a replacement that is not of it
    /// signals a `<type-error>`, and so on until one passes, so the code
    /// after the send only sees results that keep the generic's contract.
    /// A result known to be one particular callable is not offered: the
    /// dispatcher returns that callable, its only value.
    #[must_use]
    pub fn no_applicable_method(&self) -> Option<NoApplicableMethodReport> {
        if !self.failed_check {
            return None;
        }
        self.runtime.no_applicable_method_report(self.condition)
    }
}

/// A chooser's answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// Report the condition as unhandled; the run fails.
    Unhandled,
    /// Invoke a [`OfferedRestart::Handler`] with arguments matching its
    /// descriptor's parameters.
    Restart {
        /// The offered binding.
        token: BindingToken,
        /// One value per descriptor parameter.
        arguments: Vec<Value>,
    },
    /// Resume the signal with values matching [`OfferedRestart::UseValue`].
    UseValue(Vec<Value>),
    /// Install replacement code while every frame stays suspended, then be
    /// asked again. Frames keep the code they run; generic sends and retries
    /// take the newest. The unit must keep the run's runtime layout and each
    /// generic's signature and contract, as every unit whose code may still
    /// run has them, earlier runs' included ([`CodeUnit::check_replaces`]);
    /// otherwise it is refused and [`UnhandledCondition::refused`] says why.
    Install(Arc<CodeUnit>),
    /// Invoke [`OfferedRestart::Retry`] on the frame with this activation.
    Retry {
        /// The frame to retry.
        frame: ActivationId,
    },
    /// Unwind the run, running cleanups.
    Abort,
}

/// Chooses what to do about an unhandled condition.
pub trait ConditionChooser {
    /// Chooses a restart while the signalling computation is suspended.
    fn choose(&mut self, condition: &UnhandledCondition<'_>) -> Choice;
}

/// The default chooser: every unhandled condition is reported.
#[derive(Copy, Clone, Debug, Default)]
pub struct ReportUnhandled;

impl ConditionChooser for ReportUnhandled {
    fn choose(&mut self, _condition: &UnhandledCondition<'_>) -> Choice {
        Choice::Unhandled
    }
}
