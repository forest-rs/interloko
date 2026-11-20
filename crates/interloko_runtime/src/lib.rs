//! Runtime layout, host-call signatures, and `execution_tape` host support.
//!
//! This crate owns the executable runtime boundary for Interloko-shaped
//! programs. It intentionally does not own Loko syntax, semantic IR, lowering,
//! sessions, or compiler query state.

#![no_std]

extern crate alloc;

mod boxed;
mod census;
pub mod chooser;
pub mod code;
mod collection;
pub use collection::MAX_VECTOR_SIZE;
mod constant;
pub mod control;
pub mod driver;
mod evacuate;
mod integer;
mod list;
mod no_applicable_method;
mod region;
mod send;
mod text;
mod turn;
mod type_error;
mod type_object;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use core::fmt;
#[cfg(test)]
use execution_tape::host::AccessSink;
use execution_tape::host::{
    Host, HostContext, HostError, HostSig, ResourceKeyRef, SigHash, ValueRef,
};
use execution_tape::program::{HostTypeId, ValueType};
use execution_tape::value::{AggHandle, Obj, ObjHandle, Value};

use interloko_model::sealing::{ClosedWorld, Sealing};
use interloko_model::types::{ClassHierarchy, IntRange, TypeNames};
use interloko_model::{
    BuiltinValueClass, ClassId, FloatBits, GenericFunctionId, LimitedInteger, MethodId,
    OBJECT_CLASS_ID, SingleFloatBits, SlotId,
};

/// Fuel each runtime host call costs above the instruction that makes it.
///
/// A host call takes about as long as this many VM instructions (in the
/// `lisp` bench, about 80 ns against 10 ns): it marshals its arguments and
/// results and looks its symbol up, and most read or allocate in the
/// runtime heap. Charging fuel for it lets one
/// budget, the VM's fuel, bound a run however its work divides between
/// instructions and host calls, so an embedder sets no separate count of
/// host calls.
pub const HOST_CALL_FUEL: u64 = 8;

/// Host object type used for Interloko runtime objects.
pub const INTERLOKO_OBJECT_TYPE: HostTypeId = HostTypeId(0x494c_4f4b_5f4f_424a);
/// Host object type used for boxed top `<object>` values.
///
/// Floats, booleans, integers in
/// [`IMMEDIATE_INTEGER_MIN`]..=[`IMMEDIATE_INTEGER_MAX`], and runtime objects
/// are carried in the handle itself and never allocate; other boxed values
/// live in the host's append-only boxed-value heap.
pub const INTERLOKO_VALUE_TYPE: HostTypeId = HostTypeId(0x494c_4f4b_5f56_414c);

use boxed::BoxedHandle;

/// What a boxed `<object>` holds; see [`InterlokoHost::unbox_top_value`].
#[derive(Clone, Debug, PartialEq)]
pub enum Unboxed {
    /// A value with an `execution_tape` representation of its own: a
    /// number, a boolean, a callable or a runtime object.
    Value(Value),
    /// A `<character>`, which a `u32` register holds unboxed. It is kept
    /// apart because a `u32` value is otherwise a `<u32>` integer.
    Character(char),
    /// A `<string-position>`, a byte offset an `i64` register holds
    /// unboxed, kept apart from the `<integer>` that register would be.
    Position(u64),
}
pub use boxed::{IMMEDIATE_INTEGER_MAX, IMMEDIATE_INTEGER_MIN};
pub use census::{HeapStats, TurnStats};
pub use constant::{
    CONSTANT_ABANDON, CONSTANT_CLAIM, CONSTANT_GET, CONSTANT_READ, CONSTANT_SET, ConstantCellError,
    ConstantCycleReport, constant_abandon_signature, constant_claim_signature,
};
pub use integer::{
    INTEGER_POWER, IntegerDivision, Rounding, integer_power, integer_power_signature,
};
pub use no_applicable_method::NoApplicableMethodReport;
use region::{NurserySerials, ObjectRef, OpenTurn, Region, Space};
pub use send::SendCacheReport;
use text::StringText;
pub use text::{
    STRING_MAX_BYTES, TextFunction, TextKind, character_literal, is_position_offset,
    string_literal, symbol_literal,
};
pub use turn::ImageRootId;
pub use type_error::TypeErrorReport;

/// Host-call symbol for object construction.
pub const MAKE_OBJECT: &str = "interloko.make_object";
/// Host-call symbol for reading an object's class id.
pub const CLASS_OF: &str = "interloko.class_of";
/// Host-call symbol for reading a typed runtime-managed slot.
pub const SLOT_GET: &str = "interloko.slot_get";
/// Host-call symbol for writing a typed runtime-managed slot.
pub const SLOT_SET: &str = "interloko.slot_set";
/// Host-call symbol for making a shared local's cell holding its first
/// value; see [`InterlokoHost::make_cell`].
pub const CELL_NEW: &str = "interloko.cell_new";
/// Host-call symbol for reading the value a cell holds.
pub const CELL_GET: &str = "interloko.cell_get";
/// Host-call symbol for storing a value in a cell.
pub const CELL_SET: &str = "interloko.cell_set";
/// Host-call symbol for boxing a concrete value as the top `<object>` value.
pub const BOX_VALUE: &str = "interloko.box_value";
/// Host-call symbol for unboxing an integer, float or boolean from a boxed
/// top `<object>` value after checking its class.
pub const UNBOX_VALUE: &str = "interloko.unbox_value";
/// Host-call symbol for boxing a `<character>`, held unboxed in a `u32`
/// register, as the top `<object>` value. It takes
/// [`box_character_signature`]. A `<u32>` integer boxes through
/// [`BOX_VALUE`] after widening, so the two never meet.
pub const BOX_CHARACTER: &str = "interloko.box_character";
/// Host-call symbol for checking that a `use-value` replacement for a failed
/// unboxing to `<character>` is a scalar value. It takes
/// [`check_character_signature`]: the replacement and the class the
/// unboxing checked, which it does not need, and returns the replacement,
/// or fails with the check-failed tag.
pub const CHECK_CHARACTER: &str = "interloko.check_character";
/// Host-call symbol for checking that a `use-value` replacement for a failed
/// unboxing to `<string-position>` is an offset some string can have. It
/// takes [`check_position_signature`], as [`CHECK_CHARACTER`] does.
pub const CHECK_POSITION: &str = "interloko.check_position";
/// Host-call symbol for boxing a rejected character argument for the
/// `<type-error>` that reports it. It takes [`box_character_signature`].
/// A scalar value boxes as the `<character>` it is; any other value a
/// `u32` register can hold, such as a `use-value` replacement that was not
/// a scalar value, boxes as the `<integer>` it is, so the report shows it.
pub const BOX_REJECTED_CHARACTER: &str = "interloko.box_rejected_character";
/// Host-call symbol for boxing a `<string-position>`, held unboxed in an
/// `i64` register, as the top `<object>` value. It takes
/// [`box_position_signature`].
pub const BOX_POSITION: &str = "interloko.box_position";
/// Host-call symbol for boxing a rejected position argument for the
/// `<type-error>` that reports it. It takes [`box_position_signature`]. An
/// offset a string can have boxes as a `<string-position>`, and any other
/// `i64` as the `<integer>` it is.
pub const BOX_REJECTED_POSITION: &str = "interloko.box_rejected_position";
/// Host-call symbol for testing whether a value is an instance of a class.
pub const INSTANCE_OF: &str = "interloko.instance_of";
/// Host-call symbol for casting a value to a known runtime object class.
pub const CAST_OBJECT: &str = "interloko.cast_object";
/// Host-call symbol for casting a value to a class object naming a known
/// class or one of its subclasses. It takes [`cast_object_signature`].
pub const CAST_CLASS_OBJECT: &str = "interloko.cast_class_object";
/// Host-call symbol for constructing a string object from an execution string.
pub const STRING_LITERAL: &str = "interloko.literal.string";
/// Host-call symbol for constructing an interned symbol object from an execution string.
pub const SYMBOL_LITERAL: &str = "interloko.literal.symbol";
/// Host-call symbol for immutable vector literals.
pub const VECTOR_LITERAL: &str = "interloko.literal.vector";
/// Host-call symbol for runtime value equality.
pub const VALUE_EQ: &str = "interloko.value_eq";
/// Host-call symbol for identity, `==`, of two boxed values. It takes
/// [`identical_signature`].
pub const IDENTICAL: &str = "interloko.identical";
/// Host-call symbol for creating a class metaobject.
pub const META_CLASS_OBJECT: &str = "interloko.meta.class_object";
/// Host-call symbol for reading the class id a class metaobject names.
pub const META_CLASS_ID: &str = "interloko.meta.class_id";
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
/// Host-call symbol for testing whether a value is an instance of a type
/// object: a class object, a `<limited-integer>` or a `<subclass-type>`.
/// It takes [`instance_of_type_signature`].
pub const INSTANCE_OF_TYPE: &str = "interloko.instance_of_type";
/// Host-call symbol for constructing a `<limited-integer>` type object. It
/// takes [`limited_integer_type_signature`].
pub const LIMITED_INTEGER_TYPE: &str = "interloko.limited_integer_type";
/// Host-call symbol for reading a `<limited-integer>`'s base class, which is
/// always `<integer>`.
pub const MOP_LIMITED_INTEGER_BASE: &str = "interloko.mop.limited_integer_base";
/// Host-call symbol for reading a `<limited-integer>`'s lower bound, or `#f`.
pub const MOP_LIMITED_INTEGER_MIN: &str = "interloko.mop.limited_integer_min";
/// Host-call symbol for reading a `<limited-integer>`'s upper bound, or `#f`.
pub const MOP_LIMITED_INTEGER_MAX: &str = "interloko.mop.limited_integer_max";
/// Host-call symbol for reading the class a `<subclass-type>` bounds.
pub const MOP_SUBCLASS_TYPE_CLASS: &str = "interloko.mop.subclass_type_class";
/// Host-call symbol for writing a type object as Loko writes the type.
pub const MOP_TYPE_NAME: &str = "interloko.mop.type_name";
/// Host-call symbol for making a `<vector>` or `<stretchy-vector>`: the
/// class, a size of zero or more, and the element every slot starts as. It
/// takes [`make_vector_signature`].
pub const MAKE_VECTOR: &str = "interloko.collection.make_vector";
/// Host-call symbol for making an empty `<table>`. It takes
/// [`make_table_signature`].
pub const MAKE_TABLE: &str = "interloko.collection.make_table";
/// Host-call symbol for the number of elements of a vector or entries of a
/// table. It takes [`collection_size_signature`].
pub const COLLECTION_SIZE: &str = "interloko.collection.size";
/// Host-call symbol for a vector's element at an index. An index outside
/// the vector raises [`control::CHECK_FAILED_TAG`]. It takes
/// [`vector_element_signature`].
pub const VECTOR_ELEMENT: &str = "interloko.collection.vector_element";
/// Host-call symbol for a new constant `<vector>` of a vector's elements
/// from an index, `(vector, start) -> <vector>`: a `#rest` parameter's
/// vector, of the arguments after the required ones. It takes
/// [`vector_tail_signature`].
pub const VECTOR_TAIL: &str = "interloko.collection.vector_tail";
/// Host-call symbol for a vector's element at an index, or a default when
/// the index is outside the vector. It takes
/// [`vector_element_or_signature`].
pub const VECTOR_ELEMENT_OR: &str = "interloko.collection.vector_element_or";
/// Host-call symbol for storing a vector's element at an index, `(value,
/// vector, index) -> value`. An index outside the vector raises
/// [`control::CHECK_FAILED_TAG`]. It takes
/// [`vector_element_setter_signature`].
pub const VECTOR_ELEMENT_SETTER: &str = "interloko.collection.vector_element_setter";
/// Host-call symbol for adding an element at the end of a
/// `<stretchy-vector>`, which it returns. It takes
/// [`stretchy_add_signature`].
pub const STRETCHY_ADD: &str = "interloko.collection.stretchy_add";
/// Host-call symbol for the value stored under a key in a `<table>`. A key
/// the table lacks raises [`control::CHECK_FAILED_TAG`]. It takes
/// [`table_element_signature`].
pub const TABLE_ELEMENT: &str = "interloko.collection.table_element";
/// Host-call symbol for the value stored under a key in a `<table>`, or a
/// default when the table lacks the key. It takes
/// [`table_element_or_signature`].
pub const TABLE_ELEMENT_OR: &str = "interloko.collection.table_element_or";
/// Host-call symbol for storing a value under a key in a `<table>`,
/// `(value, table, key) -> value`. It takes
/// [`table_element_setter_signature`].
pub const TABLE_ELEMENT_SETTER: &str = "interloko.collection.table_element_setter";
/// Host-call symbol for whether a `<table>` has a key. It takes
/// [`table_key_signature`].
pub const TABLE_KEY_EXISTS: &str = "interloko.collection.table_key_exists";
/// Host-call symbol for removing a key's entry from a `<table>`, returning
/// whether it had one. It takes [`table_key_signature`].
pub const TABLE_REMOVE_KEY: &str = "interloko.collection.table_remove_key";
/// Host-call symbol for `pair(head, tail)`, `(<object>, <object>) ->
/// <pair>`. It takes [`pair_signature`].
pub const LIST_PAIR: &str = "interloko.list.pair";
/// Host-call symbol for `head(list)`, `(<list>) -> <object>`: a pair's
/// head, or the empty list for the empty list. It takes
/// [`list_part_signature`].
pub const LIST_HEAD: &str = "interloko.list.head";
/// Host-call symbol for `tail(list)`, as [`LIST_HEAD`] reads the head.
pub const LIST_TAIL: &str = "interloko.list.tail";
/// Host-call symbol for `head-setter(value, pair)`, `(<object>, <pair>) ->
/// <object>`. It takes [`pair_part_setter_signature`].
pub const LIST_HEAD_SETTER: &str = "interloko.list.head_setter";
/// Host-call symbol for `tail-setter(value, pair)`, as
/// [`LIST_HEAD_SETTER`] stores the head.
pub const LIST_TAIL_SETTER: &str = "interloko.list.tail_setter";
/// Host-call symbol for `#()`, the empty list, `() -> <empty-list>`. It
/// takes [`empty_list_signature`].
pub const EMPTY_LIST: &str = "interloko.list.empty";
/// Host-call symbol for whether a vector is constant, as a literal is. It
/// takes [`vector_constant_signature`].
pub const VECTOR_CONSTANT: &str = "interloko.collection.vector_constant";
/// Host-call symbol for a `<table>`'s iteration version, which changes
/// whenever its keys do. It takes [`collection_size_signature`].
pub const TABLE_VERSION: &str = "interloko.collection.table_version";
/// Host-call symbol for a `<table>`'s number of entry positions, the limit
/// of its iteration states. It takes [`collection_size_signature`].
pub const TABLE_POSITIONS: &str = "interloko.collection.table_positions";
/// Host-call symbol for a `<table>`'s first entry position at or after a
/// position, `(table, from, version) -> position`, or its number of
/// positions when there is none. A table whose version is no longer
/// `version` raises [`control::CHECK_FAILED_TAG`]. It takes
/// [`table_next_position_signature`].
pub const TABLE_NEXT_POSITION: &str = "interloko.collection.table_next_position";
/// Host-call symbol for the key of a `<table>`'s entry at a position,
/// `(table, position, version) -> key`. A position holding no entry, or a
/// table whose version is no longer `version`, raises
/// [`control::CHECK_FAILED_TAG`]. It takes
/// [`table_position_signature`].
pub const TABLE_POSITION_KEY: &str = "interloko.collection.table_position_key";
/// Host-call symbol for the value of a `<table>`'s entry at a position, as
/// [`TABLE_POSITION_KEY`] reads its key. It takes
/// [`table_position_signature`].
pub const TABLE_POSITION_VALUE: &str = "interloko.collection.table_position_value";
/// Host-call symbol for `$unsupplied`, the value of a keyword argument a
/// call does not pass when the keyword has no default: one object, made
/// with the host, that nothing else is `==`.
pub const UNSUPPLIED: &str = "interloko.unsupplied";
/// Host-call symbol for the marker a call through a callable value passes
/// for a keyword it does not pass: an object of its own, distinct from
/// `$unsupplied`, which only the value's entry ever sees, and which it
/// replaces with the keyword's default.
pub const KEYWORD_ABSENT: &str = "interloko.keyword_absent";
/// Host-call symbol for testing whether a boxed value is the
/// [`KEYWORD_ABSENT`] marker.
pub const IS_KEYWORD_ABSENT: &str = "interloko.is_keyword_absent";
/// Host-call symbol for rendering a float as its shortest round-trip literal.
pub const FLOAT_TO_STRING: &str = "interloko.float_to_string";
/// Host-call symbol for rendering an integer in decimal.
pub const INTEGER_TO_STRING: &str = "interloko.integer_to_string";
/// Host-call symbol for concatenating two strings into a new string.
pub const STRING_CONCATENATE: &str = "interloko.string_concatenate";

