//! Handle encoding for boxed top `<object>` values.
//!
//! A boxed value is an `execution_tape` [`Obj`] whose host type is always
//! [`crate::INTERLOKO_VALUE_TYPE`], so every `<object>` register has one VM
//! type. Its 64-bit [`ObjHandle`] carries the value itself whenever it can:
//!
//! - Any bit pattern whose top 16 bits are not all set is an immediate
//!   `<double-float>`: the handle is the float's IEEE 754 bits.
//! - Top 16 bits set and bit 47 set: an immediate `<integer>`, the low 47
//!   bits sign-extended ([`IMMEDIATE_INTEGER_MIN`]..=[`IMMEDIATE_INTEGER_MAX`]).
//! - Top 16 bits set and bit 47 clear: bits 46..45 select the kind of the
//!   low 45 bits: a one-based index into the image's boxed values (`00`), a
//!   runtime object's own handle (`01`), a small immediate (`10`), or the
//!   serial of a nursery box (`11`). A small immediate is a boolean (payload
//!   `0` or `1`) or, with bit 32 set, a `<single-float>` whose IEEE 754 bits
//!   are the low 32 bits. Nursery serials count across turns, so a box kept
//!   past its turn is detected rather than aliased; see
//!   [`crate::region`].
//!
//! The tagged space is a sliver of NaN encodings (negative NaNs with the top
//! payload bits set), which no IEEE arithmetic produces by default. Floats
//! with those exact bits, integers outside the immediate range, and other
//! values (`u64`s, objects whose handle does not fit) are boxed on the heap,
//! so boxing never changes a value's bits. Every `<single-float>` is
//! immediate.
//!
//! `<i32>` and `<u32>` are limited `<integer>` types, not classes: their
//! values box as the `<integer>` they are, always immediate.
//!
//! [`Obj`]: execution_tape::value::Obj

use execution_tape::value::ObjHandle;

use crate::region::{NurserySerials, Space};

/// Top-16-bit tag shared by every non-float encoding.
const TAG: u64 = 0xFFFF_0000_0000_0000;
/// Set for immediate integers.
const INTEGER_FLAG: u64 = 1 << 47;
/// Integer payload bits below the tag and flag.
const INTEGER_PAYLOAD: u64 = INTEGER_FLAG - 1;
/// Bits that sign-extend a 47-bit payload to 64 bits.
const SIGN_EXTEND_SHIFT: u32 = 64 - 47;
/// Position of the two-bit kind selector for non-integer tagged handles.
const KIND_SHIFT: u32 = 45;
/// Mask of the two-bit kind selector.
const KIND_MASK: u64 = 0b11 << KIND_SHIFT;
/// Payload bits below the kind selector.
const PAYLOAD: u64 = (1 << KIND_SHIFT) - 1;
const KIND_HEAP: u64 = 0b00 << KIND_SHIFT;
const KIND_OBJECT: u64 = 0b01 << KIND_SHIFT;
const KIND_SMALL: u64 = 0b10 << KIND_SHIFT;
/// Set in a small immediate's payload for a `<single-float>`.
const SINGLE_FLOAT_FLAG: u64 = 1 << 32;
const KIND_NURSERY_HEAP: u64 = 0b11 << KIND_SHIFT;

/// Width of a nursery box's serial: the whole payload.
pub(crate) const NURSERY_SERIAL_BITS: u32 = KIND_SHIFT;
/// Mask of a nursery box's serial.
pub(crate) const NURSERY_SERIAL_MASK: u64 = (1 << NURSERY_SERIAL_BITS) - 1;

/// Smallest `<integer>` boxed without heap allocation.
pub const IMMEDIATE_INTEGER_MIN: i64 = -(1 << 46);
/// Largest `<integer>` boxed without heap allocation.
pub const IMMEDIATE_INTEGER_MAX: i64 = (1 << 46) - 1;

/// Decoded meaning of a boxed-value handle.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum BoxedHandle {
    /// An immediate `<double-float>`.
    Float(f64),
    /// An immediate `<integer>`.
    Integer(i64),
    /// An immediate `<single-float>`.
    SingleFloat(f32),
    /// An immediate boolean.
    Bool(bool),
    /// A runtime object, by its own [`crate::INTERLOKO_OBJECT_TYPE`] handle.
    Object(ObjHandle),
    /// A zero-based index into a region's boxed values.
    Heap(Space, usize),
    /// A tagged handle that names nothing.
    Invalid,
}

impl BoxedHandle {
    /// A tagged heap handle with payload zero, which never resolves.
    pub(crate) const INVALID: ObjHandle = ObjHandle(TAG | KIND_HEAP);

