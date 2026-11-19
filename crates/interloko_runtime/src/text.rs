//! Text operations served by the runtime host: `<character>` predicates,
//! case mapping and conversions, `<string>` positions and copies, and the
//! `<string-builder>`.
//!
//! A `<character>` is a Unicode scalar value. It travels unboxed in a `u32`
//! register, so every operation here that takes one checks it is a scalar
//! value before reading it: a register can hold any `u32`, such as a
//! `use-value` replacement a chooser supplied.
//!
//! A `<string>` is immutable, validated UTF-8 with a header
//! ([`StringText`]) that caches its scalar count and whether it is all
//! ASCII. A `<string-position>` is a byte offset in an `i64` register. Only
//! these operations make positions, but a register can hold any `i64`, and a
//! position from one string can be used with another, so every operation
//! checks the positions it is given: in bounds and on a character boundary.
//! A position that fails is rejected, never read past.
//!
//! Each operation is a host call named [`TextFunction::symbol`] whose
//! signature [`TextFunction::signature`] derives from its parameter and
//! result kinds ([`TextKind`]). Compiled code answers the reads that step
//! through a string itself, from the bytes
//! [`InterlokoHost::string_bytes`] lends it, and makes the host call only
//! for a position that has no character, so the two must agree on which
//! positions those are. An operation that can reject an argument
//! ([`TextFunction::checked_arg`]) fails with the check-failed tag, so the
//! calling code can signal a `type-error.range` and retry with a
//! `use-value` replacement for that argument.
//!
//! The predicates follow one rule: `digit?` and `digit-value` are ASCII
//! only, as a tokenizer wants and as they pair; `alphabetic?`,
//! `whitespace?` and the other unprefixed predicates use the Unicode
//! properties `core` carries; the `ascii-` predicates are ASCII only. Case
//! mapping is simple for a character, one scalar to one, unchanged where
//! Unicode maps it to several, and full for a string (`ß` to `SS`).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use execution_tape::host::{HostSig, ValueRef};
use execution_tape::program::ValueType;
use execution_tape::value::Value;

use crate::region::Space;
use crate::{
    BuiltinValueClass, INTERLOKO_OBJECT_TYPE, InterlokoHost, RuntimeObject, RuntimeObjectStorage,
    object_type,
};

/// The largest byte length a `<string>` may have: a boxed
/// `<string-position>` carries its offset in 44 bits.
pub const STRING_MAX_BYTES: u64 = (1 << 44) - 1;

/// Whether a byte length or offset is one a `<string>` can have.
fn within_string_bytes(bytes: usize) -> bool {
    u64::try_from(bytes).is_ok_and(|bytes| bytes <= STRING_MAX_BYTES)
}

/// Whether `offset`, a position register's value, is one some string can
/// have.
#[must_use]
pub fn is_position_offset(offset: i64) -> bool {
    u64::try_from(offset).is_ok_and(|offset| offset <= STRING_MAX_BYTES)
}

/// A `<string>`'s text and header.
///
/// The header is computed once, at construction, which already copies or
/// validates every byte: the number of scalar values, so `size` is O(1),
/// and whether every byte is ASCII, so `position-at` is O(1) then.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StringText {
    text: String,
    scalars: usize,
    ascii: bool,
}

impl StringText {
    /// Wraps `text`, or returns `None` when it is longer than
    /// [`STRING_MAX_BYTES`].
    pub(crate) fn new(text: String) -> Option<Self> {
        if !within_string_bytes(text.len()) {
            return None;
        }
        // A scalar starts at every byte that is not a continuation byte.
        let scalars = text.bytes().filter(|byte| (*byte as i8) >= -0x40).count();
        let ascii = scalars == text.len();
        Some(Self {
            text,
            scalars,
            ascii,
        })
    }

    /// The UTF-8 text.
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// The number of Unicode scalar values.
    pub(crate) const fn scalars(&self) -> usize {
        self.scalars
    }

