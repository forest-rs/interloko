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

/// Host-call symbol for object construction.
pub const MAKE_OBJECT: &str = "cairn.make_object";
/// Host-call symbol for reading an object's class id.
pub const CLASS_OF: &str = "cairn.class_of";
/// Host-call symbol for reading a typed runtime-managed slot.
pub const SLOT_GET: &str = "cairn.slot_get";

/// Returns the `execution_tape` value type for Cairn objects.
#[must_use]
pub const fn object_type() -> ValueType {
    ValueType::Obj(CAIRN_OBJECT_TYPE)
}

/// Runtime class layout needed by the host object heap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeClass {
    /// Class name for diagnostics and inspection.
    pub name: String,
    /// Slots in semantic slot order.
    pub slots: Vec<RuntimeSlot>,
}

/// Runtime slot layout needed by host object operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSlot {
    /// Slot name for diagnostics and inspection.
    pub name: String,
    /// Slot value category.
    pub kind: sem::ValueKind,
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
                    slots: class
                        .slots
                        .iter()
                        .map(|slot| RuntimeSlot {
                            name: slot.name.clone(),
                            kind: slot.kind,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RuntimeObject {
    class: sem::ClassId,
    slots: Vec<Value>,
}

/// Host implementation for Cairn runtime object operations.
#[derive(Clone, Debug, PartialEq)]
pub struct CairnHost {
    layout: RuntimeLayout,
    objects: Vec<RuntimeObject>,
}

impl CairnHost {
    /// Creates a new runtime host with an empty object heap.
    #[must_use]
    pub fn new(layout: RuntimeLayout) -> Self {
        Self {
            layout,
            objects: Vec::new(),
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
                if args.len() != class.slots.len() + 1 {
                    return Err(HostError::Failed);
                }
                let mut slots = Vec::with_capacity(class.slots.len());
                for (arg, slot) in args[1..].iter().zip(&class.slots) {
                    slots.push(self.value_for_slot(*arg, slot.kind)?);
                }
                let handle = ObjHandle(u64::try_from(self.objects.len() + 1).unwrap_or(u64::MAX));
                self.objects.push(RuntimeObject {
                    class: sem::ClassId(u32::try_from(class_index).unwrap_or(u32::MAX)),
                    slots,
                });
                rets[0] = Value::Obj(Obj {
                    host_type: CAIRN_OBJECT_TYPE,
                    handle,
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
                let value = object
                    .slots
                    .get(slot_index)
                    .cloned()
                    .ok_or(HostError::Failed)?;
                rets[0] = value;
                Ok(0)
            }
            _ => Err(HostError::UnknownSymbol),
        }
    }
}

impl CairnHost {
    fn value_for_slot(
        &self,
        value: ValueRef<'_>,
        kind: sem::ValueKind,
    ) -> Result<Value, HostError> {
        match (kind, value) {
            (sem::ValueKind::I64, ValueRef::I64(value)) => Ok(Value::I64(value)),
            (sem::ValueKind::Bool, ValueRef::Bool(value)) => Ok(Value::Bool(value)),
            (sem::ValueKind::Object(expected_class), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                if object.class == expected_class {
                    Ok(Value::Obj(obj))
                } else {
                    Err(HostError::Failed)
                }
            }
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

fn value_type(kind: sem::ValueKind) -> Option<ValueType> {
    match kind {
        sem::ValueKind::I64 => Some(ValueType::I64),
        sem::ValueKind::Bool => Some(ValueType::Bool),
        sem::ValueKind::Object(_) => Some(object_type()),
        sem::ValueKind::Error => None,
    }
}
