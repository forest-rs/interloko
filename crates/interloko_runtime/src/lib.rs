//! Runtime layout, host-call signatures, and `execution_tape` host support.
//!
//! This crate owns the executable runtime boundary for Interloko-shaped
//! programs. It intentionally does not own Cairn syntax, semantic IR, lowering,
//! sessions, or compiler query state.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use execution_tape::host::{AccessSink, Host, HostError, SigHash, ValueRef};
use execution_tape::program::{HostTypeId, ValueType};
use execution_tape::value::{Obj, ObjHandle, Value};
use interloko_model::{BuiltinValueClass, ClassId, GenericFunctionId, MethodId, SlotId};

/// Host object type used for Cairn runtime objects.
pub const INTERLOKO_OBJECT_TYPE: HostTypeId = HostTypeId(0x494c_4f4b_5f4f_424a);
/// Host object type used for boxed top `<object>` values.
pub const INTERLOKO_VALUE_TYPE: HostTypeId = HostTypeId(0x494c_4f4b_5f56_414c);

/// Host-call symbol for object construction.
pub const MAKE_OBJECT: &str = "interloko.make_object";
/// Host-call symbol for reading an object's class id.
pub const CLASS_OF: &str = "interloko.class_of";
/// Host-call symbol for reading a typed runtime-managed slot.
pub const SLOT_GET: &str = "interloko.slot_get";
/// Host-call symbol for writing a typed runtime-managed slot.
pub const SLOT_SET: &str = "interloko.slot_set";
/// Host-call symbol for boxing a concrete value as the top `<object>` value.
pub const BOX_VALUE: &str = "interloko.box_value";
/// Host-call symbol for testing whether a value is an instance of a class.
pub const INSTANCE_OF: &str = "interloko.instance_of";
/// Host-call symbol for casting a value to a known runtime object class.
pub const CAST_OBJECT: &str = "interloko.cast_object";
/// Host-call symbol prefix for string literals.
pub const STRING_LITERAL_PREFIX: &str = "interloko.literal.string:";
/// Host-call symbol prefix for symbol literals.
pub const SYMBOL_LITERAL_PREFIX: &str = "interloko.literal.symbol:";
/// Host-call symbol for immutable vector literals.
pub const VECTOR_LITERAL: &str = "interloko.literal.vector";
/// Host-call symbol for runtime value equality.
pub const VALUE_EQ: &str = "interloko.value_eq";
/// Host-call symbol for creating a class metaobject.
pub const META_CLASS_OBJECT: &str = "interloko.meta.class_object";
/// Host-call symbol for creating a generic-function metaobject.
pub const META_GENERIC_OBJECT: &str = "interloko.meta.generic_object";
/// Host-call symbol for creating a method metaobject.
pub const META_METHOD_OBJECT: &str = "interloko.meta.method_object";
/// Host-call symbol for creating a slot-descriptor metaobject.
pub const META_SLOT_DESCRIPTOR_OBJECT: &str = "interloko.meta.slot_descriptor_object";
/// Host-call symbol for reading a value's runtime class metaobject.
pub const MOP_OBJECT_CLASS: &str = "interloko.mop.object_class";
/// Host-call symbol for reading a class metaobject's name.
pub const MOP_CLASS_NAME: &str = "interloko.mop.class_name";
/// Host-call symbol for reading a class metaobject's direct superclass.
pub const MOP_CLASS_DIRECT_SUPERCLASS: &str = "interloko.mop.class_direct_superclass";
/// Host-call symbol for reading a class metaobject's direct slot descriptors.
pub const MOP_CLASS_DIRECT_SLOTS: &str = "interloko.mop.class_direct_slots";
/// Host-call symbol for reading a class metaobject's effective slot descriptors.
pub const MOP_CLASS_EFFECTIVE_SLOTS: &str = "interloko.mop.class_effective_slots";
/// Host-call symbol for reading a slot descriptor's source name.
pub const MOP_SLOT_NAME: &str = "interloko.mop.slot_name";
/// Host-call symbol for reading a slot descriptor's declared type text.
pub const MOP_SLOT_TYPE: &str = "interloko.mop.slot_type";
/// Host-call symbol for reading a slot descriptor's init keyword text.
pub const MOP_SLOT_INIT_KEYWORD: &str = "interloko.mop.slot_init_keyword";
/// Host-call symbol for reading a slot descriptor's mutability flag.
pub const MOP_SLOT_MUTABLE: &str = "interloko.mop.slot_mutable";
/// Host-call symbol for reading a slot descriptor's getter accessor name.
pub const MOP_SLOT_GETTER: &str = "interloko.mop.slot_getter";
/// Host-call symbol for reading a slot descriptor's setter generic.
pub const MOP_SLOT_SETTER: &str = "interloko.mop.slot_setter";
/// Host-call symbol for reading a generic-function metaobject's name.
pub const MOP_GENERIC_FUNCTION_NAME: &str = "interloko.mop.generic_function_name";
/// Host-call symbol for reading a generic-function metaobject's methods.
pub const MOP_GENERIC_FUNCTION_METHODS: &str = "interloko.mop.generic_function_methods";
/// Host-call symbol for reading a method metaobject's generic function.
pub const MOP_METHOD_GENERIC_FUNCTION: &str = "interloko.mop.method_generic_function";
/// Host-call symbol for reading a method metaobject's specializers.
pub const MOP_METHOD_SPECIALIZERS: &str = "interloko.mop.method_specializers";
/// Host-call symbol for immutable vector length.
pub const VECTOR_SIZE: &str = "interloko.vector_size";
/// Host-call symbol for immutable vector indexing.
pub const VECTOR_REF: &str = "interloko.vector_ref";

