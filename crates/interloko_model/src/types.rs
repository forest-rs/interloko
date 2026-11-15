//! Static types: one lattice of value sets shared by Interloko's checkers.
//!
//! A [`Type`] denotes a set of runtime values. It is either [`Type::top`],
//! every value, or a finite union of [`Atom`]s; the empty union is
//! [`Type::bottom`], the type of no value (an expression that never
//! produces one). The atoms are:
//!
//! - [`Atom::Cone`]: instances of a class or any of its subclasses;
//! - [`Atom::Exact`]: instances of exactly one class;
//! - [`Atom::Integer`]: `<integer>` values in a range ([`IntRange`]), which
//!   is how `<integer>` itself and the limited types `<i32>` and `<u32>` are
//!   written;
//! - [`Atom::Boolean`]: one boolean value;
//! - [`Atom::Singleton`]: one metaobject, such as a particular generic
//!   function;
//! - [`Atom::Callable`]: first-class callable values.
//!
//! Classes come from a [`ClassHierarchy`]. Inheritance is single, so two
//! cones over unrelated classes are disjoint. The lattice is open-world: a
//! class that is not [closed](ClassHierarchy::is_closed) may gain
//! subclasses, so a cone is never covered by its known subclasses. It also
//! does not model abstract classes: every class may have direct instances.
//!
//! # Normal form
//!
//! Every `Type` is normalized when it is built, so equal value sets have
//! equal representations and `==` is set equality:
//!
//! - `Cone(<object>)` is [`Type::top`];
//! - `Cone(<integer>)` and `Exact(<integer>)` are `Integer(IntRange::INTEGER)`,
//!   and the hierarchy's `<integer>` class must be closed;
//! - a cone over a closed class is `Exact` of that class, so floats are
//!   `Exact(<double-float>)` or `Exact(<single-float>)` and carry no ranges;
//! - integer ranges that overlap or touch are merged, atoms contained in
//!   another atom are dropped, and the rest are sorted.
//!
//! A union holds at most [`MAX_ATOMS`] atoms. A larger one is widened: its
//! integer ranges to their hull, then its metaobject singletons to their
//! classes, then every class-shaped atom to a cone over their least common
//! superclass (which may be [`Type::top`]). Until that bound is reached,
//! [`Type::join`] and [`Type::meet`] are exact set union and intersection;
//! past it, a join is an upper bound that may depend on grouping.
//!
//! # Extension
//!
//! Parametric types (`Apply(constructor, [Type])`), predicate refinements,
//! and symbol or string singletons are not part of the lattice yet. [`Atom`]
//! is deliberately not `Copy` so a parametric atom can be added without
//! changing how atoms are passed around.

use alloc::vec::Vec;
use core::fmt;

use crate::{
    BuiltinValueClass, ClassId, GenericFunctionId, LimitedInteger, MethodId, OBJECT_CLASS_ID,
    SlotId,
};

/// Largest number of atoms a normalized union holds before it is widened.
pub const MAX_ATOMS: usize = 8;

/// Class structure the lattice is computed against.
///
/// The superclass chain must be acyclic.
pub trait ClassHierarchy {
    /// Returns the direct superclass of `class`, or `None` for the root.
    fn superclass(&self, class: ClassId) -> Option<ClassId>;

    /// Returns whether `class` has no subclasses and can never gain one, so a
    /// value bounded by it has exactly that class.
    fn is_closed(&self, class: ClassId) -> bool;

    /// Returns the class of a built-in value class, when the hierarchy
    /// defines it.
    fn builtin_class(&self, class: BuiltinValueClass) -> Option<ClassId>;

    /// Returns whether `class` is `ancestor` or inherits from it.
    fn is_same_or_subclass(&self, class: ClassId, ancestor: ClassId) -> bool {
        let mut current = Some(class);
        while let Some(class) = current {
            if class == ancestor {
                return true;
            }
            current = self.superclass(class);
        }
        false
    }
}

/// Names the classes and metaobjects a [`Type`] mentions, for display.
pub trait TypeNames {
    /// Writes the source name of `class`, such as `<circle>`.
    fn write_class(&self, out: &mut dyn fmt::Write, class: ClassId) -> fmt::Result;

    /// Writes the name of `generic`.
    fn write_generic(&self, out: &mut dyn fmt::Write, generic: GenericFunctionId) -> fmt::Result;

