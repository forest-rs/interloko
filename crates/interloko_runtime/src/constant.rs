//! Constant cells: where a constant's value is kept once its initializer
//! has run.
//!
//! A constant whose initializer is more than a literal is evaluated once:
//! its first read runs the initializer and stores the value in the
//! constant's cell, and every later read takes the value from the cell.
//!
//! Each program has cells of its own, named by the compiler's key for the
//! constant. A unit a hot swap installs therefore initializes its constants
//! afresh with its own code, while frames still running the code it
//! replaced keep the values that code computed: a constant is never read
//! with a value another program's initializer made, so what the compiler
//! proved about the value holds.
//!
//! Cells last for one driver run (see [`crate::driver::run_installed`]),
//! and a turn's beginning and end clear them too. A constant's value may
//! hold nursery objects of the turn and aggregates (callables) of the VM
//! run, whose handles a cell keeps past the host call that stored them:
//! that is sound because the VM collects no aggregates during a run, and
//! nothing kept outlives the run. A later run initializes its constants
//! again. Program addresses name cells only while their programs are
//! installed, which they stay for the whole run.
//!
//! A read asks once whether the cell holds a value, and gets the value with
//! the answer. A cell that holds none is then claimed: it becomes
//! *initializing* and the reader runs the initializer, then stores the
//! value, or abandons the claim if the initializer exits another way. A
//! claim on a cell that is still initializing fails: the initializer reads
//! its own constant, which the compiler rejects where it can see it, and
//! the compiled code signals a `<constant-cycle-error>` naming the constant
//! and the reader instead.
//!
//! String literals are constants too, as in Dylan: the first evaluation of
//! a literal in a run makes its string, and every later evaluation of a
//! literal with the same text returns that string, so `s() == s()` for a
//! function `s` returning a literal. Strings are immutable, so nothing but
//! `==` can tell. The strings are kept like cells, for the run.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use core::fmt;

use execution_tape::host::{HostContext, HostError, HostSig, ValueRef};
use execution_tape::program::ValueType;
use execution_tape::value::{Obj, ObjHandle, Value};
use interloko_model::SlotId;
use interloko_model::condition::{ConditionClass, ConstantCycleErrorSlot};

use crate::InterlokoHost;

/// Host-call symbol that reads a constant's cell: the cell key in, whether
/// it holds a value out, and, when the call declares a second result, the
/// value (a placeholder of the declared type when there is none). A
/// callable, an aggregate the host cannot make a placeholder of, is read
/// with [`CONSTANT_GET`] once the cell answers that it holds one.
pub const CONSTANT_READ: &str = "interloko.constant.read";
/// Host-call symbol that claims a constant's empty cell: it marks the cell
/// initializing and answers `#t`, or answers `#f` when the cell is already
/// initializing. It takes [`constant_claim_signature`].
pub const CONSTANT_CLAIM: &str = "interloko.constant.claim";
/// Host-call symbol that reads the value a constant's cell holds: the cell
/// key in, the value out, typed as the constant's values are.
pub const CONSTANT_GET: &str = "interloko.constant.get";
/// Host-call symbol that stores a constant's value in its claimed cell: the
/// cell key and the value in, typed as the constant's values are.
pub const CONSTANT_SET: &str = "interloko.constant.set";
/// Host-call symbol that gives up a claim whose initializer did not return,
/// so a later read runs it again. It takes
/// [`constant_abandon_signature`].
pub const CONSTANT_ABANDON: &str = "interloko.constant.abandon";

/// A constant cell host call compiled code should never make, which fails
/// the call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConstantCellError {
    /// The cell was read before it held a value.
    Uninitialized {
        /// The constant's cell key.
        key: u64,
    },
    /// The cell was claimed although it held a value.
    AlreadySet {
        /// The constant's cell key.
        key: u64,
    },
}