/// Float library function served by the runtime host.
///
/// These are the float operations `execution_tape` has no instruction for;
/// rounding and `sqrt` are tape instructions (`f64.floor`, `f64.sqrt`, ...).
/// Each is a host call named [`FloatFunction::symbol`] taking
/// [`FloatFunction::arity`] `f64` arguments and returning one `f64`.
/// Implementations come from the pure-Rust `libm` crate rather than the
/// platform C library, so results are identical on every host, which keeps
/// recorded runs replayable.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FloatFunction {
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

/// Returns the host signature for [`FLOAT_TO_STRING`] on a `<single-float>`.
#[must_use]
pub fn single_float_to_string_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::F32],
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

/// Returns the host signature for [`BOX_CHARACTER`].
#[must_use]
pub fn box_character_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U32],
        rets: vec![any_type()],
    }
}

/// Returns the host signature for [`BOX_POSITION`].
#[must_use]
pub fn box_position_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::I64],
        rets: vec![any_type()],
    }
}

/// Returns the host signature for [`CHECK_POSITION`].
#[must_use]
pub fn check_position_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::I64, ValueType::U64],
        rets: vec![ValueType::I64],
    }
}

/// Returns the host signature for [`CHECK_CHARACTER`].
#[must_use]
pub fn check_character_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U32, ValueType::U64],
        rets: vec![ValueType::U32],
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
    /// `<i32>` integer value.
    I32,
    /// `<u32>` integer value.
    U32,
    /// IEEE 754 binary32 float value.
    F32,
    /// Boolean value.
    Bool,
    /// `<character>` value: a Unicode scalar value in a `u32` register.
    Character,
    /// `<string-position>` value: a byte offset in an `i64` register,
    /// checked against its string wherever it is used.
    Position,
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
    /// First-class callable, stored as a VM closure. Its environment handle
    /// belongs to the invocation that created it.
    Callable,
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
    /// Whether the class's direct subclasses are exactly the ones this
    /// layout lists; see [`ClassHierarchy::sealed_subclasses`].
    pub sealing: Sealing,
    /// Whether the class has no direct instances.
    pub is_abstract: bool,
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
    /// Whether the generic's methods are exactly the ones this layout lists
    /// and nothing from outside its source may add one.
    pub sealing: Sealing,
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
    /// A cell operation named an object that is not a cell.
    NotACell,
    /// A `<string>` would hold more bytes than a `<string-position>` can
    /// address.
    StringTooLong,
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
    /// Native object construction was requested for an abstract class,
    /// which has no direct instances.
    ClassIsAbstract {
        /// Requested class.
        class: ClassId,
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
    /// A nursery handle, for a runtime object or a heap box, named no entry
    /// of the open turn: the turn that allocated it has finished or
    /// aborted, so the handle is stale. Keep values past a turn by passing
    /// them to [`InterlokoHost::finish_turn`] or registering them with
    /// [`InterlokoHost::register_root`].
    StaleNurseryHandle {
        /// The stale handle, with its host type.
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
    /// [`InterlokoHost::begin_turn`] was called while a turn was open.
    TurnAlreadyOpen,
    /// A turn operation was called with no turn open.
    NoOpenTurn,
    /// Evacuating a turn's survivors would exceed the evacuation budget.
    /// The turn stays open; abort it.
    EvacuationBudgetExceeded {
        /// Work the evacuation needs.
        work: u64,
        /// Configured budget.
        budget: u64,
    },
    /// An image root id was never registered or was already released.
    InvalidImageRoot {
        /// Offending root id.
        root: ImageRootId,
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

    /// Returns the record this layout makes of the class or generic
    /// `assumption` names, by source name: for a sealed class, its direct
    /// subclasses and abstractness here, or `None` when the class is
    /// missing or open; for a generic, its methods and sealing here, or
    /// `None` when it is missing. [`crate::code::CodeUnit::check_assumptions`]
    /// compares the two.
    #[must_use]
    pub fn closed_world_like(&self, assumption: &ClosedWorld) -> Option<ClosedWorld> {
        let mut name = String::new();
        match assumption {
            ClosedWorld::SealedClass { class, .. } => {
                let id = self.class_id(class)?;
                let runtime = self.class(id)?;
                if !runtime.sealing.is_sealed() {
                    return None;
                }
                Some(ClosedWorld::SealedClass {
                    class: runtime.name.clone(),
                    subclasses: self
                        .direct_subclasses(id)
                        .into_iter()
                        .filter_map(|subclass| Some(self.class(subclass)?.name.clone()))
                        .collect(),
                    is_abstract: runtime.is_abstract,
                })
            }
            ClosedWorld::Methods { generic, .. } => {
                let id = self.generic_id(generic)?;
                let runtime = self.generic(id)?;
                let methods = runtime
                    .methods
                    .iter()
                    .map(|method| {
                        name.clear();
                        let _ = self.write_method(&mut name, *method);
                        name.clone()
                    })
                    .collect();
                Some(ClosedWorld::Methods {
                    generic: runtime.name.clone(),
                    sealing: runtime.sealing,
                    methods,
                })
            }
        }
    }

    /// Returns the classes whose direct superclass is `class`, in class id
    /// order.
    #[must_use]
    pub fn direct_subclasses(&self, class: ClassId) -> Vec<ClassId> {
        self.classes
            .iter()
            .enumerate()
            .filter(|(_, other)| other.superclass == Some(class))
            .map(|(index, _)| ClassId(u32::try_from(index).unwrap_or(u32::MAX)))
            .collect()
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

/// The runtime layout's classes, for comparing [`interloko_model::types::Type`]s
/// such as code-unit [contracts](crate::code::Contracts).
///
/// The layout mirrors the compiler's classes in the same order, so it
/// answers as the compiler's hierarchy did when it built those types.
impl ClassHierarchy for RuntimeLayout {
    fn superclass(&self, class: ClassId) -> Option<ClassId> {
        self.class(class)?.superclass
    }

    /// A built-in value or host-backed class, or a sealed concrete native
    /// class, is closed unless another class inherits from it, as in the
    /// compiler.
    fn is_closed(&self, class: ClassId) -> bool {
        self.class(class).is_some_and(|runtime| match runtime.kind {
            HostObjectKind::BuiltinValue { .. } | HostObjectKind::HostBacked => true,
            HostObjectKind::Native => runtime.sealing.is_sealed() && !runtime.is_abstract,
        }) && !self
            .classes
            .iter()
            .any(|other| other.superclass == Some(class))
    }

    fn sealed_subclasses(&self, class: ClassId) -> Option<Vec<ClassId>> {
        self.class(class)?
            .sealing
            .is_sealed()
            .then(|| self.direct_subclasses(class))
    }

    fn is_abstract(&self, class: ClassId) -> bool {
        self.class(class).is_some_and(|runtime| runtime.is_abstract)
    }

    fn builtin_class(&self, kind: BuiltinValueClass) -> Option<ClassId> {
        self.classes
            .iter()
            .position(|class| class.kind == HostObjectKind::BuiltinValue { kind })
            .map(|index| ClassId(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    fn is_same_or_subclass(&self, class: ClassId, ancestor: ClassId) -> bool {
        Self::is_same_or_subclass(self, class, ancestor)
    }
}

impl TypeNames for RuntimeLayout {
    fn write_class(&self, out: &mut dyn fmt::Write, class: ClassId) -> fmt::Result {
        match self.class(class) {
            Some(runtime) => out.write_str(&runtime.name),
            None => write!(out, "class#{}", class.0),
        }
    }

    fn write_generic(&self, out: &mut dyn fmt::Write, generic: GenericFunctionId) -> fmt::Result {
        match self.generic(generic) {
            Some(runtime) => out.write_str(&runtime.name),
            None => write!(out, "generic#{}", generic.0),
        }
    }

    /// Writes `generic(<specializer>, ...)`, with `<object>` for an
    /// unspecialized parameter.
    fn write_method(&self, out: &mut dyn fmt::Write, method: MethodId) -> fmt::Result {
        let Some(runtime) = self.method(method) else {
            return write!(out, "method#{}", method.0);
        };
        self.write_generic(out, runtime.generic)?;
        out.write_str("(")?;
        for (index, specializer) in runtime.specializers.iter().enumerate() {
            if index > 0 {
                out.write_str(", ")?;
            }
            self.write_class(out, specializer.unwrap_or(OBJECT_CLASS_ID))?;
        }
        out.write_str(")")
    }

    /// Writes `<class>.slot-name`.
    fn write_slot(&self, out: &mut dyn fmt::Write, class: ClassId, slot: SlotId) -> fmt::Result {
        self.write_class(out, class)?;
        match self.slot(class, slot) {
            Some(runtime) => write!(out, ".{}", runtime.name),
            None => write!(out, ".slot#{}", slot.0),
        }
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
    /// A `<string>`: immutable, validated UTF-8 with its header.
    String {
        text: StringText,
    },
    /// A `<string-builder>`'s growing text and its character count.
    StringBuilder {
        text: String,
        scalars: usize,
    },
    Symbol {
        name: String,
    },
    /// A `<vector>` or `<stretchy-vector>`'s elements, each a boxed
    /// `<object>`.
    Vector {
        elements: Vec<Value>,
        /// Whether programs may not store into it: a vector literal, or a
        /// vector the runtime made, such as a metaobject's slots.
        constant: bool,
    },
    /// A `<table>`'s entries.
    Table(alloc::boxed::Box<collection::Table>),
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
    /// A `<limited-integer>` type object.
    LimitedIntegerType {
        range: IntRange,
        named: Option<LimitedInteger>,
    },
    /// A `<subclass-type>` type object.
    SubclassType {
        bound: ClassId,
    },
    /// A `<singleton>` or `<union>` type object: one that holds values or
    /// other types rather than naming a class or a range.
    CompoundType(interloko_model::type_object::TypeObject),
    /// A kernel marker: the one object of its kind, which no other value is
    /// `==`: a direct `<object>` instance, or `#()`, the `<empty-list>`.
    Marker(Marker),
    HostBacked {
        adapter: alloc::boxed::Box<dyn HostObjectAdapter>,
    },
}

impl RuntimeObjectStorage {
    /// Whether two objects are the same Loko object although the heap holds
    /// them separately.
    ///
    /// Symbols, metaobjects and type objects stand for something outside the
    /// heap (a name, a class, a generic, a method, a slot, a type), and a
    /// fresh heap object may be made each time one is named, so each is the
    /// same object as any other standing for the same thing. Every other
    /// object is only ever itself.
    fn denotes_same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Symbol { name: left }, Self::Symbol { name: right }) => left == right,
            (Self::GenericMeta { generic: left }, Self::GenericMeta { generic: right }) => {
                left == right
            }
            (Self::MethodMeta { method: left }, Self::MethodMeta { method: right }) => {
                left == right
            }
            (
                Self::SlotDescriptor {
                    class: left_class,
                    slot: left_slot,
                },
                Self::SlotDescriptor {
                    class: right_class,
                    slot: right_slot,
                },
            ) => left_class == right_class && left_slot == right_slot,
            // Type objects are the same when they are the same type.
            (left, right) => match (left.type_object(), right.type_object()) {
                (Some(left), Some(right)) => left.same_type(&right),
                _ => false,
            },
        }
    }
}

/// A kernel marker object; see [`InterlokoHost::marker`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Marker {
    /// `$unsupplied`, which a keyword without a default takes when a call
    /// does not pass it.
    Unsupplied,
    /// What a call through a callable value passes for a keyword it does
    /// not pass ([`KEYWORD_ABSENT`]).
    KeywordAbsent,
    /// `#()`, the empty list, the one instance of `<empty-list>`.
    EmptyList,
}

impl Marker {
    const COUNT: usize = 3;

    /// How a value report writes the marker.
    const fn name(self) -> &'static str {
        match self {
            Self::Unsupplied => "$unsupplied",
            Self::KeywordAbsent => "{keyword absent}",
            Self::EmptyList => "#()",
        }
    }

    /// The marker's class: `<empty-list>` for the empty list, and
    /// `<object>` for the others, which are direct instances of it.
    const fn class(self) -> Option<BuiltinValueClass> {
        match self {
            Self::EmptyList => Some(BuiltinValueClass::EmptyList),
            Self::Unsupplied | Self::KeywordAbsent => None,
        }
    }
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
///
/// The heap is split into a long-lived image and a per-turn nursery; see
/// [`InterlokoHost::begin_turn`].
pub struct InterlokoHost {
    layout: RuntimeLayout,
    image: Region,
    nursery: Region,
    /// Where the open turn's nursery handle serials start.
    nursery_serials: NurserySerials,
    symbols: Vec<InternedObject>,
    /// `$unsupplied` and the keyword-absent marker, by [`Marker`], each made
    /// in the image the first time anything asks for it, so testing for
    /// one is a handle compare. Like an interned symbol, a marker stays
    /// however the turn that made it ends.
    markers: [Option<Obj>; Marker::COUNT],
    /// Values registered as image roots; released roots leave `None`.
    roots: Vec<Option<Value>>,
    turn: Option<OpenTurn>,
    /// Most evacuation work a turn may need to finish.
    evacuation_budget: u64,
    /// The open driver run's generic sends, answered inline; see
    /// [`InterlokoHost::send_cache`].
    sends: send::RunSends,
    /// The values of the constants read so far; see `constant.rs`.
    constants: constant::ConstantCells,
}

impl InterlokoHost {
    /// Creates a new runtime host with an empty object heap.
    #[must_use]
    pub fn new(layout: RuntimeLayout) -> Self {
        debug_assert!(layout.validate().is_ok());
        Self {
            layout,
            image: Region::default(),
            nursery: Region::default(),
            nursery_serials: NurserySerials::default(),
            symbols: Vec::new(),
            markers: [None; Marker::COUNT],
            roots: Vec::new(),
            turn: None,
            evacuation_budget: u64::MAX,
            sends: send::RunSends::default(),
            constants: constant::ConstantCells::default(),
        }
    }

    /// Returns the runtime layout this host serves.
    #[must_use]
    pub const fn layout(&self) -> &RuntimeLayout {
        &self.layout
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
                text: StringText::new(value.into()).ok_or(RuntimeError::StringTooLong)?,
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
            RuntimeObjectStorage::String { text } => Ok(text.as_str()),
            _ => Err(RuntimeError::ValueKindMismatch {
                expected: ValueKind::String(class),
            }),
        }
    }

    /// The UTF-8 of the `<string>` `object`, which compiled code reads with
    /// `obj.utf8_next` and friends instead of a host call per character, or
    /// `None` for any other object.
    ///
    /// A `<string>` is immutable once made, so its bytes never change while
    /// it lives, as `Host::object_bytes` requires. A `<string-builder>`
    /// changes, so it lends nothing.
    #[must_use]
    pub fn string_bytes(&self, object: Obj) -> Option<&[u8]> {
        if object.host_type != INTERLOKO_OBJECT_TYPE {
            return None;
        }
        match &self.object(object).ok()?.storage {
            RuntimeObjectStorage::String { text } => Some(text.as_str().as_bytes()),
            _ => None,
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
        // Typing reads a sealed abstract class's cone without its own
        // instances, so none may exist.
        if class_info.is_abstract {
            return Err(RuntimeError::ClassIsAbstract { class });
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
            | RuntimeObjectStorage::StringBuilder { .. }
            | RuntimeObjectStorage::Symbol { .. }
            | RuntimeObjectStorage::Vector { .. }
            | RuntimeObjectStorage::Table(_)
            | RuntimeObjectStorage::ClassMeta { .. }
            | RuntimeObjectStorage::GenericMeta { .. }
            | RuntimeObjectStorage::MethodMeta { .. }
            | RuntimeObjectStorage::SlotDescriptor { .. }
            | RuntimeObjectStorage::LimitedIntegerType { .. }
            | RuntimeObjectStorage::SubclassType { .. }
            | RuntimeObjectStorage::CompoundType(_)
            | RuntimeObjectStorage::Marker(_) => Err(RuntimeError::InvalidSlot {
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

    /// Makes a cell holding `value`: the storage of a local that `:=`
    /// assigns and closures capture, which the frame and each closure share.
    ///
    /// A cell is a native `<object>` with one slot that only the cell
    /// operations touch; the compiler keeps it out of reach of source code,
    /// and keeps what it stores within the local's declared type. It lives
    /// in the heap like any object, so it survives its frame for as long as
    /// a closure refers to it, within the run that made it.
    pub fn make_cell(&mut self, value: Value) -> Value {
        self.push_object(RuntimeObject {
            class: OBJECT_CLASS_ID,
            storage: RuntimeObjectStorage::Native { slots: vec![value] },
        })
    }

    /// Reads the value the cell `cell` holds.
    pub fn read_cell(&self, cell: Obj) -> Result<Value, RuntimeError> {
        match &self.object(cell)?.storage {
            RuntimeObjectStorage::Native { slots } if slots.len() == 1 => Ok(slots[0].clone()),
            _ => Err(RuntimeError::NotACell),
        }
    }

    /// Stores `value` in the cell `cell`, logging an image cell's old value
    /// as a slot store is logged, so an aborted turn restores it.
    pub fn write_cell(&mut self, cell: Obj, value: Value) -> Result<(), RuntimeError> {
        let location = self.object_ref(cell)?;
        let RuntimeObjectStorage::Native { slots } = &self.object(cell)?.storage else {
            return Err(RuntimeError::NotACell);
        };
        let [old] = slots.as_slice() else {
            return Err(RuntimeError::NotACell);
        };
        let old = old.clone();
        self.log_store(location.space, || region::LoggedStore::Slot {
            index: location.index,
            slot: 0,
            old,
        });
        let RuntimeObjectStorage::Native { slots } = &mut self.object_mut(cell)?.storage else {
            return Err(RuntimeError::NotACell);
        };
        slots[0] = value;
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
        let location = self.object_ref(object)?;
        let in_image_turn = self.turn.is_some() && location.space == Space::Image;
        if in_image_turn
            && let RuntimeObjectStorage::Native { slots } = &self.object(object)?.storage
            && let Some(old) = slots.get(slot.0 as usize)
        {
            let old = old.clone();
            self.log_store(location.space, || region::LoggedStore::Slot {
                index: location.index,
                slot: slot.0 as usize,
                old,
            });
        }
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

    /// Decodes a value of the top `<object>` runtime representation into
    /// what it holds, undoing [`Self::box_top_value`]: an `<i32>` or
    /// `<u32>` boxed comes back as the `<integer>` it is.
    pub fn unbox_top_value(&self, value: &Value) -> Result<Unboxed, RuntimeError> {
        match value {
            Value::Obj(obj) => self.unbox(*obj),
            _ => Err(RuntimeError::ValueKindMismatch {
                expected: ValueKind::Any,
            }),
        }
    }

    /// Boxes a `<character>` into the top `<object>` runtime
    /// representation, which a `u32` register value would box as a `<u32>`
    /// integer instead.
    #[must_use]
    pub const fn box_top_character(value: char) -> Value {
        Value::Obj(Obj {
            host_type: INTERLOKO_VALUE_TYPE,
            handle: BoxedHandle::immediate_character(value),
        })
    }

    /// Boxes a value into the top `<object>` runtime representation.
    ///
    /// `<i32>` and `<u32>` values box as the `<integer>`s they are.
    pub fn box_top_value(&mut self, value: Value) -> Result<Value, RuntimeError> {
        match value {
            Value::I64(_)
            | Value::F64(_)
            | Value::I32(_)
            | Value::U32(_)
            | Value::F32(_)
            | Value::Bool(_)
            | Value::U64(_)
            | Value::Agg(_) => Ok(self.push_boxed_value(value)),
            Value::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                Ok(self.push_boxed_value(Value::Obj(obj)))
            }
            Value::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => Ok(Value::Obj(obj)),
            _ => Err(RuntimeError::CannotBoxValue),
        }
    }

    fn object_ref(&self, obj: Obj) -> Result<ObjectRef, RuntimeError> {
        if obj.host_type != INTERLOKO_OBJECT_TYPE {
            return Err(RuntimeError::ObjectHostTypeMismatch {
                object: obj,
                expected: INTERLOKO_OBJECT_TYPE,
            });
        }
        ObjectRef::decode(obj.handle, self.nursery_serials)
            .ok_or(RuntimeError::InvalidObjectHandle { object: obj })
    }

    /// The error for a well-formed handle into `space` that names no entry.
    #[cold]
    const fn unresolved(space: Space, object: Obj, invalid: RuntimeError) -> RuntimeError {
        match space {
            Space::Image => invalid,
            Space::Nursery => RuntimeError::StaleNurseryHandle { object },
        }
    }

    const fn region(&self, space: Space) -> &Region {
        match space {
            Space::Image => &self.image,
            Space::Nursery => &self.nursery,
        }
    }

    const fn region_mut(&mut self, space: Space) -> &mut Region {
        match space {
            Space::Image => &mut self.image,
            Space::Nursery => &mut self.nursery,
        }
    }

    fn object(&self, obj: Obj) -> Result<&RuntimeObject, RuntimeError> {
        let location = self.object_ref(obj)?;
        self.region(location.space)
            .objects
            .get(location.index)
            .ok_or_else(|| {
                Self::unresolved(
                    location.space,
                    obj,
                    RuntimeError::InvalidObjectHandle { object: obj },
                )
            })
    }

    fn object_mut(&mut self, obj: Obj) -> Result<&mut RuntimeObject, RuntimeError> {
        let location = self.object_ref(obj)?;
        self.region_mut(location.space)
            .objects
            .get_mut(location.index)
            .ok_or_else(|| {
                Self::unresolved(
                    location.space,
                    obj,
                    RuntimeError::InvalidObjectHandle { object: obj },
                )
            })
    }

    /// Decodes a boxed `<object>` value.
    fn unbox(&self, obj: Obj) -> Result<Unboxed, RuntimeError> {
        if obj.host_type != INTERLOKO_VALUE_TYPE {
            return Err(RuntimeError::ValueHostTypeMismatch {
                object: obj,
                expected: INTERLOKO_VALUE_TYPE,
            });
        }
        let value = match BoxedHandle::decode(obj.handle, self.nursery_serials) {
            BoxedHandle::Float(value) => Value::F64(value),
            BoxedHandle::SingleFloat(value) => Value::F32(value),
            BoxedHandle::Integer(value) => Value::I64(value),
            BoxedHandle::Bool(value) => Value::Bool(value),
            BoxedHandle::Character(value) => return Ok(Unboxed::Character(value)),
            BoxedHandle::Position(offset) => return Ok(Unboxed::Position(offset)),
            BoxedHandle::Object(handle) => Value::Obj(Obj {
                host_type: INTERLOKO_OBJECT_TYPE,
                handle,
            }),
            BoxedHandle::Heap(space, index) => self
                .region(space)
                .values
                .get(index)
                .cloned()
                .ok_or_else(|| {
                    Self::unresolved(
                        space,
                        obj,
                        RuntimeError::InvalidBoxedValueHandle { object: obj },
                    )
                })?,
            BoxedHandle::Invalid => {
                return Err(RuntimeError::InvalidBoxedValueHandle { object: obj });
            }
        };
        Ok(Unboxed::Value(value))
    }

    /// Decodes a boxed `<object>` value that has an `execution_tape` value
    /// of its own: anything but a `<character>`, whose register type, `u32`,
    /// would read as a `<u32>` integer.
    fn boxed_value(&self, obj: Obj) -> Result<Value, RuntimeError> {
        match self.unbox(obj)? {
            Unboxed::Value(value) => Ok(value),
            Unboxed::Character(_) | Unboxed::Position(_) => {
                Err(RuntimeError::InvalidBoxedValueHandle { object: obj })
            }
        }
    }

    /// Boxes `value` as a top `<object>` value, carrying integers, floats,
    /// booleans, and runtime-object handles in the handle when they fit and
    /// allocating a heap box otherwise.
    fn push_boxed_value(&mut self, value: Value) -> Value {
        let value = match value {
            Value::I32(value) => Value::I64(i64::from(value)),
            Value::U32(value) => Value::I64(i64::from(value)),
            value => value,
        };
        let immediate = match value {
            Value::I64(value) => BoxedHandle::immediate_integer(value),
            Value::F64(value) => BoxedHandle::immediate_float(value),
            Value::F32(value) => Some(BoxedHandle::immediate_single_float(value)),
            Value::Bool(value) => Some(BoxedHandle::immediate_bool(value)),
            Value::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                BoxedHandle::immediate_object(obj.handle)
            }
            _ => None,
        };
        let handle = immediate.unwrap_or_else(|| {
            let space = self.allocation_space();
            let region = self.region_mut(space);
            let index = region.values.len();
            region.values.push(value);
            // A heap too large to index yields a handle that never resolves,
            // so it fails on use instead of aliasing another value.
            BoxedHandle::heap(space, index, self.nursery_serials).unwrap_or(BoxedHandle::INVALID)
        });
        Value::Obj(Obj {
            host_type: INTERLOKO_VALUE_TYPE,
            handle,
        })
    }

    /// Allocates `object` in the nursery during a turn and in the image
    /// otherwise.
    fn push_object(&mut self, object: RuntimeObject) -> Value {
        let space = self.allocation_space();
        self.push_object_in(space, object)
    }

    fn push_object_in(&mut self, space: Space, object: RuntimeObject) -> Value {
        let region = self.region_mut(space);
        let index = region.objects.len();
        region.objects.push(object);
        // A region too large to index yields a handle that never resolves.
        let handle = ObjectRef { space, index }
            .encode(self.nursery_serials)
            .unwrap_or(ObjHandle(0));
        Value::Obj(Obj {
            host_type: INTERLOKO_OBJECT_TYPE,
            handle,
        })
    }
}

impl Host for InterlokoHost {
    /// Every import but the driver's control requests runs inline, and so
    /// does [`code::CODE_DISPATCH`] while the open run can tell its senders
    /// apart (see [`InterlokoHost::send_cache`]).
    fn is_synchronous(&self, symbol: &str, _: SigHash) -> bool {
        if symbol == code::CODE_DISPATCH {
            return self.sends.inline();
        }
        !crate::control::is_control_symbol(symbol)
    }
    fn object_key(&self, object: Obj) -> Option<u64> {
        Self::object_key(self, object)
    }
    fn convert_object(&self, object: Obj, to: HostTypeId) -> Option<Obj> {
        Self::convert_object(self, object, to)
    }
    fn object_bytes<R>(&self, object: Obj, read: impl FnOnce(Option<&[u8]>) -> R) -> R {
        read(self.string_bytes(object))
    }
    fn object_field(&self, object: Obj, field: u32) -> Option<Value> {
        Self::object_field(self, object, field)
    }
    fn set_object_field(&mut self, object: Obj, field: u32, value: Value) -> bool {
        Self::set_object_field(self, object, field, &value)
    }
    /// Charges every host call [`HOST_CALL_FUEL`], above the instruction
    /// that makes it.
    fn call(
        &mut self,
        symbol: &str,
        sig_hash: SigHash,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
        ctx: HostContext<'_, '_>,
    ) -> Result<u64, HostError> {
        self.call_import(symbol, sig_hash, args, rets, ctx)
            .map(|extra| extra.saturating_add(HOST_CALL_FUEL))
    }
}

impl InterlokoHost {
    /// Answers one runtime host call; [`Host::call`] charges for it.
    fn call_import(
        &mut self,
        symbol: &str,
        sig_hash: SigHash,
        args: &[ValueRef<'_>],
        rets: &mut [Value],
        mut ctx: HostContext<'_, '_>,
    ) -> Result<u64, HostError> {
        if symbol == code::CODE_DISPATCH {
            let target = self.dispatch_send(ctx.program(), args)?;
            let [ret] = rets else {
                return Err(HostError::Failed);
            };
            *ret = Value::Func(target);
            return Ok(0);
        }
        if let Some(result) = self.constant_call(symbol, &ctx, args, rets) {
            return result;
        }
        if let Some(function) = IntegerDivision::from_symbol(symbol) {
            let ([ValueRef::I64(dividend), ValueRef::I64(divisor)], [ret]) = (args, rets) else {
                return Err(HostError::Failed);
            };
            *ret = Value::I64(
                function
                    .apply(*dividend, *divisor)
                    .ok_or(HostError::Failed)?,
            );
            return Ok(0);
        }
        if symbol == INTEGER_POWER {
            let ([ValueRef::I64(base), ValueRef::I64(exponent)], [power, overflowed]) =
                (args, rets)
            else {
                return Err(HostError::Failed);
            };
            if *exponent < 0 {
                return Err(HostError::Failed);
            }
            let result = integer_power(*base, *exponent);
            *power = Value::I64(result.unwrap_or(0));
            *overflowed = Value::Bool(result.is_none());
            return Ok(0);
        }
        if let Some(function) = TextFunction::from_symbol(symbol) {
            let [ret] = rets else {
                return Err(HostError::Failed);
            };
            *ret = match self.call_text(function, args) {
                Ok(value) => value,
                Err(text::TextFailure::Rejected) => return Err(type_error::check_failed(&ctx)),
                Err(text::TextFailure::Failed) => return Err(HostError::Failed),
            };
            return Ok(0);
        }
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
                // A literal is a constant: see `constant.rs`.
                if let Some(string) = self.constants.literal(value) {
                    rets[0] = Value::Obj(string);
                    return Ok(0);
                }
                let class = self.builtin_class(BuiltinValueClass::String)?;
                let string = self.push_object(RuntimeObject {
                    class,
                    storage: RuntimeObjectStorage::String {
                        text: StringText::new(String::from(*value)).ok_or(HostError::Failed)?,
                    },
                });
                if let Value::Obj(object) = string {
                    self.constants.remember_literal(value, object);
                }
                rets[0] = string;
                Ok(0)
            }
            LIST_PAIR => {
                let [head, tail] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.make_pair(head.to_value(), tail.to_value())?;
                Ok(0)
            }
            LIST_HEAD | LIST_TAIL => {
                let [ValueRef::Obj(list)] = args else {
                    return Err(HostError::Failed);
                };
                let part = if symbol == LIST_HEAD {
                    list::PairPart::Head
                } else {
                    list::PairPart::Tail
                };
                rets[0] = self.list_part(*list, part)?;
                Ok(0)
            }
            LIST_HEAD_SETTER | LIST_TAIL_SETTER => {
                let [value, ValueRef::Obj(pair)] = args else {
                    return Err(HostError::Failed);
                };
                let part = if symbol == LIST_HEAD_SETTER {
                    list::PairPart::Head
                } else {
                    list::PairPart::Tail
                };
                rets[0] = self.set_pair_part(value.to_value(), *pair, part)?;
                Ok(0)
            }
            EMPTY_LIST => {
                let [] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Obj(self.empty_list());
                Ok(0)
            }
            IS_KEYWORD_ABSENT => {
                let [value] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Bool(self.is_marker(*value, Marker::KeywordAbsent));
                Ok(0)
            }
            UNSUPPLIED | KEYWORD_ABSENT => {
                let [] = args else {
                    return Err(HostError::Failed);
                };
                let marker = if symbol == UNSUPPLIED {
                    Marker::Unsupplied
                } else {
                    Marker::KeywordAbsent
                };
                let object = self.marker(marker);
                rets[0] = self.push_boxed_value(Value::Obj(object));
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
                    storage: RuntimeObjectStorage::Vector {
                        elements: values,
                        constant: true,
                    },
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
            IDENTICAL => {
                let [ValueRef::Obj(lhs), ValueRef::Obj(rhs)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Bool(self.identical(*lhs, *rhs, &ctx)?);
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
            META_CLASS_ID => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::U64(u64::from(self.meta_class_from_any(*arg)?.0));
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
            INSTANCE_OF_TYPE => {
                let [value, ty] = args else {
                    return Err(HostError::Failed);
                };
                let ty = self.type_object_from_any(*ty)?;
                rets[0] = Value::Bool(self.value_is_type_instance(*value, &ty));
                Ok(0)
            }
            LIMITED_INTEGER_TYPE => {
                let [
                    ValueRef::Bool(has_min),
                    ValueRef::I64(min),
                    ValueRef::Bool(has_max),
                    ValueRef::I64(max),
                    ValueRef::U64(named),
                ] = args
                else {
                    return Err(HostError::Failed);
                };
                let range = IntRange {
                    min: has_min.then_some(*min),
                    max: has_max.then_some(*max),
                };
                let named = match named {
                    0 => None,
                    index => Some(
                        *LimitedInteger::ALL
                            .get(usize::try_from(index - 1).map_err(|_| HostError::Failed)?)
                            .ok_or(HostError::Failed)?,
                    ),
                };
                rets[0] =
                    self.make_type_object(
                        &interloko_model::type_object::TypeObject::LimitedInteger { range, named },
                    )
                    .map_err(|_| HostError::Failed)?;
                Ok(0)
            }
            MOP_LIMITED_INTEGER_BASE | MOP_LIMITED_INTEGER_MIN | MOP_LIMITED_INTEGER_MAX => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let interloko_model::type_object::TypeObject::LimitedInteger { range, .. } =
                    self.type_object_from_any(*arg)?
                else {
                    return Err(HostError::Failed);
                };
                let bound = match symbol {
                    MOP_LIMITED_INTEGER_BASE => {
                        let integer = self.builtin_class(BuiltinValueClass::Integer)?;
                        rets[0] = self.push_class_meta(integer)?;
                        return Ok(0);
                    }
                    MOP_LIMITED_INTEGER_MIN => range.min,
                    _ => range.max,
                };
                rets[0] = self.push_boxed_value(bound.map_or(Value::Bool(false), Value::I64));
                Ok(0)
            }
            MOP_TYPE_NAME => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let name = self.type_object_name(&self.type_object_from_any(*arg)?);
                rets[0] = self.push_string(name)?;
                Ok(0)
            }
            MOP_SUBCLASS_TYPE_CLASS => {
                let [arg] = args else {
                    return Err(HostError::Failed);
                };
                let interloko_model::type_object::TypeObject::Subclass(bound) =
                    self.type_object_from_any(*arg)?
                else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.push_class_meta(bound)?;
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
                let text = match args {
                    [ValueRef::F64(value)] => alloc::format!("{}", FloatBits::from_f64(*value)),
                    [ValueRef::F32(value)] => {
                        alloc::format!("{}", SingleFloatBits::from_f32(*value))
                    }
                    _ => return Err(HostError::Failed),
                };
                rets[0] = self.push_string(text)?;
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
            MAKE_VECTOR => {
                let [ValueRef::U64(class), ValueRef::I64(size), fill] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.make_vector(runtime_class_id(*class)?, *size, fill.to_value())?;
                Ok(0)
            }
            MAKE_TABLE => {
                rets[0] = self.make_table()?;
                Ok(0)
            }
            COLLECTION_SIZE => {
                let [ValueRef::Obj(collection)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::I64(self.collection_size(*collection)?);
                Ok(0)
            }
            VECTOR_ELEMENT => {
                let [ValueRef::Obj(vector), ValueRef::I64(index)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self
                    .vector_element(*vector, *index)?
                    .ok_or_else(|| type_error::check_failed(&ctx))?;
                Ok(0)
            }
            VECTOR_TAIL => {
                let [ValueRef::Obj(vector), ValueRef::I64(start)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.vector_tail(*vector, *start)?;
                Ok(0)
            }
            VECTOR_ELEMENT_OR => {
                let [ValueRef::Obj(vector), ValueRef::I64(index), default] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self
                    .vector_element(*vector, *index)?
                    .unwrap_or_else(|| default.to_value());
                Ok(0)
            }
            VECTOR_ELEMENT_SETTER => {
                let [value, ValueRef::Obj(vector), ValueRef::I64(index)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self
                    .vector_element_setter(value.to_value(), *vector, *index)?
                    .ok_or_else(|| type_error::check_failed(&ctx))?;
                Ok(0)
            }
            STRETCHY_ADD => {
                let [ValueRef::Obj(vector), value] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.stretchy_add(*vector, value.to_value())?;
                Ok(0)
            }
            TABLE_ELEMENT => {
                let [ValueRef::Obj(table), key] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self
                    .table_lookup(*table, key.to_value(), Some(&ctx))?
                    .ok_or_else(|| type_error::check_failed(&ctx))?;
                Ok(0)
            }
            TABLE_ELEMENT_OR => {
                let [ValueRef::Obj(table), key, default] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self
                    .table_lookup(*table, key.to_value(), Some(&ctx))?
                    .unwrap_or_else(|| default.to_value());
                Ok(0)
            }
            TABLE_ELEMENT_SETTER => {
                let [value, ValueRef::Obj(table), key] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.table_element_setter(
                    value.to_value(),
                    *table,
                    key.to_value(),
                    Some(&ctx),
                )?;
                Ok(0)
            }
            TABLE_KEY_EXISTS => {
                let [ValueRef::Obj(table), key] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Bool(
                    self.table_lookup(*table, key.to_value(), Some(&ctx))?
                        .is_some(),
                );
                Ok(0)
            }
            VECTOR_CONSTANT => {
                let [ValueRef::Obj(vector)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Bool(self.vector_is_constant(*vector)?);
                Ok(0)
            }
            TABLE_VERSION | TABLE_POSITIONS => {
                let [ValueRef::Obj(table)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::I64(if symbol == TABLE_VERSION {
                    self.table_iteration_version(*table)?
                } else {
                    self.table_positions(*table)?
                });
                Ok(0)
            }
            TABLE_NEXT_POSITION => {
                let [
                    ValueRef::Obj(table),
                    ValueRef::I64(from),
                    ValueRef::I64(version),
                ] = args
                else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::I64(
                    self.table_next_position(*table, *from, *version)?
                        .ok_or_else(|| type_error::check_failed(&ctx))?,
                );
                Ok(0)
            }
            TABLE_POSITION_KEY | TABLE_POSITION_VALUE => {
                let [
                    ValueRef::Obj(table),
                    ValueRef::I64(position),
                    ValueRef::I64(version),
                ] = args
                else {
                    return Err(HostError::Failed);
                };
                rets[0] = self
                    .table_entry_part(*table, *position, *version, symbol == TABLE_POSITION_KEY)?
                    .ok_or_else(|| type_error::check_failed(&ctx))?;
                Ok(0)
            }
            TABLE_REMOVE_KEY => {
                let [ValueRef::Obj(table), key] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::Bool(self.table_remove_key(*table, key.to_value(), Some(&ctx))?);
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
                let [value @ ValueRef::Obj(_)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = Value::U64(u64::from(self.class_of_any(*value)?.0));
                Ok(0)
            }
            UNBOX_VALUE => {
                let [ValueRef::Obj(obj), ValueRef::U64(class_raw)] = args else {
                    return Err(HostError::Failed);
                };
                let class = runtime_class_id(*class_raw)?;
                if !self.value_is_instance(ValueRef::Obj(*obj), class) {
                    return Err(type_error::check_failed(&ctx));
                }
                rets[0] = match self.unbox(*obj)? {
                    Unboxed::Value(
                        value @ (Value::I64(_)
                        | Value::F64(_)
                        | Value::F32(_)
                        | Value::Bool(_)
                        | Value::Agg(_)),
                    ) => value,
                    Unboxed::Character(value) => Value::U32(u32::from(value)),
                    Unboxed::Position(offset) => {
                        Value::I64(i64::try_from(offset).map_err(|_| HostError::Failed)?)
                    }
                    Unboxed::Value(_) => return Err(HostError::Failed),
                };
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
            CELL_NEW => {
                let [value] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.make_cell(value.to_value());
                Ok(0)
            }
            CELL_GET => {
                let [ValueRef::Obj(cell)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = self.read_cell(*cell)?;
                Ok(0)
            }
            CELL_SET => {
                let [ValueRef::Obj(cell), value] = args else {
                    return Err(HostError::Failed);
                };
                self.write_cell(*cell, value.to_value())?;
                Ok(0)
            }
            CHECK_POSITION => {
                let [ValueRef::I64(offset), ValueRef::U64(_)] = args else {
                    return Err(HostError::Failed);
                };
                if !is_position_offset(*offset) {
                    return Err(type_error::check_failed(&ctx));
                }
                rets[0] = Value::I64(*offset);
                Ok(0)
            }
            CHECK_CHARACTER => {
                let [ValueRef::U32(value), ValueRef::U64(_)] = args else {
                    return Err(HostError::Failed);
                };
                if char::from_u32(*value).is_none() {
                    return Err(type_error::check_failed(&ctx));
                }
                rets[0] = Value::U32(*value);
                Ok(0)
            }
            BOX_POSITION | BOX_REJECTED_POSITION => {
                let [ValueRef::I64(offset)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = match BoxedHandle::immediate_position(*offset) {
                    Some(handle) => Value::Obj(Obj {
                        host_type: INTERLOKO_VALUE_TYPE,
                        handle,
                    }),
                    None if symbol == BOX_REJECTED_POSITION => {
                        self.push_boxed_value(Value::I64(*offset))
                    }
                    None => return Err(HostError::Failed),
                };
                Ok(0)
            }
            BOX_CHARACTER | BOX_REJECTED_CHARACTER => {
                let [ValueRef::U32(value)] = args else {
                    return Err(HostError::Failed);
                };
                rets[0] = match char::from_u32(*value) {
                    Some(value) => Value::Obj(Obj {
                        host_type: INTERLOKO_VALUE_TYPE,
                        handle: BoxedHandle::immediate_character(value),
                    }),
                    None if symbol == BOX_REJECTED_CHARACTER => {
                        self.push_boxed_value(Value::I64(i64::from(*value)))
                    }
                    None => return Err(HostError::Failed),
                };
                Ok(0)
            }
            BOX_VALUE => {
                let value = match args {
                    [ValueRef::I64(value)] => Value::I64(*value),
                    [ValueRef::F64(value)] => Value::F64(*value),
                    [ValueRef::F32(value)] => Value::F32(*value),
                    [ValueRef::Bool(value)] => Value::Bool(*value),
                    [ValueRef::U64(value)] => Value::U64(*value),
                    // A callable value boxes as the `<function>` it is.
                    [ValueRef::Agg(callable)] => Value::Agg(*callable),
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
                // A stale or invalid handle fails, as `obj.key` traps on it,
                // rather than answering no.
                let actual = self.class_of_any(*value)?;
                rets[0] = Value::Bool(self.layout.is_same_or_subclass(actual, class));
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
                    .ok_or_else(|| type_error::check_failed(&ctx))?;
                rets[0] = Value::Obj(obj);
                Ok(0)
            }
            CAST_CLASS_OBJECT => {
                let [value, ValueRef::U64(bound_raw)] = args else {
                    return Err(HostError::Failed);
                };
                let bound = runtime_class_id(*bound_raw)?;
                let obj = self
                    .value_object(*value)
                    .filter(|obj| {
                        self.object(*obj).is_ok_and(|object| {
                            matches!(
                                object.storage,
                                RuntimeObjectStorage::ClassMeta { class }
                                    if self.layout.is_same_or_subclass(class, bound)
                            )
                        })
                    })
                    .ok_or_else(|| type_error::check_failed(&ctx))?;
                rets[0] = Value::Obj(obj);
                Ok(0)
            }
            _ => Err(HostError::UnknownSymbol),
        }
    }
}

/// Set in the key of a boxed `<object>` value that holds no runtime object,
/// such as a boxed `<integer>`, above its class id.
///
/// [`InterlokoHost::object_key`] keys an object by its class, so guests test
/// classes with `obj.key`; the bit also tells a boxed scalar from a boxed
/// object, which `obj.convert` can unbox, without a host call.
pub const SCALAR_KEY_BIT: u64 = 1 << 63;

impl InterlokoHost {
    /// The value slot `field` of a native runtime object holds, which `obj.field` reads
    /// without a host call: `None` for any other object, a slot it lacks, or a value that is
    /// not inline (a callable's), which compiled code reads through `slot_get` instead.
    #[must_use]
    pub fn object_field(&self, object: Obj, field: u32) -> Option<Value> {
        if object.host_type != INTERLOKO_OBJECT_TYPE {
            return None;
        }
        let RuntimeObjectStorage::Native { slots } = &self.object(object).ok()?.storage else {
            return None;
        };
        slots
            .get(usize::try_from(field).ok()?)
            .filter(|value| value.value_type().is_inline())
            .cloned()
    }

    /// Stores `value` in slot `field` of a native runtime object for `obj.set_field`, as
    /// `slot_set` would: checked against the slot's type and logged like any store into the
    /// image during a turn. Returns whether it stored it.
    pub fn set_object_field(&mut self, object: Obj, field: u32, value: &Value) -> bool {
        if object.host_type != INTERLOKO_OBJECT_TYPE || !value.value_type().is_inline() {
            return false;
        }
        let value = match *value {
            Value::Bool(value) => ValueRef::Bool(value),
            Value::I64(value) => ValueRef::I64(value),
            Value::U64(value) => ValueRef::U64(value),
            Value::F64(value) => ValueRef::F64(value),
            Value::I32(value) => ValueRef::I32(value),
            Value::U32(value) => ValueRef::U32(value),
            Value::F32(value) => ValueRef::F32(value),
            Value::Obj(value) => ValueRef::Obj(value),
            _ => return false,
        };
        self.write_slot(object, SlotId(field), value).is_ok()
    }

    /// The key `obj.key` reads for `object`: its class id, with
    /// [`SCALAR_KEY_BIT`] set for a boxed value that holds no runtime
    /// object. `None` for a stale or invalid handle, which traps: a handle
    /// kept past its turn is a bug, and class reads and `instance?` tests
    /// fail on one rather than answer for an object that is gone.
    ///
    /// An object's class never changes, so neither does its key.
    #[must_use]
    pub fn object_key(&self, object: Obj) -> Option<u64> {
        match object.host_type {
            INTERLOKO_OBJECT_TYPE => Some(u64::from(self.object(object).ok()?.class.0)),
            INTERLOKO_VALUE_TYPE => match self.unbox(object).ok()? {
                Unboxed::Value(Value::Obj(boxed)) if boxed.host_type == INTERLOKO_OBJECT_TYPE => {
                    Some(u64::from(self.object(boxed).ok()?.class.0))
                }
                // Scalars, characters and string positions: whatever class
                // a send selects them by, marked as no runtime object.
                _ => {
                    let class = self.class_of_any(ValueRef::Obj(object)).ok()?;
                    Some(u64::from(class.0) | SCALAR_KEY_BIT)
                }
            },
            _ => None,
        }
    }

    /// `object` in the representation of host type `to`, for `obj.convert`:
    /// a boxed `<object>` value holding a runtime object unboxed to it, or a
    /// runtime object boxed. Both carry the object's handle, so neither
    /// allocates; `None` for any other conversion.
    #[must_use]
    pub fn convert_object(&self, object: Obj, to: HostTypeId) -> Option<Obj> {
        match (object.host_type, to) {
            (INTERLOKO_VALUE_TYPE, INTERLOKO_OBJECT_TYPE) => {
                self.value_object(ValueRef::Obj(object))
            }
            (INTERLOKO_OBJECT_TYPE, INTERLOKO_VALUE_TYPE) => {
                BoxedHandle::immediate_object(object.handle).map(|handle| Obj {
                    host_type: INTERLOKO_VALUE_TYPE,
                    handle,
                })
            }
            _ => None,
        }
    }

    fn builtin_class(&self, kind: BuiltinValueClass) -> Result<ClassId, HostError> {
        self.layout.class_id(kind.name()).ok_or(HostError::Failed)
    }

    /// The `marker` object, made in the image on first use.
    fn marker(&mut self, marker: Marker) -> Obj {
        if let Some(object) = self.markers[marker as usize] {
            return object;
        }
        let class = marker
            .class()
            .and_then(|kind| self.layout.class_id(kind.name()))
            .unwrap_or(OBJECT_CLASS_ID);
        let Value::Obj(object) = self.push_object_in(
            Space::Image,
            RuntimeObject {
                class,
                storage: RuntimeObjectStorage::Marker(marker),
            },
        ) else {
            unreachable!("pushing an object yields an object");
        };
        self.markers[marker as usize] = Some(object);
        object
    }

    /// Whether `value` is the `marker` object.
    fn is_marker(&self, value: ValueRef<'_>, marker: Marker) -> bool {
        self.markers[marker as usize].is_some_and(|object| self.value_object(value) == Some(object))
    }

    /// How many markers have been made.
    pub(crate) fn markers_made(&self) -> usize {
        self.markers.iter().flatten().count()
    }

    fn intern_symbol(&mut self, name: String) -> Result<Value, HostError> {
        if let Some(entry) = self.symbols.iter().find(|entry| entry.name == name) {
            return Ok(Value::Obj(entry.object));
        }
        let class = self.builtin_class(BuiltinValueClass::Symbol)?;
        // Symbols are interned for the host's lifetime, so they live in the
        // image even when a turn interns them, and an aborted turn keeps
        // them: dropping one would let the image reuse its index.
        let value = self.push_object_in(
            Space::Image,
            RuntimeObject {
                class,
                storage: RuntimeObjectStorage::Symbol { name: name.clone() },
            },
        );
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
            storage: RuntimeObjectStorage::String {
                text: StringText::new(value).ok_or(HostError::Failed)?,
            },
        }))
    }

    fn push_vector(&mut self, elements: Vec<Value>) -> Result<Value, HostError> {
        let class = self.builtin_class(BuiltinValueClass::Vector)?;
        Ok(self.push_object(RuntimeObject {
            class,
            storage: RuntimeObjectStorage::Vector {
                elements,
                constant: true,
            },
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
                    Value::Obj(boxed) if boxed.host_type == INTERLOKO_OBJECT_TYPE => boxed,
                    _ => return Err(HostError::Failed),
                }
            }
            _ => return Err(HostError::Failed),
        };
        Ok(self.object(obj)?)
    }

    pub(crate) fn class_of_any(&self, value: ValueRef<'_>) -> Result<ClassId, HostError> {
        match value {
            ValueRef::I64(_) | ValueRef::I32(_) | ValueRef::U32(_) => {
                self.builtin_class(BuiltinValueClass::Integer)
            }
            ValueRef::F64(_) => self.builtin_class(BuiltinValueClass::DoubleFloat),
            ValueRef::F32(_) => self.builtin_class(BuiltinValueClass::SingleFloat),
            // A layout without `<boolean>` predates it; its booleans are
            // plain `<object>`s.
            ValueRef::Bool(_) => self
                .builtin_class(BuiltinValueClass::Boolean)
                .or(Ok(OBJECT_CLASS_ID)),
            ValueRef::U64(_) => Ok(OBJECT_CLASS_ID),
            // A callable value is a tuple of its entries and shape.
            ValueRef::Agg(_) => self.builtin_class(BuiltinValueClass::Function),
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => {
                Ok(self.object(obj)?.class)
            }
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                let value = match self.unbox(obj)? {
                    Unboxed::Value(value) => value,
                    Unboxed::Character(_) => {
                        return self.builtin_class(BuiltinValueClass::Character);
                    }
                    Unboxed::Position(_) => {
                        return self.builtin_class(BuiltinValueClass::StringPosition);
                    }
                };
                match value {
                    Value::I64(_) | Value::I32(_) | Value::U32(_) => {
                        self.builtin_class(BuiltinValueClass::Integer)
                    }
                    Value::F64(_) => self.builtin_class(BuiltinValueClass::DoubleFloat),
                    Value::F32(_) => self.builtin_class(BuiltinValueClass::SingleFloat),
                    Value::Bool(_) => self
                        .builtin_class(BuiltinValueClass::Boolean)
                        .or(Ok(OBJECT_CLASS_ID)),
                    Value::U64(_) => Ok(OBJECT_CLASS_ID),
                    Value::Agg(_) => self.builtin_class(BuiltinValueClass::Function),
                    Value::Obj(boxed) if boxed.host_type == INTERLOKO_OBJECT_TYPE => {
                        Ok(self.object(boxed)?.class)
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

    fn value_for_any(&self, value: ValueRef<'_>) -> Result<Value, HostError> {
        let ValueRef::Obj(obj) = value else {
            return Err(HostError::Failed);
        };
        let _ = self.unbox(obj)?;
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
                RuntimeObjectStorage::String { text: left },
                RuntimeObjectStorage::String { text: right },
            ) => left.as_str() == right.as_str(),
            (
                RuntimeObjectStorage::Vector { elements: left, .. },
                RuntimeObjectStorage::Vector {
                    elements: right, ..
                },
            ) => left == right,
            (left, right) => left.denotes_same(right),
        })
    }

    /// Whether two boxed values are identical, `==`.
    ///
    /// Heap objects are identical when they are the same object, and
    /// callables when they are the same function closed over the same
    /// environment ([`same_callable`]). Integers,
    /// floats and booleans have no identity apart from their value, so they
    /// are identical when they are the same class and value: integers by
    /// value, floats by class and IEEE 754 bit pattern (so a NaN is identical
    /// to itself, and `0.0` and `-0.0` are not identical). Whether a value is
    /// boxed in its handle or on the heap does not change the answer.
    fn identical(&self, lhs: Obj, rhs: Obj, ctx: &HostContext<'_, '_>) -> Result<bool, HostError> {
        if lhs == rhs {
            return Ok(true);
        }
        let (lhs, rhs) = match (self.unbox(lhs)?, self.unbox(rhs)?) {
            (Unboxed::Value(lhs), Unboxed::Value(rhs)) => (lhs, rhs),
            (Unboxed::Character(lhs), Unboxed::Character(rhs)) => return Ok(lhs == rhs),
            (Unboxed::Position(lhs), Unboxed::Position(rhs)) => return Ok(lhs == rhs),
            _ => return Ok(false),
        };
        Ok(match (lhs, rhs) {
            (Value::I64(left), Value::I64(right)) => left == right,
            (Value::F64(left), Value::F64(right)) => left.to_bits() == right.to_bits(),
            (Value::F32(left), Value::F32(right)) => left.to_bits() == right.to_bits(),
            (Value::Bool(left), Value::Bool(right)) => left == right,
            (Value::Agg(left), Value::Agg(right)) => same_callable(left, right, ctx)?,
            (Value::Obj(left), Value::Obj(right))
                if left.host_type == INTERLOKO_OBJECT_TYPE
                    && right.host_type == INTERLOKO_OBJECT_TYPE =>
            {
                left == right
                    || self
                        .object(left)?
                        .storage
                        .denotes_same(&self.object(right)?.storage)
            }
            _ => false,
        })
    }

    fn value_object(&self, value: ValueRef<'_>) -> Option<Obj> {
        match value {
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_OBJECT_TYPE => Some(obj),
            ValueRef::Obj(obj) if obj.host_type == INTERLOKO_VALUE_TYPE => {
                match self.boxed_value(obj).ok()? {
                    Value::Obj(boxed) if boxed.host_type == INTERLOKO_OBJECT_TYPE => Some(boxed),
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
                let _ = self.unbox(obj)?;
                Ok(Value::Obj(obj))
            }
            (ValueKind::I64, ValueRef::I64(value)) => Ok(Value::I64(value)),
            (ValueKind::F64, ValueRef::F64(value)) => Ok(Value::F64(value)),
            (ValueKind::I32, ValueRef::I32(value)) => Ok(Value::I32(value)),
            (ValueKind::U32, ValueRef::U32(value)) => Ok(Value::U32(value)),
            (ValueKind::F32, ValueRef::F32(value)) => Ok(Value::F32(value)),
            (ValueKind::Bool, ValueRef::Bool(value)) => Ok(Value::Bool(value)),
            // A `u32` register can hold any value; a character slot holds
            // only scalar values.
            (ValueKind::Character, ValueRef::U32(value)) if char::from_u32(value).is_some() => {
                Ok(Value::U32(value))
            }
            (ValueKind::Position, ValueRef::I64(offset)) => Ok(Value::I64(offset)),
            (ValueKind::Callable, ValueRef::Agg(callable)) => Ok(Value::Agg(callable)),
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
            // A class object slot holds a class object naming its bound or a
            // subclass of it.
            (ValueKind::ClassObject(expected), ValueRef::Obj(obj)) => {
                let object = self.object(obj)?;
                match &object.storage {
                    RuntimeObjectStorage::ClassMeta { class }
                        if self.layout.is_same_or_subclass(*class, expected) =>
                    {
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

/// Returns the `class_of` host signature for a boxed top `<object>` input.
///
/// [`CLASS_OF`] accepts runtime objects and boxed values alike; this is the
/// signature for the boxed form.
#[must_use]
pub fn class_of_any_signature() -> HostSig {
    HostSig {
        args: vec![any_type()],
        rets: vec![ValueType::U64],
    }
}

/// Returns the [`UNBOX_VALUE`] host signature producing `output`
/// (`ValueType::I64`, `ValueType::F64` or `ValueType::F32`).
#[must_use]
pub fn unbox_value_signature(output: ValueType) -> HostSig {
    HostSig {
        args: vec![any_type(), ValueType::U64],
        rets: vec![output],
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

/// Host signature for [`CELL_NEW`] with a value of type `value`.
#[must_use]
pub fn cell_new_signature(value: ValueType) -> HostSig {
    HostSig {
        args: vec![value],
        rets: vec![object_type()],
    }
}

/// Host signature for [`CELL_GET`] of a value of type `value`.
#[must_use]
pub fn cell_get_signature(value: ValueType) -> HostSig {
    HostSig {
        args: vec![object_type()],
        rets: vec![value],
    }
}

/// Host signature for [`CELL_SET`] of a value of type `value`.
#[must_use]
pub fn cell_set_signature(value: ValueType) -> HostSig {
    HostSig {
        args: vec![object_type(), value],
        rets: vec![],
    }
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

/// Host signature for [`IS_KEYWORD_ABSENT`].
#[must_use]
pub fn is_keyword_absent_signature() -> HostSig {
    HostSig {
        args: vec![any_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for [`UNSUPPLIED`] and [`KEYWORD_ABSENT`], which return
/// their marker boxed.
#[must_use]
pub fn unsupplied_signature() -> HostSig {
    HostSig {
        args: vec![],
        rets: vec![any_type()],
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

/// Host signature for identity, `==`: two boxed values in, a boolean out.
#[must_use]
pub fn identical_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), any_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Whether two callable values, tuples of a typed entry, a boxed entry and
/// a shape, are the same `<function>`.
///
/// A callable value is made afresh each time a function is named or a
/// closure built, so the tuple is not its identity: the typed entry's code
/// and environment are. Two values running the same code over the same
/// environment cannot be told apart, and neither can two values of the same
/// code with nothing captured, such as two mentions of one function.
fn same_callable(
    left: AggHandle,
    right: AggHandle,
    ctx: &HostContext<'_, '_>,
) -> Result<bool, HostError> {
    if left == right {
        return Ok(true);
    }
    let entry = |callable| match ctx.tuple_get(callable, 0) {
        Ok(Value::Closure(closure)) => Ok(closure),
        _ => Err(HostError::Failed),
    };
    let (left, right) = (entry(left)?, entry(right)?);
    if left.func != right.func {
        return Ok(false);
    }
    let captures = |env| ctx.tuple_len(env).map_err(|_| HostError::Failed);
    Ok(left.env == right.env || (captures(left.env)? == 0 && captures(right.env)? == 0))
}

/// Host signature for constructing a class metaobject from a class id.
#[must_use]
pub fn meta_class_object_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64],
        rets: vec![object_type()],
    }
}

/// Host signature for reading the class id a class metaobject names.
#[must_use]
pub fn meta_class_id_signature() -> HostSig {
    HostSig {
        args: vec![object_type()],
        rets: vec![ValueType::U64],
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

/// Host signature for `instance?` against a type object: the value and
/// the type, both boxed.
#[must_use]
pub fn instance_of_type_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), any_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for constructing a `<limited-integer>`: whether it has a
/// lower bound and the bound, the same for its upper bound, and the named
/// limited type it is written as, `0` for none or one plus its index in
/// [`LimitedInteger::ALL`].
#[must_use]
pub fn limited_integer_type_signature() -> HostSig {
    HostSig {
        args: vec![
            ValueType::Bool,
            ValueType::I64,
            ValueType::Bool,
            ValueType::I64,
            ValueType::U64,
        ],
        rets: vec![object_type()],
    }
}

/// Host signature for [`MAKE_VECTOR`]: `(class, size, fill) -> vector`.
#[must_use]
pub fn make_vector_signature() -> HostSig {
    HostSig {
        args: vec![ValueType::U64, ValueType::I64, any_type()],
        rets: vec![object_type()],
    }
}

/// Host signature for [`MAKE_TABLE`]: `() -> table`.
#[must_use]
pub fn make_table_signature() -> HostSig {
    HostSig {
        args: Vec::new(),
        rets: vec![object_type()],
    }
}

/// Host signature for [`COLLECTION_SIZE`]: `(collection) -> size`.
#[must_use]
pub fn collection_size_signature() -> HostSig {
    HostSig {
        args: vec![object_type()],
        rets: vec![ValueType::I64],
    }
}

/// Host signature for [`VECTOR_ELEMENT`]: `(vector, index) -> element`.
#[must_use]
pub fn vector_element_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), ValueType::I64],
        rets: vec![any_type()],
    }
}

/// Host signature for [`VECTOR_TAIL`]: `(vector, start) -> <vector>`.
#[must_use]
pub fn vector_tail_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), ValueType::I64],
        rets: vec![object_type()],
    }
}

/// Host signature for [`VECTOR_ELEMENT_OR`]: `(vector, index, default) ->
/// element`.
#[must_use]
pub fn vector_element_or_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), ValueType::I64, any_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for [`VECTOR_ELEMENT_SETTER`]: `(value, vector, index) ->
/// value`.
#[must_use]
pub fn vector_element_setter_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), object_type(), ValueType::I64],
        rets: vec![any_type()],
    }
}

/// Host signature for [`STRETCHY_ADD`]: `(vector, element) -> vector`.
#[must_use]
pub fn stretchy_add_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), any_type()],
        rets: vec![object_type()],
    }
}

/// Host signature for [`TABLE_ELEMENT`]: `(table, key) -> value`.
#[must_use]
pub fn table_element_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), any_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for [`TABLE_ELEMENT_OR`]: `(table, key, default) ->
/// value`.
#[must_use]
pub fn table_element_or_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), any_type(), any_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for [`TABLE_ELEMENT_SETTER`]: `(value, table, key) ->
/// value`.
#[must_use]
pub fn table_element_setter_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), object_type(), any_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for [`LIST_PAIR`]: `(head, tail) -> <pair>`.
#[must_use]
pub fn pair_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), any_type()],
        rets: vec![object_type()],
    }
}

/// Host signature for [`LIST_HEAD`] and [`LIST_TAIL`]: `(list) ->
/// <object>`.
#[must_use]
pub fn list_part_signature() -> HostSig {
    HostSig {
        args: vec![object_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for [`LIST_HEAD_SETTER`] and [`LIST_TAIL_SETTER`]:
/// `(value, pair) -> value`.
#[must_use]
pub fn pair_part_setter_signature() -> HostSig {
    HostSig {
        args: vec![any_type(), object_type()],
        rets: vec![any_type()],
    }
}

/// Host signature for [`EMPTY_LIST`]: `() -> <empty-list>`.
#[must_use]
pub fn empty_list_signature() -> HostSig {
    HostSig {
        args: vec![],
        rets: vec![object_type()],
    }
}

/// Host signature for [`VECTOR_CONSTANT`]: `(vector) -> <boolean>`.
#[must_use]
pub fn vector_constant_signature() -> HostSig {
    HostSig {
        args: vec![object_type()],
        rets: vec![ValueType::Bool],
    }
}

/// Host signature for [`TABLE_POSITION_KEY`] and [`TABLE_POSITION_VALUE`]:
/// `(table, position, version) -> <object>`.
#[must_use]
pub fn table_position_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), ValueType::I64, ValueType::I64],
        rets: vec![any_type()],
    }
}

/// Host signature for [`TABLE_NEXT_POSITION`]: `(table, from, version) ->
/// position`.
#[must_use]
pub fn table_next_position_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), ValueType::I64, ValueType::I64],
        rets: vec![ValueType::I64],
    }
}

