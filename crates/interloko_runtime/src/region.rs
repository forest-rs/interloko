//! The runtime host's two heap regions and the turns that use them.
//!
//! The **image** holds long-lived runtime state: objects created outside a
//! turn (an embedding's setup, host-backed objects), interned symbols, and
//! (once evacuation lands) turn survivors. The **nursery** holds everything
//! a turn allocates: instances, strings, vectors, metaobjects and heap
//! boxes. A turn is one bounded call into the runtime; when it ends the
//! nursery is emptied in one step, so a turn's garbage costs nothing to
//! reclaim.
//!
//! Object handles say which region they index. An image object's handle is
//! its one-based index; a nursery object's handle also sets
//! [`NURSERY_OBJECT_BIT`]. Heap-boxed values use the boxed-handle kinds `00`
//! (image) and `11` (nursery); see [`crate::boxed`].

use alloc::vec::Vec;

use execution_tape::value::{ObjHandle, Value};

use crate::RuntimeObject;

/// Handle bit marking a nursery object. Object handles stay below
/// `2^45` so that a boxed `<object>` can carry any of them immediately.
pub(crate) const NURSERY_OBJECT_BIT: u64 = 1 << 44;

/// Which heap region an entry lives in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Space {
    /// Long-lived runtime state.
    Image = 0,
    /// The current turn's allocations.
    Nursery = 1,
}

/// Location of a runtime object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectRef {
    pub(crate) space: Space,
    pub(crate) index: usize,
}

impl ObjectRef {
    /// Decodes an object handle, or `None` when it names no slot.
    pub(crate) fn decode(handle: ObjHandle) -> Option<Self> {
        let (space, one_based) = if handle.0 & NURSERY_OBJECT_BIT == 0 {
            (Space::Image, handle.0)
        } else {
            (Space::Nursery, handle.0 & !NURSERY_OBJECT_BIT)
        };
        if one_based >= NURSERY_OBJECT_BIT {
            return None;
        }
        let index = usize::try_from(one_based).ok()?.checked_sub(1)?;
        Some(Self { space, index })
    }

    /// Encodes the handle of this location, or `None` when the index is too
    /// large to address.
    pub(crate) fn encode(self) -> Option<ObjHandle> {
        let one_based = u64::try_from(self.index).ok()?.checked_add(1)?;
        if one_based >= NURSERY_OBJECT_BIT {
            return None;
        }
        Some(ObjHandle(match self.space {
            Space::Image => one_based,
            Space::Nursery => NURSERY_OBJECT_BIT | one_based,
        }))
    }
}

/// One heap region: runtime objects and heap-boxed values.
#[derive(Default)]
pub(crate) struct Region {
    pub(crate) objects: Vec<RuntimeObject>,
    pub(crate) values: Vec<Value>,
}

impl Region {
    pub(crate) fn truncate(&mut self, mark: RegionMark) {
        self.objects.truncate(mark.objects);
        self.values.truncate(mark.values);
    }

    pub(crate) fn mark(&self) -> RegionMark {
        RegionMark {
            objects: self.objects.len(),
            values: self.values.len(),
        }
    }
}

/// Length of a region at some point, for rolling back to it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RegionMark {
    pub(crate) objects: usize,
    pub(crate) values: usize,
}

/// State of the turn in progress.
#[derive(Clone, Debug)]
pub(crate) struct OpenTurn {
    /// Image length when the turn began. Only symbols interned during the
    /// turn extend the image while it is open.
    pub(crate) image: RegionMark,
    /// Interned-symbol count when the turn began.
    pub(crate) symbols: usize,
    /// Every store the turn made into an image object, in order.
    pub(crate) write_log: Vec<LoggedStore>,
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
    use super::{NURSERY_OBJECT_BIT, ObjectRef, Space};
    use execution_tape::value::ObjHandle;

    #[test]
    fn object_handles_round_trip_per_space() {
        for space in [Space::Image, Space::Nursery] {
            for index in [0, 1, 41, (1 << 44) - 2] {
                let location = ObjectRef { space, index };
                let handle = location.encode().expect("addressable");
                assert!(handle.0 < 1 << 45);
                assert_eq!(ObjectRef::decode(handle), Some(location));
            }
            assert_eq!(
                ObjectRef {
                    space,
                    index: (1 << 44) - 1
                }
                .encode(),
                None
            );
        }
        assert_eq!(ObjectRef::decode(ObjHandle(0)), None);
        assert_eq!(ObjectRef::decode(ObjHandle(NURSERY_OBJECT_BIT)), None);
    }
}
