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
//! Handles are decoded against the finishing turn's nursery serials, so a
//! stale nursery handle among the roots (one kept from an earlier turn)
//! names no survivor and is left as it is: it stays stale rather than being
//! rewritten to the image handle of an unrelated object.
//!
//! The cost is charged in work units, checked against the host's
//! evacuation budget before anything changes: one per evacuated entry, one
//! per value it holds, and one per root.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec;
use alloc::vec::Vec;
use core::mem;
use core::ops::Range;

use execution_tape::value::{Obj, Value};

use crate::boxed::BoxedHandle;
use crate::census::{boxed_value_bytes, object_bytes, object_edges};
use crate::collection::{Table, TableUndo, identity_handle};
use crate::region::{LoggedStore, NurserySerials, ObjectRef, OpenTurn, Space};
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

/// Returns the nursery entry `value` refers to, if any. The index of a
/// stale handle is past the nursery's end; callers bounds-check it.
fn nursery_entry(value: &Value, serials: NurserySerials) -> Option<NurseryEntry> {
    let Value::Obj(obj) = value else {
        return None;
    };
    if obj.host_type == INTERLOKO_OBJECT_TYPE {
        let at = ObjectRef::decode(obj.handle, serials)?;
        return (at.space == Space::Nursery).then_some(NurseryEntry::Object(at.index));
    }
    if obj.host_type != INTERLOKO_VALUE_TYPE {
        return None;
    }
    match BoxedHandle::decode(obj.handle, serials) {
        BoxedHandle::Heap(Space::Nursery, index) => Some(NurseryEntry::Box(index)),
        BoxedHandle::Object(handle) => nursery_entry(
            &Value::Obj(Obj {
                host_type: INTERLOKO_OBJECT_TYPE,
                handle,
            }),
            serials,
        ),
        _ => None,
    }
}

/// New image indices for evacuated nursery entries.
struct Remap {
    objects: Vec<Option<usize>>,
    values: Vec<Option<usize>>,
    /// The finishing turn's serials, against which nursery handles decode.
    serials: NurserySerials,
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
        match BoxedHandle::decode(obj.handle, self.serials) {
            BoxedHandle::Heap(Space::Nursery, index) => {
                if let Some(handle) = self
                    .values
                    .get(index)
                    .copied()
                    .flatten()
                    .and_then(|image| BoxedHandle::heap(Space::Image, image, self.serials))
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
        let at = ObjectRef::decode(handle, self.serials)?;
        if at.space != Space::Nursery {
            return None;
        }
        let index = self.objects.get(at.index).copied().flatten()?;
        ObjectRef {
            space: Space::Image,
            index,
        }
        .encode(self.serials)
    }
}

impl Remap {
    /// Rewrites the references to nursery survivors among the values at
    /// `place` of an object's storage ([`stored_values`]). A table whose
    /// identity key moved is reindexed under the key's new handle.
    fn stored(&self, storage: &mut RuntimeObjectStorage, place: Range<usize>) {
        match storage {
            RuntimeObjectStorage::Native { slots: values }
            | RuntimeObjectStorage::Vector {
                elements: values, ..
            } => {
                let end = place.end.min(values.len());
                values
                    .get_mut(place.start.min(end)..end)
                    .into_iter()
                    .flatten()
                    .for_each(|value| self.value(value));
            }
            RuntimeObjectStorage::Table(table) => {
                let end = place.end.min(table.slots.len());
                for slot in place.start.min(end)..end {
                    let old = identity_handle(&table.slots[slot]);
                    self.value(&mut table.slots[slot]);
                    // Even slots hold keys.
                    if slot % 2 == 0
                        && let (Some(old), Some(new)) = (old, identity_handle(&table.slots[slot]))
                    {
                        table.rekey(slot / 2, old, new);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The values an object's storage holds, which may refer to other heap
/// entries: a native object's slots, a vector's elements, or a table's
/// keys and values, in its entry order.
pub(crate) fn stored_values(storage: &RuntimeObjectStorage) -> Option<&[Value]> {
    match storage {
        RuntimeObjectStorage::Native { slots: values }
        | RuntimeObjectStorage::Vector {
            elements: values, ..
        } => Some(values),
        RuntimeObjectStorage::Table(table) => Some(&table.slots),
        _ => None,
    }
}

/// The places of image objects a turn's logged stores may have made refer
/// to the nursery, as ranges of [`stored_values`] indices, each once.
///
/// A vector that grew contributes everything past the shortest length it
/// grew from, so many additions to one vector cost one range, not one per
/// addition; a table compacted during the turn contributes all of itself.
fn logged_places(write_log: &[LoggedStore]) -> Vec<(usize, Range<usize>)> {
    let mut slots = BTreeSet::new();
    let mut grown = BTreeMap::<usize, usize>::new();
    for store in write_log {
        match store {
            LoggedStore::Slot { index, slot, .. } => {
                slots.insert((*index, *slot));
            }
            LoggedStore::Grew { index, len } => {
                let from = grown.entry(*index).or_insert(*len);
                *from = (*from).min(*len);
            }
            // A compaction moves every entry, so earlier logged positions
            // no longer say where the turn's entries are: the whole table
            // is a root.
            LoggedStore::Table {
                index,
                undo: TableUndo::Compacted(_),
            } => {
                grown.insert(*index, 0);
            }
            LoggedStore::Table { index, undo } => {
                if let Some(position) = Table::logged_position(undo) {
                    slots.insert((*index, 2 * position));
                    slots.insert((*index, 2 * position + 1));
                }
            }
        }
    }
    slots
        .into_iter()
        .map(|(index, slot)| (index, slot..slot + 1))
        .chain(
            grown
                .into_iter()
                .map(|(index, from)| (index, from..usize::MAX)),
        )
        .collect()
}

impl InterlokoHost {
    /// Returns the turn's evacuation roots other than the values a caller
    /// keeps: roots it registered, and the current contents of the image
    /// slots it stored into.
    pub(crate) fn turn_roots(&self, turn: &OpenTurn) -> Vec<Value> {
        let mut roots: Vec<Value> = self.roots[turn.roots..].iter().flatten().cloned().collect();
        for (index, place) in logged_places(&turn.write_log) {
            let Some(values) = self
                .image
                .objects
                .get(index)
                .and_then(|object| stored_values(&object.storage))
            else {
                continue;
            };
            let end = place.end.min(values.len());
            roots.extend(values[place.start.min(end)..end].iter().cloned());
        }
        roots
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
        let serials = self.nursery_serials;
        let mut pending: Vec<NurseryEntry> = roots
            .iter()
            .filter_map(|root| nursery_entry(root, serials))
            .collect();
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
                        pending.extend(nursery_entry(edge, serials));
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
                    pending.extend(nursery_entry(&self.nursery.values[index], serials));
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
            serials,
        };

        let mut promoted = HeapStats::default();
        let nursery = mem::take(&mut self.nursery);
        for (mut object, marked) in nursery.objects.into_iter().zip(object_marks) {
            if !marked {
                continue;
            }
            let len = stored_values(&object.storage).map_or(0, <[Value]>::len);
            remap.stored(&mut object.storage, 0..len);
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
        for (index, place) in logged_places(&turn.write_log) {
            let Some(storage) = self
                .image
                .objects
                .get_mut(index)
                .map(|object| &mut object.storage)
            else {
                continue;
            };
            remap.stored(storage, place);
        }
        Ok(Evacuation { promoted, work })
    }
}
