//! Typing facts as lowering consumes them.
//!
//! Under [`CodegenPolicy::Proved`], lowering asks the soft typing pass what
//! it decided about each check site, by the site's owner definition and
//! [`SiteId`], and drops or specializes the checks it proved. Under
//! [`CodegenPolicy::Off`], or when typing did not run, there are no facts and
//! lowering emits what it always has.
//!
//! # Why the facts hold in generated code
//!
//! A typing decision depends on the owner's own body and on what its
//! reasons cite beyond it ([`crate::typing::DefinitionTyping::dependencies`]):
//!
//! - **The body itself** (literals, `make`, narrowing by a test, joins,
//!   interval arithmetic). A code unit is immutable and its frames finish the
//!   code they entered, so a fact derived inside a body holds in the code
//!   compiled from it.
//! - **A callee's, accessor's, or constant's contract**
//!   ([`crate::typing::ReasonKind::Contract`]). The flow pass gives such a
//!   value the type sem's category gives it, never anything finer read from
//!   the callee's body, so it relies on no more than lowering already
//!   trusts. A generic's contract is checked when a unit is installed under
//!   live frames, a function's when a frame is retried (direct calls stay in
//!   their unit), a slot's by the runtime layout, and a constant's value is
//!   lowered into the body that reads it.
//! - **A closed world** ([`crate::typing::Assumption`]): a generic's method
//!   set or a closed class's cone. Both are part of the runtime layout, which
//!   a unit installed under live frames must keep. A numeric send's result
//!   ([`crate::typing::ReasonKind::Dispatched`]) is its selected method's,
//!   which the method set fixes.
//!
//! A linked send ([`super::Instr::LinkedSend`]) relies on a dispatch fact too,
//! but not on the selected method staying the one that runs: the runtime
//! repoints its link at the newest code's selection on every install, and
//! the send's results keep the generic's kinds and contract.
//!
//! So every fact lowering acts on is either local to the unit or backed by
//! something install or retry already checks.
//!
//! # Sites decided twice
//!
//! Typing analyzes a closure where the closure is created, so a closure
//! whose expression appears more than once (a slot default copied into
//! several `make` calls) can leave two decisions for one site. A site whose
//! decisions disagree is treated as undecided.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::{CheckCodegenRecord, CodegenPolicy};
use crate::identity::DefinitionId;
use crate::sem::{self, SiteId};
use crate::syntax::Span;
use crate::typing::{
    self, Assumption, Binding, DefinitionTyping, TypingArtifact, TypingDecision, TypingDependency,
    TypingStatus,
};
use interloko_model::sealing::ClosedWorld;
use interloko_model::types::Type;

/// Typing decisions lowering may act on, by owner and site.
#[derive(Debug, Default)]
pub(super) struct TypingFacts<'a> {
    owners: BTreeMap<DefinitionId, OwnerFacts<'a>>,
}

/// One definition's typing and its decisions by site.
#[derive(Debug)]
struct OwnerFacts<'a> {
    typing: &'a DefinitionTyping,
    /// Decisions in site order; `None` where a site's decisions disagree.
    sites: Vec<(SiteId, Option<&'a TypingDecision>)>,
}

impl<'a> TypingFacts<'a> {
    /// Facts from `typing` under `policy`: none unless the policy is
    /// [`CodegenPolicy::Proved`] and the pass checked the module.
    pub(super) fn new(typing: &'a TypingArtifact, policy: CodegenPolicy) -> Self {
        let mut owners = BTreeMap::new();
        if policy != CodegenPolicy::Proved || typing.status != TypingStatus::Checked {
            return Self { owners };
        }
        for definition in &typing.definitions {
            let mut sites = definition
                .decisions
                .iter()
                .map(|decision| {
                    let usable = relies_on_checked_inputs(definition, decision);
                    (decision.site, usable.then_some(decision))
                })
                .collect::<Vec<_>>();
            sites.sort_by_key(|(site, _)| *site);
            let mut unique: Vec<(SiteId, Option<&TypingDecision>)> =
                Vec::with_capacity(sites.len());
            for (site, decision) in sites {
                match unique.last_mut() {
                    Some((last, kept)) if *last == site => {
                        if kept
                            .zip(decision)
                            .is_none_or(|(kept, decision)| !same_codegen_inputs(kept, decision))
                        {
                            *kept = None;
                        }
                    }
                    _ => unique.push((site, decision)),
                }
            }
            owners.insert(
                definition.definition,
                OwnerFacts {
                    typing: definition,
                    sites: unique,
                },
            );
        }
        Self { owners }
    }