    /// The position of a register's `offset` when it is a character
    /// boundary of this text, at most its end.
    fn boundary(&self, offset: i64) -> Option<usize> {
        let offset = usize::try_from(offset).ok()?;
        self.text.is_char_boundary(offset).then_some(offset)
    }

    /// The position of the `n`th scalar value, or the end for `n` equal to
    /// the scalar count: O(1) for ASCII text, O(n) otherwise.
    fn nth(&self, n: i64) -> Option<usize> {
        let n = usize::try_from(n).ok()?;
        if n > self.scalars {
            return None;
        }
        if self.ascii || n == self.scalars {
            return Some(if n == self.scalars {
                self.text.len()
            } else {
                n
            });
        }
        self.text.char_indices().nth(n).map(|(offset, _)| offset)
    }

    /// The character at boundary `offset`, or `None` at the end.
    fn character_at(&self, offset: usize) -> Option<char> {
        self.text.get(offset..)?.chars().next()
    }
}

/// The kind of a [`TextFunction`] parameter or result, as Loko sees it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextKind {
    /// An `<integer>`, in an `i64` register.
    Integer,
    /// A `<boolean>`.
    Boolean,
    /// A `<character>`, in a `u32` register.
    Character,
    /// A `<string-position>`, a byte offset in an `i64` register.
    Position,
    /// A `<string>`, a runtime object.
    String,
    /// A `<string-builder>`, a runtime object.
    Builder,
}

impl TextKind {
    /// Returns the register type a value of this kind travels in.
    #[must_use]
    pub const fn value_type(self) -> ValueType {
        match self {
            Self::Integer | Self::Position => ValueType::I64,
            Self::Boolean => ValueType::Bool,
            Self::Character => ValueType::U32,
            Self::String | Self::Builder => object_type(),
        }
    }
}

