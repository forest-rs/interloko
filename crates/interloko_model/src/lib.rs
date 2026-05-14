//! Shared Interloko substrate model facts.
//!
//! This crate owns small, durable identifiers and value-class names that are
//! shared by compiler and runtime-shaped code. It intentionally does not own
//! syntax, semantic IR, lowering, tape emission, runtime host execution,
//! sessions, collaboration, permissions, or persistence.

#![no_std]

/// Durable class identifier within an Interloko model module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClassId(
    /// Zero-based class index.
    pub u32,
);

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
        }
    }
}
