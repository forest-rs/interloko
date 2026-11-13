//! Numeric promotion rules: how mixed numeric operands combine.
//!
//! This module is the one place that decides what `+`, `-`, `*`, `/` and the
//! comparisons mean for each pair of concrete numeric kinds, in the spirit of
//! Julia's promotion rules with Common Lisp's contagion. Static lowering of
//! monomorphic operands and the methods of the built-in arithmetic generic
//! functions both ask it, so a direct `1 + 2.5` and a dispatched one agree
//! by construction.
//!
//! The rules today:
//!
//! - Operands of one kind stay that kind. Integer `+ - *` are
//!   overflow-checked for every width: `<integer>` traps where bignum
//!   promotion will go, and `<i32>`/`<u32>` trap when the result leaves the
//!   type's range, because those limited types promise their range. Integer
//!   `/` is undefined (it becomes exact rational division later).
//! - Floats are contagious, to the widest float present: anything with a
//!   `<double-float>` computes in binary64, otherwise anything with a
//!   `<single-float>` computes in binary32 (`2 * x` with a single-float `x`
//!   stays single). Integer operands convert to the float, rounding to
//!   nearest, ties to even.
//! - Integers of different kinds compute as `<integer>`: `<i32>` and `<u32>`
//!   are ranges of `<integer>`, and only an operation inside one range can
//!   promise to stay in it. Unsuffixed literals adapt to their partner's
//!   type first (see `sem::adapt_literal`), so `x + 1` with an `<i32>` `x`
//!   stays `<i32>`.
//! - Negation keeps the operand's kind, except that `<u32>` has no negative
//!   members, so `-x` of a `<u32>` is the `<integer>` it denotes.
//! - Comparisons are exact, as mathematical values (Common Lisp
//!   semantics). Operands meet at a kind that holds both exactly: integers
//!   at `<integer>`, floats at binary64, and a 32-bit integer with a float
//!   at binary64. Only an `<integer>` meeting a float needs the exact mixed
//!   comparison, because binary64 cannot hold every `i64`: `2^53 + 1 =
//!   2.0^53` is false. Every ordering operator is exact, not only `=`, so
//!   `<`, `=` and `>` stay a trichotomy on non-NaN operands.

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
    /// Convert both operands to `kind`, which holds each of them exactly,
    /// then compare directly.
    Promote {
        /// Common operand kind.
        kind: NumericKind,
    },
    /// One `<integer>` and one float, compared exactly as mathematical
    /// values. A `<single-float>` operand widens to binary64 first, which
    /// is exact.
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
    /// combination of concrete numeric classes the rules define.
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

/// Concrete numeric classes the generic methods cover, in method
/// declaration order. `<i32>` and `<u32>` are not classes: their values
/// dispatch as the `<integer>`s they are.
pub const NUMERIC_KINDS: [NumericKind; 3] = [
    NumericKind::Integer,
    NumericKind::Float,
    NumericKind::SingleFloat,
];

/// Returns the common kind two numeric operands promote to.
#[must_use]
pub const fn promote(lhs: NumericKind, rhs: NumericKind) -> NumericKind {
    match (lhs, rhs) {
        (NumericKind::Float, _) | (_, NumericKind::Float) => NumericKind::Float,
        (NumericKind::SingleFloat, _) | (_, NumericKind::SingleFloat) => NumericKind::SingleFloat,
        (NumericKind::I32, NumericKind::I32) => NumericKind::I32,
        (NumericKind::U32, NumericKind::U32) => NumericKind::U32,
        _ => NumericKind::Integer,
    }
}

/// Returns how `lhs op rhs` is computed.
#[must_use]
pub const fn arithmetic(op: BinaryOp, lhs: NumericKind, rhs: NumericKind) -> ArithmeticRule {
    let kind = promote(lhs, rhs);
    if matches!(op, BinaryOp::Div) && kind.is_integer() {
        ArithmeticRule::Undefined
    } else {
        ArithmeticRule::Promote { kind }
    }
}

