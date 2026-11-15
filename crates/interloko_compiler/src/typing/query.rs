//! Read-only queries over recorded typing facts.
//!
//! [`TypingArtifact::type_of`] answers "what type does this have, and why"
//! for a binding, a check site, or a source span;
//! [`TypingArtifact::dump_types`] lists every binding's type; and
//! [`TypingArtifact::contract_dependents`] finds the decisions whose reasons
//! cite a definition's contract.
//!
//! Binding facts are stored per definition ([`DefinitionTyping::bindings`]).
//! Facts about arbitrary expressions are not: a site or span query runs its
//! definition's analysis again with a probe that keeps the one expression it
//! selects. The analysis is deterministic and per definition, so the answer
//! matches the stored facts without holding a fact per expression.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use interloko_model::types::Type;

use super::flow::{ProbeHit, ProbeTarget};
use super::{
    Binding, DefinitionTyping, Reason, ReasonId, TypingArtifact, TypingDecision, TypingStatus,
    definition_label, run_definition, type_name, write_reason_chain,
};
use crate::identity::DefinitionId;
use crate::sem::{self, ClassId, GenericFunctionId, LocalId, SiteId, ValueKind};
use crate::syntax::Span;

/// Something outside its own body that a typing decision's verdict relies
/// on, so changing it may change the verdict.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TypingDependency {
    /// The result contract of a callee, accessor, or constant, which an
    /// operand's reason chain cites.
    Contract(DefinitionId),
    /// The generic's method set ([`super::Assumption::Generic`]).
    Methods(GenericFunctionId),
    /// That the closed class gains no subclasses ([`super::Assumption::Class`]).
    Class(ClassId),
}

/// What [`TypingArtifact::type_of`] asks about.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypeSelector {
    /// A binding of a function, method, or closure inside one: its type
    /// where it is bound, and its narrower types at uses.
    Binding {
        /// The function, method, or constant whose body binds it.
        definition: DefinitionId,
        /// The binding.
        binding: Binding,
    },
    /// The expression at a check site: the value it produces.
    Site {
        /// The function, method, or constant owning the site.
        definition: DefinitionId,
        /// The check site.
        site: SiteId,
    },
    /// The innermost expression, or the binding name, whose span covers
    /// this span, in the smallest function, method, or constant covering
    /// it. A binding name wins over an expression with the same span.
    Span(Span),
}

/// What a [`TypeReport`] describes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypeSubject {
    /// A binding.
    Binding(Binding),
    /// An expression, with its check site when it leaves a runtime check.
    Expression {
        /// The expression's check site, if any.
        site: Option<SiteId>,
    },
}

/// The flow type of one binding or expression, with its provenance.
///
/// The report owns its reasons, re-indexed so that [`Self::reason`] and
/// every [`Reason::from`] index [`Self::reasons`]; it borrows nothing from
/// the artifact it came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeReport {
    /// The definition whose analysis produced the fact.
    pub definition: DefinitionId,
    /// What was found.
    pub subject: TypeSubject,
    /// Source span of the binding name or expression.
    pub span: Span,
    /// The flow type: what values the binding or expression can hold.
    pub ty: Type,
    /// Sem's category, which is the value's representation.
    pub rep: ValueKind,
    /// The type sem's checker gives the binding or expression
    /// ([`sem::Module::kind_type`] of [`Self::rep`]): a binding's declared
    /// type, or the category sem inferred where none is written. Flow
    /// typing only narrows it, so [`Self::ty`] is always a subtype.
    pub declared: Type,
    /// The reason in [`Self::reasons`] that explains [`Self::ty`].
    pub reason: ReasonId,
    /// The provenance graph of [`Self::reason`] and of every use's reason,
    /// [`Self::reason`] first.
    pub reasons: Vec<Reason>,
    /// For a binding, its reads whose type is narrower than [`Self::ty`],
    /// in evaluation order; empty for an expression.
    pub uses: Vec<UseType>,
}

/// A read of a binding whose type is narrower than the binding's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UseType {
    /// Source span of the read.
    pub span: Span,
    /// The type the read has.
    pub ty: Type,
    /// The reason in [`TypeReport::reasons`] that explains it.
    pub reason: ReasonId,
}

impl TypeReport {
    /// Renders the reason chain of [`Self::reason`], as in
    /// [`TypingArtifact::dump`].
    #[must_use]
    pub fn reason_chain(&self, module: &sem::Module) -> String {
        self.chain(module, self.reason)
    }

    /// Renders the reason chain rooted at `id` in [`Self::reasons`], such as
    /// a use's reason.
    #[must_use]
    pub fn chain(&self, module: &sem::Module, id: ReasonId) -> String {
        let mut out = String::new();
        write_reason_chain(&mut out, module, &self.reasons, id, 0);
        out
    }
}

