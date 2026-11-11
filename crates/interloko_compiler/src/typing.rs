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
//! - `as(<class>, value)` tests the value's runtime class and traps when the
//!   test fails.
//!
//! This module is the *soft* layer over those residual runtime checks. It
//! classifies each one as an obligation with a [`TypingVerdict`]:
//!
//! - [`TypingVerdict::Proved`]: the static types guarantee the check passes;
//! - [`TypingVerdict::Checked`]: the outcome depends on runtime classes, so
//!   the runtime check is what decides;
//! - [`TypingVerdict::Refuted`]: the static types guarantee the check fails
//!   whenever it is reached.
//!
//! Verdicts are computed bidirectionally. Argument and operand types are
//! *synthesized* from facts sem already recorded ([`sem::Module::expr_kind`])
//! plus the exact runtime class when it is statically known
//! ([`StaticType`]). Each obligation then *checks* its operand against the
//! expectation supplied by its context (a cast target or the generic's method
//! specializers); checking distributes over `if` and handler branches and
//! joins the per-branch verdicts.
//!
//! The pass is advisory. Its facts are recorded per enclosing definition
//! ([`DefinitionTyping`]) and never feed lowering, tape emission, or the
//! runtime, which keep their own dynamic checks. [`TypingConfig`] maps each
//! check and verdict to a [`TypingSeverity`]; the default reports refuted
//! obligations as warnings and keeps runtime-checked ones silent. Changing
//! the configuration re-derives diagnostics from the recorded decisions
//! without re-checking ([`TypingArtifact::with_config`]).
//!
//! The pass runs only when semantic lowering produced no diagnostics, so it
//! never repeats or contradicts a hard semantic error.
//!
//! # Scalar value kinds
//!
//! The pass has no type representation of its own: [`StaticType`] is a
//! [`ValueKind`] plus an optional exact class. Scalars reach it through their
//! classes: [`ValueKind::F64`] is bounded by `<double-float>` via
//! [`sem::Module::value_kind_class`], and that class is exact because nothing
//! inherits from it. Abstract numeric classes (`<number>`, `<real>`,
//! `<rational>`, `<float>`) have built-in subclasses, so they are only ever upper bounds.
//! A new scalar kind needs the same class mapping in
//! [`sem::Module::value_kind_class`] and [`sem::Module::builtin_value_kind`];
//! a kind without a runtime class must also be placed in the class-less arm
//! of this module's specializer fit, which matches [`ValueKind`]
//! exhaustively for that reason.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode, Severity};
use crate::identity::{DefinitionId, DefinitionKind};
use crate::sem::{self, ClassId, ClassOrigin, GenericFunctionId, MethodId, ValueKind};
use crate::syntax::Span;

/// Residual runtime check classified by the soft typing pass.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingCheck {
    /// Generic dispatch must find an applicable method for the arguments'
    /// runtime classes.
    Dispatch,
    /// A checked cast must find the target class in the value's runtime
    /// class chain.
    Cast,
}

impl TypingCheck {
    /// Every check in canonical order.
    pub const ALL: &'static [Self] = &[Self::Dispatch, Self::Cast];

    /// Returns the stable label used in dumps, EOP records, and config names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Cast => "cast",
        }
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
/// Proved obligations are never reported.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TypingPolicy {
    /// Severity for obligations the runtime check decides.
    pub checked: TypingSeverity,
    /// Severity for obligations that always fail when reached.
    pub refuted: TypingSeverity,
}

impl TypingPolicy {
    /// Creates a policy from per-verdict severities.
    #[must_use]
    pub const fn new(checked: TypingSeverity, refuted: TypingSeverity) -> Self {
        Self { checked, refuted }
    }

    /// Returns the severity this policy assigns to `verdict`.
    #[must_use]
    pub const fn severity(self, verdict: TypingVerdict) -> TypingSeverity {
        match verdict {
            TypingVerdict::Proved => TypingSeverity::Silent,
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
}

impl TypingConfig {
    /// Named presets accepted by [`Self::preset`], in canonical order.
    pub const PRESETS: &'static [&'static str] = &["soft", "silent", "strict"];

    /// Warns about obligations that always fail; runtime-checked obligations
    /// stay silent.
    #[must_use]
    pub const fn soft() -> Self {
        let policy = TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Warning);
        Self {
            dispatch: policy,
            cast: policy,
        }
    }

    /// Records decisions without reporting any of them.
    #[must_use]
    pub const fn silent() -> Self {
        let policy = TypingPolicy::new(TypingSeverity::Silent, TypingSeverity::Silent);
        Self {
            dispatch: policy,
            cast: policy,
        }
    }

    /// Reports always-failing obligations as errors and runtime-checked
    /// obligations as warnings.
    #[must_use]
    pub const fn strict() -> Self {
        let policy = TypingPolicy::new(TypingSeverity::Warning, TypingSeverity::Error);
        Self {
            dispatch: policy,
            cast: policy,
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
        }
    }

    /// Returns this configuration with `check` governed by `policy`.
    #[must_use]
    pub const fn with_policy(mut self, check: TypingCheck, policy: TypingPolicy) -> Self {
        match check {
            TypingCheck::Dispatch => self.dispatch = policy,
            TypingCheck::Cast => self.cast = policy,
        }
        self
    }

