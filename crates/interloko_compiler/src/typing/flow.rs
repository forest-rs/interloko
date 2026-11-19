//! The flow-typing abstract interpreter behind [`super::check_definition`].
//!
//! [`Analysis`] walks one definition's body in evaluation order with an
//! environment ([`Env`]) from locals and captures to facts: a [`Type`] plus
//! the [`ReasonId`] that explains it. A local's fact holds until an
//! assignment replaces it, and the walk needs no control-flow graph:
//! branches are analyzed under narrowed copies of the environment and
//! joined afterwards, and a loop's body is analyzed from a loop head whose
//! facts are iterated to a fixpoint ([`super::widen_to_fixpoint`]) before
//! the pass that records its decisions. An environment whose `dead` reason is set describes
//! code no value reaches; expressions there have [`Type::bottom`] and the
//! checks there are recorded as [`TypingRule::Unreachable`].

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use interloko_model::types::{Atom, IntRange, Metaobject, Relation, Type, sealed_pieces};

use super::{
    Assumption, Binding, BindingFact, Branch, CheckOrigin, ContractSource, Reason, ReasonId,
    ReasonKind, TypingCheck, TypingDecision, TypingExpectation, TypingRule, TypingVerdict, UseFact,
    range_type, specializer_type,
};
use crate::identity::DefinitionId;
use crate::sem::{
    self, BinaryOp, CaptureId, ClassId, ClosureId, CompareOp, GenericFunctionId, LocalId, Number32,
    NumericKind, SiteId, ValueKind,
};
use crate::syntax::Span;

/// Nesting of constant references followed before a constant's type falls
/// back to its sem category.
const MAX_CONSTANT_DEPTH: usize = 16;

/// The shift counts that do not trap.
const SHIFT_COUNTS: IntRange = IntRange {
    min: Some(0),
    max: Some(31),
};

/// The `<integer>` values the current runtime represents; integer
/// arithmetic outside it traps.
const INTEGER_REGISTER: IntRange = IntRange {
    min: Some(i64::MIN),
    max: Some(i64::MAX),
};

/// A type together with the reason that explains it.
#[derive(Clone, Debug)]
struct Fact {
    ty: Type,
    reason: ReasonId,
}

/// The method a proved dispatch selects, and the reasons it relies on.
#[derive(Clone, Debug)]
struct Selected {
    method: sem::MethodId,
    because: Vec<ReasonId>,
}

/// A binding the environment tracks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Var {
    Local(LocalId),
    Capture(CaptureId),
}

/// Facts at one program point of one function or closure body.
#[derive(Clone, Debug)]
struct Env {
    /// Why no value reaches this point, when none does.
    dead: Option<ReasonId>,
    locals: Vec<Option<Fact>>,
    captures: Vec<Option<Fact>>,
}

impl Env {
    fn new(locals: usize, captures: usize) -> Self {
        Self {
            dead: None,
            locals: vec![None; locals],
            captures: vec![None; captures],
        }
    }

    fn get(&self, var: Var) -> Option<&Fact> {
        match var {
            Var::Local(local) => self.locals.get(local.0 as usize)?.as_ref(),
            Var::Capture(capture) => self.captures.get(capture.0 as usize)?.as_ref(),
        }
    }

    fn set(&mut self, var: Var, fact: Fact) {
        let (slots, index) = match var {
            Var::Local(local) => (&mut self.locals, local.0 as usize),
            Var::Capture(capture) => (&mut self.captures, capture.0 as usize),
        };
        if slots.len() <= index {
            slots.resize(index + 1, None);
        }
        slots[index] = Some(fact);
    }

    fn kill(&mut self, reason: ReasonId) {
        if self.dead.is_none() {
            self.dead = Some(reason);
        }
    }
}

/// What a trial pass over a loop body may record, to undo before the next
/// pass: only the final pass, from the loop's fixpoint head, decides.
struct Checkpoint {
    decisions: usize,
    bindings: usize,
    uses: usize,
    probe: Option<(ProbeTarget, Option<ProbeHit>)>,
    bound_locals: Vec<Option<ReasonId>>,
    bound_captures: Vec<Option<ReasonId>>,
}

/// A `block`'s exit procedure while its body is analyzed.
struct Exit {
    local: LocalId,
    /// Join of the arguments of every reachable call.
    joined: Option<Fact>,
    /// Whether a reachable call exists.
    reached: bool,
    /// Whether the procedure is used other than by calling it, or with
    /// other than one value, so unseen calls may supply the block's value.
    escaped: bool,
}

/// One function or closure body on the analysis stack.
struct Context<'a> {
    /// The closure whose body this is, or `None` for the definition's own
    /// body.
    closure: Option<ClosureId>,
    locals: &'a [sem::Local],
    captures: &'a [sem::Capture],
    exits: Vec<Exit>,
    /// The reason of each local's binding fact, once bound.
    bound_locals: Vec<Option<ReasonId>>,
    /// The reason of each capture's binding fact, once bound.
    bound_captures: Vec<Option<ReasonId>>,
}

impl<'a> Context<'a> {
    fn new(
        closure: Option<ClosureId>,
        locals: &'a [sem::Local],
        captures: &'a [sem::Capture],
    ) -> Self {
        Self {
            closure,
            locals,
            captures,
            exits: Vec::new(),
            bound_locals: Vec::new(),
            bound_captures: Vec::new(),
        }
    }

    /// The binding `var` names in this body; only closures have captures.
    const fn binding(&self, var: Var) -> Option<Binding> {
        match (var, self.closure) {
            (Var::Local(local), None) => Some(Binding::Local(local)),
            (Var::Local(local), Some(closure)) => Some(Binding::ClosureLocal { closure, local }),
            (Var::Capture(capture), Some(closure)) => Some(Binding::Capture { closure, capture }),
            (Var::Capture(_), None) => None,
        }
    }

    fn bound_slot(&mut self, var: Var) -> &mut Option<ReasonId> {
        let (slots, index) = match var {
            Var::Local(local) => (&mut self.bound_locals, local.0 as usize),
            Var::Capture(capture) => (&mut self.bound_captures, capture.0 as usize),
        };
        if slots.len() <= index {
            slots.resize(index + 1, None);
        }
        &mut slots[index]
    }
}

/// What a probe run of the analysis looks for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum ProbeTarget {
    /// The innermost expression whose span covers this span.
    Span(Span),
    /// The expression at this check site.
    Site(SiteId),
}

/// The expression a probe found, with its fact.
#[derive(Clone, Debug)]
pub(super) struct ProbeHit {
    pub(super) span: Span,
    pub(super) site: Option<SiteId>,
    pub(super) kind: ValueKind,
    pub(super) ty: Type,
    pub(super) reason: ReasonId,
}

/// Everything one run of the analysis records.
pub(super) struct Finished {
    pub(super) decisions: Vec<TypingDecision>,
    pub(super) reasons: Vec<Reason>,
    pub(super) bindings: Vec<BindingFact>,
    pub(super) uses: Vec<UseFact>,
    pub(super) result: Option<ReasonId>,
    pub(super) probe: Option<ProbeHit>,
}

/// The integers an operand may hold; `None` bounds are unbounded. Bounds
/// are wide enough for any sum or product of 64-bit bounds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Interval {
    lo: Option<i128>,
    hi: Option<i128>,
}

impl Interval {
    const UNBOUNDED: Self = Self { lo: None, hi: None };

    fn of(range: IntRange) -> Self {
        Self {
            lo: range.min.map(i128::from),
            hi: range.max.map(i128::from),
        }
    }

    /// The hull of an all-integer type, or `None` for any other type.
    fn hull(ty: &Type) -> Option<Self> {
        let atoms = ty.atoms()?;
        let mut hull: Option<IntRange> = None;
        for atom in atoms {
            let Atom::Integer(range) = atom else {
                return None;
            };
            hull = Some(hull.map_or(*range, |hull| hull.hull(*range)));
        }
        hull.map(Self::of)
    }

    fn point(self) -> Option<i128> {
        match (self.lo, self.hi) {
            (Some(lo), Some(hi)) if lo == hi => Some(lo),
            _ => None,
        }
    }

    fn add(self, other: Self) -> Self {
        Self {
            lo: self.lo.zip(other.lo).map(|(a, b)| a + b),
            hi: self.hi.zip(other.hi).map(|(a, b)| a + b),
        }
    }

    fn sub(self, other: Self) -> Self {
        self.add(other.negate())
    }

    fn mul(self, other: Self) -> Self {
        let (Some(a), Some(b), Some(c), Some(d)) = (self.lo, self.hi, other.lo, other.hi) else {
            return Self::UNBOUNDED;
        };
        let products = [a * c, a * d, b * c, b * d];
        Self {
            lo: products.iter().copied().min(),
            hi: products.iter().copied().max(),
        }
    }

    fn negate(self) -> Self {
        Self {
            lo: self.hi.map(|hi| -hi),
            hi: self.lo.map(|lo| -lo),
        }
    }

    /// The smallest [`IntRange`] holding the interval; a bound beyond the
    /// 64-bit range becomes unbounded.
    fn range(self) -> IntRange {
        IntRange {
            min: self.lo.and_then(|lo| i64::try_from(lo).ok()),
            max: self.hi.and_then(|hi| i64::try_from(hi).ok()),
        }
    }

    fn within(self, range: IntRange) -> bool {
        let above = range
            .min
            .is_none_or(|min| self.lo.is_some_and(|lo| lo >= i128::from(min)));
        let below = range
            .max
            .is_none_or(|max| self.hi.is_some_and(|hi| hi <= i128::from(max)));
        above && below
    }

    fn disjoint(self, range: IntRange) -> bool {
        let above = range
            .max
            .zip(self.lo)
            .is_some_and(|(max, lo)| lo > i128::from(max));
        let below = range
            .min
            .zip(self.hi)
            .is_some_and(|(min, hi)| hi < i128::from(min));
        above || below
    }
}

/// The flow analysis of one definition.
pub(super) struct Analysis<'a> {
    module: &'a sem::Module,
    owner: DefinitionId,
    decisions: Vec<TypingDecision>,
    reasons: Vec<Reason>,
    bindings: Vec<BindingFact>,
    uses: Vec<UseFact>,
    result: Option<ReasonId>,
    contexts: Vec<Context<'a>>,
    assumptions: Vec<(Assumption, ReasonId)>,
    /// The closed-world reasons each reason's chain cites, parallel to
    /// `reasons`.
    cited: Vec<Vec<ReasonId>>,
    /// The definitions whose contracts each reason's chain cites, parallel
    /// to `reasons`.
    contracts: Vec<Vec<DefinitionId>>,
    constants: Vec<Option<Type>>,
    depth: usize,
    probe: Option<(ProbeTarget, Option<ProbeHit>)>,
    /// The last head each loop reached, by loop. An enclosing loop's
    /// passes give an inner loop entry facts that only grow, so its next
    /// fixpoint starts from there instead of from scratch, which keeps
    /// nested loops from multiplying their passes.
    loop_heads: Vec<(*const sem::Expr, Vec<Type>)>,
}

impl<'a> Analysis<'a> {
    pub(super) fn new(module: &'a sem::Module, owner: DefinitionId) -> Self {
        Self {
            module,
            owner,
            decisions: Vec::new(),
            reasons: Vec::new(),
            bindings: Vec::new(),
            uses: Vec::new(),
            result: None,
            contexts: Vec::new(),
            assumptions: Vec::new(),
            cited: Vec::new(),
            contracts: Vec::new(),
            constants: vec![None; module.constants.len()],
            depth: 0,
            probe: None,
            loop_heads: Vec::new(),
        }
    }

    /// Makes this run also report the expression `target` selects.
    pub(super) fn probe(&mut self, target: ProbeTarget) {
        self.probe = Some((target, None));
    }

    /// Analyzes a function or method body whose parameters have their
    /// declared types.
    pub(super) fn body(
        &mut self,
        locals: &'a [sem::Local],
        params: &[LocalId],
        body: &'a sem::Block,
    ) {
        self.contexts.push(Context::new(None, locals, &[]));
        let mut env = Env::new(locals.len(), 0);
        for param in params {
            self.bind_declared(&mut env, *param);
        }
        let result = self.block(&mut env, body);
        self.result = Some(result.reason);
        self.contexts.pop();
    }