/// Returns the `execution_tape` value type for Cairn objects.
#[must_use]
pub const fn object_type() -> ValueType {
    ValueType::Obj(INTERLOKO_OBJECT_TYPE)
}

/// Returns the `execution_tape` value type for boxed top `<object>` values.
#[must_use]
pub const fn any_type() -> ValueType {
    ValueType::Obj(INTERLOKO_VALUE_TYPE)
}

/// Runtime-representable value category used by host signatures and slot IO.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// Boxed top object value.
    Any,
    /// Signed integer value.
    I64,
    /// Boolean value.
    Bool,
    /// String object value.
    String(ClassId),
    /// Symbol object value.
    Symbol(ClassId),
    /// Vector object value.
    Vector(ClassId),
    /// Runtime object value.
    Object(ClassId),
    /// Class metaobject for a specific class.
    ClassObject(ClassId),
    /// Generic-function metaobject for a specific generic.
    GenericFunctionObject(GenericFunctionId),
    /// Method metaobject for a specific method.
    MethodObject(MethodId),
    /// Slot descriptor metaobject for a specific effective slot.
    SlotDescriptorObject {
        /// Class whose effective slot vector contains the slot.
        class: ClassId,
        /// Slot id within the class's effective slot vector.
        slot: SlotId,
    },
}

/// Runtime method specializer. `None` means unspecialized / `<object>`.
pub type MethodSpecializer = Option<ClassId>;

/// Runtime class layout needed by the host object heap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeClass {
    /// Class name for diagnostics and inspection.
    pub name: String,
    /// Runtime object kind for this class.
    pub kind: HostObjectKind,
    /// Runtime superclass, if this class inherits from another native class.
    pub superclass: Option<ClassId>,
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
        kind: BuiltinValueClass,
    },
    /// Object is supplied by a Rust host adapter.
    HostBacked,
}

/// Runtime slot layout needed by host object operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSlot {
    /// Slot name for diagnostics and inspection.
    pub name: String,
    /// Declared source type name.
    pub type_name: String,
    /// Slot value category.
    pub kind: ValueKind,
    /// Class that introduced this effective slot.
    pub source_class: ClassId,
    /// Direct slot id on `source_class`.
    pub source_slot: SlotId,
    /// External construction keyword without the trailing colon.
    pub init_keyword: String,
    /// Whether this slot is writable through Cairn.
    pub mutable: bool,
    /// Getter accessor call name.
    pub getter: String,
    /// Generated setter generic, when one exists.
    pub setter: Option<GenericFunctionId>,
    /// Whether this slot is read through a Rust host adapter.
    pub host_backed: bool,
}