/// Why [`TypingArtifact::type_of`] has no answer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TypeQueryError {
    /// The typing pass did not run, so there are no facts.
    NotChecked(TypingStatus),
    /// The definition is no checked function, method, or constant.
    UnknownDefinition(DefinitionId),
    /// The definition's body does not bind the binding.
    UnknownBinding(Binding),
    /// The definition has no expression at the check site.
    UnknownSite(SiteId),
    /// No checked definition, expression, or binding covers the span.
    NothingAtSpan(Span),
}

impl fmt::Display for TypeQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotChecked(status) => write!(f, "typing did not run ({})", status.as_str()),
            Self::UnknownDefinition(definition) => {
                write!(
                    f,
                    "{definition} is not a typed function, method, or constant"
                )
            }
            Self::UnknownBinding(binding) => write!(f, "no binding {}", binding_key(*binding)),
            Self::UnknownSite(site) => write!(f, "no check site {site}"),
            Self::NothingAtSpan(span) => {
                write!(f, "nothing typed covers {}..{}", span.start, span.end)
            }
        }
    }
}

impl core::error::Error for TypeQueryError {}

impl TypingArtifact {
    /// Returns the flow type of what `selector` names, with its provenance.
    ///
    /// Binding queries read the stored binding facts. Site and span queries
    /// run their definition's analysis again with a probe, so they cost one
    /// pass over that definition's body.
    ///
    /// # Errors
    ///
    /// Returns a [`TypeQueryError`] when the pass did not run or the
    /// selector names nothing it typed.
    pub fn type_of(
        &self,
        module: &sem::Module,
        selector: &TypeSelector,
    ) -> Result<TypeReport, TypeQueryError> {
        if self.status != TypingStatus::Checked {
            return Err(TypeQueryError::NotChecked(self.status));
        }
        match *selector {
            TypeSelector::Binding {
                definition,
                binding,
            } => {
                let typed = self
                    .definition(definition)
                    .ok_or(TypeQueryError::UnknownDefinition(definition))?;
                binding_report(module, typed, binding)
            }
            TypeSelector::Site { definition, site } => {
                if self.definition(definition).is_none() {
                    return Err(TypeQueryError::UnknownDefinition(definition));
                }
                let (typed, hit) =
                    run_definition(module, definition, Some(ProbeTarget::Site(site)))
                        .ok_or(TypeQueryError::UnknownDefinition(definition))?;
                let hit = hit.ok_or(TypeQueryError::UnknownSite(site))?;
                Ok(expression_report(module, &typed, hit))
            }
            TypeSelector::Span(span) => {
                let nothing = TypeQueryError::NothingAtSpan(span);
                let definition = definition_at(module, span)
                    .filter(|definition| self.definition(*definition).is_some())
                    .ok_or(nothing)?;
                let (typed, hit) =
                    run_definition(module, definition, Some(ProbeTarget::Span(span)))
                        .ok_or(nothing)?;
                let binding = typed
                    .bindings
                    .iter()
                    .filter_map(|fact| {
                        let info = binding_info(module, definition, fact.binding)?;
                        covers(info.span, span).then_some((fact.binding, info.span))
                    })
                    .min_by_key(|(_, at)| span_len(*at));
                match (binding, hit) {
                    (Some((binding, at)), hit)
                        if hit
                            .as_ref()
                            .is_none_or(|hit| span_len(at) <= span_len(hit.span)) =>
                    {
                        binding_report(module, &typed, binding)
                    }
                    (_, Some(hit)) => Ok(expression_report(module, &typed, hit)),
                    (_, None) => Err(nothing),
                }
            }
        }
    }

    /// Returns the decisions whose operands' reason chains cite
    /// `definition`'s result contract, in module order.
    ///
    /// This is a query over recorded reasons: it finds what the current
    /// facts rely on, not every definition that could.
    #[must_use]
    pub fn contract_dependents(&self, definition: DefinitionId) -> Vec<&TypingDecision> {
        let wanted = TypingDependency::Contract(definition);
        self.definitions
            .iter()
            .flat_map(|typed| {
                typed
                    .decisions
                    .iter()
                    .filter(move |decision| typed.dependencies(decision).contains(&wanted))
            })
            .collect()
    }

