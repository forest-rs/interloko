//! Type contracts: what code compiled against a definition relies on.
//!
//! A [`Contract`] records the values a definition accepts and returns as the
//! code calling it was compiled to assume. The compiler trusts those
//! assumptions when it lowers a call (a result typed `<circle>` has its slots
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
//! Each parameter or result is a [`ValueContract`]: a [`Type`], plus the
//! signature callers assume when the value is a callable they will call.
//! The lattice's callable atom carries no signature, so the signature is
//! recorded beside it as a nested contract and compared with the same rule,
//! flipped where the callable is itself a parameter: a callable callers
//! pass must accept what the new definition will pass it, and a callable
//! the new definition returns must accept what callers will pass it. A
//! signature that mentions itself (a function returning itself) refers to
//! an [`enclosing`](CallableContract::Enclosing) one, and contracts holding
//! such a reference compare by equality.
//!
//! Contracts are derived from declared types and the compiler's own inferred
//! value kinds, never from a richer analysis of a callee's body, so editing a
//! body leaves its contract unchanged unless the kinds the compiler trusts
//! change too.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use crate::types::{ClassHierarchy, Type, TypeNames};

/// What a definition's callers rely on, or what callers of a callable value
/// rely on when they call it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Contract {
    /// What each parameter accepts, in call order.
    pub params: Vec<ValueContract>,
    /// What each result may hold, in return order.
    pub results: Vec<ValueContract>,
    /// Whether the last parameter is a `#rest` parameter: callers pass the
    /// arguments after the required ones in a vector they make, so a
    /// definition that takes one cannot stand in for one that does not,
    /// nor the other way round.
    pub rest: bool,
}

/// What callers assume about one parameter or result.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ValueContract {
    /// The values it may hold.
    pub ty: Type,
    /// For a callable that callers call, the signature they call it with;
    /// `None` when they assume none.
    pub signature: Option<CallableContract>,
}

/// The signature callers assume when they call a callable value.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum CallableContract {
    /// The callable's parameters and results.
    Signature(Box<Contract>),
    /// The same signature as an enclosing contract, counted outward from
    /// the one this value is a parameter or result of (`0`), for a
    /// signature that mentions itself.
    Enclosing(usize),
}

impl From<Type> for ValueContract {
    fn from(ty: Type) -> Self {
        Self {
            ty,
            signature: None,
        }
    }
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
    /// hold a value it could not, either in its type or in the signature
    /// of a callable it holds.
    Value {
        /// The parameter or result.
        position: ContractPosition,
        /// It in the old contract.
        old: ValueContract,
        /// It in the new contract.
        new: ValueContract,
    },
}

impl Contract {
    /// Checks that a definition with this contract can stand in for one
    /// with `old`, for code compiled against `old`: every parameter at least
    /// as wide, every result at most as wide, and every callable signature
    /// compatible the same way. When either contract holds a
    /// [`CallableContract::Enclosing`] reference, every value must be equal
    /// instead.
    ///
    /// Returns the first break, parameters before results.
    pub fn check_replaces<H: ClassHierarchy + ?Sized>(
        &self,
        old: &Self,
        hierarchy: &H,
    ) -> Result<(), ContractBreak> {
        if self.params.len() != old.params.len()
            || self.results.len() != old.results.len()
            || self.rest != old.rest
        {
            return Err(ContractBreak::Arity);
        }
        let exact = self.refers_outward() || old.refers_outward();
        let broken = |position, old: &ValueContract, new: &ValueContract| ContractBreak::Value {
            position,
            old: old.clone(),
            new: new.clone(),
        };
        for (index, (new, old)) in self.params.iter().zip(&old.params).enumerate() {
            let kept = if exact {
                new == old
            } else {
                old.ty.is_subtype(&new.ty, hierarchy)
                    && stands_in(old.signature.as_ref(), new.signature.as_ref(), hierarchy)
            };
            if !kept {
                return Err(broken(ContractPosition::Parameter(index), old, new));
            }
        }
        for (index, (new, old)) in self.results.iter().zip(&old.results).enumerate() {
            let kept = if exact {
                new == old
            } else {
                new.ty.is_subtype(&old.ty, hierarchy)
                    && stands_in(new.signature.as_ref(), old.signature.as_ref(), hierarchy)
            };
            if !kept {
                return Err(broken(ContractPosition::Result(index), old, new));
            }
        }
        Ok(())
    }

    /// Whether any value, at any depth, holds an
    /// [`CallableContract::Enclosing`] reference.
    fn refers_outward(&self) -> bool {
        self.params
            .iter()
            .chain(&self.results)
            .any(|value| match &value.signature {
                Some(CallableContract::Enclosing(_)) => true,
                Some(CallableContract::Signature(contract)) => contract.refers_outward(),
                None => false,
            })
    }
}