    /// Returns the severity this configuration assigns to one outcome.
    #[must_use]
    pub const fn severity(self, check: TypingCheck, verdict: TypingVerdict) -> TypingSeverity {
        self.policy(check).severity(verdict)
    }
}

impl Default for TypingConfig {
    fn default() -> Self {
        Self::soft()
    }
}

/// Synthesized static type of one expression.
///
/// `kind` is the category semantic lowering recorded; for class-shaped kinds
/// it is an upper bound on the runtime class. `exact_class` is the runtime
/// class itself when it is statically known: a `make` of a known class, or a
/// value of a class that cannot be subclassed (built-in value classes and
/// host-backed classes).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StaticType {
    /// Semantic value category.
    pub kind: ValueKind,
    /// Exact runtime class, when statically known.
    pub exact_class: Option<ClassId>,
}

/// Expectation an obligation checks its operands against.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypingExpectation {
    /// The value's runtime class must be `class` or one of its subclasses.
    Class(ClassId),
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
}

/// Rule that produced a decision's verdict.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypingRule {
    /// A method applies to the static argument classes, and so to every
    /// runtime subclass of them.
    DispatchCovered,
    /// Methods apply only to some runtime classes the arguments may have.
    DispatchNeedsRuntimeClass,
    /// No method can apply to any runtime class the arguments may have.
    DispatchNoApplicableMethod,
    /// The value's class is already the target class or a subclass of it.
    CastUpcast,
    /// The value's class is a superclass of the target; the runtime test
    /// decides.
    CastNarrowing,
    /// The value's runtime class can never be the target class.
    CastDisjoint,
    /// Branches of a conditional operand reached different verdicts.
    BranchesDisagree,
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
            Self::BranchesDisagree => "branches-disagree",
        }
    }
}

/// One classified runtime check.
///
/// Decisions are configuration-independent facts; the reporting severity is
/// derived from a [`TypingConfig`] by [`TypingArtifact::severity`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypingDecision {
    /// Definition whose body contains the check.
    pub owner: DefinitionId,
    /// Zero-based ordinal of this decision within `owner`, in evaluation
    /// order. Stable across edits to other definitions.
    pub index: usize,
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
    /// Synthesized operand types: one per argument for dispatch, one value
    /// for a cast.
    pub actual: Vec<StaticType>,
    /// Method a dispatch statically selects when every argument's exact
    /// runtime class is known.
    pub selected_method: Option<MethodId>,
}

/// Typing decisions for one top-level definition.
///
/// This is the unit of recomputation: a definition's decisions depend only on
/// its own body and the module's class and method tables.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionTyping {
    /// Stable identity of the definition.
    pub definition: DefinitionId,
    /// Kind of the definition.
    pub kind: DefinitionKind,
    /// Decisions in evaluation order.
    pub decisions: Vec<TypingDecision>,
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

    /// Returns the decision `index` of `owner`, if present.
    #[must_use]
    pub fn decision(&self, owner: DefinitionId, index: usize) -> Option<&TypingDecision> {
        self.definitions
            .iter()
            .find(|definition| definition.definition == owner)
            .and_then(|definition| definition.decisions.get(index))
    }

    /// Returns the severity the artifact's configuration assigns to
    /// `decision`.
    #[must_use]
    pub const fn severity(&self, decision: &TypingDecision) -> TypingSeverity {
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
                "  policy {} checked={} refuted={}",
                check.as_str(),
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
        match self
            .definitions
            .iter()
            .find(|typed| typed.definition == definition)
        {
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
            let _ = writeln!(
                out,
                "    t{} {} {} rule={} span={}..{} expected={} actual={} severity={}",
                decision.index,
                decision.check.as_str(),
                decision.verdict.as_str(),
                decision.rule.as_str(),
                decision.span.start,
                decision.span.end,
                expectation_label(module, &decision.expected),
                static_type_name_list(module, &decision.actual),
                self.severity(decision).as_str()
            );
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
            .flat_map(|definition| definition.decisions.iter())
            .filter_map(|decision| decision_diagnostic(module, config, decision))
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
    let mut checker = Checker {
        module,
        owner: definition,
        decisions: Vec::new(),
    };
    let kind = if let Some(function) = module
        .functions
        .iter()
        .find(|function| function.definition == definition)
    {
        checker.block(&function.body);
        DefinitionKind::Function
    } else if let Some(method) = module
        .methods
        .iter()
        .find(|method| method.definition == definition)
    {
        checker.block(&method.body);
        DefinitionKind::Method
    } else {
        let constant = module
            .constants
            .iter()
            .find(|constant| constant.definition == definition)?;
        checker.expr(&constant.value);
        DefinitionKind::Constant
    };
    Some(DefinitionTyping {
        definition,
        kind,
        decisions: checker.decisions,
    })
}

/// Synthesizes the static type of `expr`.
#[must_use]
pub fn synthesize(module: &sem::Module, expr: &sem::Expr) -> StaticType {
    let kind = module.expr_kind(expr);
    StaticType {
        kind,
        exact_class: exact_class(module, expr, kind),
    }
}

/// Alternatives explored before a conditional operand collapses to its
/// synthesized type.
const MAX_ALTERNATIVES: usize = 8;

/// Argument-type tuples explored before a dispatch falls back to synthesized
/// argument types.
const MAX_DISPATCH_TUPLES: usize = 16;

struct Checker<'module> {
    module: &'module sem::Module,
    owner: DefinitionId,
    decisions: Vec<TypingDecision>,
}

