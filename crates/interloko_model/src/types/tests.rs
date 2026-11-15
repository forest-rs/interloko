//! Lattice-law tests over a small fixture universe.
//!
//! Each type is also interpreted as a set of sample values, so the laws are
//! checked against set semantics rather than only against each other.

extern crate std;

use std::string::ToString;
use std::vec::Vec;

use super::*;

const OBJECT: ClassId = OBJECT_CLASS_ID;
const SHAPE: ClassId = ClassId(1);
const CIRCLE: ClassId = ClassId(2);
/// Closed leaf.
const SQUARE: ClassId = ClassId(3);
const LABEL: ClassId = ClassId(4);
const RATIONAL: ClassId = ClassId(5);
/// Closed, and the hierarchy's `<integer>`.
const INTEGER: ClassId = ClassId(6);
/// Closed, and the hierarchy's `<class>`.
const CLASS: ClassId = ClassId(7);
/// Subclass of `<circle>` that no fixture type names.
const FILLED: ClassId = ClassId(8);
/// Subclass of `<shape>` that no fixture type names.
const TRIANGLE: ClassId = ClassId(9);
/// Subclass of `<object>` that no fixture type names.
const OTHER: ClassId = ClassId(10);
/// Subclass of `<shape>` used only to overflow a union.
const ELLIPSE: ClassId = ClassId(11);

struct Toy;

impl Toy {
    const NAMES: [&'static str; 12] = [
        "<object>",
        "<shape>",
        "<circle>",
        "<square>",
        "<label>",
        "<rational>",
        "<integer>",
        "<class>",
        "<filled-circle>",
        "<triangle>",
        "<other>",
        "<ellipse>",
    ];
}

impl ClassHierarchy for Toy {
    fn superclass(&self, class: ClassId) -> Option<ClassId> {
        match class {
            OBJECT => None,
            CIRCLE | SQUARE | TRIANGLE | ELLIPSE => Some(SHAPE),
            INTEGER => Some(RATIONAL),
            FILLED => Some(CIRCLE),
            _ => Some(OBJECT),
        }
    }

    fn is_closed(&self, class: ClassId) -> bool {
        matches!(class, SQUARE | INTEGER | CLASS)
    }

    fn builtin_class(&self, class: BuiltinValueClass) -> Option<ClassId> {
        match class {
            BuiltinValueClass::Integer => Some(INTEGER),
            BuiltinValueClass::Rational => Some(RATIONAL),
            BuiltinValueClass::Class => Some(CLASS),
            _ => None,
        }
    }
}

impl TypeNames for Toy {
    fn write_class(&self, out: &mut dyn fmt::Write, class: ClassId) -> fmt::Result {
        out.write_str(Self::NAMES[class.0 as usize])
    }

    fn write_generic(&self, out: &mut dyn fmt::Write, generic: GenericFunctionId) -> fmt::Result {
        write!(out, "g{}", generic.0)
    }

    fn write_method(&self, out: &mut dyn fmt::Write, method: MethodId) -> fmt::Result {
        write!(out, "m{}", method.0)
    }

    fn write_slot(&self, out: &mut dyn fmt::Write, class: ClassId, slot: SlotId) -> fmt::Result {
        write!(out, "c{}.s{}", class.0, slot.0)
    }
}

/// A runtime value the fixture types are interpreted over.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Sample {
    Instance(ClassId),
    Int(i128),
    Bool(bool),
    ClassObject(ClassId),
    Callable,
}

fn samples() -> Vec<Sample> {
    let mut out = [
        OBJECT, SHAPE, CIRCLE, SQUARE, LABEL, RATIONAL, FILLED, TRIANGLE, OTHER,
    ]
    .into_iter()
    .map(Sample::Instance)
    .collect::<Vec<_>>();
    let big = i128::from(i64::MAX) + 1;
    for value in [
        -big,
        i64::MIN.into(),
        i128::from(i32::MIN) - 1,
        i32::MIN.into(),
        -1,
        0,
        1,
        4,
        5,
        7,
        10,
        11,
        i32::MAX.into(),
        i128::from(i32::MAX) + 1,
        u32::MAX.into(),
        i128::from(u32::MAX) + 1,
        i64::MAX.into(),
        big,
    ] {
        out.push(Sample::Int(value));
    }
    out.extend([
        Sample::Bool(false),
        Sample::Bool(true),
        Sample::ClassObject(CIRCLE),
        Sample::ClassObject(SHAPE),
        Sample::Callable,
    ]);
    out
}

