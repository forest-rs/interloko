//! Strict typing: which definitions must prove their checks, what each
//! unproved check means for them, and the edits that could prove it.
//!
//! See *Strict typing* in the [module documentation](super).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use interloko_model::types::{Atom, IntRange, Type};

use super::{
    Binding, CheckOrigin, DefinitionTyping, TypingCheck, TypingDecision, TypingExpectation,
    TypingRule, TypingVerdict, class_name, generic_name, specializer_type, type_name,
};
use crate::identity::DefinitionId;
use crate::sem::{self, ClassId, GenericFunctionId, LocalId};
use crate::syntax::Span;

/// Which definitions a session holds to strict typing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StrictScope {
    /// Definitions declared `strict`, and the methods of a `strict`
    /// generic.
    Declared,
    /// Every source function, method, and constant, as if each were
    /// declared `strict`.
    All,
}

impl StrictScope {
    /// Returns the stable label used in dumps, policy settings, and EOP
    /// records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::All => "all",
        }
    }
}

/// The strict half of a [`super::TypingConfig`]: who is held to strict
/// typing, and whether overflow is among the checks they must prove.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StrictPolicy {
    /// Which definitions are strict.
    pub scope: StrictScope,
    /// Whether a strict definition must also prove that its integer
    /// arithmetic does not overflow and its shift counts are in range.
    ///
    /// Off by default: `<integer>` is held in 64 bits, a limit of the
    /// current runtime rather than of the type, so nearly every `+` on
    /// declared integers may overflow, and proving otherwise takes a
    /// `limited` range on every operand. Certain overflow is an error in
    /// strict code either way.
    pub overflow: bool,
}

impl StrictPolicy {
    /// Only declared definitions are strict, and overflow stays a runtime
    /// check.
    pub const DECLARED: Self = Self {
        scope: StrictScope::Declared,
        overflow: false,
    };

    /// Applies one `strict.` setting written after the prefix, such as
    /// `scope=all` or `overflow=proved`; `None` when it is not one.
    #[must_use]
    pub fn apply(mut self, setting: &str) -> Option<Self> {
        match setting.split_once('=')? {
            ("scope", "declared") => self.scope = StrictScope::Declared,
            ("scope", "all") => self.scope = StrictScope::All,
            ("overflow", "runtime") => self.overflow = false,
            ("overflow", "proved") => self.overflow = true,
            _ => return None,
        }
        Some(self)
    }

    /// Returns the policy as its settings, `strict.scope=declared
    /// strict.overflow=runtime`.
    #[must_use]
    pub fn label(self) -> String {
        format!(
            "strict.scope={} strict.overflow={}",
            self.scope.as_str(),
            if self.overflow { "proved" } else { "runtime" }
        )
    }

    /// Whether a strict definition must prove `decision`'s check.
    ///
    /// The compiler's own checks must be proved: a send's dispatch, and the
    /// casts and range checks it inserts where a value meets a declaration
    /// or comes back through a `<callable>`; overflow too when
    /// [`Self::overflow`] says so. An explicit `as` is a check the program
    /// makes deliberately, an `instance?` test answers a question, and an
    /// indirect entry's check guards the body against callers typing does
    /// not see, so none of those has to be proved.
    #[must_use]
    pub const fn requires(self, decision: &TypingDecision) -> bool {
        match (decision.origin, decision.check) {
            (
                CheckOrigin::Implicit,
                TypingCheck::Dispatch | TypingCheck::Cast | TypingCheck::Range,
            ) => true,
            (CheckOrigin::Implicit, TypingCheck::Overflow) => self.overflow,
            _ => false,
        }
    }

    /// What strict typing makes of `decision` in a strict definition:
    /// `None` when the decision is fine as it is.
    #[must_use]
    pub const fn violation(self, decision: &TypingDecision) -> Option<StrictViolation> {
        if matches!(decision.rule, TypingRule::Unreachable) {
            return None;
        }
        match decision.verdict {
            TypingVerdict::Checked if self.requires(decision) => Some(StrictViolation::Unproved),
            // A test that is always false is dead code, not a failure.
            TypingVerdict::Refuted if !matches!(decision.origin, CheckOrigin::Test) => {
                Some(StrictViolation::Refuted)
            }
            _ => None,
        }
    }
}

impl Default for StrictPolicy {
    fn default() -> Self {
        Self::DECLARED
    }
}

/// Why a definition is held to strict typing.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum StrictSource {
    /// The definition is declared `strict`.
    Declared,
    /// The method's generic is declared `strict`.
    Generic(GenericFunctionId),
    /// The session's typing policy holds every definition to it
    /// ([`StrictScope::All`]).
    Policy,
}

