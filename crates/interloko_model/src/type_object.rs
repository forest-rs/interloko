//! Type objects: types a program holds as values.
//!
//! A type object denotes a set of values, as a static type does, but it is a
//! runtime value: a `<type-error>` reports the type its failed check
//! expected as one, and `instance?` tests a value against one. They are the
//! instances of the abstract kernel class `<type>`:
//!
//! ```text
//! <object>
//!   <type>
//!     <class>               a class: its instances and its subclasses'
//!     <limited-integer>     limited(<integer>, min: m, max: n), <i32>, <u32>
//!     <subclass-type>       the class objects of a class and its subclasses
//!     <singleton>           singleton(v): the one value v
//!     <union>               type-union(t, ...): the instances of any member
//! ```
//!
//! [`TypeObject`] describes one. It covers the types a runtime check
//! expects; callable types, which no check expects (a call through an
//! untyped `<callable>` is checked by the VM), have no type object.

use alloc::vec::Vec;
use core::fmt;

use crate::types::{IntRange, TypeNames};
use crate::{ClassId, LimitedInteger};

/// The value a [`TypeObject::Singleton`] holds: one that types can name
/// exactly, a boolean or an `<integer>`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SingletonValue {
    /// `#t` or `#f`.
    Boolean(bool),
    /// An `<integer>`.
    Integer(i64),
}

impl fmt::Display for SingletonValue {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(true) => out.write_str("#t"),
            Self::Boolean(false) => out.write_str("#f"),
            Self::Integer(value) => write!(out, "{value}"),
        }
    }
}

/// A type object: a class, a limited integer type, a subclass type, a
/// singleton or a union of these.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum TypeObject {
    /// A class, whose type object is its class object: its instances and
    /// those of its subclasses.
    Class(ClassId),
    /// A `<limited-integer>`: the `<integer>`s in a range.
    LimitedInteger {
        /// The range; a `None` bound is unbounded.
        range: IntRange,
        /// The named limited type it was written as, such as `<i32>`, so it
        /// prints as that name; `None` prints the range in full.
        named: Option<LimitedInteger>,
    },
    /// A `<subclass-type>`: the class objects naming the class or one of its
    /// subclasses, the values of a `class <shape>` declaration.
    Subclass(ClassId),
    /// A `<singleton>`: the one value it holds, `singleton(#f)`.
    Singleton(SingletonValue),
    /// A `<union>`: the instances of any of its members, written
    /// `type-union(<a>, <b>)`, and `false-or(<a>)` for `<a>` or `#f`. Its
    /// members are none of them unions; [`Self::union`] makes one of at
    /// least two, or of none, the type of no value.
    Union(Vec<TypeObject>),
}

impl TypeObject {
    /// The type object of the named limited type `limited`, such as `<i32>`.
    #[must_use]
    pub const fn named(limited: LimitedInteger) -> Self {
        Self::LimitedInteger {
            range: IntRange {
                min: Some(limited.min()),
                max: Some(limited.max()),
            },
            named: Some(limited),
        }
    }

    /// The type object of `limited(<integer>, ...)` over `range`, printed
    /// in full even when it equals a named type's range.
    #[must_use]
    pub const fn limited(range: IntRange) -> Self {
        Self::LimitedInteger { range, named: None }
    }

    /// The type object of `false-or(ty)`: `ty`'s values and `#f`.
    #[must_use]
    pub fn false_or(ty: Self) -> Self {
        Self::union([ty, Self::Singleton(SingletonValue::Boolean(false))])
    }

    /// The union of `members`: their members when they are unions
    /// themselves, each once, in order; the one member when there is only
    /// one.
    #[must_use]
    pub fn union(members: impl IntoIterator<Item = Self>) -> Self {
        let mut flat: Vec<Self> = Vec::new();
        for member in members {
            let parts = match member {
                Self::Union(parts) => parts,
                member => alloc::vec![member],
            };
            for part in parts {
                if !flat.iter().any(|known| known.same_type(&part)) {
                    flat.push(part);
                }
            }
        }
        if flat.len() == 1 {
            return flat.pop().unwrap_or(Self::Union(Vec::new()));
        }
        Self::Union(flat)
    }

    /// Whether `self` and `other` denote the same values: the same class,
    /// the same range whatever its name, the same subclass bound, the same
    /// singleton value, or unions of the same members in any order. This
    /// is what `=` compares.
    #[must_use]
    pub fn same_type(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::LimitedInteger { range: left, .. },
                Self::LimitedInteger { range: right, .. },
            ) => left == right,
            (Self::Union(left), Self::Union(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .all(|member| right.iter().any(|other| member.same_type(other)))
            }
            _ => self == other,
        }
    }

    /// Returns a value that displays this as Loko writes it: `<circle>`,
    /// `<i32>`, `limited(<integer>, min: 0, max: 255)`, `subclass(<shape>)`,
    /// `singleton(#f)`, `type-union(<a>, <b>)` or, for a union of one type
    /// and `#f`, `false-or(<a>)`.
    #[must_use]
    pub fn display<'a, N: TypeNames + ?Sized>(&'a self, names: &'a N) -> TypeObjectDisplay<'a, N> {
        TypeObjectDisplay {
            object: self,
            names,
        }
    }
}

