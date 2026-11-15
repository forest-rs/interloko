//! Sealing: which classes and generics may be extended from outside the
//! source that defines them.
//!
//! A class or generic is [`Sealing::Open`] unless its definition says
//! `sealed`. A sealed class's direct subclasses, and a sealed generic's
//! methods, are exactly those its defining source declares: nothing added
//! later, by a session mutation or by code installed into a running program,
//! may extend them. That lets the type lattice read a cone over a sealed
//! class as the finite union of its pieces
//! ([`ClassHierarchy::sealed_subclasses`](crate::types::ClassHierarchy::sealed_subclasses)).

/// Whether a class or generic may be extended from outside its defining
/// source.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Sealing {
    /// It may gain subclasses or methods later: the default.
    #[default]
    Open,
    /// Its direct subclasses or methods are exactly those its source
    /// declares.
    Sealed,
}

impl Sealing {
    /// Returns the stable label used in dumps and EOP records: `open` or
    /// `sealed`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Sealed => "sealed",
        }
    }

    /// Returns whether this is [`Self::Sealed`].
    #[must_use]
    pub const fn is_sealed(self) -> bool {
        matches!(self, Self::Sealed)
    }
}