    /// The decision for `site` of `owner`, if lowering may act on one.
    pub(super) fn decision(
        &self,
        owner: Option<DefinitionId>,
        site: SiteId,
    ) -> Option<&'a TypingDecision> {
        let sites = &self.owners.get(&owner?)?.sites;
        let index = sites.binary_search_by_key(&site, |(site, _)| *site).ok()?;
        sites.get(index).and_then(|(_, decision)| *decision)
    }

    /// The closed-world assumptions of every decision `records` says
    /// lowering acted on, as a code unit records them
    /// ([`crate::typing::closed_world`]), in first-use order. Closed
    /// built-in classes are left out: no source edit can extend them.
    pub(super) fn closed_world(
        &self,
        module: &sem::Module,
        records: &[CheckCodegenRecord],
    ) -> Vec<ClosedWorld> {
        let mut out = Vec::new();
        for record in records {
            let Some(owner) = self.owners.get(&record.owner) else {
                continue;
            };
            let Some(decision) = self.decision(Some(record.owner), record.site) else {
                continue;
            };
            for dependency in owner.typing.dependencies(decision) {
                let assumption = match dependency {
                    TypingDependency::Methods(generic) => Assumption::Generic(generic),
                    TypingDependency::Sealed(class) => Assumption::Sealed(class),
                    TypingDependency::Class(_) | TypingDependency::Contract(_) => continue,
                };
                if let Some(record) = typing::closed_world(module, assumption)
                    && !out.contains(&record)
                {
                    out.push(record);
                }
            }
        }
        out
    }

    /// The type flow typing established for a read of `binding` at `span`
    /// in `owner`'s body; see [`DefinitionTyping::read_type`].
    pub(super) fn read_type(
        &self,
        owner: Option<DefinitionId>,
        binding: Binding,
        span: Span,
    ) -> Option<&'a Type> {
        self.owners.get(&owner?)?.typing.read_type(binding, span)
    }
}

/// Whether everything `decision` relies on beyond its body is something an
/// install or retry under live frames already checks; see the module docs.
///
/// Every kind of dependency typing records today is, so this accepts all of
/// them. The match is exhaustive on purpose: a new kind, such as a fact
/// read from a callee's body, must decide here whether generated code may
/// trust it.
fn relies_on_checked_inputs(typing: &DefinitionTyping, decision: &TypingDecision) -> bool {
    typing
        .dependencies(decision)
        .iter()
        .all(|dependency| match dependency {
            // Sem's category for the value, which the contract records:
            // a generic's is checked at install, a function's at retry, a
            // slot's by the layout; a constant's value is lowered inline.
            TypingDependency::Contract(_) => true,
            // Fixed by the runtime layout, which an install must keep. A
            // unit records the sealed classes and method sets its code
            // relies on, so a refused install names the one it would break.
            TypingDependency::Methods(_)
            | TypingDependency::Class(_)
            | TypingDependency::Sealed(_) => true,
        })
}

/// Whether two decisions for one site would lead lowering to the same code.
fn same_codegen_inputs(left: &TypingDecision, right: &TypingDecision) -> bool {
    left.check == right.check
        && left.verdict == right.verdict
        && left.rule == right.rule
        && left.expected == right.expected
        && left.selected_method == right.selected_method
}

