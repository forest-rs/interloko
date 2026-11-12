//! Evacuation: moving a finished turn's survivors from the nursery into the
//! image.
//!
//! Survivors are the nursery entries reachable from the turn's roots. They
//! are appended to the image in increasing nursery index order, the same
//! rule `execution_tape` uses when it merges a run's aggregates into a base
//! heap, so the image a turn produces depends only on what the turn did,
//! never on traversal order. Every reference to a survivor (in other
//! survivors, in the roots, and in the image slots the write log names) is
//! then rewritten to its image handle, and the nursery is dropped whole.
//!
//! The cost is charged in work units, checked against the host's
//! evacuation budget before anything changes: one per evacuated entry, one
//! per value it holds, and one per root.

use alloc::vec;
use alloc::vec::Vec;
use core::mem;

use execution_tape::value::{Obj, Value};

use crate::boxed::BoxedHandle;
use crate::census::{boxed_value_bytes, object_bytes, object_edges};
use crate::region::{ObjectRef, OpenTurn, Space};
use crate::{
    HeapStats, INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE, InterlokoHost, RuntimeError,
    RuntimeObjectStorage,
};

/// Outcome of evacuating one turn.
pub(crate) struct Evacuation {
    pub(crate) promoted: HeapStats,
    pub(crate) work: u64,
}

/// A nursery entry a value refers to directly.
#[derive(Copy, Clone)]
enum NurseryEntry {
    Object(usize),
    Box(usize),
}

/// Returns the nursery entry `value` refers to, if any.
fn nursery_entry(value: &Value) -> Option<NurseryEntry> {
    let Value::Obj(obj) = value else {
        return None;
    };
    if obj.host_type == INTERLOKO_OBJECT_TYPE {
        let at = ObjectRef::decode(obj.handle)?;
        return (at.space == Space::Nursery).then_some(NurseryEntry::Object(at.index));
    }
    if obj.host_type != INTERLOKO_VALUE_TYPE {
        return None;
    }
    match BoxedHandle::decode(obj.handle) {
        BoxedHandle::Heap(Space::Nursery, index) => Some(NurseryEntry::Box(index)),
        BoxedHandle::Object(handle) => nursery_entry(&Value::Obj(Obj {
            host_type: INTERLOKO_OBJECT_TYPE,
            handle,
        })),
        _ => None,
    }
}

/// New image indices for evacuated nursery entries.
struct Remap {
    objects: Vec<Option<usize>>,
    values: Vec<Option<usize>>,
}

impl Remap {
    /// Rewrites a reference to a nursery survivor into its image handle.
    fn value(&self, value: &mut Value) {
        let Value::Obj(obj) = value else {
            return;
        };
        if obj.host_type == INTERLOKO_OBJECT_TYPE {
            if let Some(handle) = self.object_handle(obj.handle) {
                obj.handle = handle;
            }
            return;
        }
        if obj.host_type != INTERLOKO_VALUE_TYPE {
            return;
        }
        match BoxedHandle::decode(obj.handle) {
            BoxedHandle::Heap(Space::Nursery, index) => {
                if let Some(handle) = self
                    .values
                    .get(index)
                    .copied()
                    .flatten()
                    .and_then(|image| BoxedHandle::heap(Space::Image, image))
                {
                    obj.handle = handle;
                }
            }
            BoxedHandle::Object(handle) => {
                if let Some(handle) = self
                    .object_handle(handle)
                    .and_then(BoxedHandle::immediate_object)
                {
                    obj.handle = handle;
                }
            }
            _ => {}
        }
    }

    fn object_handle(
        &self,
        handle: execution_tape::value::ObjHandle,
    ) -> Option<execution_tape::value::ObjHandle> {
        let at = ObjectRef::decode(handle)?;
        if at.space != Space::Nursery {
            return None;
        }
        let index = self.objects.get(at.index).copied().flatten()?;
        ObjectRef {
            space: Space::Image,
            index,
        }
        .encode()
    }
}

