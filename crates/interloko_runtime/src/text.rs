//! Text operations served by the runtime host: `<character>` predicates,
//! case mapping and conversions.
//!
//! A `<character>` is a Unicode scalar value. It travels unboxed in a `u32`
//! register, so every operation here that takes one checks it is a scalar
//! value before reading it: a register can hold any `u32`, such as a
//! `use-value` replacement a chooser supplied.
//!
//! Each operation is a host call named [`TextFunction::symbol`] whose
//! signature [`TextFunction::signature`] derives from its parameter and
//! result kinds ([`TextKind`]). An operation that can reject an argument
//! ([`TextFunction::checked_arg`]) fails with the check-failed tag, so the
//! calling code can signal a `type-error.range` and retry with a
//! `use-value` replacement for that argument.
//!
//! The predicates follow one rule: `digit?` and `digit-value` are ASCII
//! only, as a tokenizer wants and as they pair; `alphabetic?`,
//! `whitespace?` and the other unprefixed predicates use the Unicode
//! properties `core` carries; the `ascii-` predicates are ASCII only. Case
//! mapping is simple: one scalar to one scalar, unchanged where Unicode maps
//! it to several.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use execution_tape::host::{HostSig, ValueRef};
use execution_tape::program::ValueType;
use execution_tape::value::Value;

/// The kind of a [`TextFunction`] parameter or result, as Loko sees it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextKind {
    /// An `<integer>`, in an `i64` register.
    Integer,
    /// A `<boolean>`.
    Boolean,
    /// A `<character>`, in a `u32` register.
    Character,
}

impl TextKind {
    /// Returns the register type a value of this kind travels in.
    #[must_use]
    pub const fn value_type(self) -> ValueType {
        match self {
            Self::Integer => ValueType::I64,
            Self::Boolean => ValueType::Bool,
            Self::Character => ValueType::U32,
        }
    }
}

/// A text operation served by the runtime host.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextFunction {
    /// `as(<character>, i)`: the character whose scalar value is `i`. It
    /// rejects a value outside `0..=#x10FFFF` or a surrogate.
    CharacterFromInteger,
    /// Returns a character register's value when it is a scalar value, and
    /// rejects it otherwise. Compiled code checks a `<character>` a
    /// `use-value` restart supplied with it: a restart's replacement can
    /// be any `u32`.
    CheckCharacter,
    /// `digit-value(c)`: the value of an ASCII decimal digit. It rejects any
    /// other character.
    DigitValue,
    /// `digit?(c)`: whether `c` is an ASCII decimal digit.
    IsDigit,
    /// `alphabetic?(c)`: Unicode `Alphabetic`.
    IsAlphabetic,
    /// `alphanumeric?(c)`: Unicode `Alphabetic` or `Numeric`.
    IsAlphanumeric,
    /// `whitespace?(c)`: Unicode `White_Space`.
    IsWhitespace,
    /// `uppercase?(c)`: Unicode `Uppercase`.
    IsUppercase,
    /// `lowercase?(c)`: Unicode `Lowercase`.
    IsLowercase,
    /// `ascii?(c)`: whether `c` is below `#x80`.
    IsAscii,
    /// `ascii-alphabetic?(c)`: `A`–`Z` or `a`–`z`.
    IsAsciiAlphabetic,
    /// `ascii-alphanumeric?(c)`: an ASCII letter or digit.
    IsAsciiAlphanumeric,
    /// `ascii-whitespace?(c)`: space, tab, line feed, form feed or carriage
    /// return, as Rust's `char::is_ascii_whitespace` defines them.
    IsAsciiWhitespace,
    /// `ascii-uppercase?(c)`: `A`–`Z`.
    IsAsciiUppercase,
    /// `ascii-lowercase?(c)`: `a`–`z`.
    IsAsciiLowercase,
    /// `as-uppercase(c)`: the simple uppercase mapping, or `c` when Unicode
    /// maps it to more than one scalar.
    CharacterUppercase,
    /// `as-lowercase(c)`: the simple lowercase mapping, or `c` when Unicode
    /// maps it to more than one scalar.
    CharacterLowercase,
}