    /// Renders the type of every binding and result, grouped by definition.
    ///
    /// The header counts definitions, bindings, and narrowed uses. Each
    /// definition lists its bindings in evaluation order as
    /// `role name@start..end type`, followed by `declared=` when sem's type
    /// is wider and the reason chain; each narrowed use follows its binding
    /// as `use @start..end type`. The definition's result comes last.
    #[must_use]
    pub fn dump_types(&self, module: &sem::Module) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "types status={} definitions={} bindings={} narrowed-uses={}",
            self.status.as_str(),
            self.definitions.len(),
            self.definitions
                .iter()
                .map(|typed| typed.bindings.len())
                .sum::<usize>(),
            self.definitions
                .iter()
                .map(|typed| typed.uses.len())
                .sum::<usize>(),
        );
        for typed in &self.definitions {
            write_definition_types(&mut out, module, typed);
        }
        out
    }

    /// Renders one definition's binding and result types in the format of
    /// [`Self::dump_types`].
    #[must_use]
    pub fn dump_definition_types(&self, module: &sem::Module, definition: DefinitionId) -> String {
        let mut out = String::new();
        match self.definition(definition) {
            Some(typed) => write_definition_types(&mut out, module, typed),
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
}

fn write_definition_types(out: &mut String, module: &sem::Module, typed: &DefinitionTyping) {
    let _ = writeln!(
        out,
        "  definition {} {}",
        typed.definition,
        definition_label(module, typed.definition)
    );
    let chain = |id: ReasonId| {
        let mut chain = String::new();
        write_reason_chain(&mut chain, module, &typed.reasons, id, 0);
        chain
    };
    for fact in &typed.bindings {
        let Some(reason) = typed.reason(fact.reason) else {
            continue;
        };
        let info = binding_info(module, typed.definition, fact.binding);
        let (label, at) = info.as_ref().map_or_else(
            || (binding_key(fact.binding), reason.span),
            |info| (info.label(fact.binding), info.span),
        );
        let _ = write!(
            out,
            "    {label}@{}..{} {}",
            at.start,
            at.end,
            type_name(module, &reason.ty)
        );
        if let Some(info) = &info {
            let declared = module.kind_type(info.kind);
            if declared != reason.ty {
                let _ = write!(out, " declared={}", type_name(module, &declared));
            }
        }
        let _ = writeln!(out, " because={}", chain(fact.reason));
        for read in typed
            .uses
            .iter()
            .filter(|read| read.binding == fact.binding)
        {
            if let Some(narrowed) = typed.reason(read.reason) {
                let _ = writeln!(
                    out,
                    "      use @{}..{} {} because={}",
                    read.span.start,
                    read.span.end,
                    type_name(module, &narrowed.ty),
                    chain(read.reason)
                );
            }
        }
    }
    if let Some(result) = typed.result.and_then(|id| Some((id, typed.reason(id)?))) {
        let _ = writeln!(
            out,
            "    result {} because={}",
            type_name(module, &result.1.ty),
            chain(result.0)
        );
    }
}

fn binding_report(
    module: &sem::Module,
    typed: &DefinitionTyping,
    binding: Binding,
) -> Result<TypeReport, TypeQueryError> {
    let unknown = TypeQueryError::UnknownBinding(binding);
    let fact = typed.binding(binding).ok_or(unknown)?;
    let reason = typed.reason(fact.reason).ok_or(unknown)?;
    let info = binding_info(module, typed.definition, binding);
    let reads = typed
        .uses
        .iter()
        .filter(|read| read.binding == binding)
        .collect::<Vec<_>>();
    let roots = core::iter::once(fact.reason)
        .chain(reads.iter().map(|read| read.reason))
        .collect::<Vec<_>>();
    let (reasons, roots) = extract(&typed.reasons, &roots);
    let uses = reads
        .iter()
        .zip(roots.iter().skip(1))
        .filter_map(|(read, root)| {
            Some(UseType {
                span: read.span,
                ty: typed.reason(read.reason)?.ty.clone(),
                reason: *root,
            })
        })
        .collect();
    let rep = info.as_ref().map_or(ValueKind::Any, |info| info.kind);
    Ok(TypeReport {
        definition: typed.definition,
        subject: TypeSubject::Binding(binding),
        span: info.as_ref().map_or(reason.span, |info| info.span),
        ty: reason.ty.clone(),
        rep,
        declared: module.kind_type(rep),
        reason: roots.first().copied().unwrap_or(ReasonId(0)),
        reasons,
        uses,
    })
}

fn expression_report(module: &sem::Module, typed: &DefinitionTyping, hit: ProbeHit) -> TypeReport {
    let (reasons, roots) = extract(&typed.reasons, &[hit.reason]);
    TypeReport {
        definition: typed.definition,
        subject: TypeSubject::Expression { site: hit.site },
        span: hit.span,
        ty: hit.ty,
        rep: hit.kind,
        declared: module.kind_type(hit.kind),
        reason: roots.first().copied().unwrap_or(ReasonId(0)),
        reasons,
        uses: Vec::new(),
    }
}

/// Copies the reasons reachable from `roots` out of `arena`, re-indexed in
/// breadth-first order from the roots, and returns them with the roots'
/// new ids.
fn extract(arena: &[Reason], roots: &[ReasonId]) -> (Vec<Reason>, Vec<ReasonId>) {
    let mut renumbered: Vec<Option<ReasonId>> = vec![None; arena.len()];
    let mut order = Vec::new();
    let mut queue = roots.to_vec();
    let mut next = 0;
    while let Some(id) = queue.get(next).copied() {
        next += 1;
        let Some(slot) = renumbered.get_mut(id.0 as usize) else {
            continue;
        };
        if slot.is_some() {
            continue;
        }
        *slot = Some(ReasonId(u32::try_from(order.len()).unwrap_or(u32::MAX)));
        order.push(id);
        queue.extend(arena[id.0 as usize].from.iter().copied());
    }
    let reasons = order
        .iter()
        .map(|id| {
            let reason = &arena[id.0 as usize];
            Reason {
                kind: reason.kind.clone(),
                span: reason.span,
                ty: reason.ty.clone(),
                from: reason
                    .from
                    .iter()
                    .filter_map(|from| renumbered.get(from.0 as usize).copied().flatten())
                    .collect(),
            }
        })
        .collect();
    let roots = roots
        .iter()
        .map(|root| {
            renumbered
                .get(root.0 as usize)
                .copied()
                .flatten()
                .unwrap_or(ReasonId(0))
        })
        .collect();
    (reasons, roots)
}

/// Source facts about one binding.
struct BindingInfo<'a> {
    name: &'a str,
    kind: ValueKind,
    span: Span,
    role: &'static str,
}

