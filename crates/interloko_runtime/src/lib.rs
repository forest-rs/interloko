//! Runtime layout, host-call signatures, and `execution_tape` host support.
//!
//! This crate owns the executable runtime boundary for Interloko-shaped
//! programs. It intentionally does not own Loko syntax, semantic IR, lowering,
//! sessions, or compiler query state.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[cfg(test)]
use execution_tape::host::AccessSink;
use execution_tape::host::{
    Host, HostContext, HostError, HostSig, ResourceKeyRef, SigHash, ValueRef,
};
use execution_tape::program::{HostTypeId, ValueType};
use execution_tape::value::{Obj, ObjHandle, Value};
use interloko_model::{
    BuiltinValueClass, ClassId, FloatBits, GenericFunctionId, MethodId, OBJECT_CLASS_ID, SlotId,
};

/// Host object type used for Interloko runtime objects.
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
/// Host-call symbol for constructing a string object from an execution string.
pub const STRING_LITERAL: &str = "interloko.literal.string";
/// Host-call symbol for constructing an interned symbol object from an execution string.
pub const SYMBOL_LITERAL: &str = "interloko.literal.symbol";
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
/// Host-call symbol for rendering a float as its shortest round-trip literal.
pub const FLOAT_TO_STRING: &str = "interloko.float_to_string";
/// Host-call symbol for rendering an integer in decimal.
pub const INTEGER_TO_STRING: &str = "interloko.integer_to_string";
/// Host-call symbol for concatenating two strings into a new string.
pub const STRING_CONCATENATE: &str = "interloko.string_concatenate";

/// Float library function served by the runtime host.
///
/// These are the float operations `execution_tape` has no instruction for.
/// Each is a host call named [`FloatFunction::symbol`] taking
/// [`FloatFunction::arity`] `f64` arguments and returning one `f64`.
/// Implementations come from the pure-Rust `libm` crate rather than the
/// platform C library, so results are identical on every host, which keeps
/// recorded runs replayable. Rounding functions follow IEEE 754 and keep
/// the sign of zero; `round` rounds half-way cases to even.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FloatFunction {
    /// Largest integral value not greater than the argument.
    Floor,
    /// Smallest integral value not less than the argument.
    Ceiling,
    /// Nearest integral value, ties to even.
    Round,
    /// Integral part, rounding toward zero.
    Truncate,
    /// Correctly rounded square root; NaN for negative arguments.
    Sqrt,
    /// Cube root.
    Cbrt,
    /// `e` raised to the argument.
    Exp,
    /// Natural logarithm.
    Log,
    /// Base-2 logarithm.
    Log2,
    /// Base-10 logarithm.
    Log10,
    /// Sine of an angle in radians.
    Sin,
    /// Cosine of an angle in radians.
    Cos,
    /// Tangent of an angle in radians.
    Tan,
    /// Arcsine, in radians.
    Asin,
    /// Arccosine, in radians.
    Acos,
    /// Arctangent, in radians.
    Atan,
    /// Two-argument arctangent `atan2(y, x)`, in radians.
    Atan2,
    /// Hyperbolic sine.
    Sinh,
    /// Hyperbolic cosine.
    Cosh,
    /// Hyperbolic tangent.
    Tanh,
    /// `pow(base, exponent)` with C99 special cases.
    Pow,
    /// `hypot(x, y)`, the Euclidean norm without undue overflow.
    Hypot,
}

impl FloatFunction {
    /// Every float library function in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Floor,
        Self::Ceiling,
        Self::Round,
        Self::Truncate,
        Self::Sqrt,
        Self::Cbrt,
        Self::Exp,
        Self::Log,
        Self::Log2,
        Self::Log10,
        Self::Sin,
        Self::Cos,
        Self::Tan,
        Self::Asin,
        Self::Acos,
        Self::Atan,
        Self::Atan2,
        Self::Sinh,
        Self::Cosh,
        Self::Tanh,
        Self::Pow,
        Self::Hypot,
    ];

    /// Returns the host-call symbol that serves this function.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Floor => "interloko.float.floor",
            Self::Ceiling => "interloko.float.ceiling",
            Self::Round => "interloko.float.round",
            Self::Truncate => "interloko.float.truncate",
            Self::Sqrt => "interloko.float.sqrt",
            Self::Cbrt => "interloko.float.cbrt",
            Self::Exp => "interloko.float.exp",
            Self::Log => "interloko.float.log",
            Self::Log2 => "interloko.float.log2",
            Self::Log10 => "interloko.float.log10",
            Self::Sin => "interloko.float.sin",
            Self::Cos => "interloko.float.cos",
            Self::Tan => "interloko.float.tan",
            Self::Asin => "interloko.float.asin",
            Self::Acos => "interloko.float.acos",
            Self::Atan => "interloko.float.atan",
            Self::Atan2 => "interloko.float.atan2",
            Self::Sinh => "interloko.float.sinh",
            Self::Cosh => "interloko.float.cosh",
            Self::Tanh => "interloko.float.tanh",
            Self::Pow => "interloko.float.pow",
            Self::Hypot => "interloko.float.hypot",
        }
    }

    /// Returns the function served by host-call `symbol`, if any.
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|function| function.symbol() == symbol)
    }

    /// Returns the number of `f64` arguments.
    #[must_use]
    pub const fn arity(self) -> usize {
        match self {
            Self::Atan2 | Self::Pow | Self::Hypot => 2,
            _ => 1,
        }
    }

    /// Returns the host-call signature: `arity` floats in, one float out.
    #[must_use]
    pub fn signature(self) -> HostSig {
        HostSig {
            args: vec![ValueType::F64; self.arity()],
            rets: vec![ValueType::F64],
        }
    }

    /// Applies the function. Missing arguments read as NaN.
    #[must_use]
    pub fn apply(self, args: &[f64]) -> f64 {
        let x = args.first().copied().unwrap_or(f64::NAN);
        let y = args.get(1).copied().unwrap_or(f64::NAN);
        match self {
            Self::Floor => libm::floor(x),
            Self::Ceiling => libm::ceil(x),
            Self::Round => libm::roundeven(x),
            Self::Truncate => libm::trunc(x),
            Self::Sqrt => libm::sqrt(x),
            Self::Cbrt => libm::cbrt(x),
            Self::Exp => libm::exp(x),
            Self::Log => libm::log(x),
            Self::Log2 => libm::log2(x),
            Self::Log10 => libm::log10(x),
            Self::Sin => libm::sin(x),
            Self::Cos => libm::cos(x),
            Self::Tan => libm::tan(x),
            Self::Asin => libm::asin(x),
            Self::Acos => libm::acos(x),
            Self::Atan => libm::atan(x),
            Self::Atan2 => libm::atan2(x, y),
            Self::Sinh => libm::sinh(x),
            Self::Cosh => libm::cosh(x),
            Self::Tanh => libm::tanh(x),
            Self::Pow => libm::pow(x, y),
            Self::Hypot => libm::hypot(x, y),
        }
    }
}

/// Returns the host signature for [`FLOAT_TO_STRING`].
#[must_use]
pub fn float_to_string_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::F64],
        rets: vec![object_type()],
    }
}

/// Returns the host signature for [`INTEGER_TO_STRING`].
#[must_use]
pub fn integer_to_string_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::I64],
        rets: vec![object_type()],
    }
}

/// Returns the host signature for [`STRING_CONCATENATE`].
#[must_use]
pub fn string_concatenate_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), object_type()],
        rets: vec![object_type()],
    }
}

/// Returns the `execution_tape` value type for Interloko objects.
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
    /// IEEE 754 binary64 float value.
    F64,
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

/// Runtime method specializer. `None` is reserved for invalid or legacy
/// compatibility records; source `<object>` is `OBJECT_CLASS_ID`.
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

/// Runtime object storage kind for a nominal Interloko class.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HostObjectKind {
    /// Object is allocated and stored by the Interloko runtime heap.
    Native,
    /// Object is a built-in value allocated by the Interloko runtime heap.
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
    /// Whether this slot is writable through Interloko.
    pub mutable: bool,
    /// Getter accessor call name.
    pub getter: String,
    /// Generated setter generic, when one exists.
    pub setter: Option<GenericFunctionId>,
    /// Whether this slot is read through a Rust host adapter.
    pub host_backed: bool,
}

impl RuntimeSlot {
    /// Returns the executable storage protocol for this slot.
    #[must_use]
    pub const fn storage(&self) -> RuntimeSlotStorage {
        if self.host_backed {
            RuntimeSlotStorage::HostBacked
        } else {
            RuntimeSlotStorage::Native
        }
    }

    /// Returns the slot's runtime access capability.
    #[must_use]
    pub const fn capability(&self) -> RuntimeSlotCapability {
        let storage = self.storage();
        let write_rejection = if self.host_backed {
            Some(RuntimeSlotWriteRejection::HostBackedSlot)
        } else if self.mutable {
            None
        } else {
            Some(RuntimeSlotWriteRejection::ConstantSlot)
        };
        RuntimeSlotCapability {
            storage,
            readable: true,
            writable: write_rejection.is_none(),
            write_rejection,
        }
    }
}

/// Executable storage protocol for a runtime slot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RuntimeSlotStorage {
    /// Slot is stored in Interloko's native object heap.
    Native,
    /// Slot value is read from a Rust host adapter.
    HostBacked,
}

