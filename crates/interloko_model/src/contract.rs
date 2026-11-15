//! Type contracts: what code compiled against a definition relies on.
//!
//! A [`Contract`] records, as [`Type`]s, the values a definition accepts and
//! returns as the code calling it was compiled to assume. The compiler trusts
//! those types when it lowers a call (a result typed `<circle>` has its slots
//! read without a class check), so they are a promise that outlives one
//! compile: when replacement code takes over a definition while code compiled
//! against the old one still runs, the new contract must keep the old
//! promise. [`Contract::check_replaces`] states that rule:
//!
//! - a parameter may *widen*: the new definition accepts at least every
//!   value callers were compiled to pass;
//! - a result may *narrow*: the new definition returns only values callers
//!   were compiled to expect.
//!
//! Anything else is a [`ContractBreak`].
//!
//! Contracts are derived from declared types and the compiler's own inferred
//! value kinds, never from a richer analysis of a callee's body, so editing a
//! body leaves its contract unchanged unless the kinds the compiler trusts
//! change too.

use alloc::vec::Vec;

use crate::types::{ClassHierarchy, Type};

/// The types a definition's callers rely on.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Contract {
    /// Values each parameter accepts, in call order.
    pub params: Vec<Type>,
    /// Values each result may hold, in return order.
    pub results: Vec<Type>,
}

/// Which value of a contract a [`ContractBreak`] concerns.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ContractPosition {
    /// The zero-based parameter.
    Parameter(usize),
    /// The zero-based result.
    Result(usize),
}

/// A change to a contract that code compiled against the old one could
/// observe.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ContractBreak {
    /// The definitions take or return different numbers of values.
    Arity,
    /// A parameter no longer accepts every value it did, or a result may
    /// hold a value it could not.
    Type {
        /// The parameter or result.
        position: ContractPosition,
        /// Its type in the old contract.
        old: Type,
        /// Its type in the new contract.
        new: Type,
    },
}

impl Contract {
    /// Checks that a definition with this contract can stand in for one
    /// with `old`, for code compiled against `old`: every parameter at least
    /// as wide, every result at most as wide.
    ///
    /// Returns the first break, parameters before results.
    pub fn check_replaces<H: ClassHierarchy + ?Sized>(
        &self,
        old: &Self,
        hierarchy: &H,
    ) -> Result<(), ContractBreak> {
        if self.params.len() != old.params.len() || self.results.len() != old.results.len() {
            return Err(ContractBreak::Arity);
        }
        let broken = |position, old: &Type, new: &Type| ContractBreak::Type {
            position,
            old: old.clone(),
            new: new.clone(),
        };
        for (index, (new, old)) in self.params.iter().zip(&old.params).enumerate() {
            if !old.is_subtype(new, hierarchy) {
                return Err(broken(ContractPosition::Parameter(index), old, new));
            }
        }
        for (index, (new, old)) in self.results.iter().zip(&old.results).enumerate() {
            if !new.is_subtype(old, hierarchy) {
                return Err(broken(ContractPosition::Result(index), old, new));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Atom, IntRange};
    use crate::{BuiltinValueClass, ClassId};

    /// `<object>` (0) with `<shape>` (1) and its subclass `<circle>` (2).
    struct Shapes;

    impl ClassHierarchy for Shapes {
        fn superclass(&self, class: ClassId) -> Option<ClassId> {
            match class.0 {
                1 => Some(ClassId(0)),
                2 => Some(ClassId(1)),
                _ => None,
            }
        }

        fn is_closed(&self, _class: ClassId) -> bool {
            false
        }

        fn builtin_class(&self, _class: BuiltinValueClass) -> Option<ClassId> {
            None
        }
    }

    fn cone(class: u32) -> Type {
        Type::of(Atom::Cone(ClassId(class)), &Shapes)
    }

    fn contract(params: &[Type], results: &[Type]) -> Contract {
        Contract {
            params: params.to_vec(),
            results: results.to_vec(),
        }
    }

    #[test]
    fn parameters_may_widen_and_results_may_narrow() {
        let old = contract(&[cone(2)], &[cone(1)]);
        assert_eq!(old.check_replaces(&old, &Shapes), Ok(()));
        let widened = contract(&[cone(1)], &[cone(2)]);
        assert_eq!(widened.check_replaces(&old, &Shapes), Ok(()));
        let top = contract(&[Type::top()], &[Type::bottom()]);
        assert_eq!(top.check_replaces(&old, &Shapes), Ok(()));
    }

    #[test]
    fn a_narrowed_parameter_or_widened_result_breaks_the_contract() {
        let old = contract(&[cone(1)], &[cone(2)]);
        assert_eq!(
            contract(&[cone(2)], &[cone(2)]).check_replaces(&old, &Shapes),
            Err(ContractBreak::Type {
                position: ContractPosition::Parameter(0),
                old: cone(1),
                new: cone(2),
            })
        );
        assert_eq!(
            contract(&[cone(1)], &[Type::top()]).check_replaces(&old, &Shapes),
            Err(ContractBreak::Type {
                position: ContractPosition::Result(0),
                old: cone(2),
                new: Type::top(),
            })
        );
        let integer = Type::of(Atom::Integer(IntRange::INTEGER), &Shapes);
        let i32 = Type::of(Atom::Integer(IntRange::I32), &Shapes);
        assert!(
            contract(&[i32], &[])
                .check_replaces(&contract(&[integer], &[]), &Shapes)
                .is_err(),
            "integer ranges compare too"
        );
    }

    #[test]
    fn a_different_number_of_values_is_an_arity_break() {
        let old = contract(&[cone(1)], &[]);
        assert_eq!(
            contract(&[cone(1), cone(1)], &[]).check_replaces(&old, &Shapes),
            Err(ContractBreak::Arity)
        );
        assert_eq!(
            contract(&[cone(1)], &[cone(1)]).check_replaces(&old, &Shapes),
            Err(ContractBreak::Arity)
        );
    }
}