impl StrictSource {
    /// Returns the stable label used in dumps and EOP records: `declared`,
    /// `generic`, or `policy`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Generic(_) => "generic",
            Self::Policy => "policy",
        }
    }

    /// Describes the source with names: `declared`, `generic area is
    /// strict`, or `typing policy`.
    #[must_use]
    pub fn describe(self, module: &sem::Module) -> String {
        match self {
            Self::Declared => "declared".into(),
            Self::Generic(generic) => {
                format!("generic {} is strict", generic_name(module, generic))
            }
            Self::Policy => "typing policy".into(),
        }
    }
}

/// A definition held to strict typing, and why.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StrictDefinition {
    /// The function, method, or constant.
    pub definition: DefinitionId,
    /// Why it is strict.
    pub source: StrictSource,
}

/// How a decision falls short in a strict definition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StrictViolation {
    /// Typing could not prove a check strict typing requires, so it would
    /// be left to the runtime.
    Unproved,
    /// The check fails whenever it is reached.
    Refuted,
}

impl StrictViolation {
    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unproved => "unproved",
            Self::Refuted => "refuted",
        }
    }
}

/// Why `definition` is held to strict typing under `scope`, or `None` when
/// it is not.
///
/// A definition's own `strict` comes first, then its generic's for a
/// method, then the session's [`StrictScope::All`]. Only functions,
/// constants, methods from source, and slot defaults that run code are ever
/// strict: the compiler's numeric and slot-setter methods are not code a
/// program can change. A slot default has no `strict` of its own, so only
/// the session's policy holds it; a strict function that makes the object
/// does not make the defaults it runs strict.
#[must_use]
pub fn strictness(
    module: &sem::Module,
    scope: StrictScope,
    definition: DefinitionId,
) -> Option<StrictSource> {
    let policy = (scope == StrictScope::All).then_some(StrictSource::Policy);
    if let Some(function) = module
        .functions
        .iter()
        .find(|function| function.definition == definition)
    {
        return if function.strict {
            Some(StrictSource::Declared)
        } else {
            policy
        };
    }
    if let Some(method) = module
        .methods
        .iter()
        .find(|method| method.definition == definition)
    {
        if !matches!(
            method.origin,
            sem::MethodOrigin::Source | sem::MethodOrigin::Session { .. }
        ) {
            return None;
        }
        if method.strict {
            return Some(StrictSource::Declared);
        }
        let generic = module.generics.get(method.generic.0 as usize);
        return if generic.is_some_and(|generic| generic.strict) {
            Some(StrictSource::Generic(method.generic))
        } else {
            policy
        };
    }
    if module.slot_initializer(definition).is_some() {
        return policy;
    }
    let constant = module
        .constants
        .iter()
        .find(|constant| constant.definition == definition)?;
    if constant.strict {
        Some(StrictSource::Declared)
    } else {
        policy
    }
}

/// An edit that could prove an unproved check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StrictFix {
    /// Declare the parameter the operand is as `ty`, so the check holds
    /// where it is bound and its callers are checked against it instead.
    Annotate {
        /// The parameter.
        binding: Binding,
        /// Its source name.
        name: String,
        /// Source span of the bound name.
        span: Span,
        /// The type to declare.
        ty: Type,
    },
    /// Test the operand with `instance?` against `classes` first, so the
    /// check holds in each branch that passes.
    Narrow {
        /// The tested binding's source name.
        name: String,
        /// The classes to test against.
        classes: Vec<ClassId>,
    },
    /// Declare `class` sealed (and abstract, when `make_abstract`), so its
    /// pieces are exactly its current subclasses, which cover the check.
    Seal {
        /// The class to seal.
        class: ClassId,
        /// Whether it must also be declared abstract.
        make_abstract: bool,
    },
    /// Write the check as `as(ty, ...)`: an explicit check, which strict
    /// typing keeps at runtime as the program asked.
    Explicit {
        /// The type to check against.
        ty: Type,
    },
}

