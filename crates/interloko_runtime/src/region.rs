//! The runtime host's two heap regions and the turns that use them.
//!
//! The **image** holds long-lived runtime state: objects created outside a
//! turn (an embedding's setup, host-backed objects), interned symbols, and
//! the survivors evacuated from finished turns. The **nursery** holds everything
//! a turn allocates: instances, strings, vectors, metaobjects and heap
//! boxes. A turn is one bounded call into the runtime; when it ends the
//! nursery is emptied in one step, so a turn's garbage costs nothing to
//! reclaim.
//!
//! The image only grows. A turn appends to it only by interning symbols,
//! and an aborted turn keeps them: abort restores everything except the
//! symbols interned during the turn, which stay interned. No image index is
//! ever reused, so an image handle names the same object for the host's
//! lifetime.
//!
//! Object handles say which region they index. An image object's handle is
//! its one-based index; a nursery object's handle sets
//! [`NURSERY_OBJECT_BIT`] above a *serial*. Heap-boxed values use the
//! boxed-handle kinds `00` (image index) and `11` (nursery serial); see
//! [`crate::boxed`].
//!
//! # Nursery serials
//!
//! Every nursery entry gets the next serial of its kind, counting across
//! turns rather than restarting at each one: a turn's entries take the
//! serials after the previous turn's, whether that turn finished or
//! aborted. [`NurserySerials`] records where the open turn's serials start,
//! and a nursery handle resolves only when its serial falls within the
//! open turn's entries. A handle kept past the end of its turn therefore
//! names no entry of a later turn: resolving it fails with
//! [`crate::RuntimeError::StaleNurseryHandle`] instead of aliasing whatever
//! the later turn allocated at the same index.
//!
//! Serials are [`OBJECT_SERIAL_BITS`] wide for objects and
//! [`crate::boxed::NURSERY_SERIAL_BITS`] wide for heap boxes, and wrap
//! modulo that width. A stale handle can resolve again only once the
//! counter has come back round to its serial, after `2^44` more nursery
//! objects (`2^45` more boxes) have been allocated; until then it is
//! always detected. That window is the guarantee, and it is deliberately
//! finite: the serials have to fit beside the tag bits of a boxed
//! `<object>`, and refusing to wrap would end the host's ability to run
//! turns. Image handles carry no serial and never go stale.
//!
//! Decoding a nursery handle costs one subtraction and one mask beyond an
//! image handle's decoding; the bounds check that follows is the one every
//! handle already pays.

use alloc::vec::Vec;

use execution_tape::value::{ObjHandle, Value};

use crate::RuntimeObject;
use crate::boxed;

/// Handle bit marking a nursery object. Object handles stay below
/// `2^45` so that a boxed `<object>` can carry any of them immediately.
pub(crate) const NURSERY_OBJECT_BIT: u64 = 1 << OBJECT_SERIAL_BITS;

/// Width of a nursery object's serial: the handle bits below
/// [`NURSERY_OBJECT_BIT`].
pub(crate) const OBJECT_SERIAL_BITS: u32 = 44;

/// Mask of a nursery object's serial.
const OBJECT_SERIAL_MASK: u64 = NURSERY_OBJECT_BIT - 1;

/// Which heap region an entry lives in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Space {
    /// Long-lived runtime state.
    Image = 0,
    /// The current turn's allocations.
    Nursery = 1,
}

/// Where the open turn's nursery serials start.
///
/// The nursery entry at index `i` has serial `start + i`, modulo the
/// serial width, and a nursery serial `s` names index `s - start`, modulo
/// the same width; only indices below the nursery's length resolve. When a
/// turn ends, [`Self::advance`] moves both starts past every serial it
/// used.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NurserySerials {
    /// Serial of the open nursery's first object.
    pub(crate) objects: u64,
    /// Serial of the open nursery's first heap box.
    pub(crate) values: u64,
}