impl TextFunction {
    /// Every text operation, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::CharacterFromInteger,
        Self::CheckCharacter,
        Self::DigitValue,
        Self::IsDigit,
        Self::IsAlphabetic,
        Self::IsAlphanumeric,
        Self::IsWhitespace,
        Self::IsUppercase,
        Self::IsLowercase,
        Self::IsAscii,
        Self::IsAsciiAlphabetic,
        Self::IsAsciiAlphanumeric,
        Self::IsAsciiWhitespace,
        Self::IsAsciiUppercase,
        Self::IsAsciiLowercase,
        Self::CharacterUppercase,
        Self::CharacterLowercase,
    ];

    /// Returns the host-call symbol that serves this operation.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::CharacterFromInteger => "interloko.text.character_from_integer",
            Self::CheckCharacter => "interloko.text.check_character",
            Self::DigitValue => "interloko.text.digit_value",
            Self::IsDigit => "interloko.text.is_digit",
            Self::IsAlphabetic => "interloko.text.is_alphabetic",
            Self::IsAlphanumeric => "interloko.text.is_alphanumeric",
            Self::IsWhitespace => "interloko.text.is_whitespace",
            Self::IsUppercase => "interloko.text.is_uppercase",
            Self::IsLowercase => "interloko.text.is_lowercase",
            Self::IsAscii => "interloko.text.is_ascii",
            Self::IsAsciiAlphabetic => "interloko.text.is_ascii_alphabetic",
            Self::IsAsciiAlphanumeric => "interloko.text.is_ascii_alphanumeric",
            Self::IsAsciiWhitespace => "interloko.text.is_ascii_whitespace",
            Self::IsAsciiUppercase => "interloko.text.is_ascii_uppercase",
            Self::IsAsciiLowercase => "interloko.text.is_ascii_lowercase",
            Self::CharacterUppercase => "interloko.text.character_uppercase",
            Self::CharacterLowercase => "interloko.text.character_lowercase",
        }
    }

    /// Returns the operation served by host-call `symbol`, if any.
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        if !symbol.starts_with("interloko.text.") {
            return None;
        }
        Self::ALL
            .iter()
            .copied()
            .find(|function| function.symbol() == symbol)
    }

    /// Returns the parameters' kinds, in order.
    #[must_use]
    pub const fn params(self) -> &'static [TextKind] {
        match self {
            Self::CharacterFromInteger => &[TextKind::Integer],
            _ => &[TextKind::Character],
        }
    }

    /// Returns the result's kind.
    #[must_use]
    pub const fn result(self) -> TextKind {
        match self {
            Self::CharacterFromInteger
            | Self::CheckCharacter
            | Self::CharacterUppercase
            | Self::CharacterLowercase => TextKind::Character,
            Self::DigitValue => TextKind::Integer,
            _ => TextKind::Boolean,
        }
    }

    /// Returns the parameter this operation can reject, if any.
    ///
    /// A rejected argument fails the call with the check-failed tag; the
    /// caller signals a `type-error.range` naming it and retries the call
    /// with a `use-value` replacement for it.
    #[must_use]
    pub const fn checked_arg(self) -> Option<usize> {
        match self {
            Self::CharacterFromInteger | Self::CheckCharacter | Self::DigitValue => Some(0),
            _ => None,
        }
    }

    /// Returns the host-call signature.
    #[must_use]
    pub fn signature(self) -> HostSig {
        HostSig {
            args: self
                .params()
                .iter()
                .map(|kind| kind.value_type())
                .collect::<Vec<_>>(),
            rets: alloc::vec![self.result().value_type()],
        }
    }

    /// Applies the operation to `args`, or returns `None` when an argument
    /// is not of its kind or the operation rejects it.
    #[must_use]
    pub fn apply(self, args: &[ValueRef<'_>]) -> Option<Value> {
        if let Self::CharacterFromInteger = self {
            let [ValueRef::I64(value)] = args else {
                return None;
            };
            let scalar = char::from_u32(u32::try_from(*value).ok()?)?;
            return Some(Value::U32(u32::from(scalar)));
        }
        let [ValueRef::U32(value)] = args else {
            return None;
        };
        let c = char::from_u32(*value)?;
        let boolean = |value: bool| Some(Value::Bool(value));
        let character = |value: char| Some(Value::U32(u32::from(value)));
        match self {
            Self::CharacterFromInteger => None,
            Self::CheckCharacter => character(c),
            Self::DigitValue => c
                .to_digit(10)
                .filter(|_| c.is_ascii())
                .map(|digit| Value::I64(i64::from(digit))),
            Self::IsDigit => boolean(c.is_ascii_digit()),
            Self::IsAlphabetic => boolean(c.is_alphabetic()),
            Self::IsAlphanumeric => boolean(c.is_alphanumeric()),
            Self::IsWhitespace => boolean(c.is_whitespace()),
            Self::IsUppercase => boolean(c.is_uppercase()),
            Self::IsLowercase => boolean(c.is_lowercase()),
            Self::IsAscii => boolean(c.is_ascii()),
            Self::IsAsciiAlphabetic => boolean(c.is_ascii_alphabetic()),
            Self::IsAsciiAlphanumeric => boolean(c.is_ascii_alphanumeric()),
            Self::IsAsciiWhitespace => boolean(c.is_ascii_whitespace()),
            Self::IsAsciiUppercase => boolean(c.is_ascii_uppercase()),
            Self::IsAsciiLowercase => boolean(c.is_ascii_lowercase()),
            Self::CharacterUppercase => character(simple_case(c.to_uppercase()).unwrap_or(c)),
            Self::CharacterLowercase => character(simple_case(c.to_lowercase()).unwrap_or(c)),
        }
    }
}

/// Writes `value` as a Loko character literal, such as `'a'` or `'\n'`.
#[must_use]
pub fn character_literal(value: char) -> String {
    let mut literal = String::with_capacity(4);
    literal.push('\'');
    push_escaped(&mut literal, value, '\'');
    literal.push('\'');
    literal
}