    /// Classifies the checks a function's indirect entry makes on its
    /// parameters; see [`sem::EntryCheck`]. The values are whatever an
    /// indirect caller passes.
    pub(super) fn entry_checks(&mut self, locals: &'a [sem::Local], checks: &[sem::EntryCheck]) {
        self.contexts.push(Context::new(None, locals, &[]));
        self.check_entry(&Env::new(locals.len(), 0), checks);
        self.contexts.pop();
    }

    fn check_entry(&mut self, env: &Env, checks: &[sem::EntryCheck]) {
        for check in checks {
            let Some(span) = self.local(check.param).map(|local| local.span) else {
                continue;
            };
            // An argument reaches the typed entry at its register type and
            // the boxed entry as a boxed `<object>`; either may hold any value
            // of that type.
            let passed = self.fact(ReasonKind::IndirectEntry, span, Type::top(), Vec::new());
            let check_kind = match check.target {
                sem::EntryTarget::Range(_) => TypingCheck::Range,
                _ => TypingCheck::Cast,
            };
            let (decided, expected, assumes) = match check.target {
                sem::EntryTarget::Instance(class) => {
                    let target = Type::of(Atom::Cone(class), self.module);
                    let mut assumes = self.class_assumptions(&[&passed.ty, &target]);
                    let (decided, sealed) =
                        decide_sealed(self.module, env, &passed.ty, &target, CAST_RULES);
                    assumes.extend(self.sealed_assumptions(&sealed));
                    (decided, TypingExpectation::Class(class), assumes)
                }
                sem::EntryTarget::ClassObject(bound) => (
                    decide_class_object(self.module, env, &passed.ty, bound),
                    TypingExpectation::ClassObject(bound),
                    Vec::new(),
                ),
                sem::EntryTarget::Range(range) => {
                    let target = range_type(self.module, range);
                    (
                        decide(self.module, env, &passed.ty, &target, RANGE_RULES),
                        TypingExpectation::Type(target),
                        Vec::new(),
                    )
                }
                sem::EntryTarget::Unbox(kind) => {
                    let target = self.module.kind_type(kind);
                    (
                        decide(self.module, env, &passed.ty, &target, CAST_RULES),
                        TypingExpectation::Type(target),
                        Vec::new(),
                    )
                }
            };
            self.record(
                check.site,
                (check_kind, CheckOrigin::Entry),
                decided,
                span,
                expected,
                &[passed],
                assumes,
                None,
            );
        }
    }

    /// Analyzes an expression outside any function body, such as a
    /// constant's initializer, and returns its type.
    pub(super) fn detached(&mut self, expr: &'a sem::Expr) -> Type {
        self.contexts.push(Context::new(None, &[], &[]));
        let mut env = Env::new(0, 0);
        let fact = self.eval(&mut env, expr);
        self.result = Some(fact.reason);
        self.contexts.pop();
        fact.ty
    }

    /// Returns the decisions in check-site order, the reason arena, and the
    /// binding facts, in evaluation order.
    pub(super) fn finish(mut self) -> Finished {
        self.decisions.sort_by_key(|decision| decision.site);
        Finished {
            decisions: self.decisions,
            reasons: self.reasons,
            bindings: self.bindings,
            uses: self.uses,
            result: self.result,
            probe: self.probe.and_then(|(_, hit)| hit),
        }
    }

    /// Binds `var` to `fact` and records the binding's fact the first time
    /// it is bound.
    ///
    /// An assigned local holds other values later, so its binding fact is
    /// its declared type, and each read records the fact it sees.
    fn define(&mut self, env: &mut Env, var: Var, fact: Fact) {
        let first = self
            .contexts
            .last_mut()
            .is_some_and(|context| context.bound_slot(var).is_none());
        if first {
            let reason = match var {
                Var::Local(local) => match self.local(local).filter(|binding| binding.assigned) {
                    Some(binding) => {
                        let ty = self.declared_type(binding);
                        self.reason(ReasonKind::Declared, binding.span, &ty, Vec::new())
                    }
                    None => fact.reason,
                },
                Var::Capture(_) => fact.reason,
            };
            if let Some(context) = self.contexts.last_mut() {
                *context.bound_slot(var) = Some(reason);
                if let Some(binding) = context.binding(var) {
                    self.bindings.push(BindingFact { binding, reason });
                }
            }
        }
        env.set(var, fact);
    }

    /// Saves what a trial pass over a loop body records.
    fn checkpoint(&self) -> Checkpoint {
        let context = self.contexts.last();
        Checkpoint {
            decisions: self.decisions.len(),
            bindings: self.bindings.len(),
            uses: self.uses.len(),
            probe: self.probe.clone(),
            bound_locals: context
                .map(|context| context.bound_locals.clone())
                .unwrap_or_default(),
            bound_captures: context
                .map(|context| context.bound_captures.clone())
                .unwrap_or_default(),
        }
    }

    /// Undoes what was recorded since `checkpoint`. Reasons stay: facts
    /// derived in the trial pass may cite them.
    fn rollback(&mut self, checkpoint: Checkpoint) {
        self.decisions.truncate(checkpoint.decisions);
        self.bindings.truncate(checkpoint.bindings);
        self.uses.truncate(checkpoint.uses);
        self.probe = checkpoint.probe;
        if let Some(context) = self.contexts.last_mut() {
            context.bound_locals = checkpoint.bound_locals;
            context.bound_captures = checkpoint.bound_captures;
        }
    }

    /// Records a read of `var` whose fact differs from its binding fact.
    fn record_use(&mut self, var: Var, span: Span, reason: ReasonId) {
        let Some(context) = self.contexts.last_mut() else {
            return;
        };
        match *context.bound_slot(var) {
            Some(bound) if bound != reason => {
                if let Some(binding) = context.binding(var) {
                    self.uses.push(UseFact {
                        binding,
                        span,
                        reason,
                    });
                }
            }
            _ => {}
        }
    }

    /// Offers an evaluated expression to the probe, if any.
    fn observe(&mut self, expr: &sem::Expr, fact: &Fact) {
        let Some((target, hit)) = &mut self.probe else {
            return;
        };
        let span = expr.span();
        let site = site_of(expr);
        let found = match *target {
            ProbeTarget::Site(wanted) => site == Some(wanted) && hit.is_none(),
            // Expressions finish inner first, so among equal spans the
            // first one seen is the innermost.
            ProbeTarget::Span(wanted) => {
                span.start <= wanted.start
                    && wanted.end <= span.end
                    && hit
                        .as_ref()
                        .is_none_or(|hit| span_len(span) < span_len(hit.span))
            }
        };
        if found {
            *hit = Some(ProbeHit {
                span,
                site,
                kind: self.module.expr_kind(expr),
                ty: fact.ty.clone(),
                reason: fact.reason,
            });
        }
    }

    fn reason(&mut self, kind: ReasonKind, span: Span, ty: &Type, from: Vec<ReasonId>) -> ReasonId {
        let id = ReasonId(u32::try_from(self.reasons.len()).unwrap_or(u32::MAX));
        // A reason cites what the reasons it derives from cite. There are
        // few assumptions per definition, so each list stays short.
        let mut cited = Vec::new();
        if matches!(kind, ReasonKind::ClosedWorld(_)) {
            cited.push(id);
        }
        for parent in &from {
            for assumption in self.cited.get(parent.0 as usize).into_iter().flatten() {
                if !cited.contains(assumption) {
                    cited.push(*assumption);
                }
            }
        }
        self.cited.push(cited);
        let mut contracts = Vec::new();
        if let ReasonKind::Contract { definition, .. } = kind {
            contracts.push(definition);
        }
        for parent in &from {
            for definition in self.contracts.get(parent.0 as usize).into_iter().flatten() {
                if !contracts.contains(definition) {
                    contracts.push(*definition);
                }
            }
        }
        self.contracts.push(contracts);
        self.reasons.push(Reason {
            kind,
            span,
            ty: ty.clone(),
            from,
        });
        id
    }

    fn fact(&mut self, kind: ReasonKind, span: Span, ty: Type, from: Vec<ReasonId>) -> Fact {
        let reason = self.reason(kind, span, &ty, from);
        Fact { ty, reason }
    }

    /// The type of every value sem's category for `expr` can hold.
    fn bound(&self, expr: &sem::Expr) -> Type {
        self.module.kind_type(self.module.expr_kind(expr))
    }

    /// The type of a call `expr` whose callee declares `ranges` for its
    /// results: sem's category, within the range of a single limited
    /// result, which the callee checks before it returns.
    fn limited_result(&self, expr: &sem::Expr, ranges: &[Option<IntRange>]) -> Type {
        match ranges {
            [Some(range)] => self
                .bound(expr)
                .meet(&range_type(self.module, *range), self.module),
            _ => self.bound(expr),
        }
    }

    fn assume(&mut self, assumption: Assumption) -> ReasonId {
        if let Some((_, reason)) = self
            .assumptions
            .iter()
            .find(|(known, _)| *known == assumption)
        {
            return *reason;
        }
        let reason = self.reason(
            ReasonKind::ClosedWorld(assumption),
            Span::new(0, 0),
            &Type::top(),
            Vec::new(),
        );
        self.assumptions.push((assumption, reason));
        reason
    }

    /// Closed classes whose exactness the types rely on. A source class is
    /// closed only because it is sealed, so its assumption is its sealing.
    fn class_assumptions(&mut self, types: &[&Type]) -> Vec<ReasonId> {
        let mut classes: Vec<ClassId> = Vec::new();
        for ty in types {
            for atom in ty.atoms().unwrap_or_default() {
                if let Atom::Exact(class) = atom
                    && interloko_model::types::ClassHierarchy::is_closed(self.module, *class)
                    && !classes.contains(class)
                {
                    classes.push(*class);
                }
            }
        }
        classes
            .into_iter()
            .map(|class| {
                let sealed = self
                    .module
                    .classes
                    .get(class.0 as usize)
                    .is_some_and(|record| record.sealing.is_sealed());
                self.assume(if sealed {
                    Assumption::Sealed(class)
                } else {
                    Assumption::Class(class)
                })
            })
            .collect()
    }

    /// The assumptions that `classes` stay sealed as they are.
    fn sealed_assumptions(&mut self, classes: &[ClassId]) -> Vec<ReasonId> {
        classes
            .iter()
            .map(|class| self.assume(Assumption::Sealed(*class)))
            .collect()
    }

