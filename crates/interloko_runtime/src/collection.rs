//! Mutable collections: `<vector>`, `<stretchy-vector>` and `<table>`.
//!
//! Collections are runtime objects in the host's heap, like instances,
//! rather than `execution_tape` aggregates, which are immutable values. They
//! take part in turns as instances do: a turn allocates them in the
//! nursery, and a store into an image collection is logged so an aborted
//! turn can undo it and a finished one can evacuate what it stored.
//!
//! A `<vector>` holds its elements in order, boxed as `<object>`s; a
//! `<stretchy-vector>` is a `<vector>` that also grows and shrinks at its
//! end. Both share one storage, so every vector operation takes either.
//!
//! A `<table>` maps keys to values. Keys compare as Dylan's `<table>`
//! compares them, by `==`, with one exception: a `<string>` key compares
//! by its text, as `=` and Dylan's `<string-table>` compare strings, so
//! `t["max"]` finds the entry stored under any other `"max"`. In full:
//!
//! - integers by value, whatever limited type held them;
//! - floats by class and IEEE 754 bit pattern (as `==`: `1.0` and `1` are
//!   different keys, and a NaN finds itself);
//! - booleans, characters and string positions by value, symbols by
//!   name, strings by text;
//! - classes, generics, methods, slot descriptors and type objects by what
//!   they denote, as `==` compares them;
//! - callables by their code and environment, as `==` compares them; like
//!   a callable in a slot, one used as a key holds its environment only
//!   for the run that made it;
//! - every other object by identity.
//!
//! A string key is copied into the table's index when it is stored, so the
//! table stays consistent whatever later happens to the key object.
//!
//! Entries keep the order they were added in. A table's state for
//! iteration is a position in that order, and its [`Table::version`]
//! counts the changes to its set of keys (an added key, a removed one), so
//! iteration can tell when the table changed under it. Replacing the value
//! of a key already present is not such a change.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;

use execution_tape::host::{HostContext, HostError};
use execution_tape::value::{FuncRef, Obj, Value};
use interloko_model::{BuiltinValueClass, ClassId};

use crate::boxed::BoxedHandle;
use crate::region::{LoggedStore, Space};
use crate::{
    INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE, InterlokoHost, RuntimeError, RuntimeObject,
    RuntimeObjectStorage, Unboxed,
};

/// The most elements `make` gives a new vector: 2^28, 4 GiB of element
/// slots. Until the heap has a byte budget, this bounds the one allocation
/// a single operation can ask for; a size past it is a `type-error.range`.
pub const MAX_VECTOR_SIZE: usize = 1 << 28;

/// The key a table indexes an entry by: what the key's equality compares.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum TableKey {
    /// An integer, by value.
    Integer(i64),
    /// A `<double-float>`, by bit pattern.
    Float(u64),
    /// A `<single-float>`, by bit pattern.
    SingleFloat(u32),
    /// A boolean.
    Boolean(bool),
    /// A `<character>`, by its Unicode scalar value.
    Character(u32),
    /// A `<string-position>`, by its byte offset.
    Position(u64),
    /// A string, by its text.
    String(String),
    /// A symbol, by name.
    Symbol(String),
    /// A class object, by class.
    Class(u32),
    /// A generic-function metaobject, by generic.
    Generic(u32),
    /// A method metaobject, by method.
    Method(u32),
    /// A slot descriptor, by class and slot.
    Slot(u32, u32),
    /// A `<limited-integer>`, by range.
    LimitedInteger(Option<i64>, Option<i64>),
    /// A `<subclass-type>`, by bound.
    Subclass(u32),
    /// A `<singleton>` or `<union>`, by its members, in an order that does
    /// not depend on how they were written.
    CompoundType(String),
    /// A callable, by its code and, when it captures anything, its
    /// environment.
    Callable(FuncRef, Option<u32>),
    /// Any other object, by identity: its runtime object handle.
    Object(u64),
}

/// A `<table>`'s storage.
#[derive(Clone, Debug, Default)]
pub(crate) struct Table {
    /// Entries as key, value pairs in the order they were added; a removed
    /// entry leaves a pair of [`Value::Unit`]s, so positions stay put.
    pub(crate) slots: Vec<Value>,
    /// Each present key's entry position.
    index: BTreeMap<TableKey, usize>,
    /// Number of present entries.
    live: usize,
    /// Changes to the set of keys so far.
    version: u64,
}

/// How to undo one store into an image table.
#[derive(Clone, Debug)]
pub(crate) enum TableUndo {
    /// A new key was added at entry `position`, the last one.
    Added {
        /// The key.
        key: TableKey,
        /// Entry position.
        position: usize,
    },
    /// The value at entry `position` was replaced.
    Replaced {
        /// Entry position.
        position: usize,
        /// The value before.
        old: Value,
    },
    /// The entry at `position` was removed.
    Removed {
        /// The key.
        key: TableKey,
        /// Entry position.
        position: usize,
        /// The entry's key and value.
        entry: [Value; 2],
    },
    /// The table was compacted; this is the whole table before.
    Compacted(Box<Table>),
}

impl Table {
    /// Number of entries.
    pub(crate) const fn len(&self) -> usize {
        self.live
    }

