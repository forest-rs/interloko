//! Soft typing over semantically valid modules.
//!
//! Semantic lowering ([`crate::sem`]) is Interloko's hard, bidirectional type
//! checker: it pushes expected result kinds into expressions, checks every
//! declared contract (parameters, results, slots, `let` annotations), and
//! rejects programs that violate them. What it accepts can still fail at run
//! time, because some checks are deliberately left to the runtime:
//!
//! - generic dispatch picks a method from the arguments' runtime classes and
//!   traps when no method applies;
//! - `as(<class>, value)` tests the value's runtime class and signals a
//!   `<type-error>` when the test fails;
//! - converting to a limited integer type signals a `<type-error>` when the
//!   value is out of range, and so does a value reaching a declaration of a
//!   `limited(<integer>, ...)` type (an argument, `let` binding, slot,
//!   constant or result; see [`sem::Expr::CheckRange`]); integer arithmetic
//!   signals one when it overflows.
//!
//! A `<type-error>`'s `use-value` restart continues the failed check with a
//! replacement of its expected type (a number of the operation's result
//! type, or an object a cast checks again), so a check that fails does not
//! end the code after it (see *Transfer* below).
//!
//! This module is the *soft* layer over those residual runtime checks, plus
//! the `instance?` tests whose outcome the types already decide. It
//! classifies each one, per check site, as an obligation with a
//! [`TypingVerdict`]:
//!
//! - [`TypingVerdict::Proved`]: the static types guarantee the check passes
//!   (for an `instance?` test: it is always true);
//! - [`TypingVerdict::Checked`]: the outcome depends on runtime values, so
//!   the runtime check is what decides;
//! - [`TypingVerdict::Refuted`]: the static types guarantee the check fails
//!   whenever it is reached (for an `instance?` test: it is always false).
//!
//! # Flow typing
//!
//! Verdicts come from an abstract interpreter over sem's structured IR. The
//! IR has no loops and no local assignment, so locals are immutable and no
//! control-flow graph is needed: the interpreter walks each body once, in
//! evaluation order, carrying an environment from locals and closure
//! captures to [`Type`]s of the shared lattice in
//! [`interloko_model::types`]. Slots are mutable, so slot reads are never
//! narrowed.
//!
//! - **Transfer.** Literals have singleton types (integers have point
//!   ranges), `make` has its exact class, a proved cast meets its operand
//!   with the target, and integer arithmetic uses interval arithmetic. A
//!   check that may fail yields every value of its expected type: a failed
//!   check signals a `<type-error>` whose `use-value` restart supplies a
//!   replacement of the expected type, so code after the check runs with a
//!   value that passes it but may be unrelated to the operand.
//!   A cast that may fail has its target type, and a result that may
//!   overflow has the full range of its kind. A call has its
//!   callee's declared or sem-inferred result, within a declared limited
//!   range, never a type derived from the callee's body, so a definition's
//!   typing depends only on its own body. A binding has its declared type,
//!   within its limited range. Anything else has the type of sem's category
//!   for the expression ([`sem::Module::kind_type`]).
//! - **Narrowing.** `instance?(x, C)` narrows `x` to `x ⊓ C` in the then
//!   branch and to `x ∖ C` in the else branch ([`Type::minus`] is
//!   conservative); a limited-type test does the same with ranges. Comparing
//!   an integer local with another integer narrows the local's range in both
//!   branches, also through a conversion of the local typing proved in range
//!   (one that may fail yields a `use-value` replacement, not the local's
//!   value). A condition whose type is one boolean, such as a literal,
//!   takes only its branch. A handler's condition local has the handler's
//!   class.
//! - **Joins.** `if` and `handle` join their branches. An expression that
//!   cannot produce a value (a block exit call, a narrowing that leaves no
//!   value) has type [`Type::bottom`]; code after it is
//!   unreachable, and an unreachable branch drops out of the join. A `block`
//!   joins its body's value with the arguments of every call to its exit
//!   procedure; when the exit procedure escapes, with the block's declared
//!   result.
//! - **Loops.** There are none yet. [`widen_to_fixpoint`] is the documented
//!   loop-head hook they will use.
//!
//! The pass checks every residual check against its expectation (a cast
//! target, the generic's method specializers, a limited integer range) with
//! [`Type::relate`]. Dispatch distributes over the members of a union, so a
//! call on `type-union(exactly <circle>, exactly <square>)` is proved when
//! each member has a method. A check that no value reaches is
//! [`TypingVerdict::Proved`] with [`TypingRule::Unreachable`].
//!
//! # Provenance
//!
//! Every fact the interpreter derives carries a [`ReasonId`] into its
//! definition's reason arena ([`DefinitionTyping::reasons`]). A
//! [`Reason`] names how the fact arose (a declaration, a literal, a
//! narrowing at a check site, a join...) and the reasons it was derived
//! from, so a decision's [`TypingDecision::because`] chains explain each
//! operand type back to its sources. Decisions also list the closed-world
//! assumptions they rely on ([`TypingDecision::assumes`]); a later slice
//! wires those into incremental invalidation.
//!
//! Besides decisions, each definition keeps the fact of every binding where
//! it is bound ([`DefinitionTyping::bindings`]), the reads a test narrowed
//! further ([`DefinitionTyping::uses`]), and its result
//! ([`DefinitionTyping::result`]). These reuse reasons already in the
//! arena, so they add one small entry per binding and narrowed read.
//! Expression facts are not stored: [`TypingArtifact::type_of`] answers a
//! site or span query by analyzing that one definition again.
//!
//! # Policy
//!
//! The pass is advisory. Its facts are recorded per enclosing definition
//! ([`DefinitionTyping`]), each keyed by the check's [`SiteId`] within that
//! definition. By default lowering ignores them and keeps every dynamic
//! check; under [`crate::lower::CodegenPolicy::Proved`] it drops or
//! specializes the checks proved here (see [`crate::lower`]). That is sound
//! only because a fact's reasons cite nothing beyond the owner's body but
//! contracts, which give a value sem's category for it (never a finer type
//! read from another body), and the closed-world assumptions the runtime
//! layout fixes. [`TypingConfig`] maps each check and
//! verdict to a [`TypingSeverity`]; the default reports refuted obligations
//! and statically decided `instance?` tests as warnings and keeps
//! runtime-checked ones silent. Changing the configuration re-derives
//! diagnostics from the recorded decisions without re-checking
//! ([`TypingArtifact::with_config`]).
//!
//! The pass runs only when semantic lowering produced no diagnostics, so it
//! never repeats or contradicts a hard semantic error.
//!
//! # Types
//!
//! A [`Type`] is a normalized union of atoms: class cones and exact
//! classes, `<integer>` ranges, booleans, metaobject singletons, and
//! callables (see [`interloko_model::types`]). [`sem::Module`] is the
//! [`ClassHierarchy`] the lattice is computed against. Because a
//! [`ValueKind`](sem::ValueKind) is a representation rather than a type,
//! `<i32>` and `<u32>` values are
//! `<integer>` ranges, `<double-float>` and `<single-float>` values have
//! exactly those closed classes, and abstract numeric classes (`<number>`,
//! `<real>`, `<rational>`, `<float>`) are only ever cones.
//!
//! Synthesis keeps one invariant, checked in debug builds: the type of an
//! expression is a subtype of `kind_type(expr_kind(expression))`. The soft
//! pass only narrows what sem allowed; it never contradicts it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use interloko_model::types::{Atom, ClassHierarchy, IntRange, Type};

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode, DiagnosticLabel, Severity};
use crate::identity::{DefinitionId, DefinitionKind};
use crate::sem::{
    self, CaptureId, ClassId, ClosureId, GenericFunctionId, LocalId, MethodId, SiteId,
};
use crate::syntax::Span;

