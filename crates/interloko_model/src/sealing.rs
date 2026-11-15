//! Sealing: which classes and generics may be extended from outside the
//! source that defines them.
//!
//! A class or generic is [`Sealing::Open`] unless its definition says
//! `sealed`. A sealed class's direct subclasses, and a sealed generic's
//! methods, are exactly those its defining source declares: nothing added
//! later, by a session mutation or by code installed into a running program,
//! may extend them. That lets the type lattice read a cone over a sealed
//! class as the finite union of its pieces
//! ([`ClassHierarchy::sealed_subclasses`](crate::types::ClassHierarchy::sealed_subclasses)),
//! and compiled code that relies on such a reading records it as a
//! [`ClosedWorld`] assumption, which an install under live frames must keep.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

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

/// A closed-world assumption compiled code relies on, written with source
/// names so it can be checked against a runtime layout that numbers classes
/// and methods differently.
///
/// The names are the ones the defining source uses: classes as `<circle>`,
/// generics by name, and methods as `area(<circle>)`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum ClosedWorld {
    /// A sealed class has exactly these direct subclasses, and, when it is
    /// abstract, no direct instances.
    SealedClass {
        /// The sealed class.
        class: String,
        /// Its direct subclasses, in definition order.
        subclasses: Vec<String>,
        /// Whether the class is abstract.
        is_abstract: bool,
    },
    /// A generic has exactly these methods.
    Methods {
        /// The generic.
        generic: String,
        /// Whether the generic is sealed.
        sealing: Sealing,
        /// Its methods, in definition order.
        methods: Vec<String>,
    },
}

/// Most members a [`ClosedWorld`] label lists before eliding the rest.
pub const MAX_LISTED: usize = 6;

impl fmt::Display for ClosedWorld {
    /// Renders the assumption as a sentence, such as `sealed abstract class
    /// <shape> has exactly direct subclasses {<circle>, <square>}` or
    /// `generic area has exactly methods {area(<circle>)}`. Past
    /// [`MAX_LISTED`] members the rest are counted, as in `{a, b, ...,
    /// +3 more}`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SealedClass {
                class,
                subclasses,
                is_abstract,
            } => {
                let kind = if *is_abstract {
                    "sealed abstract class"
                } else {
                    "sealed class"
                };
                if subclasses.is_empty() {
                    write!(formatter, "{kind} {class} has no subclasses")
                } else {
                    write!(formatter, "{kind} {class} has exactly direct subclasses ")?;
                    write_set(formatter, subclasses)
                }
            }
            Self::Methods {
                generic,
                sealing,
                methods,
            } => {
                if sealing.is_sealed() {
                    formatter.write_str("sealed ")?;
                }
                write!(formatter, "generic {generic} has exactly methods ")?;
                write_set(formatter, methods)
            }
        }
    }
}

/// Writes `{a, b}`, eliding members past [`MAX_LISTED`].
pub fn write_set(formatter: &mut dyn fmt::Write, members: &[String]) -> fmt::Result {
    formatter.write_str("{")?;
    for (index, member) in members.iter().take(MAX_LISTED).enumerate() {
        if index > 0 {
            formatter.write_str(", ")?;
        }
        formatter.write_str(member)?;
    }
    if members.len() > MAX_LISTED {
        write!(formatter, ", ..., +{} more", members.len() - MAX_LISTED)?;
    }
    formatter.write_str("}")
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::string::ToString;
    use alloc::vec;

    use super::*;

    #[test]
    fn assumptions_render_as_sentences() {
        let shapes = ClosedWorld::SealedClass {
            class: "<shape>".into(),
            subclasses: vec!["<circle>".into(), "<square>".into()],
            is_abstract: true,
        };
        assert_eq!(
            shapes.to_string(),
            "sealed abstract class <shape> has exactly direct subclasses {<circle>, <square>}"
        );
        let leaf = ClosedWorld::SealedClass {
            class: "<circle>".into(),
            subclasses: Vec::new(),
            is_abstract: false,
        };
        assert_eq!(leaf.to_string(), "sealed class <circle> has no subclasses");
        let area = ClosedWorld::Methods {
            generic: "area".into(),
            sealing: Sealing::Sealed,
            methods: (0..8).map(|index| alloc::format!("m{index}")).collect(),
        };
        assert_eq!(
            area.to_string(),
            "sealed generic area has exactly methods {m0, m1, m2, m3, m4, m5, ..., +2 more}"
        );
    }
}