    fn local(&self, local: LocalId) -> Option<&'a sem::Local> {
        let locals = self.contexts.last()?.locals;
        locals
            .get(local.0 as usize)
            .filter(|candidate| candidate.id == local)
            .or_else(|| locals.iter().find(|candidate| candidate.id == local))
    }

    /// The type `binding` declares: its kind's, within its limited range.
    fn declared_type(&self, binding: &sem::Local) -> Type {
        declared(self.module, binding.kind, binding.range)
    }

    /// Binds `local` to its declared type.
    fn bind_declared(&mut self, env: &mut Env, local: LocalId) {
        let Some(binding) = self.local(local) else {
            return;
        };
        let ty = self.declared_type(binding);
        let fact = self.fact(ReasonKind::Declared, binding.span, ty, Vec::new());
        self.define(env, Var::Local(local), fact);
    }

    /// Binds `local` to `fact`, narrowed by the local's declared type, and
    /// returns the fact it now has.
    fn assign(&mut self, env: &mut Env, local: LocalId, fact: Fact) -> Fact {
        let fact = match self.local(local) {
            Some(binding) => {
                let ty = fact.ty.meet(&self.declared_type(binding), self.module);
                if ty == fact.ty {
                    fact
                } else {
                    self.fact(ReasonKind::Declared, binding.span, ty, vec![fact.reason])
                }
            }
            None => fact,
        };
        if fact.ty.is_bottom() {
            env.kill(fact.reason);
        }
        self.define(env, Var::Local(local), fact.clone());
        fact
    }

    /// Gives each of `locals` bound in `env` its declared type: the facts
    /// at a point some assignments to them may or may not have reached.
    fn forget(&mut self, env: &mut Env, locals: &[LocalId], span: Span) {
        for local in locals {
            if env.get(Var::Local(*local)).is_none() {
                continue;
            }
            let Some(binding) = self.local(*local) else {
                continue;
            };
            let ty = self.declared_type(binding);
            let fact = self.fact(ReasonKind::Declared, span, ty, Vec::new());
            env.set(Var::Local(*local), fact);
        }
    }

    fn block(&mut self, env: &mut Env, block: &'a sem::Block) -> Fact {
        for statement in &block.statements {
            match statement {
                sem::Statement::Let(binding) => self.bind(env, binding),
                sem::Statement::Expr(expr) => {
                    let _ = self.eval(env, expr);
                }
            }
        }
        self.eval(env, &block.result)
    }

    /// Analyzes a `while` or `until` loop and leaves `env` at its exit.
    ///
    /// The locals the loop assigns are its loop-carried locals. Their facts
    /// at the loop head are the join of their entry facts and their facts
    /// at the end of each iteration, iterated to a fixpoint with trial
    /// passes over the loop that record nothing ([`Self::checkpoint`]).
    /// The final pass, from that head, records the loop's decisions, so
    /// they hold on every iteration. The loop leaves when its test says
    /// so, with the facts that outcome implies at the head.
    fn loop_expr(
        &mut self,
        env: &mut Env,
        expr: &'a sem::Expr,
        kind: sem::LoopKind,
        test: &'a sem::Expr,
        body: &'a sem::Block,
        span: Span,
    ) -> Fact {
        let carried = expr
            .assigned_locals()
            .into_iter()
            .filter(|local| env.get(Var::Local(*local)).is_some())
            .collect::<Vec<_>>();
        let entry = env.clone();
        let entry_types = carried
            .iter()
            .map(|local| {
                entry
                    .get(Var::Local(*local))
                    .map_or_else(Type::bottom, |fact| fact.ty.clone())
            })
            .collect::<Vec<_>>();
        // The back edge of the pass from the final head: what the head's
        // facts derive from besides the entry, and whether the head is
        // exactly their join or was widened past it.
        let mut back: Vec<Option<Fact>> = Vec::new();
        let head = if carried.is_empty() || entry.dead.is_some() {
            Vec::new()
        } else {
            let key = core::ptr::from_ref(expr);
            let start = match self
                .loop_heads
                .iter()
                .find(|(loop_expr, _)| *loop_expr == key)
            {
                Some((_, previous)) if previous.len() == entry_types.len() => entry_types
                    .iter()
                    .zip(previous)
                    .map(|(entry, previous)| entry.join(previous, self.module))
                    .collect(),
                _ => entry_types.clone(),
            };
            let head = super::widen_to_fixpoint(self.module, start, |head| {
                let checkpoint = self.checkpoint();
                let head_env = self.loop_head(&entry, &carried, head, &[], span);
                let (_, back_env) = self.iterate(head_env, kind, test, body);
                self.rollback(checkpoint);
                back = carried
                    .iter()
                    .map(|local| match back_env.dead {
                        None => back_env.get(Var::Local(*local)).cloned(),
                        // No iteration comes back; the head relies on why,
                        // unless an exit simply leaves.
                        Some(dead) if !self.is_exit(dead) => Some(Fact {
                            ty: Type::bottom(),
                            reason: dead,
                        }),
                        Some(_) => None,
                    })
                    .collect();
                back.iter()
                    .map(|fact| {
                        fact.as_ref()
                            .map_or_else(Type::bottom, |fact| fact.ty.clone())
                    })
                    .collect()
            });
            match self
                .loop_heads
                .iter_mut()
                .find(|(loop_expr, _)| *loop_expr == key)
            {
                Some((_, previous)) => previous.clone_from(&head),
                None => self.loop_heads.push((key, head.clone())),
            }
            head
        };
        let back = head
            .iter()
            .zip(&entry_types)
            .zip(back)
            .map(|((head, entry), back)| {
                let back_type = back
                    .as_ref()
                    .map_or_else(Type::bottom, |fact| fact.ty.clone());
                let exact = *head == entry.join(&back_type, self.module);
                (exact, back.map(|fact| fact.reason))
            })
            .collect::<Vec<_>>();
        let head_env = self.loop_head(&entry, &carried, &head, &back, span);
        let (exit, _) = self.iterate(head_env, kind, test, body);
        *env = exit;
        self.declared(expr)
    }

    /// The environment at a loop head where the loop-carried locals
    /// `carried` have the types `head`, within their declared types.
    ///
    /// Each head fact derives from the entry's and, when `back` has it, the
    /// back edge's fact (or why the back edge is dead), whose assumptions it
    /// relies on too. A head type `back` says is exactly their join is the
    /// entry's ([`ReasonKind::Kept`]) or [`ReasonKind::Joined`]; any other
    /// is [`ReasonKind::Widened`], which may hold values no iteration
    /// produces.
    fn loop_head(
        &mut self,
        entry: &Env,
        carried: &[LocalId],
        head: &[Type],
        back: &[(bool, Option<ReasonId>)],
        span: Span,
    ) -> Env {
        let mut env = entry.clone();
        for (index, (local, ty)) in carried.iter().zip(head).enumerate() {
            let (Some(entry_fact), Some(binding)) =
                (entry.get(Var::Local(*local)), self.local(*local))
            else {
                continue;
            };
            let (exact, back_reason) = back.get(index).copied().unwrap_or((false, None));
            // A head the back edge adds nothing to still relies on what the
            // back edge's fact, or its deadness, assumed, so it cites it.
            if *ty == entry_fact.ty && back_reason.is_none_or(|back| back == entry_fact.reason) {
                continue;
            }
            let ty = ty.meet(&self.declared_type(binding), self.module);
            let kind = if !exact {
                ReasonKind::Widened
            } else if ty == entry_fact.ty {
                ReasonKind::Kept
            } else {
                ReasonKind::Joined
            };
            let mut from = vec![entry_fact.reason];
            from.extend(back_reason);
            let fact = self.fact(kind, span, ty, from);
            env.set(Var::Local(*local), fact);
        }
        env
    }

    /// One pass over a loop from `head`: the test, then the body when the
    /// test continues the loop. Returns the environments where the loop
    /// exits and at the end of the body, the back edge.
    fn iterate(
        &mut self,
        mut head: Env,
        kind: sem::LoopKind,
        test: &'a sem::Expr,
        body: &'a sem::Block,
    ) -> (Env, Env) {
        let (_, when_true, when_false) = self.condition(&mut head, test);
        let (mut back, exit) = if kind.continues_on() {
            (when_true, when_false)
        } else {
            (when_false, when_true)
        };
        let _ = self.block(&mut back, body);
        (exit, back)
    }

    fn bind(&mut self, env: &mut Env, binding: &'a sem::Let) {
        let locals = if binding.locals.is_empty() {
            core::slice::from_ref(&binding.local)
        } else {
            binding.locals.as_slice()
        };
        match (&binding.value, locals) {
            (_, [local]) => {
                let fact = self.eval(env, &binding.value);
                let _ = self.assign(env, *local, fact);
            }
            (sem::Expr::Values { values, .. }, locals) if values.len() == locals.len() => {
                for (value, local) in values.iter().zip(locals) {
                    let fact = self.eval(env, value);
                    let _ = self.assign(env, *local, fact);
                }
            }
            (value, locals) => {
                let _ = self.eval(env, value);
                for local in locals {
                    self.bind_declared(env, *local);
                }
            }
        }
    }

    /// Evaluates `expr` under `env`, leaving `env` at the point after it.
    fn eval(&mut self, env: &mut Env, expr: &'a sem::Expr) -> Fact {
        // Code no value reaches has no value: its fact is why it is dead,
        // even where the code would itself transfer control.
        let unreached = env.dead;
        let fact = self.transfer(env, expr);
        let fact = match (unreached, env.dead) {
            (Some(dead), _) => Fact {
                ty: Type::bottom(),
                reason: dead,
            },
            (None, Some(dead)) if !fact.ty.is_bottom() => Fact {
                ty: Type::bottom(),
                reason: dead,
            },
            _ => fact,
        };
        debug_assert!(
            fact.ty.is_subtype(&self.bound(expr), self.module),
            "soft type {} of {expr:?} is not within sem's {}",
            super::type_name(self.module, &fact.ty),
            super::type_name(self.module, &self.bound(expr)),
        );
        if fact.ty.is_bottom() {
            env.kill(fact.reason);
        }
        self.observe(expr, &fact);
        fact
    }

    fn eval_all(&mut self, env: &mut Env, exprs: &'a [sem::Expr]) -> Vec<Fact> {
        exprs.iter().map(|expr| self.eval(env, expr)).collect()
    }

    fn declared(&mut self, expr: &sem::Expr) -> Fact {
        let ty = self.bound(expr);
        self.fact(ReasonKind::Declared, expr.span(), ty, Vec::new())
    }

    fn literal(&mut self, span: Span, ty: Type) -> Fact {
        self.fact(ReasonKind::Literal, span, ty, Vec::new())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one transfer arm per expression form reads best as one match"
    )]
    fn transfer(&mut self, env: &mut Env, expr: &'a sem::Expr) -> Fact {
        use sem::Expr as E;
        let module = self.module;
        match expr {
            E::ConstI64 { value, span } => {
                self.literal(*span, range_type(module, IntRange::singleton(*value)))
            }
            E::Const32 { value, span } => {
                let ty = match value {
                    Number32::I32(value) => {
                        range_type(module, IntRange::singleton(i64::from(*value)))
                    }
                    Number32::U32(value) => {
                        range_type(module, IntRange::singleton(i64::from(*value)))
                    }
                    Number32::F32(_) => self.bound(expr),
                };
                self.literal(*span, ty)
            }
            E::ConstBool { value, span } => {
                self.literal(*span, Type::of(Atom::Boolean(*value), module))
            }
            E::ConstF64 { span, .. }
            | E::StringLiteral { span, .. }
            | E::SymbolLiteral { span, .. }
            | E::KnownCallable { span, .. }
            | E::GenericFunctionObject { span, .. }
            | E::TypeObject { span, .. } => {
                let ty = self.bound(expr);
                self.literal(*span, ty)
            }
            E::ClassObject { class, span } => self.literal(
                *span,
                Type::of(
                    Atom::Singleton(interloko_model::types::Metaobject::Class(*class)),
                    module,
                ),
            ),
            E::VectorLiteral { elements, span, .. } => {
                let _ = self.eval_all(env, elements);
                let ty = self.bound(expr);
                self.literal(*span, ty)
            }
            E::Values { values, .. } => match values.as_slice() {
                [value] => self.eval(env, value),
                values => {
                    let _ = self.eval_all(env, values);
                    self.declared(expr)
                }
            },
            E::Local {
                local,
                result_kind,
                span,
                ..
            } => self.read(env, Var::Local(*local), *result_kind, *span),
            E::Capture {
                capture,
                result_kind,
                span,
                ..
            } => self.read(env, Var::Capture(*capture), *result_kind, *span),
            E::ConstantRef { constant, span, .. } => {
                let Some(definition) = module.constants.get(constant.0 as usize) else {
                    return self.declared(expr);
                };
                let ty = self
                    .constant_type(constant.0 as usize)
                    .meet(&self.bound(expr), module);
                self.fact(
                    ReasonKind::Contract {
                        definition: definition.definition,
                        source: ContractSource::Inferred,
                    },
                    *span,
                    ty,
                    Vec::new(),
                )
            }
            E::Closure { closure, span, .. } => {
                self.closure(env, closure.0 as usize);
                let ty = self.bound(expr);
                self.fact(ReasonKind::Constructed, *span, ty, Vec::new())
            }
            E::IntrinsicCall { args, .. }
            | E::HostCall { args, .. }
            | E::FloatCall { args, .. } => {
                let _ = self.eval_all(env, args);
                self.declared(expr)
            }
            E::DirectCall {
                function,
                args,
                span,
            } => {
                let _ = self.eval_all(env, args);
                let Some(function) = module.functions.get(function.0 as usize) else {
                    return self.declared(expr);
                };
                let source = contract_source(function.return_annotation.is_some());
                let ty = self.limited_result(expr, &function.result_ranges);
                self.fact(
                    ReasonKind::Contract {
                        definition: function.definition,
                        source,
                    },
                    *span,
                    ty,
                    Vec::new(),
                )
            }
            E::GenericCall {
                generic,
                args,
                site,
                span,
            } => {
                let facts = self.eval_all(env, args);
                let selected = self.dispatch(env, *generic, &facts, *span, *site);
                let Some(generic) = module.generics.get(generic.0 as usize) else {
                    return self.declared(expr);
                };
                if let Some(fact) = selected
                    .and_then(|selected| self.numeric_result(expr, generic, selected, *site))
                {
                    return fact;
                }
                let source = contract_source(generic.return_annotation.is_some());
                let ty = self.limited_result(expr, &generic.result_ranges);
                self.fact(
                    ReasonKind::Contract {
                        definition: generic.definition,
                        source,
                    },
                    *span,
                    ty,
                    Vec::new(),
                )
            }
            E::CallableCall {
                callable,
                args,
                span,
                ..
            } => {
                if let Some(exit) = var_of(callable).and_then(|var| self.exit_of(var)) {
                    let facts = self.eval_all(env, args);
                    let from = facts.iter().map(|fact| fact.reason).collect();
                    self.exit_call(env, exit, facts, *span);
                    return self.fact(ReasonKind::Exit, *span, Type::bottom(), from);
                }
                let _ = self.eval(env, callable);
                let _ = self.eval_all(env, args);
                self.declared(expr)
            }
            E::Signal {
                condition: inner, ..
            }
            | E::ClassOf { object: inner, .. } => {
                let _ = self.eval(env, inner);
                self.declared(expr)
            }
            E::InstanceOf {
                value,
                class,
                site,
                span,
            } => {
                let value = self.eval(env, value);
                let target = Type::of(Atom::Cone(*class), module);
                let mut assumes = self.class_assumptions(&[&value.ty, &target]);
                let (decided, sealed) =
                    decide_sealed(module, env, &value.ty, &target, INSTANCE_RULES);
                assumes.extend(self.sealed_assumptions(&sealed));
                self.record(
                    *site,
                    (TypingCheck::Instance, CheckOrigin::Test),
                    decided,
                    *span,
                    TypingExpectation::Class(*class),
                    &[value],
                    assumes,
                    None,
                );
                self.declared(expr)
            }
            E::InstanceOfLimited {
                value,
                range,
                site,
                span,
            } => {
                let value = self.eval(env, value);
                let target = range_type(module, *range);
                let decided = decide(module, env, &value.ty, &target, RANGE_RULES);
                self.record(
                    *site,
                    (TypingCheck::Range, CheckOrigin::Test),
                    decided,
                    *span,
                    TypingExpectation::Type(target),
                    &[value],
                    Vec::new(),
                    None,
                );
                self.declared(expr)
            }
            E::Cast {
                value,
                class,
                site,
                explicit,
                span,
            } => {
                let value = self.eval(env, value);
                let target = Type::of(Atom::Cone(*class), module);
                let mut assumes = self.class_assumptions(&[&value.ty, &target]);
                let (decided, sealed) = decide_sealed(module, env, &value.ty, &target, CAST_RULES);
                assumes.extend(self.sealed_assumptions(&sealed));
                // A failed cast signals a `<type-error>` whose `use-value`
                // restart may supply any instance of the target, which the
                // cast checks again, so only a proved cast keeps what is
                // known about its operand.
                let ty = if decided.0 == TypingVerdict::Proved {
                    value.ty.meet(&self.bound(expr), module)
                } else {
                    self.bound(expr)
                };
                // A cast proved by splitting passes its value on relying on
                // the same sealing.
                let mut from = vec![value.reason];
                if decided.0 == TypingVerdict::Proved {
                    from.extend(self.sealed_assumptions(&sealed));
                }
                self.record(
                    *site,
                    (TypingCheck::Cast, CheckOrigin::of(*explicit)),
                    decided,
                    *span,
                    TypingExpectation::Class(*class),
                    &[value],
                    assumes,
                    None,
                );
                self.fact(ReasonKind::Check { site: *site }, *span, ty, from)
            }
            E::MakeObject {
                class,
                class_id,
                fields,
                span,
                ..
            } => {
                let _ = self.eval(env, class);
                for field in fields {
                    let _ = self.eval(env, &field.value);
                }
                self.fact(
                    ReasonKind::Constructed,
                    *span,
                    Type::of(Atom::Exact(*class_id), module),
                    Vec::new(),
                )
            }
            // The class value may name any subclass of the bound, so the
            // object is only known to be in the bound's cone.
            E::MakeRuntimeClass {
                class,
                bound,
                init_args,
                candidates,
                span,
            } => {
                let _ = self.eval(env, class);
                for arg in init_args {
                    let _ = self.eval(env, &arg.value);
                }
                for field in candidates.iter().flat_map(|candidate| &candidate.defaults) {
                    let _ = self.eval(env, &field.value);
                }
                self.fact(
                    ReasonKind::Constructed,
                    *span,
                    Type::of(Atom::Cone(*bound), module),
                    Vec::new(),
                )
            }
            E::AccessorCall {
                accessor,
                receiver,
                class,
                slot,
                span,
                ..
            } => {
                let _ = self.eval(env, receiver);
                let range = module
                    .classes
                    .get(class.0 as usize)
                    .and_then(|class| class.effective_slots.get(slot.0 as usize))
                    .and_then(|slot| slot.range);
                let ty = self.limited_result(expr, &[range]);
                self.fact(
                    ReasonKind::Contract {
                        definition: *accessor,
                        source: ContractSource::Declared,
                    },
                    *span,
                    ty,
                    Vec::new(),
                )
            }
            E::SlotWrite {
                setter,
                receiver,
                value,
                class,
                slot,
                span,
                ..
            } => {
                let _ = self.eval(env, receiver);
                let _ = self.eval(env, value);
                let range = module
                    .classes
                    .get(class.0 as usize)
                    .and_then(|class| class.effective_slots.get(slot.0 as usize))
                    .and_then(|slot| slot.range);
                let ty = self.limited_result(expr, &[range]);
                self.fact(
                    ReasonKind::Contract {
                        definition: *setter,
                        source: ContractSource::Declared,
                    },
                    *span,
                    ty,
                    Vec::new(),
                )
            }
            E::Binary {
                op,
                kind,
                lhs,
                rhs,
                site,
                span,
            } => {
                let lhs = self.eval(env, lhs);
                let rhs = self.eval(env, rhs);
                match site {
                    Some(site) if kind.is_integer() => {
                        let bound = self.bound(expr);
                        self.binary(env, *op, *kind, lhs, rhs, *site, *span, bound)
                    }
                    _ => self.declared(expr),
                }
            }
            E::Negate {
                kind,
                value,
                site,
                span,
            } => {
                let value = self.eval(env, value);
                match site {
                    Some(site) if kind.is_integer() => {
                        let interval = Interval::hull(&value.ty)
                            .unwrap_or(Interval::UNBOUNDED)
                            .negate();
                        let bound = self.bound(expr);
                        self.arithmetic(env, *kind, interval, vec![value], *site, *span, bound)
                    }
                    _ => self.declared(expr),
                }
            }
            E::Convert {
                to,
                value,
                site,
                explicit,
                span,
            } => {
                let from = NumericKind::of(module.expr_kind(value));
                let value = self.eval(env, value);
                let bound = self.bound(expr);
                self.convert(env, from, *to, value, (*site, *explicit), *span, bound)
            }
            E::CheckRange {
                value,
                range,
                site,
                explicit,
                span,
            } => {
                let value = self.eval(env, value);
                self.check_range(env, value, *range, (*site, *explicit), *span)
            }
            E::Compare { lhs, rhs, .. }
            | E::MixedCompare { lhs, rhs, .. }
            | E::ValueEqual { lhs, rhs, .. }
            | E::Identical { lhs, rhs, .. } => {
                let _ = self.eval(env, lhs);
                let _ = self.eval(env, rhs);
                self.declared(expr)
            }
            E::If {
                condition,
                then_branch,
                else_branch,
                span,
                ..
            } => {
                let (_, mut then_env, mut else_env) = self.condition(env, condition);
                let then_fact = self.eval(&mut then_env, then_branch);
                let else_fact = self.eval(&mut else_env, else_branch);
                *env = self.join_env(then_env, else_env, *span);
                self.join_facts(then_fact, else_fact, *span)
            }
            E::Discard { value, .. } => {
                let _ = self.eval(env, value);
                self.declared(expr)
            }
            E::AssignLocal { local, value, .. } => {
                let fact = self.eval(env, value);
                self.assign(env, *local, fact)
            }
            E::Loop {
                kind,
                test,
                body,
                span,
            } => self.loop_expr(env, expr, *kind, test, body, *span),
            E::Handle {
                body,
                restart,
                span,
            } => self.handle(env, body, restart, *span),
            E::Block {
                exit,
                body,
                cleanup,
                span,
            } => {
                let bound = self.bound(expr);
                self.block_expr(env, exit.as_ref(), body, cleanup.as_deref(), *span, bound)
            }
            E::Error { .. } => self.declared(expr),
        }
    }

    /// Reads a local or capture at a use site whose sem category is `kind`.
    fn read(&mut self, env: &Env, var: Var, kind: ValueKind, span: Span) -> Fact {
        if let Some((context, index)) = self.exit_of(var) {
            self.contexts[context].exits[index].escaped = true;
        }
        let at_use = self.module.kind_type(kind);
        match env.get(var).cloned() {
            Some(fact) => {
                let ty = fact.ty.meet(&at_use, self.module);
                let fact = if ty == fact.ty {
                    fact
                } else {
                    self.fact(ReasonKind::Declared, span, ty, vec![fact.reason])
                };
                if env.dead.is_none() {
                    self.record_use(var, span, fact.reason);
                }
                fact
            }
            None => self.fact(ReasonKind::Declared, span, at_use, Vec::new()),
        }
    }

    fn constant_type(&mut self, index: usize) -> Type {
        if let Some(Some(ty)) = self.constants.get(index) {
            return ty.clone();
        }
        let Some(constant) = self.module.constants.get(index) else {
            return Type::top();
        };
        let ty = if self.depth >= MAX_CONSTANT_DEPTH {
            self.module.kind_type(constant.result_kind)
        } else {
            let mut nested = Analysis::new(self.module, constant.definition);
            nested.depth = self.depth + 1;
            nested.detached(&constant.value)
        };
        if let Some(slot) = self.constants.get_mut(index) {
            *slot = Some(ty.clone());
        }
        ty
    }

    /// Analyzes a closure body where the closure is created: its captures
    /// hold the facts of their sources here, which no assignment changes: sem
    /// rejects capturing an assigned local.
    fn closure(&mut self, env: &Env, index: usize) {
        let Some(closure) = self.module.closures.get(index) else {
            return;
        };
        let mut inner = Env::new(closure.locals.len(), closure.captures.len());
        inner.dead = env.dead;
        self.contexts.push(Context::new(
            Some(closure.id),
            &closure.locals,
            &closure.captures,
        ));
        for capture in &closure.captures {
            let declared = declared(self.module, capture.kind, capture.range);
            let fact = match env.get(Var::Local(capture.source)).cloned() {
                Some(fact) => {
                    let ty = fact.ty.meet(&declared, self.module);
                    if ty == fact.ty {
                        fact
                    } else {
                        self.fact(ReasonKind::Declared, capture.span, ty, vec![fact.reason])
                    }
                }
                None => self.fact(ReasonKind::Declared, capture.span, declared, Vec::new()),
            };
            self.define(&mut inner, Var::Capture(capture.id), fact);
        }
        for param in &closure.params {
            self.bind_declared(&mut inner, *param);
        }
        self.check_entry(&inner, &closure.entry_checks);
        let _ = self.block(&mut inner, &closure.body);
        self.contexts.pop();
    }

    /// Finds the active block exit `var` names, following captures out to
    /// the context that bound it.
    fn exit_of(&self, var: Var) -> Option<(usize, usize)> {
        let mut context = self.contexts.len().checked_sub(1)?;
        let mut var = var;
        loop {
            match var {
                Var::Local(local) => {
                    return self.contexts[context]
                        .exits
                        .iter()
                        .rposition(|exit| exit.local == local)
                        .map(|index| (context, index));
                }
                Var::Capture(capture) => {
                    let source = self.contexts[context]
                        .captures
                        .get(capture.0 as usize)?
                        .source;
                    context = context.checked_sub(1)?;
                    var = Var::Local(source);
                }
            }
        }
    }

    /// Records a reachable call of a block's exit procedure: its argument
    /// joins the block's value.
    fn exit_call(
        &mut self,
        env: &Env,
        (context, index): (usize, usize),
        args: Vec<Fact>,
        span: Span,
    ) {
        if env.dead.is_some() {
            return;
        }
        let exit = &mut self.contexts[context].exits[index];
        exit.reached = true;
        let Ok([arg]) = <[Fact; 1]>::try_from(args) else {
            // Several values: the block's declared types stand in for them.
            exit.escaped = true;
            return;
        };
        let joined = match exit.joined.take() {
            Some(previous) => self.join_facts(previous, arg, span),
            None => arg,
        };
        self.contexts[context].exits[index].joined = Some(joined);
    }

    fn block_expr(
        &mut self,
        env: &mut Env,
        exit: Option<&sem::BlockExit>,
        body: &'a sem::Block,
        cleanup: Option<&'a sem::Block>,
        span: Span,
        bound: Type,
    ) -> Fact {
        let entry = env.clone();
        if let Some(exit) = exit {
            self.bind_declared(env, exit.local);
            if let Some(context) = self.contexts.last_mut() {
                context.exits.push(Exit {
                    local: exit.local,
                    joined: None,
                    reached: false,
                    escaped: exit.result_kinds.len() != 1,
                });
            }
        }
        let mut result = self.block(env, body);
        let state = exit.and_then(|_| self.contexts.last_mut()?.exits.pop());
        if let Some(state) = state {
            if let Some(joined) = state.joined {
                result = self.join_facts(result, joined, span);
            }
            if state.escaped {
                let declared = self.fact(ReasonKind::Declared, span, bound.clone(), Vec::new());
                result = self.join_facts(result, declared, span);
            }
            // A call to the exit resumes after the block with the facts of
            // its entry, which still hold for locals the body does not
            // assign; one it assigns may have any value of its type.
            if (state.reached || state.escaped) && entry.dead.is_none() {
                *env = entry.clone();
                self.forget(env, &body.assigned_locals(), span);
            }
        }
        if let Some(cleanup) = cleanup {
            // The cleanup runs wherever the body stopped.
            let mut cleanup_env = entry;
            self.forget(&mut cleanup_env, &body.assigned_locals(), span);
            let _ = self.block(&mut cleanup_env, cleanup);
            if let Some(dead) = cleanup_env.dead {
                env.kill(dead);
            }
            self.forget(env, &cleanup.assigned_locals(), span);
        }
        let ty = result.ty.meet(&bound, self.module);
        if ty == result.ty {
            result
        } else {
            self.fact(ReasonKind::Declared, span, ty, vec![result.reason])
        }
    }

    fn handle(
        &mut self,
        env: &mut Env,
        body: &'a sem::Expr,
        restart: &'a sem::Restart,
        span: Span,
    ) -> Fact {
        let entry = env.clone();
        if let Some(guard) = &restart.guard {
            let mut guard_env = entry.clone();
            let _ = self.eval(&mut guard_env, guard);
        }
        let body_fact = self.eval(env, body);
        // A condition may be signalled anywhere in the body, so the handler
        // starts from the facts at the body's entry, except for the locals
        // the body assigns.
        let mut handler_env = entry;
        self.forget(&mut handler_env, &body.assigned_locals(), span);
        if let Some(local) = restart.condition_local {
            let mut ty = Type::of(Atom::Cone(restart.specializer), self.module);
            if let Some(binding) = self.local(local) {
                ty = ty.meet(&self.module.kind_type(binding.kind), self.module);
            }
            let fact = self.fact(
                ReasonKind::HandlerCondition {
                    specializer: restart.specializer,
                },
                restart.span,
                ty,
                Vec::new(),
            );
            self.define(&mut handler_env, Var::Local(local), fact);
        }
        let handler_fact = self.eval(&mut handler_env, &restart.body);
        let body_env = core::mem::replace(env, Env::new(0, 0));
        *env = self.join_env(body_env, handler_env, span);
        self.join_facts(body_fact, handler_fact, span)
    }

    /// Evaluates a test and returns its value with the environments of its
    /// true and false outcomes.
    fn condition(&mut self, env: &mut Env, expr: &'a sem::Expr) -> (Fact, Env, Env) {
        if let sem::Expr::If {
            condition,
            then_branch,
            else_branch,
            span,
            ..
        } = expr
        {
            let (_, mut then_env, mut else_env) = self.condition(env, condition);
            let (then_fact, then_true, then_false) = self.condition(&mut then_env, then_branch);
            let (else_fact, else_true, else_false) = self.condition(&mut else_env, else_branch);
            let fact = self.join_facts(then_fact, else_fact, *span);
            let when_true = self.join_env(then_true, else_true, *span);
            let when_false = self.join_env(then_false, else_false, *span);
            *env = self.join_env(when_true.clone(), when_false.clone(), *span);
            self.observe(expr, &fact);
            return (fact, when_true, when_false);
        }
        let fact = self.eval(env, expr);
        let mut when_true = env.clone();
        let mut when_false = env.clone();
        if env.dead.is_none() {
            self.narrow(expr, &mut when_true, &mut when_false);
            let module = self.module;
            if fact
                .ty
                .meet(&Type::of(Atom::Boolean(true), module), module)
                .is_bottom()
            {
                when_true.kill(fact.reason);
            }
            if fact
                .ty
                .meet(&Type::of(Atom::Boolean(false), module), module)
                .is_bottom()
            {
                when_false.kill(fact.reason);
            }
        }
        (fact, when_true, when_false)
    }

    /// Applies the narrowing a test implies to the environments of its
    /// outcomes.
    fn narrow(&mut self, test: &sem::Expr, when_true: &mut Env, when_false: &mut Env) {
        let module = self.module;
        match test {
            sem::Expr::InstanceOf {
                value,
                class,
                site,
                span,
            } => {
                if let Some(var) = var_of(value) {
                    let target = Type::of(Atom::Cone(*class), module);
                    self.narrow_by(when_true, when_false, var, &target, Some(*site), *span);
                }
            }
            sem::Expr::InstanceOfLimited {
                value,
                range,
                site,
                span,
            } => {
                if let Some(var) = var_of(value) {
                    let target = range_type(module, *range);
                    self.narrow_by(when_true, when_false, var, &target, Some(*site), *span);
                }
            }
            // A test that assigns a local compares values read at different
            // times, so it narrows nothing.
            sem::Expr::Compare {
                op,
                kind,
                lhs,
                rhs,
                span,
            } if kind.is_integer() && test.assigned_locals().is_empty() => {
                // Each side narrows the other by its hull, and relies on how
                // that hull was derived.
                let hull = |peeked: Option<(Type, Option<ReasonId>)>| {
                    peeked.and_then(|(ty, reason)| Some((Interval::hull(&ty)?, reason)))
                };
                let lhs_hull = hull(self.peek(when_true, lhs));
                let rhs_hull = hull(self.peek(when_true, rhs));
                if let (Some(var), Some(other)) = (self.integer_var_of(lhs), rhs_hull) {
                    self.narrow_compare(when_true, when_false, var, *op, other, *span);
                }
                if let (Some(var), Some(other)) = (self.integer_var_of(rhs), lhs_hull) {
                    self.narrow_compare(when_true, when_false, var, op.reversed(), other, *span);
                }
            }
            // `x == k` for a literal `k` with exactly one value: where it is
            // true, `x` is that value; where false, it is not.
            sem::Expr::Identical { lhs, rhs, span } => {
                for (var, key) in [(lhs, rhs), (rhs, lhs)] {
                    if let (Some(var), Some(value)) = (var_of(var), self.literal_singleton(key)) {
                        self.narrow_by(when_true, when_false, var, &value, None, *span);
                    }
                }
            }
            _ => {}
        }
    }

    /// The type holding exactly the value of the literal `expr`, when
    /// types can name it: an integer or a boolean.
    fn literal_singleton(&self, expr: &sem::Expr) -> Option<Type> {
        let module = self.module;
        let value = match expr {
            sem::Expr::ConstI64 { value, .. } => *value,
            sem::Expr::Const32 {
                value: Number32::I32(value),
                ..
            } => i64::from(*value),
            sem::Expr::Const32 {
                value: Number32::U32(value),
                ..
            } => i64::from(*value),
            sem::Expr::ConstBool { value, .. } => {
                return Some(Type::of(Atom::Boolean(*value), module));
            }
            _ => return None,
        };
        Some(range_type(module, IntRange::singleton(value)))
    }

    /// Narrows `var` to `var ⊓ target` when the test is true and to
    /// `var ∖ target` when it is false.
    fn narrow_by(
        &mut self,
        when_true: &mut Env,
        when_false: &mut Env,
        var: Var,
        target: &Type,
        site: Option<SiteId>,
        span: Span,
    ) {
        let module = self.module;
        self.refine(when_true, var, (site, None), Branch::Then, span, |ty| {
            (ty.meet(target, module), Vec::new())
        });
        // What a sealed cone loses to the test leaves its other pieces.
        self.refine(when_false, var, (site, None), Branch::Else, span, |ty| {
            let difference = ty.difference(target, module);
            (difference.ty, difference.sealed)
        });
    }

    /// Narrows the integer `var` by `var op other` in both outcomes.
    fn narrow_compare(
        &mut self,
        when_true: &mut Env,
        when_false: &mut Env,
        var: Var,
        op: CompareOp,
        (other, other_reason): (Interval, Option<ReasonId>),
        span: Span,
    ) {
        let module = self.module;
        let at_most = |bound: Option<i128>| Interval {
            lo: None,
            hi: bound,
        };
        let at_least = |bound: Option<i128>| Interval {
            lo: bound,
            hi: None,
        };
        let minus_one = |bound: Option<i128>| bound.map(|bound| bound - 1);
        let plus_one = |bound: Option<i128>| bound.map(|bound| bound + 1);
        let (then_bound, else_bound) = match op {
            CompareOp::Lt => (at_most(minus_one(other.hi)), at_least(other.lo)),
            CompareOp::Le => (at_most(other.hi), at_least(plus_one(other.lo))),
            CompareOp::Gt => (at_least(plus_one(other.lo)), at_most(other.hi)),
            CompareOp::Ge => (at_least(other.lo), at_most(minus_one(other.hi))),
            CompareOp::Eq => (other, Interval::UNBOUNDED),
        };
        let then_type = range_type(module, then_bound.range());
        self.refine(
            when_true,
            var,
            (None, other_reason),
            Branch::Then,
            span,
            |ty| (ty.meet(&then_type, module), Vec::new()),
        );
        if op == CompareOp::Eq {
            if let Some(point) = other.point().and_then(|point| i64::try_from(point).ok()) {
                let hole = range_type(module, IntRange::singleton(point));
                self.refine(
                    when_false,
                    var,
                    (None, other_reason),
                    Branch::Else,
                    span,
                    |ty| (ty.minus(&hole, module), Vec::new()),
                );
            }
        } else {
            let else_type = range_type(module, else_bound.range());
            self.refine(
                when_false,
                var,
                (None, other_reason),
                Branch::Else,
                span,
                |ty| (ty.meet(&else_type, module), Vec::new()),
            );
        }
    }

    /// Narrows `var` in `env` to what `narrow` returns for its current
    /// type, which also names the sealed classes the narrowing relied on;
    /// the narrowed fact cites the fact it narrows, the fact `by` of what
    /// it was compared against, if any, and those classes' assumptions.
    fn refine(
        &mut self,
        env: &mut Env,
        var: Var,
        (site, by): (Option<SiteId>, Option<ReasonId>),
        branch: Branch,
        span: Span,
        narrow: impl FnOnce(&Type) -> (Type, Vec<ClassId>),
    ) {
        let Some(current) = env.get(var).cloned() else {
            return;
        };
        let (ty, sealed) = narrow(&current.ty);
        if ty == current.ty {
            return;
        }
        let mut from = vec![current.reason];
        from.extend(by);
        from.extend(self.sealed_assumptions(&sealed));
        let reason = self.reason(ReasonKind::Narrowed { site, branch }, span, &ty, from);
        if ty.is_bottom() {
            env.kill(reason);
        }
        env.set(var, Fact { ty, reason });
    }

    /// The integer local or capture whose value `expr` is: the binding
    /// itself, or an integer conversion or range check of it that typing
    /// proved in range.
    ///
    /// A conversion that may fail is not looked through: a failure signals
    /// a `<type-error>` whose `use-value` restart supplies some other value,
    /// so what the conversion yields says nothing about its operand.
    fn integer_var_of(&self, expr: &sem::Expr) -> Option<Var> {
        match expr {
            sem::Expr::Convert { value, site, .. } | sem::Expr::CheckRange { value, site, .. }
                if is_integer_expr(self.module, value) && self.proved_here(*site) =>
            {
                self.integer_var_of(value)
            }
            _ => var_of(expr),
        }
    }

    /// Whether the check at `site`, already evaluated in this definition,
    /// was proved: it never fails, so it yields its operand.
    fn proved_here(&self, site: SiteId) -> bool {
        self.decisions
            .iter()
            .rev()
            .find(|decision| decision.site == site)
            .is_some_and(|decision| decision.verdict == TypingVerdict::Proved)
    }

    /// The type of a comparison operand without evaluating it again: a
    /// local's or capture's fact, a literal's value, a proved conversion's
    /// operand, or sem's category.
    fn peek(&self, env: &Env, expr: &sem::Expr) -> Option<(Type, Option<ReasonId>)> {
        match expr {
            sem::Expr::Local { .. } | sem::Expr::Capture { .. } => env
                .get(var_of(expr)?)
                .map(|fact| (fact.ty.clone(), Some(fact.reason))),
            sem::Expr::ConstI64 { value, .. } => {
                Some((range_type(self.module, IntRange::singleton(*value)), None))
            }
            sem::Expr::Const32 {
                value: Number32::I32(value),
                ..
            } => Some((
                range_type(self.module, IntRange::singleton(i64::from(*value))),
                None,
            )),
            sem::Expr::Const32 {
                value: Number32::U32(value),
                ..
            } => Some((
                range_type(self.module, IntRange::singleton(i64::from(*value))),
                None,
            )),
            sem::Expr::Convert { value, site, .. } | sem::Expr::CheckRange { value, site, .. }
                if is_integer_expr(self.module, value) && self.proved_here(*site) =>
            {
                self.peek(env, value)
            }
            _ => Some((self.bound(expr), None)),
        }
    }

    fn join_facts(&mut self, left: Fact, right: Fact, span: Span) -> Fact {
        if left.ty.is_bottom() && right.ty.is_bottom() {
            let reason = self.join_dead(left.reason, right.reason, span);
            return Fact {
                ty: Type::bottom(),
                reason,
            };
        }
        if right.ty.is_bottom() {
            return self.only_live(left, right.reason, span);
        }
        if left.ty.is_bottom() {
            return self.only_live(right, left.reason, span);
        }
        if left.reason == right.reason {
            return right;
        }
        // Both sides are one fact, one of them with more cited: the join is
        // that fact, citing what either relies on.
        if self.principal(left.reason) == self.principal(right.reason) {
            if self.covers(right.reason, left.reason) {
                return right;
            }
            if self.covers(left.reason, right.reason) {
                return left;
            }
            let ty = left.ty;
            return self.fact(ReasonKind::Kept, span, ty, vec![left.reason, right.reason]);
        }
        let ty = left.ty.join(&right.ty, self.module);
        self.fact(
            ReasonKind::Joined,
            span,
            ty,
            vec![left.reason, right.reason],
        )
    }

    /// Joins the environments of two paths that meet. A binding missing on
    /// either path is out of scope after the join.
    fn join_env(&mut self, left: Env, right: Env, span: Span) -> Env {
        match (left.dead, right.dead) {
            (Some(left_dead), Some(right_dead)) => {
                // A binding either dead path changed is marked as changed,
                // so a join with a live path cites why both are dead.
                let dead = self.join_dead(left_dead, right_dead, span);
                let mark = |left: Vec<Option<Fact>>, right: Vec<Option<Fact>>| {
                    left.into_iter()
                        .zip(right)
                        .map(|(left, right)| match (left, right) {
                            (Some(left), Some(right)) if left.reason == right.reason => Some(left),
                            (None, None) => None,
                            _ => Some(Fact {
                                ty: Type::bottom(),
                                reason: dead,
                            }),
                        })
                        .collect()
                };
                return Env {
                    dead: Some(dead),
                    locals: mark(left.locals, right.locals),
                    captures: mark(left.captures, right.captures),
                };
            }
            (None, Some(dead)) => return self.only_live_env(left, &right, dead, span),
            (Some(dead), None) => return self.only_live_env(right, &left, dead, span),
            (None, None) => {}
        }
        let Env {
            locals: left_locals,
            captures: left_captures,
            ..
        } = left;
        let Env {
            locals: right_locals,
            captures: right_captures,
            ..
        } = right;
        Env {
            dead: None,
            locals: self.join_slots(left_locals, right_locals, span),
            captures: self.join_slots(left_captures, right_captures, span),
        }
    }

    /// Whether `dead`, why a path is dead, is an exit's transfer, which
    /// relies on nothing.
    fn is_exit(&self, dead: ReasonId) -> bool {
        self.reasons
            .get(dead.0 as usize)
            .is_some_and(|reason| matches!(reason.kind, ReasonKind::Exit))
    }

    /// Why no value reaches where two dead paths meet: what either path's
    /// reason relies on, an exit's relying on nothing.
    fn join_dead(&mut self, left: ReasonId, right: ReasonId, span: Span) -> ReasonId {
        if left == right || self.is_exit(right) || self.covers(left, right) {
            return left;
        }
        if self.is_exit(left) || self.covers(right, left) {
            return right;
        }
        self.reason(ReasonKind::Kept, span, &Type::bottom(), vec![left, right])
    }

    /// The fact `live` where it joins a path no value reaches because of
    /// `dead`. Values arrive only by the live path, which the join relies
    /// on as it relies on why the other path is dead, such as a sealing that
    /// let a test rule it out. An exit's transfer relies on nothing.
    fn only_live(&mut self, live: Fact, dead: ReasonId, span: Span) -> Fact {
        if live.ty.is_bottom() || self.is_exit(dead) || self.covers(live.reason, dead) {
            return live;
        }
        let ty = live.ty.clone();
        self.fact(ReasonKind::Kept, span, ty, vec![live.reason, dead])
    }

    /// [`Self::only_live`] for the bindings of the environment `live` that
    /// the dead path's environment `unreached` holds other facts for: a
    /// binding the dead path left as it was would join to the same fact.
    fn only_live_env(&mut self, mut live: Env, unreached: &Env, dead: ReasonId, span: Span) -> Env {
        for (slots, other) in [
            (&mut live.locals, &unreached.locals),
            (&mut live.captures, &unreached.captures),
        ] {
            let taken = core::mem::take(slots);
            *slots = taken
                .into_iter()
                .enumerate()
                .map(|(index, fact)| {
                    let fact = fact?;
                    let untouched = other
                        .get(index)
                        .and_then(Option::as_ref)
                        .is_some_and(|other| other.reason == fact.reason);
                    Some(if untouched {
                        fact
                    } else {
                        self.only_live(fact, dead, span)
                    })
                })
                .collect();
        }
        live
    }

    fn join_slots(
        &mut self,
        left: Vec<Option<Fact>>,
        right: Vec<Option<Fact>>,
        span: Span,
    ) -> Vec<Option<Fact>> {
        left.into_iter()
            .zip(right)
            .map(|(left, right)| {
                let (left, right) = (left?, right?);
                if left.reason == right.reason {
                    return Some(left);
                }
                let ty = left.ty.join(&right.ty, self.module);
                // A side whose type is the join explains it alone, when it
                // relies on everything the other side does: values arrive
                // by both paths, so the join relies on both derivations.
                // When both sides have the type, the one citing more is
                // kept, so a claim names that side's cause.
                if ty == left.ty && self.covers(left.reason, right.reason) {
                    Some(left)
                } else if ty == right.ty && self.covers(right.reason, left.reason) {
                    Some(right)
                } else if ty == left.ty {
                    Some(self.fact(ReasonKind::Kept, span, ty, vec![left.reason, right.reason]))
                } else if ty == right.ty {
                    Some(self.fact(ReasonKind::Kept, span, ty, vec![right.reason, left.reason]))
                } else {
                    Some(self.fact(
                        ReasonKind::Joined,
                        span,
                        ty,
                        vec![left.reason, right.reason],
                    ))
                }
            })
            .collect()
    }

    /// The reason that explains `reason`'s type, through kept joins; see
    /// [`super::DefinitionTyping::principal_reason`].
    fn principal(&self, reason: ReasonId) -> ReasonId {
        super::principal_reason(&self.reasons, reason)
    }

    /// Whether the chain of `reason` cites every closed-world assumption
    /// and contract the chain of `other` does.
    fn covers(&self, reason: ReasonId, other: ReasonId) -> bool {
        subset(&self.cited, other, reason) && subset(&self.contracts, other, reason)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a decision is assembled from every part of its check"
    )]
    fn record(
        &mut self,
        site: SiteId,
        (check, origin): (TypingCheck, CheckOrigin),
        (verdict, rule): (TypingVerdict, TypingRule),
        span: Span,
        expected: TypingExpectation,
        operands: &[Fact],
        assumes: Vec<ReasonId>,
        selected_method: Option<sem::MethodId>,
    ) {
        let mut unique = Vec::with_capacity(assumes.len());
        for assumption in assumes {
            if !unique.contains(&assumption) {
                unique.push(assumption);
            }
        }
        let mut assumes = unique;
        // A verdict relies on whatever its operands' facts relied on.
        for cited in self.cited_assumptions(operands) {
            if !assumes.contains(&cited) {
                assumes.push(cited);
            }
        }
        self.decisions.push(TypingDecision {
            owner: self.owner,
            site,
            check,
            origin,
            verdict,
            rule,
            span,
            expected,
            actual: operands.iter().map(|fact| fact.ty.clone()).collect(),
            because: operands.iter().map(|fact| fact.reason).collect(),
            assumes,
            selected_method,
        });
    }

    /// The closed-world assumptions the reason chains of `operands` cite,
    /// in operand order.
    fn cited_assumptions(&self, operands: &[Fact]) -> Vec<ReasonId> {
        let mut out = Vec::new();
        for operand in operands {
            for assumption in self
                .cited
                .get(operand.reason.0 as usize)
                .into_iter()
                .flatten()
            {
                if !out.contains(assumption) {
                    out.push(*assumption);
                }
            }
        }
        out
    }

    /// Records the dispatch decision for a send of `generic` on `args`.
    ///
    /// Returns the method typing proved every call selects, with the
    /// reasons the selection relies on (the arguments and the closed-world
    /// assumptions), when there is one.
    fn dispatch(
        &mut self,
        env: &Env,
        generic: GenericFunctionId,
        args: &[Fact],
        span: Span,
        site: SiteId,
    ) -> Option<Selected> {
        let module = self.module;
        let methods = module
            .methods
            .iter()
            .filter(|method| method.generic == generic)
            .collect::<Vec<_>>();
        let actual = args.iter().map(|fact| fact.ty.clone()).collect::<Vec<_>>();
        let mut sealed = Vec::new();
        let decided = if env.dead.is_some() {
            (TypingVerdict::Proved, TypingRule::Unreachable)
        } else {
            let specializers = methods
                .iter()
                .map(|method| {
                    method
                        .specializers
                        .iter()
                        .map(|specializer| specializer_type(module, *specializer))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            // Distribute over union members first; while that leaves the
            // runtime to decide, split the members' sealed cones one level
            // further, so only the sealing that mattered is relied on.
            let mut members = dispatch_members(module, &actual);
            let mut verdict = dispatch_verdict(module, &specializers, &actual, &members);
            while verdict == TypingVerdict::Checked {
                let (next, split) = split_members(module, &members);
                if split.is_empty() {
                    break;
                }
                extend_unique(&mut sealed, split);
                verdict = dispatch_verdict(module, &specializers, &actual, &next);
                members = next;
            }
            if verdict == TypingVerdict::Checked {
                sealed.clear();
            }
            let rule = match verdict {
                TypingVerdict::Proved => TypingRule::DispatchCovered,
                TypingVerdict::Checked => TypingRule::DispatchNeedsRuntimeClass,
                TypingVerdict::Refuted => TypingRule::DispatchNoApplicableMethod,
            };
            (verdict, rule)
        };
        // A covered send selects one method when every class its arguments
        // may have, once sealed cones are split, selects the same one.
        let selected_method = (decided == (TypingVerdict::Proved, TypingRule::DispatchCovered))
            .then(|| super::exact_class_tuples(module, &actual))
            .flatten()
            .and_then(|(tuples, split)| {
                let mut chosen = None;
                for tuple in &tuples {
                    let sem::MethodSelection::Selected(method) =
                        module.select_method_for_classes(generic, tuple)
                    else {
                        return None;
                    };
                    if chosen
                        .replace(method)
                        .is_some_and(|earlier| earlier != method)
                    {
                        return None;
                    }
                }
                extend_unique(&mut sealed, split);
                chosen
            });
        let mut assumes = vec![self.assume(Assumption::Generic(generic))];
        let types = actual.iter().collect::<Vec<_>>();
        assumes.extend(self.class_assumptions(&types));
        assumes.extend(self.sealed_assumptions(&sealed));
        let selected = (decided == (TypingVerdict::Proved, TypingRule::DispatchCovered))
            .then_some(selected_method)
            .flatten()
            .map(|method| Selected {
                method,
                because: args
                    .iter()
                    .map(|fact| fact.reason)
                    .chain(assumes.iter().copied())
                    .collect(),
            });
        self.record(
            site,
            (TypingCheck::Dispatch, CheckOrigin::Implicit),
            decided,
            span,
            TypingExpectation::Dispatch {
                generic,
                methods: methods.iter().map(|method| method.id).collect(),
                boxed_params: (0..args.len())
                    .filter(|index| module.generic_param_boxes_scalars(generic, *index))
                    .collect(),
            },
            args,
            assumes,
            selected_method,
        );
        selected
    }

    /// The result of a send to a built-in numeric generic whose method
    /// `selected` names: the type of the method's result category, such as
    /// `<integer>` for `<integer> + <integer>`, instead of the generic's
    /// `<object>` contract.
    ///
    /// A numeric generic is sealed over `<real>` and its methods come from
    /// the promotion rules, so the method set fixes what the send runs; the
    /// fact cites the dispatch's closed-world assumption. This is what lets
    /// a chain of such sends stay proved, and unboxed, link by link.
    fn numeric_result(
        &mut self,
        expr: &sem::Expr,
        generic: &sem::GenericFunction,
        selected: Selected,
        site: SiteId,
    ) -> Option<Fact> {
        if !matches!(generic.origin, sem::GenericFunctionOrigin::Numeric { .. }) {
            return None;
        }
        let method = self.module.methods.get(selected.method.0 as usize)?;
        let [kind] = method.result_kinds.as_slice() else {
            return None;
        };
        if method.generic != generic.id
            || !matches!(method.origin, sem::MethodOrigin::Numeric { .. })
        {
            return None;
        }
        let ty = self
            .module
            .kind_type(*kind)
            .meet(&self.bound(expr), self.module);
        if ty.is_bottom() {
            return None;
        }
        Some(self.fact(
            ReasonKind::Dispatched { site },
            expr.span(),
            ty,
            selected.because,
        ))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the operands, site, and kind bound of one arithmetic check"
    )]
    fn binary(
        &mut self,
        env: &Env,
        op: BinaryOp,
        kind: NumericKind,
        lhs: Fact,
        rhs: Fact,
        site: SiteId,
        span: Span,
        bound: Type,
    ) -> Fact {
        let module = self.module;
        let unreachable = (TypingVerdict::Proved, TypingRule::Unreachable);
        let expected_range = representable(kind);
        if op.is_shift() {
            let counts = range_type(module, SHIFT_COUNTS);
            let decided = if env.dead.is_some() {
                unreachable
            } else {
                match rhs.ty.relate(&counts, module) {
                    Relation::Always => (TypingVerdict::Proved, TypingRule::ShiftCountInRange),
                    Relation::Maybe => (TypingVerdict::Checked, TypingRule::ShiftCountUnchecked),
                    Relation::Never => (TypingVerdict::Refuted, TypingRule::ShiftCountOutOfRange),
                }
            };
            let from = vec![lhs.reason, rhs.reason];
            self.record(
                site,
                (TypingCheck::Overflow, CheckOrigin::Implicit),
                decided,
                span,
                TypingExpectation::Type(counts),
                &[lhs, rhs],
                Vec::new(),
                None,
            );
            // A count outside the range signals a `<type-error>`, and the
            // shift continues with whatever result a `use-value` restart
            // supplies, so even a refuted check yields its kind's values.
            return self.fact(ReasonKind::Check { site }, span, bound, from);
        }
        if op.is_bitwise() {
            let decided = if env.dead.is_some() {
                unreachable
            } else {
                (TypingVerdict::Proved, TypingRule::CannotTrap)
            };
            let from = vec![lhs.reason, rhs.reason];
            self.record(
                site,
                (TypingCheck::Overflow, CheckOrigin::Implicit),
                decided,
                span,
                TypingExpectation::Type(range_type(module, expected_range)),
                &[lhs, rhs],
                Vec::new(),
                None,
            );
            return self.fact(ReasonKind::Declared, span, bound, from);
        }
        let left = Interval::hull(&lhs.ty).unwrap_or(Interval::UNBOUNDED);
        let right = Interval::hull(&rhs.ty).unwrap_or(Interval::UNBOUNDED);
        let interval = match op {
            BinaryOp::Add | BinaryOp::WrappingAdd => left.add(right),
            BinaryOp::Sub | BinaryOp::WrappingSub => left.sub(right),
            BinaryOp::Mul | BinaryOp::WrappingMul => left.mul(right),
            _ => Interval::UNBOUNDED,
        };
        if op.is_wrapping() {
            let exact = range_type(module, interval.range());
            let computed = self.fact(
                ReasonKind::Interval,
                span,
                exact,
                vec![lhs.reason, rhs.reason],
            );
            let decided = if env.dead.is_some() {
                unreachable
            } else {
                (TypingVerdict::Proved, TypingRule::CannotTrap)
            };
            self.record(
                site,
                (TypingCheck::Overflow, CheckOrigin::Implicit),
                decided,
                span,
                TypingExpectation::Type(range_type(module, expected_range)),
                &[lhs, rhs],
                Vec::new(),
                None,
            );
            // Out-of-range results wrap to anywhere in the kind.
            return if interval.within(expected_range) {
                computed
            } else {
                self.fact(
                    ReasonKind::Check { site },
                    span,
                    bound,
                    vec![computed.reason],
                )
            };
        }
        self.arithmetic(env, kind, interval, vec![lhs, rhs], site, span, bound)
    }

    /// Records the overflow check of a checked integer operation whose
    /// exact result is in `interval`, and returns the values it yields.
    #[expect(
        clippy::too_many_arguments,
        reason = "the operands, site, and kind bound of one arithmetic check"
    )]
    fn arithmetic(
        &mut self,
        env: &Env,
        kind: NumericKind,
        interval: Interval,
        operands: Vec<Fact>,
        site: SiteId,
        span: Span,
        bound: Type,
    ) -> Fact {
        let module = self.module;
        let expected_range = representable(kind);
        let exact = range_type(module, interval.range());
        let from = operands.iter().map(|fact| fact.reason).collect();
        let computed = self.fact(ReasonKind::Interval, span, exact, from);
        let decided = if env.dead.is_some() {
            (TypingVerdict::Proved, TypingRule::Unreachable)
        } else if interval.within(expected_range) {
            (TypingVerdict::Proved, TypingRule::OverflowImpossible)
        } else if interval.disjoint(expected_range) {
            (TypingVerdict::Refuted, TypingRule::OverflowCertain)
        } else {
            (TypingVerdict::Checked, TypingRule::OverflowPossible)
        };
        self.record(
            site,
            (TypingCheck::Overflow, CheckOrigin::Implicit),
            decided,
            span,
            TypingExpectation::Type(range_type(module, expected_range)),
            &operands,
            Vec::new(),
            None,
        );
        match decided.0 {
            TypingVerdict::Proved => computed,
            // An overflow signals a `<type-error>`, and the operation
            // continues with whatever result a `use-value` restart supplies,
            // so a result that may (or must) overflow has the full range of
            // its kind.
            TypingVerdict::Checked | TypingVerdict::Refuted => self.fact(
                ReasonKind::Check { site },
                span,
                bound,
                vec![computed.reason],
            ),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the operand, kinds, site, and bound of one conversion"
    )]
    fn convert(
        &mut self,
        env: &Env,
        from: Option<NumericKind>,
        to: NumericKind,
        value: Fact,
        (site, explicit): (SiteId, bool),
        span: Span,
        bound: Type,
    ) -> Fact {
        let module = self.module;
        let decided = match from {
            _ if env.dead.is_some() => (TypingVerdict::Proved, TypingRule::Unreachable),
            Some(from) if from.is_integer() && to.is_integer() => {
                if integer_range(to).includes(integer_range(from)) {
                    (TypingVerdict::Proved, TypingRule::CannotTrap)
                } else {
                    decide(module, env, &value.ty, &bound, RANGE_RULES)
                }
            }
            Some(from) if from.is_float() && to.is_integer() => {
                (TypingVerdict::Checked, TypingRule::FloatToInteger)
            }
            _ => (TypingVerdict::Proved, TypingRule::CannotTrap),
        };
        let integral = from.is_some_and(NumericKind::is_integer) && to.is_integer();
        // A value out of range signals a `<type-error>` whose `use-value`
        // restart may supply any value of the target type, so only a proved
        // conversion keeps the operand's narrower range.
        let ty = if integral && decided.0 == TypingVerdict::Proved {
            value.ty.meet(&bound, module)
        } else {
            bound.clone()
        };
        let from_reasons = vec![value.reason];
        self.record(
            site,
            (TypingCheck::Range, CheckOrigin::of(explicit)),
            decided,
            span,
            TypingExpectation::Type(bound),
            &[value],
            Vec::new(),
            None,
        );
        self.fact(ReasonKind::Check { site }, span, ty, from_reasons)
    }

    /// Classifies the check that the integer `value` is in a limited type's
    /// `range`.
    fn check_range(
        &mut self,
        env: &Env,
        value: Fact,
        range: IntRange,
        (site, explicit): (SiteId, bool),
        span: Span,
    ) -> Fact {
        let module = self.module;
        let target = range_type(module, range);
        let decided = decide(module, env, &value.ty, &target, RANGE_RULES);
        // A value out of range signals a `<type-error>` whose `use-value`
        // replacement is checked again, so the result is always in range,
        // but only a proved check keeps the operand's narrower range.
        let ty = if decided.0 == TypingVerdict::Proved {
            value.ty.meet(&target, module)
        } else {
            target.clone()
        };
        let from = vec![value.reason];
        self.record(
            site,
            (TypingCheck::Range, CheckOrigin::of(explicit)),
            decided,
            span,
            TypingExpectation::Type(target),
            &[value],
            Vec::new(),
            None,
        );
        self.fact(ReasonKind::Check { site }, span, ty, from)
    }
}