impl Checker<'_> {
    fn block(&mut self, block: &sem::Block) {
        for binding in &block.lets {
            self.expr(&binding.value);
        }
        self.expr(&block.result);
    }

    fn expr(&mut self, expr: &sem::Expr) {
        match expr {
            sem::Expr::GenericCall {
                generic,
                args,
                span,
            } => {
                for arg in args {
                    self.expr(arg);
                }
                self.dispatch(*generic, args, *span);
            }
            sem::Expr::Cast { value, class, span } => {
                self.expr(value);
                self.cast(value, *class, *span);
            }
            sem::Expr::Closure { closure, .. } => {
                if let Some(closure) = self.module.closures.get(closure.0 as usize) {
                    self.block(&closure.body);
                }
            }
            _ => for_each_child(expr, |child| self.expr(child)),
        }
    }

    fn cast(&mut self, value: &sem::Expr, class: ClassId, span: Span) {
        let verdicts = alternatives(self.module, value)
            .into_iter()
            .map(|alternative| fit(self.module, alternative, Some(class)).verdict())
            .collect::<Vec<_>>();
        let (verdict, rule) = joined(TypingCheck::Cast, &verdicts);
        self.decisions.push(TypingDecision {
            owner: self.owner,
            index: self.decisions.len(),
            check: TypingCheck::Cast,
            verdict,
            rule,
            span,
            expected: TypingExpectation::Class(class),
            actual: vec![synthesize(self.module, value)],
            selected_method: None,
        });
    }

    fn dispatch(&mut self, generic: GenericFunctionId, args: &[sem::Expr], span: Span) {
        let methods = self
            .module
            .methods
            .iter()
            .filter(|method| method.generic == generic)
            .collect::<Vec<_>>();
        let actual = args
            .iter()
            .map(|arg| synthesize(self.module, arg))
            .collect::<Vec<_>>();
        let per_arg = args
            .iter()
            .map(|arg| alternatives(self.module, arg))
            .collect::<Vec<_>>();
        let tuple_count = per_arg
            .iter()
            .try_fold(1_usize, |count, options| count.checked_mul(options.len()))
            .unwrap_or(usize::MAX);
        let tuples = if tuple_count <= MAX_DISPATCH_TUPLES {
            cartesian(&per_arg)
        } else {
            vec![actual.clone()]
        };

        let verdicts = tuples
            .iter()
            .map(|tuple| {
                methods
                    .iter()
                    .filter(|method| method.specializers.len() == tuple.len())
                    .map(|method| {
                        method.specializers.iter().zip(tuple).fold(
                            Fit::Always,
                            |acc, (specializer, arg)| {
                                acc.meet(fit(self.module, *arg, *specializer))
                            },
                        )
                    })
                    .fold(Fit::Never, Fit::best)
                    .verdict()
            })
            .collect::<Vec<_>>();
        let (verdict, rule) = joined(TypingCheck::Dispatch, &verdicts);

        let selected_method = actual
            .iter()
            .map(|arg| arg.exact_class)
            .collect::<Option<Vec<_>>>()
            .and_then(
                |exact| match self.module.select_method_for_classes(generic, &exact) {
                    sem::MethodSelection::Selected(method) => Some(method),
                    sem::MethodSelection::None | sem::MethodSelection::Ambiguous => None,
                },
            );
        self.decisions.push(TypingDecision {
            owner: self.owner,
            index: self.decisions.len(),
            check: TypingCheck::Dispatch,
            verdict,
            rule,
            span,
            expected: TypingExpectation::Dispatch {
                generic,
                methods: methods.iter().map(|method| method.id).collect(),
                boxed_params: (0..args.len())
                    .filter(|index| self.module.generic_param_boxes_scalars(generic, *index))
                    .collect(),
            },
            actual,
            selected_method,
        });
    }
}

/// Joins per-alternative verdicts and names the rule that decided them.
fn joined(check: TypingCheck, verdicts: &[TypingVerdict]) -> (TypingVerdict, TypingRule) {
    let Some((first, rest)) = verdicts.split_first() else {
        return (TypingVerdict::Checked, TypingRule::BranchesDisagree);
    };
    if rest.iter().any(|verdict| verdict != first) {
        let verdict = rest.iter().fold(*first, |acc, verdict| acc.join(*verdict));
        return (verdict, TypingRule::BranchesDisagree);
    }
    let rule = match (check, first) {
        (TypingCheck::Dispatch, TypingVerdict::Proved) => TypingRule::DispatchCovered,
        (TypingCheck::Dispatch, TypingVerdict::Checked) => TypingRule::DispatchNeedsRuntimeClass,
        (TypingCheck::Dispatch, TypingVerdict::Refuted) => TypingRule::DispatchNoApplicableMethod,
        (TypingCheck::Cast, TypingVerdict::Proved) => TypingRule::CastUpcast,
        (TypingCheck::Cast, TypingVerdict::Checked) => TypingRule::CastNarrowing,
        (TypingCheck::Cast, TypingVerdict::Refuted) => TypingRule::CastDisjoint,
    };
    (*first, rule)
}