/// Runtime slot read/write capability.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSlotCapability {
    /// Storage protocol used by the slot.
    pub storage: RuntimeSlotStorage,
    /// Whether the slot can be read through the runtime slot protocol.
    pub readable: bool,
    /// Whether the slot can be written through the runtime slot protocol.
    pub writable: bool,
    /// Reason writes are rejected, when not writable.
    pub write_rejection: Option<RuntimeSlotWriteRejection>,
}

/// Structural reason a runtime slot write is rejected.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RuntimeSlotWriteRejection {
    /// Slot was declared constant.
    ConstantSlot,
    /// Slot is supplied by a host adapter and is read-only in this slice.
    HostBackedSlot,
    /// The receiver's storage is not a native slot vector.
    NonNativeObject,
}

/// Runtime generic-function metadata needed by MOP host calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeGeneric {
    /// Generic function name.
    pub name: String,
    /// Number of positional parameters the generic declares. Every method
    /// listed under this generic must have exactly this many specializers.
    pub arity: usize,
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

/// Structured error from runtime object protocol operations.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// Runtime layout omits a built-in value class required by an operation.
    MissingBuiltinClass {
        /// Stable source-level class name.
        name: &'static str,
    },
    /// Runtime layout does not contain the requested class.
    InvalidClass {
        /// Invalid class id.
        class: ClassId,
    },
    /// Runtime layout does not contain the requested generic function.
    InvalidGeneric {
        /// Invalid generic id.
        generic: GenericFunctionId,
    },
    /// Runtime layout does not contain the requested method.
    InvalidMethod {
        /// Invalid method id.
        method: MethodId,
    },
    /// Runtime layout does not contain the requested effective slot.
    InvalidSlot {
        /// Class whose slot table was queried.
        class: ClassId,
        /// Invalid effective slot id.
        slot: SlotId,
    },
    /// Native object construction was requested for a non-native class.
    ClassIsNotNative {
        /// Requested class.
        class: ClassId,
        /// Actual runtime class kind.
        kind: HostObjectKind,
    },
    /// Host-backed object installation was requested for a non-host-backed class.
    ClassIsNotHostBacked {
        /// Requested class.
        class: ClassId,
        /// Actual runtime class kind.
        kind: HostObjectKind,
    },
    /// Object construction field count does not match the runtime class layout.
    FieldArityMismatch {
        /// Requested class.
        class: ClassId,
        /// Expected layout field count.
        expected: usize,
        /// Actual field count.
        actual: usize,
    },
    /// Method insertion specializer count does not match the generic's arity.
    MethodArityMismatch {
        /// Target generic.
        generic: GenericFunctionId,
        /// Generic's declared arity.
        expected: usize,
        /// Specializer count supplied by the caller.
        actual: usize,
    },
    /// A host object handle did not use the Interloko object host type.
    ObjectHostTypeMismatch {
        /// Object supplied by the caller.
        object: Obj,
        /// Expected host object type.
        expected: HostTypeId,
    },
    /// A boxed value handle did not use the Interloko boxed-value host type.
    ValueHostTypeMismatch {
        /// Object supplied by the caller.
        object: Obj,
        /// Expected host object type.
        expected: HostTypeId,
    },
    /// A host object handle was well typed but unknown to this runtime host.
    InvalidObjectHandle {
        /// Unknown object.
        object: Obj,
    },
    /// A boxed value handle was well typed but unknown to this runtime host.
    InvalidBoxedValueHandle {
        /// Unknown boxed value object.
        object: Obj,
    },
    /// A runtime value could not be represented as boxed top `<object>`.
    CannotBoxValue,
    /// A value did not match the expected runtime value category.
    ValueKindMismatch {
        /// Expected value kind.
        expected: ValueKind,
    },
    /// Slot writes are not permitted for this slot/receiver combination.
    SlotWriteRejected {
        /// Receiver runtime class.
        class: ClassId,
        /// Effective slot id.
        slot: SlotId,
        /// Structural rejection reason.
        reason: RuntimeSlotWriteRejection,
    },
    /// A host object adapter could not read a requested slot.
    HostAdapterReadFailed {
        /// Effective slot id requested from the adapter.
        slot: SlotId,
    },
}

impl RuntimeError {
    /// Maps this structured runtime error into the coarse `execution_tape` host ABI error.
    #[must_use]
    pub const fn host_error(&self) -> HostError {
        match self {
            Self::ObjectHostTypeMismatch { .. } | Self::ValueHostTypeMismatch { .. } => {
                HostError::SignatureMismatch
            }
            _ => HostError::Failed,
        }
    }
}

impl From<RuntimeError> for HostError {
    fn from(error: RuntimeError) -> Self {
        error.host_error()
    }
}

/// First invalid invariant found while checking a [`RuntimeLayout`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeLayoutError {
    /// The class vector does not contain `OBJECT_CLASS_ID`.
    MissingObjectRoot,
    /// Class `OBJECT_CLASS_ID` is not named `<object>`.
    ObjectRootName,
    /// Class `OBJECT_CLASS_ID` is not a native runtime class.
    ObjectRootKind,
    /// Class `OBJECT_CLASS_ID` incorrectly has a superclass.
    ObjectRootHasSuperclass {
        /// Superclass attached to the root.
        superclass: ClassId,
    },
    /// A non-root class has no direct superclass.
    ClassMissingSuperclass {
        /// Class with no direct superclass.
        class: ClassId,
    },
    /// A class points at a superclass outside the class table.
    InvalidSuperclass {
        /// Class whose superclass reference is invalid.
        class: ClassId,
        /// Invalid superclass id.
        superclass: ClassId,
    },
    /// A class's inheritance chain loops.
    InheritanceCycle {
        /// Class whose chain loops.
        class: ClassId,
    },
    /// A class's inheritance chain ends before reaching `<object>`.
    InheritanceDoesNotReachRoot {
        /// Class whose chain misses the root.
        class: ClassId,
    },
    /// A runtime slot points at a missing source class.
    InvalidSlotSourceClass {
        /// Class containing the effective slot.
        class: ClassId,
        /// Effective slot id.
        slot: SlotId,
        /// Invalid source class.
        source_class: ClassId,
    },
    /// A runtime slot points at a source slot that is not present on its source class.
    InvalidSlotSourceSlot {
        /// Class containing the effective slot.
        class: ClassId,
        /// Effective slot id.
        slot: SlotId,
        /// Source class.
        source_class: ClassId,
        /// Invalid source slot.
        source_slot: SlotId,
    },
    /// A generic's method list points at a missing method.
    InvalidGenericMethod {
        /// Generic containing the bad method reference.
        generic: GenericFunctionId,
        /// Invalid method id.
        method: MethodId,
    },
    /// A generic lists a method that belongs to another generic.
    GenericMethodMismatch {
        /// Generic containing the mismatched method reference.
        generic: GenericFunctionId,
        /// Method id.
        method: MethodId,
        /// Method's actual generic.
        actual: GenericFunctionId,
    },
    /// A method points at a missing generic.
    InvalidMethodGeneric {
        /// Method containing the bad generic reference.
        method: MethodId,
        /// Invalid generic id.
        generic: GenericFunctionId,
    },
    /// A method is absent from its generic's method list.
    MethodMissingFromGeneric {
        /// Method that is not listed by its generic.
        method: MethodId,
        /// Generic implemented by the method.
        generic: GenericFunctionId,
    },
    /// A method's specializer count does not match the owning generic's arity.
    MethodArityMismatch {
        /// Method whose specializer count disagrees with the generic arity.
        method: MethodId,
        /// Owning generic.
        generic: GenericFunctionId,
        /// Generic's declared arity.
        expected: usize,
        /// Method specializer count.
        actual: usize,
    },
    /// A method contains a legacy empty specializer.
    MissingMethodSpecializer {
        /// Method containing the empty specializer.
        method: MethodId,
        /// Zero-based argument index.
        argument: usize,
    },
    /// A method specializer points at a missing class.
    InvalidMethodSpecializer {
        /// Method containing the bad specializer.
        method: MethodId,
        /// Zero-based argument index.
        argument: usize,
        /// Invalid class id.
        class: ClassId,
    },
}

/// Error returned by exact runtime generic dispatch lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeDispatchError {
    /// The requested generic is not present in the layout.
    InvalidGeneric {
        /// Invalid generic id.
        generic: GenericFunctionId,
    },
    /// One exact runtime argument class is not present in the layout.
    InvalidActualClass {
        /// Zero-based argument index.
        argument: usize,
        /// Invalid class id.
        class: ClassId,
    },
    /// A method referenced by the generic is not present in the layout.
    InvalidMethod {
        /// Invalid method id.
        method: MethodId,
    },
}

/// Exact runtime generic dispatch selection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RuntimeMethodSelection {
    /// No method is applicable.
    None,
    /// Exactly one most-specific method applies.
    Selected(MethodId),
    /// Multiple incomparable applicable methods remain.
    Ambiguous,
}