mod flow;
mod query;

pub use query::{
    TypeQueryError, TypeReport, TypeSelector, TypeSubject, TypesAnchor, TypingDependency, UseType,
    no_type_labels,
};

/// Residual runtime check classified by the soft typing pass.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingCheck {
    /// Generic dispatch must find an applicable method for the arguments'
    /// runtime classes.
    Dispatch,
    /// A checked cast must find the target class in the value's runtime
    /// class chain.
    Cast,
    /// An `instance?` test of a class. The test never traps; it is
    /// [`TypingVerdict::Proved`] when always true and
    /// [`TypingVerdict::Refuted`] when always false.
    Instance,
    /// A value must be within a limited integer range: a numeric conversion
    /// (which signals a `<type-error>` when it is not) or an `instance?`
    /// test of a limited integer type (which is false when it is not).
    Range,
    /// Integer arithmetic must not overflow its kind, and a shift count must
    /// be in `0..=31`.
    Overflow,
}

impl TypingCheck {
    /// Every check in canonical order.
    pub const ALL: &'static [Self] = &[
        Self::Dispatch,
        Self::Cast,
        Self::Instance,
        Self::Range,
        Self::Overflow,
    ];

    /// Returns the stable label used in dumps, EOP records, and config names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Cast => "cast",
            Self::Instance => "instance",
            Self::Range => "range",
            Self::Overflow => "overflow",
        }
    }

    /// Parses a label written by [`Self::as_str`].
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|check| check.as_str() == label)
    }
}

/// Static classification of one runtime check.
///
/// The three outcomes are ordered by information: a join of disagreeing
/// verdicts is [`Self::Checked`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingVerdict {
    /// The static types guarantee the runtime check passes.
    Proved,
    /// The runtime check decides; static types allow both outcomes.
    Checked,
    /// The static types guarantee the runtime check fails when reached.
    Refuted,
}

impl TypingVerdict {
    /// Every verdict in canonical order.
    pub const ALL: &'static [Self] = &[Self::Proved, Self::Checked, Self::Refuted];

    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proved => "proved",
            Self::Checked => "checked",
            Self::Refuted => "refuted",
        }
    }

    /// Parses a label written by [`Self::as_str`].
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|verdict| verdict.as_str() == label)
    }

    /// Joins the verdicts of two alternatives that may each be taken.
    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Proved, Self::Proved) => Self::Proved,
            (Self::Refuted, Self::Refuted) => Self::Refuted,
            _ => Self::Checked,
        }
    }
}

/// How loudly one typing outcome is reported.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingSeverity {
    /// The decision is recorded but produces no diagnostic.
    Silent,
    /// The decision produces a warning diagnostic.
    Warning,
    /// The decision produces an error diagnostic.
    ///
    /// Typing errors mark the compilation as failed for tools that gate on
    /// errors, but lowering, tape emission, and execution do not consult them.
    Error,
}

impl TypingSeverity {
    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// Parses a label written by [`Self::as_str`].
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        [Self::Silent, Self::Warning, Self::Error]
            .into_iter()
            .find(|severity| severity.as_str() == label)
    }

    /// Returns the diagnostic severity to emit, or `None` when silent.
    #[must_use]
    pub const fn diagnostic_severity(self) -> Option<Severity> {
        match self {
            Self::Silent => None,
            Self::Warning => Some(Severity::Warning),
            Self::Error => Some(Severity::Error),
        }
    }
}

/// Reporting policy for one [`TypingCheck`].
///
/// Decisions about unreachable checks ([`TypingRule::Unreachable`]) are
/// never reported, whatever the policy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TypingPolicy {
    /// Severity for obligations the static types prove.
    pub proved: TypingSeverity,
    /// Severity for obligations the runtime check decides.
    pub checked: TypingSeverity,
    /// Severity for obligations that always fail when reached.
    pub refuted: TypingSeverity,
}

impl TypingPolicy {
    /// Creates a policy from the severities of runtime-checked and refuted
    /// obligations; proved obligations stay silent.
    #[must_use]
    pub const fn new(checked: TypingSeverity, refuted: TypingSeverity) -> Self {
        Self {
            proved: TypingSeverity::Silent,
            checked,
            refuted,
        }
    }

    /// Returns this policy with proved obligations reported at `proved`.
    #[must_use]
    pub const fn with_proved(mut self, proved: TypingSeverity) -> Self {
        self.proved = proved;
        self
    }

    /// Returns the severity this policy assigns to `verdict`.
    #[must_use]
    pub const fn severity(self, verdict: TypingVerdict) -> TypingSeverity {
        match verdict {
            TypingVerdict::Proved => self.proved,
            TypingVerdict::Checked => self.checked,
            TypingVerdict::Refuted => self.refuted,
        }
    }
}

/// Configuration for the soft typing pass.
///
/// Sessions own one configuration and pass it to every compile, the way they
/// pass [`crate::session::RuntimeMode`]. The default is [`Self::soft`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TypingConfig {
    /// Policy for generic dispatch obligations.
    pub dispatch: TypingPolicy,
    /// Policy for checked cast obligations.
    pub cast: TypingPolicy,
    /// Policy for `instance?` tests of a class.
    pub instance: TypingPolicy,
    /// Policy for limited integer range obligations.
    pub range: TypingPolicy,
    /// Policy for integer overflow and shift-count obligations.
    pub overflow: TypingPolicy,
}