    /// Decodes a boxed-value handle against the open turn's `serials`.
    ///
    /// A nursery box from an earlier turn decodes to an index past the
    /// nursery's end, so the caller's bounds check rejects it.
    pub(crate) fn decode(handle: ObjHandle, serials: NurserySerials) -> Self {
        let bits = handle.0;
        if bits & TAG != TAG {
            return Self::Float(f64::from_bits(bits));
        }
        if bits & INTEGER_FLAG != 0 {
            // Shift the 47-bit payload to the top, then arithmetic-shift back.
            let shifted = (bits << SIGN_EXTEND_SHIFT).cast_signed();
            return Self::Integer(shifted >> SIGN_EXTEND_SHIFT);
        }
        let payload = bits & PAYLOAD;
        match bits & KIND_MASK {
            // Image indices are one-based so that the zero payload is invalid.
            KIND_HEAP => payload
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
                .map_or(Self::Invalid, |index| Self::Heap(Space::Image, index)),
            // An offset too large for `usize` is past any nursery's end too.
            KIND_NURSERY_HEAP => {
                let offset = payload.wrapping_sub(serials.values) & NURSERY_SERIAL_MASK;
                Self::Heap(
                    Space::Nursery,
                    usize::try_from(offset).unwrap_or(usize::MAX),
                )
            }
            KIND_OBJECT => Self::Object(ObjHandle(payload)),
            KIND_SMALL if payload <= 1 => Self::Bool(payload == 1),
            KIND_SMALL if payload >> 32 == SINGLE_FLOAT_FLAG >> 32 => {
                // The flag check leaves exactly the low 32 bits.
                Self::SingleFloat(f32::from_bits(payload as u32))
            }
            _ => Self::Invalid,
        }
    }

    /// Encodes `value` as an immediate handle, or `None` when it must be
    /// boxed on the heap.
    pub(crate) fn immediate_float(value: f64) -> Option<ObjHandle> {
        let bits = value.to_bits();
        (bits & TAG != TAG).then_some(ObjHandle(bits))
    }

    /// Encodes `value` as an immediate handle, or `None` when it must be
    /// boxed on the heap.
    pub(crate) fn immediate_integer(value: i64) -> Option<ObjHandle> {
        let payload = value.cast_unsigned() & INTEGER_PAYLOAD;
        (IMMEDIATE_INTEGER_MIN..=IMMEDIATE_INTEGER_MAX)
            .contains(&value)
            .then_some(ObjHandle(TAG | INTEGER_FLAG | payload))
    }

    /// Encodes a boolean as an immediate handle.
    pub(crate) const fn immediate_bool(value: bool) -> ObjHandle {
        ObjHandle(TAG | KIND_SMALL | value as u64)
    }

    /// Encodes a `<single-float>` as an immediate handle; every bit pattern
    /// fits.
    pub(crate) const fn immediate_single_float(value: f32) -> ObjHandle {
        ObjHandle(TAG | KIND_SMALL | SINGLE_FLOAT_FLAG | value.to_bits() as u64)
    }

    /// Encodes a runtime object's handle, or `None` when it is too large to
    /// carry and must be boxed on the heap.
    pub(crate) fn immediate_object(handle: ObjHandle) -> Option<ObjHandle> {
        (handle.0 <= PAYLOAD).then_some(ObjHandle(TAG | KIND_OBJECT | handle.0))
    }

