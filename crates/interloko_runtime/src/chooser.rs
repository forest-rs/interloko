//! The seam where an unhandled condition meets its embedding.
//!
//! When a signal finds no applicable handler, [`crate::driver`] stops before
//! anything unwinds and hands a [`ConditionChooser`] the condition, the live
//! frames and the restarts on offer, all as data. The chooser answers with a
//! [`Choice`]; the driver validates it against the restart's typed signature
//! and carries it out. A REPL break loop, a scripted chooser or a recorded
//! oracle are all choosers; [`ReportUnhandled`] is the default.

use alloc::vec::Vec;

use execution_tape::program::ValueType;
use execution_tape::value::{Obj, Value};
use execution_tape::vm::{ActivationId, PausedFrame};
use interloko_model::ClassId;

use crate::InterlokoHost;
use crate::control::{BindingToken, RestartDescriptor};

/// One initialized register of a suspended frame.
#[derive(Clone, Debug)]
pub struct RegisterSnapshot {
    /// Function-local register number.
    pub register: u32,
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
    /// Unwind the whole run, running every crossed cleanup.
    Abort,
}

/// A condition no live handler accepted, with the suspended computation.
pub struct UnhandledCondition<'a> {
    /// The signalled condition object.
    pub condition: Obj,
    /// Its exact class.
    pub class: ClassId,
    /// Runtime objects, classes and slots, for presenting the condition.
    pub runtime: &'a InterlokoHost,
    /// Live frames, innermost (the signaller) first. Nothing has unwound.
    pub frames: &'a [FrameSnapshot],
    /// Restarts on offer, innermost handler sites first, then the built-ins.
    pub restarts: &'a [OfferedRestart<'a>],
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