/// Runtime generic-function metadata needed by MOP host calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeGeneric {
    /// Generic function name.
    pub name: String,
    /// Method ids implementing this generic in semantic order.
    pub methods: Vec<MethodId>,
}

/// Runtime method metadata needed by MOP host calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeMethod {
    /// Generic function implemented by this method.
    pub generic: GenericFunctionId,
    /// Method specializers in call order.
    pub specializers: Vec<MethodSpecializer>,
}

/// Runtime layout emitted alongside a tape program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeLayout {
    /// Classes in semantic class-id order.
    pub classes: Vec<RuntimeClass>,
    /// Generic functions in semantic generic-id order.
    pub generics: Vec<RuntimeGeneric>,
    /// Methods in semantic method-id order.
    pub methods: Vec<RuntimeMethod>,
}

impl RuntimeLayout {
    /// Returns the runtime class id for a Cairn-visible class name.
    #[must_use]
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.classes
            .iter()
            .position(|class| class.name == name)
            .map(|index| ClassId(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// Returns true when `actual` is `expected` or inherits from it.
    #[must_use]
    pub fn is_same_or_subclass(&self, actual: ClassId, expected: ClassId) -> bool {
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
    class: ClassId,
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
    ClassMeta {
        class: ClassId,
    },
    GenericMeta {
        generic: GenericFunctionId,
    },
    MethodMeta {
        method: MethodId,
    },
    SlotDescriptor {
        class: ClassId,
        slot: SlotId,
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
    fn read_slot(&self, slot: SlotId, kind: ValueKind) -> Result<Value, HostError>;
}

/// Host implementation for Cairn runtime object operations.
pub struct InterlokoHost {
    layout: RuntimeLayout,
    objects: Vec<RuntimeObject>,
    values: Vec<Value>,
    symbols: Vec<InternedObject>,
}

impl InterlokoHost {
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
        class: ClassId,
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
            Value::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                Ok(self.push_boxed_value(Value::Obj(obj)))
            }
            Value::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => Ok(Value::Obj(obj)),
            _ => Err(HostError::Failed),
        }
    }

    fn object(&self, obj: Obj) -> Result<&RuntimeObject, HostError> {
        if obj.host_type != INTERLOKO_OBJECT_TYPE {
            return Err(HostError::SignatureMismatch);
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(HostError::Failed)?;
        self.objects.get(index).ok_or(HostError::Failed)
    }

    fn object_mut(&mut self, obj: Obj) -> Result<&mut RuntimeObject, HostError> {
        if obj.host_type != INTERLOKO_OBJECT_TYPE {
            return Err(HostError::SignatureMismatch);
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(HostError::Failed)?;
        self.objects.get_mut(index).ok_or(HostError::Failed)
    }

    fn boxed_value(&self, obj: Obj) -> Result<&Value, HostError> {
        if obj.host_type != INTERLOKO_VALUE_TYPE {
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
            host_type: INTERLOKO_VALUE_TYPE,
            handle,
        })
    }

    fn push_object(&mut self, object: RuntimeObject) -> Value {
        let handle = ObjHandle(u64::try_from(self.objects.len() + 1).unwrap_or(u64::MAX));
        self.objects.push(object);
        Value::Obj(Obj {
            host_type: INTERLOKO_OBJECT_TYPE,
            handle,
        })
    }
}

impl Host for InterlokoHost {
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
                let class = self.builtin_class(BuiltinValueClass::String)?;
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
                let class = self.builtin_class(BuiltinValueClass::Vector)?;
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
            META_CLASS_OBJECT => {
                let [ValueRef::U64(class_raw)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_class_meta(runtime_class_id(*class_raw)?)?;
                Ok(0)
            }
            META_GENERIC_OBJECT => {
                let [ValueRef::U64(generic_raw)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_generic_meta(runtime_generic_id(*generic_raw)?)?;
                Ok(0)
            }
            META_METHOD_OBJECT => {
                let [ValueRef::U64(method_raw)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_method_meta(runtime_method_id(*method_raw)?)?;
                Ok(0)
            }
            META_SLOT_DESCRIPTOR_OBJECT => {
                let [ValueRef::U64(class_raw), ValueRef::U64(slot_raw)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_slot_descriptor(
                    runtime_class_id(*class_raw)?,
                    runtime_slot_id(*slot_raw)?,
                )?;
                Ok(0)
            }
            MOP_OBJECT_CLASS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.class_of_any(*arg)?;
                let class = self.push_class_meta(class)?;
                rets[0] = self.push_boxed_value(class);
                Ok(0)
            }
            MOP_CLASS_NAME => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.meta_class_from_any(*arg)?;
                let name = self
                    .layout
                    .classes
                    .get(class.0 as usize)
                    .ok_or(HostError::Failed)?
                    .name
                    .clone();
                rets[0] = self.push_string(name)?;
                Ok(0)
            }
            MOP_CLASS_DIRECT_SUPERCLASS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.meta_class_from_any(*arg)?;
                rets[0] = if let Some(superclass) = self
                    .layout
                    .classes
                    .get(class.0 as usize)
                    .ok_or(HostError::Failed)?
                    .superclass
                {
                    let superclass = self.push_class_meta(superclass)?;
                    self.push_boxed_value(superclass)
                } else {
                    self.push_boxed_value(Value::Bool(false))
                };
                Ok(0)
            }
            MOP_CLASS_DIRECT_SLOTS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.meta_class_from_any(*arg)?;
                rets[0] = self.push_slot_descriptor_vector(class, true)?;
                Ok(0)
            }
            MOP_CLASS_EFFECTIVE_SLOTS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.meta_class_from_any(*arg)?;
                rets[0] = self.push_slot_descriptor_vector(class, false)?;
                Ok(0)
            }
            MOP_SLOT_NAME => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let slot = self.runtime_slot_from_any(*arg)?;
                rets[0] = self.push_string(slot.name.clone())?;
                Ok(0)
            }
            MOP_SLOT_TYPE => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let slot = self.runtime_slot_from_any(*arg)?;
                rets[0] = self.push_string(slot.type_name.clone())?;
                Ok(0)
            }
            MOP_SLOT_INIT_KEYWORD => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let slot = self.runtime_slot_from_any(*arg)?;
                rets[0] = self.push_string(slot.init_keyword.clone())?;
                Ok(0)
            }
            MOP_SLOT_MUTABLE => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let slot = self.runtime_slot_from_any(*arg)?;
                rets[0] = Value::Bool(slot.mutable);
                Ok(0)
            }
            MOP_SLOT_GETTER => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let slot = self.runtime_slot_from_any(*arg)?;
                rets[0] = self.push_string(slot.getter.clone())?;
                Ok(0)
            }
            MOP_SLOT_SETTER => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let setter = self.runtime_slot_from_any(*arg)?.setter;
                rets[0] = if let Some(setter) = setter {
                    let setter = self.push_generic_meta(setter)?;
                    self.push_boxed_value(setter)
                } else {
                    self.push_boxed_value(Value::Bool(false))
                };
                Ok(0)
            }
            MOP_GENERIC_FUNCTION_NAME => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let generic = self.meta_generic_from_any(*arg)?;
                let name = self
                    .layout
                    .generics
                    .get(generic.0 as usize)
                    .ok_or(HostError::Failed)?
                    .name
                    .clone();
                rets[0] = self.push_string(name)?;
                Ok(0)
            }
            MOP_GENERIC_FUNCTION_METHODS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let generic = self.meta_generic_from_any(*arg)?;
                let methods = self
                    .layout
                    .generics
                    .get(generic.0 as usize)
                    .ok_or(HostError::Failed)?
                    .methods
                    .clone();
                let mut elements = Vec::with_capacity(methods.len());
                for method in methods {
                    let method = self.push_method_meta(method)?;
                    elements.push(self.push_boxed_value(method));
                }
                rets[0] = self.push_vector(elements)?;
                Ok(0)
            }
            MOP_METHOD_GENERIC_FUNCTION => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let method = self.meta_method_from_any(*arg)?;
                let generic = self
                    .layout
                    .methods
                    .get(method.0 as usize)
                    .ok_or(HostError::Failed)?
                    .generic;
                let generic = self.push_generic_meta(generic)?;
                rets[0] = self.push_boxed_value(generic);
                Ok(0)
            }
            MOP_METHOD_SPECIALIZERS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let method = self.meta_method_from_any(*arg)?;
                let specializers = self
                    .layout
                    .methods
                    .get(method.0 as usize)
                    .ok_or(HostError::Failed)?
                    .specializers
                    .clone();
                let mut elements = Vec::with_capacity(specializers.len());
                for specializer in specializers {
                    let value = if let Some(class) = specializer {
                        self.push_class_meta(class)?
                    } else {
                        self.push_boxed_value(Value::Bool(false))
                    };
                    elements.push(self.box_top_value(value)?);
                }
                rets[0] = self.push_vector(elements)?;
                Ok(0)
            }
            VECTOR_SIZE => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::I64(
                    i64::try_from(self.vector_elements_from_any(*arg)?.len()).unwrap_or(i64::MAX),
                );
                Ok(0)
            }
            VECTOR_REF => {
                let [arg, ValueRef::I64(index)] = args else {
                    return Err(HostError::Failed);
                };
                let elements = self.vector_elements_from_any(*arg)?;
                let index = usize::try_from(*index).map_err(|_| HostError::Failed)?;
                rets[0] = elements.get(index).cloned().ok_or(HostError::Failed)?;
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
                    class: ClassId(u32::try_from(class_index).unwrap_or(u32::MAX)),
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
                let slot = SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX));
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
                    | RuntimeObjectStorage::Vector { .. }
                    | RuntimeObjectStorage::ClassMeta { .. }
                    | RuntimeObjectStorage::GenericMeta { .. }
                    | RuntimeObjectStorage::MethodMeta { .. }
                    | RuntimeObjectStorage::SlotDescriptor { .. } => {
                        return Err(HostError::Failed);
                    }
                    RuntimeObjectStorage::HostBacked { adapter } => {
                        adapter.read_slot(slot, slot_info.kind)?
                    }
                };
                Ok(0)
            }
            SLOT_SET => {
                let [ValueRef::Obj(obj), ValueRef::U64(slot_raw), value] = args else {
                    return Err(HostError::Failed);
                };
                let object_class = self.object(*obj)?.class;
                let slot_index = usize::try_from(*slot_raw).map_err(|_| HostError::Failed)?;
                let slot_info = self
                    .layout
                    .classes
                    .get(object_class.0 as usize)
                    .and_then(|class| class.slots.get(slot_index))
                    .ok_or(HostError::Failed)?;
                if slot_info.host_backed {
                    return Err(HostError::Failed);
                }
                let value = self.value_for_slot(*value, slot_info.kind)?;
                let object = self.object_mut(*obj)?;
                let RuntimeObjectStorage::Native { slots } = &mut object.storage else {
                    return Err(HostError::Failed);
                };
                let target = slots.get_mut(slot_index).ok_or(HostError::Failed)?;
                *target = value.clone();
                rets[0] = value;
                Ok(0)
            }
            BOX_VALUE => {
                let value = match args {
                    [ValueRef::I64(value)] => Value::I64(*value),
                    [ValueRef::Bool(value)] => Value::Bool(*value),
                    [ValueRef::U64(value)] => Value::U64(*value),
                    [ValueRef::Obj(obj)] if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                        Value::Obj(*obj)
                    }
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

impl InterlokoHost {
    fn builtin_class(&self, kind: BuiltinValueClass) -> Result<ClassId, HostError> {
        self.layout.class_id(kind.name()).ok_or(HostError::Failed)
    }

    fn intern_symbol(&mut self, name: String) -> Result<Value, HostError> {
        if let Some(entry) = self.symbols.iter().find(|entry| entry.name == name) {
            return Ok(Value::Obj(entry.object));
        }
        let class = self.builtin_class(BuiltinValueClass::Symbol)?;
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

    fn push_string(&mut self, value: String) -> Result<Value, HostError> {
        let class = self.builtin_class(BuiltinValueClass::String)?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::String { value },
        }))
    }

    fn push_vector(&mut self, elements: Vec<Value>) -> Result<Value, HostError> {
        let class = self.builtin_class(BuiltinValueClass::Vector)?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Vector { elements },
        }))
    }

    fn push_class_meta(&mut self, class: ClassId) -> Result<Value, HostError> {
        let _ = self
            .layout
            .classes
            .get(class.0 as usize)
            .ok_or(HostError::Failed)?;
        let meta_class = self.builtin_class(BuiltinValueClass::Class)?;
        Ok(self.push_object(RuntimeObject {
            class: meta_class,
            storage: RuntimeObjectStorage::ClassMeta { class },
        }))
    }

    fn push_generic_meta(&mut self, generic: GenericFunctionId) -> Result<Value, HostError> {
        let _ = self
            .layout
            .generics
            .get(generic.0 as usize)
            .ok_or(HostError::Failed)?;
        let meta_class = self.builtin_class(BuiltinValueClass::GenericFunction)?;
        Ok(self.push_object(RuntimeObject {
            class: meta_class,
            storage: RuntimeObjectStorage::GenericMeta { generic },
        }))
    }

    fn push_method_meta(&mut self, method: MethodId) -> Result<Value, HostError> {
        let _ = self
            .layout
            .methods
            .get(method.0 as usize)
            .ok_or(HostError::Failed)?;
        let meta_class = self.builtin_class(BuiltinValueClass::Method)?;
        Ok(self.push_object(RuntimeObject {
            class: meta_class,
            storage: RuntimeObjectStorage::MethodMeta { method },
        }))
    }

    fn push_slot_descriptor(&mut self, class: ClassId, slot: SlotId) -> Result<Value, HostError> {
        let _ = self
            .layout
            .classes
            .get(class.0 as usize)
            .and_then(|class| class.slots.get(slot.0 as usize))
            .ok_or(HostError::Failed)?;
        let meta_class = self.builtin_class(BuiltinValueClass::SlotDescriptor)?;
        Ok(self.push_object(RuntimeObject {
            class: meta_class,
            storage: RuntimeObjectStorage::SlotDescriptor { class, slot },
        }))
    }

    fn push_slot_descriptor_vector(
        &mut self,
        class: ClassId,
        direct_only: bool,
    ) -> Result<Value, HostError> {
        let slots = self
            .layout
            .classes
            .get(class.0 as usize)
            .ok_or(HostError::Failed)?
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot_index, slot)| {
                if !direct_only || slot.source_class == class {
                    Some(SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX)))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let mut elements = Vec::with_capacity(slots.len());
        for slot in slots {
            let descriptor = self.push_slot_descriptor(class, slot)?;
            elements.push(self.push_boxed_value(descriptor));
        }
        self.push_vector(elements)
    }

    fn object_from_any(&self, value: ValueRef<'_>) -> Result<&RuntimeObject, HostError> {
        let obj = match value {
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => obj,
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj)? {
                    Value::Obj(boxed) if boxed.host_type == INTERLOKO_OBJECT_TYPE => *boxed,
                    _ => return Err(HostError::Failed),
                }
            }
            _ => return Err(HostError::Failed),
        };
        self.object(obj)
    }

    fn class_of_any(&self, value: ValueRef<'_>) -> Result<ClassId, HostError> {
        match value {
            ValueRef::I64(_) => self.builtin_class(BuiltinValueClass::Integer),
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                Ok(self.object(obj)?.class)
            }
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj)? {
                    Value::I64(_) => self.builtin_class(BuiltinValueClass::Integer),
                    Value::Obj(boxed) if boxed.host_type == INTERLOKO_OBJECT_TYPE => {
                        Ok(self.object(*boxed)?.class)
                    }
                    _ => Err(HostError::Failed),
                }
            }
            _ => Err(HostError::Failed),
        }
    }

    fn meta_class_from_any(&self, value: ValueRef<'_>) -> Result<ClassId, HostError> {
        let object = self.object_from_any(value)?;
        match &object.storage {
            RuntimeObjectStorage::ClassMeta { class } => Ok(*class),
            _ => Err(HostError::Failed),
        }
    }

    fn meta_generic_from_any(&self, value: ValueRef<'_>) -> Result<GenericFunctionId, HostError> {
        let object = self.object_from_any(value)?;
        match &object.storage {
            RuntimeObjectStorage::GenericMeta { generic } => Ok(*generic),
            _ => Err(HostError::Failed),
        }
    }

    fn meta_method_from_any(&self, value: ValueRef<'_>) -> Result<MethodId, HostError> {
        let object = self.object_from_any(value)?;
        match &object.storage {
            RuntimeObjectStorage::MethodMeta { method } => Ok(*method),
            _ => Err(HostError::Failed),
        }
    }

    fn slot_descriptor_from_any(
        &self,
        value: ValueRef<'_>,
    ) -> Result<(ClassId, SlotId), HostError> {
        let object = self.object_from_any(value)?;
        match &object.storage {
            RuntimeObjectStorage::SlotDescriptor { class, slot } => Ok((*class, *slot)),
            _ => Err(HostError::Failed),
        }
    }

    fn runtime_slot_from_any(&self, value: ValueRef<'_>) -> Result<&RuntimeSlot, HostError> {
        let (class, slot) = self.slot_descriptor_from_any(value)?;
        self.layout
            .classes
            .get(class.0 as usize)
            .and_then(|class| class.slots.get(slot.0 as usize))
            .ok_or(HostError::Failed)
    }

    fn vector_elements_from_any(&self, value: ValueRef<'_>) -> Result<&[Value], HostError> {
        let object = self.object_from_any(value)?;
        match &object.storage {
            RuntimeObjectStorage::Vector { elements } => Ok(elements),
            _ => Err(HostError::Failed),
        }
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
            (
                RuntimeObjectStorage::ClassMeta { class: left },
                RuntimeObjectStorage::ClassMeta { class: right },
            ) => left == right,
            (
                RuntimeObjectStorage::GenericMeta { generic: left },
                RuntimeObjectStorage::GenericMeta { generic: right },
            ) => left == right,
            (
                RuntimeObjectStorage::MethodMeta { method: left },
                RuntimeObjectStorage::MethodMeta { method: right },
            ) => left == right,
            (
                RuntimeObjectStorage::SlotDescriptor {
                    class: left_class,
                    slot: left_slot,
                },
                RuntimeObjectStorage::SlotDescriptor {
                    class: right_class,
                    slot: right_slot,
                },
            ) => left_class == right_class && left_slot == right_slot,
            _ => false,
        })
    }

    fn value_object(&self, value: ValueRef<'_>) -> Option<Obj> {
        match value {
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => Some(obj),
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj).ok()? {
                    Value::Obj(boxed) if boxed.host_type == INTERLOKO_OBJECT_TYPE => Some(*boxed),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn value_is_instance(&self, value: ValueRef<'_>, class: ClassId) -> bool {
        self.value_object(value).is_some_and(|obj| {
            self.object(obj)
                .is_ok_and(|object| self.layout.is_same_or_subclass(object.class, class))
        })
    }

    fn value_for_slot(&self, value: ValueRef<'_>, kind: ValueKind) -> Result<Value, HostError> {
        match (kind, value) {
            (ValueKind::Any, ValueRef::Obj(obj)) => {
                let _ = self.boxed_value(obj)?;
                Ok(Value::Obj(obj))
            }
            (ValueKind::I64, ValueRef::I64(value)) => Ok(Value::I64(value)),
            (ValueKind::Bool, ValueRef::Bool(value)) => Ok(Value::Bool(value)),
            (
                ValueKind::Object(expected_class)
                | ValueKind::String(expected_class)
                | ValueKind::Symbol(expected_class)
                | ValueKind::Vector(expected_class),
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
            (ValueKind::ClassObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::ClassMeta { class } if *class == expected => {
                        Ok(Value::Obj(obj))
                    }
                    _ => Err(HostError::Failed),
                }
            }
            (ValueKind::GenericFunctionObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::GenericMeta { generic } if *generic == expected => {
                        Ok(Value::Obj(obj))
                    }
                    _ => Err(HostError::Failed),
                }
            }
            (ValueKind::MethodObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::MethodMeta { method } if *method == expected => {
                        Ok(Value::Obj(obj))
                    }
                    _ => Err(HostError::Failed),
                }
            }
            (ValueKind::SlotDescriptorObject { class, slot }, ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::SlotDescriptor {
                        class: actual_class,
                        slot: actual_slot,
                    } if *actual_class == class && *actual_slot == slot => Ok(Value::Obj(obj)),
                    _ => Err(HostError::Failed),
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
pub fn slot_get_signature(kind: ValueKind) -> Option<execution_tape::host::HostSig> {
    Some(execution_tape::host::HostSig {
        args: vec![object_type(), ValueType::U64],
        rets: vec![value_type(kind)?],
    })
}

/// Host signature for `slot_set`.
#[must_use]
pub fn slot_set_signature(kind: ValueKind) -> Option<execution_tape::host::HostSig> {
    Some(execution_tape::host::HostSig {
        args: vec![object_type(), ValueType::U64, value_type(kind)?],
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

/// Host signature for constructing a class metaobject from a class id.
#[must_use]
pub fn meta_class_object_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a generic-function metaobject.
#[must_use]
pub fn meta_generic_object_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a method metaobject.
#[must_use]
pub fn meta_method_object_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a slot descriptor metaobject.
#[must_use]
pub fn meta_slot_descriptor_object_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![ValueType::U64, ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for a one-argument metaobject query returning an object.
#[must_use]
pub fn meta_query_object_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![any_type()],
        rets: vec![object_type()],
    }
}

/// Host signature for a one-argument metaobject query returning `<object>`.
#[must_use]
pub fn meta_query_any_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![any_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for a one-argument metaobject query returning `<boolean>`.
#[must_use]
pub fn meta_query_bool_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![any_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for immutable vector length.
#[must_use]
pub fn vector_size_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![any_type()],
        rets: vec![ValueType::I64],
    }
}

/// Host signature for immutable vector indexing.
#[must_use]
pub fn vector_ref_signature() -> execution_tape::host::HostSig {
    execution_tape::host::HostSig {
        args: vec![any_type(), ValueType::I64],
        rets: vec![any_type()],
    }
}

fn value_type(kind: ValueKind) -> Option<ValueType> {
    match kind {
        ValueKind::Any => Some(any_type()),
        ValueKind::I64 => Some(ValueType::I64),
        ValueKind::Bool => Some(ValueType::Bool),
        ValueKind::String(_)
        | ValueKind::Symbol(_)
        | ValueKind::Vector(_)
        | ValueKind::Object(_)
        | ValueKind::ClassObject(_)
        | ValueKind::GenericFunctionObject(_)
        | ValueKind::MethodObject(_)
        | ValueKind::SlotDescriptorObject { .. } => Some(object_type()),
    }
}

fn runtime_class_id(raw: u64) -> Result<ClassId, HostError> {
    Ok(ClassId(u32::try_from(raw).map_err(|_| HostError::Failed)?))
}

fn runtime_generic_id(raw: u64) -> Result<GenericFunctionId, HostError> {
    Ok(GenericFunctionId(
        u32::try_from(raw).map_err(|_| HostError::Failed)?,
    ))
}

fn runtime_method_id(raw: u64) -> Result<MethodId, HostError> {
    Ok(MethodId(u32::try_from(raw).map_err(|_| HostError::Failed)?))
}

fn runtime_slot_id(raw: u64) -> Result<SlotId, HostError> {
    Ok(SlotId(u32::try_from(raw).map_err(|_| HostError::Failed)?))
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
