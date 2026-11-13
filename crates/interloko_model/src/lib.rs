//! Shared Interloko substrate model facts.
//!
//! This crate owns small, durable identifiers and value-class names that are
//! shared by compiler and runtime-shaped code. It intentionally does not own
//! syntax, semantic IR, lowering, tape emission, runtime host execution,
//! sessions, collaboration, permissions, or persistence.

#![no_std]

use core::fmt;

/// Durable class identifier within an Interloko model module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClassId(
    /// Zero-based class index.
    pub u32,
);

/// Stable class id for Interloko's `<object>` root class.
pub const OBJECT_CLASS_ID: ClassId = ClassId(0);

/// Durable slot identifier within a finalized class.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SlotId(
    /// Zero-based slot index within the class.
    pub u32,
);

/// Durable generic function identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct GenericFunctionId(
    /// Zero-based generic index.
    pub u32,
);

/// Durable method identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct MethodId(
    /// Zero-based method index.
    pub u32,
);

/// Built-in value class supplied by the Loko language kernel.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BuiltinValueClass {
    /// The built-in `<string>` class.
    String,
    /// The built-in `<symbol>` class.
    Symbol,
    /// The concrete built-in `<integer>` class of exact integers.
    Integer,
    /// The built-in `<vector>` class.
    Vector,
    /// The built-in `<class>` metaobject class.
    Class,
    /// The built-in `<generic-function>` metaobject class.
    GenericFunction,
    /// The built-in `<method>` metaobject class.
    Method,
    /// The built-in `<slot-descriptor>` metaobject class.
    SlotDescriptor,
    /// The abstract built-in `<number>` class, root of the numeric classes.
    Number,
    /// The abstract built-in `<real>` class of real numbers.
    Real,
    /// The abstract built-in `<float>` class of floating-point numbers.
    Float,
    /// The concrete built-in `<double-float>` class of IEEE 754 binary64
    /// values. Float literals are `<double-float>` instances.
    DoubleFloat,
    /// The abstract built-in `<rational>` class of exact real numbers.
    Rational,
    /// The concrete built-in `<single-float>` class of IEEE 754 binary32
    /// values, written with an `f32` suffix such as `1.5f32`.
    SingleFloat,
}