fn class_of(sample: Sample) -> Option<ClassId> {
    match sample {
        Sample::Instance(class) => Some(class),
        Sample::Int(_) => Some(INTEGER),
        Sample::ClassObject(_) => Some(CLASS),
        Sample::Bool(_) | Sample::Callable => None,
    }
}

fn atom_holds(atom: &Atom, sample: Sample) -> bool {
    match atom {
        Atom::Cone(class) => {
            class_of(sample).is_some_and(|actual| Toy.is_same_or_subclass(actual, *class))
        }
        Atom::Exact(class) => class_of(sample) == Some(*class),
        Atom::Integer(range) => match sample {
            Sample::Int(value) => {
                range.min.is_none_or(|min| i128::from(min) <= value)
                    && range.max.is_none_or(|max| value <= i128::from(max))
            }
            _ => false,
        },
        Atom::Boolean(value) => sample == Sample::Bool(*value),
        Atom::Singleton(Metaobject::Class(class)) => sample == Sample::ClassObject(*class),
        Atom::Singleton(_) => false,
        Atom::Callable => sample == Sample::Callable,
    }
}

fn holds(ty: &Type, sample: Sample) -> bool {
    ty.atoms()
        .is_none_or(|atoms| atoms.iter().any(|atom| atom_holds(atom, sample)))
}

/// The set of samples a type holds, as a bit mask.
fn members(ty: &Type) -> u64 {
    samples()
        .into_iter()
        .enumerate()
        .filter(|(_, sample)| holds(ty, *sample))
        .fold(0, |mask, (index, _)| mask | (1 << index))
}

fn of(atom: Atom) -> Type {
    Type::of(atom, &Toy)
}

fn range(min: i64, max: i64) -> Atom {
    Atom::Integer(IntRange {
        min: Some(min),
        max: Some(max),
    })
}

fn universe() -> Vec<Type> {
    Vec::from([
        Type::top(),
        Type::bottom(),
        of(Atom::Cone(SHAPE)),
        of(Atom::Cone(CIRCLE)),
        of(Atom::Exact(SHAPE)),
        of(Atom::Exact(CIRCLE)),
        of(Atom::Cone(SQUARE)),
        of(Atom::Cone(LABEL)),
        of(Atom::Cone(RATIONAL)),
        of(Atom::Integer(IntRange::INTEGER)),
        of(Atom::Integer(IntRange::I32)),
        of(Atom::Integer(IntRange::U32)),
        of(range(0, 0)),
        of(range(5, 10)),
        of(Atom::Boolean(false)),
        of(Atom::Boolean(true)),
        Type::union([Atom::Boolean(false), Atom::Boolean(true)], &Toy),
        of(Atom::Singleton(Metaobject::Class(CIRCLE))),
        of(Atom::Cone(CLASS)),
        of(Atom::Callable),
    ])
}

/// The universe closed under one round of join and meet, which mixes
/// atom kinds without exceeding [`MAX_ATOMS`].
fn closure() -> Vec<Type> {
    let base = universe();
    let mut out = base.clone();
    for left in &base {
        for right in &base {
            for ty in [left.join(right, &Toy), left.meet(right, &Toy)] {
                if !out.contains(&ty) {
                    out.push(ty);
                }
            }
        }
    }
    out
}

