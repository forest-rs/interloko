//! Generic sends answered where the guest makes them.
//!
//! A dynamic send asks [`CODE_DISPATCH`] for the function to call. While a
//! [`crate::driver`] run is open the [`InterlokoHost`] answers it inline,
//! without yielding the turn: the driver hands the host the units the run may
//! run when it starts, and again whenever a chooser installs one, and the
//! host selects from the newest unit's [`crate::code::GenericDispatch`]
//! table, caching the choice by the classes of the arguments that select it.
//!
//! The cache belongs to one newest unit. Every handover empties it, so a
//! choice made before an install never answers a send made after it, and it
//! does not outlive the run. Outside a run the host answers no send.
//!
//! Only code the run knows may send: nothing compared another install's
//! code with the newest unit. The host knows a sender by its program. When
//! a program the run runs is also installed as a unit the run does not know
//! (the same code installed twice in one VM), the program no longer names its
//! sender, so for that run sends yield to the driver, which checks the
//! sender's unit and only then lets the host answer: the host refuses such a
//! run's sends that the driver did not hand it, even from a host wrapper
//! that runs the import inline anyway.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use execution_tape::host::{HostError, ValueRef};
use execution_tape::program::Program;
use execution_tape::value::{FuncRef, UnitId};
use execution_tape::vm::Units;
use interloko_model::{ClassId, GenericFunctionId};

use crate::InterlokoHost;
#[cfg(doc)]
use crate::code::CODE_DISPATCH;
use crate::code::{CodeUnit, Definition, GenericDispatch, UnitTable};

/// What one open run's generic sends have cost so far.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SendCacheReport {
    /// The unit sends select from: the run's newest.
    pub newest: UnitId,
    /// Selections cached for the newest unit.
    pub entries: usize,
    /// Whether sends are answered inline; `false` when the run cannot tell
    /// its senders apart by program, so each send yields to the driver.
    pub inline: bool,
    /// Sends whose argument classes were read, so the cache was consulted.
    pub sends: u64,
    /// Those the cache answered.
    pub hits: u64,
}

/// The open run's send state: the units it may run, newest last, and the
/// selections made from the newest. Between runs it is closed and empty,
/// keeping only its storage for the next run.
#[derive(Debug, Default)]
pub(crate) struct RunSends {
    open: bool,
    units: UnitTable,
    inline: bool,
    /// Whether the driver checked the sender of the send it is handing the
    /// host now; needed when [`Self::inline`] is not.
    attributed: bool,
    cache: BTreeMap<SendKey, FuncRef>,
    sends: u64,
    hits: u64,
}

impl RunSends {
    /// Whether a run is open and the host answers its sends inline.
    pub(crate) const fn inline(&self) -> bool {
        self.open && self.inline
    }
}

/// The classes that select a send, for up to [`SendKey::POSITIONS`]
/// specialized positions; wider sends are not cached.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SendKey {
    generic: u32,
    classes: [u32; SendKey::POSITIONS],
}

impl SendKey {
    const POSITIONS: usize = 4;
}

/// The classes of a send's selecting arguments, in the order of the plan's
/// positions.
struct SendClasses<'p> {
    positions: &'p [usize],
    first: [ClassId; SendKey::POSITIONS],
    rest: Vec<ClassId>,
}