    /// Writes a label for `method`.
    fn write_method(&self, out: &mut dyn fmt::Write, method: MethodId) -> fmt::Result;

    /// Writes a label for the effective slot `slot` of `class`.
    fn write_slot(&self, out: &mut dyn fmt::Write, class: ClassId, slot: SlotId) -> fmt::Result;
}

/// A range of `<integer>` values; `None` bounds are unbounded.
///
/// `<integer>` is unbounded, so [`Self::INTEGER`] has no bounds. A range
/// whose `min` exceeds its `max` is empty.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct IntRange {
    /// Smallest member, or `None` when unbounded below.
    pub min: Option<i64>,
    /// Largest member, or `None` when unbounded above.
    pub max: Option<i64>,
}

impl IntRange {
    /// Every `<integer>`.
    pub const INTEGER: Self = Self {
        min: None,
        max: None,
    };

    /// The range of `<i32>`.
    pub const I32: Self = Self {
        min: Some(LimitedInteger::I32.min()),
        max: Some(LimitedInteger::I32.max()),
    };

    /// The range of `<u32>`.
    pub const U32: Self = Self {
        min: Some(LimitedInteger::U32.min()),
        max: Some(LimitedInteger::U32.max()),
    };

    /// Returns the range holding only `value`.
    #[must_use]
    pub const fn singleton(value: i64) -> Self {
        Self {
            min: Some(value),
            max: Some(value),
        }
    }

    /// Returns whether the range has no members.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        matches!((self.min, self.max), (Some(min), Some(max)) if min > max)
    }

    /// Returns whether `value` is a member.
    #[must_use]
    pub const fn contains(self, value: i64) -> bool {
        let above = match self.min {
            Some(min) => min <= value,
            None => true,
        };
        let below = match self.max {
            Some(max) => value <= max,
            None => true,
        };
        above && below
    }

    /// Returns whether every member of `other` is a member of `self`.
    #[must_use]
    pub fn includes(self, other: Self) -> bool {
        other.is_empty() || (low(self) <= low(other) && high(other) <= high(self))
    }

    /// Returns the members common to both ranges, or `None` when there are
    /// none.
    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let range = Self {
            min: max_bound(self.min, other.min),
            max: min_bound(self.max, other.max),
        };
        (!range.is_empty() && !self.is_empty() && !other.is_empty()).then_some(range)
    }

    /// Returns the smallest range including both.
    #[must_use]
    pub fn hull(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            min: self.min.zip(other.min).map(|(a, b)| a.min(b)),
            max: self.max.zip(other.max).map(|(a, b)| a.max(b)),
        }
    }

    /// Returns the limited integer type with exactly this range, if any.
    #[must_use]
    pub fn limited(self) -> Option<LimitedInteger> {
        LimitedInteger::ALL
            .iter()
            .copied()
            .find(|limited| Self::from(*limited) == self)
    }

    /// Rounds the bounds outward to the next widening threshold: lower
    /// bounds to `0`, `i32::MIN`, or `i64::MIN`, upper bounds to `i32::MAX`,
    /// `u32::MAX`, or `i64::MAX`. Unbounded sides stay unbounded.
    fn widened(self) -> Self {
        const LOWER: [i64; 3] = [0, i32::MIN as i64, i64::MIN];
        const UPPER: [i64; 3] = [i32::MAX as i64, u32::MAX as i64, i64::MAX];
        Self {
            min: self
                .min
                .map(|min| LOWER.into_iter().find(|bound| *bound <= min).unwrap_or(min)),
            max: self
                .max
                .map(|max| UPPER.into_iter().find(|bound| *bound >= max).unwrap_or(max)),
        }
    }
}

impl From<LimitedInteger> for IntRange {
    fn from(limited: LimitedInteger) -> Self {
        Self {
            min: Some(limited.min()),
            max: Some(limited.max()),
        }
    }
}

fn low(range: IntRange) -> i128 {
    range.min.map_or(i128::MIN, i128::from)
}

fn high(range: IntRange) -> i128 {
    range.max.map_or(i128::MAX, i128::from)
}

fn max_bound(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (bound, None) | (None, bound) => bound,
    }
}

fn min_bound(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (bound, None) | (None, bound) => bound,
    }
}

