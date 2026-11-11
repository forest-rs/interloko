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

use crate::sem::{BinaryOp, NumericKind};

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
    use super::{ArithmeticRule, ComparisonRule, arithmetic, comparison, promote};
    use crate::sem::{BinaryOp, NumericKind};

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
}