/// Structured result of exact runtime generic dispatch lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDispatchLookup {
    /// Generic being dispatched.
    pub generic: GenericFunctionId,
    /// Exact runtime argument classes.
    pub actuals: Vec<ClassId>,
    /// Applicable methods in most-specific order.
    pub applicable: Vec<RuntimeDispatchCandidate>,
    /// Methods rejected for this exact argument tuple.
    pub rejected: Vec<RuntimeDispatchRejection>,
    /// Final exact dispatch selection.
    pub selection: RuntimeMethodSelection,
}

impl RuntimeDispatchLookup {
    /// Returns the selected method id, if lookup found a unique method.
    #[must_use]
    pub const fn selected_method(&self) -> Option<MethodId> {
        match self.selection {
            RuntimeMethodSelection::Selected(method) => Some(method),
            RuntimeMethodSelection::None | RuntimeMethodSelection::Ambiguous => None,
        }
    }
}

/// Runtime method that can apply to an exact argument tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDispatchCandidate {
    /// Method id.
    pub method: MethodId,
    /// Method specializers in argument order.
    pub specializers: Vec<MethodSpecializer>,
    /// Per-argument distance from exact class to specializer.
    pub distances: Vec<usize>,
    /// Whether this method is the unique selected method.
    pub selected: bool,
}

/// Runtime method that cannot apply to an exact argument tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDispatchRejection {
    /// Method id.
    pub method: MethodId,
    /// Method specializers in argument order.
    pub specializers: Vec<MethodSpecializer>,
    /// Structural rejection reason.
    pub reason: RuntimeDispatchRejectionReason,
}

/// Structural reason a runtime method cannot apply.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RuntimeDispatchRejectionReason {
    /// Method arity does not match the runtime argument tuple.
    ArityMismatch {
        /// Method specializer count.
        expected: usize,
        /// Runtime argument count.
        actual: usize,
    },
    /// Method has a legacy empty specializer entry.
    MissingSpecializer {
        /// Zero-based argument index.
        argument: usize,
    },
    /// Runtime argument class is not an instance of the method specializer.
    ArgumentNotInstance {
        /// Zero-based argument index.
        argument: usize,
        /// Exact runtime argument class.
        actual: ClassId,
        /// Method specializer class.
        specializer: ClassId,
    },
}

impl RuntimeLayout {
    /// Returns all runtime classes in class-id order.
    #[must_use]
    pub fn classes(&self) -> &[RuntimeClass] {
        &self.classes
    }

    /// Returns all runtime generics in generic-id order.
    #[must_use]
    pub fn generics(&self) -> &[RuntimeGeneric] {
        &self.generics
    }

    /// Returns all runtime methods in method-id order.
    #[must_use]
    pub fn methods(&self) -> &[RuntimeMethod] {
        &self.methods
    }

    /// Returns the `<object>` root class.
    #[must_use]
    pub fn object_root(&self) -> Option<&RuntimeClass> {
        self.class(OBJECT_CLASS_ID)
    }

