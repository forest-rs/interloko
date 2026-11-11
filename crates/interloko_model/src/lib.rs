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
    /// The built-in `<integer>` class.
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
        }
    }

    /// Returns this class's built-in direct superclass, or `None` when its
    /// direct superclass is `<object>`.
    ///
    /// The numeric classes form the start of a numeric tower:
    /// `<number>` > `<real>` > `<float>` > `<double-float>`. `<integer>`
    /// still sits directly under `<object>`.
    #[must_use]
    pub const fn builtin_superclass(self) -> Option<Self> {
        match self {
            Self::Real => Some(Self::Number),
            Self::Float => Some(Self::Real),
            Self::DoubleFloat => Some(Self::Float),
            _ => None,
        }
    }

    /// Returns whether this class is abstract: it classifies values but has
    /// no direct instances of its own.
    #[must_use]
    pub const fn is_abstract(self) -> bool {
        matches!(self, Self::Number | Self::Real | Self::Float)
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

    use super::FloatBits;

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
    fn float_bits_compare_by_bit_pattern() {
        assert_ne!(FloatBits::from_f64(0.0), FloatBits::from_f64(-0.0));
        assert_eq!(FloatBits::from_f64(f64::NAN), FloatBits::from_f64(f64::NAN));
    }
}
