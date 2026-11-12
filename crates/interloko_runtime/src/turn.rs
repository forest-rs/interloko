//! Turns: bounded calls into the runtime that allocate into the nursery.
//!
//! A turn begins with [`InterlokoHost::begin_turn`]. Until it ends, every
//! object and heap box the runtime allocates goes to the nursery, except
//! interned symbols, which belong to the image. A turn ends with
//! [`InterlokoHost::finish_turn`] when it returned, or
//! [`InterlokoHost::abort_turn`] when it trapped; either way the nursery is
//! emptied, so the turn's garbage costs nothing to reclaim. An aborted turn
//! also drops the symbols it interned and undoes its stores into image
//! objects, which the turn's write log records, leaving the image exactly
//! as the turn found it: turns are transactional.

use execution_tape::value::{Obj, Value};

use crate::boxed::BoxedHandle;
use alloc::vec::Vec;

use crate::region::{LoggedStore, ObjectRef, OpenTurn, Space};
use crate::{
    HeapStats, INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE, InterlokoHost, RuntimeError,
    RuntimeObjectStorage, TurnStats,
};

impl InterlokoHost {
    /// Opens a turn. Until it ends, allocations go to the nursery.
    pub fn begin_turn(&mut self) -> Result<(), RuntimeError> {
        if self.turn.is_some() {
            return Err(RuntimeError::TurnAlreadyOpen);
        }
        debug_assert!(self.nursery.objects.is_empty() && self.nursery.values.is_empty());
        self.turn = Some(OpenTurn {
            image: self.image.mark(),
            symbols: self.symbols.len(),
            write_log: Vec::new(),
        });
        Ok(())
    }

    /// Returns whether a turn is open.
    #[must_use]
    pub const fn turn_open(&self) -> bool {
        self.turn.is_some()
    }

    /// Ends the open turn after it returned `results`.
    ///
    /// Callers must read anything they need out of nursery results first:
    /// the nursery is emptied, so nursery handles in `results` no longer
    /// resolve afterwards. The report counts what the turn allocated and
    /// what of it was still reachable from `results`.
    pub fn finish_turn(&mut self, results: &[Value]) -> Result<TurnStats, RuntimeError> {
        let Some(turn) = self.turn.take() else {
            return Err(RuntimeError::NoOpenTurn);
        };
        let stats = self.turn_stats_for(&turn, results);
        self.nursery.truncate(Default::default());
        Ok(stats)
    }

    /// Ends the open turn after it trapped, undoing its effect on the heap:
    /// stores into image objects are reverted newest first, the nursery is
    /// emptied, and symbols interned during the turn are dropped, so the
    /// image and [`InterlokoHost::heap_stats`] are as they were before the
    /// turn began.
    pub fn abort_turn(&mut self) -> Result<TurnStats, RuntimeError> {
        let Some(turn) = self.turn.take() else {
            return Err(RuntimeError::NoOpenTurn);
        };
        let stats = self.turn_stats_for(&turn, &[]);
        for store in turn.write_log.into_iter().rev() {
            if let Some(RuntimeObjectStorage::Native { slots }) = self
                .image
                .objects
                .get_mut(store.index)
                .map(|object| &mut object.storage)
                && let Some(slot) = slots.get_mut(store.slot)
            {
                *slot = store.old;
            }
        }
        self.nursery.truncate(Default::default());
        self.image.truncate(turn.image);
        self.symbols.truncate(turn.symbols);
        Ok(stats)
    }

    /// Returns the region new allocations go to: the nursery during a turn,
    /// the image otherwise.
    pub(crate) const fn allocation_space(&self) -> Space {
        if self.turn.is_some() {
            Space::Nursery
        } else {
            Space::Image
        }
    }

    /// Returns the sizes of the image, the long-lived region.
    #[must_use]
    pub fn image_stats(&self) -> HeapStats {
        self.image.stats()
    }

    fn turn_stats_for(&self, turn: &OpenTurn, results: &[Value]) -> TurnStats {
        TurnStats {
            allocated: self.nursery.stats(),
            reachable: self.reachable_stats(results),
            image: self.image.stats(),
            logged_stores: turn.write_log.len(),
        }
    }

    /// Records that a turn is about to overwrite `slot` of the image object
    /// at `index`, which currently holds `old`. Outside a turn, image stores
    /// are not logged.
    pub(crate) fn log_image_store(&mut self, index: usize, slot: usize, old: Value) {
        if let Some(turn) = &mut self.turn {
            turn.write_log.push(LoggedStore { index, slot, old });
        }
    }

    /// Returns whether `value` is, or directly boxes, a nursery entry.
    pub(crate) fn references_nursery(&self, value: &Value) -> bool {
        let Value::Obj(obj) = value else {
            return false;
        };
        if obj.host_type == INTERLOKO_OBJECT_TYPE {
            return ObjectRef::decode(obj.handle).is_some_and(|at| at.space == Space::Nursery);
        }
        if obj.host_type != INTERLOKO_VALUE_TYPE {
            return false;
        }
        match BoxedHandle::decode(obj.handle) {
            BoxedHandle::Heap(space, _) => space == Space::Nursery,
            BoxedHandle::Object(handle) => self.references_nursery(&Value::Obj(Obj {
                host_type: INTERLOKO_OBJECT_TYPE,
                handle,
            })),
            BoxedHandle::Float(_)
            | BoxedHandle::Integer(_)
            | BoxedHandle::Bool(_)
            | BoxedHandle::Invalid => false,
        }
    }
}