impl NurserySerials {
    /// Moves past the serials of a nursery that ended holding `ended`
    /// entries, wrapping at the serial width.
    pub(crate) const fn advance(&mut self, ended: RegionMark) {
        self.objects = self.objects.wrapping_add(ended.objects as u64) & OBJECT_SERIAL_MASK;
        self.values = self.values.wrapping_add(ended.values as u64) & boxed::NURSERY_SERIAL_MASK;
    }
}

/// Location of a runtime object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectRef {
    pub(crate) space: Space,
    pub(crate) index: usize,
}

impl ObjectRef {
    /// Decodes an object handle against the open turn's `serials`, or
    /// returns `None` when it names no slot.
    ///
    /// A nursery handle always decodes; one from an earlier turn decodes to
    /// an index past the nursery's end, so the caller's bounds check
    /// rejects it.
    pub(crate) fn decode(handle: ObjHandle, serials: NurserySerials) -> Option<Self> {
        let bits = handle.0;
        if bits >> (OBJECT_SERIAL_BITS + 1) != 0 {
            return None;
        }
        if bits & NURSERY_OBJECT_BIT == 0 {
            let index = usize::try_from(bits).ok()?.checked_sub(1)?;
            return Some(Self {
                space: Space::Image,
                index,
            });
        }
        let offset = bits.wrapping_sub(serials.objects) & OBJECT_SERIAL_MASK;
        // An offset too large for `usize` is past any nursery's end too.
        Some(Self {
            space: Space::Nursery,
            index: usize::try_from(offset).unwrap_or(usize::MAX),
        })
    }

    /// Encodes the handle of this location against the open turn's
    /// `serials`, or returns `None` when the index is too large to address.
    pub(crate) fn encode(self, serials: NurserySerials) -> Option<ObjHandle> {
        let index = u64::try_from(self.index).ok()?;
        match self.space {
            Space::Image => {
                let one_based = index.checked_add(1)?;
                (one_based < NURSERY_OBJECT_BIT).then_some(ObjHandle(one_based))
            }
            Space::Nursery => (index <= OBJECT_SERIAL_MASK).then(|| {
                ObjHandle(
                    NURSERY_OBJECT_BIT | (serials.objects.wrapping_add(index) & OBJECT_SERIAL_MASK),
                )
            }),
        }
    }
}

/// One heap region: runtime objects and heap-boxed values.
#[derive(Default)]
pub(crate) struct Region {
    pub(crate) objects: Vec<RuntimeObject>,
    pub(crate) values: Vec<Value>,
}

impl Region {
    /// Drops every entry, keeping the allocations for the next turn.
    pub(crate) fn clear(&mut self) {
        self.objects.clear();
        self.values.clear();
    }

    pub(crate) fn mark(&self) -> RegionMark {
        RegionMark {
            objects: self.objects.len(),
            values: self.values.len(),
        }
    }
}

/// Length of a region at some point.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RegionMark {
    pub(crate) objects: usize,
    pub(crate) values: usize,
}

/// State of the turn in progress.
#[derive(Clone, Debug)]
pub(crate) struct OpenTurn {
    /// Image length when the turn began. Only symbols interned during the
    /// turn extend the image while it is open, so the objects past this mark
    /// are those symbols, and they stay in the image however the turn ends.
    pub(crate) image: RegionMark,
    /// Interned-symbol count when the turn began.
    pub(crate) symbols: usize,
    /// Every store the turn made into an image object, in order.
    pub(crate) write_log: Vec<LoggedStore>,
    /// Registered-root count when the turn began; roots at or past it were
    /// registered by the turn.
    pub(crate) roots: usize,
    /// Roots the turn released, with the value each held, in order.
    pub(crate) released_roots: Vec<(usize, Value)>,
}

/// One store into an image object during a turn.
///
/// The log is the turn's undo journal (an aborted turn restores `old` in
/// reverse order), its remembered set (the image slots that may now refer
/// to the nursery), and evidence of what the turn changed in the image.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoggedStore {
    /// Image object written.
    pub(crate) index: usize,
    /// Slot written.
    pub(crate) slot: usize,
    /// Value the slot held before the store.
    pub(crate) old: Value,
}