/// Rules for a check that relates one operand to an expected type, by
/// relation: always, maybe, never.
type RelationRules = [TypingRule; 3];

const CAST_RULES: RelationRules = [
    TypingRule::CastUpcast,
    TypingRule::CastNarrowing,
    TypingRule::CastDisjoint,
];

const INSTANCE_RULES: RelationRules = [
    TypingRule::InstanceAlways,
    TypingRule::InstanceSometimes,
    TypingRule::InstanceNever,
];

const RANGE_RULES: RelationRules = [
    TypingRule::RangeWithin,
    TypingRule::RangeOverlaps,
    TypingRule::RangeDisjoint,
];

/// Decides a check of `operand` against `expected`.
fn decide(
    module: &sem::Module,
    env: &Env,
    operand: &Type,
    expected: &Type,
    [always, maybe, never]: RelationRules,
) -> (TypingVerdict, TypingRule) {
    if env.dead.is_some() {
        return (TypingVerdict::Proved, TypingRule::Unreachable);
    }
    match operand.relate(expected, module) {
        Relation::Always => (TypingVerdict::Proved, always),
        Relation::Maybe => (TypingVerdict::Checked, maybe),
        Relation::Never => (TypingVerdict::Refuted, never),
    }
}

/// Decides a check of `operand` against the class type `expected`, splitting
/// the operand's sealed cones when that decides more
/// ([`Type::relate_sealed`]); also returns the sealed classes the verdict
/// relies on.
fn decide_sealed(
    module: &sem::Module,
    env: &Env,
    operand: &Type,
    expected: &Type,
    [always, maybe, never]: RelationRules,
) -> ((TypingVerdict, TypingRule), Vec<ClassId>) {
    if env.dead.is_some() {
        return ((TypingVerdict::Proved, TypingRule::Unreachable), Vec::new());
    }
    let (relation, sealed) = operand.relate_sealed(expected, module);
    let decided = match relation {
        Relation::Always => (TypingVerdict::Proved, always),
        Relation::Maybe => (TypingVerdict::Checked, maybe),
        Relation::Never => (TypingVerdict::Refuted, never),
    };
    (decided, sealed)
}