impl TypingConfig {
    /// Named presets accepted by [`Self::preset`], in canonical order.
    pub const PRESETS: &'static [&'static str] = &["soft", "silent", "strict"];

    /// Warns about obligations that always fail and about `instance?` tests
    /// whose outcome the types decide; runtime-checked obligations stay
    /// silent.
    ///
    /// A decided `instance?` test warns because one of its branches is dead
    /// or the test is redundant. Flow typing derives the decision only from
    /// facts that hold on every execution, so the warning is never a false
    /// alarm about the program's behavior, only about its intent, and it
    /// usually marks a stale test after a type was tightened.
    #[must_use]
    pub const fn soft() -> Self {
        let policy = TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Warning);
        Self {
            dispatch: policy,
            cast: policy,
            instance: policy.with_proved(TypingSeverity::Warning),
            range: policy,
            overflow: policy,
        }
    }

    /// Records decisions without reporting any of them.
    #[must_use]
    pub const fn silent() -> Self {
        let policy = TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Silent);
        Self {
            dispatch: policy,
            cast: policy,
            instance: policy,
            range: policy,
            overflow: policy,
        }
    }

    /// Reports always-failing obligations as errors and runtime-checked
    /// obligations as warnings, with two exceptions.
    ///
    /// An `instance?` test never fails: a runtime-decided one is its normal
    /// use and stays silent, and a decided one is dead code rather than a
    /// failure, so it stays a warning. And almost every integer operation
    /// on declared types may overflow, mostly because `<integer>` is held
    /// in 64 bits, a limit of the current runtime rather than of the type;
    /// warning on each would bury every other report, so runtime-checked
    /// overflow stays silent while certain overflow is an error.
    #[must_use]
    pub const fn strict() -> Self {
        let policy = TypingPolicy::new(TypingSeverity::Warning, TypingSeverity::Error);
        Self {
            dispatch: policy,
            cast: policy,
            instance: TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Warning)
                .with_proved(TypingSeverity::Warning),
            range: policy,
            overflow: TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Error),
        }
    }

    /// Returns the preset named `name`, if any.
    #[must_use]
    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "soft" => Some(Self::soft()),
            "silent" => Some(Self::silent()),
            "strict" => Some(Self::strict()),
            _ => None,
        }
    }

    /// Returns the preset name matching this configuration, if any.
    #[must_use]
    pub fn preset_name(self) -> Option<&'static str> {
        Self::PRESETS
            .iter()
            .copied()
            .find(|name| Self::preset(name) == Some(self))
    }

    /// Returns the policy for `check`.
    #[must_use]
    pub const fn policy(self, check: TypingCheck) -> TypingPolicy {
        match check {
            TypingCheck::Dispatch => self.dispatch,
            TypingCheck::Cast => self.cast,
            TypingCheck::Instance => self.instance,
            TypingCheck::Range => self.range,
            TypingCheck::Overflow => self.overflow,
        }
    }

    /// Returns this configuration with `check` governed by `policy`.
    #[must_use]
    pub const fn with_policy(mut self, check: TypingCheck, policy: TypingPolicy) -> Self {
        match check {
            TypingCheck::Dispatch => self.dispatch = policy,
            TypingCheck::Cast => self.cast = policy,
            TypingCheck::Instance => self.instance = policy,
            TypingCheck::Range => self.range = policy,
            TypingCheck::Overflow => self.overflow = policy,
        }
        self
    }

    /// Returns the severity this configuration assigns to one outcome.
    #[must_use]
    pub const fn severity(self, check: TypingCheck, verdict: TypingVerdict) -> TypingSeverity {
        self.policy(check).severity(verdict)
    }

    /// Returns this configuration with one setting changed: one outcome of
    /// one check now reported at `severity`.
    #[must_use]
    pub const fn with_severity(
        self,
        check: TypingCheck,
        verdict: TypingVerdict,
        severity: TypingSeverity,
    ) -> Self {
        let mut policy = self.policy(check);
        match verdict {
            TypingVerdict::Proved => policy.proved = severity,
            TypingVerdict::Checked => policy.checked = severity,
            TypingVerdict::Refuted => policy.refuted = severity,
        }
        self.with_policy(check, policy)
    }

    /// Returns this configuration changed by `spec`: whitespace-separated
    /// words, each a preset name (see [`Self::PRESETS`]), which replaces the
    /// whole configuration, or a `check.verdict=severity` setting such as
    /// `cast.checked=warning`, applied in order. `strict
    /// overflow.checked=warning` is the strict preset with checked overflow
    /// reported.
    ///
    /// # Errors
    ///
    /// Returns the first word that is neither, or
    /// [`TypingConfigError::Empty`] when `spec` has no words.
    pub fn apply_spec(self, spec: &str) -> Result<Self, TypingConfigError> {
        let mut config = self;
        let mut words = spec.split_whitespace().peekable();
        if words.peek().is_none() {
            return Err(TypingConfigError::Empty);
        }
        for word in words {
            if let Some(preset) = Self::preset(word) {
                config = preset;
                continue;
            }
            let invalid = || TypingConfigError::InvalidWord(word.into());
            let (setting, severity) = word.split_once('=').ok_or_else(invalid)?;
            let (check, verdict) = setting.split_once('.').ok_or_else(invalid)?;
            let check = TypingCheck::from_label(check).ok_or_else(invalid)?;
            let verdict = TypingVerdict::from_label(verdict).ok_or_else(invalid)?;
            let severity = TypingSeverity::from_label(severity).ok_or_else(invalid)?;
            config = config.with_severity(check, verdict, severity);
        }
        Ok(config)
    }
}

/// Why [`TypingConfig::apply_spec`] rejected a specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypingConfigError {
    /// The specification has no words.
    Empty,
    /// This word is neither a preset nor a `check.verdict=severity`
    /// setting.
    InvalidWord(String),
}

impl fmt::Display for TypingConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => {
                f.write_str("empty typing policy; expected a preset or check.verdict=severity")
            }
            Self::InvalidWord(word) => write!(
                f,
                "`{word}` is neither a typing preset ({}) nor check.verdict=severity",
                TypingConfig::PRESETS.join("|")
            ),
        }
    }
}

impl Default for TypingConfig {
    fn default() -> Self {
        Self::soft()
    }
}

/// Expectation an obligation checks its operands against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypingExpectation {
    /// The value's runtime class must be `class` or one of its subclasses
    /// (casts and `instance?` tests of a class).
    Class(ClassId),
    /// The value must be a class object naming `class` or one of its
    /// subclasses (an indirect entry's check of a `class <c>` parameter).
    ClassObject(ClassId),
    /// Some method of `generic` must apply to the arguments' runtime classes.
    Dispatch {
        /// Generic function being called.
        generic: GenericFunctionId,
        /// Methods considered, in module order.
        methods: Vec<MethodId>,
        /// Parameter positions that carry boxed scalars, as decided by
        /// [`sem::Module::generic_param_boxes_scalars`]. Other `<object>` or
        /// unannotated positions carry raw object handles, and semantic
        /// lowering rejects scalar arguments there.
        boxed_params: Vec<usize>,
    },
    /// The value must be in this type: a conversion's target, a limited
    /// integer range, the range an integer operation's result must fit in,
    /// or the shift counts `0..=31`.
    Type(Type),
}

/// Rule that produced a decision's verdict.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingRule {
    /// A method applies to every class the arguments may have.
    DispatchCovered,
    /// Methods apply only to some runtime classes the arguments may have.
    DispatchNeedsRuntimeClass,
    /// No method can apply to any runtime class the arguments may have.
    DispatchNoApplicableMethod,
    /// The value's class is already the target class or a subclass of it.
    CastUpcast,
    /// The value may or may not have the target class; the runtime test
    /// decides.
    CastNarrowing,
    /// The value's runtime class can never be the target class.
    CastDisjoint,
    /// Every value the operand may have is an instance: the test is always
    /// true.
    InstanceAlways,
    /// The operand may or may not be an instance; the runtime test decides.
    InstanceSometimes,
    /// No value the operand may have is an instance: the test is always
    /// false.
    InstanceNever,
    /// Every value the operand may have is within the range.
    RangeWithin,
    /// The operand's values are partly within the range.
    RangeOverlaps,
    /// No value the operand may have is within the range.
    RangeDisjoint,
    /// A float converts to an integer only when it is integral and in
    /// range, which the runtime check decides.
    FloatToInteger,
    /// The operation has no runtime check: a widening conversion, wrapping
    /// arithmetic, or a bitwise function.
    CannotTrap,
    /// Interval arithmetic keeps the result within its kind.
    OverflowImpossible,
    /// Interval arithmetic allows results outside the kind.
    OverflowPossible,
    /// Interval arithmetic puts every result outside the kind.
    OverflowCertain,
    /// The shift count is always in `0..=31`.
    ShiftCountInRange,
    /// The shift count may be outside `0..=31`.
    ShiftCountUnchecked,
    /// The shift count is never in `0..=31`.
    ShiftCountOutOfRange,
    /// No value reaches the check: an earlier expression on every path to
    /// it cannot produce a value. The check vacuously passes.
    Unreachable,
}

impl TypingRule {
    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DispatchCovered => "dispatch-covered",
            Self::DispatchNeedsRuntimeClass => "dispatch-needs-runtime-class",
            Self::DispatchNoApplicableMethod => "dispatch-no-applicable-method",
            Self::CastUpcast => "cast-upcast",
            Self::CastNarrowing => "cast-narrowing",
            Self::CastDisjoint => "cast-disjoint",
            Self::InstanceAlways => "instance-always",
            Self::InstanceSometimes => "instance-sometimes",
            Self::InstanceNever => "instance-never",
            Self::RangeWithin => "range-within",
            Self::RangeOverlaps => "range-overlaps",
            Self::RangeDisjoint => "range-disjoint",
            Self::FloatToInteger => "float-to-integer",
            Self::CannotTrap => "cannot-trap",
            Self::OverflowImpossible => "overflow-impossible",
            Self::OverflowPossible => "overflow-possible",
            Self::OverflowCertain => "overflow-certain",
            Self::ShiftCountInRange => "shift-count-in-range",
            Self::ShiftCountUnchecked => "shift-count-unchecked",
            Self::ShiftCountOutOfRange => "shift-count-out-of-range",
            Self::Unreachable => "unreachable",
        }
    }
}