impl StrictFix {
    /// Returns the stable label of the fix's kind: `annotate`, `narrow`,
    /// `seal`, or `explicit`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Annotate { .. } => "annotate",
            Self::Narrow { .. } => "narrow",
            Self::Seal { .. } => "seal",
            Self::Explicit { .. } => "explicit",
        }
    }

    /// Describes the fix in source terms, such as `annotate parameter s as
    /// <circle>` or `declare <shape> sealed abstract`.
    #[must_use]
    pub fn describe(&self, module: &sem::Module) -> String {
        match self {
            Self::Annotate { name, ty, .. } => format!(
                "annotate parameter {name} as {}",
                type_source(module, ty).unwrap_or_else(|| type_name(module, ty))
            ),
            Self::Narrow { name, classes } => format!(
                "test {name} with instance? against {} first",
                classes
                    .iter()
                    .map(|class| class_name(module, *class))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Self::Seal {
                class,
                make_abstract,
            } => format!(
                "declare {} sealed{}",
                class_name(module, *class),
                if *make_abstract { " abstract" } else { "" }
            ),
            Self::Explicit { ty } => format!(
                "write as({}, ...) to check deliberately",
                type_source(module, ty).unwrap_or_else(|| type_name(module, ty))
            ),
        }
    }

    /// What making the fix does to the program beyond proving the check,
    /// which is what a reviewer must accept along with it:
    ///
    /// - [`Self::Annotate`] narrows the parameter's contract: its callers
    ///   now carry the check, and inputs it accepted before are rejected;
    /// - [`Self::Narrow`] proves the check locally, under a test the body
    ///   makes;
    /// - [`Self::Seal`] relies on a closed world: the class gains no
    ///   subclasses outside its source;
    /// - [`Self::Explicit`] keeps the check at run time, written
    ///   deliberately.
    #[must_use]
    pub const fn consequence(&self) -> &'static str {
        match self {
            Self::Annotate { .. } => {
                "narrows the contract: callers carry the obligation, and inputs accepted before are rejected"
            }
            Self::Narrow { .. } => "proves the check locally, under a test",
            Self::Seal { .. } => "relies on a closed world: no subclasses outside its source",
            Self::Explicit { .. } => "keeps the check at run time, deliberately",
        }
    }

    /// The type source an [`Self::Annotate`] fix writes, as an annotation
    /// mutation takes it.
    #[must_use]
    pub fn annotation_source(&self, module: &sem::Module) -> Option<String> {
        match self {
            Self::Annotate { ty, .. } => type_source(module, ty),
            _ => None,
        }
    }
}

/// The edits that could prove `decision`, a check strict typing could not:
/// for each operand whose type falls short of what the check needs,
/// declaring the parameter it is, testing it first, sealing the class it is
/// a cone over, or checking it explicitly.
///
/// Fixes are suggestions computed from the decision alone. Whether one
/// proves the check, and what else it changes, is what previewing its
/// edit reports.
#[must_use]
pub fn fixes(
    module: &sem::Module,
    typed: &DefinitionTyping,
    decision: &TypingDecision,
) -> Vec<StrictFix> {
    let mut out = Vec::new();
    for (index, required) in required_types(module, decision) {
        let (Some(actual), Some(reason)) =
            (decision.actual.get(index), decision.because.get(index))
        else {
            continue;
        };
        // A read records the fact it saw, which may be a join that kept
        // one side's fact; that side is the binding's.
        let read = read_binding(module, typed, *reason)
            .or_else(|| read_binding(module, typed, typed.principal_reason(*reason)));
        if let Some((binding, local, true)) = &read
            && type_source(module, &required).is_some()
        {
            push_unique(
                &mut out,
                StrictFix::Annotate {
                    binding: *binding,
                    name: local.name.clone(),
                    span: local.span,
                    ty: required.clone(),
                },
            );
        }
        let classes = narrowing_classes(module, decision, index, &required);
        if let Some((_, local, _)) = &read
            && !classes.is_empty()
        {
            push_unique(
                &mut out,
                StrictFix::Narrow {
                    name: local.name.clone(),
                    classes,
                },
            );
        }
        if let Some(fix) = seal_fix(module, actual, &required) {
            push_unique(&mut out, fix);
        }
        if type_source(module, &required).is_some() {
            push_unique(&mut out, StrictFix::Explicit { ty: required });
        }
    }
    out
}

fn push_unique(fixes: &mut Vec<StrictFix>, fix: StrictFix) {
    if !fixes.contains(&fix) {
        fixes.push(fix);
    }
}

/// The operand positions of `decision` whose type falls short, with the
/// type each needs: a cast's target, a range, or for a send, the union of
/// its methods' specializers at a position the argument is not within.
pub(crate) fn required_types(
    module: &sem::Module,
    decision: &TypingDecision,
) -> Vec<(usize, Type)> {
    match &decision.expected {
        TypingExpectation::Class(class) => {
            alloc::vec![(0, Type::of(Atom::Cone(*class), module))]
        }
        TypingExpectation::Type(ty) if decision.check != TypingCheck::Overflow => {
            alloc::vec![(0, ty.clone())]
        }
        TypingExpectation::Dispatch { methods, .. } => decision
            .actual
            .iter()
            .enumerate()
            .filter_map(|(index, actual)| {
                let specializers = methods
                    .iter()
                    .filter_map(|method| module.methods.get(method.0 as usize))
                    .filter(|method| method.specializers.len() == decision.actual.len())
                    .map(|method| specializer_type(module, method.specializers[index]))
                    .reduce(|left, right| left.join(&right, module))?;
                (!specializers.is_top() && !actual.is_subtype(&specializers, module))
                    .then_some((index, specializers))
            })
            .collect(),
        TypingExpectation::ClassObject(_) | TypingExpectation::Type(_) => Vec::new(),
    }
}