/// Decides whether `operand` is a class object naming `bound` or one of
/// its subclasses, by the cast rules.
fn decide_class_object(
    module: &sem::Module,
    env: &Env,
    operand: &Type,
    bound: ClassId,
) -> (TypingVerdict, TypingRule) {
    if env.dead.is_some() {
        return (TypingVerdict::Proved, TypingRule::Unreachable);
    }
    let class_class = module.builtin_value_class(sem::BuiltinValueClass::Class);
    let relation = operand.atoms().map_or(Relation::Maybe, |atoms| {
        atoms
            .iter()
            .map(|atom| match atom {
                Atom::Singleton(Metaobject::Class(class)) => {
                    if module.is_same_or_subclass(*class, bound) {
                        Relation::Always
                    } else {
                        Relation::Never
                    }
                }
                // Instances of a class that may be `<class>` may be any
                // class object.
                Atom::Cone(class) | Atom::Exact(class)
                    if class_class.is_some_and(|class_class| {
                        module.classes_may_overlap(*class, class_class)
                    }) =>
                {
                    Relation::Maybe
                }
                _ => Relation::Never,
            })
            .reduce(|left, right| match (left, right) {
                (Relation::Always, Relation::Always) => Relation::Always,
                (Relation::Never, Relation::Never) => Relation::Never,
                _ => Relation::Maybe,
            })
            .unwrap_or(Relation::Always)
    });
    let [always, maybe, never] = CAST_RULES;
    match relation {
        Relation::Always => (TypingVerdict::Proved, always),
        Relation::Maybe => (TypingVerdict::Checked, maybe),
        Relation::Never => (TypingVerdict::Refuted, never),
    }
}