impl fmt::Display for ConstantCellError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uninitialized { key } => write!(
                formatter,
                "a constant cell was read before it held a value (cell {key:#018x})"
            ),
            Self::AlreadySet { key } => write!(
                formatter,
                "a constant cell was claimed after it held a value (cell {key:#018x})"
            ),
        }
    }
}

/// What a `<constant-cycle-error>` reports, read from its slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstantCycleReport {
    /// The stable code, `interloko_model`'s `CONSTANT_CYCLE_CODE` when
    /// compiled code signalled the condition.
    pub code: String,
    /// The constant's source name, such as `$table`.
    pub constant: String,
    /// The source name of the definition whose code read it.
    pub reader: String,
}

impl fmt::Display for ConstantCycleReport {
    /// Writes ``constant.read-during-initialization: `$x` was read while
    /// its own initializer was running (via `twice`)``.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: `{}` was read while its own initializer was running (via `{}`)",
            self.code, self.constant, self.reader
        )
    }
}

impl core::error::Error for ConstantCellError {}

impl From<ConstantCellError> for HostError {
    fn from(error: ConstantCellError) -> Self {
        Self::Custom(Arc::new(error))
    }
}

/// The state of one constant's cell.
#[derive(Clone, Debug)]
pub(crate) enum ConstantCell {
    /// The initializer is running.
    Initializing,
    /// The constant's value.
    Ready(Value),
}

/// Which cell a constant host call means: the calling program's, by its
/// address, and the constant's key in it.
type CellName = (usize, u64);

/// The constant cells of one host; see the module documentation.
#[derive(Clone, Debug, Default)]
pub(crate) struct ConstantCells {
    cells: BTreeMap<CellName, ConstantCell>,
    /// The string each literal text evaluated to so far.
    literals: BTreeMap<Box<str>, Obj>,
}

impl ConstantCells {
    /// Forgets every cell and literal, as a turn ends or begins.
    pub(crate) fn clear(&mut self) {
        self.cells.clear();
        self.literals.clear();
    }

    /// The string a literal with this text evaluated to earlier in the run.
    pub(crate) fn literal(&self, text: &str) -> Option<Obj> {
        self.literals.get(text).copied()
    }

    /// Keeps `string` as what a literal with this text evaluates to.
    pub(crate) fn remember_literal(&mut self, text: &str, string: Obj) {
        self.literals.insert(Box::from(text), string);
    }

    /// Returns how many cells hold a value.
    pub(crate) fn ready(&self) -> usize {
        self.cells
            .values()
            .filter(|cell| matches!(cell, ConstantCell::Ready(_)))
            .count()
    }
}

/// Signature of [`CONSTANT_CLAIM`]: the cell key in, whether it holds a
/// value out.
#[must_use]
pub fn constant_claim_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64],
        rets: vec![ValueType::Bool],
    }
}

/// Signature of [`CONSTANT_ABANDON`]: the cell key in, nothing out.
#[must_use]
pub fn constant_abandon_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64],
        rets: vec![],
    }
}

impl InterlokoHost {
    /// Returns how many constants hold a value in their cells.
    #[must_use]
    pub fn constant_cells_ready(&self) -> usize {
        self.constants.ready()
    }

