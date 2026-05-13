//! Runtime host support for Cairn-generated `execution_tape` programs.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::host::{AccessSink, Host, HostError, SigHash, ValueRef};
use execution_tape::program::{HostTypeId, ValueType};
use execution_tape::value::{Obj, ObjHandle, Value};

use crate::sem;

/// Host object type used for Cairn runtime objects.
pub const CAIRN_OBJECT_TYPE: HostTypeId = HostTypeId(0x4341_4952_4e5f_4f42);
/// Host object type used for boxed top `<object>` values.
pub const CAIRN_VALUE_TYPE: HostTypeId = HostTypeId(0x4341_4952_4e5f_5641);

/// Host-call symbol for object construction.
pub const MAKE_OBJECT: &str = "cairn.make_object";
/// Host-call symbol for reading an object's class id.
pub const CLASS_OF: &str = "cairn.class_of";
/// Host-call symbol for reading a typed runtime-managed slot.
pub const SLOT_GET: &str = "cairn.slot_get";
/// Host-call symbol for boxing a concrete value as the top `<object>` value.
pub const BOX_VALUE: &str = "cairn.box_value";
/// Host-call symbol for testing whether a value is an instance of a class.
pub const INSTANCE_OF: &str = "cairn.instance_of";
/// Host-call symbol for casting a value to a known runtime object class.
pub const CAST_OBJECT: &str = "cairn.cast_object";
/// Host-call symbol prefix for string literals.
pub const STRING_LITERAL_PREFIX: &str = "cairn.literal.string:";
/// Host-call symbol prefix for symbol literals.
pub const SYMBOL_LITERAL_PREFIX: &str = "cairn.literal.symbol:";
/// Host-call symbol for immutable vector literals.
pub const VECTOR_LITERAL: &str = "cairn.literal.vector";
/// Host-call symbol for runtime value equality.
pub const VALUE_EQ: &str = "cairn.value_eq";

/// Returns the `execution_tape` value type for Cairn objects.
#[must_use]
pub const fn object_type() -> ValueType {
    ValueType::Obj(CAIRN_OBJECT_TYPE)
}

/// Returns the `execution_tape` value type for boxed top `<object>` values.
#[must_use]
pub const fn any_type() -> ValueType {
    ValueType::Obj(CAIRN_VALUE_TYPE)
}

/// Runtime class layout needed by the host object heap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeClass {
    /// Class name for diagnostics and inspection.
    pub name: String,
    /// Runtime object kind for this class.
    pub kind: HostObjectKind,
    /// Runtime superclass, if this class inherits from another native class.
    pub superclass: Option<sem::ClassId>,
    /// Slots in semantic slot order.
    pub slots: Vec<RuntimeSlot>,
}

/// Runtime object storage kind for a nominal Cairn class.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HostObjectKind {
    /// Object is allocated and stored by the Cairn runtime heap.
    Native,
    /// Object is a built-in value allocated by the Cairn runtime heap.
    BuiltinValue {
        /// Built-in value class represented by this runtime class.
        kind: sem::BuiltinValueClass,
    },
    /// Object is supplied by a Rust host adapter.
    HostBacked,
}

/// Runtime slot layout needed by host object operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSlot {
    /// Slot name for diagnostics and inspection.
    pub name: String,
    /// Slot value category.
    pub kind: sem::ValueKind,
    /// Whether this slot is read through a Rust host adapter.
    pub host_backed: bool,
}

/// Runtime layout emitted alongside a tape program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeLayout {
    /// Classes in semantic class-id order.
    pub classes: Vec<RuntimeClass>,
}