    /// Estimated bytes of the entries and their index, for the heap
    /// census.
    pub(crate) fn bytes(&self) -> usize {
        let keys = self
            .index
            .keys()
            .map(|key| match key {
                TableKey::String(text) | TableKey::Symbol(text) => text.len(),
                _ => 0,
            })
            .sum::<usize>();
        self.slots.len() * size_of::<Value>()
            + self.index.len() * size_of::<(TableKey, usize)>()
            + keys
    }

    /// Changes to the set of keys so far.
    pub(crate) const fn version(&self) -> u64 {
        self.version
    }

    fn position(&self, key: &TableKey) -> Option<usize> {
        self.index.get(key).copied()
    }

    fn value_at(&self, position: usize) -> Option<&Value> {
        self.slots.get(2 * position + 1)
    }

    /// Stores `value` under `key`, whose boxed object is `key_value`, and
    /// says how to undo it.
    fn insert(&mut self, key: TableKey, key_value: Value, value: Value) -> TableUndo {
        if let Some(position) = self.position(&key) {
            let old = core::mem::replace(&mut self.slots[2 * position + 1], value);
            return TableUndo::Replaced { position, old };
        }
        let position = self.slots.len() / 2;
        self.slots.extend([key_value, value]);
        self.index.insert(key.clone(), position);
        self.live += 1;
        self.version += 1;
        TableUndo::Added { key, position }
    }

    /// Removes `key`'s entry, if any, and says how to undo it.
    fn remove(&mut self, key: &TableKey) -> Option<TableUndo> {
        let position = self.index.remove(key)?;
        let entry = [
            core::mem::replace(&mut self.slots[2 * position], Value::Unit),
            core::mem::replace(&mut self.slots[2 * position + 1], Value::Unit),
        ];
        self.live -= 1;
        self.version += 1;
        Some(TableUndo::Removed {
            key: key.clone(),
            position,
            entry,
        })
    }

    /// Whether removed entries outnumber present ones enough to compact.
    fn wants_compaction(&self) -> bool {
        let removed = self.slots.len() / 2 - self.live;
        removed > 8 && removed > self.live
    }

    /// Drops removed entries, keeping the others in order.
    fn compact(&mut self) {
        let mut slots = Vec::with_capacity(2 * self.live);
        let mut moved = Vec::with_capacity(self.slots.len() / 2);
        for [key, value] in self.slots.as_chunks::<2>().0 {
            moved.push(slots.len() / 2);
            if !matches!(key, Value::Unit) {
                slots.extend([key.clone(), value.clone()]);
            }
        }
        for position in self.index.values_mut() {
            *position = moved[*position];
        }
        self.slots = slots;
        self.version += 1;
    }

    /// Number of entry positions, present or removed: one past the last
    /// iteration state.
    pub(crate) fn positions(&self) -> usize {
        self.slots.len() / 2
    }

    /// The first position at or after `from` holding an entry, or
    /// [`Self::positions`] when none does.
    pub(crate) fn next_position(&self, from: usize) -> usize {
        (from..self.positions())
            .find(|position| !matches!(self.slots[2 * position], Value::Unit))
            .unwrap_or_else(|| self.positions())
    }

    /// The key and value at entry `position`, when it holds an entry.
    pub(crate) fn entry_at(&self, position: usize) -> Option<(&Value, &Value)> {
        let key = self.slots.get(2 * position)?;
        (!matches!(key, Value::Unit)).then(|| (key, &self.slots[2 * position + 1]))
    }

    /// Undoes one logged store.
    pub(crate) fn undo(&mut self, undo: TableUndo) {
        match undo {
            TableUndo::Added { key, position } => {
                self.index.remove(&key);
                self.slots.truncate(2 * position);
                self.live -= 1;
                self.version -= 1;
            }
            TableUndo::Replaced { position, old } => {
                if let Some(value) = self.slots.get_mut(2 * position + 1) {
                    *value = old;
                }
            }
            TableUndo::Removed {
                key,
                position,
                entry: [key_value, value],
            } => {
                self.slots[2 * position] = key_value;
                self.slots[2 * position + 1] = value;
                self.index.insert(key, position);
                self.live += 1;
                self.version -= 1;
            }
            TableUndo::Compacted(before) => *self = *before,
        }
    }

    /// The entry positions a logged store may have made refer to the
    /// nursery.
    pub(crate) const fn logged_position(undo: &TableUndo) -> Option<usize> {
        match undo {
            TableUndo::Added { position, .. } | TableUndo::Replaced { position, .. } => {
                Some(*position)
            }
            TableUndo::Removed { .. } | TableUndo::Compacted(_) => None,
        }
    }

    /// Moves the identity key at entry `position` from handle `old` to
    /// `new`, after the object it names moved to the image.
    pub(crate) fn rekey(&mut self, position: usize, old: u64, new: u64) {
        if old != new && self.index.get(&TableKey::Object(old)) == Some(&position) {
            self.index.remove(&TableKey::Object(old));
            self.index.insert(TableKey::Object(new), position);
        }
    }
}