    /// Answers a constant host call, or `None` when `symbol` is not one.
    pub(crate) fn constant_call(
        &mut self,
        symbol: &str,
        ctx: &HostContext<'_, '_>,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
    ) -> Option<Result<u64, HostError>> {
        if !matches!(
            symbol,
            CONSTANT_READ | CONSTANT_CLAIM | CONSTANT_GET | CONSTANT_SET | CONSTANT_ABANDON
        ) {
            return None;
        }
        let program = core::ptr::from_ref(ctx.program()).addr();
        let ([ValueRef::U64(key), rest @ ..], rets) = (args, rets) else {
            return Some(Err(HostError::Failed));
        };
        let name = (program, *key);
        let result = match (symbol, rest, rets) {
            (CONSTANT_READ, [], [ready]) => {
                *ready = Value::Bool(self.constant_value(name).is_some());
                Ok(())
            }
            (CONSTANT_READ, [], [ready, ret]) => {
                let held = self.constant_value(name);
                *ready = Value::Bool(held.is_some());
                held.or_else(|| placeholder(ctx))
                    .map(|value| *ret = value)
                    .ok_or(HostError::Failed)
            }
            (CONSTANT_CLAIM, [], [ret]) => self.claim_constant(name).map(|claimed| {
                *ret = Value::Bool(claimed);
            }),
            (CONSTANT_GET, [], [ret]) => self.get_constant(name).map(|value| *ret = value),
            (CONSTANT_SET, [value], []) => self.set_constant(name, value),
            (CONSTANT_ABANDON, [], []) => {
                self.abandon_constant(name);
                Ok(())
            }
            _ => Err(HostError::Failed),
        };
        Some(result.map(|()| 0))
    }

    /// The value the cell `name` holds, if it holds one.
    fn constant_value(&self, name: CellName) -> Option<Value> {
        match self.constants.cells.get(&name) {
            Some(ConstantCell::Ready(value)) => Some(value.clone()),
            _ => None,
        }
    }

    /// Claims the empty cell `name`: `true` once it is marked initializing,
    /// `false` when it already was.
    fn claim_constant(&mut self, name: CellName) -> Result<bool, HostError> {
        match self.constants.cells.get(&name) {
            Some(ConstantCell::Ready(_)) => {
                Err(ConstantCellError::AlreadySet { key: name.1 }.into())
            }
            Some(ConstantCell::Initializing) => Ok(false),
            None => {
                self.constants
                    .cells
                    .insert(name, ConstantCell::Initializing);
                Ok(true)
            }
        }
    }

    fn get_constant(&self, name: CellName) -> Result<Value, HostError> {
        match self.constants.cells.get(&name) {
            Some(ConstantCell::Ready(value)) => Ok(value.clone()),
            _ => Err(ConstantCellError::Uninitialized { key: name.1 }.into()),
        }
    }

    fn set_constant(&mut self, name: CellName, value: &ValueRef<'_>) -> Result<(), HostError> {
        let value = owned_value(value).ok_or(HostError::Failed)?;
        self.constants
            .cells
            .insert(name, ConstantCell::Ready(value));
        Ok(())
    }

    fn abandon_constant(&mut self, name: CellName) {
        if matches!(
            self.constants.cells.get(&name),
            Some(ConstantCell::Initializing)
        ) {
            self.constants.cells.remove(&name);
        }
    }
}

/// A value of the call's declared second result type, returned with the
/// answer that the cell holds none. The compiled code never uses it.
fn placeholder(ctx: &HostContext<'_, '_>) -> Option<Value> {
    let program = ctx.program();
    let entry = program.host_sig(ctx.host_sig_id())?;
    let ty = *program.host_sig_rets(entry).ok()?.get(1)?;
    Some(match ty {
        ValueType::Bool => Value::Bool(false),
        ValueType::I64 => Value::I64(0),
        ValueType::U64 => Value::U64(0),
        ValueType::I32 => Value::I32(0),
        ValueType::U32 => Value::U32(0),
        ValueType::F64 => Value::F64(0.0),
        ValueType::F32 => Value::F32(0.0),
        ValueType::Obj(host_type) => Value::Obj(Obj {
            host_type,
            handle: ObjHandle(0),
        }),
        _ => return None,
    })
}