/// Host signature for [`TABLE_KEY_EXISTS`] and [`TABLE_REMOVE_KEY`]:
/// `(table, key) -> <boolean>`.
#[must_use]
pub fn table_key_signature() -> HostSig {
    HostSig {
        args: vec![object_type(), any_type()],
        rets: vec![ValueType::Bool],
    }
}

fn value_type(kind: ValueKind) -> Option<ValueType> {
    match kind {
        ValueKind::Any => Some(any_type()),
        ValueKind::I64 => Some(ValueType::I64),
        ValueKind::F64 => Some(ValueType::F64),
        ValueKind::I32 => Some(ValueType::I32),
        ValueKind::U32 => Some(ValueType::U32),
        ValueKind::F32 => Some(ValueType::F32),
        ValueKind::Bool => Some(ValueType::Bool),
        ValueKind::Character => Some(ValueType::U32),
        ValueKind::Position => Some(ValueType::I64),
        ValueKind::String(_)
        | ValueKind::Symbol(_)
        | ValueKind::Vector(_)
        | ValueKind::Object(_)
        | ValueKind::ClassObject(_)
        | ValueKind::GenericFunctionObject(_)
        | ValueKind::MethodObject(_)
        | ValueKind::SlotDescriptorObject { .. } => Some(object_type()),
        ValueKind::Callable => Some(ValueType::Agg),
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
    use alloc::format;
    use execution_tape::asm::{Asm, FunctionSig, ProgramBuilder, Reg};
    use execution_tape::trace::TraceMask;
    use execution_tape::value::FuncRef;
    use execution_tape::vm::{Limits, Vm};

    #[test]
    fn float_functions_follow_their_special_cases() {
        assert!(FloatFunction::Log.apply(&[-1.0]).is_nan());
        assert_eq!(FloatFunction::Exp.apply(&[0.0]), 1.0);
        assert_eq!(FloatFunction::Cbrt.apply(&[-27.0]), -3.0);
        assert_eq!(FloatFunction::Pow.apply(&[2.0, 10.0]), 1024.0);
        assert_eq!(FloatFunction::Pow.apply(&[f64::NAN, 0.0]), 1.0);
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
            sealing: Sealing::Open,
            is_abstract: false,
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
            sealing: Sealing::Open,
            is_abstract: false,
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
                sealing: Sealing::Open,
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

    /// `<object>`, `<string>`, `<cell>` with one `<object>` slot, and
    /// `<symbol>`.
    fn cell_layout() -> RuntimeLayout {
        RuntimeLayout {
            classes: vec![
                class("<object>", None, Vec::new()),
                class("<string>", Some(OBJECT_CLASS_ID), Vec::new()),
                class(
                    "<cell>",
                    Some(OBJECT_CLASS_ID),
                    vec![slot("value", ValueKind::Any, ClassId(2), SlotId(0))],
                ),
                class("<symbol>", Some(OBJECT_CLASS_ID), Vec::new()),
            ],
            generics: Vec::new(),
            methods: Vec::new(),
        }
    }

    fn object_of(value: &Value) -> Obj {
        let Value::Obj(object) = value else {
            panic!("expected an object, got {value:?}");
        };
        *object
    }

    #[test]
    fn strings_lend_their_utf8_and_nothing_else_does() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        let string = object_of(&host.make_string("h\u{e9}llo").expect("string"));
        let boxed = object_of(&host.box_top_value(Value::Bool(false)).expect("box"));
        let cell = object_of(
            &host
                .make_object(ClassId(2), &[ValueRef::Obj(boxed)])
                .expect("cell"),
        );
        let bytes = |object| host.object_bytes(object, |bytes| bytes.map(<[u8]>::to_vec));
        assert_eq!(bytes(string), Some("h\u{e9}llo".as_bytes().to_vec()));
        // Compiled code reading any other object traps rather than reading it.
        assert_eq!(bytes(cell), None);
        assert_eq!(bytes(boxed), None);
    }

    #[test]
    fn an_abstract_class_is_never_allocated() {
        let mut layout = dispatch_layout();
        layout.classes[1].is_abstract = true;
        let mut host = InterlokoHost::new_checked(layout).expect("layout");
        assert_eq!(
            host.make_object(ClassId(1), &[ValueRef::I64(1)]),
            Err(RuntimeError::ClassIsAbstract { class: ClassId(1) })
        );
        assert!(
            host.make_object(ClassId(2), &[ValueRef::I64(1), ValueRef::I64(2)])
                .is_ok()
        );
    }

    #[test]
    fn turn_allocations_live_in_the_nursery() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        let boxed_none = host.box_top_value(Value::Bool(false)).expect("box");
        host.make_object(ClassId(2), &[ValueRef::Obj(object_of(&boxed_none))])
            .expect("image cell");
        let image = host.heap_stats();
        assert_eq!(image.objects, 1);

        host.begin_turn().expect("turn opens");
        assert_eq!(host.begin_turn(), Err(RuntimeError::TurnAlreadyOpen));
        let text = host.make_string("transient").expect("string");
        host.make_string("garbage").expect("string");
        assert_eq!(host.heap_stats().objects, 3);
        assert_eq!(host.string_text(&text), Ok("transient"));
        let stats = host.finish_turn(&[text], &mut []).expect("turn closes");
        assert_eq!(stats.allocated.objects, 2);
        // The result was reachable, but the caller did not keep it.
        assert_eq!(stats.reachable.objects, 1);
        assert_eq!(stats.promoted, HeapStats::default());
        assert_eq!(stats.image, image);
        assert_eq!(host.heap_stats(), image);
        assert_eq!(
            host.finish_turn(&[], &mut []),
            Err(RuntimeError::NoOpenTurn)
        );
    }

    #[test]
    fn finished_turns_evacuate_survivors_in_nursery_order() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        let empty = host.box_top_value(Value::Bool(false)).expect("box");
        let cell = object_of(
            &host
                .make_object(ClassId(2), &[ValueRef::Obj(object_of(&empty))])
                .expect("image cell"),
        );

        host.begin_turn().expect("turn opens");
        host.make_string("garbage").expect("string");
        let stored = host.make_string("stored").expect("string");
        let kept = host.make_string("kept").expect("string");
        let rooted = host.make_string("rooted").expect("string");
        let wide = host.box_top_value(Value::I64(i64::MAX)).expect("heap box");
        host.make_string("more garbage").expect("string");
        // An image slot now refers to the nursery; the write log remembers it.
        let boxed_stored = host.box_top_value(stored).expect("box");
        host.write_slot(cell, SlotId(0), ValueRef::Obj(object_of(&boxed_stored)))
            .expect("image store");
        let root = host.register_root(rooted);
        let wide_root = host.register_root(wide);
        let mut keep = [kept];
        let stats = host.finish_turn(&[], &mut keep).expect("turn finishes");

        assert_eq!(stats.allocated.objects, 5);
        assert_eq!(stats.promoted.objects, 3);
        assert_eq!(stats.promoted.boxed_values, 1);
        assert_eq!(stats.reachable, stats.promoted);
        assert_eq!(stats.logged_stores, 1);
        assert!(stats.evacuation_work > 0);
        assert_eq!(host.heap_stats().objects, 4);
        // Survivors keep nursery order: "stored", "kept", "rooted".
        let text = |host: &InterlokoHost, value: &Value| {
            String::from(host.string_text(value).expect("string"))
        };
        assert_eq!(text(&host, &keep[0]), "kept");
        assert_eq!(object_of(&keep[0]).handle.0, 3);
        let rooted = host.image_root(root).expect("root").clone();
        assert_eq!(text(&host, &rooted), "rooted");
        assert_eq!(object_of(&rooted).handle.0, 4);
        let stored = host.boxed_value(object_of(&host.read_slot(cell, SlotId(0)).expect("slot")));
        assert_eq!(text(&host, &stored.expect("unboxes")), "stored");
        let wide = host.image_root(wide_root).expect("root").clone();
        assert_eq!(host.boxed_value(object_of(&wide)), Ok(Value::I64(i64::MAX)));

        // A budget too small for the survivors leaves the turn open.
        host.set_evacuation_budget(1);
        host.begin_turn().expect("turn opens");
        let survivor = host.make_string("survivor").expect("string");
        host.register_root(survivor);
        let before = host.heap_stats();
        assert!(matches!(
            host.finish_turn(&[], &mut []),
            Err(RuntimeError::EvacuationBudgetExceeded { budget: 1, .. })
        ));
        assert!(host.turn_open());
        assert_eq!(host.heap_stats(), before);
        host.abort_turn().expect("turn aborts");
        assert_eq!(host.heap_stats().objects, 4);
        assert!(host.image_root(ImageRootId(2)).is_err());
    }

    #[test]
    fn cells_hold_any_value_and_aborted_turns_restore_them() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        // Made outside a turn, the cell is in the image.
        let cell = object_of(&host.make_cell(Value::I64(1)));
        assert_eq!(host.read_cell(cell), Ok(Value::I64(1)));
        host.begin_turn().expect("turn opens");
        host.write_cell(cell, Value::Bool(true)).expect("store");
        assert_eq!(host.read_cell(cell), Ok(Value::Bool(true)));
        let stats = host.abort_turn().expect("turn aborts");
        assert_eq!(stats.logged_stores, 1);
        assert_eq!(host.read_cell(cell), Ok(Value::I64(1)));
        // A value that is not a cell is refused.
        let boxed = object_of(&host.box_top_value(Value::I64(2)).expect("box"));
        assert!(host.read_cell(boxed).is_err());
    }

    #[test]
    fn aborted_turns_undo_image_stores() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        let original = host.box_top_value(Value::I64(1)).expect("box");
        let cell = object_of(
            &host
                .make_object(ClassId(2), &[ValueRef::Obj(object_of(&original))])
                .expect("image cell"),
        );
        let boxed = |host: &mut InterlokoHost, value| {
            object_of(&host.box_top_value(Value::I64(value)).expect("box"))
        };
        let before = host.heap_stats();

        host.begin_turn().expect("turn opens");
        for value in [5, 7] {
            let value = boxed(&mut host, value);
            host.write_slot(cell, SlotId(0), ValueRef::Obj(value))
                .expect("image store");
        }
        let stats = host.abort_turn().expect("turn aborts");
        assert_eq!(stats.logged_stores, 2);
        assert_eq!(host.read_slot(cell, SlotId(0)), Ok(original.clone()));
        assert_eq!(host.heap_stats(), before);

        host.begin_turn().expect("turn opens");
        let seven = boxed(&mut host, 7);
        host.write_slot(cell, SlotId(0), ValueRef::Obj(seven))
            .expect("image store");
        let stats = host.finish_turn(&[], &mut []).expect("turn finishes");
        assert_eq!(stats.logged_stores, 1);
        assert_eq!(host.read_slot(cell, SlotId(0)), Ok(Value::Obj(seven)));

        // Outside a turn, image stores are ordinary and unlogged.
        host.write_slot(cell, SlotId(0), ValueRef::Obj(object_of(&original)))
            .expect("setup store");
        assert_eq!(host.read_slot(cell, SlotId(0)), Ok(original));
    }

    /// The image's objects and heap boxes, rendered for exact comparison.
    fn heap_snapshot(host: &InterlokoHost) -> String {
        let objects = host
            .image
            .objects
            .iter()
            .map(|object| {
                let storage = match &object.storage {
                    RuntimeObjectStorage::Native { slots } => format!("native {slots:?}"),
                    RuntimeObjectStorage::String { text } => format!("string {:?}", text.as_str()),
                    RuntimeObjectStorage::Symbol { name } => format!("symbol {name:?}"),
                    RuntimeObjectStorage::Vector { elements, .. } => format!("vector {elements:?}"),
                    _ => String::from("other"),
                };
                format!("{:?} {storage}", object.class)
            })
            .collect::<Vec<_>>();
        format!("objects {objects:?}\nvalues {:?}", host.image.values)
    }

    /// Everything the image holds: its heap, roots and interned symbols.
    fn image_snapshot(host: &InterlokoHost) -> String {
        let symbols = host
            .symbols
            .iter()
            .map(|entry| (entry.name.as_str(), entry.object))
            .collect::<Vec<_>>();
        format!(
            "{}\nroots {:?}\nsymbols {symbols:?}",
            heap_snapshot(host),
            host.roots
        )
    }

    /// Builds the image every abort test starts from: a heap-boxed integer,
    /// a cell holding it, a rooted string and an interned symbol. Returns the
    /// host, the boxed integer, the cell and the root.
    fn abort_fixture() -> (InterlokoHost, Value, Obj, ImageRootId) {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        let wide = host.box_top_value(Value::I64(i64::MAX)).expect("image box");
        let cell = object_of(
            &host
                .make_object(ClassId(2), &[ValueRef::Obj(object_of(&wide))])
                .expect("image cell"),
        );
        let held = host.make_string("held").expect("string");
        let held_root = host.register_root(held);
        host.intern_symbol("setup".into()).expect("symbol");
        (host, wide, cell, held_root)
    }

    /// Returns the name of the symbol `value` refers to.
    fn symbol_name(host: &InterlokoHost, value: &Value) -> String {
        match &host
            .object(object_of(value))
            .expect("symbol resolves")
            .storage
        {
            RuntimeObjectStorage::Symbol { name } => name.clone(),
            _ => panic!("expected a symbol, got {value:?}"),
        }
    }

    #[test]
    fn aborted_turns_restore_everything_but_interned_symbols() {
        let (mut host, wide, cell, held_root) = abort_fixture();
        let before = host.heap_stats();

        host.begin_turn().expect("turn opens");
        // Point the image cell into the nursery, twice.
        let text = host.make_string("nursery").expect("string");
        let boxed_text = host.box_top_value(text).expect("box");
        host.write_slot(cell, SlotId(0), ValueRef::Obj(object_of(&boxed_text)))
            .expect("image store");
        let narrow = host.box_top_value(Value::I64(i64::MIN)).expect("box");
        host.write_slot(cell, SlotId(0), ValueRef::Obj(object_of(&narrow)))
            .expect("image store");
        // Release a root from before the turn and register a new one.
        host.release_root(held_root).expect("root releases");
        let rooted = host.make_string("rooted").expect("string");
        host.register_root(rooted);
        // Intern a new symbol and look up an existing one.
        let fresh = host.intern_symbol("fresh".into()).expect("symbol");
        host.intern_symbol("setup".into()).expect("symbol");
        let stats = host.abort_turn().expect("turn aborts");

        // The image is exactly that of a host which only interned the new
        // symbol: every store, root change and nursery entry is undone.
        let (mut reference, ..) = abort_fixture();
        let expected = reference.intern_symbol("fresh".into()).expect("symbol");
        assert_eq!(image_snapshot(&host), image_snapshot(&reference));
        assert_eq!(fresh, expected);
        assert_eq!(host.read_slot(cell, SlotId(0)), Ok(wide.clone()));
        assert_eq!(host.boxed_value(object_of(&wide)), Ok(Value::I64(i64::MAX)));
        let held = host.image_root(held_root).expect("root restored").clone();
        assert_eq!(host.string_text(&held), Ok("held"));

        // The report counts the kept symbol, and only it.
        assert_eq!(stats.logged_stores, 2);
        assert_eq!(stats.interned.objects, 1);
        assert_eq!(stats.interned.boxed_values, 0);
        assert_eq!(
            stats.interned.bytes,
            census::object_bytes(&host.image.objects[3])
        );
        assert_eq!(stats.promoted, HeapStats::default());
        assert_eq!(stats.image, host.image_stats());
        assert_eq!(
            host.heap_stats(),
            HeapStats {
                objects: before.objects + stats.interned.objects,
                boxed_values: before.boxed_values,
                bytes: before.bytes + stats.interned.bytes,
            }
        );
    }

    #[test]
    fn unsupplied_is_one_object_that_outlives_the_turn_that_made_it() {
        let (mut host, ..) = abort_fixture();
        let before = host.image_stats();
        host.begin_turn().expect("turn opens");
        let unsupplied = host.marker(Marker::Unsupplied);
        let symbol = host.intern_symbol("unsupplied".into()).expect("symbol");
        host.abort_turn().expect("turn aborts");
        // Made once, it stays however its turn ends, and nothing else,
        // not even the symbol of its name, denotes it.
        assert_eq!(host.marker(Marker::Unsupplied), unsupplied);
        assert_eq!(host.image_stats().objects, before.objects + 2);
        let Value::Obj(symbol) = symbol else {
            panic!("a symbol is an object");
        };
        assert_ne!(symbol, unsupplied);
        let storage = |obj| &host.object(obj).expect("object resolves").storage;
        assert!(!storage(unsupplied).denotes_same(storage(symbol)));
        host.begin_turn().expect("turn opens");
        assert_eq!(host.marker(Marker::Unsupplied), unsupplied);
        let stats = host.abort_turn().expect("turn aborts");
        assert_eq!(stats.interned, HeapStats::default());
    }

    #[test]
    fn symbols_interned_in_an_aborted_turn_keep_their_handles() {
        let (mut host, ..) = abort_fixture();
        host.begin_turn().expect("turn opens");
        host.make_string("garbage").expect("string");
        let kept = host.intern_symbol("kept".into()).expect("symbol");
        host.abort_turn().expect("turn aborts");
        assert_eq!(symbol_name(&host, &kept), "kept");

        // Re-interning the name finds the same symbol, outside a turn and
        // in a later one, and a turn that only looks it up interns nothing.
        assert_eq!(
            host.intern_symbol("kept".into()).expect("symbol"),
            kept.clone()
        );
        host.begin_turn().expect("turn opens");
        assert_eq!(
            host.intern_symbol("kept".into()).expect("symbol"),
            kept.clone()
        );
        let stats = host.abort_turn().expect("turn aborts");
        assert_eq!(stats.interned, HeapStats::default());

        // Later turns append to the image, by evacuation and by interning;
        // none of them reuses the kept symbol's index.
        let before = host.image_stats();
        host.begin_turn().expect("turn opens");
        let mut keep = [host.make_string("survivor").expect("string")];
        let other = host.intern_symbol("other".into()).expect("symbol");
        let stats = host.finish_turn(&[], &mut keep).expect("turn finishes");
        assert_eq!(stats.interned.objects, 1);
        assert_eq!(stats.promoted.objects, 1);
        assert_eq!(
            stats.image,
            HeapStats {
                objects: before.objects + 2,
                boxed_values: before.boxed_values,
                bytes: before.bytes + stats.interned.bytes + stats.promoted.bytes,
            }
        );
        let late = host.intern_symbol("late".into()).expect("symbol");
        for symbol in [&other, &late, &keep[0]] {
            assert_ne!(object_of(symbol).handle, object_of(&kept).handle);
        }
        assert_eq!(symbol_name(&host, &kept), "kept");
        assert_eq!(symbol_name(&host, &other), "other");
        assert_eq!(host.string_text(&keep[0]), Ok("survivor"));
        assert_eq!(host.intern_symbol("kept".into()).expect("symbol"), kept);
    }

    /// The result of resolving a stale nursery handle.
    fn stale<T>(object: Obj) -> Result<T, RuntimeError> {
        Err(RuntimeError::StaleNurseryHandle { object })
    }

    #[test]
    fn handles_kept_across_finish_turn_are_stale() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        host.begin_turn().expect("turn opens");
        let text = host.make_string("first").expect("string");
        let wide = object_of(&host.box_top_value(Value::I64(i64::MAX)).expect("heap box"));
        let boxed_text = object_of(&host.box_top_value(text.clone()).expect("box"));
        let cell = object_of(
            &host
                .make_object(ClassId(2), &[ValueRef::Obj(wide)])
                .expect("cell"),
        );
        host.finish_turn(&[], &mut []).expect("turn finishes");

        let check = |host: &mut InterlokoHost| {
            assert_eq!(host.string_text(&text), stale(object_of(&text)));
            assert_eq!(host.boxed_value(wide), stale(wide));
            assert_eq!(host.read_slot(cell, SlotId(0)), stale(cell));
            assert_eq!(
                host.write_slot(cell, SlotId(0), ValueRef::Obj(boxed_text)),
                stale(cell)
            );
            // An immediate box carries the object handle, which is stale too.
            let unboxed = host.boxed_value(boxed_text).expect("immediate box decodes");
            assert_eq!(host.string_text(&unboxed), stale(object_of(&text)));
        };
        // Outside a turn the nursery is empty...
        check(&mut host);
        // ...and a later turn's entries at the same indices are not named
        // by the old handles.
        host.begin_turn().expect("turn opens");
        let second = host.make_string("second").expect("string");
        let other_wide = object_of(&host.box_top_value(Value::I64(i64::MIN)).expect("heap box"));
        let other_cell = host
            .make_object(ClassId(2), &[ValueRef::Obj(other_wide)])
            .expect("cell");
        check(&mut host);
        assert_eq!(host.string_text(&second), Ok("second"));
        assert_eq!(host.boxed_value(other_wide), Ok(Value::I64(i64::MIN)));
        // Stale handles reach nothing, so they keep nothing alive, and a
        // later turn's evacuation leaves them as they are.
        let mut keep = [text.clone(), Value::Obj(wide), Value::Obj(cell)];
        assert_eq!(host.reachable_stats(&keep), HeapStats::default());
        host.register_root(Value::Obj(boxed_text));
        let stats = host
            .finish_turn(&[other_cell], &mut keep)
            .expect("turn finishes");
        assert_eq!(stats.promoted, HeapStats::default());
        assert_eq!(keep, [text.clone(), Value::Obj(wide), Value::Obj(cell)]);
        assert_eq!(host.image_stats(), HeapStats::default());
        check(&mut host);
    }

    #[test]
    fn handles_kept_across_an_aborted_turn_are_stale() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        host.begin_turn().expect("turn opens");
        let text = host.make_string("doomed").expect("string");
        let wide = object_of(&host.box_top_value(Value::I64(i64::MAX)).expect("heap box"));
        host.abort_turn().expect("turn aborts");
        assert_eq!(host.string_text(&text), stale(object_of(&text)));
        assert_eq!(host.boxed_value(wide), stale(wide));

        host.begin_turn().expect("turn opens");
        let live = host.make_string("live").expect("string");
        let live_wide = object_of(&host.box_top_value(Value::I64(i64::MIN)).expect("heap box"));
        assert_eq!(host.string_text(&text), stale(object_of(&text)));
        assert_eq!(host.boxed_value(wide), stale(wide));
        assert_eq!(host.string_text(&live), Ok("live"));
        assert_eq!(host.boxed_value(live_wide), Ok(Value::I64(i64::MIN)));
        host.abort_turn().expect("turn aborts");
    }

    #[test]
    fn survivors_resolve_through_stable_image_handles() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        host.begin_turn().expect("turn opens");
        host.make_string("garbage").expect("string");
        let mut keep = [
            host.make_string("kept").expect("string"),
            host.box_top_value(Value::I64(i64::MAX)).expect("heap box"),
        ];
        let nursery = keep.clone();
        host.finish_turn(&[], &mut keep).expect("turn finishes");
        assert_eq!(object_of(&keep[0]).handle, ObjHandle(1));
        // The nursery handles are stale; the image handles they became
        // resolve through any number of later turns, finished or aborted.
        assert_eq!(
            host.string_text(&nursery[0]),
            Err(RuntimeError::StaleNurseryHandle {
                object: object_of(&nursery[0])
            })
        );
        for round in 0..4 {
            host.begin_turn().expect("turn opens");
            host.make_string("churn").expect("string");
            host.box_top_value(Value::I64(i64::MIN)).expect("heap box");
            assert_eq!(host.string_text(&keep[0]), Ok("kept"));
            assert_eq!(
                host.boxed_value(object_of(&keep[1])),
                Ok(Value::I64(i64::MAX))
            );
            if round % 2 == 0 {
                host.finish_turn(&[], &mut []).expect("turn finishes");
            } else {
                host.abort_turn().expect("turn aborts");
            }
        }
        assert_eq!(object_of(&keep[0]).handle, ObjHandle(1));
        assert_eq!(host.string_text(&keep[0]), Ok("kept"));
    }

    #[test]
    fn nursery_serials_wrap_at_their_width() {
        let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
        // Start two entries short of the wrap, so this turn's entries
        // straddle it.
        let top = NurserySerials {
            objects: (1 << 44) - 2,
            values: (1 << 45) - 2,
        };
        host.nursery_serials = top;
        host.begin_turn().expect("turn opens");
        let texts = ["a", "b", "c", "d"].map(|text| host.make_string(text).expect("string"));
        let boxes = [1, 2, 3, 4]
            .map(|n| object_of(&host.box_top_value(Value::I64(-(1 << 60) - n)).expect("box")));
        // Handles past the wrap restart at serial zero.
        assert_eq!(object_of(&texts[2]).handle.0, region::NURSERY_OBJECT_BIT);
        for (value, text) in texts.iter().zip(["a", "b", "c", "d"]) {
            assert_eq!(host.string_text(value), Ok(text));
        }
        for (boxed, n) in boxes.iter().zip(1..) {
            assert_eq!(host.boxed_value(*boxed), Ok(Value::I64(-(1 << 60) - n)));
        }
        let mut keep = [texts[3].clone()];
        host.finish_turn(&[], &mut keep).expect("turn finishes");
        assert_eq!(
            host.nursery_serials,
            NurserySerials {
                objects: 2,
                values: 2
            }
        );
        assert_eq!(host.string_text(&keep[0]), Ok("d"));

        // After the wrap, handles from both sides of it are stale while
        // the next turn's entries resolve.
        host.begin_turn().expect("turn opens");
        let fresh = ["e", "f", "g", "h"].map(|text| host.make_string(text).expect("string"));
        for value in &texts {
            assert_eq!(
                host.string_text(value),
                Err(RuntimeError::StaleNurseryHandle {
                    object: object_of(value)
                })
            );
        }
        for boxed in boxes {
            assert_eq!(
                host.boxed_value(boxed),
                Err(RuntimeError::StaleNurseryHandle { object: boxed })
            );
        }
        assert_eq!(host.string_text(&fresh[0]), Ok("e"));
        host.finish_turn(&[], &mut []).expect("turn finishes");

        // The window is finite: once the serial counter has come all the
        // way round to a stale handle's serial, the handle names that
        // turn's entry. The guarantee is that this takes `2^44` nursery
        // objects; here the counter is moved there directly.
        host.nursery_serials = top;
        host.begin_turn().expect("turn opens");
        let reused = host.make_string("reused").expect("string");
        assert_eq!(reused, texts[0]);
        assert_eq!(host.string_text(&texts[0]), Ok("reused"));
        host.finish_turn(&[], &mut []).expect("turn finishes");
    }

    #[test]
    fn evacuation_is_independent_of_root_order() {
        let run = |reverse: bool| {
            let mut host = InterlokoHost::new_checked(cell_layout()).expect("layout");
            host.begin_turn().expect("turn opens");
            let a = host.make_string("a").expect("string");
            host.make_string("garbage").expect("string");
            let b = host.make_string("b").expect("string");
            let boxed_a = host.box_top_value(a.clone()).expect("box");
            let cell = host
                .make_object(ClassId(2), &[ValueRef::Obj(object_of(&boxed_a))])
                .expect("cell");
            let wide = host.box_top_value(Value::I64(i64::MAX)).expect("heap box");
            let (mut roots, mut keep) = (vec![b, wide], vec![cell, a]);
            if reverse {
                roots.reverse();
                keep.reverse();
            }
            let ids = roots
                .into_iter()
                .map(|root| host.register_root(root))
                .collect::<Vec<_>>();
            let stats = host.finish_turn(&[], &mut keep).expect("turn finishes");
            if reverse {
                keep.reverse();
            }
            let mut roots = ids
                .into_iter()
                .map(|id| host.image_root(id).expect("root").clone())
                .collect::<Vec<_>>();
            if reverse {
                roots.reverse();
            }
            (host, keep, roots, stats)
        };
        let (host, keep, roots, stats) = run(false);
        let (other, other_keep, other_roots, other_stats) = run(true);
        assert_eq!(heap_snapshot(&other), heap_snapshot(&host));
        assert_eq!(
            (other_keep, other_roots, other_stats),
            (keep.clone(), roots.clone(), stats)
        );

        // Survivors take nursery order, not traversal order: "a", "b", the
        // cell, then the wide box; "a" is reached twice but moved once.
        let handles = [&keep[1], &roots[0], &keep[0]].map(|value| object_of(value).handle.0);
        assert_eq!(handles, [1, 2, 3]);
        let slot = host
            .read_slot(object_of(&keep[0]), SlotId(0))
            .expect("slot");
        let a = host.boxed_value(object_of(&slot)).expect("unboxes");
        assert_eq!(a, keep[1]);
        assert_eq!(
            host.boxed_value(object_of(&roots[1])),
            Ok(Value::I64(i64::MAX))
        );
        // Every survivor was reachable, and every reachable entry was kept.
        assert_eq!(stats.reachable, stats.promoted);
        assert_eq!(stats.promoted.objects, 3);
        assert_eq!(stats.promoted.boxed_values, 1);
        assert_eq!(stats.image, host.heap_stats());
        assert_eq!(stats.promoted, host.image_stats());
        // Four roots, one per string, two for the cell (itself and its
        // slot), and two for the heap box (itself and its value).
        assert_eq!(stats.evacuation_work, 4 + 1 + 1 + 2 + 2);
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
        run_slot_get(host, object, slot, Some(access)).expect("slot-get program should run")
    }

    fn run_slot_get(
        host: InterlokoHost,
        object: Obj,
        slot: SlotId,
        access: Option<&mut dyn AccessSink>,
    ) -> Result<Vec<Value>, execution_tape::vm::TrapInfo> {
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
        asm.const_u64(Reg(2), u64::from(slot.0));
        asm.host_call(
            Reg::EFFECT,
            host_sig,
            Reg::EFFECT,
            &[Reg(1), Reg(2)],
            &[Reg(3)],
        );
        asm.ret(Reg::EFFECT, &[Reg(3)]);
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
        let unit = vm.install(program).expect("test program should install");
        let mut outputs = Vec::new();
        vm.run_into(
            FuncRef::new(unit, entry),
            &[Value::Obj(object)],
            &mut outputs,
            TraceMask::NONE,
            None,
            access,
        )?;
        Ok(outputs)
    }

    /// Runs `as(class, object)` and returns 1 when it passes, 0 when the
    /// program catches [`control::CHECK_FAILED_TAG`], which it imports when
    /// `catches` holds.
    fn run_checked_cast(
        host: InterlokoHost,
        object: Obj,
        class: ClassId,
        catches: bool,
    ) -> Result<Vec<Value>, execution_tape::vm::TrapInfo> {
        use execution_tape::asm::Catch;

        let mut builder = ProgramBuilder::new();
        let cast = builder.host_sig_for(CAST_OBJECT, cast_object_signature(object_type()));
        let tag = catches.then(|| {
            builder
                .import_tag(control::CHECK_FAILED_TAG, &[])
                .expect("tag import")
        });
        let mut asm = Asm::new();
        asm.set_register_types(alloc::vec![
            ValueType::Unit,
            object_type(),
            ValueType::U64,
            object_type(),
            ValueType::I64,
        ]);
        let failed = asm.label();
        asm.const_u64(Reg(2), u64::from(class.0));
        if let Some(tag) = tag {
            asm.begin_region(&[Catch::Tag(tag, failed)]);
        }
        asm.host_call(Reg::EFFECT, cast, Reg::EFFECT, &[Reg(1), Reg(2)], &[Reg(3)]);
        if tag.is_some() {
            asm.end_region().expect("region closes");
        }
        asm.const_i64(Reg(4), 1);
        asm.ret(Reg::EFFECT, &[Reg(4)]);
        asm.place(failed).expect("label");
        asm.const_i64(Reg(4), 0);
        asm.ret(Reg::EFFECT, &[Reg(4)]);
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
        vm.define_tag(control::CHECK_FAILED_TAG, &[])
            .expect("tag defines");
        let unit = vm.install(program).expect("test program should install");
        let mut outputs = Vec::new();
        vm.run_into(
            FuncRef::new(unit, entry),
            &[Value::Obj(object)],
            &mut outputs,
            TraceMask::NONE,
            None,
            None,
        )?;
        Ok(outputs)
    }

    #[test]
    fn a_failed_cast_raises_the_check_failed_tag_its_program_imports() {
        let square = |host: &mut InterlokoHost| {
            object_of(
                &host
                    .make_object(ClassId(3), &[ValueRef::I64(1), ValueRef::I64(2)])
                    .expect("square"),
            )
        };
        let mut host = InterlokoHost::new_checked(dispatch_layout()).expect("layout");
        let object = square(&mut host);
        assert_eq!(
            run_checked_cast(host, object, ClassId(1), true).expect("passes"),
            [Value::I64(1)]
        );
        let mut host = InterlokoHost::new_checked(dispatch_layout()).expect("layout");
        let object = square(&mut host);
        assert_eq!(
            run_checked_cast(host, object, ClassId(2), true).expect("caught"),
            [Value::I64(0)]
        );
        // A program that does not import the tag traps as before.
        let mut host = InterlokoHost::new_checked(dispatch_layout()).expect("layout");
        let object = square(&mut host);
        let trap = run_checked_cast(host, object, ClassId(2), false).expect_err("traps");
        assert!(
            matches!(
                &trap.trap,
                execution_tape::vm::Trap::HostCallFailed {
                    symbol,
                    error: HostError::Failed,
                    ..
                } if symbol.as_ref() == CAST_OBJECT
            ),
            "{trap:?}"
        );
    }

    #[test]
    fn stale_nursery_handles_trap_in_the_vm() {
        let mut host = InterlokoHost::new_checked(dispatch_layout()).expect("layout");
        let circle = |host: &mut InterlokoHost, radius| {
            object_of(
                &host
                    .make_object(ClassId(2), &[ValueRef::I64(1), ValueRef::I64(radius)])
                    .expect("circle"),
            )
        };
        host.begin_turn().expect("turn opens");
        let stale = circle(&mut host, 10);
        host.finish_turn(&[], &mut []).expect("turn finishes");
        // The next turn allocates a circle at the same nursery index.
        host.begin_turn().expect("turn opens");
        let live = circle(&mut host, 20);
        assert_ne!(live, stale);
        let trap = run_slot_get(host, stale, SlotId(1), None)
            .expect_err("a stale handle must not read the live circle");
        assert!(
            matches!(
                &trap.trap,
                execution_tape::vm::Trap::HostCallFailed {
                    symbol,
                    error: HostError::Failed,
                    ..
                } if symbol.as_ref() == SLOT_GET
            ),
            "{trap:?}"
        );
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