/// A metaobject that a singleton type names.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Metaobject {
    /// A class metaobject.
    Class(ClassId),
    /// A generic function metaobject.
    Generic(GenericFunctionId),
    /// A method metaobject.
    Method(MethodId),
    /// A slot descriptor metaobject.
    SlotDescriptor {
        /// Class whose effective slot is described.
        class: ClassId,
        /// Effective slot described.
        slot: SlotId,
    },
}

impl Metaobject {
    /// Returns the built-in class of this metaobject.
    #[must_use]
    pub const fn metaclass(self) -> BuiltinValueClass {
        match self {
            Self::Class(_) => BuiltinValueClass::Class,
            Self::Generic(_) => BuiltinValueClass::GenericFunction,
            Self::Method(_) => BuiltinValueClass::Method,
            Self::SlotDescriptor { .. } => BuiltinValueClass::SlotDescriptor,
        }
    }
}

/// One member of a type union.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Atom {
    /// Instances of the class or of any subclass.
    Cone(ClassId),
    /// Instances of exactly the class.
    Exact(ClassId),
    /// `<integer>` values in the range.
    Integer(IntRange),
    /// The boolean value.
    Boolean(bool),
    /// The metaobject itself.
    Singleton(Metaobject),
    /// First-class callable values.
    Callable,
}

impl Atom {
    /// Returns the class every value of this atom is an instance of, when
    /// there is one.
    fn class<H: ClassHierarchy + ?Sized>(&self, hierarchy: &H) -> Option<ClassId> {
        match self {
            Self::Cone(class) | Self::Exact(class) => Some(*class),
            Self::Integer(_) => hierarchy.builtin_class(BuiltinValueClass::Integer),
            Self::Singleton(object) => hierarchy.builtin_class(object.metaclass()),
            Self::Boolean(_) | Self::Callable => None,
        }
    }

    /// Returns the class of every value of this atom when they all share
    /// exactly one.
    fn exact_class<H: ClassHierarchy + ?Sized>(&self, hierarchy: &H) -> Option<ClassId> {
        match self {
            Self::Exact(_) | Self::Integer(_) | Self::Singleton(_) => self.class(hierarchy),
            Self::Cone(_) | Self::Boolean(_) | Self::Callable => None,
        }
    }

    /// Canonical sort key; integer ranges sort by their lower bound.
    fn key(&self) -> (u8, i128, i128) {
        match self {
            Self::Cone(class) => (0, class.0.into(), 0),
            Self::Exact(class) => (1, class.0.into(), 0),
            Self::Integer(range) => (2, low(*range), high(*range)),
            Self::Boolean(value) => (3, i128::from(*value), 0),
            Self::Singleton(Metaobject::Class(class)) => (4, class.0.into(), 0),
            Self::Singleton(Metaobject::Generic(generic)) => (5, generic.0.into(), 0),
            Self::Singleton(Metaobject::Method(method)) => (6, method.0.into(), 0),
            Self::Singleton(Metaobject::SlotDescriptor { class, slot }) => {
                (7, class.0.into(), slot.0.into())
            }
            Self::Callable => (8, 0, 0),
        }
    }

    /// Returns whether every value of `self` is a value of `other`. Both
    /// atoms are canonical.
    fn is_within<H: ClassHierarchy + ?Sized>(&self, other: &Self, hierarchy: &H) -> bool {
        match other {
            Self::Cone(ancestor) => self
                .class(hierarchy)
                .is_some_and(|class| hierarchy.is_same_or_subclass(class, *ancestor)),
            Self::Exact(class) => self.exact_class(hierarchy) == Some(*class),
            Self::Integer(outer) => matches!(self, Self::Integer(inner) if outer.includes(*inner)),
            Self::Boolean(_) | Self::Singleton(_) | Self::Callable => self == other,
        }
    }

    /// Returns the values common to two canonical atoms, or `None` when
    /// they are disjoint.
    fn meet<H: ClassHierarchy + ?Sized>(&self, other: &Self, hierarchy: &H) -> Option<Self> {
        if self.is_within(other, hierarchy) {
            return Some(self.clone());
        }
        if other.is_within(self, hierarchy) {
            return Some(other.clone());
        }
        // Single inheritance, closed `<integer>`, and exactly-classed
        // singletons leave overlapping ranges as the only partial overlap.
        match (self, other) {
            (Self::Integer(left), Self::Integer(right)) => {
                left.intersect(*right).map(Self::Integer)
            }
            _ => None,
        }
    }
}