    /// Returns the runtime class id for a Loko-visible class name.
    #[must_use]
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.classes
            .iter()
            .position(|class| class.name == name)
            .map(|index| ClassId(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// Returns the runtime class id and layout for a Loko-visible class name.
    #[must_use]
    pub fn class_by_name(&self, name: &str) -> Option<(ClassId, &RuntimeClass)> {
        let id = self.class_id(name)?;
        self.class(id).map(|class| (id, class))
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

    /// Returns the distance from `actual` to `expected` in the class lattice.
    #[must_use]
    pub fn inheritance_distance(&self, actual: ClassId, expected: ClassId) -> Option<usize> {
        let mut current = Some(actual);
        let mut distance = 0;
        let mut visited = 0;
        while let Some(class) = current {
            if class == expected {
                return Some(distance);
            }
            visited += 1;
            if visited > self.classes.len() {
                return None;
            }
            current = self
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.superclass);
            distance += 1;
        }
        None
    }

    /// Returns one runtime class by id.
    #[must_use]
    pub fn class(&self, class: ClassId) -> Option<&RuntimeClass> {
        self.classes.get(class.0 as usize)
    }

    /// Returns superclass ids from direct superclass up to `<object>`.
    #[must_use]
    pub fn superclass_chain(&self, class: ClassId) -> Option<Vec<ClassId>> {
        let mut current = self.class(class)?.superclass;
        let mut chain = Vec::new();
        let mut visited = 0;
        while let Some(class_id) = current {
            chain.push(class_id);
            visited += 1;
            if visited > self.classes.len() {
                return None;
            }
            current = self.class(class_id)?.superclass;
        }
        Some(chain)
    }

    /// Returns effective slots for one runtime class.
    #[must_use]
    pub fn effective_slots(&self, class: ClassId) -> Option<&[RuntimeSlot]> {
        self.class(class).map(|class| class.slots.as_slice())
    }

    /// Returns one runtime effective slot by class and slot id.
    #[must_use]
    pub fn slot(&self, class: ClassId, slot: SlotId) -> Option<&RuntimeSlot> {
        self.class(class)
            .and_then(|class| class.slots.get(slot.0 as usize))
    }

    /// Returns runtime slot access capability for one effective slot.
    #[must_use]
    pub fn slot_capability(&self, class: ClassId, slot: SlotId) -> Option<RuntimeSlotCapability> {
        self.slot(class, slot).map(RuntimeSlot::capability)
    }

    /// Returns the runtime generic id for a generic-function name.
    #[must_use]
    pub fn generic_id(&self, name: &str) -> Option<GenericFunctionId> {
        self.generics
            .iter()
            .position(|generic| generic.name == name)
            .map(|index| GenericFunctionId(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// Returns one runtime generic by id.
    #[must_use]
    pub fn generic(&self, generic: GenericFunctionId) -> Option<&RuntimeGeneric> {
        self.generics.get(generic.0 as usize)
    }

    /// Returns the method ids implementing one runtime generic.
    #[must_use]
    pub fn methods_for_generic(&self, generic: GenericFunctionId) -> Option<&[MethodId]> {
        self.generic(generic)
            .map(|generic| generic.methods.as_slice())
    }

    /// Returns one runtime method by id.
    #[must_use]
    pub fn method(&self, method: MethodId) -> Option<&RuntimeMethod> {
        self.methods.get(method.0 as usize)
    }

    /// Validates runtime layout invariants required by object storage and dispatch.
    pub fn validate(&self) -> Result<(), RuntimeLayoutError> {
        let root = self
            .classes
            .get(OBJECT_CLASS_ID.0 as usize)
            .ok_or(RuntimeLayoutError::MissingObjectRoot)?;
        if root.name != "<object>" {
            return Err(RuntimeLayoutError::ObjectRootName);
        }
        if root.kind != HostObjectKind::Native {
            return Err(RuntimeLayoutError::ObjectRootKind);
        }
        if let Some(superclass) = root.superclass {
            return Err(RuntimeLayoutError::ObjectRootHasSuperclass { superclass });
        }

        for (index, class) in self.classes.iter().enumerate() {
            let class_id = ClassId(u32::try_from(index).unwrap_or(u32::MAX));
            if class_id == OBJECT_CLASS_ID {
                continue;
            }
            let superclass = class
                .superclass
                .ok_or(RuntimeLayoutError::ClassMissingSuperclass { class: class_id })?;
            if self.class(superclass).is_none() {
                return Err(RuntimeLayoutError::InvalidSuperclass {
                    class: class_id,
                    superclass,
                });
            }
            self.validate_chain_reaches_root(class_id)?;
        }

        for (class_index, class) in self.classes.iter().enumerate() {
            let class_id = ClassId(u32::try_from(class_index).unwrap_or(u32::MAX));
            for (slot_index, slot) in class.slots.iter().enumerate() {
                let slot_id = SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX));
                let Some(source_class) = self.class(slot.source_class) else {
                    return Err(RuntimeLayoutError::InvalidSlotSourceClass {
                        class: class_id,
                        slot: slot_id,
                        source_class: slot.source_class,
                    });
                };
                if source_class.slots.iter().all(|candidate| {
                    candidate.source_class != slot.source_class
                        || candidate.source_slot != slot.source_slot
                }) {
                    return Err(RuntimeLayoutError::InvalidSlotSourceSlot {
                        class: class_id,
                        slot: slot_id,
                        source_class: slot.source_class,
                        source_slot: slot.source_slot,
                    });
                }
            }
        }

        for (generic_index, generic) in self.generics.iter().enumerate() {
            let generic_id = GenericFunctionId(u32::try_from(generic_index).unwrap_or(u32::MAX));
            for method in &generic.methods {
                let method_info = self.methods.get(method.0 as usize).ok_or(
                    RuntimeLayoutError::InvalidGenericMethod {
                        generic: generic_id,
                        method: *method,
                    },
                )?;
                if method_info.generic != generic_id {
                    return Err(RuntimeLayoutError::GenericMethodMismatch {
                        generic: generic_id,
                        method: *method,
                        actual: method_info.generic,
                    });
                }
            }
        }

        for (method_index, method) in self.methods.iter().enumerate() {
            let method_id = MethodId(u32::try_from(method_index).unwrap_or(u32::MAX));
            let generic = self.generics.get(method.generic.0 as usize).ok_or(
                RuntimeLayoutError::InvalidMethodGeneric {
                    method: method_id,
                    generic: method.generic,
                },
            )?;
            if !generic.methods.contains(&method_id) {
                return Err(RuntimeLayoutError::MethodMissingFromGeneric {
                    method: method_id,
                    generic: method.generic,
                });
            }
            if method.specializers.len() != generic.arity {
                return Err(RuntimeLayoutError::MethodArityMismatch {
                    method: method_id,
                    generic: method.generic,
                    expected: generic.arity,
                    actual: method.specializers.len(),
                });
            }
            for (argument, specializer) in method.specializers.iter().enumerate() {
                let Some(class) = specializer else {
                    return Err(RuntimeLayoutError::MissingMethodSpecializer {
                        method: method_id,
                        argument,
                    });
                };
                if self.class(*class).is_none() {
                    return Err(RuntimeLayoutError::InvalidMethodSpecializer {
                        method: method_id,
                        argument,
                        class: *class,
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_chain_reaches_root(&self, class: ClassId) -> Result<(), RuntimeLayoutError> {
        let mut current = Some(class);
        let mut visited = 0;
        while let Some(class_id) = current {
            if class_id == OBJECT_CLASS_ID {
                return Ok(());
            }
            visited += 1;
            if visited > self.classes.len() {
                return Err(RuntimeLayoutError::InheritanceCycle { class });
            }
            current = self.class(class_id).and_then(|class| class.superclass);
        }
        Err(RuntimeLayoutError::InheritanceDoesNotReachRoot { class })
    }

    /// Performs exact runtime generic dispatch lookup.
    pub fn dispatch_lookup(
        &self,
        generic: GenericFunctionId,
        actuals: &[ClassId],
    ) -> Result<RuntimeDispatchLookup, RuntimeDispatchError> {
        for (argument, class) in actuals.iter().copied().enumerate() {
            if self.class(class).is_none() {
                return Err(RuntimeDispatchError::InvalidActualClass { argument, class });
            }
        }
        let generic_info = self
            .generics
            .get(generic.0 as usize)
            .ok_or(RuntimeDispatchError::InvalidGeneric { generic })?;
        let mut methods = generic_info.methods.clone();
        for method in &methods {
            if self.methods.get(method.0 as usize).is_none() {
                return Err(RuntimeDispatchError::InvalidMethod { method: *method });
            }
        }
        methods.sort_by(|left_id, right_id| {
            let left = &self.methods[left_id.0 as usize];
            let right = &self.methods[right_id.0 as usize];
            if self.method_more_specific(left, right) {
                core::cmp::Ordering::Less
            } else if self.method_more_specific(right, left) {
                core::cmp::Ordering::Greater
            } else {
                left_id.0.cmp(&right_id.0)
            }
        });

        let maximal = methods
            .iter()
            .copied()
            .filter(|candidate| {
                let candidate_method = &self.methods[candidate.0 as usize];
                self.method_applies_to_classes(candidate_method, actuals)
                    && !methods.iter().copied().any(|other| {
                        other != *candidate
                            && self
                                .method_applies_to_classes(&self.methods[other.0 as usize], actuals)
                            && self.method_more_specific(
                                &self.methods[other.0 as usize],
                                candidate_method,
                            )
                    })
            })
            .collect::<Vec<_>>();
        let selection = match maximal.as_slice() {
            [] => RuntimeMethodSelection::None,
            [method] => RuntimeMethodSelection::Selected(*method),
            _ => RuntimeMethodSelection::Ambiguous,
        };

        let mut applicable = Vec::new();
        let mut rejected = Vec::new();
        for method_id in methods {
            let method = &self.methods[method_id.0 as usize];
            match self.method_distances(method, actuals) {
                Ok(distances) => applicable.push(RuntimeDispatchCandidate {
                    method: method_id,
                    specializers: method.specializers.clone(),
                    distances,
                    selected: selection == RuntimeMethodSelection::Selected(method_id),
                }),
                Err(reason) => rejected.push(RuntimeDispatchRejection {
                    method: method_id,
                    specializers: method.specializers.clone(),
                    reason,
                }),
            }
        }

        Ok(RuntimeDispatchLookup {
            generic,
            actuals: actuals.to_vec(),
            applicable,
            rejected,
            selection,
        })
    }

    /// Selects the unique most-specific method for exact runtime classes.
    pub fn select_method_for_classes(
        &self,
        generic: GenericFunctionId,
        actuals: &[ClassId],
    ) -> Result<RuntimeMethodSelection, RuntimeDispatchError> {
        self.dispatch_lookup(generic, actuals)
            .map(|lookup| lookup.selection)
    }

    /// Inserts a new method into an existing generic and returns its new id.
    ///
    /// The generic must already be present in the layout, and every concrete
    /// specializer class must be present. The returned [`MethodId`] is
    /// appended both to [`RuntimeLayout::methods`] and to the generic's
    /// method list, so subsequent [`Self::dispatch_lookup`] calls see the
    /// inserted method as an ordinary candidate.
    ///
    /// Repeated insertions with identical specializers each produce a
    /// distinct method id; ambiguity resolution is left to dispatch and
    /// to higher layers. This mirrors source `define method` semantics
    /// where each definition is its own method until explicitly removed.
    pub fn add_method(
        &mut self,
        generic: GenericFunctionId,
        specializers: Vec<MethodSpecializer>,
    ) -> Result<MethodId, RuntimeError> {
        let generic_info = self
            .generic(generic)
            .ok_or(RuntimeError::InvalidGeneric { generic })?;
        if specializers.len() != generic_info.arity {
            return Err(RuntimeError::MethodArityMismatch {
                generic,
                expected: generic_info.arity,
                actual: specializers.len(),
            });
        }
        for specializer in &specializers {
            if let Some(class) = specializer
                && self.class(*class).is_none()
            {
                return Err(RuntimeError::InvalidClass { class: *class });
            }
        }
        let method_id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
        self.methods.push(RuntimeMethod {
            generic,
            specializers,
        });
        self.generics[generic.0 as usize].methods.push(method_id);
        Ok(method_id)
    }

    fn method_applies_to_classes(&self, method: &RuntimeMethod, actuals: &[ClassId]) -> bool {
        self.method_distances(method, actuals).is_ok()
    }

    fn method_distances(
        &self,
        method: &RuntimeMethod,
        actuals: &[ClassId],
    ) -> Result<Vec<usize>, RuntimeDispatchRejectionReason> {
        if method.specializers.len() != actuals.len() {
            return Err(RuntimeDispatchRejectionReason::ArityMismatch {
                expected: method.specializers.len(),
                actual: actuals.len(),
            });
        }
        method
            .specializers
            .iter()
            .zip(actuals)
            .enumerate()
            .map(|(argument, (specializer, actual))| {
                let Some(specializer) = *specializer else {
                    return Err(RuntimeDispatchRejectionReason::MissingSpecializer { argument });
                };
                self.inheritance_distance(*actual, specializer).ok_or(
                    RuntimeDispatchRejectionReason::ArgumentNotInstance {
                        argument,
                        actual: *actual,
                        specializer,
                    },
                )
            })
            .collect()
    }

    fn method_more_specific(&self, left: &RuntimeMethod, right: &RuntimeMethod) -> bool {
        if left.specializers.len() != right.specializers.len() {
            return false;
        }
        let mut stricter = false;
        for (left, right) in left.specializers.iter().zip(&right.specializers) {
            match (*left, *right) {
                (Some(left), Some(right)) if left == right => {}
                (Some(left), Some(right)) if self.is_same_or_subclass(left, right) => {
                    stricter = true;
                }
                _ => return false,
            }
        }
        stricter
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
/// Implementors own the concrete Rust value access. Interloko only passes the
/// resolved slot id and expected Interloko value category.
pub trait HostObjectAdapter {
    /// Reads one slot from a host-backed object.
    fn read_slot(&self, slot: SlotId, kind: ValueKind) -> Result<Value, RuntimeError>;
}

/// Host implementation for Interloko runtime object operations.
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
        debug_assert!(layout.validate().is_ok());
        Self {
            layout,
            objects: Vec::new(),
            values: Vec::new(),
            symbols: Vec::new(),
        }
    }

    /// Creates a new runtime host after validating its layout.
    pub fn new_checked(layout: RuntimeLayout) -> Result<Self, RuntimeLayoutError> {
        layout.validate()?;
        Ok(Self::new(layout))
    }

    /// Allocates one runtime `<string>` value owned by this host.
    pub fn make_string(&mut self, value: impl Into<String>) -> Result<Value, RuntimeError> {
        let name = BuiltinValueClass::String.name();
        let class = self
            .layout
            .class_id(name)
            .ok_or(RuntimeError::MissingBuiltinClass { name })?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::String {
                value: value.into(),
            },
        }))
    }

    /// Borrows the UTF-8 text stored in one runtime `<string>` value.
    pub fn string_text<'a>(&'a self, value: &Value) -> Result<&'a str, RuntimeError> {
        let name = BuiltinValueClass::String.name();
        let class = self
            .layout
            .class_id(name)
            .ok_or(RuntimeError::MissingBuiltinClass { name })?;
        let Value::Obj(object) = value else {
            return Err(RuntimeError::ValueKindMismatch {
                expected: ValueKind::String(class),
            });
        };
        let object = self.object(*object)?;
        match &object.storage {
            RuntimeObjectStorage::String { value } => Ok(value),
            _ => Err(RuntimeError::ValueKindMismatch {
                expected: ValueKind::String(class),
            }),
        }
    }

    /// Allocates a native runtime object for `class` using layout-ordered fields.
    pub fn make_object(
        &mut self,
        class: ClassId,
        fields: &[ValueRef<'_>],
    ) -> Result<Value, RuntimeError> {
        let class_info = self
            .layout
            .class(class)
            .ok_or(RuntimeError::InvalidClass { class })?;
        if class_info.kind != HostObjectKind::Native {
            return Err(RuntimeError::ClassIsNotNative {
                class,
                kind: class_info.kind,
            });
        }
        if fields.len() != class_info.slots.len() {
            return Err(RuntimeError::FieldArityMismatch {
                class,
                expected: class_info.slots.len(),
                actual: fields.len(),
            });
        }
        let mut slots = Vec::with_capacity(class_info.slots.len());
        for (field, slot) in fields.iter().zip(&class_info.slots) {
            slots.push(self.value_for_slot(*field, slot.kind)?);
        }
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Native { slots },
        }))
    }

    /// Reads one runtime effective slot from an object.
    pub fn read_slot(&self, object: Obj, slot: SlotId) -> Result<Value, RuntimeError> {
        let object = self.object(object)?;
        let object_class = object.class;
        let slot_info = self
            .layout
            .slot(object_class, slot)
            .ok_or(RuntimeError::InvalidSlot {
                class: object_class,
                slot,
            })?;
        match &object.storage {
            RuntimeObjectStorage::Native { slots } => {
                slots
                    .get(slot.0 as usize)
                    .cloned()
                    .ok_or(RuntimeError::InvalidSlot {
                        class: object_class,
                        slot,
                    })
            }
            RuntimeObjectStorage::HostBacked { adapter } => adapter.read_slot(slot, slot_info.kind),
            RuntimeObjectStorage::String { .. }
            | RuntimeObjectStorage::Symbol { .. }
            | RuntimeObjectStorage::Vector { .. }
            | RuntimeObjectStorage::ClassMeta { .. }
            | RuntimeObjectStorage::GenericMeta { .. }
            | RuntimeObjectStorage::MethodMeta { .. }
            | RuntimeObjectStorage::SlotDescriptor { .. } => Err(RuntimeError::InvalidSlot {
                class: object_class,
                slot,
            }),
        }
    }

    fn record_slot_read_access(
        &self,
        object: Obj,
        slot: SlotId,
        sig_hash: SigHash,
        ctx: &mut HostContext<'_, '_>,
    ) -> Result<(), RuntimeError> {
        let object = self.object(object)?;
        let object_class = object.class;
        self.layout
            .slot(object_class, slot)
            .ok_or(RuntimeError::InvalidSlot {
                class: object_class,
                slot,
            })?;
        if matches!(object.storage, RuntimeObjectStorage::HostBacked { .. }) {
            ctx.record_read(ResourceKeyRef::OpaqueHost { op: sig_hash });
        }
        Ok(())
    }

    /// Writes one mutable runtime effective slot and returns the stored value.
    pub fn write_slot(
        &mut self,
        object: Obj,
        slot: SlotId,
        value: ValueRef<'_>,
    ) -> Result<Value, RuntimeError> {
        let object_class = self.object(object)?.class;
        let slot_info = self
            .layout
            .slot(object_class, slot)
            .ok_or(RuntimeError::InvalidSlot {
                class: object_class,
                slot,
            })?;
        if let Some(reason) = slot_info.capability().write_rejection {
            return Err(RuntimeError::SlotWriteRejected {
                class: object_class,
                slot,
                reason,
            });
        }
        let value = self.value_for_slot(value, slot_info.kind)?;
        let object = self.object_mut(object)?;
        let RuntimeObjectStorage::Native { slots } = &mut object.storage else {
            return Err(RuntimeError::SlotWriteRejected {
                class: object_class,
                slot,
                reason: RuntimeSlotWriteRejection::NonNativeObject,
            });
        };
        let target = slots
            .get_mut(slot.0 as usize)
            .ok_or(RuntimeError::InvalidSlot {
                class: object_class,
                slot,
            })?;
        *target = value.clone();
        Ok(value)
    }

    /// Installs a Rust-owned host-backed object and returns its Interloko handle.
    pub fn push_host_object(
        &mut self,
        class: ClassId,
        adapter: impl HostObjectAdapter + 'static,
    ) -> Result<Value, RuntimeError> {
        let class_info = self
            .layout
            .class(class)
            .ok_or(RuntimeError::InvalidClass { class })?;
        if class_info.kind != HostObjectKind::HostBacked {
            return Err(RuntimeError::ClassIsNotHostBacked {
                class,
                kind: class_info.kind,
            });
        }
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::HostBacked {
                adapter: alloc::boxed::Box::new(adapter),
            },
        }))
    }

    /// Boxes a value into the top `<object>` runtime representation.
    pub fn box_top_value(&mut self, value: Value) -> Result<Value, RuntimeError> {
        match value {
            Value::I64(_) | Value::F64(_) | Value::Bool(_) | Value::U64(_) => {
                Ok(self.push_boxed_value(value))
            }
            Value::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                Ok(self.push_boxed_value(Value::Obj(obj)))
            }
            Value::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => Ok(Value::Obj(obj)),
            _ => Err(RuntimeError::CannotBoxValue),
        }
    }

    fn object(&self, obj: Obj) -> Result<&RuntimeObject, RuntimeError> {
        if obj.host_type != INTERLOKO_OBJECT_TYPE {
            return Err(RuntimeError::ObjectHostTypeMismatch {
                object: obj,
                expected: INTERLOKO_OBJECT_TYPE,
            });
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(RuntimeError::InvalidObjectHandle { object: obj })?;
        self.objects
            .get(index)
            .ok_or(RuntimeError::InvalidObjectHandle { object: obj })
    }

    fn object_mut(&mut self, obj: Obj) -> Result<&mut RuntimeObject, RuntimeError> {
        if obj.host_type != INTERLOKO_OBJECT_TYPE {
            return Err(RuntimeError::ObjectHostTypeMismatch {
                object: obj,
                expected: INTERLOKO_OBJECT_TYPE,
            });
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(RuntimeError::InvalidObjectHandle { object: obj })?;
        self.objects
            .get_mut(index)
            .ok_or(RuntimeError::InvalidObjectHandle { object: obj })
    }

    fn boxed_value(&self, obj: Obj) -> Result<&Value, RuntimeError> {
        if obj.host_type != INTERLOKO_VALUE_TYPE {
            return Err(RuntimeError::ValueHostTypeMismatch {
                object: obj,
                expected: INTERLOKO_VALUE_TYPE,
            });
        }
        let index = usize::try_from(obj.handle.0)
            .ok()
            .and_then(|raw| raw.checked_sub(1))
            .ok_or(RuntimeError::InvalidBoxedValueHandle { object: obj })?;
        self.values
            .get(index)
            .ok_or(RuntimeError::InvalidBoxedValueHandle { object: obj })
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
        sig_hash: SigHash,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
        mut ctx: HostContext<'_, '_>,
    ) -> Result<u64, HostError> {
        if let Some(function) = FloatFunction::from_symbol(symbol) {
            let mut operands = [0.0; 2];
            if args.len() != function.arity() {
                return Err(HostError::Failed);
            }
            for (operand, arg) in operands.iter_mut().zip(args) {
                let ValueRef::F64(value) = arg else {
                    return Err(HostError::Failed);
                };
                *operand = *value;
            }
            rets[0] = Value::F64(function.apply(&operands[..args.len()]));
            return Ok(0);
        }
        match symbol {
            STRING_LITERAL => {
                let [ValueRef::Str(value)] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.builtin_class(BuiltinValueClass::String)?;
                rets[0] = self.push_object(RuntimeObject {
                    class,
                    storage: RuntimeObjectStorage::String {
                        value: String::from(*value),
                    },
                });
                Ok(0)
            }
            SYMBOL_LITERAL => {
                let [ValueRef::Str(name)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.intern_symbol(String::from(*name))?;
                Ok(0)
            }
            VECTOR_LITERAL => {
                let [ValueRef::Agg(elements)] = args else {
                    return Err(HostError::Failed);
                };
                let class = self.builtin_class(BuiltinValueClass::Vector)?;
                let len = ctx.array_len(*elements).map_err(|_| HostError::Failed)?;
                let mut values = Vec::with_capacity(len);
                for index in 0..len {
                    let value = ctx
                        .array_get_ref(*elements, index)
                        .map_err(|_| HostError::Failed)?;
                    values.push(self.value_for_any(value)?);
                }
                rets[0] = self.push_object(RuntimeObject {
                    class,
                    storage: RuntimeObjectStorage::Vector { elements: values },
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
            FLOAT_TO_STRING => {
                let [ValueRef::F64(value)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_string(alloc::format!("{}", FloatBits::from_f64(*value)))?;
                Ok(0)
            }
            INTEGER_TO_STRING => {
                let [ValueRef::I64(value)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_string(alloc::format!("{value}"))?;
                Ok(0)
            }
            STRING_CONCATENATE => {
                let [ValueRef::Obj(lhs), ValueRef::Obj(rhs)] = args else {
                    return Err(HostError::Failed);
                };
                let mut text = String::from(self.string_text(&Value::Obj(*lhs))?);
                text.push_str(self.string_text(&Value::Obj(*rhs))?);
                rets[0] = self.push_string(text)?;
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
                let [ValueRef::U64(class_raw), ValueRef::Agg(fields)] = args else {
                    return Err(HostError::Failed);
                };
                let len = ctx.tuple_len(*fields).map_err(|_| HostError::Failed)?;
                let mut values = Vec::with_capacity(len);
                for index in 0..len {
                    values.push(
                        ctx.tuple_get_ref(*fields, index)
                            .map_err(|_| HostError::Failed)?,
                    );
                }
                rets[0] = self.make_object(runtime_class_id(*class_raw)?, &values)?;
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
                let slot = runtime_slot_id(*slot_raw)?;
                self.record_slot_read_access(*obj, slot, sig_hash, &mut ctx)?;
                rets[0] = self.read_slot(*obj, slot)?;
                Ok(0)
            }
            SLOT_SET => {
                let [ValueRef::Obj(obj), ValueRef::U64(slot_raw), value] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.write_slot(*obj, runtime_slot_id(*slot_raw)?, *value)?;
                Ok(0)
            }
            BOX_VALUE => {
                let value = match args {
                    [ValueRef::I64(value)] => Value::I64(*value),
                    [ValueRef::F64(value)] => Value::F64(*value),
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
        Ok(self.object(obj)?)
    }

    fn class_of_any(&self, value: ValueRef<'_>) -> Result<ClassId, HostError> {
        match value {
            ValueRef::I64(_) => self.builtin_class(BuiltinValueClass::Integer),
            ValueRef::F64(_) => self.builtin_class(BuiltinValueClass::DoubleFloat),
            ValueRef::Bool(_) | ValueRef::U64(_) => Ok(OBJECT_CLASS_ID),
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                Ok(self.object(obj)?.class)
            }
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj)? {
                    Value::I64(_) => self.builtin_class(BuiltinValueClass::Integer),
                    Value::F64(_) => self.builtin_class(BuiltinValueClass::DoubleFloat),
                    Value::Bool(_) | Value::U64(_) => Ok(OBJECT_CLASS_ID),
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
        self.class_of_any(value)
            .is_ok_and(|actual| self.layout.is_same_or_subclass(actual, class))
    }

    fn value_for_slot(&self, value: ValueRef<'_>, kind: ValueKind) -> Result<Value, RuntimeError> {
        match (kind, value) {
            (ValueKind::Any, ValueRef::Obj(obj)) => {
                let _ = self.boxed_value(obj)?;
                Ok(Value::Obj(obj))
            }
            (ValueKind::I64, ValueRef::I64(value)) => Ok(Value::I64(value)),
            (ValueKind::F64, ValueRef::F64(value)) => Ok(Value::F64(value)),
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
                    Err(RuntimeError::ValueKindMismatch { expected: kind })
                }
            }
            (ValueKind::ClassObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::ClassMeta { class } if *class == expected => {
                        Ok(Value::Obj(obj))
                    }
                    _ => Err(RuntimeError::ValueKindMismatch { expected: kind }),
                }
            }
            (ValueKind::GenericFunctionObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::GenericMeta { generic } if *generic == expected => {
                        Ok(Value::Obj(obj))
                    }
                    _ => Err(RuntimeError::ValueKindMismatch { expected: kind }),
                }
            }
            (ValueKind::MethodObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::MethodMeta { method } if *method == expected => {
                        Ok(Value::Obj(obj))
                    }
                    _ => Err(RuntimeError::ValueKindMismatch { expected: kind }),
                }
            }
            (ValueKind::SlotDescriptorObject { class, slot }, ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::SlotDescriptor {
                        class: actual_class,
                        slot: actual_slot,
                    } if *actual_class == class && *actual_slot == slot => Ok(Value::Obj(obj)),
                    _ => Err(RuntimeError::ValueKindMismatch { expected: kind }),
                }
            }
            _ => Err(RuntimeError::ValueKindMismatch { expected: kind }),
        }
    }
}

/// Host signature for object construction from a class id and aggregate payload.
#[must_use]
pub fn make_object_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64, ValueType::Agg],
        rets: vec![object_type()],
    }
}

/// Host signature for `class_of`.
#[must_use]
pub fn class_of_signature() -> HostSig {
    HostSig {
        args: vec![object_type()],
        rets: vec![ValueType::U64],
    }
}

/// Host signature for `slot_get`.
#[must_use]
pub fn slot_get_signature(kind: ValueKind) -> Option<HostSig> {
    Some(HostSig {
        args: vec![object_type(), ValueType::U64],
        rets: vec![value_type(kind)?],
    })
}

/// Host signature for `slot_set`.
#[must_use]
pub fn slot_set_signature(kind: ValueKind) -> Option<HostSig> {
    Some(HostSig {
        args: vec![object_type(), ValueType::U64, value_type(kind)?],
        rets: vec![value_type(kind)?],
    })
}

/// Host signature for boxing a concrete value as top `<object>`.
#[must_use]
pub fn box_value_signature(input: ValueType) -> HostSig {
    HostSig {
        args: vec![input],
        rets: vec![any_type()],
    }
}

/// Host signature for `instance?`.
#[must_use]
pub fn instance_of_signature(input: ValueType) -> HostSig {
    HostSig {
        args: vec![input, ValueType::U64],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for `as` to a runtime object.
#[must_use]
pub fn cast_object_signature(input: ValueType) -> HostSig {
    HostSig {
        args: vec![input, ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a string object from an execution string literal.
#[must_use]
pub fn string_literal_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::Str],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing an interned symbol object from an execution string literal.
#[must_use]
pub fn symbol_literal_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::Str],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a vector literal from an aggregate payload.
#[must_use]
pub fn vector_literal_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::Agg],
        rets: vec![object_type()],
    }
}

/// Host signature for runtime value equality.
#[must_use]
pub fn value_eq_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), object_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for constructing a class metaobject from a class id.
#[must_use]
pub fn meta_class_object_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a generic-function metaobject.
#[must_use]
pub fn meta_generic_object_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a method metaobject.
#[must_use]
pub fn meta_method_object_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for constructing a slot descriptor metaobject.
#[must_use]
pub fn meta_slot_descriptor_object_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64, ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for a one-argument metaobject query returning an object.
#[must_use]
pub fn meta_query_object_signature() -> HostSig {
    HostSig {
        args: vec![any_type()],
        rets: vec![object_type()],
    }
}