impl RuntimeLayout {
    /// Creates a runtime layout from a semantic module.
    #[must_use]
    pub fn from_sem(module: &sem::Module) -> Self {
        Self {
            classes: module
                .classes
                .iter()
                .map(|class| RuntimeClass {
                    name: class.name.clone(),
                    kind: match class.origin {
                        sem::ClassOrigin::Native => HostObjectKind::Native,
                        sem::ClassOrigin::BuiltinValue { kind } => {
                            HostObjectKind::BuiltinValue { kind }
                        }
                        sem::ClassOrigin::HostBacked { .. } => HostObjectKind::HostBacked,
                    },
                    superclass: class.superclass,
                    slots: class
                        .effective_slots
                        .iter()
                        .map(|slot| RuntimeSlot {
                            name: slot.name.clone(),
                            kind: slot.kind,
                            host_backed: matches!(slot.origin, sem::SlotOrigin::HostBacked { .. }),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Returns the runtime class id for a Cairn-visible class name.
    #[must_use]
    pub fn class_id(&self, name: &str) -> Option<sem::ClassId> {
        self.classes
            .iter()
            .position(|class| class.name == name)
            .map(|index| sem::ClassId(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// Returns true when `actual` is `expected` or inherits from it.
    #[must_use]
    pub fn is_same_or_subclass(&self, actual: sem::ClassId, expected: sem::ClassId) -> bool {
        let mut current = Some(actual);
        let mut visited = 0;
        while let Some(class) = current {
            if class == expected {
                return true;
            }
            visited += 1;
            if visited > self.classes.len() {
                return false;
            }
            current = self
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.superclass);
        }
        false
    }
}

struct RuntimeObject {
    class: sem::ClassId,
    storage: RuntimeObjectStorage,
}

enum RuntimeObjectStorage {
    Native {
        slots: Vec<Value>,
    },
    String {
        value: String,
    },
    Symbol {
        name: String,
    },
    Vector {
        elements: Vec<Value>,
    },
    HostBacked {
        adapter: alloc::boxed::Box<dyn HostObjectAdapter>,
    },
}

struct InternedObject {
    name: String,
    object: Obj,
}

/// Adapter for reading data from a Rust-owned host-backed object.
///
/// Implementors own the concrete Rust value access. Cairn only passes the
/// resolved slot id and expected Cairn value category.
pub trait HostObjectAdapter {
    /// Reads one slot from a host-backed object.
    fn read_slot(&self, slot: sem::SlotId, kind: sem::ValueKind) -> Result<Value, HostError>;
}

/// Host implementation for Cairn runtime object operations.
pub struct CairnHost {
    layout: RuntimeLayout,
    objects: Vec<RuntimeObject>,
    values: Vec<Value>,
    symbols: Vec<InternedObject>,
}

impl CairnHost {
    /// Creates a new runtime host with an empty object heap.
    #[must_use]
    pub fn new(layout: RuntimeLayout) -> Self {
        Self {
            layout,
            objects: Vec::new(),
            values: Vec::new(),
            symbols: Vec::new(),
        }
    }

    /// Installs a Rust-owned host-backed object and returns its Cairn handle.
    pub fn push_host_object(
        &mut self,
        class: sem::ClassId,
        adapter: impl HostObjectAdapter + 'static,
    ) -> Result<Value, HostError> {
        let class_info = self
            .layout
            .classes
            .get(class.0 as usize)
            .ok_or(HostError::Failed)?;
        if class_info.kind != HostObjectKind::HostBacked {
            return Err(HostError::Failed);
        }
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::HostBacked {
                adapter: alloc::boxed::Box::new(adapter),
            },
        }))
    }

    /// Boxes a value into the top `<object>` runtime representation.
    pub fn box_top_value(&mut self, value: Value) -> Result<Value, HostError> {
        match value {
            Value::I64(_) | Value::Bool(_) | Value::U64(_) => Ok(self.push_boxed_value(value)),
            Value::Obj(obj) if obj.host_type == CAIRN_OBJECT_TYPE => {
                Ok(self.push_boxed_value(Value::Obj(obj)))
            }
            Value::Obj(obj) if obj.host_type == CAIRN_VALUE_TYPE => Ok(Value::Obj(obj)),
            _ => Err(HostError::Failed),
        }
    }

    fn object(&self, obj: Obj) -> Result<&RuntimeObject, HostError> {
        if obj.host_type != CAIRN_OBJECT_TYPE {
            return Err(HostError::SignatureMismatch);
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(HostError::Failed)?;
        self.objects.get(index).ok_or(HostError::Failed)
    }

    fn boxed_value(&self, obj: Obj) -> Result<&Value, HostError> {
        if obj.host_type != CAIRN_VALUE_TYPE {
            return Err(HostError::SignatureMismatch);
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(HostError::Failed)?;
        self.values.get(index).ok_or(HostError::Failed)
    }

    fn push_boxed_value(&mut self, value: Value) -> Value {
        let handle = ObjHandle(u64::try_from(self.values.len() + 1).unwrap_or(u64::MAX));
        self.values.push(value);
        Value::Obj(Obj {
            host_type: CAIRN_VALUE_TYPE,
            handle,
        })
    }

    fn push_object(&mut self, object: RuntimeObject) -> Value {
        let handle = ObjHandle(u64::try_from(self.objects.len() + 1).unwrap_or(u64::MAX));
        self.objects.push(object);
        Value::Obj(Obj {
            host_type: CAIRN_OBJECT_TYPE,
            handle,
        })
    }
}

impl Host for CairnHost {
    fn call(
        &mut self,
        symbol: &str,
        _sig_hash: SigHash,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
        _access: Option<&mut dyn AccessSink>,
    ) -> Result<u64, HostError> {
        match symbol {
            _ if symbol.starts_with(STRING_LITERAL_PREFIX) => {
                let value = decode_literal_suffix(symbol, STRING_LITERAL_PREFIX)?;
                let class = self.builtin_class(sem::BuiltinValueClass::String)?;
                rets[0] = self.push_object(RuntimeObject {
                    class,
                    storage: RuntimeObjectStorage::String { value },
                });
                Ok(0)
            }
            _ if symbol.starts_with(SYMBOL_LITERAL_PREFIX) => {
                let name = decode_literal_suffix(symbol, SYMBOL_LITERAL_PREFIX)?;
                rets[0] = self.intern_symbol(name)?;
                Ok(0)
            }
            VECTOR_LITERAL => {
                let class = self.builtin_class(sem::BuiltinValueClass::Vector)?;
                let mut elements = Vec::with_capacity(args.len());
                for arg in args {
                    elements.push(self.value_for_any(*arg)?);
                }
                rets[0] = self.push_object(RuntimeObject {
                    class,
                    storage: RuntimeObjectStorage::Vector { elements },
                });
                Ok(0)
            }
            VALUE_EQ => {
                let [ValueRef::Obj(lhs), ValueRef::Obj(rhs)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Bool(self.objects_equal(*lhs, *rhs)?);
                Ok(0)
            }
            MAKE_OBJECT => {
                let Some(ValueRef::U64(class_raw)) = args.first().copied() else {
                    return Err(HostError::Failed);
                };
                let class_index = usize::try_from(class_raw).map_err(|_| HostError::Failed)?;
                let class = self
                    .layout
                    .classes
                    .get(class_index)
                    .ok_or(HostError::Failed)?;
                if class.kind != HostObjectKind::Native {
                    return Err(HostError::Failed);
                }
                if args.len() != class.slots.len() + 1 {
                    return Err(HostError::Failed);
                }
                let mut slots = Vec::with_capacity(class.slots.len());
                for (arg, slot) in args[1..].iter().zip(&class.slots) {
                    slots.push(self.value_for_slot(*arg, slot.kind)?);
                }
                rets[0] = self.push_object(RuntimeObject {
                    class: sem::ClassId(u32::try_from(class_index).unwrap_or(u32::MAX)),
                    storage: RuntimeObjectStorage::Native { slots },
                });
                Ok(0)
            }
            CLASS_OF => {
                let [ValueRef::Obj(obj)] = args else {
                    return Err(HostError::Failed);
                };
                let object = self.object(*obj)?;
                rets[0] = Value::U64(u64::from(object.class.0));
                Ok(0)
            }
            SLOT_GET => {
                let [ValueRef::Obj(obj), ValueRef::U64(slot_raw)] = args else {
                    return Err(HostError::Failed);
                };
                let object = self.object(*obj)?;
                let slot_index = usize::try_from(*slot_raw).map_err(|_| HostError::Failed)?;
                let slot = sem::SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX));
                let slot_info = self
                    .layout
                    .classes
                    .get(object.class.0 as usize)
                    .and_then(|class| class.slots.get(slot_index))
                    .ok_or(HostError::Failed)?;
                rets[0] = match &object.storage {
                    RuntimeObjectStorage::Native { slots } => {
                        slots.get(slot_index).cloned().ok_or(HostError::Failed)?
                    }
                    RuntimeObjectStorage::String { .. }
                    | RuntimeObjectStorage::Symbol { .. }
                    | RuntimeObjectStorage::Vector { .. } => return Err(HostError::Failed),
                    RuntimeObjectStorage::HostBacked { adapter } => {
                        adapter.read_slot(slot, slot_info.kind)?
                    }
                };
                Ok(0)
            }
            BOX_VALUE => {
                let value = match args {
                    [ValueRef::I64(value)] => Value::I64(*value),
                    [ValueRef::Bool(value)] => Value::Bool(*value),
                    [ValueRef::U64(value)] => Value::U64(*value),
                    [ValueRef::Obj(obj)] if obj.host_type == CAIRN_OBJECT_TYPE => Value::Obj(*obj),
                    _ => return Err(HostError::Failed),
                };
                rets[0] = self.push_boxed_value(value);
                Ok(0)
            }
            INSTANCE_OF => {
                let [value, ValueRef::U64(class_raw)] = args else {
                    return Err(HostError::Failed);
                };
                let class = runtime_class_id(*class_raw)?;
                rets[0] = Value::Bool(self.value_is_instance(*value, class));
                Ok(0)
            }
            CAST_OBJECT => {
                let [value, ValueRef::U64(class_raw)] = args else {
                    return Err(HostError::Failed);
                };
                let class = runtime_class_id(*class_raw)?;
                let obj = self
                    .value_object(*value)
                    .filter(|obj| {
                        self.object(*obj).is_ok_and(|object| {
                            self.layout.is_same_or_subclass(object.class, class)
                        })
                    })
                    .ok_or(HostError::Failed)?;
                rets[0] = Value::Obj(obj);
                Ok(0)
            }
            _ => Err(HostError::UnknownSymbol),
        }
    }
}

impl CairnHost {
    fn builtin_class(&self, kind: sem::BuiltinValueClass) -> Result<sem::ClassId, HostError> {
        self.layout.class_id(kind.name()).ok_or(HostError::Failed)
    }

    fn intern_symbol(&mut self, name: String) -> Result<Value, HostError> {
        if let Some(entry) = self.symbols.iter().find(|entry| entry.name == name) {
            return Ok(Value::Obj(entry.object));
        }
        let class = self.builtin_class(sem::BuiltinValueClass::Symbol)?;
        let value = self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Symbol { name: name.clone() },
        });
        let Value::Obj(object) = value else {
            return Err(HostError::Failed);
        };
        self.symbols.push(InternedObject { name, object });
        Ok(value)
    }

    fn value_for_any(&self, value: ValueRef<'_>) -> Result<Value, HostError> {
        let ValueRef::Obj(obj) = value else {
            return Err(HostError::Failed);
        };
        let _ = self.boxed_value(obj)?;
        Ok(Value::Obj(obj))
    }

    fn objects_equal(&self, lhs: Obj, rhs: Obj) -> Result<bool, HostError> {
        if lhs == rhs {
            return Ok(true);
        }
        let lhs = self.object(lhs)?;
        let rhs = self.object(rhs)?;
        Ok(match (&lhs.storage, &rhs.storage) {
            (
                RuntimeObjectStorage::String { value: left },
                RuntimeObjectStorage::String { value: right },
            ) => left == right,
            (
                RuntimeObjectStorage::Symbol { name: left },
                RuntimeObjectStorage::Symbol { name: right },
            ) => left == right,
            (
                RuntimeObjectStorage::Vector { elements: left },
                RuntimeObjectStorage::Vector { elements: right },
            ) => left == right,
            _ => false,
        })
    }

    fn value_object(&self, value: ValueRef<'_>) -> Option<Obj> {
        match value {
            ValueRef::Obj(obj) if obj.host_type == CAIRN_OBJECT_TYPE => Some(obj),
            ValueRef::Obj(obj) if obj.host_type == CAIRN_VALUE_TYPE => {
                match self.boxed_value(obj).ok()? {
                    Value::Obj(boxed) if boxed.host_type == CAIRN_OBJECT_TYPE => Some(*boxed),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn value_is_instance(&self, value: ValueRef<'_>, class: sem::ClassId) -> bool {
        self.value_object(value).is_some_and(|obj| {
            self.object(obj)
                .is_ok_and(|object| self.layout.is_same_or_subclass(object.class, class))
        })
    }

    fn value_for_slot(
        &self,
        value: ValueRef<'_>,
        kind: sem::ValueKind,
    ) -> Result<Value, HostError> {
        match (kind, value) {
            (sem::ValueKind::Any, ValueRef::Obj(obj)) => {
                let _ = self.boxed_value(obj)?;
                Ok(Value::Obj(obj))
            }
            (sem::ValueKind::I64, ValueRef::I64(value)) => Ok(Value::I64(value)),
            (sem::ValueKind::Bool, ValueRef::Bool(value)) => Ok(Value::Bool(value)),
            (
                sem::ValueKind::Object(expected_class)
                | sem::ValueKind::String(expected_class)
                | sem::ValueKind::Symbol(expected_class)
                | sem::ValueKind::Vector(expected_class),
                ValueRef::Obj(obj),
            ) => {
                let object = self.object(obj)?;
                if self
                    .layout
                    .is_same_or_subclass(object.class, expected_class)
                {
                    Ok(Value::Obj(obj))
                } else {
                    Err(HostError::Failed)
                }
            }
            (sem::ValueKind::KnownCallable(_), _) => Err(HostError::Failed),
            _ => Err(HostError::Failed),
        }
    }
}

/// Host signature for object construction for a runtime class.
#[must_use]
pub fn make_object_signature(class: &RuntimeClass) -> Option<execution_tape::host::HostSig> {
    let mut args = vec![ValueType::U64];
    for slot in &class.slots {
        args.push(value_type(slot.kind)?);
    }
    Some(execution_tape::host::HostSig {
        args,
        rets: vec![object_type()],
    })
}

/// Host signature for `class_of`.
#[must_use]
pub fn class_of_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![object_type()],
        rets: vec![ValueType::U64],
    }
}

/// Host signature for `slot_get`.
#[must_use]
pub fn slot_get_signature(kind: sem::ValueKind) -> Option<execution_tape::host::HostSig> {
    Some(execution_tape::host::HostSig {
        args: vec![object_type(), ValueType::U64],
        rets: vec![value_type(kind)?],
    })
}

/// Host signature for boxing a concrete value as top `<object>`.
#[must_use]
pub fn box_value_signature(input: ValueType) -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![input],
        rets: vec![any_type()],
    }
}

/// Host signature for `instance?`.
#[must_use]
pub fn instance_of_signature(input: ValueType) -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![input, ValueType::U64],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for `as` to a runtime object.
#[must_use]
pub fn cast_object_signature(input: ValueType) -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![input, ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for a literal host call with no runtime arguments.
#[must_use]
pub fn object_literal_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: Vec::new(),
        rets: vec![object_type()],
    }
}

/// Host signature for a vector literal of `arity` boxed elements.
#[must_use]
pub fn vector_literal_signature(arity: usize) -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![any_type(); arity],
        rets: vec![object_type()],
    }
}

/// Host signature for runtime value equality.
#[must_use]
pub fn value_eq_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![object_type(), object_type()],
        rets: vec![ValueType::Bool],
    }
}

