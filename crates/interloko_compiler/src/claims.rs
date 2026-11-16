//! Claim reports: one reviewable account per definition of what typing
//! established, what the generated code relies on, which checks stay at
//! run time and why, and what supports the result.
//!
//! A [`ClaimReport`] aggregates facts the compiler already records; it
//! derives nothing new about the program:
//!
//! - the definition's [`TypingDecision`]s, with their verdicts, rules,
//!   origins, reasons and closed-world assumptions ([`crate::typing`]);
//! - what lowering did with each check, from the lowered module's
//!   [`crate::lower::CheckCodegenRecord`]s ([`CheckCodegen`]);
//! - the contracts and sealed or closed worlds the decisions cite
//!   ([`TypingDependency`]), and the definition's own contract
//!   ([`interloko_model::contract`]);
//! - strict typing's policy and the fixes it suggests for a check it could
//!   not prove ([`typing::StrictFix`]).
//!
//! The report is the unit of review: a reader checks the claims, not the
//! lines. It separates a check typing *proved* from one generated code
//! *relies on* (dropped, specialized, inlined or linked because of the
//! proof), and for each check left at run time names why typing could not
//! prove it ([`ResidualCause`]), distinguishing a check the program asked
//! for, a declared type that genuinely admits failing values, an
//! unannotated input, an open class, analysis that widened, and a relation
//! typing does not track. Each suggested fix says what it costs
//! ([`typing::StrictFix::consequence`]): annotating a parameter proves the
//! body's check by moving it to the callers, which narrows the contract.
//!
//! A report describes one compilation. [`ClaimReport::basis`] fingerprints
//! the inputs it was derived from (source, typing configuration, codegen
//! policy), and a retained session stamps its revision on it
//! ([`ClaimReport::revision`]), so a holder can tell when it is stale.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};
use core::fmt::Write as _;

use interloko_model::contract::{Contract, ValueContract};
use interloko_model::types::{Atom, MAX_ATOMS, Type};

use crate::annotation::{AnnotationSite, AnnotationTarget};
use crate::identity::DefinitionId;
use crate::lower::{self, CheckCodegen, CodegenPolicy};
use crate::sem::{self, ClassId};
use crate::session::{StrictFixOffer, fix_offers, typing_dependency_object_id};
use crate::syntax::Span;
use crate::typing::{
    self, CheckOrigin, DefinitionTyping, ReasonId, ReasonKind, StrictPolicy, StrictSource,
    TypingArtifact, TypingCheck, TypingDecision, TypingDependency, TypingExpectation, TypingRule,
    TypingSeverity, TypingStatus, TypingVerdict,
};

/// How much a [`ClaimReport`] carries.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ClaimDetail {
    /// Counts, support, and the checks that stay at run time, each with why
    /// and its fixes: what a reviewer must look at.
    #[default]
    Summary,
    /// Also every proved site, and each site's reason chains and
    /// assumptions.
    Full,
}

impl ClaimDetail {
    /// Returns the stable label: `summary` or `full`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Full => "full",
        }
    }

    /// Parses a label written by [`Self::as_str`].
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "summary" => Some(Self::Summary),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// What typing and code generation claim about one function, method, or
/// constant, as of one compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimReport {
    /// The retained session revision the report describes, when a session
    /// that counts revisions produced it (`interloko_session::Session`);
    /// `None` from bare compiler artifacts. A report whose revision is not
    /// the session's current one is stale.
    pub revision: Option<u64>,
    /// Fingerprint of what the report was derived from: the source text,
    /// the typing configuration and the codegen policy. Two reports with
    /// the same basis describe the same compilation;
    /// [`crate::session::SessionArtifacts::claims_basis`] gives the current
    /// one.
    pub basis: u64,
    /// The definition.
    pub definition: DefinitionId,
    /// The report's EOP id, `claims:<definition>`.
    pub object: String,
    /// The definition as source names it, such as `function twice`.
    pub label: String,
    /// How much the report carries.
    pub detail: ClaimDetail,
    /// Whether typing ran; when it did not, nothing is established and
    /// every check stays at run time.
    pub typing: TypingStatus,
    /// The codegen policy the generated code was lowered under.
    pub codegen: CodegenPolicy,
    /// The definition's own contract: what its callers must pass and may
    /// rely on. `None` for a constant.
    pub contract: Option<ClaimContract>,
    /// What strict typing guarantees here, exactly.
    pub strict: StrictClaim,
    /// Counts over every check site in the definition.
    pub tally: ClaimTally,
    /// The contracts and closed worlds the decisions rely on, in first-use
    /// order.
    pub support: Vec<ClaimSupport>,
    /// Check sites in site order: under [`ClaimDetail::Summary`] those
    /// typing did not prove, under [`ClaimDetail::Full`] every one.
    pub sites: Vec<ClaimSite>,
}