/// Host signature for a one-argument metaobject query returning `<object>`.
#[must_use]
pub fn meta_query_any_signature() -> HostSig {
    HostSig {
        args: vec![any_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for a one-argument metaobject query returning `<boolean>`.
#[must_use]
pub fn meta_query_bool_signature() -> HostSig {
    HostSig {
        args: vec![any_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for immutable vector length.
#[must_use]
pub fn vector_size_signature() -> HostSig {
    HostSig {
        args: vec![any_type()],
        rets: vec![ValueType::I64],
    }
}

/// Host signature for immutable vector indexing.
#[must_use]
pub fn vector_ref_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), ValueType::I64],
        rets: vec![any_type()],
    }
}

fn value_type(kind: ValueKind) -> Option<ValueType> {
    match kind {
        ValueKind::Any => Some(any_type()),
        ValueKind::I64 => Some(ValueType::I64),
        ValueKind::F64 => Some(ValueType::F64),
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

#[cfg(test)]
mod tests {
    use super::*;
    use execution_tape::asm::{Asm, FunctionSig, ProgramBuilder};
    use execution_tape::trace::TraceMask;
    use execution_tape::vm::{ExecutionContext, Limits, Vm};

    #[test]
    fn float_functions_are_exact_where_ieee_defines_them() {
        extern crate std;
        let samples = [
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.5,
            2.5,
            -2.5,
            3.5,
            0.49999999999999994,
            4503599627370497.0,
            1e300,
            -1e-300,
            5e-324,
            f64::INFINITY,
            f64::NEG_INFINITY,
            2.0,
            10.0,
        ];
        for x in samples {
            let check = |function: FloatFunction, expected: f64| {
                let actual = function.apply(&[x]);
                assert_eq!(actual.to_bits(), expected.to_bits(), "{function:?}({x})");
            };
            check(FloatFunction::Floor, std::primitive::f64::floor(x));
            check(FloatFunction::Ceiling, std::primitive::f64::ceil(x));
            check(FloatFunction::Truncate, std::primitive::f64::trunc(x));
            check(
                FloatFunction::Round,
                std::primitive::f64::round_ties_even(x),
            );
            check(FloatFunction::Sqrt, std::primitive::f64::sqrt(x));
        }
        assert!(FloatFunction::Sqrt.apply(&[-1.0]).is_nan());
        assert!(FloatFunction::Floor.apply(&[f64::NAN]).is_nan());
        assert_eq!(FloatFunction::Pow.apply(&[2.0, 10.0]), 1024.0);
        assert_eq!(FloatFunction::Hypot.apply(&[3.0, 4.0]), 5.0);
        assert_eq!(
            FloatFunction::Atan2.apply(&[1.0, 1.0]),
            core::f64::consts::FRAC_PI_4
        );
    }

    #[test]
    fn float_functions_round_trip_their_host_symbols() {
        for function in FloatFunction::ALL {
            assert_eq!(
                FloatFunction::from_symbol(function.symbol()),
                Some(*function)
            );
            assert_eq!(function.signature().args.len(), function.arity());
        }
        assert_eq!(FloatFunction::from_symbol("interloko.float.nope"), None);
    }

    fn slot(
        name: &str,
        kind: ValueKind,
        source_class: ClassId,
        source_slot: SlotId,
    ) -> RuntimeSlot {
        RuntimeSlot {
            name: name.into(),
            type_name: "<integer>".into(),
            kind,
            source_class,
            source_slot,
            init_keyword: name.into(),
            mutable: true,
            getter: name.into(),
            setter: None,
            host_backed: false,
        }
    }

    fn class(name: &str, superclass: Option<ClassId>, slots: Vec<RuntimeSlot>) -> RuntimeClass {
        RuntimeClass {
            name: name.into(),
            kind: HostObjectKind::Native,
            superclass,
            slots,
        }
    }

    fn host_backed_class(
        name: &str,
        superclass: Option<ClassId>,
        mut slots: Vec<RuntimeSlot>,
    ) -> RuntimeClass {
        for slot in &mut slots {
            slot.host_backed = true;
        }
        RuntimeClass {
            name: name.into(),
            kind: HostObjectKind::HostBacked,
            superclass,
            slots,
        }
    }

    fn dispatch_layout() -> RuntimeLayout {
        RuntimeLayout {
            classes: vec![
                class("<object>", None, Vec::new()),
                class(
                    "<shape>",
                    Some(OBJECT_CLASS_ID),
                    vec![slot("size", ValueKind::I64, ClassId(1), SlotId(0))],
                ),
                class(
                    "<circle>",
                    Some(ClassId(1)),
                    vec![
                        slot("size", ValueKind::I64, ClassId(1), SlotId(0)),
                        slot("radius", ValueKind::I64, ClassId(2), SlotId(0)),
                    ],
                ),
                class(
                    "<square>",
                    Some(ClassId(1)),
                    vec![
                        slot("size", ValueKind::I64, ClassId(1), SlotId(0)),
                        slot("side", ValueKind::I64, ClassId(3), SlotId(0)),
                    ],
                ),
            ],
            generics: vec![RuntimeGeneric {
                name: "score".into(),
                arity: 1,
                methods: vec![MethodId(0), MethodId(1), MethodId(2)],
            }],
            methods: vec![
                RuntimeMethod {
                    generic: GenericFunctionId(0),
                    specializers: vec![Some(ClassId(1))],
                },
                RuntimeMethod {
                    generic: GenericFunctionId(0),
                    specializers: vec![Some(ClassId(2))],
                },
                RuntimeMethod {
                    generic: GenericFunctionId(0),
                    specializers: vec![Some(ClassId(3))],
                },
            ],
        }
    }

    #[test]
    fn layout_validation_accepts_rooted_layout() {
        let layout = dispatch_layout();

        assert_eq!(layout.validate(), Ok(()));
        assert_eq!(layout.classes().len(), 4);
        assert_eq!(layout.generics().len(), 1);
        assert_eq!(layout.methods().len(), 3);
        assert_eq!(
            layout.object_root().map(|class| class.name.as_str()),
            Some("<object>")
        );
        assert_eq!(
            layout.class_by_name("<circle>").map(|(id, _)| id),
            Some(ClassId(2))
        );
        assert_eq!(
            layout.superclass_chain(ClassId(2)),
            Some(vec![ClassId(1), OBJECT_CLASS_ID])
        );
        assert_eq!(layout.effective_slots(ClassId(2)).map(<[_]>::len), Some(2));
        assert_eq!(layout.generic_id("score"), Some(GenericFunctionId(0)));
        assert_eq!(
            layout.methods_for_generic(GenericFunctionId(0)),
            Some([MethodId(0), MethodId(1), MethodId(2)].as_slice())
        );
        assert_eq!(
            layout.inheritance_distance(ClassId(2), OBJECT_CLASS_ID),
            Some(2)
        );
        assert!(layout.is_same_or_subclass(ClassId(2), ClassId(1)));
    }

    #[test]
    fn layout_validation_rejects_broken_root_and_method_facts() {
        let mut layout = dispatch_layout();
        layout.classes[0].superclass = Some(ClassId(1));
        assert_eq!(
            layout.validate(),
            Err(RuntimeLayoutError::ObjectRootHasSuperclass {
                superclass: ClassId(1)
            })
        );

        let mut layout = dispatch_layout();
        layout.methods[0].specializers[0] = None;
        assert_eq!(
            layout.validate(),
            Err(RuntimeLayoutError::MissingMethodSpecializer {
                method: MethodId(0),
                argument: 0
            })
        );
    }

    #[test]
    fn slot_api_reads_and_writes_native_slots() {
        let layout = dispatch_layout();
        assert_eq!(
            layout.slot_capability(ClassId(2), SlotId(1)),
            Some(RuntimeSlotCapability {
                storage: RuntimeSlotStorage::Native,
                readable: true,
                writable: true,
                write_rejection: None
            })
        );
        let mut host = InterlokoHost::new_checked(layout).expect("layout should validate");
        let value = host
            .make_object(ClassId(2), &[ValueRef::I64(4), ValueRef::I64(10)])
            .expect("object should be allocated");
        let Value::Obj(object) = value else {
            panic!("expected object");
        };

        assert_eq!(
            host.read_slot(object, SlotId(1)).expect("slot should read"),
            Value::I64(10)
        );
        assert_eq!(
            host.write_slot(object, SlotId(1), ValueRef::I64(12))
                .expect("slot should write"),
            Value::I64(12)
        );
        assert_eq!(
            host.read_slot(object, SlotId(1)).expect("slot should read"),
            Value::I64(12)
        );
    }

    #[derive(Debug)]
    struct TestHostObject {
        value: i64,
    }

    impl HostObjectAdapter for TestHostObject {
        fn read_slot(&self, slot: SlotId, kind: ValueKind) -> Result<Value, RuntimeError> {
            match (slot, kind) {
                (SlotId(0), ValueKind::I64) => Ok(Value::I64(self.value)),
                _ => Err(RuntimeError::HostAdapterReadFailed { slot }),
            }
        }
    }

    #[derive(Default)]
    struct RecordingAccessSink {
        reads: Vec<SigHash>,
        writes: usize,
    }

    impl AccessSink for RecordingAccessSink {
        fn read(&mut self, key: ResourceKeyRef<'_>) {
            let ResourceKeyRef::OpaqueHost { op } = key else {
                panic!("expected opaque host read key");
            };
            self.reads.push(op);
        }

        fn write(&mut self, _key: ResourceKeyRef<'_>) {
            self.writes += 1;
        }
    }

    fn run_slot_get_with_access(
        host: InterlokoHost,
        object: Obj,
        slot: SlotId,
        access: &mut dyn AccessSink,
    ) -> Vec<Value> {
        let sig = slot_get_signature(ValueKind::I64).expect("slot kind should have host sig");
        let mut builder = ProgramBuilder::new();
        let host_sig = builder.host_sig_for(SLOT_GET, sig);

        let mut asm = Asm::new();
        asm.set_register_types(alloc::vec![
            ValueType::Unit,
            object_type(),
            ValueType::U64,
            ValueType::I64
        ]);
        asm.const_u64(2, u64::from(slot.0));
        asm.host_call(0, host_sig, 0, &[1, 2], &[3]);
        asm.ret(0, &[3]);
        let entry = builder
            .push_function_checked(
                asm,
                FunctionSig {
                    arg_types: vec![object_type()],
                    ret_types: vec![ValueType::I64],
                },
            )
            .expect("test tape should verify");
        let program = builder
            .build_verified()
            .expect("test program should verify");

        let mut vm = Vm::new(host, Limits::default());
        let mut ctx = ExecutionContext::new();
        let mut outputs = Vec::new();
        vm.run_with_ctx(
            &mut ctx,
            &program,
            entry,
            &[Value::Obj(object)],
            &mut outputs,
            TraceMask::NONE,
            None,
            Some(access),
        )
        .expect("slot-get program should run");
        outputs
    }

    #[test]
    fn slot_api_reads_host_backed_slots_but_rejects_writes() {
        let mut layout = dispatch_layout();
        layout.classes.push(host_backed_class(
            "<rust-counter>",
            Some(OBJECT_CLASS_ID),
            vec![slot("value", ValueKind::I64, ClassId(4), SlotId(0))],
        ));
        assert_eq!(layout.validate(), Ok(()));
        assert_eq!(
            layout.slot_capability(ClassId(4), SlotId(0)),
            Some(RuntimeSlotCapability {
                storage: RuntimeSlotStorage::HostBacked,
                readable: true,
                writable: false,
                write_rejection: Some(RuntimeSlotWriteRejection::HostBackedSlot)
            })
        );

        let mut host = InterlokoHost::new_checked(layout).expect("layout should validate");
        let value = host
            .push_host_object(ClassId(4), TestHostObject { value: 99 })
            .expect("host object should install");
        let Value::Obj(object) = value else {
            panic!("expected object");
        };

        assert_eq!(
            host.read_slot(object, SlotId(0)).expect("slot should read"),
            Value::I64(99)
        );
        assert_eq!(
            host.write_slot(object, SlotId(0), ValueRef::I64(100)),
            Err(RuntimeError::SlotWriteRejected {
                class: ClassId(4),
                slot: SlotId(0),
                reason: RuntimeSlotWriteRejection::HostBackedSlot
            })
        );
    }

    #[test]
    fn host_call_records_access_for_host_backed_slot_reads() {
        let mut layout = dispatch_layout();
        layout.classes.push(host_backed_class(
            "<rust-counter>",
            Some(OBJECT_CLASS_ID),
            vec![slot("value", ValueKind::I64, ClassId(4), SlotId(0))],
        ));
        let mut host = InterlokoHost::new_checked(layout).expect("layout should validate");
        let value = host
            .push_host_object(ClassId(4), TestHostObject { value: 99 })
            .expect("host object should install");
        let Value::Obj(object) = value else {
            panic!("expected object");
        };
        let sig = slot_get_signature(ValueKind::I64).expect("slot kind should have host sig");
        let sig_hash = execution_tape::host::sig_hash(&sig);
        let mut access = RecordingAccessSink::default();

        let out = run_slot_get_with_access(host, object, SlotId(0), &mut access);

        assert_eq!(out, vec![Value::I64(99)]);
        assert_eq!(access.reads, vec![sig_hash]);
        assert_eq!(access.writes, 0);
    }

    #[test]
    fn host_call_does_not_record_access_for_native_slot_reads() {
        let layout = dispatch_layout();
        let mut host = InterlokoHost::new_checked(layout).expect("layout should validate");
        let value = host
            .make_object(ClassId(2), &[ValueRef::I64(4), ValueRef::I64(10)])
            .expect("object should be allocated");
        let Value::Obj(object) = value else {
            panic!("expected object");
        };
        let mut access = RecordingAccessSink::default();

        let out = run_slot_get_with_access(host, object, SlotId(1), &mut access);

        assert_eq!(out, vec![Value::I64(10)]);
        assert!(access.reads.is_empty());
        assert_eq!(access.writes, 0);
    }

    #[test]
    fn slot_api_rejects_constant_slots() {
        let mut layout = dispatch_layout();
        layout.classes[2].slots[1].mutable = false;
        let mut host = InterlokoHost::new_checked(layout).expect("layout should validate");
        let value = host
            .make_object(ClassId(2), &[ValueRef::I64(4), ValueRef::I64(10)])
            .expect("object should be allocated");
        let Value::Obj(object) = value else {
            panic!("expected object");
        };

        assert_eq!(
            host.write_slot(object, SlotId(1), ValueRef::I64(12)),
            Err(RuntimeError::SlotWriteRejected {
                class: ClassId(2),
                slot: SlotId(1),
                reason: RuntimeSlotWriteRejection::ConstantSlot
            })
        );
    }

    #[test]
    fn dispatch_lookup_selects_most_specific_method() {
        let layout = dispatch_layout();
        let lookup = layout
            .dispatch_lookup(GenericFunctionId(0), &[ClassId(2)])
            .expect("dispatch lookup should succeed");

        assert_eq!(
            lookup.selection,
            RuntimeMethodSelection::Selected(MethodId(1))
        );
        assert_eq!(lookup.selected_method(), Some(MethodId(1)));
        assert_eq!(lookup.applicable.len(), 2);
        assert_eq!(lookup.applicable[0].method, MethodId(1));
        assert_eq!(lookup.applicable[0].distances, [0]);
        assert!(lookup.applicable[0].selected);
        assert_eq!(lookup.applicable[1].method, MethodId(0));
        assert_eq!(lookup.applicable[1].distances, [1]);
        assert_eq!(lookup.rejected.len(), 1);
        assert_eq!(
            lookup.rejected[0].reason,
            RuntimeDispatchRejectionReason::ArgumentNotInstance {
                argument: 0,
                actual: ClassId(2),
                specializer: ClassId(3),
            }
        );
    }

    #[test]
    fn dispatch_lookup_reports_ambiguous_maximal_methods() {
        let mut layout = dispatch_layout();
        layout.generics[0].methods.push(MethodId(3));
        layout.methods.push(RuntimeMethod {
            generic: GenericFunctionId(0),
            specializers: vec![Some(ClassId(2))],
        });

        let lookup = layout
            .dispatch_lookup(GenericFunctionId(0), &[ClassId(2)])
            .expect("dispatch lookup should succeed");

        assert_eq!(lookup.selection, RuntimeMethodSelection::Ambiguous);
        assert_eq!(lookup.selected_method(), None);
    }

    #[test]
    fn add_method_inserts_and_dispatches_new_method() {
        let mut layout = dispatch_layout();
        let scratch_class = ClassId(u32::try_from(layout.classes.len()).unwrap());
        layout.classes.push(class(
            "<triangle>",
            Some(ClassId(1)),
            vec![
                slot("size", ValueKind::I64, ClassId(1), SlotId(0)),
                slot("base", ValueKind::I64, scratch_class, SlotId(0)),
            ],
        ));
        assert_eq!(layout.validate(), Ok(()));

        let method_id = layout
            .add_method(GenericFunctionId(0), vec![Some(scratch_class)])
            .expect("add_method should succeed");

        assert_eq!(method_id, MethodId(3));
        assert_eq!(
            layout.method(method_id).map(|m| m.generic),
            Some(GenericFunctionId(0))
        );
        assert_eq!(
            layout.methods_for_generic(GenericFunctionId(0)),
            Some([MethodId(0), MethodId(1), MethodId(2), method_id].as_slice())
        );
        assert_eq!(layout.validate(), Ok(()));

        let lookup = layout
            .dispatch_lookup(GenericFunctionId(0), &[scratch_class])
            .expect("dispatch lookup should succeed");
        assert_eq!(
            lookup.selection,
            RuntimeMethodSelection::Selected(method_id)
        );
    }

    #[test]
    fn add_method_rejects_unknown_generic_or_specializer() {
        let mut layout = dispatch_layout();
        let missing_generic = GenericFunctionId(99);
        assert_eq!(
            layout.add_method(missing_generic, vec![Some(ClassId(1))]),
            Err(RuntimeError::InvalidGeneric {
                generic: missing_generic
            })
        );

        let missing_class = ClassId(99);
        assert_eq!(
            layout.add_method(GenericFunctionId(0), vec![Some(missing_class)]),
            Err(RuntimeError::InvalidClass {
                class: missing_class
            })
        );

        // Layout must be untouched after each failed insertion.
        assert_eq!(layout.methods().len(), 3);
        assert_eq!(
            layout
                .methods_for_generic(GenericFunctionId(0))
                .map(<[_]>::len),
            Some(3)
        );
    }

    #[test]
    fn add_method_rejects_arity_mismatch() {
        let mut layout = dispatch_layout();
        let before = layout.methods().len();

        // dispatch_layout's `score` generic has arity 1; a two-specializer
        // insertion must be rejected before any layout state changes.
        let result = layout.add_method(
            GenericFunctionId(0),
            vec![Some(ClassId(2)), Some(ClassId(2))],
        );
        assert_eq!(
            result,
            Err(RuntimeError::MethodArityMismatch {
                generic: GenericFunctionId(0),
                expected: 1,
                actual: 2
            })
        );
        assert_eq!(layout.methods().len(), before);
        assert_eq!(
            layout
                .methods_for_generic(GenericFunctionId(0))
                .map(<[_]>::len),
            Some(3)
        );

        // Zero specializers is just as wrong as too many.
        let result = layout.add_method(GenericFunctionId(0), Vec::new());
        assert_eq!(
            result,
            Err(RuntimeError::MethodArityMismatch {
                generic: GenericFunctionId(0),
                expected: 1,
                actual: 0
            })
        );
        assert_eq!(layout.methods().len(), before);
        assert_eq!(layout.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_method_with_arity_disagreeing_with_generic() {
        let mut layout = dispatch_layout();
        layout.methods[0].specializers.push(Some(ClassId(1)));

        assert_eq!(
            layout.validate(),
            Err(RuntimeLayoutError::MethodArityMismatch {
                method: MethodId(0),
                generic: GenericFunctionId(0),
                expected: 1,
                actual: 2,
            })
        );
    }

    #[test]
    fn add_method_two_calls_produce_distinct_methods() {
        let mut layout = dispatch_layout();
        let first = layout
            .add_method(GenericFunctionId(0), vec![Some(ClassId(2))])
            .expect("first add_method should succeed");
        let second = layout
            .add_method(GenericFunctionId(0), vec![Some(ClassId(2))])
            .expect("second add_method should succeed");

        assert_ne!(first, second);
        assert_eq!(first, MethodId(3));
        assert_eq!(second, MethodId(4));
        assert_eq!(layout.validate(), Ok(()));

        // Two methods with identical specializers correctly produce ambiguity;
        // resolving that ambiguity is the caller's responsibility, not the
        // runtime's.
        let lookup = layout
            .dispatch_lookup(GenericFunctionId(0), &[ClassId(2)])
            .expect("dispatch lookup should succeed");
        assert_eq!(lookup.selection, RuntimeMethodSelection::Ambiguous);
    }
}