/// Writes `text` as a Loko string literal, such as `"a\tb"`.
#[must_use]
pub fn string_literal(text: &str) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for value in text.chars() {
        push_escaped(&mut literal, value, '"');
    }
    literal.push('"');
    literal
}

/// Pushes `value` as it appears inside a literal delimited by `quote`:
/// escaped when it is the quote, a backslash or a control character, and
/// as itself otherwise. Every escape is one the literal syntax reads back.
fn push_escaped(out: &mut String, value: char, quote: char) {
    match value {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\t' => out.push_str("\\t"),
        '\r' => out.push_str("\\r"),
        '\0' => out.push_str("\\0"),
        value if value == quote => {
            out.push('\\');
            out.push(value);
        }
        value if value.is_control() => {
            let _ = write!(out, "\\u{{{:x}}}", u32::from(value));
        }
        value => out.push(value),
    }
}

/// The one scalar a case mapping yields, or `None` when it yields several.
fn simple_case(mut mapping: impl Iterator<Item = char>) -> Option<char> {
    let first = mapping.next()?;
    mapping.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::{TextFunction, TextKind, character_literal, string_literal};
    use execution_tape::host::ValueRef;
    use execution_tape::value::Value;

    fn apply(function: TextFunction, c: char) -> Option<Value> {
        function.apply(&[ValueRef::U32(u32::from(c))])
    }

    #[test]
    fn functions_round_trip_by_symbol_and_match_their_signatures() {
        for function in TextFunction::ALL {
            assert_eq!(
                TextFunction::from_symbol(function.symbol()),
                Some(*function)
            );
            let signature = function.signature();
            assert_eq!(signature.args.len(), function.params().len());
            assert!(
                function
                    .checked_arg()
                    .is_none_or(|index| index < function.params().len())
            );
        }
        assert_eq!(TextFunction::from_symbol("interloko.text.nope"), None);
        assert_eq!(TextFunction::from_symbol("interloko.float.sin"), None);
    }

    #[test]
    fn integers_convert_only_when_they_are_scalar_values() {
        let convert =
            |value: i64| TextFunction::CharacterFromInteger.apply(&[ValueRef::I64(value)]);
        assert_eq!(convert(97), Some(Value::U32(97)));
        assert_eq!(convert(0x10_FFFF), Some(Value::U32(0x10_FFFF)));
        for rejected in [-1, 0xD800, 0xDFFF, 0x11_0000, i64::MAX] {
            assert_eq!(convert(rejected), None, "{rejected}");
        }
        assert_eq!(
            TextFunction::CharacterFromInteger.result(),
            TextKind::Character
        );
    }

    #[test]
    fn digits_are_ascii_and_the_other_predicates_unicode() {
        assert_eq!(apply(TextFunction::DigitValue, '7'), Some(Value::I64(7)));
        // A Unicode digit is not a `digit?`, and has no `digit-value`.
        assert_eq!(apply(TextFunction::DigitValue, '\u{0663}'), None);
        assert_eq!(
            apply(TextFunction::IsDigit, '\u{0663}'),
            Some(Value::Bool(false))
        );
        assert_eq!(apply(TextFunction::DigitValue, 'x'), None);
        assert_eq!(
            apply(TextFunction::IsAlphabetic, 'é'),
            Some(Value::Bool(true))
        );
        assert_eq!(
            apply(TextFunction::IsAsciiAlphabetic, 'é'),
            Some(Value::Bool(false))
        );
        assert_eq!(
            apply(TextFunction::IsWhitespace, '\u{3000}'),
            Some(Value::Bool(true))
        );
        assert_eq!(
            apply(TextFunction::IsAsciiWhitespace, '\u{3000}'),
            Some(Value::Bool(false))
        );
    }

    #[test]
    fn case_maps_one_scalar_to_one() {
        assert_eq!(
            apply(TextFunction::CharacterUppercase, 'a'),
            Some(Value::U32(u32::from('A')))
        );
        assert_eq!(
            apply(TextFunction::CharacterLowercase, 'É'),
            Some(Value::U32(u32::from('é')))
        );
        // `ß` uppercases to "SS", two scalars, so it stays.
        assert_eq!(
            apply(TextFunction::CharacterUppercase, 'ß'),
            Some(Value::U32(u32::from('ß')))
        );
    }

    #[test]
    fn literals_escape_what_the_syntax_reads_back() {
        assert_eq!(character_literal('a'), "'a'");
        assert_eq!(character_literal('\''), "'\\''");
        assert_eq!(character_literal('"'), "'\"'");
        assert_eq!(character_literal('\0'), "'\\0'");
        assert_eq!(character_literal('\u{7}'), "'\\u{7}'");
        assert_eq!(character_literal('é'), "'é'");
        assert_eq!(string_literal("a\"b'\\\n"), "\"a\\\"b'\\\\\\n\"");
    }

    #[test]
    fn a_register_that_is_not_a_scalar_value_is_rejected() {
        for function in TextFunction::ALL {
            if function.params() == [TextKind::Character] {
                assert_eq!(function.apply(&[ValueRef::U32(0xD800)]), None);
                assert_eq!(function.apply(&[ValueRef::I64(65)]), None);
            }
        }
    }
}