impl InterlokoHost {
    /// Returns the turn's evacuation roots other than the values a caller
    /// keeps: roots it registered, and the current contents of the image
    /// slots it stored into.
    pub(crate) fn turn_roots(&self, turn: &OpenTurn) -> Vec<Value> {
        let registered = self.roots[turn.roots..].iter().flatten().cloned();
        let logged = turn.write_log.iter().filter_map(|store| {
            match &self.image.objects.get(store.index)?.storage {
                RuntimeObjectStorage::Native { slots } => slots.get(store.slot).cloned(),
                _ => None,
            }
        });
        registered.chain(logged).collect()
    }

    /// Evacuates the survivors of `turn` into the image and empties the
    /// nursery, rewriting `keep`, the turn's registered roots and its logged
    /// image slots to image handles.
    pub(crate) fn evacuate(
        &mut self,
        turn: &OpenTurn,
        keep: &mut [Value],
    ) -> Result<Evacuation, RuntimeError> {
        let mut roots = self.turn_roots(turn);
        roots.extend(keep.iter().cloned());

        // Mark the nursery entries reachable from the roots. Image entries
        // are not traversed: an image object refers to the nursery only
        // through a slot the write log names, and those slots are roots.
        let mut object_marks = vec![false; self.nursery.objects.len()];
        let mut value_marks = vec![false; self.nursery.values.len()];
        let mut work = u64::try_from(roots.len()).unwrap_or(u64::MAX);
        let mut pending: Vec<NurseryEntry> = roots.iter().filter_map(nursery_entry).collect();
        while let Some(entry) = pending.pop() {
            match entry {
                NurseryEntry::Object(index) => {
                    let Some(seen) = object_marks.get_mut(index) else {
                        continue;
                    };
                    if mem::replace(seen, true) {
                        continue;
                    }
                    let object = &self.nursery.objects[index];
                    for edge in object_edges(object) {
                        work = work.saturating_add(1);
                        pending.extend(nursery_entry(edge));
                    }
                }
                NurseryEntry::Box(index) => {
                    let Some(seen) = value_marks.get_mut(index) else {
                        continue;
                    };
                    if mem::replace(seen, true) {
                        continue;
                    }
                    work = work.saturating_add(1);
                    pending.extend(nursery_entry(&self.nursery.values[index]));
                }
            }
            work = work.saturating_add(1);
        }
        if work > self.evacuation_budget {
            return Err(RuntimeError::EvacuationBudgetExceeded {
                work,
                budget: self.evacuation_budget,
            });
        }

        // Assign image indices in increasing nursery index order.
        let assign = |marks: &[bool], base: usize| {
            let mut next = base;
            marks
                .iter()
                .map(|marked| {
                    marked.then(|| {
                        next += 1;
                        next - 1
                    })
                })
                .collect::<Vec<_>>()
        };
        let remap = Remap {
            objects: assign(&object_marks, self.image.objects.len()),
            values: assign(&value_marks, self.image.values.len()),
        };

        let mut promoted = HeapStats::default();
        let nursery = mem::take(&mut self.nursery);
        for (mut object, marked) in nursery.objects.into_iter().zip(object_marks) {
            if !marked {
                continue;
            }
            match &mut object.storage {
                RuntimeObjectStorage::Native { slots } => {
                    slots.iter_mut().for_each(|value| remap.value(value));
                }
                RuntimeObjectStorage::Vector { elements } => {
                    elements.iter_mut().for_each(|value| remap.value(value));
                }
                _ => {}
            }
            promoted.objects += 1;
            promoted.bytes += object_bytes(&object);
            self.image.objects.push(object);
        }
        for (mut value, marked) in nursery.values.into_iter().zip(value_marks) {
            if !marked {
                continue;
            }
            remap.value(&mut value);
            promoted.boxed_values += 1;
            promoted.bytes += boxed_value_bytes();
            self.image.values.push(value);
        }

        keep.iter_mut().for_each(|value| remap.value(value));
        self.roots[turn.roots..]
            .iter_mut()
            .flatten()
            .for_each(|value| remap.value(value));
        for store in &turn.write_log {
            if let Some(RuntimeObjectStorage::Native { slots }) = self
                .image
                .objects
                .get_mut(store.index)
                .map(|object| &mut object.storage)
                && let Some(slot) = slots.get_mut(store.slot)
            {
                remap.value(slot);
            }
        }
        Ok(Evacuation { promoted, work })
    }
}