/// How an operand relates to one class expectation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Fit {
    /// Every runtime value of the operand satisfies the expectation.
    Always,
    /// Some runtime values may satisfy it.
    Maybe,
    /// No runtime value of the operand satisfies it.
    Never,
}

impl Fit {
    /// Combines expectations that must all hold.
    const fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Never, _) | (_, Self::Never) => Self::Never,
            (Self::Maybe, _) | (_, Self::Maybe) => Self::Maybe,
            (Self::Always, Self::Always) => Self::Always,
        }
    }

    /// Combines alternatives where any one suffices.
    const fn best(self, other: Self) -> Self {
        match (self, other) {
            (Self::Always, _) | (_, Self::Always) => Self::Always,
            (Self::Maybe, _) | (_, Self::Maybe) => Self::Maybe,
            (Self::Never, Self::Never) => Self::Never,
        }
    }

    /// Verdict for a check whose expectation relates to its operand this way.
    const fn verdict(self) -> TypingVerdict {
        match self {
            Self::Always => TypingVerdict::Proved,
            Self::Maybe => TypingVerdict::Checked,
            Self::Never => TypingVerdict::Refuted,
        }
    }
}

/// Relates an operand to a specializer or cast target.
///
/// `None` and `<object>` accept every value.
fn fit(module: &sem::Module, operand: StaticType, expected: Option<ClassId>) -> Fit {
    let Some(expected) = expected else {
        return Fit::Always;
    };
    if expected == sem::OBJECT_CLASS_ID {
        return Fit::Always;
    }
    if let Some(exact) = operand.exact_class {
        return if module.is_same_or_subclass(exact, expected) {
            Fit::Always
        } else {
            Fit::Never
        };
    }
    if let Some(bound) = module.value_kind_class(operand.kind) {
        return if module.is_same_or_subclass(bound, expected) {
            Fit::Always
        } else if module.classes_may_overlap(bound, expected) {
            Fit::Maybe
        } else {
            Fit::Never
        };
    }
    // Class-less categories. Listed exhaustively so a new value kind must
    // decide where it belongs.
    match operand.kind {
        ValueKind::Any | ValueKind::Error => Fit::Maybe,
        ValueKind::Bool | ValueKind::Callable | ValueKind::KnownCallable(_) => Fit::Never,
        ValueKind::I64
        | ValueKind::F64
        | ValueKind::String(_)
        | ValueKind::Symbol(_)
        | ValueKind::Vector(_)
        | ValueKind::Object(_)
        | ValueKind::ClassObject(_)
        | ValueKind::GenericFunctionObject(_)
        | ValueKind::MethodObject(_)
        | ValueKind::SlotDescriptorObject { .. } => Fit::Maybe,
    }
}

/// Check-mode operand types: conditional operands contribute one type per
/// branch, everything else its synthesized type.
fn alternatives(module: &sem::Module, expr: &sem::Expr) -> Vec<StaticType> {
    let mut out = Vec::new();
    collect_alternatives(module, expr, &mut out);
    if out.len() > MAX_ALTERNATIVES {
        return vec![synthesize(module, expr)];
    }
    out
}

fn collect_alternatives(module: &sem::Module, expr: &sem::Expr, out: &mut Vec<StaticType>) {
    if out.len() > MAX_ALTERNATIVES {
        return;
    }
    match expr {
        sem::Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            collect_alternatives(module, then_branch, out);
            collect_alternatives(module, else_branch, out);
        }
        sem::Expr::Handle { body, restart, .. } => {
            collect_alternatives(module, body, out);
            collect_alternatives(module, &restart.body, out);
        }
        sem::Expr::Block { body, .. } => collect_alternatives(module, &body.result, out),
        _ => out.push(synthesize(module, expr)),
    }
}

fn cartesian(per_arg: &[Vec<StaticType>]) -> Vec<Vec<StaticType>> {
    let mut tuples = vec![Vec::new()];
    for options in per_arg {
        let mut next = Vec::with_capacity(tuples.len().saturating_mul(options.len()));
        for tuple in &tuples {
            for option in options {
                let mut extended = tuple.clone();
                extended.push(*option);
                next.push(extended);
            }
        }
        tuples = next;
    }
    tuples
}

fn exact_class(module: &sem::Module, expr: &sem::Expr, kind: ValueKind) -> Option<ClassId> {
    let structural = match expr {
        sem::Expr::MakeObject { class_id, .. } => Some(*class_id),
        sem::Expr::Cast { value, .. } => synthesize(module, value).exact_class,
        sem::Expr::ConstantRef { constant, .. } => module
            .constants
            .get(constant.0 as usize)
            .and_then(|constant| synthesize(module, &constant.value).exact_class),
        sem::Expr::Values { values, .. } => match values.as_slice() {
            [value] => synthesize(module, value).exact_class,
            _ => None,
        },
        sem::Expr::If {
            then_branch,
            else_branch,
            ..
        } => same_exact_class(module, then_branch, else_branch),
        sem::Expr::Handle { body, restart, .. } => same_exact_class(module, body, &restart.body),
        sem::Expr::Block { body, .. } => synthesize(module, &body.result).exact_class,
        _ => None,
    };
    structural.or_else(|| {
        module
            .value_kind_class(kind)
            .filter(|class| class_is_sealed(module, *class))
    })
}