/// Displays a [`TypeObject`]; see [`TypeObject::display`].
pub struct TypeObjectDisplay<'a, N: ?Sized> {
    object: &'a TypeObject,
    names: &'a N,
}

impl<N: TypeNames + ?Sized> fmt::Display for TypeObjectDisplay<'_, N> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.object {
            TypeObject::Class(class) => self.names.write_class(out, *class),
            TypeObject::LimitedInteger {
                named: Some(limited),
                ..
            } => out.write_str(limited.name()),
            TypeObject::LimitedInteger { range, named: None } => {
                out.write_str("limited(<integer>")?;
                if let Some(min) = range.min {
                    write!(out, ", min: {min}")?;
                }
                if let Some(max) = range.max {
                    write!(out, ", max: {max}")?;
                }
                out.write_str(")")
            }
            TypeObject::Subclass(class) => {
                out.write_str("subclass(")?;
                self.names.write_class(out, *class)?;
                out.write_str(")")
            }
            TypeObject::Singleton(value) => write!(out, "singleton({value})"),
            TypeObject::Union(members) => {
                let false_singleton = TypeObject::Singleton(SingletonValue::Boolean(false));
                if let [first, second] = members.as_slice() {
                    let other = if *second == false_singleton {
                        Some(first)
                    } else if *first == false_singleton {
                        Some(second)
                    } else {
                        None
                    };
                    if let Some(other) = other {
                        return write!(out, "false-or({})", other.display(self.names));
                    }
                }
                out.write_str("type-union(")?;
                for (index, member) in members.iter().enumerate() {
                    if index > 0 {
                        out.write_str(", ")?;
                    }
                    write!(out, "{}", member.display(self.names))?;
                }
                out.write_str(")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::ToString;

    use super::*;
    use crate::{GenericFunctionId, MethodId, SlotId};

    struct Names;

    impl TypeNames for Names {
        fn write_class(&self, out: &mut dyn fmt::Write, class: ClassId) -> fmt::Result {
            write!(out, "<c{}>", class.0)
        }

        fn write_generic(
            &self,
            out: &mut dyn fmt::Write,
            generic: GenericFunctionId,
        ) -> fmt::Result {
            write!(out, "g{}", generic.0)
        }

        fn write_method(&self, out: &mut dyn fmt::Write, method: MethodId) -> fmt::Result {
            write!(out, "m{}", method.0)
        }

        fn write_slot(
            &self,
            out: &mut dyn fmt::Write,
            class: ClassId,
            slot: SlotId,
        ) -> fmt::Result {
            write!(out, "c{}.s{}", class.0, slot.0)
        }
    }

    #[test]
    fn type_objects_print_as_loko_writes_them() {
        let print = |object: TypeObject| object.display(&Names).to_string();
        assert_eq!(print(TypeObject::Class(ClassId(3))), "<c3>");
        assert_eq!(print(TypeObject::Subclass(ClassId(3))), "subclass(<c3>)");
        assert_eq!(print(TypeObject::named(LimitedInteger::U32)), "<u32>");
        assert_eq!(
            print(TypeObject::limited(IntRange {
                min: Some(0),
                max: Some(255)
            })),
            "limited(<integer>, min: 0, max: 255)"
        );
        assert_eq!(
            print(TypeObject::limited(IntRange {
                min: Some(0),
                max: None
            })),
            "limited(<integer>, min: 0)"
        );
    }

    #[test]
    fn singletons_and_unions_print_as_loko_writes_them() {
        let print = |object: TypeObject| object.display(&Names).to_string();
        let no = TypeObject::Singleton(SingletonValue::Boolean(false));
        assert_eq!(print(no.clone()), "singleton(#f)");
        assert_eq!(
            print(TypeObject::Singleton(SingletonValue::Integer(-3))),
            "singleton(-3)"
        );
        assert_eq!(
            print(TypeObject::false_or(TypeObject::Class(ClassId(3)))),
            "false-or(<c3>)"
        );
        let union = TypeObject::union([
            TypeObject::Class(ClassId(3)),
            TypeObject::union([TypeObject::named(LimitedInteger::I32), no.clone()]),
            TypeObject::Class(ClassId(3)),
        ]);
        assert_eq!(print(union), "type-union(<c3>, <i32>, singleton(#f))");
        assert_eq!(
            TypeObject::union([no.clone(), no.clone()]),
            no,
            "a union of one member is that member"
        );
    }

    #[test]
    fn unions_are_the_same_type_whatever_their_order() {
        let no = TypeObject::Singleton(SingletonValue::Boolean(false));
        let class = TypeObject::Class(ClassId(1));
        assert!(
            TypeObject::union([class.clone(), no.clone()])
                .same_type(&TypeObject::union([no.clone(), class.clone()]))
        );
        assert!(
            !TypeObject::union([class.clone(), no])
                .same_type(&TypeObject::union([class, TypeObject::Class(ClassId(2))]))
        );
    }

    #[test]
    fn a_named_limited_type_is_the_same_type_as_its_range() {
        let spelled = TypeObject::limited(IntRange::I32);
        let named = TypeObject::named(LimitedInteger::I32);
        assert_ne!(spelled, named);
        assert!(spelled.same_type(&named));
        assert!(!named.same_type(&TypeObject::named(LimitedInteger::U32)));
        assert!(!TypeObject::Class(ClassId(1)).same_type(&TypeObject::Subclass(ClassId(1))));
    }
}