#[test]
fn normal_form_rewrites_named_atoms() {
    assert!(of(Atom::Cone(OBJECT)).is_top());
    assert_eq!(
        of(Atom::Cone(INTEGER)),
        of(Atom::Integer(IntRange::INTEGER))
    );
    assert_eq!(
        of(Atom::Exact(INTEGER)),
        of(Atom::Integer(IntRange::INTEGER))
    );
    assert_eq!(
        of(Atom::Cone(SQUARE)).atoms(),
        Some(&[Atom::Exact(SQUARE)][..])
    );
    assert_eq!(
        of(Atom::Cone(CLASS)).atoms(),
        Some(&[Atom::Exact(CLASS)][..])
    );
    assert!(of(range(3, 2)).is_bottom());
    // Overlapping and touching ranges merge; contained atoms disappear.
    assert_eq!(
        Type::union([range(0, 4), range(5, 10), range(7, 20)], &Toy).atoms(),
        Some(&[range(0, 20)][..])
    );
    assert_eq!(
        Type::union(
            [
                Atom::Exact(CIRCLE),
                Atom::Cone(SHAPE),
                range(1, 2),
                Atom::Cone(RATIONAL)
            ],
            &Toy
        )
        .atoms(),
        Some(&[Atom::Cone(SHAPE), Atom::Cone(RATIONAL)][..])
    );
    assert_eq!(
        Type::union(
            [
                Atom::Singleton(Metaobject::Class(CIRCLE)),
                Atom::Cone(CLASS)
            ],
            &Toy
        )
        .atoms(),
        Some(&[Atom::Exact(CLASS)][..])
    );
}

#[test]
fn normalization_is_canonical() {
    let types = closure();
    // Atom order and repetition do not matter.
    for ty in &types {
        if let Some(atoms) = ty.atoms() {
            let mut reversed = atoms.to_vec();
            reversed.reverse();
            reversed.extend(atoms.iter().cloned());
            assert_eq!(&Type::union(reversed, &Toy), ty);
        }
    }
    // Types with the same values have the same representation.
    for left in &types {
        for right in &types {
            assert_eq!(
                members(left) == members(right),
                left == right,
                "{left:?} {right:?}"
            );
        }
    }
}

#[test]
fn join_and_meet_are_set_union_and_intersection() {
    let types = closure();
    for left in &types {
        for right in &types {
            assert_eq!(
                members(&left.join(right, &Toy)),
                members(left) | members(right)
            );
            assert_eq!(
                members(&left.meet(right, &Toy)),
                members(left) & members(right)
            );
        }
    }
}

#[test]
fn join_and_meet_are_commutative_associative_and_idempotent() {
    let types = universe();
    for a in &types {
        assert_eq!(&a.join(a, &Toy), a);
        assert_eq!(&a.meet(a, &Toy), a);
        assert_eq!(&a.join(&Type::bottom(), &Toy), a);
        assert_eq!(&a.meet(&Type::top(), &Toy), a);
        assert!(a.join(&Type::top(), &Toy).is_top());
        assert!(a.meet(&Type::bottom(), &Toy).is_bottom());
        for b in &types {
            assert_eq!(a.join(b, &Toy), b.join(a, &Toy));
            assert_eq!(a.meet(b, &Toy), b.meet(a, &Toy));
            // Absorption.
            assert_eq!(&a.join(&a.meet(b, &Toy), &Toy), a);
            assert_eq!(&a.meet(&a.join(b, &Toy), &Toy), a);
            for c in &types {
                assert_eq!(
                    a.join(b, &Toy).join(c, &Toy),
                    a.join(&b.join(c, &Toy), &Toy)
                );
                assert_eq!(
                    a.meet(b, &Toy).meet(c, &Toy),
                    a.meet(&b.meet(c, &Toy), &Toy)
                );
            }
        }
    }
}

#[test]
fn subtyping_is_a_partial_order_matching_join_and_meet() {
    let types = closure();
    for a in &types {
        assert!(a.is_subtype(a, &Toy));
        assert!(Type::bottom().is_subtype(a, &Toy));
        assert!(a.is_subtype(&Type::top(), &Toy));
        for b in &types {
            let sub = a.is_subtype(b, &Toy);
            assert_eq!(sub, members(a) & !members(b) == 0, "{a:?} <= {b:?}");
            assert_eq!(sub, &a.join(b, &Toy) == b);
            assert_eq!(sub, &a.meet(b, &Toy) == a);
            if sub && b.is_subtype(a, &Toy) {
                assert_eq!(a, b);
            }
        }
    }
    let types = universe();
    for a in &types {
        for b in &types {
            for c in &types {
                if a.is_subtype(b, &Toy) && b.is_subtype(c, &Toy) {
                    assert!(a.is_subtype(c, &Toy));
                }
            }
        }
    }
}

