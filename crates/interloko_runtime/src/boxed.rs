//! Handle encoding for boxed top `<object>` values.
//!
//! A boxed value is an `execution_tape` [`Obj`] whose host type is always
//! [`crate::INTERLOKO_VALUE_TYPE`], so every `<object>` register has one VM
//! type. Its 64-bit [`ObjHandle`] carries either the value itself or an index
//! into the host's boxed-value heap:
//!
//! - Any bit pattern whose top 16 bits are not all set is an immediate
//!   `<double-float>`: the handle is the float's IEEE 754 bits.
//! - Top 16 bits set and bit 47 set: an immediate `<integer>`, the low 47
//!   bits sign-extended ([`IMMEDIATE_INTEGER_MIN`]..=[`IMMEDIATE_INTEGER_MAX`]).
//! - Top 16 bits set and bit 47 clear: the low 47 bits are a one-based heap
//!   index.
//!
//! The tagged space is a sliver of NaN encodings (negative NaNs with the top
//! payload bits set), which no IEEE arithmetic produces by default. Floats
//! with those exact bits, integers outside the immediate range, and every
//! non-numeric value are boxed on the heap, so boxing never changes a value's
//! bits.

use execution_tape::value::ObjHandle;

/// Top-16-bit tag shared by immediate integers and heap indices.
const TAG: u64 = 0xFFFF_0000_0000_0000;
/// Set for immediate integers, clear for heap indices.
const INTEGER_FLAG: u64 = 1 << 47;
/// Payload bits below the tag and flag.
const PAYLOAD: u64 = INTEGER_FLAG - 1;
/// Bits that sign-extend a 47-bit payload to 64 bits.
const SIGN_EXTEND_SHIFT: u32 = 64 - 47;

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
    /// A zero-based index into the boxed-value heap.
    Heap(usize),
}

impl BoxedHandle {
    /// A tagged heap handle with payload zero, which never resolves.
    pub(crate) const INVALID: ObjHandle = ObjHandle(TAG);

    /// Decodes a boxed-value handle.
    pub(crate) fn decode(handle: ObjHandle) -> Self {
        let bits = handle.0;
        if bits & TAG != TAG {
            return Self::Float(f64::from_bits(bits));
        }
        if bits & INTEGER_FLAG != 0 {
            // Shift the 47-bit payload to the top, then arithmetic-shift back.
            let shifted = (bits << SIGN_EXTEND_SHIFT).cast_signed();
            return Self::Integer(shifted >> SIGN_EXTEND_SHIFT);
        }
        // Heap indices are one-based so that the zero payload is never valid.
        Self::Heap(
            usize::try_from(bits & PAYLOAD)
                .unwrap_or(usize::MAX)
                .wrapping_sub(1),
        )
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
        let payload = value.cast_unsigned() & PAYLOAD;
        (IMMEDIATE_INTEGER_MIN..=IMMEDIATE_INTEGER_MAX)
            .contains(&value)
            .then_some(ObjHandle(TAG | INTEGER_FLAG | payload))
    }

    /// Encodes a zero-based heap index, or `None` when the heap is full.
    pub(crate) fn heap(index: usize) -> Option<ObjHandle> {
        let one_based = u64::try_from(index).ok()?.checked_add(1)?;
        (one_based <= PAYLOAD).then_some(ObjHandle(TAG | one_based))
    }
}

#[cfg(test)]
mod tests {
    use super::{BoxedHandle, IMMEDIATE_INTEGER_MAX, IMMEDIATE_INTEGER_MIN};

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
            let BoxedHandle::Float(decoded) = BoxedHandle::decode(handle) else {
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
            assert_eq!(BoxedHandle::decode(handle), BoxedHandle::Integer(value));
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
    fn heap_indices_are_disjoint_from_immediates() {
        for index in [0, 1, 1000] {
            let handle = BoxedHandle::heap(index).expect("heap handle");
            assert_eq!(BoxedHandle::decode(handle), BoxedHandle::Heap(index));
        }
        assert_eq!(BoxedHandle::heap(usize::MAX), None);
    }
}