impl<'p> SendClasses<'p> {
    /// The classes of `args` at `plan`'s positions, or `None` when one has
    /// none.
    fn read(
        plan: &'p GenericDispatch,
        args: &[ValueRef<'_>],
        class_of: impl Fn(ValueRef<'_>) -> Option<ClassId>,
    ) -> Option<Self> {
        let mut classes = Self {
            positions: &plan.positions,
            first: [ClassId(0); SendKey::POSITIONS],
            rest: Vec::new(),
        };
        for (slot, &position) in plan.positions.iter().enumerate() {
            let class = class_of(*args.get(position)?)?;
            match classes.first.get_mut(slot) {
                Some(entry) => *entry = class,
                None => classes.rest.push(class),
            }
        }
        Some(classes)
    }

    /// The class of argument `index`, when it selects.
    fn at(&self, index: usize) -> Option<ClassId> {
        let slot = self
            .positions
            .iter()
            .position(|&position| position == index)?;
        match slot.checked_sub(SendKey::POSITIONS) {
            None => self.first.get(slot).copied(),
            Some(rest) => self.rest.get(rest).copied(),
        }
    }

    fn key(&self, generic: GenericFunctionId) -> Option<SendKey> {
        if self.positions.len() > SendKey::POSITIONS {
            return None;
        }
        let mut classes = [u32::MAX; SendKey::POSITIONS];
        for (slot, class) in classes
            .iter_mut()
            .zip(&self.first)
            .take(self.positions.len())
        {
            *slot = class.0;
        }
        Some(SendKey {
            generic: generic.0,
            classes,
        })
    }
}

/// Whether every VM unit that runs a program of `units` is one of `units`,
/// so a program names its sender.
pub(crate) fn senders_attributable(vm: Units<'_>, units: &UnitTable) -> bool {
    vm.iter().all(|(unit, program)| {
        units.get(unit).is_some()
            || !units
                .iter()
                .any(|(_, code)| alloc::sync::Arc::ptr_eq(&code.program, program))
    })
}

impl InterlokoHost {
    /// Opens a run's sends over `units`, the newest last, with an empty
    /// cache. `inline` says whether [`senders_attributable`] holds.
    pub(crate) fn open_sends(&mut self, units: &UnitTable, inline: bool) {
        let run = &mut self.sends;
        run.open = true;
        run.units.copy_from(units);
        run.inline = inline;
        run.attributed = false;
        run.cache.clear();
        run.sends = 0;
        run.hits = 0;
        // A run initializes its constants afresh; see `constant.rs`.
        self.constants.clear();
    }

    /// Hands the open run's sends a newly installed newest unit, emptying
    /// the cache: its selections name the previous newest unit's methods.
    pub(crate) fn install_send_unit(
        &mut self,
        unit: UnitId,
        code: alloc::sync::Arc<CodeUnit>,
        inline: bool,
    ) {
        let run = &mut self.sends;
        if run.open {
            run.units.push(unit, code);
            run.inline = inline;
            run.cache.clear();
        }
    }

    /// Records whether the open run can tell its senders apart, after an
    /// install put a unit in the VM that the run does not run.
    pub(crate) fn set_sends_inline(&mut self, inline: bool) {
        if self.sends.open {
            self.sends.inline = inline;
        }
    }

    /// Marks the send the driver is about to hand over as checked: its
    /// sender is the run's code.
    pub(crate) fn attribute_send(&mut self) {
        self.sends.attributed = true;
    }

    /// Closes the open run's sends, returning how many consulted the cache
    /// and how many it answered.
    pub(crate) fn close_sends(&mut self) -> (u64, u64) {
        let run = &mut self.sends;
        run.open = false;
        run.units.clear();
        run.cache.clear();
        let counts = (run.sends, run.hits);
        self.constants.clear();
        counts
    }

    /// The open run's send cache, while a [`crate::driver`] run is open.
    #[must_use]
    pub fn send_cache(&self) -> Option<SendCacheReport> {
        let run = Some(&self.sends).filter(|run| run.open)?;
        Some(SendCacheReport {
            newest: run.units.newest_unit()?,
            entries: run.cache.len(),
            inline: run.inline,
            sends: run.sends,
            hits: run.hits,
        })
    }

    /// Answers a [`CODE_DISPATCH`] request from code of `program`: the
    /// method its arguments select from the newest unit, or the generic's
    /// dispatcher when the unit's dispatch table cannot decide or selects
    /// nothing, so the dispatcher's own checks apply and a send that finds
    /// no method signals its `<no-applicable-method>` there.
    ///
    /// Outside a run the symbol is unknown. Code of a program the run does
    /// not run fails, which the driver reports as foreign code.
    pub(crate) fn dispatch_send(
        &mut self,
        program: &Program,
        args: &[ValueRef<'_>],
    ) -> Result<FuncRef, HostError> {
        let run = &mut self.sends;
        if !run.open {
            return Err(HostError::UnknownSymbol);
        }
        // A run that cannot tell its senders apart by program answers only
        // the sends whose unit the driver checked.
        if !core::mem::take(&mut run.attributed) && !run.inline {
            return Err(HostError::Failed);
        }
        if !run
            .units
            .iter()
            .any(|(_, code)| core::ptr::eq(code.program.program(), program))
        {
            return Err(HostError::Failed);
        }
        let [ValueRef::U64(generic), args @ ..] = args else {
            return Err(HostError::Failed);
        };
        let generic = u32::try_from(*generic)
            .map(GenericFunctionId)
            .map_err(|_| HostError::Failed)?;
        if let Some(target) = self.select_method(generic, args) {
            return Ok(target);
        }
        self.sends
            .units
            .current(&Definition::Generic(generic))
            .ok_or(HostError::Failed)
    }

    /// Selects a directly callable method of the newest unit, consulting and
    /// filling the cache.
    fn select_method(
        &mut self,
        generic: GenericFunctionId,
        args: &[ValueRef<'_>],
    ) -> Option<FuncRef> {
        let (key, target, hit) = {
            let run = &self.sends;
            let unit = run.units.newest_unit()?;
            let code = run.units.newest()?;
            let plan = code.dispatch.get(generic.0 as usize)?.as_ref()?;
            let classes = SendClasses::read(plan, args, |value| self.class_of_any(value).ok())?;
            let key = classes.key(generic);
            match key.and_then(|key| run.cache.get(&key)) {
                Some(target) => (key, Some(*target), true),
                None => {
                    let target = plan
                        .select(
                            |index| classes.at(index),
                            |class, specializer| {
                                self.layout.is_same_or_subclass(class, specializer)
                            },
                        )
                        .filter(|case| case.direct)
                        .and_then(|case| code.methods.get(case.method.0 as usize))
                        .map(|method| FuncRef::new(unit, *method));
                    (key, target, false)
                }
            }
        };
        let run = &mut self.sends;
        run.sends = run.sends.saturating_add(1);
        if hit {
            run.hits = run.hits.saturating_add(1);
        } else if let (Some(key), Some(target)) = (key, target) {
            run.cache.insert(key, target);
        }
        target
    }
}