fn same_exact_class(module: &sem::Module, left: &sem::Expr, right: &sem::Expr) -> Option<ClassId> {
    let left = synthesize(module, left).exact_class?;
    let right = synthesize(module, right).exact_class?;
    (left == right).then_some(left)
}

/// Source classes cannot inherit from built-in value classes or host-backed
/// classes, so such a class is sealed when no other built-in class inherits
/// from it either; a value bounded by a sealed class has exactly that class.
///
/// The check is structural rather than a list of names: abstract built-ins
/// such as `<number>`, `<real>`, `<rational>`, and `<float>` have built-in subclasses and
/// so are never treated as exact, while concrete leaves such as `<integer>`
/// and `<double-float>` are.
fn class_is_sealed(module: &sem::Module, class: ClassId) -> bool {
    let closed = module.classes.get(class.0 as usize).is_some_and(|class| {
        matches!(
            class.origin,
            ClassOrigin::BuiltinValue { .. } | ClassOrigin::HostBacked { .. }
        )
    });
    closed
        && !module
            .classes
            .iter()
            .any(|other| other.superclass == Some(class))
}

fn for_each_child(expr: &sem::Expr, mut visit: impl FnMut(&sem::Expr)) {
    match expr {
        sem::Expr::VectorLiteral {
            elements: items, ..
        }
        | sem::Expr::Values { values: items, .. }
        | sem::Expr::IntrinsicCall { args: items, .. }
        | sem::Expr::HostCall { args: items, .. }
        | sem::Expr::DirectCall { args: items, .. }
        | sem::Expr::GenericCall { args: items, .. }
        | sem::Expr::FloatCall { args: items, .. } => items.iter().for_each(visit),
        sem::Expr::CallableCall { callable, args, .. } => {
            visit(callable);
            args.iter().for_each(visit);
        }
        sem::Expr::Convert { value: inner, .. }
        | sem::Expr::Negate { value: inner, .. }
        | sem::Expr::Signal {
            condition: inner, ..
        }
        | sem::Expr::ClassOf { object: inner, .. }
        | sem::Expr::InstanceOf { value: inner, .. }
        | sem::Expr::Cast { value: inner, .. }
        | sem::Expr::AccessorCall {
            receiver: inner, ..
        } => visit(inner),
        sem::Expr::MakeObject { class, fields, .. } => {
            visit(class);
            for field in fields {
                visit(&field.value);
            }
        }
        sem::Expr::SlotWrite {
            receiver, value, ..
        } => {
            visit(receiver);
            visit(value);
        }
        sem::Expr::Binary { lhs, rhs, .. }
        | sem::Expr::Compare { lhs, rhs, .. }
        | sem::Expr::MixedCompare { lhs, rhs, .. }
        | sem::Expr::ValueEqual { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        }
        sem::Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit(condition);
            visit(then_branch);
            visit(else_branch);
        }
        sem::Expr::Handle { body, restart, .. } => {
            visit(body);
            visit(&restart.body);
        }
        sem::Expr::Block { body, cleanup, .. } => {
            for block in core::iter::once(body).chain(cleanup) {
                for binding in &block.lets {
                    visit(&binding.value);
                }
                visit(&block.result);
            }
        }
        sem::Expr::ConstI64 { .. }
        | sem::Expr::ConstF64 { .. }
        | sem::Expr::ConstBool { .. }
        | sem::Expr::StringLiteral { .. }
        | sem::Expr::SymbolLiteral { .. }
        | sem::Expr::Local { .. }
        | sem::Expr::Capture { .. }
        | sem::Expr::ConstantRef { .. }
        | sem::Expr::ClassObject { .. }
        | sem::Expr::KnownCallable { .. }
        | sem::Expr::Closure { .. }
        | sem::Expr::GenericFunctionObject { .. }
        | sem::Expr::Error { .. } => {}
    }
}

fn decision_diagnostic(
    module: &sem::Module,
    config: TypingConfig,
    decision: &TypingDecision,
) -> Option<Diagnostic> {
    let severity = config
        .severity(decision.check, decision.verdict)
        .diagnostic_severity()?;
    let code = match (decision.check, decision.verdict) {
        (_, TypingVerdict::Proved) => return None,
        (TypingCheck::Dispatch, TypingVerdict::Checked) => {
            DiagnosticCode::TypingDispatchRuntimeChecked
        }
        (TypingCheck::Dispatch, TypingVerdict::Refuted) => {
            DiagnosticCode::TypingDispatchNoApplicableMethod
        }
        (TypingCheck::Cast, TypingVerdict::Checked) => DiagnosticCode::TypingCastRuntimeChecked,
        (TypingCheck::Cast, TypingVerdict::Refuted) => DiagnosticCode::TypingCastAlwaysFails,
    };
    let diagnostic = match severity {
        Severity::Warning => Diagnostic::warning(decision.span, code),
        Severity::Error => Diagnostic::error(decision.span, code),
    };
    let diagnostic = match &decision.expected {
        TypingExpectation::Dispatch { generic, .. } => diagnostic
            .with_arg(DiagnosticArg::text(
                "generic",
                generic_name(module, *generic),
            ))
            .with_arg(DiagnosticArg::text(
                "arguments",
                static_type_name_list(module, &decision.actual),
            )),
        TypingExpectation::Class(class) => diagnostic
            .with_arg(DiagnosticArg::text("expected", class_name(module, *class)))
            .with_arg(DiagnosticArg::text(
                "actual",
                static_type_name_list(module, &decision.actual),
            )),
    };
    Some(diagnostic.with_arg(DiagnosticArg::text("rule", decision.rule.as_str())))
}