/// The definition whose typing decides the checks in each closure's body,
/// in closure id order.
///
/// A closure's sites are numbered, and typed, as part of the function,
/// method, or constant body that creates it. A closure that more than one
/// body creates, or that a slot initializer creates (slot initializers are
/// numbered on their own and not typed), has no owner, so lowering keeps
/// every check in it.
pub(super) fn closure_owners(module: &sem::Module) -> Vec<Option<DefinitionId>> {
    let mut owners = alloc::vec![Owner::Unseen; module.closures.len()];
    let bodies = module
        .functions
        .iter()
        .map(|function| (Some(function.definition), &function.body))
        .chain(
            module
                .methods
                .iter()
                .map(|method| (Some(method.definition), &method.body)),
        );
    for (owner, body) in bodies {
        claim_block(module, &mut owners, owner, body);
    }
    for constant in &module.constants {
        claim_expr(
            module,
            &mut owners,
            Some(constant.definition),
            &constant.value,
        );
    }
    let initializers = module.classes.iter().flat_map(|class| {
        class
            .direct_slots
            .iter()
            .filter_map(|slot| slot.init_value.as_ref())
            .chain(
                class
                    .effective_slots
                    .iter()
                    .filter_map(|slot| slot.init_value.as_ref()),
            )
    });
    for value in initializers {
        claim_expr(module, &mut owners, None, value);
    }
    owners
        .into_iter()
        .map(|owner| match owner {
            Owner::One(definition) => Some(definition),
            Owner::Unseen | Owner::Shared => None,
        })
        .collect()
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Owner {
    Unseen,
    One(DefinitionId),
    Shared,
}

fn claim_block(
    module: &sem::Module,
    owners: &mut [Owner],
    owner: Option<DefinitionId>,
    block: &sem::Block,
) {
    for binding in &block.lets {
        claim_expr(module, owners, owner, &binding.value);
    }
    claim_expr(module, owners, owner, &block.result);
}

fn claim_expr(
    module: &sem::Module,
    owners: &mut [Owner],
    owner: Option<DefinitionId>,
    expr: &sem::Expr,
) {
    visit_exprs(expr, &mut |inner| {
        let sem::Expr::Closure { closure, .. } = inner else {
            return;
        };
        let index = closure.0 as usize;
        let Some(slot) = owners.get_mut(index) else {
            return;
        };
        let claimed = match (*slot, owner) {
            (Owner::Unseen, Some(owner)) => Owner::One(owner),
            (Owner::One(existing), Some(owner)) if existing == owner => return,
            _ => Owner::Shared,
        };
        *slot = claimed;
        if let Some(body) = module.closures.get(index).map(|closure| &closure.body) {
            claim_block(module, owners, owner, body);
        }
    });
}

/// Calls `visit` on every check site in `expr` that its own code evaluates:
/// closure bodies are lowered as functions of their own and are skipped.
pub(super) fn for_each_site(expr: &sem::Expr, visit: &mut impl FnMut(SiteId)) {
    visit_exprs(expr, &mut |inner| {
        let site = match inner {
            sem::Expr::GenericCall { site, .. }
            | sem::Expr::Cast { site, .. }
            | sem::Expr::InstanceOf { site, .. }
            | sem::Expr::InstanceOfLimited { site, .. }
            | sem::Expr::Convert { site, .. }
            | sem::Expr::CheckRange { site, .. } => Some(*site),
            sem::Expr::Binary { site, .. } | sem::Expr::Negate { site, .. } => *site,
            _ => None,
        };
        if let Some(site) = site {
            visit(site);
        }
    });
}

/// Whether the code `expr` evaluates reads `local`, directly or to capture
/// it in a closure.
pub(super) fn reads_local(expr: &sem::Expr, local: sem::LocalId) -> bool {
    let mut reads = false;
    visit_exprs(expr, &mut |inner| match inner {
        sem::Expr::Local { local: read, .. } => reads |= *read == local,
        sem::Expr::Closure { captures, .. } => {
            reads |= captures.iter().any(|capture| capture.source == local);
        }
        _ => {}
    });
    reads
}

/// Visits `expr` and every expression its own code evaluates, in evaluation
/// order. Closure bodies are not visited; a constant's value is not either.
fn visit_exprs(expr: &sem::Expr, visit: &mut impl FnMut(&sem::Expr)) {
    use sem::Expr as E;
    match expr {
        E::VectorLiteral {
            elements: items, ..
        }
        | E::Values { values: items, .. }
        | E::IntrinsicCall { args: items, .. }
        | E::HostCall { args: items, .. }
        | E::DirectCall { args: items, .. }
        | E::GenericCall { args: items, .. }
        | E::FloatCall { args: items, .. } => {
            for item in items {
                visit_exprs(item, visit);
            }
        }
        E::CallableCall { callable, args, .. } => {
            visit_exprs(callable, visit);
            for arg in args {
                visit_exprs(arg, visit);
            }
        }
        E::Convert { value: inner, .. }
        | E::CheckRange { value: inner, .. }
        | E::Negate { value: inner, .. }
        | E::Signal {
            condition: inner, ..
        }
        | E::ClassOf { object: inner, .. }
        | E::InstanceOf { value: inner, .. }
        | E::InstanceOfLimited { value: inner, .. }
        | E::Cast { value: inner, .. }
        | E::AccessorCall {
            receiver: inner, ..
        } => visit_exprs(inner, visit),
        E::MakeObject { class, fields, .. } => {
            visit_exprs(class, visit);
            for field in fields {
                visit_exprs(&field.value, visit);
            }
        }
        E::MakeRuntimeClass {
            class,
            init_args,
            candidates,
            ..
        } => {
            visit_exprs(class, visit);
            for arg in init_args {
                visit_exprs(&arg.value, visit);
            }
            for field in candidates.iter().flat_map(|candidate| &candidate.defaults) {
                visit_exprs(&field.value, visit);
            }
        }
        E::SlotWrite {
            receiver, value, ..
        } => {
            visit_exprs(receiver, visit);
            visit_exprs(value, visit);
        }
        E::Binary { lhs, rhs, .. }
        | E::Compare { lhs, rhs, .. }
        | E::MixedCompare { lhs, rhs, .. }
        | E::ValueEqual { lhs, rhs, .. } => {
            visit_exprs(lhs, visit);
            visit_exprs(rhs, visit);
        }
        E::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit_exprs(condition, visit);
            visit_exprs(then_branch, visit);
            visit_exprs(else_branch, visit);
        }
        E::Handle { body, restart, .. } => {
            visit_exprs(body, visit);
            if let Some(guard) = &restart.guard {
                visit_exprs(guard, visit);
            }
            visit_exprs(&restart.body, visit);
        }
        E::Block { body, cleanup, .. } => {
            visit_block(body, visit);
            if let Some(cleanup) = cleanup {
                visit_block(cleanup, visit);
            }
        }
        E::Closure { .. }
        | E::ConstI64 { .. }
        | E::ConstF64 { .. }
        | E::Const32 { .. }
        | E::ConstBool { .. }
        | E::StringLiteral { .. }
        | E::SymbolLiteral { .. }
        | E::Local { .. }
        | E::Capture { .. }
        | E::ConstantRef { .. }
        | E::ClassObject { .. }
        | E::TypeObject { .. }
        | E::KnownCallable { .. }
        | E::GenericFunctionObject { .. }
        | E::Error { .. } => {}
    }
    visit(expr);
}

fn visit_block(block: &sem::Block, visit: &mut impl FnMut(&sem::Expr)) {
    for binding in &block.lets {
        visit_exprs(&binding.value, visit);
    }
    visit_exprs(&block.result, visit);
}