macro_rules! text_functions {
    ($($(#[$doc:meta])* $name:ident = $symbol:literal, ($($param:ident),*) -> $result:ident, $checked:expr;)*) => {
        /// A text operation served by the runtime host.
        #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
        pub enum TextFunction {
            $($(#[$doc])* $name,)*
        }

        impl TextFunction {
            /// Every text operation, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$name),*];

            /// Returns the host-call symbol that serves this operation.
            #[must_use]
            pub const fn symbol(self) -> &'static str {
                match self {
                    $(Self::$name => $symbol,)*
                }
            }

            /// Returns the parameters' kinds, in order.
            #[must_use]
            pub const fn params(self) -> &'static [TextKind] {
                match self {
                    $(Self::$name => &[$(TextKind::$param),*],)*
                }
            }

            /// Returns the result's kind.
            #[must_use]
            pub const fn result(self) -> TextKind {
                match self {
                    $(Self::$name => TextKind::$result,)*
                }
            }

            /// Returns the parameter this operation can reject, if any.
            ///
            /// A rejected argument fails the call with the check-failed tag;
            /// the caller signals a `type-error.range` naming it and retries
            /// the call with a `use-value` replacement for it, which the
            /// operation checks again.
            #[must_use]
            pub const fn checked_arg(self) -> Option<usize> {
                match self {
                    $(Self::$name => $checked,)*
                }
            }
        }
    };
}

text_functions! {
    /// `as(<character>, i)`: the character whose scalar value is `i`. It
    /// rejects a value outside `0..=#x10FFFF` or a surrogate.
    CharacterFromInteger = "interloko.text.character_from_integer", (Integer) -> Character, Some(0);
    /// Returns a character register's value when it is a scalar value, and
    /// rejects it otherwise. Compiled code checks a `<character>` a
    /// `use-value` restart supplied with it: a restart's replacement can
    /// be any `u32`.
    CheckCharacter = "interloko.text.check_character", (Character) -> Character, Some(0);
    /// Returns a position register's value when it is an offset some string
    /// can have, and rejects it otherwise. Compiled code checks a
    /// `<string-position>` a `use-value` restart supplied with it, so every
    /// position can be boxed.
    CheckPositionValue = "interloko.text.check_position_value", (Position) -> Position, Some(0);
    /// `digit-value(c)`: the value of an ASCII decimal digit. It rejects any
    /// other character.
    DigitValue = "interloko.text.digit_value", (Character) -> Integer, Some(0);
    /// `digit?(c)`: whether `c` is an ASCII decimal digit.
    IsDigit = "interloko.text.is_digit", (Character) -> Boolean, None;
    /// `alphabetic?(c)`: Unicode `Alphabetic`.
    IsAlphabetic = "interloko.text.is_alphabetic", (Character) -> Boolean, None;
    /// `alphanumeric?(c)`: Unicode `Alphabetic` or `Numeric`.
    IsAlphanumeric = "interloko.text.is_alphanumeric", (Character) -> Boolean, None;
    /// `whitespace?(c)`: Unicode `White_Space`.
    IsWhitespace = "interloko.text.is_whitespace", (Character) -> Boolean, None;
    /// `uppercase?(c)`: Unicode `Uppercase`.
    IsUppercase = "interloko.text.is_uppercase", (Character) -> Boolean, None;
    /// `lowercase?(c)`: Unicode `Lowercase`.
    IsLowercase = "interloko.text.is_lowercase", (Character) -> Boolean, None;
    /// `ascii?(c)`: whether `c` is below `#x80`.
    IsAscii = "interloko.text.is_ascii", (Character) -> Boolean, None;
    /// `ascii-alphabetic?(c)`: `A`–`Z` or `a`–`z`.
    IsAsciiAlphabetic = "interloko.text.is_ascii_alphabetic", (Character) -> Boolean, None;
    /// `ascii-alphanumeric?(c)`: an ASCII letter or digit.
    IsAsciiAlphanumeric = "interloko.text.is_ascii_alphanumeric", (Character) -> Boolean, None;
    /// `ascii-whitespace?(c)`: space, tab, line feed, form feed or carriage
    /// return, as Rust's `char::is_ascii_whitespace` defines them.
    IsAsciiWhitespace = "interloko.text.is_ascii_whitespace", (Character) -> Boolean, None;
    /// `ascii-uppercase?(c)`: `A`–`Z`.
    IsAsciiUppercase = "interloko.text.is_ascii_uppercase", (Character) -> Boolean, None;
    /// `ascii-lowercase?(c)`: `a`–`z`.
    IsAsciiLowercase = "interloko.text.is_ascii_lowercase", (Character) -> Boolean, None;
    /// `as-uppercase(c)`: the simple uppercase mapping, or `c` when Unicode
    /// maps it to more than one scalar.
    CharacterUppercase = "interloko.text.character_uppercase", (Character) -> Character, None;
    /// `as-lowercase(c)`: the simple lowercase mapping, or `c` when Unicode
    /// maps it to more than one scalar.
    CharacterLowercase = "interloko.text.character_lowercase", (Character) -> Character, None;
    /// `as(<string>, c)`: a one-character string.
    CharacterToString = "interloko.text.character_to_string", (Character) -> String, None;
    /// `size(s)`: the number of scalar values, from the header.
    StringSize = "interloko.text.string_size", (String) -> Integer, None;
    /// `start-position(s)`: the first position, always the start.
    StartPosition = "interloko.text.start_position", (String) -> Position, None;
    /// `end-position(s)`: the position after the last character.
    EndPosition = "interloko.text.end_position", (String) -> Position, None;
    /// `s[p]`, `element(s, p)`: the character at `p`. It rejects a position
    /// that is not a boundary before the end.
    StringElement = "interloko.text.string_element", (String, Position) -> Character, Some(1);
    /// `element(s, p, default: c)`: the character at `p`, or `c` at or past
    /// the end. It rejects a position inside a character.
    StringElementOr = "interloko.text.string_element_or", (String, Position, Character) -> Character, Some(1);
    /// `next-position(s, p)`: the position after the character at `p`. It
    /// rejects the end and any position that is not a boundary.
    NextPosition = "interloko.text.next_position", (String, Position) -> Position, Some(1);
    /// `previous-position(s, p)`: the position of the character before
    /// `p`. It rejects the start and any position that is not a boundary.
    PreviousPosition = "interloko.text.previous_position", (String, Position) -> Position, Some(1);
    /// `position-at(s, n)`: the position of the `n`th character, or the end
    /// for `n = size(s)`; O(1) for ASCII text, O(n) otherwise. It rejects
    /// any other `n`.
    PositionAt = "interloko.text.position_at", (String, Integer) -> Position, Some(1);
    /// `as(<string-position>, s, n)`: the position at byte offset `n`. It
    /// rejects an offset that is not a boundary of `s`.
    PositionFromOffset = "interloko.text.position_from_offset", (String, Integer) -> Position, Some(1);
    /// Returns `p` when it is a boundary of `s`, the end included, and
    /// rejects it otherwise: `copy-sequence`'s `start:`.
    CheckPosition = "interloko.text.check_position", (String, Position) -> Position, Some(1);
    /// `copy-sequence(s, start: p, end: q)`: a new string of the characters
    /// from `p` up to `q`. It rejects a `q` that is not a boundary or comes
    /// before `p`; `p` is checked first ([`Self::CheckPosition`]).
    CopyRange = "interloko.text.copy_range", (String, Position, Position) -> String, Some(2);
    /// `starts-with?(s, prefix, at: p)`: whether `prefix`'s characters
    /// follow `p`, without allocating. It rejects a `p` that is not a
    /// boundary.
    StartsWith = "interloko.text.starts_with", (String, String, Position) -> Boolean, Some(2);
    /// `position-after(s, prefix, p)`: the position just past `prefix` at
    /// `p`. It rejects a `p` that is not a boundary or where `prefix` does
    /// not occur.
    PositionAfter = "interloko.text.position_after", (String, String, Position) -> Position, Some(2);
    /// `a < b` on strings: `-1`, `0` or `1` as `a` orders before, with or
    /// after `b`, by scalar value, which UTF-8 byte order gives.
    StringOrder = "interloko.text.string_order", (String, String) -> Integer, None;
    /// `as-uppercase(s)`: the full uppercase mapping, which may change the
    /// length (`ß` to `SS`).
    StringUppercase = "interloko.text.string_uppercase", (String) -> String, None;
    /// `as-lowercase(s)`: the full lowercase mapping.
    StringLowercase = "interloko.text.string_lowercase", (String) -> String, None;
    /// `make(<string-builder>)`: an empty builder.
    MakeBuilder = "interloko.text.make_builder", () -> Builder, None;
    /// `add!(b, c)`: appends a character and returns the builder.
    BuilderAddCharacter = "interloko.text.builder_add_character", (Builder, Character) -> Builder, None;
    /// `add!(b, s)`: appends a string and returns the builder.
    BuilderAddString = "interloko.text.builder_add_string", (Builder, String) -> Builder, None;
    /// `size(b)`: the number of characters added so far.
    BuilderSize = "interloko.text.builder_size", (Builder) -> Integer, None;
    /// `as(<string>, b)`: the text built so far as a `<string>`, moved
    /// without a copy; the builder is empty afterwards.
    BuilderToString = "interloko.text.builder_to_string", (Builder) -> String, None;
}

impl TextFunction {
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

    /// Applies a character operation to `args`, or returns `None` when an
    /// argument is not of its kind or the operation rejects it. Operations
    /// on strings and builders need the host
    /// ([`crate::InterlokoHost`]) and return `None` here.
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
            _ => None,
        }
    }
}

