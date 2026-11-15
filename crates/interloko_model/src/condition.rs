//! The condition classes the language kernel defines, and the stable codes
//! of the runtime type errors it signals.
//!
//! Conditions are ordinary objects: any instance can be signalled, and a
//! handler names the class it accepts. The kernel adds a small hierarchy so
//! the runtime has classes of its own to signal and programs have classes
//! to handle broadly or extend:
//!
//! ```text
//! <object>
//!   <condition>
//!     <error>
//!       <type-error>
//! ```
//!
//! A `<type-error>` reports a runtime type check that failed: the value it
//! rejected, the type it expected, where the check is, and a stable
//! [`TypeErrorKind`] code. Its slots are read-only, so it adds no setter
//! generics to a program.

/// A condition class the language kernel defines in every module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ConditionClass {
    /// `<condition>`: the root of the kernel's condition classes.
    Condition,
    /// `<error>`: a condition a program cannot continue past unless a
    /// handler or restart deals with it.
    Error,
    /// `<type-error>`: a runtime type check failed.
    TypeError,
}

impl ConditionClass {
    /// Every kernel condition class, superclasses first.
    pub const ALL: &'static [Self] = &[Self::Condition, Self::Error, Self::TypeError];

    /// Returns the Loko-visible class name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Condition => "<condition>",
            Self::Error => "<error>",
            Self::TypeError => "<type-error>",
        }
    }

    /// Returns the kernel class named by `name`.
    #[must_use]
    pub const fn from_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"<condition>" => Some(Self::Condition),
            b"<error>" => Some(Self::Error),
            b"<type-error>" => Some(Self::TypeError),
            _ => None,
        }
    }

    /// Returns this class's direct superclass, or `None` when it is
    /// `<object>`.
    #[must_use]
    pub const fn superclass(self) -> Option<Self> {
        match self {
            Self::Condition => None,
            Self::Error => Some(Self::Condition),
            Self::TypeError => Some(Self::Error),
        }
    }

    /// Returns the slots this class declares directly, in layout order.
    #[must_use]
    pub const fn slots(self) -> &'static [ConditionSlot] {
        match self {
            Self::Condition | Self::Error => &[],
            Self::TypeError => &TYPE_ERROR_SLOTS,
        }
    }
}

/// A read-only slot of a kernel condition class.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConditionSlot {
    /// Slot and getter accessor name, such as `type-error-value`.
    pub name: &'static str,
    /// Declared type name, such as `<string>`.
    pub type_name: &'static str,
    /// Required init keyword without its colon, such as `value`.
    pub init_keyword: &'static str,
}

/// The slots of `<type-error>`, in layout order: [`TypeErrorSlot`] indexes
/// them.
pub const TYPE_ERROR_SLOTS: [ConditionSlot; 4] = [
    ConditionSlot {
        name: "type-error-value",
        type_name: "<object>",
        init_keyword: "value",
    },
    ConditionSlot {
        name: "type-error-expected-type",
        type_name: "<string>",
        init_keyword: "expected-type",
    },
    ConditionSlot {
        name: "type-error-site",
        type_name: "<string>",
        init_keyword: "site",
    },
    ConditionSlot {
        name: "type-error-code",
        type_name: "<string>",
        init_keyword: "code",
    },
];

/// One slot of `<type-error>`, by its position in [`TYPE_ERROR_SLOTS`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TypeErrorSlot {
    /// The value the check rejected, as an `<object>`.
    Value,
    /// The printed type the check expected, such as `<circle>` or `<i32>`.
    ExpectedType,
    /// Where the check is: the typing decision id `typing:<def>:s<n>` of a
    /// check typing classified, or a description of a check it did not.
    Site,
    /// The stable [`TypeErrorKind::code`].
    Code,
}

impl TypeErrorSlot {
    /// Returns the slot's index in [`TYPE_ERROR_SLOTS`] and in the layout of
    /// `<type-error>`, whose superclasses declare no slots.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Value => 0,
            Self::ExpectedType => 1,
            Self::Site => 2,
            Self::Code => 3,
        }
    }
}

/// What kind of runtime check a `<type-error>` reports.
///
/// Each kind has a stable code in the diagnostic code scheme
/// (`area.name`), with `type-error` as its area. Tools, corpus
/// expectations and handlers match on the code; the site says which
/// check of that kind failed.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TypeErrorKind {
    /// A value is not an instance of a class: `as(<circle>, x)`, or a
    /// boxed `<object>` read as a number. Code `type-error.cast`.
    Cast,
    /// A callable's argument is not of its parameter's class, checked where
    /// the callable is entered through a value whose signature the caller
    /// could not see; or an exit procedure's value is not of its block's
    /// class. Code `type-error.entry`.
    Entry,
    /// A number is outside a limited type's range: converting to `<i32>`,
    /// a float to an integer it does not equal, a shift count outside
    /// `0..=31`, or an argument, binding, slot value, constant or result
    /// outside its `limited(<integer>, ...)` type. Code `type-error.range`.
    Range,
    /// Integer arithmetic produced a result its kind cannot represent.
    /// Code `type-error.overflow`.
    Overflow,
}

impl TypeErrorKind {
    /// Every kind, in code order.
    pub const ALL: &'static [Self] = &[Self::Cast, Self::Entry, Self::Range, Self::Overflow];

    /// Returns the stable code, such as `type-error.cast`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Cast => "type-error.cast",
            Self::Entry => "type-error.entry",
            Self::Range => "type-error.range",
            Self::Overflow => "type-error.overflow",
        }
    }

    /// Returns the kind whose [`Self::code`] is `code`.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.code() == code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_condition_classes_round_trip_and_chain_to_object() {
        for class in ConditionClass::ALL {
            assert_eq!(ConditionClass::from_name(class.name()), Some(*class));
        }
        assert_eq!(
            ConditionClass::TypeError.superclass(),
            Some(ConditionClass::Error)
        );
        assert_eq!(ConditionClass::Condition.superclass(), None);
        assert_eq!(ConditionClass::from_name("<object>"), None);
    }

    #[test]
    fn type_error_slots_follow_their_indices() {
        let slots = ConditionClass::TypeError.slots();
        assert_eq!(slots[TypeErrorSlot::Value.index()].name, "type-error-value");
        assert_eq!(
            slots[TypeErrorSlot::ExpectedType.index()].name,
            "type-error-expected-type"
        );
        assert_eq!(slots[TypeErrorSlot::Site.index()].name, "type-error-site");
        assert_eq!(slots[TypeErrorSlot::Code.index()].name, "type-error-code");
    }

    #[test]
    fn type_error_codes_round_trip_and_are_distinct() {
        for (index, kind) in TypeErrorKind::ALL.iter().copied().enumerate() {
            assert_eq!(TypeErrorKind::from_code(kind.code()), Some(kind));
            assert!(
                TypeErrorKind::ALL[..index]
                    .iter()
                    .all(|earlier| earlier.code() != kind.code())
            );
        }
        assert_eq!(TypeErrorKind::from_code("runtime.trap"), None);
    }
}