/// Index of a [`Reason`] in its definition's reason arena
/// ([`DefinitionTyping::reasons`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct ReasonId(
    /// Zero-based index into the arena.
    pub u32,
);

impl core::fmt::Display for ReasonId {
    /// Renders the id as `r<index>`, such as `r3`.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "r{}", self.0)
    }
}

/// Which branch of a test a narrowing applies to.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Branch {
    /// The branch taken when the test is true.
    Then,
    /// The branch taken when the test is false.
    Else,
}

impl Branch {
    /// Returns the stable label used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Then => "then",
            Self::Else => "else",
        }
    }
}

/// Whether a contract was written in source or inferred by sem.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ContractSource {
    /// The definition declares the contract.
    Declared,
    /// Sem inferred the contract from the definition's body.
    Inferred,
}

impl ContractSource {
    /// Returns the stable label used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Inferred => "inferred",
        }
    }
}

/// A closed-world assumption a decision relies on: a fact about the module
/// that holds now but that adding definitions could change.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Assumption {
    /// The generic's method set is the one the decision consulted.
    Generic(GenericFunctionId),
    /// The closed class gains no subclasses, so its cone is exactly it.
    Class(ClassId),
}

/// How a fact in a definition's flow analysis arose.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ReasonKind {
    /// The type sem gives the expression or binding: a parameter or `let`
    /// annotation, or the category of an expression the pass does not
    /// refine.
    Declared,
    /// A literal value.
    Literal,
    /// A value built here: `make`, or a closure.
    Constructed,
    /// The result contract of a callee, accessor, or constant.
    ///
    /// The fact's type is the category sem gives the value, which is what
    /// the callee's contract promises; it is never refined from the
    /// callee's body. A constant's is the type its value is inferred to
    /// have, which is sound to rely on because the value is lowered where
    /// it is read. Lowering under [`crate::lower::CodegenPolicy::Proved`]
    /// relies on both.
    Contract {
        /// Definition owning the contract.
        definition: DefinitionId,
        /// Whether the contract is declared or inferred.
        source: ContractSource,
    },
    /// A test narrowed a local or capture in one of its branches.
    Narrowed {
        /// The `instance?` site, or `None` for an integer comparison, which
        /// leaves no runtime check.
        site: Option<SiteId>,
        /// The branch the narrowing applies to.
        branch: Branch,
    },
    /// A value that passed, or a boolean decided by, the runtime check at
    /// `site`: a cast, a conversion, or an overflow check.
    Check {
        /// The check site.
        site: SiteId,
    },
    /// Branches or paths that each supply a value were joined.
    Joined,
    /// A handler bound its condition local.
    HandlerCondition {
        /// Class of conditions the handler accepts.
        specializer: ClassId,
    },
    /// The result of a send whose method typing selected at `site`: a
    /// built-in numeric method, whose result category the method set fixes.
    Dispatched {
        /// The dispatch site.
        site: SiteId,
    },
    /// Interval arithmetic on integer operands.
    Interval,
    /// A loop head widened its facts to reach a fixpoint.
    Widened,
    /// A call to a block's exit procedure: control leaves for the block's
    /// end, so the call produces no value.
    Exit,
    /// A parameter as a caller reaching the definition through a callable
    /// value passes it: any value its register can hold, which the entry
    /// checks before the body relies on the parameter's class.
    IndirectEntry,
    /// A closed-world assumption the decisions citing it rely on.
    ClosedWorld(Assumption),
}

impl ReasonKind {
    /// Returns the stable label used in dumps, without details.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Literal => "literal",
            Self::Constructed => "constructed",
            Self::Contract { .. } => "contract",
            Self::Narrowed { .. } => "narrowed",
            Self::Check { .. } => "check",
            Self::Dispatched { .. } => "dispatched",
            Self::Joined => "joined",
            Self::HandlerCondition { .. } => "handler-condition",
            Self::Interval => "interval",
            Self::Widened => "widened",
            Self::Exit => "exit",
            Self::IndirectEntry => "indirect-entry",
            Self::ClosedWorld(_) => "closed-world",
        }
    }
}

/// One step of provenance for a fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reason {
    /// How the fact arose.
    pub kind: ReasonKind,
    /// Source span of the expression or binding that produced the fact.
    pub span: Span,
    /// The type the fact gives.
    pub ty: Type,
    /// Reasons this one was derived from, in operand order.
    pub from: Vec<ReasonId>,
}

/// A binding whose type flow typing records: a parameter or `let` local
/// (including handler condition locals and block exit procedures), or a
/// closure's capture.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Binding {
    /// A local of the definition's own body.
    Local(LocalId),
    /// A local of a closure inside the definition. Closures number their
    /// locals separately from their enclosing body.
    ClosureLocal {
        /// The closure.
        closure: ClosureId,
        /// The local within the closure.
        local: LocalId,
    },
    /// A closure's capture, which holds its source's fact where the closure
    /// is created.
    Capture {
        /// The closure.
        closure: ClosureId,
        /// The capture within the closure.
        capture: CaptureId,
    },
}

/// The fact flow typing gives a binding where it is bound.
///
/// Locals are immutable, so this fact holds for the binding's whole scope;
/// a test can only narrow it further at particular uses ([`UseFact`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BindingFact {
    /// The binding.
    pub binding: Binding,
    /// The reason, in the owner's arena, whose type is the binding's type.
    pub reason: ReasonId,
}

/// A read of a binding whose type is narrower than its [`BindingFact`],
/// because a test, a join, or sem's own category at the use refined it.
///
/// Reads that see the binding fact itself are not recorded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UseFact {
    /// The binding read.
    pub binding: Binding,
    /// Source span of the read.
    pub span: Span,
    /// The reason, in the owner's arena, whose type the read has.
    pub reason: ReasonId,
}

/// One classified runtime check.
///
/// Decisions are configuration-independent facts; the reporting severity is
/// derived from a [`TypingConfig`] by [`TypingArtifact::severity`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypingDecision {
    /// Definition whose body contains the check.
    pub owner: DefinitionId,
    /// Check site within `owner`; with `owner` it names the check, stably
    /// across edits to other definitions.
    pub site: SiteId,
    /// Kind of runtime check.
    pub check: TypingCheck,
    /// Classification of the check.
    pub verdict: TypingVerdict,
    /// Rule that produced the verdict.
    pub rule: TypingRule,
    /// Source span of the checked expression.
    pub span: Span,
    /// Expectation supplied by the check's context.
    pub expected: TypingExpectation,
    /// Operand types: one per argument for dispatch, the value for a cast,
    /// `instance?` test, or conversion, and both operands (one for
    /// negation) for integer arithmetic, where the exact result that is
    /// checked against the expectation follows from them by interval
    /// arithmetic.
    pub actual: Vec<Type>,
    /// One reason per `actual` type, in the owner's reason arena.
    pub because: Vec<ReasonId>,
    /// Closed-world assumptions ([`ReasonKind::ClosedWorld`]) the verdict
    /// relies on, in the owner's reason arena.
    pub assumes: Vec<ReasonId>,
    /// Method a dispatch statically selects when every argument's exact
    /// runtime class is known.
    pub selected_method: Option<MethodId>,
}