/// A definition's own contract, as a claim report shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimContract {
    /// The contract's EOP id, `contract:<definition>`.
    pub object: String,
    /// The contract.
    pub contract: Contract,
    /// Each parameter as `name: type`, in call order.
    pub params: Vec<String>,
    /// Each result's type, in return order.
    pub results: Vec<String>,
    /// Decisions elsewhere whose reasons cite the contract: code compiled
    /// to rely on what it promises.
    pub relied_on_by: usize,
}

/// Counts over a definition's check sites.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ClaimTally {
    /// Check sites typing classified.
    pub sites: usize,
    /// Sites the static types prove, unreachable ones included.
    pub proved: usize,
    /// Sites the runtime check decides.
    pub checked: usize,
    /// Sites that always fail when reached.
    pub refuted: usize,
    /// Proved sites no value reaches.
    pub unreachable: usize,
    /// Proved sites the generated code relies on: their check is elided
    /// or specialized, or their send inlined or linked.
    pub relied: usize,
    /// Of [`Self::relied`] (and unreachable sites dropped with their
    /// branch), checks the generated code drops.
    pub elided: usize,
    /// Operations generated code keeps without their check.
    pub specialized: usize,
    /// Numeric sends replaced by their method's operation.
    pub inlined: usize,
    /// Sends that call their method through a relinkable link.
    pub linked: usize,
    /// Proved sites whose check the generated code still performs, as under
    /// [`CodegenPolicy::Off`].
    pub proved_kept: usize,
    /// Checks the generated code performs at run time: every reachable
    /// site whose codegen is [`CheckCodegen::Kept`].
    pub runtime: usize,
    /// Sites reported as errors, such as strict typing's.
    pub errors: usize,
}

/// What strict typing guarantees about one definition.
///
/// Strictness is not "check-free": a strict definition must prove the
/// checks the compiler inserts into it, but explicit `as(...)` casts,
/// `instance?` tests, the guards an indirect entry makes, and, unless the
/// policy asks for it, integer overflow stay runtime checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrictClaim {
    /// Why the definition is strict, or `None` when it is not.
    pub source: Option<StrictSource>,
    /// The session's strict policy, which decides whether overflow must be
    /// proved.
    pub policy: StrictPolicy,
    /// Sites strict typing would reject: its errors when the definition is
    /// strict, what `preview-strict` reports when it is not.
    pub unmet: usize,
    /// Unproved sites strict typing does not require proving, which stay
    /// runtime checks even in a strict definition.
    pub exempt: usize,
    /// The guarantee as a sentence, naming what is and is not required.
    pub guarantee: String,
}

/// Something outside the body that decisions rely on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimSupport {
    /// The dependency.
    pub dependency: TypingDependency,
    /// Its EOP id ([`crate::session::SessionArtifacts::dependency_object_id`]).
    pub object: String,
    /// What it is, such as `contract of function small` or the closed-world
    /// sentence `sealed abstract class <shape> has exactly direct
    /// subclasses {<circle>, <square>}`.
    pub description: String,
    /// Sites whose decision relies on it.
    pub sites: usize,
    /// Of those, sites the generated code relies on: a change that breaks
    /// it must be refused or recompiled, not only re-reported.
    pub relied: usize,
}

/// What a check site amounts to in generated code.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SiteStatus {
    /// Proved, and the generated code relies on the proof.
    Relied,
    /// Proved, but the generated code still performs the check.
    ProvedKept,
    /// The runtime check decides.
    Runtime,
    /// The check always fails when reached.
    Refuted,
    /// No value reaches the check.
    Unreachable,
}

impl SiteStatus {
    /// Returns the stable label: `relied`, `proved-kept`, `runtime`,
    /// `refuted`, or `unreachable`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Relied => "relied",
            Self::ProvedKept => "proved-kept",
            Self::Runtime => "runtime",
            Self::Refuted => "refuted",
            Self::Unreachable => "unreachable",
        }
    }
}

/// One check site of the definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimSite {
    /// The decision's EOP id, `typing:<definition>:s<n>`.
    pub id: String,
    /// The kind of check.
    pub check: TypingCheck,
    /// Who asked for it.
    pub origin: CheckOrigin,
    /// Its verdict.
    pub verdict: TypingVerdict,
    /// The rule that gave the verdict.
    pub rule: TypingRule,
    /// The checked expression.
    pub span: Span,
    /// What the generated code does with it.
    pub codegen: CheckCodegen,
    /// What it amounts to.
    pub status: SiteStatus,
    /// How it is reported.
    pub severity: TypingSeverity,
    /// Whether a strict definition must prove it under the session's
    /// policy.
    pub strict_required: bool,
    /// The expectation, with source names.
    pub expected: String,
    /// The operand types, with source names.
    pub actual: String,
    /// EOP ids of what the decision relies on beyond the body.
    pub depends_on: Vec<String>,
    /// Why the check stays at run time; `None` when typing proved it.
    pub residual: Option<Residual>,
    /// Each operand's reason chain; only under [`ClaimDetail::Full`].
    pub reasons: Vec<String>,
    /// The closed-world assumptions the verdict relies on, as sentences;
    /// only under [`ClaimDetail::Full`].
    pub assumes: Vec<String>,
}