/// The runtime object handle a boxed identity key names, if it names one.
pub(crate) fn identity_handle(key: &Value) -> Option<u64> {
    let Value::Obj(obj) = key else {
        return None;
    };
    if obj.host_type == INTERLOKO_OBJECT_TYPE {
        return Some(obj.handle.0);
    }
    if obj.host_type != INTERLOKO_VALUE_TYPE {
        return None;
    }
    // An object handle is always immediate, so it needs no serials to
    // decode.
    match BoxedHandle::decode(obj.handle, crate::region::NurserySerials::default()) {
        BoxedHandle::Object(handle) => Some(handle.0),
        _ => None,
    }
}

// The compiler only emits these operations on collections of the right
// class, so an argument of another class is a failure of the program's code,
// not its data, and fails the host call. A missing key, an index outside a
// vector or an empty vector's last element are the data's, and the operation
// answers `None`, which the host call turns into the check-failed tag
// ([`crate::control::CHECK_FAILED_TAG`]) for the program to signal a
// condition with a `use-value` restart.
impl InterlokoHost {
    /// Makes a `<vector>` or `<stretchy-vector>` (`class`) of `size`
    /// elements, each `fill`.
    pub(crate) fn make_vector(
        &mut self,
        class: ClassId,
        size: i64,
        fill: Value,
    ) -> Result<Value, HostError> {
        let vector = self.builtin_class(BuiltinValueClass::Vector)?;
        let stretchy = self.builtin_class(BuiltinValueClass::StretchyVector)?;
        if class != vector && class != stretchy {
            return Err(HostError::Failed);
        }
        // The compiler checks the size against `MAX_VECTOR_SIZE`; this is
        // the runtime's own guard.
        let size = usize::try_from(size)
            .ok()
            .filter(|size| *size <= MAX_VECTOR_SIZE)
            .ok_or(HostError::Failed)?;
        let fill = self.value_for_element(fill)?;
        let elements = alloc::vec![fill; size];
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Vector {
                elements,
                constant: false,
            },
        }))
    }

    /// Makes an empty `<table>`.
    pub(crate) fn make_table(&mut self) -> Result<Value, HostError> {
        let class = self.builtin_class(BuiltinValueClass::Table)?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Table(Box::default()),
        }))
    }

    /// The number of elements of a vector or entries of a table.
    pub(crate) fn collection_size(&self, collection: Obj) -> Result<i64, HostError> {
        let len = match &self.object(collection)?.storage {
            RuntimeObjectStorage::Vector { elements, .. } => elements.len(),
            RuntimeObjectStorage::Table(table) => table.len(),
            _ => return Err(HostError::Failed),
        };
        Ok(i64::try_from(len).unwrap_or(i64::MAX))
    }

    fn vector_elements(&self, vector: Obj) -> Result<&Vec<Value>, HostError> {
        match &self.object(vector)?.storage {
            RuntimeObjectStorage::Vector { elements, .. } => Ok(elements),
            _ => Err(HostError::Failed),
        }
    }

    /// The element of `vector` at `index`, or `None` for an index outside
    /// it.
    pub(crate) fn vector_element(
        &self,
        vector: Obj,
        index: i64,
    ) -> Result<Option<Value>, HostError> {
        let elements = self.vector_elements(vector)?;
        Ok(usize::try_from(index)
            .ok()
            .and_then(|index| elements.get(index))
            .cloned())
    }

    /// A new constant `<vector>` of the elements of `vector` from `start`
    /// on: the vector a `#rest` parameter takes, of the arguments after the
    /// required ones. Like a vector literal's, it is not for its receiver
    /// to change.
    pub(crate) fn vector_tail(&mut self, vector: Obj, start: i64) -> Result<Value, HostError> {
        let elements = self.vector_elements(vector)?;
        let tail = usize::try_from(start)
            .ok()
            .and_then(|start| elements.get(start..))
            .ok_or(HostError::Failed)?
            .to_vec();
        let class = self.builtin_class(BuiltinValueClass::Vector)?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Vector {
                elements: tail,
                constant: true,
            },
        }))
    }

    /// Stores `value` at `index` of `vector` and returns it, or returns
    /// `None` for an index outside it or a constant vector, which the
    /// caller tells apart by the index.
    pub(crate) fn vector_element_setter(
        &mut self,
        value: Value,
        vector: Obj,
        index: i64,
    ) -> Result<Option<Value>, HostError> {
        let value = self.value_for_element(value)?;
        let RuntimeObjectStorage::Vector { elements, constant } = &self.object(vector)?.storage
        else {
            return Err(HostError::Failed);
        };
        let len = elements.len();
        let Some(slot) = usize::try_from(index)
            .ok()
            .filter(|index| *index < len && !*constant)
        else {
            return Ok(None);
        };
        let location = self.object_ref(vector)?;
        let Some(elements) = self.vector_elements_mut(vector)? else {
            return Err(HostError::Failed);
        };
        let old = core::mem::replace(&mut elements[slot], value.clone());
        self.log_store(location.space, || LoggedStore::Slot {
            index: location.index,
            slot,
            old,
        });
        Ok(Some(value))
    }

    /// Whether `vector` is constant: a literal, or a vector the runtime
    /// made.
    pub(crate) fn vector_is_constant(&self, vector: Obj) -> Result<bool, HostError> {
        match &self.object(vector)?.storage {
            RuntimeObjectStorage::Vector { constant, .. } => Ok(*constant),
            _ => Err(HostError::Failed),
        }
    }

    /// Adds `value` at the end of the `<stretchy-vector>` `vector` and
    /// returns the vector.
    pub(crate) fn stretchy_add(&mut self, vector: Obj, value: Value) -> Result<Value, HostError> {
        let value = self.value_for_element(value)?;
        self.require_stretchy(vector)?;
        let location = self.object_ref(vector)?;
        let Some(elements) = self.vector_elements_mut(vector)? else {
            return Err(HostError::Failed);
        };
        let len = elements.len();
        elements.push(value);
        self.log_store(location.space, || LoggedStore::Grew {
            index: location.index,
            len,
        });
        Ok(Value::Obj(vector))
    }

    fn require_stretchy(&self, vector: Obj) -> Result<(), HostError> {
        let stretchy = self.builtin_class(BuiltinValueClass::StretchyVector)?;
        if self.object(vector)?.class == stretchy {
            Ok(())
        } else {
            Err(HostError::Failed)
        }
    }

    fn vector_elements_mut(&mut self, vector: Obj) -> Result<Option<&mut Vec<Value>>, HostError> {
        Ok(match &mut self.object_mut(vector)?.storage {
            RuntimeObjectStorage::Vector { elements, .. } => Some(elements),
            _ => None,
        })
    }

    fn table(&self, table: Obj) -> Result<&Table, HostError> {
        match &self.object(table)?.storage {
            RuntimeObjectStorage::Table(table) => Ok(table),
            _ => Err(HostError::Failed),
        }
    }

    fn table_mut(&mut self, table: Obj) -> Result<&mut Table, HostError> {
        match &mut self.object_mut(table)?.storage {
            RuntimeObjectStorage::Table(table) => Ok(table),
            _ => Err(HostError::Failed),
        }
    }

    /// The value stored under `key` in `table`, or `None`.
    pub(crate) fn table_lookup(
        &self,
        table: Obj,
        key: Value,
        ctx: Option<&HostContext<'_, '_>>,
    ) -> Result<Option<Value>, HostError> {
        let key = self.table_key(&key, ctx)?;
        let table = self.table(table)?;
        Ok(table
            .position(&key)
            .and_then(|position| table.value_at(position))
            .cloned())
    }

    /// Stores `value` under `key` in `table` and returns it.
    pub(crate) fn table_element_setter(
        &mut self,
        value: Value,
        table: Obj,
        key: Value,
        ctx: Option<&HostContext<'_, '_>>,
    ) -> Result<Value, HostError> {
        let value = self.value_for_element(value)?;
        let key_value = self.value_for_element(key)?;
        let key = self.table_key(&key_value, ctx)?;
        let location = self.object_ref(table)?;
        // Only a turn's store into an image table is undone, so only then
        // is the table before a compaction kept.
        let logged = location.space == Space::Image && self.turn.is_some();
        let entries = self.table_mut(table)?;
        let compacted = entries.wants_compaction().then(|| {
            let before = logged.then(|| Box::new(entries.clone()));
            entries.compact();
            before
        });
        let undo = entries.insert(key, key_value, value.clone());
        if let Some(Some(before)) = compacted {
            self.log_store(location.space, || LoggedStore::Table {
                index: location.index,
                undo: TableUndo::Compacted(before),
            });
        }
        self.log_store(location.space, || LoggedStore::Table {
            index: location.index,
            undo,
        });
        Ok(value)
    }

    /// Removes `key`'s entry from `table`; returns whether it had one.
    pub(crate) fn table_remove_key(
        &mut self,
        table: Obj,
        key: Value,
        ctx: Option<&HostContext<'_, '_>>,
    ) -> Result<bool, HostError> {
        let key = self.table_key(&key, ctx)?;
        let location = self.object_ref(table)?;
        let Some(undo) = self.table_mut(table)?.remove(&key) else {
            return Ok(false);
        };
        self.log_store(location.space, || LoggedStore::Table {
            index: location.index,
            undo,
        });
        Ok(true)
    }

    /// Returns how many times the set of keys of the `<table>` `table` has
    /// changed: each key added and each key removed counts once, and
    /// replacing the value of a key already present does not count.
    /// Iteration over a table compares it to tell whether the table changed
    /// under it.
    pub fn table_version(&self, table: &Value) -> Result<u64, RuntimeError> {
        let Value::Obj(object) = table else {
            return Err(RuntimeError::ValueKindMismatch {
                expected: crate::ValueKind::Any,
            });
        };
        match &self.object(*object)?.storage {
            RuntimeObjectStorage::Table(table) => Ok(table.version()),
            _ => Err(RuntimeError::ValueKindMismatch {
                expected: crate::ValueKind::Any,
            }),
        }
    }

    /// The iteration version of `table`: [`Table::version`] as an
    /// `<integer>`.
    pub(crate) fn table_iteration_version(&self, table: Obj) -> Result<i64, HostError> {
        Ok(i64::try_from(self.table(table)?.version()).unwrap_or(i64::MAX))
    }

    /// The number of entry positions of `table`, its iteration limit.
    pub(crate) fn table_positions(&self, table: Obj) -> Result<i64, HostError> {
        Ok(i64::try_from(self.table(table)?.positions()).unwrap_or(i64::MAX))
    }

    /// The first entry position of `table` at or after `from`, or its
    /// number of positions when there is none; `None` when the table's
    /// version is no longer `version`, because its keys changed since
    /// iteration began.
    pub(crate) fn table_next_position(
        &self,
        table: Obj,
        from: i64,
        version: i64,
    ) -> Result<Option<i64>, HostError> {
        let table = self.table(table)?;
        if i64::try_from(table.version()).ok() != Some(version) {
            return Ok(None);
        }
        let from = usize::try_from(from).unwrap_or(0);
        Ok(Some(
            i64::try_from(table.next_position(from)).unwrap_or(i64::MAX),
        ))
    }

    /// The key (`key`) or value of the entry at `position` of `table`, or
    /// `None` when no entry is there or the table's version is no longer
    /// `version`.
    pub(crate) fn table_entry_part(
        &self,
        table: Obj,
        position: i64,
        version: i64,
        key: bool,
    ) -> Result<Option<Value>, HostError> {
        let table = self.table(table)?;
        if i64::try_from(table.version()).ok() != Some(version) {
            return Ok(None);
        }
        Ok(usize::try_from(position)
            .ok()
            .and_then(|position| table.entry_at(position))
            .map(|(k, v)| if key { k.clone() } else { v.clone() }))
    }

    /// An element or key as a collection holds it: a boxed `<object>`.
    fn value_for_element(&self, value: Value) -> Result<Value, HostError> {
        match value {
            Value::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                self.unbox(obj)?;
                Ok(value)
            }
            _ => Err(HostError::Failed),
        }
    }

    /// The table key of the boxed `<object>` `key`.
    fn table_key(
        &self,
        key: &Value,
        ctx: Option<&HostContext<'_, '_>>,
    ) -> Result<TableKey, HostError> {
        let Value::Obj(boxed) = key else {
            return Err(HostError::Failed);
        };
        let value = match self.unbox(*boxed)? {
            Unboxed::Character(character) => return Ok(TableKey::Character(u32::from(character))),
            Unboxed::Position(position) => return Ok(TableKey::Position(position)),
            Unboxed::Value(value) => value,
        };
        Ok(match value {
            Value::I64(value) => TableKey::Integer(value),
            Value::I32(value) => TableKey::Integer(i64::from(value)),
            Value::U32(value) => TableKey::Integer(i64::from(value)),
            Value::F64(value) => TableKey::Float(value.to_bits()),
            Value::F32(value) => TableKey::SingleFloat(value.to_bits()),
            Value::Bool(value) => TableKey::Boolean(value),
            Value::Agg(callable) => {
                let ctx = ctx.ok_or(HostError::Failed)?;
                let closure = match ctx.tuple_get(callable, 0) {
                    Ok(Value::Closure(closure)) => closure,
                    _ => return Err(HostError::Failed),
                };
                let captures = ctx.tuple_len(closure.env).map_err(|_| HostError::Failed)?;
                TableKey::Callable(closure.func, (captures > 0).then_some(closure.env.0))
            }
            Value::Obj(object) if object.host_type == INTERLOKO_OBJECT_TYPE => {
                match &self.object(object)?.storage {
                    RuntimeObjectStorage::String { text } => {
                        TableKey::String(String::from(text.as_str()))
                    }
                    RuntimeObjectStorage::Symbol { name } => TableKey::Symbol(name.clone()),
                    RuntimeObjectStorage::ClassMeta { class } => TableKey::Class(class.0),
                    RuntimeObjectStorage::GenericMeta { generic } => TableKey::Generic(generic.0),
                    RuntimeObjectStorage::MethodMeta { method } => TableKey::Method(method.0),
                    RuntimeObjectStorage::SlotDescriptor { class, slot } => {
                        TableKey::Slot(class.0, slot.0)
                    }
                    RuntimeObjectStorage::LimitedIntegerType { range, .. } => {
                        TableKey::LimitedInteger(range.min, range.max)
                    }
                    RuntimeObjectStorage::SubclassType { bound } => TableKey::Subclass(bound.0),
                    RuntimeObjectStorage::CompoundType(ty) => {
                        TableKey::CompoundType(compound_type_key(ty))
                    }
                    _ => TableKey::Object(object.handle.0),
                }
            }
            _ => return Err(HostError::Failed),
        })
    }

    /// Logs a store into an object in `space`, made by the open turn, when
    /// the object is in the image; nursery objects need no log.
    pub(crate) fn log_store(&mut self, space: Space, store: impl FnOnce() -> LoggedStore) {
        if space == Space::Image
            && let Some(turn) = &mut self.turn
        {
            turn.write_log.push(store());
        }
    }
}