/// Whether a callable promised to have `provided` can be called as
/// `assumed`.
fn stands_in<H: ClassHierarchy + ?Sized>(
    provided: Option<&CallableContract>,
    assumed: Option<&CallableContract>,
    hierarchy: &H,
) -> bool {
    match (provided, assumed) {
        (_, None) => true,
        (
            Some(CallableContract::Signature(provided)),
            Some(CallableContract::Signature(assumed)),
        ) => provided.check_replaces(assumed, hierarchy).is_ok(),
        (provided, assumed) => provided == assumed,
    }
}

impl ValueContract {
    /// Returns a value that displays this with source names: its type, or
    /// for a callable with a signature, `callable(<integer>) => <circle>`.
    /// A self-referring signature shows as `callable(...)`.
    #[must_use]
    pub fn display<'a, N: TypeNames + ClassHierarchy + ?Sized>(
        &'a self,
        names: &'a N,
    ) -> ValueContractDisplay<'a, N> {
        ValueContractDisplay { value: self, names }
    }
}

/// Displays a [`ValueContract`]; see [`ValueContract::display`].
pub struct ValueContractDisplay<'a, N: ?Sized> {
    value: &'a ValueContract,
    names: &'a N,
}

impl<N: TypeNames + ClassHierarchy + ?Sized> fmt::Display for ValueContractDisplay<'_, N> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self.names;
        let contract = match &self.value.signature {
            None => return write!(out, "{}", self.value.ty.display(names)),
            Some(CallableContract::Enclosing(_)) => return out.write_str("callable(...)"),
            Some(CallableContract::Signature(contract)) => contract,
        };
        out.write_str("callable(")?;
        let required = contract.params.len() - usize::from(contract.rest);
        for (index, param) in contract.params.iter().enumerate() {
            if index > 0 {
                out.write_str(", ")?;
            }
            if index == required {
                out.write_str("#rest ")?;
            }
            write!(out, "{}", param.display(names))?;
        }
        out.write_str(") => ")?;
        match contract.results.as_slice() {
            [result] => write!(out, "{}", result.display(names)),
            results => {
                out.write_str("(")?;
                for (index, result) in results.iter().enumerate() {
                    if index > 0 {
                        out.write_str(", ")?;
                    }
                    write!(out, "{}", result.display(names))?;
                }
                out.write_str(")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::*;
    use crate::types::{Atom, IntRange};
    use crate::{BuiltinValueClass, ClassId, GenericFunctionId, MethodId, SlotId};

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

    impl TypeNames for Shapes {
        fn write_class(&self, out: &mut dyn fmt::Write, class: ClassId) -> fmt::Result {
            out.write_str(["<object>", "<shape>", "<circle>"][class.0 as usize])
        }

        fn write_generic(&self, out: &mut dyn fmt::Write, _: GenericFunctionId) -> fmt::Result {
            out.write_str("g")
        }

        fn write_method(&self, out: &mut dyn fmt::Write, _: MethodId) -> fmt::Result {
            out.write_str("m")
        }

        fn write_slot(&self, out: &mut dyn fmt::Write, _: ClassId, _: SlotId) -> fmt::Result {
            out.write_str("s")
        }
    }

    fn cone(class: u32) -> ValueContract {
        Type::of(Atom::Cone(ClassId(class)), &Shapes).into()
    }

    fn contract(params: &[ValueContract], results: &[ValueContract]) -> Contract {
        Contract {
            params: params.to_vec(),
            results: results.to_vec(),
            rest: false,
        }
    }

    /// A callable taking `params` and returning `results`.
    fn callable(params: &[ValueContract], results: &[ValueContract]) -> ValueContract {
        ValueContract {
            ty: Type::of(Atom::Callable, &Shapes),
            signature: Some(CallableContract::Signature(Box::new(contract(
                params, results,
            )))),
        }
    }

    fn broken(position: ContractPosition, old: ValueContract, new: ValueContract) -> ContractBreak {
        ContractBreak::Value { position, old, new }
    }

    #[test]
    fn parameters_may_widen_and_results_may_narrow() {
        let old = contract(&[cone(2)], &[cone(1)]);
        assert_eq!(old.check_replaces(&old, &Shapes), Ok(()));
        let widened = contract(&[cone(1)], &[cone(2)]);
        assert_eq!(widened.check_replaces(&old, &Shapes), Ok(()));
        let top = contract(&[Type::top().into()], &[Type::bottom().into()]);
        assert_eq!(top.check_replaces(&old, &Shapes), Ok(()));
    }

    #[test]
    fn a_narrowed_parameter_or_widened_result_breaks_the_contract() {
        let old = contract(&[cone(1)], &[cone(2)]);
        assert_eq!(
            contract(&[cone(2)], &[cone(2)]).check_replaces(&old, &Shapes),
            Err(broken(ContractPosition::Parameter(0), cone(1), cone(2)))
        );
        assert_eq!(
            contract(&[cone(1)], &[Type::top().into()]).check_replaces(&old, &Shapes),
            Err(broken(
                ContractPosition::Result(0),
                cone(2),
                Type::top().into()
            ))
        );
        let integer: ValueContract = Type::of(Atom::Integer(IntRange::INTEGER), &Shapes).into();
        let i32: ValueContract = Type::of(Atom::Integer(IntRange::I32), &Shapes).into();
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

    #[test]
    fn taking_rest_or_not_is_an_arity_break() {
        // A caller passes a `#rest` parameter's vector of the arguments
        // after the required ones itself, so a parameter of the same type
        // that is not `#rest` means a different call.
        let plain = contract(&[cone(1)], &[]);
        let rest = Contract {
            rest: true,
            ..plain.clone()
        };
        assert_eq!(rest.check_replaces(&rest, &Shapes), Ok(()));
        assert_eq!(
            rest.check_replaces(&plain, &Shapes),
            Err(ContractBreak::Arity)
        );
        assert_eq!(
            plain.check_replaces(&rest, &Shapes),
            Err(ContractBreak::Arity)
        );
    }

    #[test]
    fn a_returned_callable_may_accept_more_and_return_less() {
        let old = contract(&[], &[callable(&[cone(2)], &[cone(1)])]);
        let new = contract(&[], &[callable(&[cone(1)], &[cone(2)])]);
        assert_eq!(new.check_replaces(&old, &Shapes), Ok(()));
        assert_eq!(
            old.check_replaces(&new, &Shapes),
            Err(broken(
                ContractPosition::Result(0),
                callable(&[cone(1)], &[cone(2)]),
                callable(&[cone(2)], &[cone(1)])
            ))
        );
        // Callers assumed a signature; a result that promises none breaks it.
        let untyped = contract(&[], &[Type::of(Atom::Callable, &Shapes).into()]);
        assert!(untyped.check_replaces(&old, &Shapes).is_err());
        assert_eq!(old.check_replaces(&untyped, &Shapes), Ok(()));
    }

    #[test]
    fn a_callable_parameter_flips_the_rule() {
        // Callers pass a callable that takes a <shape> and returns a
        // <circle>. The new body may assume less of it, not more.
        let old = contract(&[callable(&[cone(1)], &[cone(2)])], &[]);
        let assumes_less = contract(&[callable(&[cone(2)], &[cone(1)])], &[]);
        assert_eq!(assumes_less.check_replaces(&old, &Shapes), Ok(()));
        assert!(old.check_replaces(&assumes_less, &Shapes).is_err());
        let untyped = contract(&[Type::of(Atom::Callable, &Shapes).into()], &[]);
        assert_eq!(untyped.check_replaces(&old, &Shapes), Ok(()));
        assert!(old.check_replaces(&untyped, &Shapes).is_err());
    }

    #[test]
    fn a_self_referring_signature_compares_by_equality() {
        let itself = ValueContract {
            ty: Type::of(Atom::Callable, &Shapes),
            signature: Some(CallableContract::Enclosing(0)),
        };
        let old = contract(&[cone(2)], core::slice::from_ref(&itself));
        assert_eq!(old.check_replaces(&old, &Shapes), Ok(()));
        // A widened parameter would do elsewhere, but not here.
        let widened = contract(&[cone(1)], &[itself]);
        assert_eq!(
            widened.check_replaces(&old, &Shapes),
            Err(broken(ContractPosition::Parameter(0), cone(2), cone(1)))
        );
    }

    #[test]
    fn values_display_with_their_signatures() {
        let show = |value: &ValueContract| value.display(&Shapes).to_string();
        assert_eq!(show(&cone(2)), "<circle>");
        assert_eq!(
            show(&callable(&[cone(1), cone(2)], &[cone(2)])),
            "callable(<shape>, <circle>) => <circle>"
        );
        assert_eq!(
            show(&callable(&[], &[cone(1), cone(2)])),
            "callable() => (<shape>, <circle>)"
        );
        let itself = ValueContract {
            ty: Type::of(Atom::Callable, &Shapes),
            signature: Some(CallableContract::Enclosing(1)),
        };
        assert_eq!(
            show(&callable(&[], &[itself])),
            String::from("callable() => callable(...)")
        );
    }
}