/// How an operand's type relates to an expected type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Relation {
    /// Every value of the operand is expected.
    Always,
    /// Some values of the operand may be expected and some may not.
    Maybe,
    /// No value of the operand is expected.
    Never,
}

/// A static type: a normalized set of runtime values.
///
/// See the [module documentation](self) for the atoms and the normal form.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Type(Repr);

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Repr {
    Top,
    Union(Vec<Atom>),
}

impl Type {
    /// Returns the type of every value.
    #[must_use]
    pub const fn top() -> Self {
        Self(Repr::Top)
    }

    /// Returns the type of no value: the empty union.
    #[must_use]
    pub const fn bottom() -> Self {
        Self(Repr::Union(Vec::new()))
    }

    /// Returns the normalized type of one atom.
    #[must_use]
    pub fn of<H: ClassHierarchy + ?Sized>(atom: Atom, hierarchy: &H) -> Self {
        Self::union([atom], hierarchy)
    }

    /// Returns the normalized union of `atoms`.
    #[must_use]
    pub fn union<H: ClassHierarchy + ?Sized>(
        atoms: impl IntoIterator<Item = Atom>,
        hierarchy: &H,
    ) -> Self {
        normalize(atoms, hierarchy)
    }

    /// Returns whether this is the type of every value.
    #[must_use]
    pub const fn is_top(&self) -> bool {
        matches!(self.0, Repr::Top)
    }

    /// Returns whether this is the type of no value.
    #[must_use]
    pub fn is_bottom(&self) -> bool {
        matches!(&self.0, Repr::Union(atoms) if atoms.is_empty())
    }

    /// Returns the normalized atoms of a union, or `None` for
    /// [`Self::top`].
    #[must_use]
    pub fn atoms(&self) -> Option<&[Atom]> {
        match &self.0 {
            Repr::Top => None,
            Repr::Union(atoms) => Some(atoms),
        }
    }

    /// Returns the union of both types: the least upper bound.
    #[must_use]
    pub fn join<H: ClassHierarchy + ?Sized>(&self, other: &Self, hierarchy: &H) -> Self {
        match (&self.0, &other.0) {
            (Repr::Top, _) | (_, Repr::Top) => Self::top(),
            (Repr::Union(left), Repr::Union(right)) => {
                normalize(left.iter().chain(right).cloned(), hierarchy)
            }
        }
    }

    /// Returns the intersection of both types: the greatest lower bound.
    #[must_use]
    pub fn meet<H: ClassHierarchy + ?Sized>(&self, other: &Self, hierarchy: &H) -> Self {
        match (&self.0, &other.0) {
            (Repr::Top, _) => other.clone(),
            (_, Repr::Top) => self.clone(),
            (Repr::Union(left), Repr::Union(right)) => normalize(
                left.iter()
                    .flat_map(|left| right.iter().filter_map(|right| left.meet(right, hierarchy))),
                hierarchy,
            ),
        }
    }

    /// Returns a type holding every value of `self` that is not a value of
    /// `other`.
    ///
    /// The result is conservative: it may keep values of `other` that the
    /// lattice cannot carve out, such as `<shape>` minus `<circle>`, which
    /// stays `<shape>`. Integer ranges and booleans are removed exactly.
    #[must_use]
    pub fn minus<H: ClassHierarchy + ?Sized>(&self, other: &Self, hierarchy: &H) -> Self {
        let removed = match &other.0 {
            Repr::Top => return Self::bottom(),
            Repr::Union(atoms) if atoms.is_empty() => return self.clone(),
            Repr::Union(atoms) => atoms,
        };
        let Repr::Union(kept) = &self.0 else {
            return Self::top();
        };
        let mut out = Vec::with_capacity(kept.len());
        for atom in kept {
            if removed
                .iter()
                .any(|removed| atom.is_within(removed, hierarchy))
            {
                continue;
            }
            match atom {
                Atom::Integer(range) => {
                    let mut pieces = Vec::from([*range]);
                    for removed in removed {
                        if let Atom::Integer(hole) = removed {
                            pieces = pieces
                                .into_iter()
                                .flat_map(|piece| range_minus(piece, *hole))
                                .collect();
                        }
                    }
                    out.extend(pieces.into_iter().map(Atom::Integer));
                }
                _ => out.push(atom.clone()),
            }
        }
        normalize(out, hierarchy)
    }