/// Why a check stays at run time, and what could change that.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Residual {
    /// The cause.
    pub cause: ResidualCause,
    /// The cause in source terms.
    pub explanation: String,
    /// For [`ResidualCause::UnannotatedInput`], the annotation target that
    /// would give the input a written type, when one names it.
    pub input: Option<AnnotationTarget>,
    /// Edits that could prove the check, each with what it costs
    /// ([`typing::StrictFix::consequence`]). Empty for a check the program
    /// asked for.
    pub fixes: Vec<StrictFixOffer>,
}

/// Why typing could not prove a check.
///
/// The first two are not lost precision: the program asked for the check,
/// or its declared types genuinely admit values the check rejects. The rest
/// are, and say where the precision went.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ResidualCause {
    /// The program asked for the check: an explicit `as(...)`, an
    /// `instance?` test, or the guard of an indirect entry, which checks
    /// callers typing does not see.
    Requested(CheckOrigin),
    /// The check fails whenever it is reached.
    AlwaysFails,
    /// The written types admit values the check rejects: the check decides
    /// something the declarations leave open.
    Declared,
    /// An operand comes from a parameter, or a callee's result, whose type
    /// no annotation writes, so typing has only sem's inferred category.
    UnannotatedInput,
    /// An operand is a cone over an open class whose current subclasses
    /// would satisfy the check: sealing the class would let typing rely on
    /// them.
    OpenClass(ClassId),
    /// The analysis widened past a bound and lost the facts it needed.
    Widened(WideningLimit),
    /// The check needs a relation typing does not track.
    Unsupported(UnsupportedRelation),
}

impl ResidualCause {
    /// Returns the stable label, such as `declared`, `open-class`, or
    /// `widened(atoms)`.
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Requested(origin) => format!("requested({})", origin.as_str()),
            Self::AlwaysFails => "always-fails".into(),
            Self::Declared => "declared".into(),
            Self::UnannotatedInput => "unannotated-input".into(),
            Self::OpenClass(_) => "open-class".into(),
            Self::Widened(limit) => format!("widened({})", limit.as_str()),
            Self::Unsupported(relation) => format!("unsupported({})", relation.as_str()),
        }
    }

    /// Whether the cause is lost precision rather than the program's own
    /// choice or its declared types.
    #[must_use]
    pub const fn is_precision_loss(self) -> bool {
        matches!(
            self,
            Self::UnannotatedInput | Self::OpenClass(_) | Self::Widened(_) | Self::Unsupported(_)
        )
    }
}

/// Which bound the analysis widened past.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WideningLimit {
    /// A narrowing over a sealed hierarchy would have left more than
    /// [`MAX_ATOMS`] pieces, so it widened back to the cone.
    Atoms,
    /// A send's arguments split into more than
    /// [`typing::MAX_DISPATCH_TUPLES`] exact class combinations.
    DispatchTuples,
    /// A loop head widened its facts to reach a fixpoint
    /// ([`ReasonKind::Widened`]).
    LoopHead,
}

impl WideningLimit {
    /// Returns the stable label: `atoms`, `dispatch-tuples`, or `loop-head`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Atoms => "atoms",
            Self::DispatchTuples => "dispatch-tuples",
            Self::LoopHead => "loop-head",
        }
    }
}

/// Something a check needs that typing has no rule for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum UnsupportedRelation {
    /// Whether a float is integral, which converting it to an integer
    /// needs ([`TypingRule::FloatToInteger`]).
    FloatIntegrality,
    /// The operand is an expression flow typing does not refine, such as
    /// the result of a call through an untyped callable value, so it has
    /// only sem's category for it ([`ReasonKind::Declared`] with nothing
    /// it was derived from).
    UnrefinedExpression,
}

impl UnsupportedRelation {
    /// Returns the stable label: `float-integrality` or
    /// `unrefined-expression`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FloatIntegrality => "float-integrality",
            Self::UnrefinedExpression => "unrefined-expression",
        }
    }
}

/// Returns the EOP id of `definition`'s claim report, `claims:<definition>`.
#[must_use]
pub fn object_id(definition: DefinitionId) -> String {
    format!("claims:{definition}")
}

/// What a report is built from.
pub(crate) struct ClaimInputs<'a> {
    pub(crate) module: &'a sem::Module,
    pub(crate) typing: &'a TypingArtifact,
    pub(crate) lowered: Option<&'a lower::Module>,
    pub(crate) codegen: CodegenPolicy,
    pub(crate) sites: &'a [AnnotationSite],
    pub(crate) basis: u64,
}

