//! The collection generics: Dylan's `size(collection)`,
//! `element(collection, key, #key default)` and
//! `element-setter(value, collection, key)`, which `collection[key]` and
//! `collection[key] := value` write.
//!
//! Each is an open built-in generic, as the numeric operators are built-in
//! generics: a module that uses the name gets the generic, with the kernel's
//! methods from [`BUILTIN_METHODS`], and a program adds its own with
//! `define method`. A send whose receiver's class typing knows runs the
//! kernel method inline, so typed code pays no dispatch; any other send
//! dispatches, and one that finds no method signals
//! `<no-applicable-method>`.
//!
//! A kernel collection class takes part by listing its methods in
//! [`BUILTIN_METHODS`]: one row per generic, naming the class, the key
//! class where the generic takes a key, and the intrinsic that implements
//! it. Over each class listed there the generic is sealed: a program's
//! method for that class or a subclass would override the kernel's, so it
//! is refused, which is what lets typed code run the kernel's inline.
//!
//! A program's own function, host function or generic of one of these
//! names replaces the built-in generic entirely, as any definition shadows a
//! built-in, and a slot getter of the name keeps serving its own class.

use super::{BuiltinValueClass, Intrinsic, TextFunction};

/// A built-in collection generic.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CollectionFunction {
    /// `size(collection) => <integer>`: the number of elements.
    Size,
    /// `element(collection, key, #key default) => <object>`: the element at
    /// `key`, or `default` when there is none.
    Element,
    /// `element-setter(value, collection, key) => <object>`: stores `value`
    /// at `key` and returns it.
    ElementSetter,
}

impl CollectionFunction {
    /// Every collection generic.
    pub const ALL: &'static [Self] = &[Self::Size, Self::Element, Self::ElementSetter];

    /// Returns the generic named `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|function| function.name() == name)
    }

    /// Returns the Loko name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Element => "element",
            Self::ElementSetter => "element-setter",
        }
    }

    /// Returns the required parameters' names, in order.
    #[must_use]
    pub const fn params(self) -> &'static [&'static str] {
        match self {
            Self::Size => &["collection"],
            Self::Element => &["collection", "key"],
            Self::ElementSetter => &["value", "collection", "key"],
        }
    }

    /// Returns the number of required arguments.
    #[must_use]
    pub const fn arity(self) -> usize {
        self.params().len()
    }

    /// Returns the position of the collection argument.
    #[must_use]
    pub const fn collection_index(self) -> usize {
        match self {
            Self::Size | Self::Element => 0,
            Self::ElementSetter => 1,
        }
    }

    /// Returns the keyword arguments a call may pass, without colons.
    ///
    /// Loko has no keyword parameters yet, so a method cannot take one: a
    /// call passing `default:` runs a kernel method's defaulting operation
    /// ([`BuiltinMethod::with_default`]), and needs a receiver whose class
    /// typing knows has one.
    #[must_use]
    pub const fn keywords(self) -> &'static [&'static str] {
        match self {
            Self::Element => &["default"],
            Self::Size | Self::ElementSetter => &[],
        }
    }
}

/// A kernel method of a collection generic.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BuiltinMethod {
    /// The generic the method belongs to.
    pub function: CollectionFunction,
    /// The collection class the method specializes on.
    pub collection: BuiltinValueClass,
    /// The key class it specializes on, for a generic that takes a key.
    pub key: Option<BuiltinValueClass>,
    /// The operation, taking the generic's arguments in order.
    pub operation: Intrinsic,
    /// The operation that serves a call passing `default:`, taking the
    /// arguments and then the default, for a generic that takes one.
    pub with_default: Option<Intrinsic>,
}

/// The kernel's methods of the collection generics, one row per generic and
/// collection class. A kernel collection class takes part by adding its
/// rows here.
pub const BUILTIN_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        function: CollectionFunction::Size,
        collection: BuiltinValueClass::Vector,
        key: None,
        operation: Intrinsic::VectorSize,
        with_default: None,
    },
    BuiltinMethod {
        function: CollectionFunction::Element,
        collection: BuiltinValueClass::Vector,
        key: Some(BuiltinValueClass::Integer),
        operation: Intrinsic::VectorRef,
        with_default: Some(Intrinsic::VectorElementOr),
    },
    // A string's size is its cached scalar count, and its keys are
    // positions: no integer indexes a string.
    BuiltinMethod {
        function: CollectionFunction::Size,
        collection: BuiltinValueClass::String,
        key: None,
        operation: Intrinsic::Text(TextFunction::StringSize),
        with_default: None,
    },
    BuiltinMethod {
        function: CollectionFunction::Element,
        collection: BuiltinValueClass::String,
        key: Some(BuiltinValueClass::StringPosition),
        operation: Intrinsic::Text(TextFunction::StringElement),
        with_default: Some(Intrinsic::Text(TextFunction::StringElementOr)),
    },
    BuiltinMethod {
        function: CollectionFunction::Size,
        collection: BuiltinValueClass::StringBuilder,
        key: None,
        operation: Intrinsic::Text(TextFunction::BuilderSize),
        with_default: None,
    },
];

/// Returns the kernel methods of `function`.
pub fn builtin_methods(
    function: CollectionFunction,
) -> impl Iterator<Item = &'static BuiltinMethod> {
    BUILTIN_METHODS
        .iter()
        .filter(move |method| method.function == function)
}

#[cfg(test)]
mod tests {
    use super::{BUILTIN_METHODS, CollectionFunction, builtin_methods};

    #[test]
    fn functions_round_trip_by_name() {
        for function in CollectionFunction::ALL {
            assert_eq!(
                CollectionFunction::from_name(function.name()),
                Some(*function)
            );
            assert!(function.collection_index() < function.arity());
        }
    }

    #[test]
    fn builtin_methods_are_unique_and_shaped_like_their_generic() {
        for (index, method) in BUILTIN_METHODS.iter().enumerate() {
            assert!(
                BUILTIN_METHODS[..index].iter().all(|earlier| (
                    earlier.function,
                    earlier.collection
                ) != (
                    method.function,
                    method.collection
                )),
                "{} has two methods for one class",
                method.function.name()
            );
            let takes_key = method.function != CollectionFunction::Size;
            assert_eq!(method.key.is_some(), takes_key);
            assert!(
                method.with_default.is_none() || !method.function.keywords().is_empty(),
                "{} takes no default",
                method.function.name()
            );
        }
        assert_eq!(
            builtin_methods(CollectionFunction::ElementSetter).count(),
            0
        );
    }
}