/// The members a dispatch distributes over, per argument: each union
/// member on its own, or the whole type for [`Type::top`].
pub(super) fn dispatch_members(module: &sem::Module, actual: &[Type]) -> Vec<Vec<Type>> {
    actual
        .iter()
        .map(|ty| match ty.atoms() {
            None => vec![ty.clone()],
            Some(atoms) => atoms
                .iter()
                .map(|atom| Type::of(atom.clone(), module))
                .collect(),
        })
        .collect()
}

/// Replaces every member that is a cone over a sealed class by its pieces,
/// one level; also returns the classes split.
pub(super) fn split_members(
    module: &sem::Module,
    members: &[Vec<Type>],
) -> (Vec<Vec<Type>>, Vec<ClassId>) {
    let mut split = Vec::new();
    let next = members
        .iter()
        .map(|members| {
            members
                .iter()
                .flat_map(|member| {
                    if let Some([Atom::Cone(class)]) = member.atoms()
                        && let Some(pieces) = sealed_pieces(*class, module)
                    {
                        if !split.contains(class) {
                            split.push(*class);
                        }
                        // A sealed leaf piece is exact because it has no
                        // subclasses.
                        for piece in &pieces {
                            if let Atom::Exact(leaf) = piece
                                && module
                                    .classes
                                    .get(leaf.0 as usize)
                                    .is_some_and(|record| record.sealing.is_sealed())
                                && !split.contains(leaf)
                            {
                                split.push(*leaf);
                            }
                        }
                        return pieces
                            .into_iter()
                            .map(|piece| Type::of(piece, module))
                            .collect();
                    }
                    vec![member.clone()]
                })
                .collect()
        })
        .collect();
    (next, split)
}

