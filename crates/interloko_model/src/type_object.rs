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
//! ```
//!
//! [`TypeObject`] describes one. It covers the types a runtime check
//! expects; callable types, which no check expects (a call through an
//! untyped `<callable>` is checked by the VM), have no type object.

use core::fmt;

use crate::types::{IntRange, TypeNames};
use crate::{ClassId, LimitedInteger};

/// A type object: a class, a limited integer type, or a subclass type.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
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

    /// Whether `self` and `other` denote the same values: the same class,
    /// the same range whatever its name, or the same subclass bound. This is
    /// what `=` compares.
    #[must_use]
    pub fn same_type(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::LimitedInteger { range: left, .. },
                Self::LimitedInteger { range: right, .. },
            ) => left == right,
            _ => self == other,
        }
    }

    /// Returns a value that displays this as Loko writes it: `<circle>`,
    /// `<i32>`, `limited(<integer>, min: 0, max: 255)` or
    /// `subclass(<shape>)`.
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
    fn a_named_limited_type_is_the_same_type_as_its_range() {
        let spelled = TypeObject::limited(IntRange::I32);
        let named = TypeObject::named(LimitedInteger::I32);
        assert_ne!(spelled, named);
        assert!(spelled.same_type(&named));
        assert!(!named.same_type(&TypeObject::named(LimitedInteger::U32)));
        assert!(!TypeObject::Class(ClassId(1)).same_type(&TypeObject::Subclass(ClassId(1))));
    }
}
