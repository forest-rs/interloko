//! Turns: bounded, transactional calls into the runtime.
//!
//! A turn begins with [`InterlokoHost::begin_turn`]. Until it ends, every
//! object and heap box the runtime allocates goes to the nursery, except
//! interned symbols, which belong to the image. Stores into image objects
//! are recorded in the turn's write log.
//!
//! A turn that returned ends with [`InterlokoHost::finish_turn`]: the
//! nursery entries reachable from the turn's roots (values the caller keeps,
//! image roots it registered, and the image slots its write log names) are
//! evacuated into the image, and the rest of the nursery is dropped in one
//! step. A turn that trapped ends with [`InterlokoHost::abort_turn`]: its
//! image stores and root changes are undone, the symbols it interned are
//! dropped and the nursery is emptied, leaving the image exactly as the turn
//! found it.
//!
//! Embedding state that must outlive turns (a break loop's held values,
//! condition handlers kept across turns) registers itself explicitly with
//! [`InterlokoHost::register_root`].

use alloc::vec::Vec;

use execution_tape::value::Value;

use crate::region::{LoggedStore, OpenTurn, Space};
use crate::{HeapStats, InterlokoHost, RuntimeError, RuntimeObjectStorage, TurnStats};

/// Identifier of a value registered as an image root.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageRootId(
    /// Zero-based index in registration order.
    pub u32,
);

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
            roots: self.roots.len(),
            released_roots: Vec::new(),
        });
        Ok(())
    }

    /// Returns whether a turn is open.
    #[must_use]
    pub const fn turn_open(&self) -> bool {
        self.turn.is_some()
    }

    /// Sets the most evacuation work (see [`TurnStats::evacuation_work`])
    /// a turn may need to finish. The default is unbounded.
    pub const fn set_evacuation_budget(&mut self, budget: u64) {
        self.evacuation_budget = budget;
    }

    /// Ends the open turn after it returned.
    ///
    /// `results` are the values the turn returned; the report measures what
    /// is reachable from them, but they are not kept. `keep` are values the
    /// caller holds past the turn: they are evacuated with every other
    /// survivor and rewritten in place to their image handles. Any other
    /// nursery handle dangles once the turn ends and must not be used again:
    /// it fails to resolve until a later turn allocates at the same index,
    /// and then names that turn's entry.
    ///
    /// When the evacuation would exceed the budget, nothing changes, the turn
    /// stays open, and the caller should abort it.
    pub fn finish_turn(
        &mut self,
        results: &[Value],
        keep: &mut [Value],
    ) -> Result<TurnStats, RuntimeError> {
        let Some(turn) = self.turn.take() else {
            return Err(RuntimeError::NoOpenTurn);
        };
        let mut stats = self.turn_stats_for(&turn, &[results, keep].concat());
        match self.evacuate(&turn, keep) {
            Ok(evacuation) => {
                stats.promoted = evacuation.promoted;
                stats.evacuation_work = evacuation.work;
                stats.image = self.image.stats();
                Ok(stats)
            }
            Err(error) => {
                self.turn = Some(turn);
                Err(error)
            }
        }
    }

    /// Ends the open turn after it trapped, undoing its effect on the heap:
    /// stores into image objects and root releases are reverted newest
    /// first, roots registered by the turn are dropped, the nursery is
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
        self.roots.truncate(turn.roots);
        for (index, value) in turn.released_roots.into_iter().rev() {
            if let Some(root) = self.roots.get_mut(index) {
                *root = Some(value);
            }
        }
        self.nursery.truncate(Default::default());
        self.image.truncate(turn.image);
        self.symbols.truncate(turn.symbols);
        Ok(stats)
    }

    /// Registers `value` as an image root: it, and everything it reaches,
    /// survives every turn until the root is released. A root registered
    /// during a turn is evacuated when the turn finishes and dropped if it
    /// aborts.
    pub fn register_root(&mut self, value: Value) -> ImageRootId {
        let id = ImageRootId(u32::try_from(self.roots.len()).unwrap_or(u32::MAX));
        self.roots.push(Some(value));
        id
    }

    /// Returns the current value of an image root.
    pub fn image_root(&self, root: ImageRootId) -> Result<&Value, RuntimeError> {
        self.roots
            .get(root.0 as usize)
            .and_then(Option::as_ref)
            .ok_or(RuntimeError::InvalidImageRoot { root })
    }

    /// Releases an image root. What it alone kept alive stays in the image
    /// until a future compaction; an aborted turn restores the root.
    pub fn release_root(&mut self, root: ImageRootId) -> Result<Value, RuntimeError> {
        let index = root.0 as usize;
        let value = self
            .roots
            .get_mut(index)
            .and_then(Option::take)
            .ok_or(RuntimeError::InvalidImageRoot { root })?;
        if let Some(turn) = &mut self.turn
            && index < turn.roots
        {
            turn.released_roots.push((index, value.clone()));
        }
        Ok(value)
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
            reachable: self.reachable_stats(&[results, &self.turn_roots(turn)].concat()),
            promoted: HeapStats::default(),
            evacuation_work: 0,
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
}