impl BuiltinValueClass {
    /// Built-in value classes in deterministic model declaration order.
    pub const ALL: &'static [(Self, &'static str)] = &[
        (Self::String, "<string>"),
        (Self::Symbol, "<symbol>"),
        (Self::Integer, "<integer>"),
        (Self::Vector, "<vector>"),
        (Self::Class, "<class>"),
        (Self::GenericFunction, "<generic-function>"),
        (Self::Method, "<method>"),
        (Self::SlotDescriptor, "<slot-descriptor>"),
        (Self::Number, "<number>"),
        (Self::Real, "<real>"),
        (Self::Float, "<float>"),
        (Self::DoubleFloat, "<double-float>"),
        (Self::Rational, "<rational>"),
        (Self::SingleFloat, "<single-float>"),
    ];

    /// Returns the Loko-visible class name for this built-in value class.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::String => "<string>",
            Self::Symbol => "<symbol>",
            Self::Integer => "<integer>",
            Self::Vector => "<vector>",
            Self::Class => "<class>",
            Self::GenericFunction => "<generic-function>",
            Self::Method => "<method>",
            Self::SlotDescriptor => "<slot-descriptor>",
            Self::Number => "<number>",
            Self::Real => "<real>",
            Self::Float => "<float>",
            Self::DoubleFloat => "<double-float>",
            Self::Rational => "<rational>",
            Self::SingleFloat => "<single-float>",
        }
    }

    /// Returns the built-in value class named by `name`.
    #[must_use]
    pub const fn from_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"<string>" => Some(Self::String),
            b"<symbol>" => Some(Self::Symbol),
            b"<integer>" => Some(Self::Integer),
            b"<vector>" => Some(Self::Vector),
            b"<class>" => Some(Self::Class),
            b"<generic-function>" => Some(Self::GenericFunction),
            b"<method>" => Some(Self::Method),
            b"<slot-descriptor>" => Some(Self::SlotDescriptor),
            b"<number>" => Some(Self::Number),
            b"<real>" => Some(Self::Real),
            b"<float>" => Some(Self::Float),
            b"<double-float>" => Some(Self::DoubleFloat),
            b"<rational>" => Some(Self::Rational),
            b"<single-float>" => Some(Self::SingleFloat),
            _ => None,
        }
    }

    /// Returns this built-in class's deterministic declaration index.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::String => 0,
            Self::Symbol => 1,
            Self::Integer => 2,
            Self::Vector => 3,
            Self::Class => 4,
            Self::GenericFunction => 5,
            Self::Method => 6,
            Self::SlotDescriptor => 7,
            Self::Number => 8,
            Self::Real => 9,
            Self::Float => 10,
            Self::DoubleFloat => 11,
            Self::Rational => 12,
            Self::SingleFloat => 13,
        }
    }

    /// Returns this class's built-in direct superclass, or `None` when its
    /// direct superclass is `<object>`.
    ///
    /// The numeric classes form a numeric tower: `<number>` > `<real>`,
    /// then `<real>` > `<rational>` > `<integer>` for exact numbers and
    /// `<real>` > `<float>` > `<double-float>` and `<single-float>` for
    /// floats. [`Self::ALL`] is in declaration order, not superclass order.
    #[must_use]
    pub const fn builtin_superclass(self) -> Option<Self> {
        match self {
            Self::Real => Some(Self::Number),
            Self::Rational | Self::Float => Some(Self::Real),
            Self::Integer => Some(Self::Rational),
            Self::DoubleFloat | Self::SingleFloat => Some(Self::Float),
            _ => None,
        }
    }

    /// Returns whether this class is abstract: it classifies values but has
    /// no direct instances of its own.
    #[must_use]
    pub const fn is_abstract(self) -> bool {
        matches!(
            self,
            Self::Number | Self::Real | Self::Rational | Self::Float
        )
    }

    /// Returns whether this class is `<number>` or one of its subclasses.
    ///
    /// Numeric values are unboxed scalars, so they are boxed rather than
    /// referenced when they travel as `<object>`.
    #[must_use]
    pub const fn is_numeric(self) -> bool {
        let mut class = self;
        loop {
            if matches!(class, Self::Number) {
                return true;
            }
            match class.builtin_superclass() {
                Some(superclass) => class = superclass,
                None => return false,
            }
        }
    }
}

/// A fixed-width integer type: `limited(<integer>, min: …, max: …)` with
/// the range of a machine integer.
///
/// Limited integer types are types, not classes. Their instances are
/// ordinary `<integer>`s whose value lies in the range, so `object-class`
/// of a `<u32>` value is `<integer>` and a boxed one is a boxed integer.
/// What the type adds is a representation: values statically known to be
/// in the range live in 32-bit registers and use the 32-bit instructions.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LimitedInteger {
    /// `<i32>`: `limited(<integer>, min: -2^31, max: 2^31 - 1)`.
    I32,
    /// `<u32>`: `limited(<integer>, min: 0, max: 2^32 - 1)`.
    U32,
}

impl LimitedInteger {
    /// Every limited integer type, in declaration order.
    pub const ALL: &'static [Self] = &[Self::I32, Self::U32];

    /// Returns the Loko-visible type name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::I32 => "<i32>",
            Self::U32 => "<u32>",
        }
    }

    /// Returns the literal suffix that writes an integer of this type, such
    /// as `7u32`.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::U32 => "u32",
        }
    }

    /// Returns the limited type named by `name`.
    #[must_use]
    pub const fn from_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"<i32>" => Some(Self::I32),
            b"<u32>" => Some(Self::U32),
            _ => None,
        }
    }

    /// Returns the smallest member of the range.
    #[must_use]
    pub const fn min(self) -> i64 {
        match self {
            Self::I32 => i32::MIN as i64,
            Self::U32 => 0,
        }
    }

    /// Returns the largest member of the range.
    #[must_use]
    pub const fn max(self) -> i64 {
        match self {
            Self::I32 => i32::MAX as i64,
            Self::U32 => u32::MAX as i64,
        }
    }

    /// Returns whether `value` is a member of the range.
    #[must_use]
    pub const fn contains(self, value: i64) -> bool {
        self.min() <= value && value <= self.max()
    }
}

