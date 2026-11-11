//! Numeric promotion rules: how mixed numeric operands combine.
//!
//! This module is the one place that decides what `+`, `-`, `*`, `/` and the
//! comparisons mean for each pair of concrete numeric kinds, in the spirit of
//! Julia's promotion rules. Static lowering of monomorphic operands and the
//! methods of the built-in arithmetic generic functions both ask it, so a
//! direct `1 + 2.5` and a dispatched one agree by construction.
//!
//! The rules today:
//!
//! - `<integer> op <integer>` stays an integer; `+ - *` are overflow-checked
//!   and `/` is undefined (it becomes exact rational division later).
//! - Any operation with a `<double-float>` operand converts the integer
//!   operand to a float (`i64.to_f64`, ties to even) and applies the IEEE 754
//!   operator.
//! - Comparisons of two integers or two floats compare directly. An integer
//!   and a float compare exactly, as mathematical values (Common Lisp
//!   semantics): `2^53 + 1 = 2.0^53` is false. Every ordering operator is
//!   exact, not only `=`, so `<`, `=` and `>` stay a trichotomy on non-NaN
//!   operands.

use alloc::vec::Vec;

use crate::sem::{BinaryOp, CompareOp, NumericKind};

/// How an arithmetic operator applies to two concrete numeric kinds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ArithmeticRule {
    /// Convert both operands to `kind`, then apply the operator there.
    Promote {
        /// Common representation of the operands and the result.
        kind: NumericKind,
    },
    /// The operator is not defined for these operands.
    Undefined,
}

/// How a comparison applies to two concrete numeric kinds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ComparisonRule {
    /// Both operands have `kind`; compare them directly.
    Same {
        /// Shared operand kind.
        kind: NumericKind,
    },
    /// One integer and one float, compared exactly as mathematical values.
    ExactMixed,
}

/// Operator of a built-in numeric generic function.
///
/// When an operand's static type is `<object>` (or a class that is not a
/// number), the operator dispatches through a compiler-provided generic
/// function whose methods are generated from these rules, one per pair of
/// concrete numeric classes. The generics are sealed over `<real>`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NumericOperator {
    /// `+`, `-`, `*` or `/`.
    Arithmetic(BinaryOp),
    /// `=`, `<`, `<=`, `>` or `>=`.
    Compare(CompareOp),
    /// Unary `-`.
    Negate,
}

impl NumericOperator {
    /// Returns the generic function's name. Binary operators use their
    /// source spelling, which no Loko definition can name; unary `-` is
    /// Dylan's `negative`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Arithmetic(BinaryOp::Add) => "+",
            Self::Arithmetic(BinaryOp::Sub) => "-",
            Self::Arithmetic(BinaryOp::Mul) => "*",
            Self::Arithmetic(BinaryOp::Div) => "/",
            Self::Compare(CompareOp::Eq) => "=",
            Self::Compare(CompareOp::Lt) => "<",
            Self::Compare(CompareOp::Le) => "<=",
            Self::Compare(CompareOp::Gt) => ">",
            Self::Compare(CompareOp::Ge) => ">=",
            Self::Negate => "negative",
        }
    }

    /// Returns the number of operands.
    #[must_use]
    pub const fn arity(self) -> usize {
        match self {
            Self::Negate => 1,
            Self::Arithmetic(_) | Self::Compare(_) => 2,
        }
    }

    /// Returns the operand kinds of every method the generic has: each
    /// combination of concrete numeric kinds the rules define.
    #[must_use]
    pub fn method_operands(self) -> Vec<Vec<NumericKind>> {
        match self {
            Self::Negate => NUMERIC_KINDS
                .iter()
                .map(|kind| alloc::vec![*kind])
                .collect(),
            Self::Arithmetic(op) => NUMERIC_KINDS
                .iter()
                .flat_map(|lhs| NUMERIC_KINDS.iter().map(move |rhs| (*lhs, *rhs)))
                .filter(|(lhs, rhs)| arithmetic(op, *lhs, *rhs) != ArithmeticRule::Undefined)
                .map(|(lhs, rhs)| alloc::vec![lhs, rhs])
                .collect(),
            Self::Compare(_) => NUMERIC_KINDS
                .iter()
                .flat_map(|lhs| NUMERIC_KINDS.iter().map(move |rhs| alloc::vec![*lhs, *rhs]))
                .collect(),
        }
    }
}

/// Concrete numeric kinds the rules cover, in method declaration order.
pub const NUMERIC_KINDS: [NumericKind; 2] = [NumericKind::Integer, NumericKind::Float];

/// Returns the common kind two numeric operands promote to.
#[must_use]
pub const fn promote(lhs: NumericKind, rhs: NumericKind) -> NumericKind {
    match (lhs, rhs) {
        (NumericKind::Integer, NumericKind::Integer) => NumericKind::Integer,
        _ => NumericKind::Float,
    }
}

/// Returns how `lhs op rhs` is computed.
#[must_use]
pub const fn arithmetic(op: BinaryOp, lhs: NumericKind, rhs: NumericKind) -> ArithmeticRule {
    match (op, promote(lhs, rhs)) {
        (BinaryOp::Div, NumericKind::Integer) => ArithmeticRule::Undefined,
        (_, kind) => ArithmeticRule::Promote { kind },
    }
}

/// Returns how a comparison of `lhs` with `rhs` is computed.
#[must_use]
pub const fn comparison(lhs: NumericKind, rhs: NumericKind) -> ComparisonRule {
    match (lhs, rhs) {
        (NumericKind::Integer, NumericKind::Integer) => ComparisonRule::Same {
            kind: NumericKind::Integer,
        },
        (NumericKind::Float, NumericKind::Float) => ComparisonRule::Same {
            kind: NumericKind::Float,
        },
        _ => ComparisonRule::ExactMixed,
    }
}

#[cfg(test)]
mod tests {
    use super::{ArithmeticRule, ComparisonRule, NumericOperator, arithmetic, comparison, promote};
    use crate::sem::{BinaryOp, CompareOp, NumericKind};

    #[test]
    fn floats_are_contagious_and_integer_division_is_undefined() {
        use NumericKind::{Float, Integer};
        assert_eq!(promote(Integer, Integer), Integer);
        assert_eq!(promote(Integer, Float), Float);
        assert_eq!(promote(Float, Integer), Float);
        assert_eq!(
            arithmetic(BinaryOp::Div, Integer, Integer),
            ArithmeticRule::Undefined
        );
        assert_eq!(
            arithmetic(BinaryOp::Div, Integer, Float),
            ArithmeticRule::Promote { kind: Float }
        );
        assert_eq!(
            arithmetic(BinaryOp::Mul, Integer, Integer),
            ArithmeticRule::Promote { kind: Integer }
        );
        assert_eq!(comparison(Float, Integer), ComparisonRule::ExactMixed);
        assert_eq!(
            comparison(Float, Float),
            ComparisonRule::Same { kind: Float }
        );
    }

    #[test]
    fn numeric_generics_cover_every_defined_operand_pair() {
        assert_eq!(
            NumericOperator::Arithmetic(BinaryOp::Add)
                .method_operands()
                .len(),
            4
        );
        // `<integer> / <integer>` has no method.
        assert_eq!(
            NumericOperator::Arithmetic(BinaryOp::Div)
                .method_operands()
                .len(),
            3
        );
        assert_eq!(
            NumericOperator::Compare(CompareOp::Lt)
                .method_operands()
                .len(),
            4
        );
        assert_eq!(NumericOperator::Negate.method_operands().len(), 2);
    }
}