/// Typing decisions for one top-level definition.
///
/// This is the unit of recomputation: a definition's decisions depend only on
/// its own body and the module's class, method, and contract tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionTyping {
    /// Stable identity of the definition.
    pub definition: DefinitionId,
    /// Kind of the definition.
    pub kind: DefinitionKind,
    /// Decisions in check-site order, which is evaluation order.
    pub decisions: Vec<TypingDecision>,
    /// Reason arena the decisions' and facts' [`ReasonId`]s index.
    pub reasons: Vec<Reason>,
    /// The fact of every binding the body binds, in evaluation order.
    pub bindings: Vec<BindingFact>,
    /// Reads of bindings narrower than their binding facts, in evaluation
    /// order.
    pub uses: Vec<UseFact>,
    /// The reason whose type is the body's result (a constant's value), or
    /// `None` when the definition has no body.
    pub result: Option<ReasonId>,
}

impl DefinitionTyping {
    /// Returns the reason `id` names, if it is in this definition's arena.
    #[must_use]
    pub fn reason(&self, id: ReasonId) -> Option<&Reason> {
        self.reasons.get(id.0 as usize)
    }

    /// Returns the fact recorded for `binding`, if the body binds it.
    #[must_use]
    pub fn binding(&self, binding: Binding) -> Option<&BindingFact> {
        self.bindings.iter().find(|fact| fact.binding == binding)
    }

    /// Returns the type of `binding` where it is bound.
    #[must_use]
    pub fn binding_type(&self, binding: Binding) -> Option<&Type> {
        self.binding(binding)
            .and_then(|fact| self.reason(fact.reason))
            .map(|reason| &reason.ty)
    }

    /// Returns the type the flow facts give a read of `binding` at `span`,
    /// or `None` when the body does not bind it.
    ///
    /// This is the read's [`UseFact`] when it has one, or else the binding's
    /// fact. A read's fact is never wider than sem's category at the use;
    /// where only that category made it narrower, this returns the fact
    /// before it was applied, so the result is what flow typing itself
    /// established.
    #[must_use]
    pub fn read_type(&self, binding: Binding, span: Span) -> Option<&Type> {
        let Some(read) = self
            .uses
            .iter()
            .find(|read| read.binding == binding && read.span == span)
        else {
            return self.binding_type(binding);
        };
        let reason = self.reason(read.reason)?;
        match (&reason.kind, reason.from.as_slice()) {
            (ReasonKind::Declared, [before]) if reason.span == span => {
                self.reason(*before).map(|before| &before.ty)
            }
            _ => Some(&reason.ty),
        }
    }

    /// Returns the type of the body's result.
    #[must_use]
    pub fn result_type(&self) -> Option<&Type> {
        self.result
            .and_then(|id| self.reason(id))
            .map(|reason| &reason.ty)
    }

    /// Returns what `decision`'s verdict depends on beyond this body: the
    /// contracts its operands' reason chains cite, in the order the chains
    /// reach them, then its closed-world assumptions.
    #[must_use]
    pub fn dependencies(&self, decision: &TypingDecision) -> Vec<TypingDependency> {
        let mut out = Vec::new();
        let mut seen = alloc::vec![false; self.reasons.len()];
        let mut queue = decision.because.clone();
        let mut next = 0;
        while let Some(id) = queue.get(next).copied() {
            next += 1;
            let Some(visited) = seen.get_mut(id.0 as usize) else {
                continue;
            };
            if core::mem::replace(visited, true) {
                continue;
            }
            let Some(reason) = self.reason(id) else {
                continue;
            };
            if let ReasonKind::Contract { definition, .. } = reason.kind {
                let dependency = TypingDependency::Contract(definition);
                if !out.contains(&dependency) {
                    out.push(dependency);
                }
            }
            // A fact derived from a closed world, such as a numeric send's
            // result, relies on it as the decision citing the fact does.
            if let ReasonKind::ClosedWorld(assumption) = reason.kind {
                let dependency = closed_world_dependency(assumption);
                if !out.contains(&dependency) {
                    out.push(dependency);
                }
            }
            queue.extend(reason.from.iter().copied());
        }
        for id in &decision.assumes {
            let Some(ReasonKind::ClosedWorld(assumption)) =
                self.reason(*id).map(|reason| &reason.kind)
            else {
                continue;
            };
            let dependency = closed_world_dependency(*assumption);
            if !out.contains(&dependency) {
                out.push(dependency);
            }
        }
        out
    }

    /// Renders the reason chain rooted at `id`, as in [`TypingArtifact::dump`].
    #[must_use]
    pub fn reason_chain(&self, module: &sem::Module, id: ReasonId) -> String {
        let mut out = String::new();
        write_reason_chain(&mut out, module, &self.reasons, id, 0);
        out
    }

    /// Returns what these decisions were derived from outside the
    /// definition's own body, read from the reason arena.
    ///
    /// The flow analysis records a [`ReasonKind::Contract`] for every callee,
    /// accessor, and constant whose contract it consults, and a
    /// [`ReasonKind::ClosedWorld`] for every assumption a verdict relies on,
    /// so the arena names every such input. The incremental graph turns them
    /// into dependency edges.
    #[must_use]
    pub fn inputs(&self) -> TypingInputs {
        let mut inputs = TypingInputs::default();
        for reason in &self.reasons {
            match reason.kind {
                ReasonKind::Contract { definition, .. } => inputs.contracts.push(definition),
                ReasonKind::ClosedWorld(Assumption::Generic(generic)) => {
                    inputs.generics.push(generic);
                }
                ReasonKind::ClosedWorld(Assumption::Class(class)) => inputs.classes.push(class),
                _ => {}
            }
        }
        inputs.contracts.sort_unstable();
        inputs.contracts.dedup();
        inputs.generics.sort_unstable_by_key(|generic| generic.0);
        inputs.generics.dedup();
        inputs.classes.sort_unstable_by_key(|class| class.0);
        inputs.classes.dedup();
        inputs
    }
}

/// The inputs one definition's typing decisions read beyond its own body:
/// see [`DefinitionTyping::inputs`]. Each list is sorted and free of
/// duplicates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TypingInputs {
    /// Definitions whose contracts a reason cites: callees, accessors,
    /// setters, and constants.
    pub contracts: Vec<DefinitionId>,
    /// Generics whose method set a verdict assumes is closed.
    pub generics: Vec<GenericFunctionId>,
    /// Closed classes a verdict assumes gain no subclasses.
    pub classes: Vec<ClassId>,
}

/// Whether the pass examined the module.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingStatus {
    /// Every definition was checked.
    Checked,
    /// Semantic lowering reported diagnostics, so the pass did not run.
    BlockedBySemanticDiagnostics {
        /// Number of semantic diagnostics.
        count: usize,
    },
}

impl TypingStatus {
    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checked => "checked",
            Self::BlockedBySemanticDiagnostics { .. } => "blocked-by-semantic-diagnostics",
        }
    }
}

/// Output of the soft typing pass for one module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypingArtifact {
    /// Configuration the diagnostics were derived with.
    pub config: TypingConfig,
    /// Whether the pass ran.
    pub status: TypingStatus,
    /// Per-definition decisions, in module order (functions, methods,
    /// constants).
    pub definitions: Vec<DefinitionTyping>,
    /// Diagnostics derived from `definitions` under `config`, in decision
    /// order.
    pub diagnostics: Vec<Diagnostic>,
}