fn value_type(kind: sem::ValueKind) -> Option<ValueType> {
    match kind {
        sem::ValueKind::Any => Some(any_type()),
        sem::ValueKind::I64 => Some(ValueType::I64),
        sem::ValueKind::Bool => Some(ValueType::Bool),
        sem::ValueKind::String(_)
        | sem::ValueKind::Symbol(_)
        | sem::ValueKind::Vector(_)
        | sem::ValueKind::Object(_) => Some(object_type()),
        sem::ValueKind::ClassObject(_) => Some(ValueType::U64),
        sem::ValueKind::KnownCallable(_) | sem::ValueKind::Error => None,
    }
}

fn runtime_class_id(raw: u64) -> Result<sem::ClassId, HostError> {
    Ok(sem::ClassId(
        u32::try_from(raw).map_err(|_| HostError::Failed)?,
    ))
}

/// Encodes a string-like literal into a host-call symbol.
#[must_use]
pub fn literal_symbol(prefix: &str, value: &str) -> String {
    let mut symbol = String::with_capacity(prefix.len() + value.len() * 2);
    symbol.push_str(prefix);
    for byte in value.as_bytes() {
        push_hex(&mut symbol, *byte);
    }
    symbol
}

fn decode_literal_suffix(symbol: &str, prefix: &str) -> Result<String, HostError> {
    let encoded = symbol.strip_prefix(prefix).ok_or(HostError::Failed)?;
    if encoded.len() % 2 != 0 {
        return Err(HostError::Failed);
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index])?;
        let low = hex_value(bytes[index + 1])?;
        decoded.push((high << 4) | low);
        index += 2;
    }
    String::from_utf8(decoded).map_err(|_| HostError::Failed)
}

fn push_hex(out: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(char::from(HEX[usize::from(byte >> 4)]));
    out.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

fn hex_value(byte: u8) -> Result<u8, HostError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HostError::Failed),
    }
}