    /// Returns whether every value of `self` is a value of `other`.
    #[must_use]
    pub fn is_subtype<H: ClassHierarchy + ?Sized>(&self, other: &Self, hierarchy: &H) -> bool {
        match (&self.0, &other.0) {
            (_, Repr::Top) => true,
            (Repr::Top, Repr::Union(_)) => false,
            (Repr::Union(inner), Repr::Union(outer)) => inner
                .iter()
                .all(|atom| outer.iter().any(|outer| atom.is_within(outer, hierarchy))),
        }
    }

    /// Relates an operand of this type to an `expected` type.
    ///
    /// [`Type::bottom`] relates [`Relation::Always`] to everything: no
    /// value can fail the expectation.
    #[must_use]
    pub fn relate<H: ClassHierarchy + ?Sized>(&self, expected: &Self, hierarchy: &H) -> Relation {
        if self.is_subtype(expected, hierarchy) {
            Relation::Always
        } else if self.meet(expected, hierarchy).is_bottom() {
            Relation::Never
        } else {
            Relation::Maybe
        }
    }

    /// Returns the class of every value when the type is non-empty and all
    /// of its values have exactly that class.
    #[must_use]
    pub fn exact_class<H: ClassHierarchy + ?Sized>(&self, hierarchy: &H) -> Option<ClassId> {
        let (first, rest) = self.atoms()?.split_first()?;
        let class = first.exact_class(hierarchy)?;
        rest.iter()
            .all(|atom| atom.exact_class(hierarchy) == Some(class))
            .then_some(class)
    }

    /// Returns the least class every value is an instance of, when the type
    /// is non-empty and every value has a class.
    ///
    /// [`Self::top`] has no bound: it includes booleans and callables.
    #[must_use]
    pub fn class_bound<H: ClassHierarchy + ?Sized>(&self, hierarchy: &H) -> Option<ClassId> {
        let (first, rest) = self.atoms()?.split_first()?;
        rest.iter()
            .try_fold(first.class(hierarchy)?, |bound, atom| {
                Some(least_common_superclass(
                    bound,
                    atom.class(hierarchy)?,
                    hierarchy,
                ))
            })
    }

    /// Returns a type at least as large as `self` whose integer bounds are
    /// rounded outward to the thresholds at the `<i32>`, `<u32>`, and
    /// 64-bit bounds.
    ///
    /// Each integer bound has finitely many widened values, so repeatedly
    /// joining and widening reaches a fixpoint.
    #[must_use]
    pub fn widen(&self) -> Self {
        let Repr::Union(atoms) = &self.0 else {
            return Self::top();
        };
        let mut out = atoms
            .iter()
            .map(|atom| match atom {
                Atom::Integer(range) => Atom::Integer(range.widened()),
                _ => atom.clone(),
            })
            .collect::<Vec<_>>();
        // Widening changes only range bounds, so merging the ranges again
        // restores the normal form.
        merge_ranges(&mut out);
        out.sort_by_key(Atom::key);
        Self(Repr::Union(out))
    }

    /// Returns a value that displays this type with source names, such as
    /// `<circle>`, `exactly <shape>`, `<i32>`, or
    /// `type-union(<string>, <boolean>)`.
    #[must_use]
    pub fn display<'a, N: TypeNames + ClassHierarchy + ?Sized>(
        &'a self,
        names: &'a N,
    ) -> TypeDisplay<'a, N> {
        TypeDisplay { ty: self, names }
    }
}

/// Displays a [`Type`] with source names; see [`Type::display`].
pub struct TypeDisplay<'a, N: ?Sized> {
    ty: &'a Type,
    names: &'a N,
}

impl<N: TypeNames + ClassHierarchy + ?Sized> fmt::Display for TypeDisplay<'_, N> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(atoms) = self.ty.atoms() else {
            return out.write_str("<object>");
        };
        let both_booleans =
            atoms.contains(&Atom::Boolean(false)) && atoms.contains(&Atom::Boolean(true));
        let shown = atoms
            .iter()
            .filter(|atom| !(both_booleans && **atom == Atom::Boolean(true)))
            .collect::<Vec<_>>();
        match shown.as_slice() {
            [atom] => self.atom(out, atom, both_booleans),
            _ => {
                out.write_str("type-union(")?;
                for (index, atom) in shown.iter().enumerate() {
                    if index > 0 {
                        out.write_str(", ")?;
                    }
                    self.atom(out, atom, both_booleans)?;
                }
                out.write_str(")")
            }
        }
    }
}