impl TypingArtifact {
    /// Returns every decision in module order.
    pub fn decisions(&self) -> impl Iterator<Item = &TypingDecision> {
        self.definitions
            .iter()
            .flat_map(|definition| definition.decisions.iter())
    }

    /// Returns the typing of `definition`, if it was checked.
    #[must_use]
    pub fn definition(&self, definition: DefinitionId) -> Option<&DefinitionTyping> {
        self.definitions
            .iter()
            .find(|typed| typed.definition == definition)
    }

    /// Returns the decision for check site `site` of `owner`, if present.
    #[must_use]
    pub fn decision(&self, owner: DefinitionId, site: SiteId) -> Option<&TypingDecision> {
        self.definition(owner).and_then(|definition| {
            definition
                .decisions
                .iter()
                .find(|decision| decision.site == site)
        })
    }

    /// Returns the severity the artifact's configuration assigns to
    /// `decision`.
    #[must_use]
    pub const fn severity(&self, decision: &TypingDecision) -> TypingSeverity {
        if matches!(decision.rule, TypingRule::Unreachable) {
            return TypingSeverity::Silent;
        }
        self.config.severity(decision.check, decision.verdict)
    }

    /// Counts decisions with `verdict`.
    #[must_use]
    pub fn count(&self, verdict: TypingVerdict) -> usize {
        self.decisions()
            .filter(|decision| decision.verdict == verdict)
            .count()
    }