/// An `f32` value that compares and hashes by its IEEE 754 bit pattern.
///
/// The binary32 counterpart of [`FloatBits`], for `<single-float>`
/// constants.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SingleFloatBits(u32);

impl SingleFloatBits {
    /// Captures `value`'s exact bit pattern.
    #[must_use]
    pub const fn from_f32(value: f32) -> Self {
        Self(value.to_bits())
    }

    /// Wraps a raw IEEE 754 binary32 bit pattern.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns the float value with the captured bit pattern.
    #[must_use]
    pub const fn to_f32(self) -> f32 {
        f32::from_bits(self.0)
    }

    /// Returns the raw IEEE 754 binary32 bit pattern.
    #[must_use]
    pub const fn to_bits(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SingleFloatBits {
    /// Renders the value as the shortest text that reads back to the same
    /// `f32`, followed by the `f32` suffix (`1.5f32`, `0.1f32`), or as
    /// `inf`, `-inf`, and `nan` (with the suffix) for non-finite values.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.to_f32();
        if value.is_nan() {
            formatter.write_str("nan")?;
        } else if value.is_infinite() {
            formatter.write_str(if value < 0.0 { "-inf" } else { "inf" })?;
        } else {
            write!(formatter, "{value:?}")?;
        }
        formatter.write_str("f32")
    }
}

/// A 32-bit numeric constant: an `<i32>` or `<u32>` integer, or a
/// `<single-float>`.
///
/// Literals with an `i32`, `u32` or `f32` suffix produce these, and an
/// unsuffixed literal becomes one when its context is a 32-bit type and the
/// value fits.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Number32 {
    /// An `<i32>` integer.
    I32(i32),
    /// A `<u32>` integer.
    U32(u32),
    /// A `<single-float>`, compared by bit pattern.
    F32(SingleFloatBits),
}

impl fmt::Display for Number32 {
    /// Renders the constant as suffixed Loko literal text: `-7i32`, `7u32`,
    /// `1.5f32`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I32(value) => write!(formatter, "{value}i32"),
            Self::U32(value) => write!(formatter, "{value}u32"),
            Self::F32(bits) => write!(formatter, "{bits}"),
        }
    }
}

/// An `f64` value that compares and hashes by its IEEE 754 bit pattern.
///
/// Compiler artifacts derive `Eq` and `Hash` so they can be cached,
/// fingerprinted, and diffed. Float constants travel through those artifacts
/// as `FloatBits`: two constants are the same artifact fact exactly when they
/// have the same bits, so `0.0` and `-0.0` stay distinct and a NaN constant
/// equals itself. Numeric comparison is a runtime operation, not this type's
/// job.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FloatBits(u64);

impl FloatBits {
    /// Captures `value`'s exact bit pattern.
    #[must_use]
    pub const fn from_f64(value: f64) -> Self {
        Self(value.to_bits())
    }

    /// Wraps a raw IEEE 754 binary64 bit pattern.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the float value with the captured bit pattern.
    #[must_use]
    pub const fn to_f64(self) -> f64 {
        f64::from_bits(self.0)
    }

    /// Returns the raw IEEE 754 binary64 bit pattern.
    #[must_use]
    pub const fn to_bits(self) -> u64 {
        self.0
    }
}

impl From<f64> for FloatBits {
    fn from(value: f64) -> Self {
        Self::from_f64(value)
    }
}

