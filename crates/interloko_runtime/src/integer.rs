//! Integer division served by the runtime host: the quotient and remainder
//! of Dylan's `truncate/`, `floor/`, `ceiling/` and `round/`.
//!
//! Each is a host call named [`IntegerDivision::symbol`] taking two `i64`s
//! and returning one. Compiled code calls them only with a divisor that is
//! neither zero, which signals a `<division-by-zero-error>`, nor `-1`, whose
//! quotient is the dividend negated; so no call here can overflow or divide
//! by zero, and one that would fails rather than wraps.

use alloc::vec;

use execution_tape::host::HostSig;
use execution_tape::program::ValueType;

/// How a division rounds its quotient.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Rounding {
    /// Toward zero: `truncate/`.
    Truncate,
    /// Toward negative infinity: `floor/`.
    Floor,
    /// Toward positive infinity: `ceiling/`.
    Ceiling,
    /// To the nearest integer, a tie to the even one: `round/`.
    Round,
}

impl Rounding {
    /// Every rounding, in declaration order.
    pub const ALL: &'static [Self] = &[Self::Truncate, Self::Floor, Self::Ceiling, Self::Round];

    /// Returns the Loko name of the division, such as `floor/`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Truncate => "truncate/",
            Self::Floor => "floor/",
            Self::Ceiling => "ceiling/",
            Self::Round => "round/",
        }
    }

    /// Returns the rounding of the division named `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|rounding| rounding.name() == name)
    }

    /// Divides `dividend` by `divisor`, returning the quotient and the
    /// remainder `dividend - quotient * divisor`, or `None` for a zero
    /// divisor or a quotient outside `i64`.
    #[must_use]
    pub fn divide(self, dividend: i64, divisor: i64) -> Option<(i64, i64)> {
        if divisor == 0 {
            return None;
        }
        let (a, b) = (i128::from(dividend), i128::from(divisor));
        let (mut q, mut r) = (a / b, a % b);
        let adjust = match self {
            Self::Truncate => 0,
            Self::Floor => -i128::from(r != 0 && (r < 0) != (b < 0)),
            Self::Ceiling => i128::from(r != 0 && (r < 0) == (b < 0)),
            Self::Round => {
                // From the floor quotient, whose remainder has the
                // divisor's sign, step up when the remainder is more than
                // half the divisor, or exactly half and the quotient odd.
                if r != 0 && (r < 0) != (b < 0) {
                    q -= 1;
                    r += b;
                }
                let twice = (2 * r).abs();
                i128::from(twice > b.abs() || (twice == b.abs() && q % 2 != 0))
            }
        };
        q += adjust;
        r -= adjust * b;
        Some((i64::try_from(q).ok()?, i64::try_from(r).ok()?))
    }
}

/// One result of an integer division.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct IntegerDivision {
    /// How the quotient rounds.
    pub rounding: Rounding,
    /// Whether this is the remainder rather than the quotient.
    pub remainder: bool,
}

impl IntegerDivision {
    /// Every integer division result.
    pub const ALL: &'static [Self] = &[
        Self::quotient(Rounding::Truncate),
        Self::remainder(Rounding::Truncate),
        Self::quotient(Rounding::Floor),
        Self::remainder(Rounding::Floor),
        Self::quotient(Rounding::Ceiling),
        Self::remainder(Rounding::Ceiling),
        Self::quotient(Rounding::Round),
        Self::remainder(Rounding::Round),
    ];

    /// The quotient of a division rounding by `rounding`.
    #[must_use]
    pub const fn quotient(rounding: Rounding) -> Self {
        Self {
            rounding,
            remainder: false,
        }
    }

    /// The remainder of a division rounding by `rounding`.
    #[must_use]
    pub const fn remainder(rounding: Rounding) -> Self {
        Self {
            rounding,
            remainder: true,
        }
    }

    /// Returns the host-call symbol that serves this result.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match (self.rounding, self.remainder) {
            (Rounding::Truncate, false) => "interloko.integer.truncate_quotient",
            (Rounding::Truncate, true) => "interloko.integer.truncate_remainder",
            (Rounding::Floor, false) => "interloko.integer.floor_quotient",
            (Rounding::Floor, true) => "interloko.integer.floor_remainder",
            (Rounding::Ceiling, false) => "interloko.integer.ceiling_quotient",
            (Rounding::Ceiling, true) => "interloko.integer.ceiling_remainder",
            (Rounding::Round, false) => "interloko.integer.round_quotient",
            (Rounding::Round, true) => "interloko.integer.round_remainder",
        }
    }

    /// Returns the result served by host-call `symbol`, if any.
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        if !symbol.starts_with("interloko.integer.") {
            return None;
        }
        Self::ALL
            .iter()
            .copied()
            .find(|function| function.symbol() == symbol)
    }

    /// Returns the host-call signature: two `i64`s in, one out.
    #[must_use]
    pub fn signature(self) -> HostSig {
        HostSig {
            args: vec![ValueType::I64, ValueType::I64],
            rets: vec![ValueType::I64],
        }
    }

    /// Computes this result, or `None` when the division has none.
    #[must_use]
    pub fn apply(self, dividend: i64, divisor: i64) -> Option<i64> {
        let (quotient, remainder) = self.rounding.divide(dividend, divisor)?;
        Some(if self.remainder { remainder } else { quotient })
    }
}

#[cfg(test)]
mod tests {
    use super::{IntegerDivision, Rounding};

    #[test]
    fn each_rounding_divides_as_dylan_does() {
        let cases = [
            (
                Rounding::Truncate,
                [
                    (7, 2, 3, 1),
                    (-7, 2, -3, -1),
                    (7, -2, -3, 1),
                    (-7, -2, 3, -1),
                ],
            ),
            (
                Rounding::Floor,
                [
                    (7, 2, 3, 1),
                    (-7, 2, -4, 1),
                    (7, -2, -4, -1),
                    (-7, -2, 3, -1),
                ],
            ),
            (
                Rounding::Ceiling,
                [
                    (7, 2, 4, -1),
                    (-7, 2, -3, -1),
                    (7, -2, -3, 1),
                    (-7, -2, 4, 1),
                ],
            ),
            // Ties go to the even quotient: 7/2 = 3.5 rounds to 4, 5/2 to 2.
            (
                Rounding::Round,
                [(7, 2, 4, -1), (5, 2, 2, 1), (-5, 2, -2, -1), (8, 3, 3, -1)],
            ),
        ];
        for (rounding, rows) in cases {
            for (a, b, q, r) in rows {
                assert_eq!(rounding.divide(a, b), Some((q, r)), "{rounding:?} {a} {b}");
                assert_eq!(q * b + r, a);
            }
        }
    }

    #[test]
    fn a_zero_divisor_or_an_overflowing_quotient_has_no_result() {
        for rounding in Rounding::ALL {
            assert_eq!(rounding.divide(1, 0), None);
            assert_eq!(rounding.divide(i64::MIN, -1), None);
            assert_eq!(rounding.divide(i64::MIN, 1), Some((i64::MIN, 0)));
            assert_eq!(Rounding::from_name(rounding.name()), Some(*rounding));
        }
        for function in IntegerDivision::ALL {
            assert_eq!(
                IntegerDivision::from_symbol(function.symbol()),
                Some(*function)
            );
        }
        assert_eq!(Rounding::Round.divide(i64::MAX, i64::MIN), Some((-1, -1)));
    }
}