/// Returns a source-level name for a synthesized type, such as `<circle>` or
/// `exactly <circle>`.
#[must_use]
pub fn static_type_name(module: &sem::Module, ty: StaticType) -> String {
    if let Some(exact) = ty.exact_class {
        // A sealed class already names exactly one runtime class.
        return if class_is_sealed(module, exact) {
            class_name(module, exact)
        } else {
            format!("exactly {}", class_name(module, exact))
        };
    }
    match ty.kind {
        ValueKind::Any => "<object>".into(),
        ValueKind::Bool => "<boolean>".into(),
        ValueKind::Callable | ValueKind::KnownCallable(_) => "<callable>".into(),
        ValueKind::Error => "<error>".into(),
        ValueKind::I64
        | ValueKind::F64
        | ValueKind::String(_)
        | ValueKind::Symbol(_)
        | ValueKind::Vector(_)
        | ValueKind::Object(_)
        | ValueKind::ClassObject(_)
        | ValueKind::GenericFunctionObject(_)
        | ValueKind::MethodObject(_)
        | ValueKind::SlotDescriptorObject { .. } => module
            .value_kind_class(ty.kind)
            .map_or_else(|| "<object>".into(), |class| class_name(module, class)),
    }
}

/// Returns a source-level name for a list of synthesized types: the single
/// name for one type, a parenthesized list otherwise.
#[must_use]
pub fn static_type_name_list(module: &sem::Module, types: &[StaticType]) -> String {
    let names = types
        .iter()
        .map(|ty| static_type_name(module, *ty))
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

/// Returns a source-level label for an expectation, such as `<circle>` or
/// `dispatch area`.
#[must_use]
pub fn expectation_label(module: &sem::Module, expected: &TypingExpectation) -> String {
    match expected {
        TypingExpectation::Class(class) => class_name(module, *class),
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
    }
}

/// Returns a source-level label for a typed definition, such as
/// `function main` or `method area(<circle>)`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grammar_frontend;
    use crate::identity::SourceFileId;
    use crate::{ast, sem};

    fn module(source: &str) -> sem::Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        sem::lower(&ast::lower(&tree))
    }

    const SHAPES: &str = "define class <shape> (<object>)\nend;\n\
         define class <circle> (<shape>)\nend;\n\
         define class <square> (<shape>)\nend;\n\
         define class <label> (<object>)\nend;\n\
         define generic area(x);\n\
         define method area(c :: <circle>)\n  1\nend;\n\
         define method area(s :: <square>)\n  2\nend;\n";

    fn decisions_for(source: &str) -> (sem::Module, TypingArtifact) {
        let module = module(source);
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let artifact = analyze(&module, TypingConfig::soft());
        (module, artifact)
    }

    fn main_decisions(module: &sem::Module, artifact: &TypingArtifact) -> Vec<TypingDecision> {
        let main = module
            .functions
            .iter()
            .find(|function| function.name == "main")
            .expect("main");
        artifact
            .definitions
            .iter()
            .find(|definition| definition.definition == main.definition)
            .map(|definition| definition.decisions.clone())
            .unwrap_or_default()
    }

    #[test]
    fn dispatch_on_a_covered_class_is_proved_and_selects_statically() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function main()\n  area(make(<circle>))\nend;"
        ));
        let decisions = main_decisions(&module, &artifact);
        assert_eq!(decisions.len(), 1);
        let decision = &decisions[0];
        assert_eq!(decision.check, TypingCheck::Dispatch);
        assert_eq!(decision.verdict, TypingVerdict::Proved);
        assert_eq!(decision.rule, TypingRule::DispatchCovered);
        assert_eq!(decision.selected_method, Some(module.methods[0].id));
        // `area` specializes only on source classes, so it never boxes.
        assert!(matches!(
            &decision.expected,
            TypingExpectation::Dispatch { boxed_params, .. } if boxed_params.is_empty()
        ));
        assert!(artifact.diagnostics.is_empty());
    }

    #[test]
    fn dispatch_on_a_superclass_is_runtime_checked_and_silent_by_default() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function measure(s :: <shape>)\n  area(s)\nend;\n\
             define function main()\n  measure(make(<circle>))\nend;"
        ));
        let decision = artifact
            .decisions()
            .find(|decision| decision.check == TypingCheck::Dispatch)
            .expect("dispatch decision");
        assert_eq!(decision.verdict, TypingVerdict::Checked);
        assert_eq!(decision.rule, TypingRule::DispatchNeedsRuntimeClass);
        assert_eq!(decision.selected_method, None);
        assert!(artifact.diagnostics.is_empty());

        let strict = artifact.with_config(&module, TypingConfig::strict());
        assert_eq!(strict.diagnostics.len(), 1);
        assert_eq!(
            strict.diagnostics[0].code,
            DiagnosticCode::TypingDispatchRuntimeChecked
        );
        assert_eq!(strict.diagnostics[0].severity, Severity::Warning);
        assert_eq!(strict.definitions, artifact.definitions);
    }

    #[test]
    fn dispatch_with_no_possible_method_is_refuted_and_warns() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function main()\n  \
             if 1 < 2 then area(make(<circle>)) else area(make(<shape>)) end\nend;"
        ));
        let decisions = main_decisions(&module, &artifact);
        let verdicts = decisions
            .iter()
            .map(|decision| decision.verdict)
            .collect::<Vec<_>>();
        assert_eq!(verdicts, [TypingVerdict::Proved, TypingVerdict::Refuted]);
        assert_eq!(decisions[1].rule, TypingRule::DispatchNoApplicableMethod);
        assert_eq!(artifact.diagnostics.len(), 1);
        let diagnostic = &artifact.diagnostics[0];
        assert_eq!(
            diagnostic.code,
            DiagnosticCode::TypingDispatchNoApplicableMethod
        );
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(diagnostic.span, decisions[1].span);
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.name == "arguments" && arg.value.render() == "exactly <shape>")
        );
    }

    #[test]
    fn dispatch_on_an_unrelated_class_is_refuted() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function name(l :: <label>)\n  area(l)\nend;\n\
             define function main()\n  0\nend;"
        ));
        let decision = artifact.decisions().next().expect("decision");
        assert_eq!(decision.verdict, TypingVerdict::Refuted);
        assert_eq!(static_type_name(&module, decision.actual[0]), "<label>");
    }

    #[test]
    fn casts_are_classified_as_upcast_narrowing_or_disjoint() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function up()\n  as(<shape>, make(<circle>))\nend;\n\
             define function down(s :: <shape>)\n  as(<circle>, s)\nend;\n\
             define function never()\n  as(<circle>, make(<shape>))\nend;\n\
             define function main()\n  0\nend;"
        ));
        let verdict_of = |name: &str| {
            let function = module
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("function");
            artifact
                .decision(function.definition, 0)
                .map(|decision| (decision.verdict, decision.rule))
                .expect("cast decision")
        };
        assert_eq!(
            verdict_of("up"),
            (TypingVerdict::Proved, TypingRule::CastUpcast)
        );
        assert_eq!(
            verdict_of("down"),
            (TypingVerdict::Checked, TypingRule::CastNarrowing)
        );
        assert_eq!(
            verdict_of("never"),
            (TypingVerdict::Refuted, TypingRule::CastDisjoint)
        );
        let codes = artifact
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(codes, [DiagnosticCode::TypingCastAlwaysFails]);
    }

    #[test]
    fn checking_distributes_over_conditional_operands() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function pick(s :: <shape>)\n  \
             as(<circle>, if 1 < 2 then make(<shape>) else s end)\nend;\n\
             define function main()\n  0\nend;"
        ));
        let pick = &module.functions[0];
        let decision = artifact.decision(pick.definition, 0).expect("decision");
        assert_eq!(decision.verdict, TypingVerdict::Checked);
        assert_eq!(decision.rule, TypingRule::BranchesDisagree);
    }

    #[test]
    fn closure_bodies_are_checked_under_their_enclosing_definition() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function main()\n  \
             let f :: <callable> = function () => <integer>\n    area(make(<shape>))\n  end;\n  0\nend;"
        ));
        let decisions = main_decisions(&module, &artifact);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].verdict, TypingVerdict::Refuted);
    }

    #[test]
    fn dump_lists_policy_and_decisions_by_definition() {
        let (module, artifact) = decisions_for(&format!(
            "{SHAPES}define function main()\n  area(make(<shape>))\nend;"
        ));
        let dump = artifact.dump(&module);
        assert!(dump.starts_with(
            "typing config=soft status=checked decisions=1 proved=0 checked=0 refuted=1\n"
        ));
        assert!(dump.contains("  policy dispatch checked=silent refuted=warning\n"));
        assert!(dump.contains(" function main\n"));
        assert!(dump.contains("t0 dispatch refuted rule=dispatch-no-applicable-method span="));
        assert!(
            dump.contains(
                "expected=dispatch area methods=2 actual=exactly <shape> severity=warning"
            )
        );
    }

    const FLOATS: &str = "define generic size-of(x);\n\
         define method size-of(x :: <double-float>)\n  1\nend;\n\
         define method size-of(x :: <integer>)\n  2\nend;\n\
         define generic magnitude(x);\n\
         define method magnitude(x :: <float>)\n  5\nend;\n\
         define generic whole(x);\n\
         define method whole(x :: <integer>)\n  x\nend;\n";

    fn verdicts_of(
        module: &sem::Module,
        artifact: &TypingArtifact,
        name: &str,
    ) -> Vec<TypingVerdict> {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function");
        artifact
            .definitions
            .iter()
            .find(|definition| definition.definition == function.definition)
            .map(|definition| {
                definition
                    .decisions
                    .iter()
                    .map(|decision| decision.verdict)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn float_values_have_exact_double_float_class_under_abstract_float() {
        let (module, artifact) = decisions_for(&format!(
            "{FLOATS}define function main()\n  \
             if 1 < 2 then size-of(2.5) + magnitude(2.5) else whole(2.5) end\nend;"
        ));
        let double_float = module
            .builtin_value_class(sem::BuiltinValueClass::DoubleFloat)
            .expect("double-float");
        let float = module
            .builtin_value_class(sem::BuiltinValueClass::Float)
            .expect("float");
        assert!(class_is_sealed(&module, double_float));
        assert!(!class_is_sealed(&module, float));
        let integer = module
            .builtin_value_class(sem::BuiltinValueClass::Integer)
            .expect("integer");
        let rational = module
            .builtin_value_class(sem::BuiltinValueClass::Rational)
            .expect("rational");
        assert!(class_is_sealed(&module, integer));
        assert!(!class_is_sealed(&module, rational));
        assert!(module.is_same_or_subclass(integer, rational));

        assert_eq!(
            verdicts_of(&module, &artifact, "main"),
            [
                TypingVerdict::Proved,
                TypingVerdict::Proved,
                TypingVerdict::Refuted
            ]
        );
        let refuted = artifact
            .decisions()
            .find(|decision| decision.verdict == TypingVerdict::Refuted)
            .expect("refuted dispatch");
        assert_eq!(refuted.actual[0].exact_class, Some(double_float));
        // `whole` specializes on `<integer>`, so its parameter carries boxed
        // scalars; the decision records sem's boxing choice.
        assert!(matches!(
            &refuted.expected,
            TypingExpectation::Dispatch { boxed_params, .. } if boxed_params == &[0]
        ));
        assert_eq!(
            static_type_name(&module, refuted.actual[0]),
            "<double-float>"
        );
    }

    #[test]
    fn float_casts_are_proved_checked_or_refuted() {
        let (module, artifact) = decisions_for(&format!(
            "{FLOATS}define function to-float(x :: <object>) => <double-float>\n  \
             as(<double-float>, x)\nend;\n\
             define function up(x :: <double-float>) => <float>\n  as(<float>, x)\nend;\n\
             define function never() => <double-float>\n  \
             as(<double-float>, as(<object>, \"text\"))\nend;\n\
             define function on-object(x :: <object>)\n  size-of(x)\nend;\n\
             define function main()\n  0\nend;"
        ));
        assert_eq!(
            verdicts_of(&module, &artifact, "to-float"),
            [TypingVerdict::Checked]
        );
        assert_eq!(
            verdicts_of(&module, &artifact, "up"),
            [TypingVerdict::Proved]
        );
        assert_eq!(
            verdicts_of(&module, &artifact, "never"),
            [TypingVerdict::Proved, TypingVerdict::Refuted]
        );
        assert_eq!(
            verdicts_of(&module, &artifact, "on-object"),
            [TypingVerdict::Checked]
        );
        let codes = artifact
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(codes, [DiagnosticCode::TypingCastAlwaysFails]);
    }

    #[test]
    fn semantic_errors_block_the_pass() {
        let module = module("define function main() => <integer>\n  #t\nend;");
        assert!(!module.diagnostics.is_empty());
        let artifact = analyze(&module, TypingConfig::soft());
        assert_eq!(
            artifact.status,
            TypingStatus::BlockedBySemanticDiagnostics {
                count: module.diagnostics.len()
            }
        );
        assert!(artifact.definitions.is_empty());
        assert!(artifact.diagnostics.is_empty());
    }

    #[test]
    fn configuration_presets_and_policies() {
        assert_eq!(TypingConfig::default(), TypingConfig::soft());
        for name in TypingConfig::PRESETS {
            let config = TypingConfig::preset(name).expect("preset");
            assert_eq!(config.preset_name(), Some(*name));
        }
        assert_eq!(TypingConfig::preset("loud"), None);
        let soft = TypingConfig::soft();
        assert_eq!(
            soft.severity(TypingCheck::Dispatch, TypingVerdict::Refuted),
            TypingSeverity::Warning
        );
        assert_eq!(
            soft.severity(TypingCheck::Cast, TypingVerdict::Checked),
            TypingSeverity::Silent
        );
        let custom = soft.with_policy(
            TypingCheck::Cast,
            TypingPolicy::new(TypingSeverity::Warning, TypingSeverity::Error),
        );
        assert_eq!(custom.preset_name(), None);
        assert_eq!(custom.dispatch, soft.dispatch);
        assert_eq!(
            custom.severity(TypingCheck::Cast, TypingVerdict::Refuted),
            TypingSeverity::Error
        );
        for verdict in TypingVerdict::ALL {
            assert_eq!(
                TypingConfig::strict().severity(TypingCheck::Dispatch, TypingVerdict::Proved),
                TypingSeverity::Silent
            );
            assert_eq!(verdict.join(*verdict), *verdict);
        }
        assert_eq!(
            TypingVerdict::Proved.join(TypingVerdict::Refuted),
            TypingVerdict::Checked
        );
    }

    #[test]
    fn decisions_are_independent_of_other_definitions() {
        let base = format!(
            "{SHAPES}define function measure(s :: <shape>)\n  area(s)\nend;\n\
             define function main()\n  area(make(<shape>))\nend;"
        );
        let (module, artifact) = decisions_for(&base);
        let measure = module.functions[0].definition;
        let alone = check_definition(&module, measure).expect("measure typing");
        assert_eq!(
            Some(&alone),
            artifact
                .definitions
                .iter()
                .find(|definition| definition.definition == measure)
        );
        assert_eq!(alone.kind, DefinitionKind::Function);
    }
}