/// The verdict of a dispatch over every combination of `members`, or over
/// the whole `actual` types when there are more than
/// [`super::MAX_DISPATCH_TUPLES`] combinations.
fn dispatch_verdict(
    module: &sem::Module,
    specializers: &[Vec<Type>],
    actual: &[Type],
    members: &[Vec<Type>],
) -> TypingVerdict {
    let count = members
        .iter()
        .try_fold(1_usize, |count, members| count.checked_mul(members.len()))
        .unwrap_or(usize::MAX);
    let tuples = if count > super::MAX_DISPATCH_TUPLES {
        vec![actual.to_vec()]
    } else {
        product(members)
    };
    tuples
        .iter()
        .map(|tuple| {
            let relation = specializers
                .iter()
                .filter(|specializers| specializers.len() == tuple.len())
                .map(|specializers| {
                    specializers
                        .iter()
                        .zip(tuple)
                        .fold(Relation::Always, |acc, (specializer, arg)| {
                            all_hold(acc, arg.relate(specializer, module))
                        })
                })
                .fold(Relation::Never, any_holds);
            verdict_of(relation)
        })
        .reduce(TypingVerdict::join)
        .unwrap_or(TypingVerdict::Checked)
}

/// Appends the classes of `more` that `classes` lacks.
fn extend_unique(classes: &mut Vec<ClassId>, more: Vec<ClassId>) {
    for class in more {
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
}

/// Every tuple taking one member of each list, in order.
pub(super) fn product<T: Clone>(lists: &[Vec<T>]) -> Vec<Vec<T>> {
    let mut tuples = vec![Vec::new()];
    for members in lists {
        let mut next = Vec::with_capacity(tuples.len().saturating_mul(members.len()));
        for tuple in &tuples {
            for member in members {
                let mut extended = tuple.clone();
                extended.push(member.clone());
                next.push(extended);
            }
        }
        tuples = next;
    }
    tuples
}

/// The values of an integer kind's type.
const fn integer_range(kind: NumericKind) -> IntRange {
    match kind {
        NumericKind::I32 => IntRange::I32,
        NumericKind::U32 => IntRange::U32,
        NumericKind::Integer | NumericKind::Float | NumericKind::SingleFloat => IntRange::INTEGER,
    }
}

/// The results an integer operation of `kind` produces without trapping.
const fn representable(kind: NumericKind) -> IntRange {
    match kind {
        NumericKind::I32 => IntRange::I32,
        NumericKind::U32 => IntRange::U32,
        NumericKind::Integer | NumericKind::Float | NumericKind::SingleFloat => INTEGER_REGISTER,
    }
}

/// The type a declaration of `kind` gives its values: the kind's, within
/// `range` when it declares a limited integer type.
pub(super) fn declared(module: &sem::Module, kind: ValueKind, range: Option<IntRange>) -> Type {
    let ty = module.kind_type(kind);
    match range {
        Some(range) => ty.meet(&range_type(module, range), module),
        None => ty,
    }
}

/// The check site an expression leaves, if any.
const fn site_of(expr: &sem::Expr) -> Option<SiteId> {
    match expr {
        sem::Expr::GenericCall { site, .. }
        | sem::Expr::Cast { site, .. }
        | sem::Expr::InstanceOf { site, .. }
        | sem::Expr::InstanceOfLimited { site, .. }
        | sem::Expr::Convert { site, .. }
        | sem::Expr::CheckRange { site, .. } => Some(*site),
        sem::Expr::Binary { site, .. } | sem::Expr::Negate { site, .. } => *site,
        _ => None,
    }
}

const fn span_len(span: Span) -> usize {
    span.end.saturating_sub(span.start)
}

fn var_of(expr: &sem::Expr) -> Option<Var> {
    match expr {
        sem::Expr::Local { local, .. } => Some(Var::Local(*local)),
        sem::Expr::Capture { capture, .. } => Some(Var::Capture(*capture)),
        _ => None,
    }
}

fn is_integer_expr(module: &sem::Module, expr: &sem::Expr) -> bool {
    NumericKind::of(module.expr_kind(expr)).is_some_and(NumericKind::is_integer)
}

/// Names the integer operation at check site `site` of `owner`, such as
/// `+` or `shift-left`.
pub(super) fn operation_at(
    module: &sem::Module,
    owner: DefinitionId,
    site: SiteId,
) -> Option<String> {
    let mut found = None;
    let mut visit = |expr: &sem::Expr| match expr {
        sem::Expr::Binary {
            op, site: Some(at), ..
        } if *at == site => {
            found = Some(String::from(op.symbol()));
        }
        sem::Expr::Negate { site: Some(at), .. } if *at == site => {
            found = Some(String::from("-"));
        }
        _ => {}
    };
    if let Some(function) = module.functions.iter().find(|f| f.definition == owner) {
        walk_block(module, &function.body, &mut visit);
    } else if let Some(method) = module.methods.iter().find(|m| m.definition == owner) {
        walk_block(module, &method.body, &mut visit);
    } else if let Some(constant) = module.constants.iter().find(|c| c.definition == owner) {
        walk(module, &constant.value, &mut visit);
    }
    found
}

fn walk_block(module: &sem::Module, block: &sem::Block, visit: &mut impl FnMut(&sem::Expr)) {
    for statement in &block.statements {
        match statement {
            sem::Statement::Let(binding) => walk(module, &binding.value, visit),
            sem::Statement::Expr(expr) => walk(module, expr, visit),
        }
    }
    walk(module, &block.result, visit);
}

/// Visits `expr` and every expression inside it, including closure bodies.
fn walk(module: &sem::Module, expr: &sem::Expr, visit: &mut impl FnMut(&sem::Expr)) {
    use sem::Expr as E;
    visit(expr);
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
                walk(module, item, visit);
            }
        }
        E::CallableCall { callable, args, .. } => {
            walk(module, callable, visit);
            for arg in args {
                walk(module, arg, visit);
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
        } => walk(module, inner, visit),
        E::MakeObject { class, fields, .. } => {
            walk(module, class, visit);
            for field in fields {
                walk(module, &field.value, visit);
            }
        }
        E::MakeRuntimeClass {
            class,
            init_args,
            candidates,
            ..
        } => {
            walk(module, class, visit);
            for arg in init_args {
                walk(module, &arg.value, visit);
            }
            for field in candidates.iter().flat_map(|candidate| &candidate.defaults) {
                walk(module, &field.value, visit);
            }
        }
        E::SlotWrite {
            receiver, value, ..
        } => {
            walk(module, receiver, visit);
            walk(module, value, visit);
        }
        E::AssignLocal { value, .. } => walk(module, value, visit),
        E::Loop { test, body, .. } => {
            walk(module, test, visit);
            walk_block(module, body, visit);
        }
        E::Binary { lhs, rhs, .. }
        | E::Compare { lhs, rhs, .. }
        | E::MixedCompare { lhs, rhs, .. }
        | E::ValueEqual { lhs, rhs, .. }
        | E::Identical { lhs, rhs, .. } => {
            walk(module, lhs, visit);
            walk(module, rhs, visit);
        }
        E::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            walk(module, condition, visit);
            walk(module, then_branch, visit);
            walk(module, else_branch, visit);
        }
        E::Discard { value, .. } => walk(module, value, visit),
        E::Handle { body, restart, .. } => {
            walk(module, body, visit);
            if let Some(guard) = &restart.guard {
                walk(module, guard, visit);
            }
            walk(module, &restart.body, visit);
        }
        E::Block { body, cleanup, .. } => {
            walk_block(module, body, visit);
            if let Some(cleanup) = cleanup {
                walk_block(module, cleanup, visit);
            }
        }
        E::Closure { closure, .. } => {
            if let Some(closure) = module.closures.get(closure.0 as usize) {
                walk_block(module, &closure.body, visit);
            }
        }
        E::ConstI64 { .. }
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
}

