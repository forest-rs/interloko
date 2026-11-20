//! Loko's built-in text functions: the `<character>` predicates, case
//! mapping and digit values, `<string>` positions, and the
//! `<string-builder>`.
//!
//! Each is a plain built-in function, as the float functions are: a call
//! resolves to it only when no definition of the program has the name, and
//! the runtime host serves it ([`TextFunction`]). A name may have several
//! overloads, such as `as-uppercase` for a character and for a string; a
//! call takes the first whose parameters its arguments match. This module
//! names them and maps their runtime kinds to value kinds; the runtime owns
//! what they do.
//!
//! `size` and `element` on strings, and `add!` on a string builder, are
//! kernel methods of the collection generics ([`super::BUILTIN_METHODS`]); `copy-sequence` and `starts-with?`
//! take keyword arguments, which sem lowers on its own.

use super::{ClassId, TextFunction, ValueKind};
use interloko_runtime::TextKind;

/// The text functions a call can name, with their Loko names and their
/// overloads in the order a call tries them.
///
/// `as(<character>, i)`, `as(<string>, x)` and the operations behind
/// `size`, `element`, `copy-sequence` and `starts-with?` are reached
/// otherwise.
pub const NAMED_TEXT_FUNCTIONS: &[(&str, &[TextFunction])] = &[
    ("digit-value", &[TextFunction::DigitValue]),
    ("digit?", &[TextFunction::IsDigit]),
    ("alphabetic?", &[TextFunction::IsAlphabetic]),
    ("alphanumeric?", &[TextFunction::IsAlphanumeric]),
    ("whitespace?", &[TextFunction::IsWhitespace]),
    ("uppercase?", &[TextFunction::IsUppercase]),
    ("lowercase?", &[TextFunction::IsLowercase]),
    ("ascii?", &[TextFunction::IsAscii]),
    ("ascii-alphabetic?", &[TextFunction::IsAsciiAlphabetic]),
    ("ascii-alphanumeric?", &[TextFunction::IsAsciiAlphanumeric]),
    ("ascii-whitespace?", &[TextFunction::IsAsciiWhitespace]),
    ("ascii-uppercase?", &[TextFunction::IsAsciiUppercase]),
    ("ascii-lowercase?", &[TextFunction::IsAsciiLowercase]),
    (
        "as-uppercase",
        &[
            TextFunction::CharacterUppercase,
            TextFunction::StringUppercase,
        ],
    ),
    (
        "as-lowercase",
        &[
            TextFunction::CharacterLowercase,
            TextFunction::StringLowercase,
        ],
    ),
    ("start-position", &[TextFunction::StartPosition]),
    ("end-position", &[TextFunction::EndPosition]),
    ("next-position", &[TextFunction::NextPosition]),
    ("previous-position", &[TextFunction::PreviousPosition]),
    ("position-at", &[TextFunction::PositionAt]),
    ("position-after", &[TextFunction::PositionAfter]),
];

/// Returns the overloads a call of `name` can reach.
#[must_use]
pub fn text_functions_for_name(name: &str) -> Option<&'static [TextFunction]> {
    NAMED_TEXT_FUNCTIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, functions)| *functions)
}