impl<N: TypeNames + ClassHierarchy + ?Sized> TypeDisplay<'_, N> {
    fn atom(&self, out: &mut fmt::Formatter<'_>, atom: &Atom, both_booleans: bool) -> fmt::Result {
        let names = self.names;
        match atom {
            Atom::Cone(class) => names.write_class(out, *class),
            Atom::Exact(class) => {
                // A closed class already names exactly one runtime class.
                if !names.is_closed(*class) {
                    out.write_str("exactly ")?;
                }
                names.write_class(out, *class)
            }
            Atom::Integer(range) => match (range.limited(), *range == IntRange::INTEGER) {
                (Some(limited), _) => out.write_str(limited.name()),
                (None, true) => out.write_str("<integer>"),
                (None, false) => {
                    out.write_str("limited(<integer>")?;
                    if let Some(min) = range.min {
                        write!(out, ", min: {min}")?;
                    }
                    if let Some(max) = range.max {
                        write!(out, ", max: {max}")?;
                    }
                    out.write_str(")")
                }
            },
            Atom::Boolean(_) if both_booleans => out.write_str("<boolean>"),
            Atom::Boolean(value) => out.write_str(if *value {
                "singleton(#t)"
            } else {
                "singleton(#f)"
            }),
            Atom::Singleton(object) => {
                out.write_str("singleton(")?;
                match object {
                    Metaobject::Class(class) => names.write_class(out, *class)?,
                    Metaobject::Generic(generic) => names.write_generic(out, *generic)?,
                    Metaobject::Method(method) => names.write_method(out, *method)?,
                    Metaobject::SlotDescriptor { class, slot } => {
                        names.write_slot(out, *class, *slot)?;
                    }
                }
                out.write_str(")")
            }
            Atom::Callable => out.write_str("<callable>"),
        }
    }
}

/// Canonical form of one atom.
enum Canonical {
    Top,
    Empty,
    Atom(Atom),
}

fn canonical<H: ClassHierarchy + ?Sized>(atom: Atom, hierarchy: &H) -> Canonical {
    let integer = hierarchy.builtin_class(BuiltinValueClass::Integer);
    match atom {
        Atom::Cone(class) if class == OBJECT_CLASS_ID => Canonical::Top,
        Atom::Cone(class) | Atom::Exact(class) if Some(class) == integer => {
            Canonical::Atom(Atom::Integer(IntRange::INTEGER))
        }
        Atom::Cone(class) if hierarchy.is_closed(class) => Canonical::Atom(Atom::Exact(class)),
        Atom::Integer(range) if range.is_empty() => Canonical::Empty,
        atom => Canonical::Atom(atom),
    }
}

fn normalize<H: ClassHierarchy + ?Sized>(
    atoms: impl IntoIterator<Item = Atom>,
    hierarchy: &H,
) -> Type {
    let mut out = Vec::new();
    for atom in atoms {
        match canonical(atom, hierarchy) {
            Canonical::Top => return Type::top(),
            Canonical::Empty => {}
            Canonical::Atom(atom) => out.push(atom),
        }
    }
    tidy(&mut out, hierarchy);
    if out.len() <= MAX_ATOMS {
        return Type(Repr::Union(out));
    }
    bound_size(out, hierarchy)
}

/// Merges ranges, drops contained atoms, and sorts canonical atoms.
fn tidy<H: ClassHierarchy + ?Sized>(atoms: &mut Vec<Atom>, hierarchy: &H) {
    merge_ranges(atoms);
    atoms.sort_by_key(Atom::key);
    atoms.dedup();
    let contained = (0..atoms.len())
        .map(|index| {
            atoms
                .iter()
                .enumerate()
                .any(|(other, outer)| other != index && atoms[index].is_within(outer, hierarchy))
        })
        .collect::<Vec<_>>();
    let mut flags = contained.into_iter();
    atoms.retain(|_| !flags.next().unwrap_or(false));
}