/// Whether `arena`'s entry for `inner` is a subset of its entry for
/// `outer`.
fn subset<T: PartialEq>(arena: &[Vec<T>], inner: ReasonId, outer: ReasonId) -> bool {
    let outer = arena.get(outer.0 as usize).map_or(&[][..], Vec::as_slice);
    arena
        .get(inner.0 as usize)
        .into_iter()
        .flatten()
        .all(|item| outer.contains(item))
}

const fn contract_source(declared: bool) -> ContractSource {
    if declared {
        ContractSource::Declared
    } else {
        ContractSource::Inferred
    }
}

/// Combines relations to expectations that must all hold.
const fn all_hold(left: Relation, right: Relation) -> Relation {
    match (left, right) {
        (Relation::Never, _) | (_, Relation::Never) => Relation::Never,
        (Relation::Maybe, _) | (_, Relation::Maybe) => Relation::Maybe,
        (Relation::Always, Relation::Always) => Relation::Always,
    }
}

/// Combines relations to alternative expectations where any one suffices.
const fn any_holds(left: Relation, right: Relation) -> Relation {
    match (left, right) {
        (Relation::Always, _) | (_, Relation::Always) => Relation::Always,
        (Relation::Maybe, _) | (_, Relation::Maybe) => Relation::Maybe,
        (Relation::Never, Relation::Never) => Relation::Never,
    }
}

/// Verdict for a check whose operand relates to its expectation this way.
const fn verdict_of(relation: Relation) -> TypingVerdict {
    match relation {
        Relation::Always => TypingVerdict::Proved,
        Relation::Maybe => TypingVerdict::Checked,
        Relation::Never => TypingVerdict::Refuted,
    }
}