#[cfg(test)]
mod tests {
    use super::{
        NURSERY_OBJECT_BIT, NurserySerials, OBJECT_SERIAL_MASK, ObjectRef, RegionMark, Space,
    };
    use execution_tape::value::ObjHandle;

    #[test]
    fn object_handles_round_trip_per_space() {
        let starts = [0, 1, 1 << 20, OBJECT_SERIAL_MASK];
        for objects in starts {
            let serials = NurserySerials { objects, values: 0 };
            for space in [Space::Image, Space::Nursery] {
                for index in [0, 1, 41, (1 << 44) - 2] {
                    let location = ObjectRef { space, index };
                    let handle = location.encode(serials).expect("addressable");
                    assert!(handle.0 < 1 << 45);
                    assert_eq!(ObjectRef::decode(handle, serials), Some(location));
                }
            }
            // Image indices are one-based below the nursery bit; nursery
            // indices use every serial.
            let last = ObjectRef {
                space: Space::Image,
                index: (1 << 44) - 1,
            };
            assert_eq!(last.encode(serials), None);
            let last = ObjectRef {
                space: Space::Nursery,
                index: (1 << 44) - 1,
            };
            assert_eq!(
                ObjectRef::decode(last.encode(serials).expect("addressable"), serials),
                Some(last)
            );
            let past = ObjectRef {
                space: Space::Nursery,
                index: 1 << 44,
            };
            assert_eq!(past.encode(serials), None);
            assert_eq!(ObjectRef::decode(ObjHandle(0), serials), None);
            assert_eq!(ObjectRef::decode(ObjHandle(1 << 45), serials), None);
            assert_eq!(ObjectRef::decode(ObjHandle(u64::MAX), serials), None);
        }
    }

    #[test]
    fn image_handles_ignore_nursery_serials() {
        let location = ObjectRef {
            space: Space::Image,
            index: 6,
        };
        let handle = location
            .encode(NurserySerials::default())
            .expect("addressable");
        assert_eq!(handle, ObjHandle(7));
        let later = NurserySerials {
            objects: 12_345,
            values: 678,
        };
        assert_eq!(location.encode(later), Some(handle));
        assert_eq!(ObjectRef::decode(handle, later), Some(location));
    }

    #[test]
    fn nursery_handles_decode_past_the_end_after_their_turn() {
        let mut serials = NurserySerials::default();
        let first = ObjectRef {
            space: Space::Nursery,
            index: 2,
        };
        let handle = first.encode(serials).expect("addressable");
        serials.advance(RegionMark {
            objects: 3,
            values: 0,
        });
        // The same index in the next turn has a different handle, and the
        // old handle decodes to an index the next turn has not reached:
        // 2 - 3 wraps to the top of the serial range.
        assert_ne!(first.encode(serials), Some(handle));
        assert_eq!(
            ObjectRef::decode(handle, serials),
            Some(ObjectRef {
                space: Space::Nursery,
                index: (1 << 44) - 1,
            })
        );
    }

    #[test]
    fn serials_wrap_at_their_width() {
        let mut serials = NurserySerials {
            objects: OBJECT_SERIAL_MASK - 1,
            values: crate::boxed::NURSERY_SERIAL_MASK,
        };
        // A nursery that straddles the wrap still round-trips.
        for index in 0..4 {
            let location = ObjectRef {
                space: Space::Nursery,
                index,
            };
            let handle = location.encode(serials).expect("addressable");
            assert_eq!(handle.0 & NURSERY_OBJECT_BIT, NURSERY_OBJECT_BIT);
            assert_eq!(ObjectRef::decode(handle, serials), Some(location));
        }
        serials.advance(RegionMark {
            objects: 4,
            values: 3,
        });
        assert_eq!(
            serials,
            NurserySerials {
                objects: 2,
                values: 2
            }
        );
    }
}
