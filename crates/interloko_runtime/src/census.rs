//! Heap census: how much a host's heaps hold, and how much of it is still
//! reachable from a set of roots.
//!
//! Sizes are logical, deterministic estimates: the fixed size of each heap
//! entry plus the length (not capacity) of its out-of-line payload. They are
//! meant for comparing turns and spotting growth, not for accounting RSS.

use core::mem::size_of;

use execution_tape::value::{Obj, Value};

use crate::boxed::BoxedHandle;
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

/// Heap accounting for one turn: one call into the runtime.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TurnStats {
    /// Everything the turn allocated.
    pub allocated: HeapStats,
    /// What is still reachable when the turn ends, from its results and the
    /// long-lived roots (interned symbols).
    pub reachable: HeapStats,
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

impl InterlokoHost {
    /// Returns the current sizes of this host's heaps.
    #[must_use]
    pub fn heap_stats(&self) -> HeapStats {
        HeapStats {
            objects: self.objects.len(),
            boxed_values: self.values.len(),
            bytes: self.objects.iter().map(object_bytes).sum::<usize>()
                + self.values.len() * boxed_value_bytes(),
        }
    }

    /// Returns the sizes of the heap entries reachable from `roots` and from
    /// the host's long-lived roots (interned symbols).
    #[must_use]
    pub fn reachable_stats(&self, roots: &[Value]) -> HeapStats {
        let mut objects = alloc::vec![false; self.objects.len()];
        let mut boxes = alloc::vec![false; self.values.len()];
        let mut pending: alloc::vec::Vec<Value> = roots.to_vec();
        pending.extend(self.symbols.iter().map(|entry| Value::Obj(entry.object)));
        while let Some(value) = pending.pop() {
            let Value::Obj(obj) = value else {
                continue;
            };
            if obj.host_type == INTERLOKO_VALUE_TYPE {
                match BoxedHandle::decode(obj.handle) {
                    BoxedHandle::Heap(index) => {
                        if let Some(seen) = boxes.get_mut(index)
                            && !*seen
                        {
                            *seen = true;
                            pending.push(self.values[index].clone());
                        }
                    }
                    BoxedHandle::Object(handle) => pending.push(Value::Obj(Obj {
                        host_type: INTERLOKO_OBJECT_TYPE,
                        handle,
                    })),
                    BoxedHandle::Float(_)
                    | BoxedHandle::Integer(_)
                    | BoxedHandle::Bool(_)
                    | BoxedHandle::Invalid => {}
                }
                continue;
            }
            if obj.host_type != INTERLOKO_OBJECT_TYPE {
                continue;
            }
            let Some(index) = usize::try_from(obj.handle.0)
                .ok()
                .and_then(|raw| raw.checked_sub(1))
            else {
                continue;
            };
            let Some(seen) = objects.get_mut(index) else {
                continue;
            };
            if *seen {
                continue;
            }
            *seen = true;
            match &self.objects[index].storage {
                RuntimeObjectStorage::Native { slots } => pending.extend(slots.iter().cloned()),
                RuntimeObjectStorage::Vector { elements } => {
                    pending.extend(elements.iter().cloned());
                }
                _ => {}
            }
        }
        let reached = |marks: &[bool]| marks.iter().filter(|seen| **seen).count();
        HeapStats {
            objects: reached(&objects),
            boxed_values: reached(&boxes),
            bytes: self
                .objects
                .iter()
                .zip(&objects)
                .filter(|(_, seen)| **seen)
                .map(|(object, _)| object_bytes(object))
                .sum::<usize>()
                + reached(&boxes) * boxed_value_bytes(),
        }
    }
}