/// Why a text operation produced no result.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum TextFailure {
    /// The operation rejected its checked argument
    /// ([`TextFunction::checked_arg`]): the caller may retry with a
    /// replacement.
    Rejected,
    /// An argument was not of its kind, or the runtime could not serve the
    /// operation: a trap.
    Failed,
}

impl InterlokoHost {
    /// Runs `function` on `args`.
    pub(crate) fn call_text(
        &mut self,
        function: TextFunction,
        args: &[ValueRef<'_>],
    ) -> Result<Value, TextFailure> {
        use TextFailure::{Failed, Rejected};
        use TextFunction as F;
        let rejected = |checked: bool| if checked { Rejected } else { Failed };
        let position = |offset: usize| Value::I64(offset as i64);
        Ok(match (function, args) {
            (F::CharacterToString, [ValueRef::U32(value)]) => {
                let value = char::from_u32(*value).ok_or(Failed)?;
                self.push_string(String::from(value)).map_err(|_| Failed)?
            }
            (F::CheckPositionValue, [ValueRef::I64(offset)]) => {
                if !is_position_offset(*offset) {
                    return Err(Rejected);
                }
                Value::I64(*offset)
            }
            (F::StringSize, [s]) => Value::I64(self.text_arg(*s)?.scalars() as i64),
            (F::StartPosition, [s]) => {
                self.text_arg(*s)?;
                position(0)
            }
            (F::EndPosition, [s]) => position(self.text_arg(*s)?.as_str().len()),
            (F::StringElement, [s, ValueRef::I64(p)]) => {
                let text = self.text_arg(*s)?;
                let at = text.boundary(*p).ok_or(Rejected)?;
                let c = text.character_at(at).ok_or(Rejected)?;
                Value::U32(u32::from(c))
            }
            (F::StringElementOr, [s, ValueRef::I64(p), ValueRef::U32(default)]) => {
                let text = self.text_arg(*s)?;
                let default = char::from_u32(*default).ok_or(Failed)?;
                // At or past the end is the default's; inside, a boundary.
                let c = if usize::try_from(*p).is_ok_and(|p| p >= text.as_str().len()) {
                    default
                } else {
                    let at = text.boundary(*p).ok_or(Rejected)?;
                    text.character_at(at).ok_or(Rejected)?
                };
                Value::U32(u32::from(c))
            }
            (F::NextPosition, [s, ValueRef::I64(p)]) => {
                let text = self.text_arg(*s)?;
                let at = text.boundary(*p).ok_or(Rejected)?;
                let c = text.character_at(at).ok_or(Rejected)?;
                position(at + c.len_utf8())
            }
            (F::PreviousPosition, [s, ValueRef::I64(p)]) => {
                let text = self.text_arg(*s)?;
                let at = text.boundary(*p).ok_or(Rejected)?;
                let c = text.as_str()[..at].chars().next_back().ok_or(Rejected)?;
                position(at - c.len_utf8())
            }
            (F::PositionAt, [s, ValueRef::I64(n)]) => {
                position(self.text_arg(*s)?.nth(*n).ok_or(Rejected)?)
            }
            (F::PositionFromOffset | F::CheckPosition, [s, ValueRef::I64(p)]) => {
                position(self.text_arg(*s)?.boundary(*p).ok_or(Rejected)?)
            }
            (F::CopyRange, [s, ValueRef::I64(p), ValueRef::I64(q)]) => {
                let text = self.text_arg(*s)?;
                let start = text.boundary(*p).ok_or(Rejected)?;
                let end = text
                    .boundary(*q)
                    .filter(|end| *end >= start)
                    .ok_or(Rejected)?;
                let copy = String::from(&text.as_str()[start..end]);
                self.push_string(copy).map_err(|_| Failed)?
            }
            (F::StartsWith | F::PositionAfter, [s, prefix, ValueRef::I64(p)]) => {
                let prefix = self.text_arg(*prefix)?.as_str();
                let text = self.text_arg(*s)?;
                let at = text.boundary(*p).ok_or(Rejected)?;
                let found = text.as_str()[at..].starts_with(prefix);
                match function {
                    F::StartsWith => Value::Bool(found),
                    _ if found => position(at + prefix.len()),
                    _ => return Err(Rejected),
                }
            }
            (F::StringOrder, [a, b]) => {
                let order = self.text_arg(*a)?.as_str().cmp(self.text_arg(*b)?.as_str());
                Value::I64(order as i64)
            }
            (F::StringUppercase | F::StringLowercase, [s]) => {
                let text = self.text_arg(*s)?.as_str();
                let mapped = if function == F::StringUppercase {
                    text.to_uppercase()
                } else {
                    text.to_lowercase()
                };
                self.push_string(mapped).map_err(|_| Failed)?
            }
            (F::MakeBuilder, []) => {
                let class = self
                    .builtin_class(BuiltinValueClass::StringBuilder)
                    .map_err(|_| Failed)?;
                self.push_object(RuntimeObject {
                    class,
                    storage: RuntimeObjectStorage::StringBuilder {
                        text: String::new(),
                        scalars: 0,
                    },
                })
            }
            (F::BuilderAddCharacter, [b, ValueRef::U32(value)]) => {
                let c = char::from_u32(*value).ok_or(Failed)?;
                let (text, scalars) = self.builder_text_mut(*b)?;
                text.push(c);
                *scalars += 1;
                b.to_value()
            }
            (F::BuilderAddString, [b, s]) => {
                let addition = self.text_arg(*s)?;
                let (added, added_scalars) = (String::from(addition.as_str()), addition.scalars());
                let (text, scalars) = self.builder_text_mut(*b)?;
                if !within_string_bytes(text.len().saturating_add(added.len())) {
                    return Err(Failed);
                }
                text.push_str(&added);
                *scalars += added_scalars;
                b.to_value()
            }
            (F::BuilderSize, [b]) => {
                let ValueRef::Obj(obj) = b else {
                    return Err(Failed);
                };
                match &self.object(*obj).map_err(|_| Failed)?.storage {
                    RuntimeObjectStorage::StringBuilder { scalars, .. } => {
                        Value::I64(*scalars as i64)
                    }
                    _ => return Err(Failed),
                }
            }
            (F::BuilderToString, [b]) => {
                let (text, scalars) = self.builder_text_mut(*b)?;
                let text = core::mem::take(text);
                *scalars = 0;
                self.push_string(text).map_err(|_| Failed)?
            }
            (function, args) => {
                return function
                    .apply(args)
                    .ok_or(rejected(function.checked_arg().is_some()));
            }
        })
    }

    /// The `<string>` text `value` holds.
    fn text_arg(&self, value: ValueRef<'_>) -> Result<&StringText, TextFailure> {
        let ValueRef::Obj(obj) = value else {
            return Err(TextFailure::Failed);
        };
        if obj.host_type != INTERLOKO_OBJECT_TYPE {
            return Err(TextFailure::Failed);
        }
        match &self.object(obj).map_err(|_| TextFailure::Failed)?.storage {
            RuntimeObjectStorage::String { text } => Ok(text),
            _ => Err(TextFailure::Failed),
        }
    }

    /// The text of the `<string-builder>` `value` and its character count,
    /// to change.
    ///
    /// A builder in the image cannot change during a turn: a turn that
    /// fails is undone, and builder text is not in the write log yet. Build
    /// within a turn, or between turns, instead.
    fn builder_text_mut(
        &mut self,
        value: ValueRef<'_>,
    ) -> Result<(&mut String, &mut usize), TextFailure> {
        let ValueRef::Obj(obj) = value else {
            return Err(TextFailure::Failed);
        };
        let location = self.object_ref(obj).map_err(|_| TextFailure::Failed)?;
        if self.turn.is_some() && location.space == Space::Image {
            return Err(TextFailure::Failed);
        }
        match &mut self
            .object_mut(obj)
            .map_err(|_| TextFailure::Failed)?
            .storage
        {
            RuntimeObjectStorage::StringBuilder { text, scalars } => Ok((text, scalars)),
            _ => Err(TextFailure::Failed),
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