#[test]
fn relate_reports_always_maybe_or_never() {
    let types = closure();
    for operand in &types {
        for expected in &types {
            let (inside, outside) = (
                members(operand) & members(expected),
                members(operand) & !members(expected),
            );
            let relation = match (inside != 0, outside != 0) {
                (_, false) => Relation::Always,
                (false, true) => Relation::Never,
                (true, true) => Relation::Maybe,
            };
            assert_eq!(
                operand.relate(expected, &Toy),
                relation,
                "{operand:?} against {expected:?}"
            );
        }
    }
    let table = [
        (
            of(Atom::Exact(CIRCLE)),
            of(Atom::Cone(SHAPE)),
            Relation::Always,
        ),
        (
            of(Atom::Cone(SHAPE)),
            of(Atom::Cone(CIRCLE)),
            Relation::Maybe,
        ),
        (
            of(Atom::Cone(LABEL)),
            of(Atom::Cone(SHAPE)),
            Relation::Never,
        ),
        (
            of(Atom::Exact(SHAPE)),
            of(Atom::Cone(CIRCLE)),
            Relation::Never,
        ),
        (
            of(Atom::Integer(IntRange::I32)),
            of(Atom::Cone(RATIONAL)),
            Relation::Always,
        ),
        (
            of(Atom::Cone(RATIONAL)),
            of(Atom::Cone(INTEGER)),
            Relation::Maybe,
        ),
        (
            of(Atom::Integer(IntRange::U32)),
            of(Atom::Integer(IntRange::I32)),
            Relation::Maybe,
        ),
        (
            of(range(5, 10)),
            of(Atom::Integer(IntRange::U32)),
            Relation::Always,
        ),
        (
            of(Atom::Boolean(true)),
            of(Atom::Cone(SHAPE)),
            Relation::Never,
        ),
        (of(Atom::Callable), of(Atom::Cone(CLASS)), Relation::Never),
        (Type::top(), of(Atom::Cone(SHAPE)), Relation::Maybe),
        (of(Atom::Cone(LABEL)), Type::top(), Relation::Always),
        (Type::bottom(), of(Atom::Cone(SHAPE)), Relation::Always),
        (
            of(Atom::Singleton(Metaobject::Class(SHAPE))),
            of(Atom::Cone(CLASS)),
            Relation::Always,
        ),
    ];
    for (operand, expected, relation) in table {
        assert_eq!(
            operand.relate(&expected, &Toy),
            relation,
            "{operand:?} against {expected:?}"
        );
    }
}

#[test]
fn minus_is_conservative() {
    let types = closure();
    for a in &types {
        for b in &types {
            let difference = a.minus(b, &Toy);
            assert!(difference.is_subtype(a, &Toy), "{a:?} - {b:?}");
            assert_eq!(
                members(a) & !members(b) & !members(&difference),
                0,
                "{a:?} - {b:?}"
            );
        }
    }
    // Ranges and booleans are removed exactly.
    let bool = Type::union([Atom::Boolean(false), Atom::Boolean(true)], &Toy);
    assert_eq!(
        bool.minus(&of(Atom::Boolean(true)), &Toy),
        of(Atom::Boolean(false))
    );
    assert_eq!(
        of(Atom::Integer(IntRange::U32)).minus(&of(range(0, 0)), &Toy),
        of(Atom::Integer(IntRange {
            min: Some(1),
            max: Some(u32::MAX.into())
        }))
    );
    // A cone keeps the subclasses it cannot carve out.
    assert_eq!(
        of(Atom::Cone(SHAPE)).minus(&of(Atom::Cone(CIRCLE)), &Toy),
        of(Atom::Cone(SHAPE))
    );
    // Removing beyond the 64-bit bounds keeps the range.
    let word = of(range(i64::MIN, i64::MAX));
    assert_eq!(
        of(Atom::Integer(IntRange::INTEGER)).minus(&word, &Toy),
        of(Atom::Integer(IntRange::INTEGER))
    );
}