/// Replaces the integer ranges with their union as disjoint, non-adjacent
/// ranges.
fn merge_ranges(atoms: &mut Vec<Atom>) {
    let mut ranges = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Integer(range) => Some(*range),
            _ => None,
        })
        .collect::<Vec<_>>();
    if ranges.len() < 2 {
        return;
    }
    atoms.retain(|atom| !matches!(atom, Atom::Integer(_)));
    ranges.sort_by_key(|range| low(*range));
    let mut merged: Vec<IntRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if low(range) <= high(*last).saturating_add(1) => *last = last.hull(range),
            _ => merged.push(range),
        }
    }
    atoms.extend(merged.into_iter().map(Atom::Integer));
}

/// Widens a union of more than [`MAX_ATOMS`] atoms until it fits.
fn bound_size<H: ClassHierarchy + ?Sized>(mut atoms: Vec<Atom>, hierarchy: &H) -> Type {
    // 1. Integer ranges to their hull.
    let hull = atoms
        .iter()
        .filter_map(|atom| match atom {
            Atom::Integer(range) => Some(*range),
            _ => None,
        })
        .reduce(IntRange::hull);
    if let Some(hull) = hull {
        atoms.retain(|atom| !matches!(atom, Atom::Integer(_)));
        atoms.push(Atom::Integer(hull));
        tidy(&mut atoms, hierarchy);
        if atoms.len() <= MAX_ATOMS {
            return Type(Repr::Union(atoms));
        }
    }

    // 2. Metaobject singletons to their classes.
    let mut widened = Vec::with_capacity(atoms.len());
    for atom in atoms {
        let atom = match atom {
            Atom::Singleton(object) => match hierarchy.builtin_class(object.metaclass()) {
                Some(class) => Atom::Cone(class),
                None => return Type::top(),
            },
            atom => atom,
        };
        match canonical(atom, hierarchy) {
            Canonical::Top => return Type::top(),
            Canonical::Empty => {}
            Canonical::Atom(atom) => widened.push(atom),
        }
    }
    tidy(&mut widened, hierarchy);
    if widened.len() <= MAX_ATOMS {
        return Type(Repr::Union(widened));
    }

    // 3. Class-shaped atoms to one cone over their least common superclass.
    // At most the two booleans and `Callable` remain beside it.
    let mut bound = None;
    let mut rest = Vec::new();
    for atom in widened {
        match atom {
            Atom::Boolean(_) | Atom::Callable => rest.push(atom),
            Atom::Cone(_) | Atom::Exact(_) | Atom::Integer(_) | Atom::Singleton(_) => {
                let Some(class) = atom.class(hierarchy) else {
                    return Type::top();
                };
                bound = Some(bound.map_or(class, |bound| {
                    least_common_superclass(bound, class, hierarchy)
                }));
            }
        }
    }
    normalize(rest.into_iter().chain(bound.map(Atom::Cone)), hierarchy)
}

fn least_common_superclass<H: ClassHierarchy + ?Sized>(
    left: ClassId,
    right: ClassId,
    hierarchy: &H,
) -> ClassId {
    let mut current = Some(left);
    while let Some(class) = current {
        if hierarchy.is_same_or_subclass(right, class) {
            return class;
        }
        current = hierarchy.superclass(class);
    }
    OBJECT_CLASS_ID
}

/// Returns the parts of `range` outside `hole`.
///
/// A part beyond the 64-bit bounds has no representable bound, so the
/// whole range is kept instead, which is conservative.
fn range_minus(range: IntRange, hole: IntRange) -> Vec<IntRange> {
    if range.intersect(hole).is_none() {
        return Vec::from([range]);
    }
    let mut pieces = Vec::new();
    if let Some(min) = hole.min
        && low(range) < i128::from(min)
    {
        let Some(max) = min.checked_sub(1) else {
            return Vec::from([range]);
        };
        pieces.push(IntRange {
            min: range.min,
            max: Some(max),
        });
    }
    if let Some(max) = hole.max
        && i128::from(max) < high(range)
    {
        let Some(min) = max.checked_add(1) else {
            return Vec::from([range]);
        };
        pieces.push(IntRange {
            min: Some(min),
            max: range.max,
        });
    }
    pieces
}

#[cfg(test)]
mod tests;