impl BindingInfo<'_> {
    /// Labels the binding as `param n`, `local x`, or, inside a closure,
    /// `closure#0 capture x`.
    fn label(&self, binding: Binding) -> String {
        match binding {
            Binding::Local(_) => alloc::format!("{} {}", self.role, self.name),
            Binding::ClosureLocal { closure, .. } | Binding::Capture { closure, .. } => {
                alloc::format!("closure#{} {} {}", closure.0, self.role, self.name)
            }
        }
    }
}

/// A binding's stable key when its source facts are unavailable, such as
/// `l3`, `closure#0 l1`, or `closure#0 capture#2`.
fn binding_key(binding: Binding) -> String {
    match binding {
        Binding::Local(local) => alloc::format!("l{}", local.0),
        Binding::ClosureLocal { closure, local } => {
            alloc::format!("closure#{} l{}", closure.0, local.0)
        }
        Binding::Capture { closure, capture } => {
            alloc::format!("closure#{} capture#{}", closure.0, capture.0)
        }
    }
}

type LocalInfo<'a> = (&'a str, ValueKind, Span, &'static str);

/// Name, kind, span, and role (`param` or `local`) of `local`.
fn local_info<'a>(
    locals: &'a [sem::Local],
    params: &[LocalId],
    local: LocalId,
) -> Option<LocalInfo<'a>> {
    let found = locals.iter().find(|candidate| candidate.id == local)?;
    let role = if params.contains(&local) {
        "param"
    } else {
        "local"
    };
    Some((found.name.as_str(), found.kind, found.span, role))
}

fn binding_info(
    module: &sem::Module,
    definition: DefinitionId,
    binding: Binding,
) -> Option<BindingInfo<'_>> {
    let (name, kind, span, role) = match binding {
        Binding::Local(local) => {
            if let Some(function) = module
                .functions
                .iter()
                .find(|function| function.definition == definition)
            {
                local_info(&function.locals, &function.params, local)?
            } else {
                let method = module
                    .methods
                    .iter()
                    .find(|method| method.definition == definition)?;
                local_info(&method.locals, &method.params, local)?
            }
        }
        Binding::ClosureLocal { closure, local } => {
            let closure = module.closures.get(closure.0 as usize)?;
            local_info(&closure.locals, &closure.params, local)?
        }
        Binding::Capture { closure, capture } => {
            let closure = module.closures.get(closure.0 as usize)?;
            let found = closure
                .captures
                .iter()
                .find(|candidate| candidate.id == capture)?;
            (found.name.as_str(), found.kind, found.span, "capture")
        }
    };
    Some(BindingInfo {
        name,
        kind,
        span,
        role,
    })
}

/// The smallest function, method, or constant whose span covers `span`.
fn definition_at(module: &sem::Module, span: Span) -> Option<DefinitionId> {
    module
        .functions
        .iter()
        .map(|function| (function.definition, function.span))
        .chain(
            module
                .methods
                .iter()
                .map(|method| (method.definition, method.span)),
        )
        .chain(
            module
                .constants
                .iter()
                .map(|constant| (constant.definition, constant.span)),
        )
        .filter(|(_, at)| covers(*at, span))
        .min_by_key(|(_, at)| span_len(*at))
        .map(|(definition, _)| definition)
}

const fn covers(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

const fn span_len(span: Span) -> usize {
    span.end.saturating_sub(span.start)
}