impl InterlokoHost {
    /// Reads what `condition` reports, when it is a `<constant-cycle-error>`
    /// (or an instance of a subclass, whose layout starts with its slots).
    #[must_use]
    pub fn constant_cycle_report(&self, condition: Obj) -> Option<ConstantCycleReport> {
        let class = self
            .layout
            .class_id(ConditionClass::ConstantCycleError.name())?;
        let object = self.object(condition).ok()?;
        if !self.layout.is_same_or_subclass(object.class, class) {
            return None;
        }
        let text = |slot: ConstantCycleErrorSlot| {
            let id = SlotId(u32::try_from(slot.index()).ok()?);
            let value = self.read_slot(condition, id).ok()?;
            Some(self.string_text(&value).ok()?.to_string())
        };
        Some(ConstantCycleReport {
            code: text(ConstantCycleErrorSlot::Code)?,
            constant: text(ConstantCycleErrorSlot::Constant)?,
            reader: text(ConstantCycleErrorSlot::Reader)?,
        })
    }
}

/// The value an argument passes, for one a cell can keep.
fn owned_value(value: &ValueRef<'_>) -> Option<Value> {
    Some(match value {
        ValueRef::I64(value) => Value::I64(*value),
        ValueRef::U64(value) => Value::U64(*value),
        ValueRef::I32(value) => Value::I32(*value),
        ValueRef::U32(value) => Value::U32(*value),
        ValueRef::F64(value) => Value::F64(*value),
        ValueRef::F32(value) => Value::F32(*value),
        ValueRef::Bool(value) => Value::Bool(*value),
        ValueRef::Obj(value) => Value::Obj(*value),
        ValueRef::Agg(value) => Value::Agg(*value),
        ValueRef::Func(value) => Value::Func(*value),
        ValueRef::Closure(value) => Value::Closure(*value),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostObjectKind, RuntimeClass, RuntimeLayout};
    use alloc::vec::Vec;
    use interloko_model::sealing::Sealing;

    fn host() -> InterlokoHost {
        InterlokoHost::new(RuntimeLayout {
            classes: vec![RuntimeClass {
                name: "<object>".into(),
                kind: HostObjectKind::Native,
                superclass: None,
                sealing: Sealing::Open,
                is_abstract: false,
                slots: Vec::new(),
            }],
            generics: Vec::new(),
            methods: Vec::new(),
        })
    }

    #[test]
    fn a_claimed_cell_is_set_once_and_not_claimed_twice() {
        let mut host = host();
        assert!(
            host.claim_constant((0, 7))
                .expect("an absent cell is claimed")
        );
        assert!(
            !host
                .claim_constant((0, 7))
                .expect("an initializing one is not")
        );
        host.set_constant((0, 7), &ValueRef::I64(42))
            .expect("a claimed cell takes its value");
        assert!(
            host.claim_constant((0, 7)).is_err(),
            "a set cell is not claimed"
        );
        assert_eq!(host.constant_value((0, 7)), Some(Value::I64(42)));
        assert_eq!(host.constant_cells_ready(), 1);
    }

    #[test]
    fn programs_have_cells_of_their_own() {
        let mut host = host();
        assert!(host.claim_constant((1, 7)).expect("claimed"));
        host.set_constant((1, 7), &ValueRef::I64(1)).expect("set");
        assert_eq!(host.constant_value((2, 7)), None);
    }

    #[test]
    fn an_abandoned_claim_is_claimed_again() {
        let mut host = host();
        assert!(host.claim_constant((0, 3)).expect("claimed"));
        host.abandon_constant((0, 3));
        assert!(host.claim_constant((0, 3)).expect("claimed again"));
    }

    #[test]
    fn every_turn_initializes_its_constants_afresh() {
        let mut host = host();
        host.begin_turn().expect("a turn opens");
        assert!(host.claim_constant((0, 1)).expect("claimed"));
        host.set_constant((0, 1), &ValueRef::Bool(true))
            .expect("set");
        host.finish_turn(&[], &mut []).expect("the turn finishes");
        assert_eq!(host.constant_cells_ready(), 0);

        host.begin_turn().expect("a second turn opens");
        assert!(host.claim_constant((0, 1)).expect("claimed afresh"));
        host.set_constant((0, 1), &ValueRef::Bool(true))
            .expect("set");
        host.abort_turn().expect("the turn aborts");
        assert_eq!(host.constant_cells_ready(), 0);
    }
}