#[test]
fn widen_is_at_least_its_input_and_reaches_thresholds() {
    for ty in closure() {
        let widened = ty.widen();
        assert!(ty.is_subtype(&widened, &Toy), "{ty:?}");
        assert_eq!(widened.widen(), widened, "{ty:?}");
    }
    let widen_range = |min, max| of(range(min, max)).widen();
    assert_eq!(widen_range(0, 0), of(range(0, i32::MAX.into())));
    assert_eq!(widen_range(5, 10), of(range(0, i32::MAX.into())));
    assert_eq!(widen_range(-1, 0), of(Atom::Integer(IntRange::I32)));
    assert_eq!(
        widen_range(0, i64::from(i32::MAX) + 1),
        of(Atom::Integer(IntRange::U32))
    );
    assert_eq!(
        widen_range(i64::MIN + 1, i64::from(u32::MAX) + 1),
        of(range(i64::MIN, i64::MAX))
    );
    assert_eq!(
        of(Atom::Integer(IntRange::I32)).widen(),
        of(Atom::Integer(IntRange::I32))
    );
    assert_eq!(
        of(Atom::Integer(IntRange::U32)).widen(),
        of(Atom::Integer(IntRange::U32))
    );
    assert_eq!(
        of(Atom::Integer(IntRange {
            min: Some(3),
            max: None
        }))
        .widen(),
        of(Atom::Integer(IntRange {
            min: Some(0),
            max: None
        }))
    );
    // Ranges that widen into each other merge.
    assert_eq!(
        Type::union([range(0, 0), range(5, 10)], &Toy).widen(),
        of(range(0, i32::MAX.into()))
    );
}

#[test]
fn unions_past_the_size_bound_are_widened() {
    // Nine disjoint ranges widen to their hull.
    let ranges = (0..9).map(|index| range(index * 10, index * 10 + 1));
    let ty = Type::union(ranges.chain([Atom::Cone(LABEL)]), &Toy);
    assert_eq!(ty.atoms(), Some(&[Atom::Cone(LABEL), range(0, 81)][..]));

    // Singletons without a known metaclass lose every bound.
    let methods = [SHAPE, CIRCLE, FILLED, SQUARE, TRIANGLE]
        .into_iter()
        .map(Atom::Exact)
        .chain((0..4).map(|index| Atom::Singleton(Metaobject::Method(MethodId(index)))));
    assert!(Type::union(methods, &Toy).is_top());

    // Singletons widen to their metaclass first.
    let classes = [SHAPE, CIRCLE, FILLED, SQUARE, TRIANGLE]
        .into_iter()
        .map(Atom::Exact)
        .chain((0..4).map(|index| Atom::Singleton(Metaobject::Class(ClassId(index)))));
    assert_eq!(
        Type::union(classes, &Toy).atoms(),
        Some(
            &[
                Atom::Exact(SHAPE),
                Atom::Exact(CIRCLE),
                Atom::Exact(SQUARE),
                Atom::Exact(CLASS),
                Atom::Exact(FILLED),
                Atom::Exact(TRIANGLE)
            ][..]
        )
    );

    // Then class-shaped atoms widen to their least common superclass,
    // keeping the class-less atoms beside it.
    let shapes = [SHAPE, CIRCLE, FILLED, SQUARE, TRIANGLE, ELLIPSE]
        .into_iter()
        .map(Atom::Exact)
        .chain([Atom::Boolean(false), Atom::Boolean(true), Atom::Callable]);
    assert_eq!(
        Type::union(shapes, &Toy).atoms(),
        Some(
            &[
                Atom::Cone(SHAPE),
                Atom::Boolean(false),
                Atom::Boolean(true),
                Atom::Callable
            ][..]
        )
    );
    let unrelated = (1..10).map(|index| Atom::Exact(ClassId(index)));
    assert!(Type::union(unrelated, &Toy).is_top());
}

