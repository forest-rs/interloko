//! Heap census: how much a host's heaps hold, and how much of it is still
//! reachable from a set of roots.
//!
//! Sizes are logical, deterministic estimates: the fixed size of each heap
//! entry plus the length (not capacity) of its out-of-line payload. They are
//! meant for comparing turns and spotting growth, not for accounting RSS.

use alloc::vec;
use alloc::vec::Vec;
use core::mem::size_of;

use execution_tape::value::{Obj, Value};

use crate::boxed::BoxedHandle;
use crate::region::{ObjectRef, Region, Space};
use crate::{
    INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE, InterlokoHost, RuntimeObject, RuntimeObjectStorage,
};

/// Entry counts and estimated payload bytes of a runtime host's heaps.
///
/// Values boxed as immediates (see [`INTERLOKO_VALUE_TYPE`]) never allocate
/// and are not counted.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct HeapStats {
    /// Runtime objects: instances, strings, symbols, vectors, metaobjects,
    /// and host-backed objects.
    pub objects: usize,
    /// Boxed `<object>` values stored on the heap rather than in a handle.
    pub boxed_values: usize,
    /// Estimated bytes held by those entries and their payloads.
    pub bytes: usize,
}

/// Heap accounting for one turn: one bounded call into the runtime.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TurnStats {
    /// Everything the turn allocated in the nursery. The nursery only grows
    /// during a turn, so this is also its high-water mark.
    pub allocated: HeapStats,
    /// The part of the nursery reachable when the turn ended, from its
    /// results, the values the caller keeps, and the image (roots it
    /// registered, slots it stored into).
    pub reachable: HeapStats,
    /// What evacuation moved into the image: the survivors of the turn's
    /// roots, which exclude results the caller did not keep. Always within
    /// `reachable`; equal to it when every result is kept.
    pub promoted: HeapStats,
    /// Work units evacuation charged against the host's budget.
    pub evacuation_work: u64,
    /// The image after the turn.
    pub image: HeapStats,
    /// Stores the turn made into image objects: the length of its write
    /// log.
    pub logged_stores: usize,
}

impl TurnStats {
    /// Returns the fraction of allocated bytes still reachable at turn end,
    /// or `None` when the turn allocated nothing.
    #[must_use]
    pub fn survival_ratio(&self) -> Option<f64> {
        if self.allocated.bytes == 0 {
            return None;
        }
        // A ratio of byte estimates does not need integer precision.
        let ratio = self.reachable.bytes as f64 / self.allocated.bytes as f64;
        Some(ratio)
    }
}

/// Returns the estimated bytes of one runtime object.
pub(crate) fn object_bytes(object: &RuntimeObject) -> usize {
    let payload = match &object.storage {
        RuntimeObjectStorage::Native { slots } => slots.len() * size_of::<Value>(),
        RuntimeObjectStorage::String { value } => value.len(),
        RuntimeObjectStorage::Symbol { name } => name.len(),
        RuntimeObjectStorage::Vector { elements } => elements.len() * size_of::<Value>(),
        RuntimeObjectStorage::ClassMeta { .. }
        | RuntimeObjectStorage::GenericMeta { .. }
        | RuntimeObjectStorage::MethodMeta { .. }
        | RuntimeObjectStorage::SlotDescriptor { .. }
        | RuntimeObjectStorage::HostBacked { .. } => 0,
    };
    size_of::<RuntimeObject>() + payload
}

/// Returns the estimated bytes of one heap-boxed value.
pub(crate) const fn boxed_value_bytes() -> usize {
    size_of::<Value>()
}

impl Region {
    /// Returns the sizes of this region's entries.
    pub(crate) fn stats(&self) -> HeapStats {
        HeapStats {
            objects: self.objects.len(),
            boxed_values: self.values.len(),
            bytes: self.objects.iter().map(object_bytes).sum::<usize>()
                + self.values.len() * boxed_value_bytes(),
        }
    }
}

impl InterlokoHost {
    /// Returns the current sizes of this host's heaps, image and nursery
    /// together.
    #[must_use]
    pub fn heap_stats(&self) -> HeapStats {
        let (image, nursery) = (self.image.stats(), self.nursery.stats());
        HeapStats {
            objects: image.objects + nursery.objects,
            boxed_values: image.boxed_values + nursery.boxed_values,
            bytes: image.bytes + nursery.bytes,
        }
    }

    /// Returns the sizes of the nursery entries reachable from `roots`.
    ///
    /// The walk follows image objects reached from the roots too, since an
    /// image object can refer to the nursery, but counts only nursery
    /// entries: the ones that would have to survive the turn.
    #[must_use]
    pub fn reachable_stats(&self, roots: &[Value]) -> HeapStats {
        let marks = self.mark_reachable(roots);
        let nursery = &marks[Space::Nursery as usize];
        let reached = |marks: &[bool]| marks.iter().filter(|seen| **seen).count();
        HeapStats {
            objects: reached(&nursery.objects),
            boxed_values: reached(&nursery.values),
            bytes: self
                .nursery
                .objects
                .iter()
                .zip(&nursery.objects)
                .filter(|(_, seen)| **seen)
                .map(|(object, _)| object_bytes(object))
                .sum::<usize>()
                + reached(&nursery.values) * boxed_value_bytes(),
        }
    }

    /// Marks every heap entry reachable from `roots`, per region (indexed by
    /// [`Space`]).
    pub(crate) fn mark_reachable(&self, roots: &[Value]) -> [RegionMarks; 2] {
        let mut marks = [Space::Image, Space::Nursery].map(|space| {
            let region = self.region(space);
            RegionMarks {
                objects: vec![false; region.objects.len()],
                values: vec![false; region.values.len()],
            }
        });
        let mut pending: Vec<Value> = roots.to_vec();
        while let Some(value) = pending.pop() {
            let Value::Obj(obj) = value else {
                continue;
            };
            if obj.host_type == INTERLOKO_VALUE_TYPE {
                match BoxedHandle::decode(obj.handle) {
                    BoxedHandle::Heap(space, index) => {
                        if let Some(seen) = marks[space as usize].values.get_mut(index)
                            && !*seen
                        {
                            *seen = true;
                            pending.push(self.region(space).values[index].clone());
                        }
                    }
                    BoxedHandle::Object(handle) => pending.push(Value::Obj(Obj {
                        host_type: INTERLOKO_OBJECT_TYPE,
                        handle,
                    })),
                    BoxedHandle::Float(_)
                    | BoxedHandle::SingleFloat(_)
                    | BoxedHandle::Integer(_)
                    | BoxedHandle::Bool(_)
                    | BoxedHandle::Invalid => {}
                }
                continue;
            }
            if obj.host_type != INTERLOKO_OBJECT_TYPE {
                continue;
            }
            let Some(at) = ObjectRef::decode(obj.handle) else {
                continue;
            };
            let Some(seen) = marks[at.space as usize].objects.get_mut(at.index) else {
                continue;
            };
            if *seen {
                continue;
            }
            *seen = true;
            pending.extend(object_edges(&self.region(at.space).objects[at.index]).cloned());
        }
        marks
    }
}

/// Reachability marks for one region.
pub(crate) struct RegionMarks {
    pub(crate) objects: Vec<bool>,
    pub(crate) values: Vec<bool>,
}

/// Returns the values an object refers to.
pub(crate) fn object_edges(object: &RuntimeObject) -> impl Iterator<Item = &Value> {
    let edges: &[Value] = match &object.storage {
        RuntimeObjectStorage::Native { slots } => slots,
        RuntimeObjectStorage::Vector { elements } => elements,
        _ => &[],
    };
    edges.iter()
}