impl ClaimInputs<'_> {
    fn codegen_of(&self, decision: &TypingDecision) -> CheckCodegen {
        self.lowered.map_or(CheckCodegen::Kept, |lowered| {
            lowered.check_codegen(decision.owner, decision.site)
        })
    }
}

/// The status of `decision` given what generated code does with it.
fn site_status(decision: &TypingDecision, codegen: CheckCodegen) -> SiteStatus {
    match decision.verdict {
        TypingVerdict::Proved if decision.rule == TypingRule::Unreachable => {
            SiteStatus::Unreachable
        }
        TypingVerdict::Proved if codegen == CheckCodegen::Kept => SiteStatus::ProvedKept,
        TypingVerdict::Proved => SiteStatus::Relied,
        TypingVerdict::Checked => SiteStatus::Runtime,
        TypingVerdict::Refuted => SiteStatus::Refuted,
    }
}

/// Builds the report for `definition`, or `None` when it names no
/// function, method, or constant.
pub(crate) fn build(
    inputs: &ClaimInputs<'_>,
    definition: DefinitionId,
    detail: ClaimDetail,
) -> Option<ClaimReport> {
    let module = inputs.module;
    let is_definition = module
        .functions
        .iter()
        .any(|function| function.definition == definition)
        || module
            .methods
            .iter()
            .any(|method| method.definition == definition)
        || module
            .constants
            .iter()
            .any(|constant| constant.definition == definition);
    if !is_definition {
        return None;
    }
    let typed = inputs.typing.definition(definition);
    let decisions = typed.map_or(&[][..], |typed| typed.decisions.as_slice());
    let policy = inputs.typing.config.strict;
    let source = inputs.typing.strictness(definition);

    let mut tally = ClaimTally::default();
    let mut support: Vec<ClaimSupport> = Vec::new();
    let mut sites = Vec::new();
    let (mut unmet, mut exempt) = (0, 0);
    for decision in decisions {
        let codegen = inputs.codegen_of(decision);
        let status = site_status(decision, codegen);
        let severity = inputs.typing.severity(decision);
        let strict_required = policy.requires(decision);
        tally.add(decision, codegen, status, severity);
        if policy.violation(decision).is_some() {
            unmet += 1;
        } else if decision.verdict != TypingVerdict::Proved && !strict_required {
            exempt += 1;
        }
        let dependencies = typed.map_or_else(Vec::new, |typed| typed.dependencies(decision));
        for dependency in &dependencies {
            let relied = status == SiteStatus::Relied;
            match support
                .iter_mut()
                .find(|entry| entry.dependency == *dependency)
            {
                Some(entry) => {
                    entry.sites += 1;
                    entry.relied += usize::from(relied);
                }
                None => support.push(ClaimSupport {
                    dependency: *dependency,
                    object: typing_dependency_object_id(module, *dependency),
                    description: dependency_description(module, *dependency),
                    sites: 1,
                    relied: usize::from(relied),
                }),
            }
        }
        let listed = detail == ClaimDetail::Full || decision.verdict != TypingVerdict::Proved;
        let (Some(typed), true) = (typed, listed) else {
            continue;
        };
        let residual =
            (decision.verdict != TypingVerdict::Proved).then(|| residual(inputs, typed, decision));
        let full = detail == ClaimDetail::Full;
        sites.push(ClaimSite {
            id: typing::decision_id(decision),
            check: decision.check,
            origin: decision.origin,
            verdict: decision.verdict,
            rule: decision.rule,
            span: decision.span,
            codegen,
            status,
            severity,
            strict_required,
            expected: typing::expectation_label(module, &decision.expected),
            actual: typing::type_name_list(module, &decision.actual),
            depends_on: dependencies
                .iter()
                .map(|dependency| typing_dependency_object_id(module, *dependency))
                .collect(),
            residual,
            reasons: if full {
                decision
                    .because
                    .iter()
                    .map(|reason| typed.reason_chain(module, *reason))
                    .collect()
            } else {
                Vec::new()
            },
            assumes: if full {
                decision
                    .assumes
                    .iter()
                    .filter_map(|reason| match typed.reason(*reason)?.kind {
                        ReasonKind::ClosedWorld(assumption) => {
                            Some(typing::assumption_label(module, assumption))
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                Vec::new()
            },
        });
    }

    Some(ClaimReport {
        revision: None,
        basis: inputs.basis,
        definition,
        object: object_id(definition),
        label: typing::definition_label(module, definition),
        detail,
        typing: inputs.typing.status,
        codegen: inputs.codegen,
        contract: own_contract(inputs, definition),
        strict: StrictClaim {
            source,
            policy,
            unmet,
            exempt,
            guarantee: strict_guarantee(module, source, policy),
        },
        tally,
        support,
        sites,
    })
}

impl ClaimTally {
    fn add(
        &mut self,
        decision: &TypingDecision,
        codegen: CheckCodegen,
        status: SiteStatus,
        severity: TypingSeverity,
    ) {
        self.sites += 1;
        match decision.verdict {
            TypingVerdict::Proved => self.proved += 1,
            TypingVerdict::Checked => self.checked += 1,
            TypingVerdict::Refuted => self.refuted += 1,
        }
        match status {
            SiteStatus::Unreachable => self.unreachable += 1,
            SiteStatus::Relied => self.relied += 1,
            SiteStatus::ProvedKept => self.proved_kept += 1,
            SiteStatus::Runtime | SiteStatus::Refuted => {}
        }
        match codegen {
            CheckCodegen::Kept if status != SiteStatus::Unreachable => self.runtime += 1,
            CheckCodegen::Elided => self.elided += 1,
            CheckCodegen::Specialized => self.specialized += 1,
            CheckCodegen::Inlined => self.inlined += 1,
            CheckCodegen::Linked => self.linked += 1,
            _ => {}
        }
        if severity == TypingSeverity::Error {
            self.errors += 1;
        }
    }
}

fn own_contract(inputs: &ClaimInputs<'_>, definition: DefinitionId) -> Option<ClaimContract> {
    let module = inputs.module;
    let contract = module.contract(definition)?;
    let names = crate::session::param_names(module, definition);
    let params = contract
        .params
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let ty = value_label(module, value);
            match names.get(index) {
                Some(name) => format!("{name}: {ty}"),
                None => ty,
            }
        })
        .collect();
    let results = contract
        .results
        .iter()
        .map(|value| value_label(module, value))
        .collect();
    Some(ClaimContract {
        object: format!("contract:{definition}"),
        relied_on_by: inputs.typing.contract_dependents(definition).len(),
        contract,
        params,
        results,
    })
}

fn value_label(module: &sem::Module, value: &ValueContract) -> String {
    format!("{}", value.display(module))
}

fn dependency_description(module: &sem::Module, dependency: TypingDependency) -> String {
    match dependency {
        TypingDependency::Contract(definition) => {
            format!(
                "contract of {}",
                typing::definition_label(module, definition)
            )
        }
        TypingDependency::Methods(generic) => {
            typing::assumption_label(module, typing::Assumption::Generic(generic))
        }
        TypingDependency::Class(class) => {
            typing::assumption_label(module, typing::Assumption::Class(class))
        }
        TypingDependency::Sealed(class) => {
            typing::assumption_label(module, typing::Assumption::Sealed(class))
        }
    }
}

/// The strict guarantee as a sentence: what a strict definition must prove
/// under `policy`, and which runtime checks it keeps regardless.
fn strict_guarantee(
    module: &sem::Module,
    source: Option<StrictSource>,
    policy: StrictPolicy,
) -> String {
    let required = if policy.overflow {
        "every check the compiler inserts is proved (send dispatch, implicit casts, range checks, integer overflow)"
    } else {
        "every check the compiler inserts is proved (send dispatch, implicit casts, range checks)"
    };
    let kept = if policy.overflow {
        "explicit as(...) casts, instance? tests and indirect-entry guards stay runtime checks"
    } else {
        "explicit as(...) casts, instance? tests, indirect-entry guards and integer overflow (strict.overflow=runtime) stay runtime checks"
    };
    match source {
        Some(source) => format!("strict ({}): {required}; {kept}", source.describe(module)),
        None => format!("not strict: nothing is required; if strict, {required}; {kept}"),
    }
}

/// Why typing could not prove `decision`, a check it left to the runtime.
fn residual(
    inputs: &ClaimInputs<'_>,
    typed: &DefinitionTyping,
    decision: &TypingDecision,
) -> Residual {
    let module = inputs.module;
    let explain = |cause: ResidualCause, explanation: String| Residual {
        cause,
        explanation,
        input: None,
        fixes: Vec::new(),
    };
    if decision.verdict == TypingVerdict::Refuted {
        return explain(
            ResidualCause::AlwaysFails,
            format!(
                "no value {} may have passes the check ({})",
                typing::type_name_list(module, &decision.actual),
                decision.rule.as_str()
            ),
        );
    }
    match decision.origin {
        CheckOrigin::Explicit => {
            return explain(
                ResidualCause::Requested(decision.origin),
                "source asked for the check with as(...)".into(),
            );
        }
        CheckOrigin::Test => {
            return explain(
                ResidualCause::Requested(decision.origin),
                "an instance? test answers its question at run time".into(),
            );
        }
        CheckOrigin::Entry => {
            return explain(
                ResidualCause::Requested(decision.origin),
                "the entry guards the body against callers reaching it through a callable value, which typing does not see".into(),
            );
        }
        CheckOrigin::Implicit => {}
    }
    let fixes = fix_offers(module, typed, decision, inputs.sites);
    let with_fixes = |cause: ResidualCause, explanation: String| Residual {
        cause,
        explanation,
        input: None,
        fixes: fixes.clone(),
    };
    if decision.rule == TypingRule::FloatToInteger {
        return with_fixes(
            ResidualCause::Unsupported(UnsupportedRelation::FloatIntegrality),
            "typing does not track whether a float is integral, which converting it to an integer needs".into(),
        );
    }
    if decision.check == TypingCheck::Dispatch {
        let count = typing::dispatch_tuple_count(module, &decision.actual);
        if count > typing::MAX_DISPATCH_TUPLES {
            return with_fixes(
                ResidualCause::Widened(WideningLimit::DispatchTuples),
                format!(
                    "the arguments split into {count} combinations of sealed pieces, more than the {} typing distributes a send over, so it checks the whole types instead",
                    typing::MAX_DISPATCH_TUPLES
                ),
            );
        }
    }
    // The operands that fall short of what the check needs; every operand
    // when that cannot be told apart, as for overflow, or for a send whose
    // methods' specializers join to more than a union holds.
    let mut operands: Vec<usize> = typing::required_types(module, decision)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    if operands.is_empty() {
        operands = (0..decision.actual.len()).collect();
    }
    for index in &operands {
        let (Some(actual), Some(reason)) =
            (decision.actual.get(*index), decision.because.get(*index))
        else {
            continue;
        };
        let chain = reason_chain(typed, *reason);
        if chain.iter().any(|kind| matches!(kind, ReasonKind::Widened)) {
            return with_fixes(
                ResidualCause::Widened(WideningLimit::LoopHead),
                "a loop head widened the operand's facts to reach a fixpoint".into(),
            );
        }
        if let Some(class) = oversized_sealed_cone(module, actual)
            && let Some(test) = earlier_test(typed, decision, *reason)
        {
            return with_fixes(
                ResidualCause::Widened(WideningLimit::Atoms),
                format!(
                    "the test {} could not narrow the sealed {}: what it leaves has more than {MAX_ATOMS} pieces, so typing widened it back to the whole cone",
                    typing::decision_id(test),
                    class_name(module, class)
                ),
            );
        }
        let unrefined = typed.reason(*reason).is_some_and(|reason| {
            matches!(reason.kind, ReasonKind::Declared) && reason.from.is_empty()
        }) && !typed.bindings.iter().any(|fact| fact.reason == *reason);
        if unrefined {
            return with_fixes(
                ResidualCause::Unsupported(UnsupportedRelation::UnrefinedExpression),
                format!(
                    "typing has no rule that refines the operand's expression, so it knows only sem's category for it ({})",
                    typing::type_name(module, actual)
                ),
            );
        }
    }
    if let Some(class) = fixes.iter().find_map(|offer| match offer.fix {
        typing::StrictFix::Seal { class, .. } => Some(class),
        _ => None,
    }) {
        return with_fixes(
            ResidualCause::OpenClass(class),
            format!(
                "{} is open: typing cannot rely on its current subclasses, which would satisfy the check",
                class_name(module, class)
            ),
        );
    }
    for index in &operands {
        let Some(reason) = decision.because.get(*index) else {
            continue;
        };
        if let Some((input, target)) = unannotated_input(inputs, typed, *reason) {
            let mut residual = with_fixes(
                ResidualCause::UnannotatedInput,
                format!(
                    "{input} has no written type, so typing knows only what sem inferred ({})",
                    decision
                        .actual
                        .get(*index)
                        .map_or_else(String::new, |ty| typing::type_name(module, ty))
                ),
            );
            residual.input = target;
            return residual;
        }
    }
    let actual = typing::type_name_list(module, &decision.actual);
    let explanation = match &decision.expected {
        _ if decision.check == TypingCheck::Overflow => format!(
            "the operands' declared types {actual} allow a result outside {}",
            typing::expectation_label(module, &decision.expected)
        ),
        TypingExpectation::Dispatch { generic, .. } => format!(
            "the declared types {actual} admit arguments no method of {} accepts",
            typing::definition_label(module, generic_definition(module, *generic))
                .trim_start_matches("generic ")
        ),
        TypingExpectation::Class(_) => format!(
            "the declared type {actual} admits values that are not {}",
            typing::expectation_label(module, &decision.expected)
        ),
        TypingExpectation::Type(_) | TypingExpectation::ClassObject(_) => format!(
            "the declared type {actual} admits values outside {}",
            typing::expectation_label(module, &decision.expected)
        ),
    };
    with_fixes(ResidualCause::Declared, explanation)
}

fn generic_definition(module: &sem::Module, generic: sem::GenericFunctionId) -> DefinitionId {
    module
        .generics
        .get(generic.0 as usize)
        .map_or(DefinitionId(0), |generic| generic.definition)
}

/// The kinds of every reason in the chain rooted at `id`.
fn reason_chain(typed: &DefinitionTyping, id: ReasonId) -> Vec<&ReasonKind> {
    let mut out = Vec::new();
    let mut seen = vec![false; typed.reasons.len()];
    let mut queue = vec![id];
    while let Some(id) = queue.pop() {
        let Some(visited) = seen.get_mut(id.0 as usize) else {
            continue;
        };
        if core::mem::replace(visited, true) {
            continue;
        }
        let Some(reason) = typed.reason(id) else {
            continue;
        };
        out.push(&reason.kind);
        queue.extend(reason.from.iter().copied());
    }
    out
}

/// An `instance?` test before `decision` of the operand whose fact is
/// `reason`, with the same type and source: a test whose narrowing, had it
/// held, could have reached the decision.
fn earlier_test<'a>(
    typed: &'a DefinitionTyping,
    decision: &TypingDecision,
    reason: ReasonId,
) -> Option<&'a TypingDecision> {
    let operand = typed.reason(reason)?;
    typed.decisions.iter().find(|test| {
        test.check == TypingCheck::Instance
            && test.span.end <= decision.span.start
            && test
                .because
                .first()
                .and_then(|id| typed.reason(*id))
                .is_some_and(|tested| tested.span == operand.span && tested.ty == operand.ty)
    })
}