#[test]
fn exact_class_and_class_bound() {
    let exact = |ty: &Type| ty.exact_class(&Toy);
    let bound = |ty: &Type| ty.class_bound(&Toy);
    assert_eq!(exact(&of(Atom::Exact(CIRCLE))), Some(CIRCLE));
    assert_eq!(exact(&of(Atom::Cone(SQUARE))), Some(SQUARE));
    assert_eq!(exact(&of(Atom::Cone(CIRCLE))), None);
    assert_eq!(
        exact(&Type::union([range(0, 0), range(5, 10)], &Toy)),
        Some(INTEGER)
    );
    assert_eq!(
        exact(&of(Atom::Singleton(Metaobject::Class(SHAPE)))),
        Some(CLASS)
    );
    assert_eq!(
        exact(&Type::union(
            [Atom::Exact(CIRCLE), Atom::Exact(SQUARE)],
            &Toy
        )),
        None
    );
    assert_eq!(exact(&of(Atom::Boolean(true))), None);
    assert_eq!(exact(&Type::top()), None);
    assert_eq!(exact(&Type::bottom()), None);

    assert_eq!(bound(&of(Atom::Cone(CIRCLE))), Some(CIRCLE));
    assert_eq!(
        bound(&Type::union(
            [Atom::Exact(FILLED), Atom::Exact(SQUARE)],
            &Toy
        )),
        Some(SHAPE)
    );
    assert_eq!(
        bound(&Type::union([Atom::Cone(CIRCLE), Atom::Cone(LABEL)], &Toy)),
        Some(OBJECT)
    );
    assert_eq!(bound(&of(Atom::Integer(IntRange::U32))), Some(INTEGER));
    assert_eq!(
        bound(&Type::union([Atom::Cone(CIRCLE), Atom::Callable], &Toy)),
        None
    );
    assert_eq!(bound(&Type::top()), None);
    assert_eq!(bound(&Type::bottom()), None);
}

#[test]
fn int_ranges() {
    assert!(IntRange::INTEGER.includes(IntRange::I32));
    assert!(IntRange::I32.contains(-1) && !IntRange::U32.contains(-1));
    assert_eq!(
        IntRange::I32.intersect(IntRange::U32),
        Some(IntRange {
            min: Some(0),
            max: Some(i32::MAX.into())
        })
    );
    assert_eq!(
        IntRange::singleton(3).intersect(IntRange::singleton(4)),
        None
    );
    assert_eq!(
        IntRange::I32.hull(IntRange::U32),
        IntRange {
            min: Some(i32::MIN.into()),
            max: Some(u32::MAX.into())
        }
    );
    assert_eq!(IntRange::I32.limited(), Some(LimitedInteger::I32));
    assert_eq!(IntRange::U32.limited(), Some(LimitedInteger::U32));
    assert_eq!(IntRange::INTEGER.limited(), None);
    assert_eq!(IntRange::from(LimitedInteger::U32), IntRange::U32);
}

#[test]
fn display_uses_source_names() {
    let show = |ty: Type| ty.display(&Toy).to_string();
    assert_eq!(show(Type::top()), "<object>");
    assert_eq!(show(Type::bottom()), "type-union()");
    assert_eq!(show(of(Atom::Cone(SHAPE))), "<shape>");
    assert_eq!(show(of(Atom::Exact(SHAPE))), "exactly <shape>");
    assert_eq!(show(of(Atom::Cone(SQUARE))), "<square>");
    assert_eq!(show(of(Atom::Integer(IntRange::INTEGER))), "<integer>");
    assert_eq!(show(of(Atom::Integer(IntRange::I32))), "<i32>");
    assert_eq!(show(of(Atom::Integer(IntRange::U32))), "<u32>");
    assert_eq!(
        show(of(range(5, 10))),
        "limited(<integer>, min: 5, max: 10)"
    );
    assert_eq!(show(of(range(7, 7))), "singleton(7)");
    assert_eq!(
        show(of(Atom::Integer(IntRange {
            min: Some(0),
            max: None
        }))),
        "limited(<integer>, min: 0)"
    );
    assert_eq!(
        show(Type::union(
            [Atom::Boolean(true), Atom::Boolean(false)],
            &Toy
        )),
        "<boolean>"
    );
    assert_eq!(show(of(Atom::Boolean(true))), "singleton(#t)");
    assert_eq!(
        show(of(Atom::Singleton(Metaobject::Class(CIRCLE)))),
        "singleton(<circle>)"
    );
    assert_eq!(
        show(of(Atom::Singleton(Metaobject::Generic(GenericFunctionId(
            2
        ))))),
        "singleton(g2)"
    );
    assert_eq!(show(of(Atom::Callable)), "<callable>");
    assert_eq!(
        show(Type::union(
            [
                Atom::Exact(CIRCLE),
                Atom::Boolean(false),
                Atom::Boolean(true),
                Atom::Callable
            ],
            &Toy
        )),
        "type-union(exactly <circle>, <boolean>, <callable>)"
    );
}
