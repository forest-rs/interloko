//! Turns: bounded calls into the runtime that allocate into the nursery.
//!
//! A turn begins with [`InterlokoHost::begin_turn`]. Until it ends, every
//! object and heap box the runtime allocates goes to the nursery, except
//! interned symbols, which belong to the image. A turn ends with
//! [`InterlokoHost::finish_turn`] when it returned, or
//! [`InterlokoHost::abort_turn`] when it trapped; either way the nursery is
//! emptied, so the turn's garbage costs nothing to reclaim. An aborted turn
//! also drops the symbols it interned, leaving the image exactly as the turn
//! found it.

use execution_tape::value::{Obj, Value};

use crate::boxed::BoxedHandle;
use crate::region::{ObjectRef, OpenTurn, Space};
use crate::{
    HeapStats, INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE, InterlokoHost, RuntimeError, TurnStats,
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
        if self.turn.is_none() {
            return Err(RuntimeError::NoOpenTurn);
        }
        let stats = self.turn_stats(results);
        self.nursery.truncate(Default::default());
        self.turn = None;
        Ok(stats)
    }

    /// Ends the open turn after it trapped, undoing its effect on the heap:
    /// the nursery is emptied and symbols interned during the turn are
    /// dropped, so [`InterlokoHost::heap_stats`] is as it was before the
    /// turn began.
    pub fn abort_turn(&mut self) -> Result<TurnStats, RuntimeError> {
        let Some(turn) = self.turn.clone() else {
            return Err(RuntimeError::NoOpenTurn);
        };
        let stats = self.turn_stats(&[]);
        self.nursery.truncate(Default::default());
        self.image.truncate(turn.image);
        self.symbols.truncate(turn.symbols);
        self.turn = None;
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

    fn turn_stats(&self, results: &[Value]) -> TurnStats {
        TurnStats {
            allocated: self.nursery.stats(),
            reachable: self.reachable_stats(results),
            image: self.image.stats(),
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