/// Returns the kind `-x` computes in for an operand of kind `kind`.
#[must_use]
pub const fn negation(kind: NumericKind) -> NumericKind {
    match kind {
        NumericKind::U32 => NumericKind::Integer,
        kind => kind,
    }
}

/// Returns how a comparison of `lhs` with `rhs` is computed.
#[must_use]
pub const fn comparison(lhs: NumericKind, rhs: NumericKind) -> ComparisonRule {
    if lhs as u8 == rhs as u8 {
        return ComparisonRule::Promote { kind: lhs };
    }
    match (lhs, rhs) {
        (NumericKind::Integer, rhs) | (rhs, NumericKind::Integer) if rhs.is_float() => {
            ComparisonRule::ExactMixed
        }
        _ if lhs.is_integer() && rhs.is_integer() => ComparisonRule::Promote {
            kind: NumericKind::Integer,
        },
        // Floats of two formats, or a 32-bit integer and a float: binary64
        // holds both operands exactly.
        _ => ComparisonRule::Promote {
            kind: NumericKind::Float,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ArithmeticRule, ComparisonRule, NumericOperator, arithmetic, comparison, negation, promote,
    };
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
            ComparisonRule::Promote { kind: Float }
        );
    }

    #[test]
    fn floats_widen_to_the_widest_format_present() {
        use NumericKind::{Float, I32, Integer, SingleFloat, U32};
        for single_partner in [Integer, I32, U32, SingleFloat] {
            assert_eq!(promote(SingleFloat, single_partner), SingleFloat);
            assert_eq!(promote(single_partner, SingleFloat), SingleFloat);
        }
        assert_eq!(promote(SingleFloat, Float), Float);
        assert_eq!(promote(Float, SingleFloat), Float);
        assert_eq!(promote(U32, Float), Float);
        assert_eq!(
            arithmetic(BinaryOp::Div, I32, SingleFloat),
            ArithmeticRule::Promote { kind: SingleFloat }
        );
    }

    #[test]
    fn integer_widths_stay_only_when_they_match() {
        use NumericKind::{I32, Integer, U32};
        assert_eq!(promote(I32, I32), I32);
        assert_eq!(promote(U32, U32), U32);
        assert_eq!(promote(I32, U32), Integer);
        assert_eq!(promote(U32, Integer), Integer);
        assert_eq!(promote(Integer, I32), Integer);
        for kind in [I32, U32] {
            assert_eq!(
                arithmetic(BinaryOp::Div, kind, kind),
                ArithmeticRule::Undefined
            );
        }
        assert_eq!(negation(I32), I32);
        assert_eq!(negation(U32), Integer);
    }

    #[test]
    fn comparisons_meet_where_both_operands_are_exact() {
        use NumericKind::{Float, I32, Integer, SingleFloat, U32};
        assert_eq!(
            comparison(I32, U32),
            ComparisonRule::Promote { kind: Integer }
        );
        assert_eq!(
            comparison(U32, SingleFloat),
            ComparisonRule::Promote { kind: Float }
        );
        assert_eq!(
            comparison(SingleFloat, Float),
            ComparisonRule::Promote { kind: Float }
        );
        assert_eq!(comparison(Integer, SingleFloat), ComparisonRule::ExactMixed);
        assert_eq!(
            comparison(SingleFloat, SingleFloat),
            ComparisonRule::Promote { kind: SingleFloat }
        );
    }

    #[test]
    fn numeric_generics_cover_every_defined_operand_pair() {
        assert_eq!(
            NumericOperator::Arithmetic(BinaryOp::Add)
                .method_operands()
                .len(),
            9
        );
        // `<integer> / <integer>` has no method.
        assert_eq!(
            NumericOperator::Arithmetic(BinaryOp::Div)
                .method_operands()
                .len(),
            8
        );
        assert_eq!(
            NumericOperator::Compare(CompareOp::Lt)
                .method_operands()
                .len(),
            9
        );
        assert_eq!(NumericOperator::Negate.method_operands().len(), 3);
    }
}