/// The classes an `instance?` test of the operand at `index` could narrow
/// it to: the cast's target, or the classes a send's methods specialize on
/// there.
fn narrowing_classes(
    module: &sem::Module,
    decision: &TypingDecision,
    index: usize,
    required: &Type,
) -> Vec<ClassId> {
    if decision.check == TypingCheck::Range {
        return Vec::new();
    }
    let mut classes = Vec::new();
    for atom in required.atoms().unwrap_or_default() {
        if let Atom::Cone(class) | Atom::Exact(class) = atom
            && !classes.contains(class)
        {
            classes.push(*class);
        }
    }
    if let TypingExpectation::Dispatch { methods, .. } = &decision.expected {
        classes.retain(|class| {
            methods
                .iter()
                .filter_map(|method| module.methods.get(method.0 as usize))
                .any(|method| method.specializers.get(index) == Some(&Some(*class)))
        });
    }
    classes
}

/// A fix that seals the open source class whose cone the operand is, when
/// each of its direct subclasses is within `required` and it has some.
fn seal_fix(module: &sem::Module, actual: &Type, required: &Type) -> Option<StrictFix> {
    let [Atom::Cone(class)] = actual.atoms()? else {
        return None;
    };
    let record = module.classes.get(class.0 as usize)?;
    if record.sealing.is_sealed() || !matches!(record.origin, sem::ClassOrigin::Native) {
        return None;
    }
    let subclasses = module.direct_subclasses(*class);
    let covered = !subclasses.is_empty()
        && subclasses
            .iter()
            .all(|subclass| Type::of(Atom::Cone(*subclass), module).is_subtype(required, module));
    let own = Type::of(Atom::Exact(*class), module).is_subtype(required, module);
    covered.then_some(StrictFix::Seal {
        class: *class,
        make_abstract: !record.is_abstract && !own,
    })
}

/// The binding whose fact the operand reason is, with its local and
/// whether it is a parameter a declaration could narrow: a function's or
/// closure's, but not a method's, whose annotation is its specializer.
fn read_binding<'a>(
    module: &'a sem::Module,
    typed: &DefinitionTyping,
    reason: super::ReasonId,
) -> Option<(Binding, &'a sem::Local, bool)> {
    let binding = typed
        .bindings
        .iter()
        .find(|fact| fact.reason == reason)
        .map(|fact| fact.binding)
        .or_else(|| {
            typed
                .uses
                .iter()
                .find(|read| read.reason == reason)
                .map(|read| read.binding)
        })?;
    let (locals, params): (&[sem::Local], &[LocalId]) = match binding {
        Binding::Local(_) => match module
            .functions
            .iter()
            .find(|function| function.definition == typed.definition)
        {
            Some(function) => (&function.locals, &function.params),
            // A method's parameters are its specializers, not bindings a
            // fix should test.
            None => (module.definition_locals(typed.definition)?.0, &[]),
        },
        Binding::ClosureLocal { closure, .. } => {
            let closure = module.closures.get(closure.0 as usize)?;
            (&closure.locals, &closure.params)
        }
        Binding::Capture { .. } => return None,
    };
    let local = match binding {
        Binding::Local(local) | Binding::ClosureLocal { local, .. } => local,
        Binding::Capture { .. } => return None,
    };
    let record = locals
        .get(local.0 as usize)
        .filter(|record| record.id == local)
        .or_else(|| locals.iter().find(|record| record.id == local))?;
    // A parameter fact is the declared one only where nothing narrowed it.
    let declarable = params.contains(&local)
        && typed
            .bindings
            .iter()
            .any(|fact| fact.binding == binding && fact.reason == reason);
    Some((binding, record, declarable))
}

/// Writes `ty` as an annotation's source: a class name for a class cone,
/// `<integer>` for every integer, `limited(<integer>, ...)` for a range. `None` for a type an
/// annotation cannot write, such as a union.
#[must_use]
pub fn type_source(module: &sem::Module, ty: &Type) -> Option<String> {
    match ty.atoms()? {
        [Atom::Cone(class)] => Some(class_name(module, *class)),
        [Atom::Integer(range)] => Some(limited_source(*range)),
        _ => None,
    }
}

fn limited_source(range: IntRange) -> String {
    if range.min.is_none() && range.max.is_none() {
        return "<integer>".into();
    }
    let mut out = String::from("limited(<integer>");
    if let Some(min) = range.min {
        let _ = write!(out, ", min: {min}");
    }
    if let Some(max) = range.max {
        let _ = write!(out, ", max: {max}");
    }
    out.push(')');
    out
}