impl fmt::Display for FloatBits {
    /// Renders the value as the shortest text that reads back to the same
    /// float.
    ///
    /// Finite values always carry a fraction or an exponent (`1.0`, `0.1`,
    /// `1e300`, `-0.0`), so the text is a Loko float literal and is never
    /// confused with an integer. Non-finite values, which have no literal
    /// syntax, render as `inf`, `-inf`, and `nan`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.to_f64();
        if value.is_nan() {
            formatter.write_str("nan")
        } else if value.is_infinite() {
            formatter.write_str(if value < 0.0 { "-inf" } else { "inf" })
        } else {
            write!(formatter, "{value:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::ToString;

    use super::{BuiltinValueClass, FloatBits, LimitedInteger, SingleFloatBits};

    #[test]
    fn numeric_tower_has_exact_and_float_branches() {
        let chain = |mut class: BuiltinValueClass| {
            let mut names = std::vec![class.name()];
            while let Some(superclass) = class.builtin_superclass() {
                names.push(superclass.name());
                class = superclass;
            }
            names
        };
        assert_eq!(
            chain(BuiltinValueClass::Integer),
            ["<integer>", "<rational>", "<real>", "<number>"]
        );
        assert_eq!(
            chain(BuiltinValueClass::DoubleFloat),
            ["<double-float>", "<float>", "<real>", "<number>"]
        );
        assert_eq!(
            chain(BuiltinValueClass::SingleFloat),
            ["<single-float>", "<float>", "<real>", "<number>"]
        );
        assert!(!BuiltinValueClass::SingleFloat.is_abstract());
        for (index, (class, name)) in BuiltinValueClass::ALL.iter().enumerate() {
            assert_eq!(class.index(), index);
            assert_eq!(class.name(), *name);
            assert_eq!(BuiltinValueClass::from_name(name), Some(*class));
            assert_eq!(
                class.is_numeric(),
                chain(*class).contains(&"<number>"),
                "{name}"
            );
        }
        assert!(BuiltinValueClass::Rational.is_abstract());
        assert!(!BuiltinValueClass::Integer.is_abstract());
        assert!(!BuiltinValueClass::String.is_numeric());
    }

    #[test]
    fn float_bits_render_as_round_trip_literals() {
        let cases: &[(f64, &str)] = &[
            (1.0, "1.0"),
            (0.1, "0.1"),
            (-0.0, "-0.0"),
            (2.5e-7, "2.5e-7"),
            (1e16, "1e16"),
            (f64::MAX, "1.7976931348623157e308"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
        ];
        for (value, text) in cases {
            assert_eq!(FloatBits::from_f64(*value).to_string(), *text);
        }
        for value in [0.1, 1.0 / 3.0, 6.02214076e23, 5e-324] {
            let text = FloatBits::from_f64(value).to_string();
            assert_eq!(text.parse::<f64>().map(f64::to_bits), Ok(value.to_bits()));
        }
    }

    #[test]
    fn limited_integers_are_ranges_of_integer() {
        for limited in LimitedInteger::ALL {
            assert_eq!(LimitedInteger::from_name(limited.name()), Some(*limited));
            assert!(limited.contains(limited.min()) && limited.contains(limited.max()));
            assert!(!limited.contains(limited.min() - 1) && !limited.contains(limited.max() + 1));
        }
        assert_eq!(LimitedInteger::I32.min(), i64::from(i32::MIN));
        assert_eq!(LimitedInteger::U32.max(), i64::from(u32::MAX));
        assert_eq!(LimitedInteger::from_name("<integer>"), None);
    }

    #[test]
    fn single_float_bits_render_as_suffixed_literals() {
        let cases: &[(f32, &str)] = &[
            (1.5, "1.5f32"),
            (0.1, "0.1f32"),
            (-0.0, "-0.0f32"),
            (f32::MAX, "3.4028235e38f32"),
            (f32::INFINITY, "inff32"),
            (f32::NAN, "nanf32"),
        ];
        for (value, text) in cases {
            assert_eq!(SingleFloatBits::from_f32(*value).to_string(), *text);
        }
    }

    #[test]
    fn float_bits_compare_by_bit_pattern() {
        assert_ne!(FloatBits::from_f64(0.0), FloatBits::from_f64(-0.0));
        assert_eq!(FloatBits::from_f64(f64::NAN), FloatBits::from_f64(f64::NAN));
    }
}