/// Returns the Loko spelling of `function`, as diagnostics and dumps write
/// it.
#[must_use]
pub fn text_function_name(function: TextFunction) -> &'static str {
    match function {
        TextFunction::CharacterFromInteger => "as(<character>, ...)",
        TextFunction::CheckCharacter => "check-character",
        TextFunction::CheckPositionValue => "check-position",
        TextFunction::CharacterToString => "as(<string>, <character>)",
        TextFunction::StringSize => "size(<string>)",
        TextFunction::StringElement => "element(<string>, ...)",
        TextFunction::StringElementOr => "element(<string>, ..., default:)",
        TextFunction::PositionFromOffset => "as(<string-position>, ...)",
        TextFunction::CheckPosition => "copy-sequence(start:)",
        TextFunction::CopyRange => "copy-sequence",
        TextFunction::StartsWith => "starts-with?",
        TextFunction::StringOrder => "<(<string>, <string>)",
        TextFunction::MakeBuilder => "make(<string-builder>)",
        TextFunction::BuilderSize => "size(<string-builder>)",
        TextFunction::BuilderToString => "as(<string>, <string-builder>)",
        TextFunction::BuilderAddCharacter => "add!(<string-builder>, <character>)",
        TextFunction::BuilderAddString => "add!(<string-builder>, <string>)",
        TextFunction::StringToSymbol => "as(<symbol>, <string>)",
        TextFunction::SymbolToString => "as(<string>, <symbol>)",
        function => NAMED_TEXT_FUNCTIONS
            .iter()
            .find(|(_, functions)| functions.contains(&function))
            .map_or("text function", |(name, _)| *name),
    }
}

/// Returns the class name values of `kind` have, for diagnostics.
#[must_use]
pub const fn text_kind_name(kind: TextKind) -> &'static str {
    match kind {
        TextKind::Integer => "<integer>",
        TextKind::Boolean => "<boolean>",
        TextKind::Character => "<character>",
        TextKind::Position => "<string-position>",
        TextKind::String => "<string>",
        TextKind::Builder => "<string-builder>",
        TextKind::Symbol => "<symbol>",
    }
}

/// The classes text values of the object kinds have in a module.
#[derive(Copy, Clone, Debug)]
pub struct TextClasses {
    /// `<string>`.
    pub string: ClassId,
    /// `<string-builder>`.
    pub builder: ClassId,
    /// `<symbol>`.
    pub symbol: ClassId,
}

impl TextClasses {
    /// Returns the value kind of a result of `kind`.
    #[must_use]
    pub const fn result_kind(self, kind: TextKind) -> ValueKind {
        match kind {
            TextKind::Integer => ValueKind::I64,
            TextKind::Boolean => ValueKind::Bool,
            TextKind::Character => ValueKind::Char,
            TextKind::Position => ValueKind::Position,
            TextKind::String => ValueKind::String(self.string),
            TextKind::Builder => ValueKind::Object(self.builder),
            TextKind::Symbol => ValueKind::Symbol(self.symbol),
        }
    }

    /// Returns whether an argument of value kind `actual` can be passed
    /// where `kind` is taken: an `<integer>` in any representation widens
    /// to one.
    #[must_use]
    pub fn arg_matches(self, kind: TextKind, actual: ValueKind) -> bool {
        match kind {
            TextKind::Integer => {
                matches!(actual, ValueKind::I64 | ValueKind::I32 | ValueKind::U32)
            }
            TextKind::Boolean => actual == ValueKind::Bool,
            TextKind::Character => actual == ValueKind::Char,
            TextKind::Position => actual == ValueKind::Position,
            TextKind::String => actual == ValueKind::String(self.string),
            TextKind::Builder => actual == ValueKind::Object(self.builder),
            TextKind::Symbol => actual == ValueKind::Symbol(self.symbol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NAMED_TEXT_FUNCTIONS, text_function_name, text_functions_for_name};
    use crate::sem::TextFunction;

    #[test]
    fn every_named_function_round_trips_and_names_are_unique() {
        for (index, (name, functions)) in NAMED_TEXT_FUNCTIONS.iter().enumerate() {
            assert_eq!(text_functions_for_name(name), Some(*functions));
            for function in *functions {
                assert_eq!(text_function_name(*function), *name);
                // Overloads of a name take as many arguments.
                assert_eq!(function.params().len(), functions[0].params().len());
            }
            assert!(
                NAMED_TEXT_FUNCTIONS[..index]
                    .iter()
                    .all(|(earlier, _)| earlier != name)
            );
        }
        assert_eq!(text_functions_for_name("as-character"), None);
        assert_eq!(
            text_function_name(TextFunction::CharacterFromInteger),
            "as(<character>, ...)"
        );
    }
}