/// A sealed class `ty` is a cone over whose hierarchy has more leaves than
/// a union holds.
fn oversized_sealed_cone(module: &sem::Module, ty: &Type) -> Option<ClassId> {
    ty.atoms()?.iter().find_map(|atom| match atom {
        Atom::Cone(class) if sealed_leaves(module, *class, 0) > MAX_ATOMS => Some(*class),
        _ => None,
    })
}

/// The pieces a sealed cone splits into, recursively; one for a class that
/// is not sealed.
fn sealed_leaves(module: &sem::Module, class: ClassId, depth: usize) -> usize {
    let Some(pieces) = interloko_model::types::sealed_pieces(class, module) else {
        return 1;
    };
    if depth > MAX_ATOMS {
        return pieces.len();
    }
    pieces
        .iter()
        .map(|piece| match piece {
            Atom::Cone(sub) => sealed_leaves(module, *sub, depth + 1),
            _ => 1,
        })
        .sum()
}

/// The input an operand reason reads without a written type, described,
/// with the annotation target that would write one: a parameter whose type
/// is inferred, or a callee, accessor, or constant whose contract is.
fn unannotated_input(
    inputs: &ClaimInputs<'_>,
    typed: &DefinitionTyping,
    reason: ReasonId,
) -> Option<(String, Option<AnnotationTarget>)> {
    let module = inputs.module;
    if let Some(ReasonKind::Contract {
        definition,
        source: typing::ContractSource::Inferred,
    }) = typed.reason(reason).map(|reason| &reason.kind)
    {
        let target = inputs
            .sites
            .iter()
            .find(|site| {
                matches!(site.target, AnnotationTarget::Result { definition: at } if at == *definition)
                    && site.written.is_none()
            })
            .map(|site| site.target.clone());
        let description = match &target {
            Some(target) => format!(
                "the result of {} ({target})",
                typing::definition_label(module, *definition)
            ),
            None => format!(
                "the result of {}",
                typing::definition_label(module, *definition)
            ),
        };
        return Some((description, target));
    }
    let binding = typed
        .bindings
        .iter()
        .find(|fact| fact.reason == reason)?
        .binding;
    let span = match binding {
        typing::Binding::Local(local) => {
            let (locals, params) = definition_locals(module, typed.definition)?;
            params.contains(&local).then_some(())?;
            locals.iter().find(|record| record.id == local)?.span
        }
        typing::Binding::ClosureLocal { closure, local } => {
            let closure = module.closures.get(closure.0 as usize)?;
            closure.params.contains(&local).then_some(())?;
            closure
                .locals
                .iter()
                .find(|record| record.id == local)?
                .span
        }
        typing::Binding::Capture { .. } => return None,
    };
    let site = inputs.sites.iter().find(|site| {
        site.span == span
            && site.target.definition() == typed.definition
            && matches!(site.target, AnnotationTarget::Binding { .. })
            && site.written.is_none()
    })?;
    Some((
        format!("parameter {}", site.target),
        Some(site.target.clone()),
    ))
}