/// The key a `<singleton>` or `<union>` type object indexes a table by:
/// what [`interloko_model::type_object::TypeObject::same_type`] compares,
/// so a union's members are sorted.
fn compound_type_key(ty: &interloko_model::type_object::TypeObject) -> String {
    use interloko_model::type_object::TypeObject;
    match ty {
        TypeObject::Union(members) => {
            let mut keys = members.iter().map(compound_type_key).collect::<Vec<_>>();
            keys.sort();
            alloc::format!("union{keys:?}")
        }
        TypeObject::LimitedInteger { range, .. } => alloc::format!("{:?}", range),
        ty => alloc::format!("{ty:?}"),
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    use execution_tape::value::{Obj, Value};
    use interloko_model::sealing::Sealing;
    use interloko_model::{BuiltinValueClass, ClassId, OBJECT_CLASS_ID};

    use crate::{HostObjectKind, InterlokoHost, RuntimeClass, RuntimeLayout, RuntimeObjectStorage};

    /// `<object>`, then `<vector>`, `<stretchy-vector>`, `<table>`,
    /// `<string>` and `<symbol>`, in that order.
    fn layout() -> RuntimeLayout {
        let builtin = |kind: BuiltinValueClass, superclass| RuntimeClass {
            name: kind.name().into(),
            kind: HostObjectKind::BuiltinValue { kind },
            superclass: Some(superclass),
            sealing: Sealing::Open,
            is_abstract: false,
            slots: Vec::new(),
        };
        RuntimeLayout {
            classes: vec![
                RuntimeClass {
                    name: "<object>".into(),
                    kind: HostObjectKind::Native,
                    superclass: None,
                    sealing: Sealing::Open,
                    is_abstract: false,
                    slots: Vec::new(),
                },
                builtin(BuiltinValueClass::Vector, OBJECT_CLASS_ID),
                builtin(BuiltinValueClass::StretchyVector, ClassId(1)),
                builtin(BuiltinValueClass::Table, OBJECT_CLASS_ID),
                builtin(BuiltinValueClass::String, OBJECT_CLASS_ID),
                builtin(BuiltinValueClass::Symbol, OBJECT_CLASS_ID),
            ],
            generics: Vec::new(),
            methods: Vec::new(),
        }
    }

    const VECTOR: ClassId = ClassId(1);
    const STRETCHY: ClassId = ClassId(2);

    fn host() -> InterlokoHost {
        InterlokoHost::new_checked(layout()).expect("layout")
    }

    fn object(value: &Value) -> Obj {
        let Value::Obj(object) = value else {
            panic!("expected an object, got {value:?}");
        };
        *object
    }

    fn boxed(host: &mut InterlokoHost, value: Value) -> Value {
        host.box_top_value(value).expect("boxes")
    }

    fn int(host: &mut InterlokoHost, value: i64) -> Value {
        boxed(host, Value::I64(value))
    }

    fn text(host: &mut InterlokoHost, value: &str) -> Value {
        let string = host.make_string(value).expect("string");
        boxed(host, string)
    }

    fn unboxed_int(host: &InterlokoHost, value: &Value) -> i64 {
        match host.boxed_value(object(value)) {
            Ok(Value::I64(value)) => value,
            other => panic!("expected an integer, got {other:?}"),
        }
    }

    /// The image's collections, rendered for exact comparison.
    fn snapshot(host: &InterlokoHost) -> String {
        host.image
            .objects
            .iter()
            .map(|object| match &object.storage {
                RuntimeObjectStorage::Vector { elements, .. } => format!("vector {elements:?}\n"),
                RuntimeObjectStorage::Table(table) => format!(
                    "table {:?} {:?} {} {}\n",
                    table.slots, table.index, table.live, table.version
                ),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn vectors_read_write_and_stretch_within_their_bounds() {
        let mut host = host();
        let zero = int(&mut host, 0);
        let vector = object(&host.make_vector(VECTOR, 3, zero.clone()).expect("vector"));
        assert_eq!(host.collection_size(vector).ok(), Some(3));
        let seven = int(&mut host, 7);
        assert_eq!(
            host.vector_element_setter(seven.clone(), vector, 2).ok(),
            Some(Some(seven.clone()))
        );
        assert_eq!(
            host.vector_element(vector, 2).ok(),
            Some(Some(seven.clone()))
        );
        assert_eq!(
            host.vector_element(vector, 0).ok(),
            Some(Some(zero.clone()))
        );
        for outside in [-1, 3, i64::MAX, i64::MIN] {
            assert_eq!(host.vector_element(vector, outside).ok(), Some(None));
            assert_eq!(
                host.vector_element_setter(seven.clone(), vector, outside)
                    .ok(),
                Some(None)
            );
        }
        // A vector the runtime made, as a literal is, is constant.
        let literal = host.push_vector(vec![seven.clone()]).expect("vector");
        let literal = object(&literal);
        assert_eq!(
            host.vector_element_setter(seven.clone(), literal, 0).ok(),
            Some(None)
        );
        assert_eq!(
            host.vector_element(literal, 0).ok(),
            Some(Some(seven.clone()))
        );
        // Only a stretchy vector stretches.
        assert!(host.stretchy_add(vector, seven.clone()).is_err());
        assert!(host.make_vector(VECTOR, -1, zero.clone()).is_err());
        assert!(host.make_vector(VECTOR, i64::MAX, zero.clone()).is_err());
        let past = i64::try_from(super::MAX_VECTOR_SIZE).expect("fits") + 1;
        assert!(host.make_vector(VECTOR, past, zero.clone()).is_err());

        let stretchy = object(&host.make_vector(STRETCHY, 0, zero).expect("vector"));
        for value in 1..=3 {
            let value = int(&mut host, value);
            assert_eq!(
                host.stretchy_add(stretchy, value).ok(),
                Some(Value::Obj(stretchy))
            );
        }
        assert_eq!(host.collection_size(stretchy).ok(), Some(3));
        let last = host
            .vector_element(stretchy, 2)
            .expect("reads")
            .expect("last");
        assert_eq!(unboxed_int(&host, &last), 3);
        assert_eq!(host.vector_element(stretchy, 3).ok(), Some(None));
    }

    #[test]
    fn table_keys_compare_strings_by_text_and_the_rest_by_identity() {
        let mut host = host();
        let table = object(&host.make_table().expect("table"));
        let set = |host: &mut InterlokoHost, key: &Value, value: i64| {
            let value = int(host, value);
            host.table_element_setter(value, table, key.clone(), None)
                .expect("stores");
        };
        let get = |host: &InterlokoHost, key: &Value| {
            host.table_lookup(table, key.clone(), None)
                .expect("looks up")
                .map(|value| unboxed_int(host, &value))
        };
        let max = text(&mut host, "max");
        let other_max = text(&mut host, "max");
        set(&mut host, &max, 1);
        // Another string with the same text is the same key.
        assert_eq!(get(&host, &other_max), Some(1));
        set(&mut host, &other_max, 2);
        assert_eq!(get(&host, &max), Some(2));
        assert_eq!(host.collection_size(table).ok(), Some(1));

        // Integers by value, floats by class and bits: 1 and 1.0 differ.
        let one = int(&mut host, 1);
        let one_float = boxed(&mut host, Value::F64(1.0));
        let wide = int(&mut host, i64::MAX);
        set(&mut host, &one, 10);
        set(&mut host, &one_float, 11);
        set(&mut host, &wide, 12);
        let probes = [
            int(&mut host, 1),
            boxed(&mut host, Value::F64(1.0)),
            int(&mut host, i64::MAX),
            boxed(&mut host, Value::F64(-0.0)),
        ];
        let found = probes.iter().map(|key| get(&host, key)).collect::<Vec<_>>();
        assert_eq!(found, [Some(10), Some(11), Some(12), None]);

        // Any other object by identity.
        let vector = host.make_vector(VECTOR, 0, one.clone()).expect("vector");
        let vector = boxed(&mut host, vector);
        let twin = host.make_vector(VECTOR, 0, one).expect("vector");
        let twin = boxed(&mut host, twin);
        set(&mut host, &vector, 20);
        assert_eq!(get(&host, &vector), Some(20));
        assert_eq!(get(&host, &twin), None);
        assert_eq!(host.collection_size(table).ok(), Some(5));
    }

    #[test]
    fn removing_keys_counts_as_a_change_and_compaction_keeps_order() {
        let mut host = host();
        let table_value = host.make_table().expect("table");
        let table = object(&table_value);
        let keys = (0..40).map(|key| int(&mut host, key)).collect::<Vec<_>>();
        for key in &keys {
            host.table_element_setter(key.clone(), table, key.clone(), None)
                .expect("stores");
        }
        assert_eq!(host.table_version(&table_value).ok(), Some(40));
        // Replacing a value changes no key.
        host.table_element_setter(keys[1].clone(), table, keys[0].clone(), None)
            .expect("stores");
        assert_eq!(host.table_version(&table_value).ok(), Some(40));
        for key in &keys[..30] {
            assert_eq!(
                host.table_remove_key(table, key.clone(), None).ok(),
                Some(true)
            );
        }
        assert_eq!(
            host.table_remove_key(table, keys[0].clone(), None).ok(),
            Some(false)
        );
        assert_eq!(host.table_version(&table_value).ok(), Some(70));
        assert_eq!(host.collection_size(table).ok(), Some(10));
        // The next addition compacts the 30 removed entries away.
        let added = int(&mut host, 99);
        host.table_element_setter(added.clone(), table, added, None)
            .expect("stores");
        let RuntimeObjectStorage::Table(entries) = &host.object(table).expect("table").storage
        else {
            panic!("expected a table");
        };
        assert_eq!(entries.slots.len(), 2 * 11);
        let order = entries
            .slots
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[key, _]| unboxed_int(&host, key))
            .collect::<Vec<_>>();
        assert_eq!(order, [30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 99]);
        for key in &keys[30..] {
            assert!(
                host.table_lookup(table, key.clone(), None)
                    .expect("looks up")
                    .is_some()
            );
        }
    }

    #[test]
    fn aborted_turns_undo_every_collection_store() {
        let mut host = host();
        let one = int(&mut host, 1);
        let vector = object(&host.make_vector(STRETCHY, 2, one.clone()).expect("vector"));
        let table = object(&host.make_table().expect("table"));
        let key = text(&mut host, "kept");
        host.table_element_setter(one.clone(), table, key.clone(), None)
            .expect("stores");
        for value in 0..20 {
            let value = int(&mut host, value);
            host.table_element_setter(value.clone(), table, value, None)
                .expect("stores");
        }
        for value in 0..15 {
            let value = int(&mut host, value);
            host.table_remove_key(table, value, None).expect("removes");
        }
        let before = snapshot(&host);

        host.begin_turn().expect("turn opens");
        let two = int(&mut host, 2);
        host.vector_element_setter(two.clone(), vector, 0)
            .expect("stores");
        host.stretchy_add(vector, two.clone()).expect("adds");
        host.vector_element_setter(two.clone(), vector, 2)
            .expect("stores");
        host.stretchy_add(vector, two.clone()).expect("adds");
        let added = text(&mut host, "added");
        host.table_element_setter(two.clone(), table, added, None)
            .expect("stores");
        host.table_element_setter(two.clone(), table, key.clone(), None)
            .expect("stores");
        host.table_remove_key(table, key, None).expect("removes");
        // Enough additions to compact the table during the turn.
        for value in 100..120 {
            let value = int(&mut host, value);
            host.table_element_setter(value.clone(), table, value, None)
                .expect("stores");
        }
        assert_ne!(snapshot(&host), before);
        let stats = host.abort_turn().expect("turn aborts");
        assert!(stats.logged_stores > 0);
        assert_eq!(snapshot(&host), before);
    }

    #[test]
    fn a_table_compacted_in_a_turn_keeps_what_the_turn_stored() {
        // The turn stores a nursery key and value into an image table, then
        // removes enough to compact it, which moves the young entry: the
        // whole table is a root, and its identity key is reindexed.
        let mut host = host();
        let table = object(&host.make_table().expect("table"));
        for key in 0..20 {
            let key = int(&mut host, key);
            host.table_element_setter(key.clone(), table, key, None)
                .expect("stores");
        }
        host.begin_turn().expect("turn opens");
        let one = int(&mut host, 1);
        let key = host.make_vector(VECTOR, 0, one).expect("vector");
        let key = boxed(&mut host, key);
        let value = text(&mut host, "young");
        host.table_element_setter(value, table, key, None)
            .expect("stores");
        for key in 0..15 {
            let key = int(&mut host, key);
            host.table_remove_key(table, key, None).expect("removes");
        }
        let added = int(&mut host, 99);
        host.table_element_setter(added.clone(), table, added, None)
            .expect("stores");
        host.finish_turn(&[], &mut []).expect("turn finishes");

        let RuntimeObjectStorage::Table(entries) = &host.object(table).expect("table").storage
        else {
            panic!("expected a table");
        };
        let slots = entries.slots.clone();
        for value in &slots {
            assert!(
                host.boxed_value(object(value)).is_ok(),
                "{value:?} names no live entry"
            );
        }
        let young_key = slots
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[key, _]| key.clone())
            .find(|key| {
                matches!(
                    host.boxed_value(object(key)),
                    Ok(Value::Obj(object)) if host.collection_size(object).is_ok()
                )
            })
            .expect("the young key");
        let found = host
            .table_lookup(table, young_key, None)
            .expect("looks up")
            .expect("present");
        let Ok(Value::Obj(string)) = host.boxed_value(object(&found)) else {
            panic!("expected a string");
        };
        assert_eq!(host.string_text(&Value::Obj(string)), Ok("young"));
    }

    #[test]
    fn finished_turns_evacuate_what_collections_were_given() {
        let mut host = host();
        let one = int(&mut host, 1);
        let vector = object(&host.make_vector(STRETCHY, 1, one.clone()).expect("vector"));
        let table = object(&host.make_table().expect("table"));

        host.begin_turn().expect("turn opens");
        let stored = text(&mut host, "stored");
        host.vector_element_setter(stored, vector, 0)
            .expect("stores");
        for word in ["first", "second"] {
            let word = text(&mut host, word);
            host.stretchy_add(vector, word).expect("adds");
        }
        // A nursery object as an identity key, and a nursery string as a
        // value.
        let key = host.make_vector(VECTOR, 0, one.clone()).expect("vector");
        let key = boxed(&mut host, key);
        let value = text(&mut host, "value");
        host.table_element_setter(value, table, key.clone(), None)
            .expect("stores");
        let mut keep = [key];
        host.finish_turn(&[], &mut keep).expect("turn finishes");

        let string_at = |host: &InterlokoHost, value: &Value| {
            let Ok(Value::Obj(string)) = host.boxed_value(object(value)) else {
                panic!("expected a boxed string, got {value:?}");
            };
            String::from(host.string_text(&Value::Obj(string)).expect("string"))
        };
        let elements = (0..3)
            .map(|index| {
                let element = host.vector_element(vector, index).expect("reads");
                string_at(&host, &element.expect("in range"))
            })
            .collect::<Vec<_>>();
        assert_eq!(elements, ["stored", "first", "second"]);
        // A table made in the turn moves to the image with its identity
        // keys, and finds them under their new handles.
        host.begin_turn().expect("turn opens");
        let young = host.make_table().expect("table");
        let key = host.make_vector(VECTOR, 0, one.clone()).expect("vector");
        let key = boxed(&mut host, key);
        let value = text(&mut host, "young");
        host.table_element_setter(value, object(&young), key.clone(), None)
            .expect("stores");
        let mut kept = [young, key];
        host.finish_turn(&[], &mut kept).expect("turn finishes");
        let found = host
            .table_lookup(object(&kept[0]), kept[1].clone(), None)
            .expect("looks up")
            .expect("present");
        assert_eq!(string_at(&host, &found), "young");
        // The key moved to the image, and the table finds it there.
        let found = host
            .table_lookup(table, keep[0].clone(), None)
            .expect("looks up")
            .expect("present");
        assert_eq!(string_at(&host, &found), "value");
    }
}