    /// Encodes a zero-based index into `space`'s boxed values against the
    /// open turn's `serials`, or `None` when the region is too large to
    /// address.
    pub(crate) fn heap(space: Space, index: usize, serials: NurserySerials) -> Option<ObjHandle> {
        let index = u64::try_from(index).ok()?;
        match space {
            Space::Image => {
                let one_based = index.checked_add(1)?;
                (one_based <= PAYLOAD).then_some(ObjHandle(TAG | KIND_HEAP | one_based))
            }
            Space::Nursery => (index <= NURSERY_SERIAL_MASK).then(|| {
                let serial = serials.values.wrapping_add(index) & NURSERY_SERIAL_MASK;
                ObjHandle(TAG | KIND_NURSERY_HEAP | serial)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoxedHandle, IMMEDIATE_INTEGER_MAX, IMMEDIATE_INTEGER_MIN, NURSERY_SERIAL_MASK};
    use crate::region::{NurserySerials, RegionMark, Space};
    use alloc::vec::Vec;
    use execution_tape::value::ObjHandle;

    fn decode(handle: ObjHandle) -> BoxedHandle {
        BoxedHandle::decode(handle, NurserySerials::default())
    }

    #[test]
    fn floats_round_trip_by_bits_or_fall_back_to_the_heap() {
        for value in [
            0.0,
            -0.0,
            1.5,
            -2.5e-300,
            f64::MAX,
            f64::MIN_POSITIVE,
            5e-324,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            -f64::NAN,
            f64::from_bits(0xFFF8_0000_0000_0001),
        ] {
            let handle = BoxedHandle::immediate_float(value).expect("immediate float");
            let BoxedHandle::Float(decoded) = decode(handle) else {
                panic!("{value} should decode as a float");
            };
            assert_eq!(decoded.to_bits(), value.to_bits());
        }
        for bits in [0xFFFF_0000_0000_0000, 0xFFFF_FFFF_FFFF_FFFF] {
            assert_eq!(BoxedHandle::immediate_float(f64::from_bits(bits)), None);
        }
    }

    #[test]
    fn integers_round_trip_within_the_immediate_range() {
        for value in [
            0,
            1,
            -1,
            42,
            IMMEDIATE_INTEGER_MIN,
            IMMEDIATE_INTEGER_MAX,
            IMMEDIATE_INTEGER_MIN + 1,
            IMMEDIATE_INTEGER_MAX - 1,
        ] {
            let handle = BoxedHandle::immediate_integer(value).expect("immediate integer");
            assert_eq!(decode(handle), BoxedHandle::Integer(value));
        }
        for value in [
            IMMEDIATE_INTEGER_MAX + 1,
            IMMEDIATE_INTEGER_MIN - 1,
            i64::MAX,
            i64::MIN,
        ] {
            assert_eq!(BoxedHandle::immediate_integer(value), None);
        }
    }

    #[test]
    fn single_floats_are_immediate_and_distinct_from_booleans() {
        for value in [
            0.0_f32,
            -0.0,
            1.5,
            f32::MAX,
            f32::MIN_POSITIVE,
            f32::INFINITY,
            f32::NAN,
            f32::from_bits(1),
            f32::from_bits(u32::MAX),
        ] {
            let handle = BoxedHandle::immediate_single_float(value);
            let BoxedHandle::SingleFloat(decoded) = decode(handle) else {
                panic!("{value} should decode as a single float");
            };
            assert_eq!(decoded.to_bits(), value.to_bits());
        }
        // `0.0f32` has payload bit 32 set, so it is never `#f`.
        assert_ne!(
            BoxedHandle::immediate_single_float(0.0),
            BoxedHandle::immediate_bool(false)
        );
        // A small payload with bits above 32 names nothing.
        assert_eq!(
            decode(ObjHandle(BoxedHandle::immediate_bool(false).0 | (1 << 33))),
            BoxedHandle::Invalid
        );
    }

    #[test]
    fn heap_indices_are_disjoint_from_immediates() {
        for start in [0, 1, 1 << 30, NURSERY_SERIAL_MASK] {
            let serials = NurserySerials {
                objects: 0,
                values: start,
            };
            for index in [0, 1, 1000] {
                for space in [Space::Image, Space::Nursery] {
                    let handle = BoxedHandle::heap(space, index, serials).expect("heap handle");
                    assert_eq!(
                        BoxedHandle::decode(handle, serials),
                        BoxedHandle::Heap(space, index)
                    );
                }
            }
        }
        let serials = NurserySerials::default();
        assert_eq!(BoxedHandle::heap(Space::Image, usize::MAX, serials), None);
        assert_eq!(decode(BoxedHandle::INVALID), BoxedHandle::Invalid);
    }

    #[test]
    fn booleans_and_object_handles_are_immediate() {
        for value in [false, true] {
            assert_eq!(
                decode(BoxedHandle::immediate_bool(value)),
                BoxedHandle::Bool(value)
            );
        }
        for raw in [1, 2, 1 << 40, (1 << 45) - 1] {
            let handle = BoxedHandle::immediate_object(ObjHandle(raw)).expect("object handle");
            assert_eq!(decode(handle), BoxedHandle::Object(ObjHandle(raw)));
        }
        assert_eq!(BoxedHandle::immediate_object(ObjHandle(1 << 45)), None);
    }

    #[test]
    fn nursery_boxes_from_an_earlier_turn_decode_past_the_end() {
        let mut serials = NurserySerials {
            objects: 0,
            values: NURSERY_SERIAL_MASK - 1,
        };
        let handles = (0..4)
            .map(|index| BoxedHandle::heap(Space::Nursery, index, serials).expect("heap handle"))
            .collect::<Vec<_>>();
        serials.advance(RegionMark {
            objects: 0,
            values: 4,
        });
        assert_eq!(serials.values, 2);
        // Each old handle now names an index at the top of the serial
        // range, which no nursery reaches.
        for (index, handle) in handles.into_iter().enumerate() {
            let BoxedHandle::Heap(Space::Nursery, decoded) = BoxedHandle::decode(handle, serials)
            else {
                panic!("a nursery box stays a nursery box");
            };
            assert_eq!(decoded, (1 << 45) - 4 + index);
        }
    }
}