fn definition_locals(
    module: &sem::Module,
    definition: DefinitionId,
) -> Option<(&[sem::Local], &[sem::LocalId])> {
    if let Some(function) = module
        .functions
        .iter()
        .find(|function| function.definition == definition)
    {
        return Some((&function.locals, &function.params));
    }
    module
        .methods
        .iter()
        .find(|method| method.definition == definition)
        .map(|method| (method.locals.as_slice(), method.params.as_slice()))
}

fn class_name(module: &sem::Module, class: ClassId) -> String {
    module
        .classes
        .get(class.0 as usize)
        .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone())
}

impl ClaimReport {
    /// Returns this report stamped with a retained session's `revision`.
    #[must_use]
    pub const fn at_revision(mut self, revision: u64) -> Self {
        self.revision = Some(revision);
        self
    }

    /// The listed sites that stay at run time: the checks a reviewer must
    /// accept, each with why.
    pub fn residual_sites(&self) -> impl Iterator<Item = &ClaimSite> {
        self.sites.iter().filter(|site| site.residual.is_some())
    }

    /// Renders the report as stable text.
    ///
    /// The header names the definition, the revision (`none` outside a
    /// retained session), the basis, the typing status and the codegen
    /// policy. Then the contract, the strict guarantee, what typing
    /// established, what generated code relies on, what supports it, and
    /// the checks typing did not prove, each with its cause and fixes.
    /// [`ClaimDetail::Full`] lists every site instead, with its reasons
    /// and assumptions.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "claims {} {}", self.object, self.label);
        let _ = writeln!(
            out,
            "  revision={} basis={:016x} typing={} codegen={} detail={}",
            self.revision
                .map_or_else(|| "none".to_string(), |revision| format!("{revision}")),
            self.basis,
            self.typing.as_str(),
            self.codegen.as_str(),
            self.detail.as_str()
        );
        match &self.contract {
            Some(contract) => {
                let _ = writeln!(
                    out,
                    "  contract {}: params=({}) results=({}) relied-on-by={}",
                    contract.object,
                    contract.params.join(", "),
                    contract.results.join(", "),
                    contract.relied_on_by
                );
            }
            None => {
                let _ = writeln!(out, "  contract: none");
            }
        }
        let _ = writeln!(
            out,
            "  {}; unmet={} exempt-runtime={}",
            self.strict.guarantee, self.strict.unmet, self.strict.exempt
        );
        let tally = self.tally;
        let _ = writeln!(
            out,
            "  established: sites={} proved={} checked={} refuted={} unreachable={}",
            tally.sites, tally.proved, tally.checked, tally.refuted, tally.unreachable
        );
        let _ = writeln!(
            out,
            "  generated code: relies-on-proof={} (elided={} specialized={} inlined={} linked={}) proved-but-kept={} runtime-checks={} errors={}",
            tally.relied,
            tally.elided,
            tally.specialized,
            tally.inlined,
            tally.linked,
            tally.proved_kept,
            tally.runtime,
            tally.errors
        );
        if self.support.is_empty() {
            let _ = writeln!(out, "  supported-by: nothing outside the body");
        } else {
            let _ = writeln!(out, "  supported-by:");
            for entry in &self.support {
                let _ = writeln!(
                    out,
                    "    {} {} sites={} relied={}",
                    entry.object, entry.description, entry.sites, entry.relied
                );
            }
        }
        let residual = self.residual_sites().count();
        if self.detail == ClaimDetail::Summary {
            let _ = writeln!(out, "  unproved checks: {residual}");
            for site in self.residual_sites() {
                render_site(&mut out, site);
            }
        } else {
            let _ = writeln!(out, "  sites: {} ({residual} unproved)", self.sites.len());
            for site in &self.sites {
                render_site(&mut out, site);
                for reason in &site.reasons {
                    let _ = writeln!(out, "      because {reason}");
                }
                for assumption in &site.assumes {
                    let _ = writeln!(out, "      assumes {assumption}");
                }
            }
        }
        out
    }
}

fn render_site(out: &mut String, site: &ClaimSite) {
    let _ = writeln!(
        out,
        "    {} {} origin={} span={}..{} status={} verdict={} codegen={} strict={} expected={} actual={}",
        site.id,
        site.check.as_str(),
        site.origin.as_str(),
        site.span.start,
        site.span.end,
        site.status.as_str(),
        site.verdict.as_str(),
        site.codegen.as_str(),
        if site.strict_required {
            "required"
        } else {
            "exempt"
        },
        site.expected,
        site.actual
    );
    if !site.depends_on.is_empty() {
        let _ = writeln!(out, "      depends-on {}", site.depends_on.join(","));
    }
    let Some(residual) = &site.residual else {
        return;
    };
    let _ = writeln!(
        out,
        "      cause={}: {}",
        residual.cause.label(),
        residual.explanation
    );
    for offer in &residual.fixes {
        let annotation = offer
            .annotation
            .as_ref()
            .map_or_else(String::new, |(target, _)| format!(" [{target}]"));
        let _ = writeln!(
            out,
            "      fix: {}{annotation}; {}",
            offer.description,
            offer.fix.consequence()
        );
    }
}
