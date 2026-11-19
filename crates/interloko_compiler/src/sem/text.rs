//! Loko's built-in text functions: the `<character>` predicates, case
//! mapping and digit values.
//!
//! Each is a plain built-in function, as the float functions are: a call
//! resolves to it only when no definition of the program has the name, and
//! the runtime host serves it ([`TextFunction`]). This module names them and
//! maps their runtime kinds to value kinds; the runtime owns what they do.

use super::{TextFunction, ValueKind};
use interloko_runtime::TextKind;

/// The text functions a call can name, with their Loko names.
///
/// `as(<character>, i)` is [`TextFunction::CharacterFromInteger`], which no
/// name reaches.
pub const NAMED_TEXT_FUNCTIONS: &[(&str, TextFunction)] = &[
    ("digit-value", TextFunction::DigitValue),
    ("digit?", TextFunction::IsDigit),
    ("alphabetic?", TextFunction::IsAlphabetic),
    ("alphanumeric?", TextFunction::IsAlphanumeric),
    ("whitespace?", TextFunction::IsWhitespace),
    ("uppercase?", TextFunction::IsUppercase),
    ("lowercase?", TextFunction::IsLowercase),
    ("ascii?", TextFunction::IsAscii),
    ("ascii-alphabetic?", TextFunction::IsAsciiAlphabetic),
    ("ascii-alphanumeric?", TextFunction::IsAsciiAlphanumeric),
    ("ascii-whitespace?", TextFunction::IsAsciiWhitespace),
    ("ascii-uppercase?", TextFunction::IsAsciiUppercase),
    ("ascii-lowercase?", TextFunction::IsAsciiLowercase),
    ("as-uppercase", TextFunction::CharacterUppercase),
    ("as-lowercase", TextFunction::CharacterLowercase),
];

/// Returns the text function a call of `name` reaches.
#[must_use]
pub fn text_function_for_name(name: &str) -> Option<TextFunction> {
    NAMED_TEXT_FUNCTIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, function)| *function)
}

/// Returns the Loko name of `function`, as diagnostics and dumps write it.
#[must_use]
pub fn text_function_name(function: TextFunction) -> &'static str {
    match function {
        TextFunction::CharacterFromInteger => "as(<character>, ...)",
        function => NAMED_TEXT_FUNCTIONS
            .iter()
            .find(|(_, candidate)| *candidate == function)
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
    }
}

/// Returns the value kind of a result of `kind`.
#[must_use]
pub const fn text_result_kind(kind: TextKind) -> ValueKind {
    match kind {
        TextKind::Integer => ValueKind::I64,
        TextKind::Boolean => ValueKind::Bool,
        TextKind::Character => ValueKind::Char,
    }
}

/// Returns whether an argument of value kind `actual` can be passed where
/// `kind` is taken: an `<integer>` in any representation widens to one.
#[must_use]
pub const fn text_arg_matches(kind: TextKind, actual: ValueKind) -> bool {
    match kind {
        TextKind::Integer => matches!(actual, ValueKind::I64 | ValueKind::I32 | ValueKind::U32),
        TextKind::Boolean => matches!(actual, ValueKind::Bool),
        TextKind::Character => matches!(actual, ValueKind::Char),
    }
}

#[cfg(test)]
mod tests {
    use super::{NAMED_TEXT_FUNCTIONS, text_function_for_name, text_function_name};
    use crate::sem::TextFunction;

    #[test]
    fn every_named_function_round_trips_and_names_are_unique() {
        for (index, (name, function)) in NAMED_TEXT_FUNCTIONS.iter().enumerate() {
            assert_eq!(text_function_for_name(name), Some(*function));
            assert_eq!(text_function_name(*function), *name);
            assert!(
                NAMED_TEXT_FUNCTIONS[..index]
                    .iter()
                    .all(|(earlier, other)| earlier != name && other != function)
            );
        }
        assert_eq!(text_function_for_name("as-character"), None);
        assert!(
            !NAMED_TEXT_FUNCTIONS
                .iter()
                .any(|(_, function)| *function == TextFunction::CharacterFromInteger)
        );
    }
}