    /// Renders the artifact as a stable text dump: configuration, status,
    /// then one line per decision grouped by definition.
    ///
    /// Each decision line ends with `because=`, the reason chain of each
    /// actual type (separated by `; `), and `assumes=` when the verdict
    /// relies on closed-world assumptions. A chain renders a reason as
    /// `kind(details)@start..end type`, followed by ` <- ` and the reasons
    /// it was derived from (several in braces, separated by ` | `).
    #[must_use]
    pub fn dump(&self, module: &sem::Module) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "typing config={} status={} decisions={} proved={} checked={} refuted={}",
            config_label(self.config),
            self.status.as_str(),
            self.decisions().count(),
            self.count(TypingVerdict::Proved),
            self.count(TypingVerdict::Checked),
            self.count(TypingVerdict::Refuted),
        );
        for check in TypingCheck::ALL {
            let policy = self.config.policy(*check);
            let _ = writeln!(
                out,
                "  policy {} proved={} checked={} refuted={}",
                check.as_str(),
                policy.proved.as_str(),
                policy.checked.as_str(),
                policy.refuted.as_str()
            );
        }
        for definition in &self.definitions {
            if !definition.decisions.is_empty() {
                self.write_definition(&mut out, module, definition);
            }
        }
        out
    }

    /// Renders one definition's decisions in the format of [`Self::dump`].
    #[must_use]
    pub fn dump_definition(&self, module: &sem::Module, definition: DefinitionId) -> String {
        let mut out = String::new();
        match self.definition(definition) {
            Some(typed) => self.write_definition(&mut out, module, typed),
            None => {
                let _ = writeln!(
                    out,
                    "  definition {definition} {} untyped status={}",
                    definition_label(module, definition),
                    self.status.as_str()
                );
            }
        }
        out
    }

    fn write_definition(&self, out: &mut String, module: &sem::Module, typed: &DefinitionTyping) {
        let _ = writeln!(
            out,
            "  definition {} {}",
            typed.definition,
            definition_label(module, typed.definition)
        );
        for decision in &typed.decisions {
            let _ = write!(
                out,
                "    {} {} {} rule={} span={}..{} expected={} actual={} severity={}",
                decision.site,
                decision.check.as_str(),
                decision.verdict.as_str(),
                decision.rule.as_str(),
                decision.span.start,
                decision.span.end,
                expectation_label(module, &decision.expected),
                type_name_list(module, &decision.actual),
                self.severity(decision).as_str()
            );
            if !decision.because.is_empty() {
                let because = decision
                    .because
                    .iter()
                    .map(|reason| typed.reason_chain(module, *reason))
                    .collect::<Vec<_>>()
                    .join("; ");
                let _ = write!(out, " because={because}");
            }
            if !decision.assumes.is_empty() {
                let assumes = decision
                    .assumes
                    .iter()
                    .map(|reason| {
                        typed.reason(*reason).map_or_else(
                            || format!("{reason}"),
                            |reason| reason_label(module, reason),
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = write!(out, " assumes={assumes}");
            }
            out.push('\n');
        }
    }

    /// Re-derives diagnostics under `config` without re-checking.
    #[must_use]
    pub fn with_config(&self, module: &sem::Module, config: TypingConfig) -> Self {
        Self::from_definitions(module, config, self.status, self.definitions.clone())
    }

    fn from_definitions(
        module: &sem::Module,
        config: TypingConfig,
        status: TypingStatus,
        definitions: Vec<DefinitionTyping>,
    ) -> Self {
        let diagnostics = definitions
            .iter()
            .flat_map(|definition| {
                definition.decisions.iter().filter_map(move |decision| {
                    decision_diagnostic(module, config, definition, decision)
                })
            })
            .collect();
        Self {
            config,
            status,
            definitions,
            diagnostics,
        }
    }
}

/// Runs the soft typing pass over `module`.
#[must_use]
pub fn analyze(module: &sem::Module, config: TypingConfig) -> TypingArtifact {
    if !module.diagnostics.is_empty() {
        return TypingArtifact::from_definitions(
            module,
            config,
            TypingStatus::BlockedBySemanticDiagnostics {
                count: module.diagnostics.len(),
            },
            Vec::new(),
        );
    }
    let definitions = module
        .functions
        .iter()
        .map(|function| function.definition)
        .chain(module.methods.iter().map(|method| method.definition))
        .chain(module.constants.iter().map(|constant| constant.definition))
        .filter_map(|definition| check_definition(module, definition))
        .collect();
    TypingArtifact::from_definitions(module, config, TypingStatus::Checked, definitions)
}

/// Checks one function, method, or constant definition of `module`.
///
/// Returns `None` when `definition` names none of them.
#[must_use]
pub fn check_definition(
    module: &sem::Module,
    definition: DefinitionId,
) -> Option<DefinitionTyping> {
    run_definition(module, definition, None).map(|(typed, _)| typed)
}

/// Checks one definition and, with a probe, also returns the expression it
/// selects. The analysis is deterministic, so a probed run records the same
/// facts, with the same reason ids, as [`check_definition`].
fn run_definition(
    module: &sem::Module,
    definition: DefinitionId,
    probe: Option<flow::ProbeTarget>,
) -> Option<(DefinitionTyping, Option<flow::ProbeHit>)> {
    let mut analysis = flow::Analysis::new(module, definition);
    if let Some(target) = probe {
        analysis.probe(target);
    }
    let kind = if let Some(function) = module
        .functions
        .iter()
        .find(|function| function.definition == definition)
    {
        analysis.body(&function.locals, &function.params, &function.body);
        analysis.entry_checks(&function.locals, &function.entry_checks);
        DefinitionKind::Function
    } else if let Some(method) = module
        .methods
        .iter()
        .find(|method| method.definition == definition)
    {
        analysis.body(&method.locals, &method.params, &method.body);
        DefinitionKind::Method
    } else {
        let constant = module
            .constants
            .iter()
            .find(|constant| constant.definition == definition)?;
        let _ = analysis.detached(&constant.value);
        DefinitionKind::Constant
    };
    let finished = analysis.finish();
    Some((
        DefinitionTyping {
            definition,
            kind,
            decisions: finished.decisions,
            reasons: finished.reasons,
            bindings: finished.bindings,
            uses: finished.uses,
            result: finished.result,
        },
        finished.probe,
    ))
}

/// Synthesizes the static type of `expr` outside any flow context.
///
/// Locals and captures have the categories sem recorded at their use sites;
/// everything else follows the transfer rules of the flow analysis. The
/// result is `kind_type(expr_kind(expr))` narrowed by what the expression's
/// shape proves, and never wider than it.
#[must_use]
pub fn synthesize(module: &sem::Module, expr: &sem::Expr) -> Type {
    let owner = DefinitionId::new(
        module.source_file,
        DefinitionKind::Function,
        "<synthesize>",
        "",
    );
    flow::Analysis::new(module, owner).detached(expr)
}

/// Iterations [`widen_to_fixpoint`] runs before giving up on convergence.
pub const MAX_FIXPOINT_ITERATIONS: usize = 64;

/// Loop-head fixpoint hook for flow typing.
///
/// The IR has no loops yet, so the flow analysis never calls this; it is
/// the documented point where loop support will plug in. `entry` holds the
/// types of the loop-carried locals on entry, and `step` runs the loop body
/// once from head types and returns their types at the back edge. The head
/// is the join of the entry and every back edge, widened
/// ([`Type::widen`]) after each step so integer bounds jump to the
/// `<i32>`, `<u32>`, and 64-bit thresholds and the iteration terminates.
/// Facts a loop head produces carry [`ReasonKind::Widened`].
///
/// Returns the head types, or [`Type::top`] for every local if the
/// iteration has not converged after [`MAX_FIXPOINT_ITERATIONS`] steps.
pub fn widen_to_fixpoint<H: ClassHierarchy + ?Sized>(
    hierarchy: &H,
    entry: Vec<Type>,
    mut step: impl FnMut(&[Type]) -> Vec<Type>,
) -> Vec<Type> {
    let mut head = entry;
    for _ in 0..MAX_FIXPOINT_ITERATIONS {
        let back = step(&head);
        let next = head
            .iter()
            .zip(back.iter().chain(core::iter::repeat(&Type::top())))
            .map(|(head, back)| head.join(back, hierarchy).widen())
            .collect::<Vec<_>>();
        if next == head {
            return head;
        }
        head = next;
    }
    head.iter().map(|_| Type::top()).collect()
}

fn decision_diagnostic(
    module: &sem::Module,
    config: TypingConfig,
    typed: &DefinitionTyping,
    decision: &TypingDecision,
) -> Option<Diagnostic> {
    if matches!(decision.rule, TypingRule::Unreachable) {
        return None;
    }
    let severity = config
        .severity(decision.check, decision.verdict)
        .diagnostic_severity()?;
    let code = diagnostic_code(decision.check, decision.verdict);
    let diagnostic = match severity {
        Severity::Warning => Diagnostic::warning(decision.span, code),
        Severity::Error => Diagnostic::error(decision.span, code),
    };
    let actual = type_name_list(module, &decision.actual);
    let diagnostic = match &decision.expected {
        TypingExpectation::Dispatch { generic, .. } => diagnostic
            .with_arg(DiagnosticArg::text(
                "generic",
                generic_name(module, *generic),
            ))
            .with_arg(DiagnosticArg::text("arguments", actual)),
        TypingExpectation::Class(class) => diagnostic
            .with_arg(DiagnosticArg::text("expected", class_name(module, *class)))
            .with_arg(DiagnosticArg::text("actual", actual)),
        TypingExpectation::ClassObject(class) => diagnostic
            .with_arg(DiagnosticArg::text(
                "expected",
                format!("class {}", class_name(module, *class)),
            ))
            .with_arg(DiagnosticArg::text("actual", actual)),
        TypingExpectation::Type(expected) => {
            let diagnostic = diagnostic
                .with_arg(DiagnosticArg::text("expected", type_name(module, expected)))
                .with_arg(DiagnosticArg::text("actual", actual));
            match operation_name(module, typed, decision) {
                Some(operation) => diagnostic.with_arg(DiagnosticArg::text("operation", operation)),
                None => diagnostic,
            }
        }
    };
    let mut diagnostic = diagnostic.with_arg(DiagnosticArg::text("rule", decision.rule.as_str()));
    if decision.verdict == TypingVerdict::Refuted {
        for label in provenance_labels(module, typed, decision) {
            diagnostic = diagnostic.with_label(label);
        }
    }
    Some(diagnostic)
}

const fn diagnostic_code(check: TypingCheck, verdict: TypingVerdict) -> DiagnosticCode {
    use DiagnosticCode as C;
    match (check, verdict) {
        (TypingCheck::Dispatch, TypingVerdict::Proved) => C::TypingDispatchProved,
        (TypingCheck::Dispatch, TypingVerdict::Checked) => C::TypingDispatchRuntimeChecked,
        (TypingCheck::Dispatch, TypingVerdict::Refuted) => C::TypingDispatchNoApplicableMethod,
        (TypingCheck::Cast, TypingVerdict::Proved) => C::TypingCastAlwaysSucceeds,
        (TypingCheck::Cast, TypingVerdict::Checked) => C::TypingCastRuntimeChecked,
        (TypingCheck::Cast, TypingVerdict::Refuted) => C::TypingCastAlwaysFails,
        (TypingCheck::Instance, TypingVerdict::Proved) => C::TypingInstanceAlwaysTrue,
        (TypingCheck::Instance, TypingVerdict::Checked) => C::TypingInstanceRuntimeChecked,
        (TypingCheck::Instance, TypingVerdict::Refuted) => C::TypingInstanceAlwaysFalse,
        (TypingCheck::Range, TypingVerdict::Proved) => C::TypingRangeAlwaysWithin,
        (TypingCheck::Range, TypingVerdict::Checked) => C::TypingRangeRuntimeChecked,
        (TypingCheck::Range, TypingVerdict::Refuted) => C::TypingRangeAlwaysOutside,
        (TypingCheck::Overflow, TypingVerdict::Proved) => C::TypingOverflowImpossible,
        (TypingCheck::Overflow, TypingVerdict::Checked) => C::TypingOverflowRuntimeChecked,
        (TypingCheck::Overflow, TypingVerdict::Refuted) => C::TypingOverflowCertain,
    }
}

/// Names the operation of an overflow decision, such as `+` or
/// `shift-left`, from the expression at its site.
fn operation_name(
    module: &sem::Module,
    typed: &DefinitionTyping,
    decision: &TypingDecision,
) -> Option<String> {
    if decision.check != TypingCheck::Overflow {
        return None;
    }
    flow::operation_at(module, typed.definition, decision.site)
}

/// Most secondary labels a refutation draws from its reason chains.
const MAX_PROVENANCE_LABELS: usize = 3;

/// Secondary labels pointing at the sources of a refuted decision's operand
/// types: declarations, literals, constructions, narrowings, and checks,
/// nearest first.
fn provenance_labels(
    module: &sem::Module,
    typed: &DefinitionTyping,
    decision: &TypingDecision,
) -> Vec<DiagnosticLabel> {
    let mut labels: Vec<DiagnosticLabel> = Vec::new();
    let mut queue = decision.because.clone();
    let mut seen = Vec::new();
    while let Some(id) = (!queue.is_empty()).then(|| queue.remove(0)) {
        if labels.len() >= MAX_PROVENANCE_LABELS {
            break;
        }
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let Some(reason) = typed.reason(id) else {
            continue;
        };
        queue.extend(reason.from.iter().copied());
        let ty = type_name(module, &reason.ty);
        let message = match &reason.kind {
            ReasonKind::Declared => format!("declared as {ty} here"),
            ReasonKind::Literal => format!("literal of type {ty}"),
            ReasonKind::Constructed => format!("constructed as {ty} here"),
            ReasonKind::Contract { .. } => format!("result contract gives {ty}"),
            ReasonKind::Narrowed { branch, .. } => {
                format!("narrowed to {ty} in the {} branch", branch.as_str())
            }
            ReasonKind::Check { .. } => format!("checked as {ty} here"),
            ReasonKind::Dispatched { .. } => format!("the selected method gives {ty}"),
            ReasonKind::HandlerCondition { .. } => format!("handler binds {ty}"),
            ReasonKind::Interval => format!("computed as {ty}"),
            ReasonKind::Widened => format!("widened to {ty}"),
            ReasonKind::Exit => "control leaves here".into(),
            ReasonKind::IndirectEntry => format!("passed indirectly as {ty}"),
            ReasonKind::Joined | ReasonKind::ClosedWorld(_) => continue,
        };
        if reason.span == decision.span
            || reason.span.start == reason.span.end
            || labels.iter().any(|label| label.span == reason.span)
        {
            continue;
        }
        labels.push(DiagnosticLabel::secondary(reason.span, message));
    }
    labels
}

/// What relying on a closed-world `assumption` makes a decision depend on.
const fn closed_world_dependency(assumption: Assumption) -> TypingDependency {
    match assumption {
        Assumption::Generic(generic) => TypingDependency::Methods(generic),
        Assumption::Class(class) => TypingDependency::Class(class),
    }
}

/// Deepest reason chain a dump renders before eliding the rest.
const MAX_CHAIN_DEPTH: usize = 8;

fn write_reason_chain(
    out: &mut String,
    module: &sem::Module,
    reasons: &[Reason],
    id: ReasonId,
    depth: usize,
) {
    let Some(reason) = reasons.get(id.0 as usize) else {
        let _ = write!(out, "{id}");
        return;
    };
    out.push_str(&reason_label(module, reason));
    if reason.from.is_empty() {
        return;
    }
    out.push_str(" <- ");
    if depth >= MAX_CHAIN_DEPTH {
        out.push_str("...");
        return;
    }
    match reason.from.as_slice() {
        [parent] => write_reason_chain(out, module, reasons, *parent, depth + 1),
        parents => {
            out.push('{');
            for (index, parent) in parents.iter().enumerate() {
                if index > 0 {
                    out.push_str(" | ");
                }
                write_reason_chain(out, module, reasons, *parent, depth + 1);
            }
            out.push('}');
        }
    }
}

/// Renders one reason as `kind(details)@start..end type`.
fn reason_label(module: &sem::Module, reason: &Reason) -> String {
    let mut label = String::from(reason.kind.as_str());
    match &reason.kind {
        ReasonKind::Contract { source, .. } => {
            let _ = write!(label, "({})", source.as_str());
        }
        ReasonKind::Narrowed { site, branch } => match site {
            Some(site) => {
                let _ = write!(label, "({site},{})", branch.as_str());
            }
            None => {
                let _ = write!(label, "({})", branch.as_str());
            }
        },
        ReasonKind::Check { site } | ReasonKind::Dispatched { site } => {
            let _ = write!(label, "({site})");
        }
        ReasonKind::HandlerCondition { specializer } => {
            let _ = write!(label, "({})", class_name(module, *specializer));
        }
        ReasonKind::ClosedWorld(Assumption::Generic(generic)) => {
            let _ = write!(label, "(generic {})", generic_name(module, *generic));
            return label;
        }
        ReasonKind::ClosedWorld(Assumption::Class(class)) => {
            let _ = write!(label, "(class {})", class_name(module, *class));
            return label;
        }
        ReasonKind::Declared
        | ReasonKind::Literal
        | ReasonKind::Constructed
        | ReasonKind::Joined
        | ReasonKind::Interval
        | ReasonKind::Widened
        | ReasonKind::Exit
        | ReasonKind::IndirectEntry => {}
    }
    let _ = write!(
        label,
        "@{}..{} {}",
        reason.span.start,
        reason.span.end,
        type_name(module, &reason.ty)
    );
    label
}

/// Returns a source-level name for a synthesized type, such as `<circle>`,
/// `exactly <circle>`, or `<i32>`.
#[must_use]
pub fn type_name(module: &sem::Module, ty: &Type) -> String {
    format!("{}", ty.display(module))
}

/// Returns a source-level name for a list of synthesized types: the single
/// name for one type, a parenthesized list otherwise.
#[must_use]
pub fn type_name_list(module: &sem::Module, types: &[Type]) -> String {
    let names = types
        .iter()
        .map(|ty| type_name(module, ty))
        .collect::<Vec<_>>();
    match names.as_slice() {
        [name] => name.clone(),
        _ => format!("({})", names.join(", ")),
    }
}

/// Returns the preset name of `config`, or `custom`.
#[must_use]
pub fn config_label(config: TypingConfig) -> &'static str {
    config.preset_name().unwrap_or("custom")
}

/// Returns a source-level label for an expectation, such as `<circle>`,
/// `dispatch area methods=2`, or `<i32>`.
#[must_use]
pub fn expectation_label(module: &sem::Module, expected: &TypingExpectation) -> String {
    match expected {
        TypingExpectation::Class(class) => class_name(module, *class),
        TypingExpectation::ClassObject(class) => format!("class {}", class_name(module, *class)),
        TypingExpectation::Dispatch {
            generic,
            methods,
            boxed_params,
        } => {
            let mut label = format!(
                "dispatch {} methods={}",
                generic_name(module, *generic),
                methods.len()
            );
            if !boxed_params.is_empty() {
                let positions = boxed_params
                    .iter()
                    .map(|index| format!("{index}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let _ = write!(label, " boxed-params={positions}");
            }
            label
        }
        TypingExpectation::Type(ty) => type_name(module, ty),
    }
}

/// Returns a source-level label for a definition that has typing facts or
/// a contract, such as `function main`, `method area(<circle>)`, or
/// `generic area`.
#[must_use]
pub fn definition_label(module: &sem::Module, definition: DefinitionId) -> String {
    if let Some(function) = module
        .functions
        .iter()
        .find(|function| function.definition == definition)
    {
        return format!("function {}", function.name);
    }
    if let Some(method) = module
        .methods
        .iter()
        .find(|method| method.definition == definition)
    {
        let specializers = method
            .specializers
            .iter()
            .map(|specializer| {
                specializer.map_or_else(|| "?".into(), |class| class_name(module, class))
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!(
            "method {}({specializers})",
            generic_name(module, method.generic)
        );
    }
    if let Some(constant) = module
        .constants
        .iter()
        .find(|constant| constant.definition == definition)
    {
        return format!("constant {}", constant.name);
    }
    if let Some(generic) = module
        .generics
        .iter()
        .find(|generic| generic.definition == definition)
    {
        return format!("generic {}", generic.name);
    }
    format!("definition {definition}")
}

fn generic_name(module: &sem::Module, generic: GenericFunctionId) -> String {
    module
        .generics
        .get(generic.0 as usize)
        .map_or_else(|| format!("g{}", generic.0), |generic| generic.name.clone())
}

fn class_name(module: &sem::Module, class: ClassId) -> String {
    module
        .classes
        .get(class.0 as usize)
        .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone())
}

/// Values a method specializer accepts; a missing specializer accepts
/// every value.
fn specializer_type(module: &sem::Module, specializer: sem::MethodSpecializer) -> Type {
    specializer.map_or_else(Type::top, |class| Type::of(Atom::Cone(class), module))
}

/// Returns the type of the integers in `range`.
fn range_type(module: &sem::Module, range: IntRange) -> Type {
    Type::of(Atom::Integer(range), module)
}

#[cfg(test)]
mod tests;
