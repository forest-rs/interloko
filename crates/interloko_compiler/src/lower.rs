//! Backend-neutral executable IR lowered from semantic Interloko IR.
//!
//! This module owns the register-shaped executable IR that backends consume. It
//! explicitly does not own syntax parsing, semantic name resolution, runtime
//! object storage, or `execution_tape` bytecode encoding.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::identity::DefinitionId;
use crate::runtime as runtime_bridge;
use crate::sem;
use crate::syntax::Span;
use interloko_runtime::RuntimeLayout;
use interloko_runtime::control::{RestartDescriptor, RestartParameter};

/// Durable function identifier within a lowered module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FunctionId(
    /// Zero-based lowered function index.
    pub u32,
);

/// Durable virtual register identifier within one lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Register(
    /// Register number. Register zero is reserved for the tape effect register.
    pub u32,
);

/// Durable label identifier within one lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LabelId(
    /// Zero-based label index.
    pub u32,
);

/// Durable callable signature identifier within a lowered module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct CallableSigId(
    /// Zero-based callable signature index.
    pub u32,
);

/// Lowered value category.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// Boxed top Interloko value corresponding to source `<object>`.
    Any,
    /// Signed integer value.
    I64,
    /// IEEE 754 binary64 float value.
    F64,
    /// Unsigned integer value used by runtime metadata.
    U64,
    /// Boolean value used by control-flow conditions.
    Bool,
    /// Internal direct function reference, optionally tied to a callable signature.
    Function(Option<CallableSigId>),
    /// First-class callable closure, optionally tied to a callable signature.
    Closure(Option<CallableSigId>),
    /// Execution aggregate value, used for closure environments.
    Aggregate,
    /// Caught exception reference, bound by a catch target and rethrown.
    Exn,
    /// Runtime object, optionally tied to a statically known nominal class.
    Object(Option<sem::ClassId>),
    /// Class metaobject, optionally tied to the class it represents.
    ClassObject(Option<sem::ClassId>),
    /// Generic-function metaobject, optionally tied to the generic it represents.
    GenericFunctionObject(Option<sem::GenericFunctionId>),
    /// Method metaobject, optionally tied to the method it represents.
    MethodObject(Option<sem::MethodId>),
    /// Slot descriptor metaobject.
    SlotDescriptorObject,
}

/// Typed call ABI for a first-class callable value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableSignature {
    /// Argument value categories in call order.
    pub params: Vec<ValueKind>,
    /// Return value categories in order.
    pub rets: Vec<ValueKind>,
}

/// A lowered module ready for backend emission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Runtime layout required by object host calls.
    pub runtime: RuntimeLayout,
    /// `handle` restart sites, indexed by [`Instr::PushHandler`] descriptors.
    pub restarts: Vec<RestartDescriptor>,
    /// Interned callable signatures used by first-class function values.
    pub call_sigs: Vec<CallableSignature>,
    /// Host-backed functions visible to emitted calls.
    pub host_functions: Vec<HostFunction>,
    /// Lowered functions in [`FunctionId`] order.
    pub functions: Vec<Function>,
    /// Lowered function id for `main`, if present.
    pub main: Option<FunctionId>,
}

/// A host-backed function signature lowered to backend value categories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFunction {
    /// Registry-local function id.
    pub id: sem::HostFunctionId,
    /// Loko-visible and tape host-symbol name.
    pub name: String,
    /// Parameter categories in call order.
    pub params: Vec<ValueKind>,
    /// Single result category.
    pub ret: ValueKind,
}

impl Module {
    /// Returns a stable, human-readable dump of the lowered module.
    #[must_use]
    pub fn dump(&self) -> String {
        format!("{self}")
    }

    /// Returns a lowered function by id.
    #[must_use]
    pub fn function(&self, id: FunctionId) -> Option<&Function> {
        self.functions.get(id.0 as usize)
    }

    /// Returns a callable signature by id.
    #[must_use]
    pub fn callable_signature(&self, id: CallableSigId) -> Option<&CallableSignature> {
        self.call_sigs.get(id.0 as usize)
    }
}

/// Source role for a lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FunctionKind {
    /// A source `define function`.
    Function(sem::FunctionId),
    /// A semantic method, either source-authored or compiler-synthesized.
    Method(sem::MethodId),
    /// A generated runtime dispatcher for a generic function.
    Dispatcher(sem::GenericFunctionId),
    /// A generated callable getter for a slot accessor.
    SlotAccessor {
        /// Stable accessor identity.
        accessor: DefinitionId,
        /// Receiver class accepted by this accessor function.
        class: sem::ClassId,
        /// Effective slot read by this accessor function.
        slot: sem::SlotId,
    },
    /// A generated closure body that adapts a known callable to the closure ABI.
    CallableThunk {
        /// Known callable wrapped by this thunk.
        callable: sem::KnownCallable,
    },
    /// A source anonymous function expression.
    Closure(sem::ClosureId),
    /// The closure body behind block exit procedures with one signature.
    ///
    /// Its environment holds the exit point's token. It checks the exit point
    /// is still live and throws the values to the block.
    ExitThunk {
        /// Exit procedure signature; parameters are the block's results.
        signature: CallableSigId,
    },
}

/// Runtime host operation emitted by lowered IR.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RuntimeOp {
    /// Builds a class metaobject from a class id.
    ClassMetaObject,
    /// Builds a generic-function metaobject from a generic id.
    GenericMetaObject,
    /// Builds a method metaobject from a method id.
    MethodMetaObject,
    /// Builds a slot descriptor metaobject from a class id and slot id.
    SlotDescriptorObject,
    /// Returns a value's runtime class metaobject as `<object>`.
    ObjectClass,
    /// Returns the name of a class metaobject.
    ClassName,
    /// Returns the direct superclass of a class metaobject, or `#f`.
    ClassDirectSuperclass,
    /// Returns a vector of direct slot descriptor metaobjects.
    ClassDirectSlots,
    /// Returns a vector of effective slot descriptor metaobjects.
    ClassEffectiveSlots,
    /// Returns a slot descriptor's source name.
    SlotName,
    /// Returns a slot descriptor's declared type text.
    SlotType,
    /// Returns a slot descriptor's construction init keyword text.
    SlotInitKeyword,
    /// Returns whether a slot descriptor names a writable slot.
    SlotMutable,
    /// Returns a slot descriptor's getter accessor name.
    SlotGetter,
    /// Returns a slot descriptor's setter generic metaobject, or `#f`.
    SlotSetter,
    /// Returns the name of a generic-function metaobject.
    GenericFunctionName,
    /// Returns a vector of method metaobjects.
    GenericFunctionMethods,
    /// Returns the generic-function metaobject implemented by a method.
    MethodGenericFunction,
    /// Returns a vector of class metaobjects for a method's specializers.
    MethodSpecializers,
    /// Returns vector length.
    VectorSize,
    /// Returns a vector element as `<object>`.
    VectorRef,
    /// Renders a float as literal text.
    FloatToString,
    /// Renders an integer in decimal.
    IntegerToString,
    /// Joins two strings into a new string.
    Concatenate,
}

/// Condition that compiled Loko code signals with an explicit tape `trap`.
///
/// The tape carries [`LokoTrap::code`] as its trap code; runtime reporting
/// recovers the condition with [`LokoTrap::from_code`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LokoTrap {
    /// A generic function call matched no applicable method.
    NoApplicableMethod,
    /// `as(<integer>, x)` received a float with a fractional part.
    InexactFloatToInteger,
    /// `<integer>` arithmetic overflowed the 64-bit fixnum range.
    IntegerOverflow,
}

impl LokoTrap {
    /// Every Loko trap in code order.
    pub const ALL: &'static [Self] = &[
        Self::NoApplicableMethod,
        Self::InexactFloatToInteger,
        Self::IntegerOverflow,
    ];

    /// Returns the tape trap code for this condition.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::NoApplicableMethod => 1,
            Self::InexactFloatToInteger => 2,
            Self::IntegerOverflow => 3,
        }
    }

    /// Returns the condition a tape trap code stands for, if any.
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::NoApplicableMethod),
            2 => Some(Self::InexactFloatToInteger),
            3 => Some(Self::IntegerOverflow),
            _ => None,
        }
    }

    /// Returns a short human-readable name for this condition.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::NoApplicableMethod => "no applicable method",
            Self::InexactFloatToInteger => "inexact float to integer conversion",
            Self::IntegerOverflow => "integer overflow",
        }
    }

    /// Returns the stable diagnostic code that reports this condition.
    #[must_use]
    pub const fn diagnostic_code(self) -> DiagnosticCode {
        match self {
            Self::IntegerOverflow => DiagnosticCode::RuntimeIntegerOverflow,
            Self::NoApplicableMethod | Self::InexactFloatToInteger => DiagnosticCode::RuntimeTrap,
        }
    }
}

/// Signed 64-bit integer arithmetic that reports overflow instead of wrapping.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum I64ArithOp {
    /// `a + b`.
    Add,
    /// `a - b`.
    Sub,
    /// `a * b`.
    Mul,
}

impl I64ArithOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Add => "i64.add_ovf",
            Self::Sub => "i64.sub_ovf",
            Self::Mul => "i64.mul_ovf",
        }
    }
}

/// Exception tag used by a language control transfer.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ControlTag {
    /// Transfer to the activation that established a selected handler.
    /// The payload is the binding token and the condition object.
    Restart,
    /// Transfer to a `block` through its exit procedure. The payload is the
    /// exit point's token, then the block's result values.
    Exit(CallableSigId),
}

/// One clause of a protected region.
///
/// Every clause binds the caught exception reference as its target's final
/// parameter, so the target can rethrow it unchanged.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Catch {
    /// Catches one control tag; the target receives its payload, then the
    /// exception.
    Tag {
        /// Caught tag.
        tag: ControlTag,
        /// Catch target.
        target: LabelId,
    },
    /// Catches every exception; the target receives only the exception.
    All {
        /// Catch target.
        target: LabelId,
    },
}

/// Two-operand IEEE 754 float operation emitted as one tape instruction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum F64BinaryOp {
    /// `a + b`.
    Add,
    /// `a - b`.
    Sub,
    /// `a * b`.
    Mul,
    /// `a / b`; division by zero yields an infinity or NaN, never a trap.
    Div,
    /// Truncated remainder with the sign of `a`.
    Rem,
    /// NaN-propagating minimum.
    Min,
    /// NaN-propagating maximum.
    Max,
}

impl F64BinaryOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Add => "f64.add",
            Self::Sub => "f64.sub",
            Self::Mul => "f64.mul",
            Self::Div => "f64.div",
            Self::Rem => "f64.rem",
            Self::Min => "f64.min",
            Self::Max => "f64.max",
        }
    }
}

/// One-operand IEEE 754 float operation emitted as one tape instruction.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum F64UnaryOp {
    /// Sign-bit negation.
    Neg,
    /// Sign-bit clear.
    Abs,
    /// Round toward negative infinity.
    Floor,
    /// Round toward positive infinity.
    Ceil,
    /// Round toward zero.
    Trunc,
    /// Round to nearest, ties to even.
    Nearest,
    /// Correctly rounded square root.
    Sqrt,
}

impl F64UnaryOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::Neg => "f64.neg",
            Self::Abs => "f64.abs",
            Self::Floor => "f64.floor",
            Self::Ceil => "f64.ceil",
            Self::Trunc => "f64.trunc",
            Self::Nearest => "f64.nearest",
            Self::Sqrt => "f64.sqrt",
        }
    }
}

/// A lowered function signature and body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Lowered function id.
    pub id: FunctionId,
    /// Human-readable function name.
    pub name: String,
    /// Source role for this lowered function.
    pub kind: FunctionKind,
    /// Function parameters in input-register order.
    pub params: Vec<Param>,
    /// Function return value kind.
    pub ret: ValueKind,
    /// Function return value kinds in order.
    pub rets: Vec<ValueKind>,
    /// Kinds of r1 onward, including every temporary; r0 is the backend effect token.
    pub register_kinds: Vec<ValueKind>,
    /// Source locals assigned to lowered registers.
    pub locals: Vec<Local>,
    /// Lowered instruction stream.
    pub instructions: Vec<Instr>,
    /// Source span for the function-like item.
    pub span: Span,
}

/// A lowered function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Param {
    /// Source local id when this parameter came from semantic IR.
    pub source: Option<sem::LocalId>,
    /// Parameter name.
    pub name: String,
    /// Parameter value category.
    pub kind: ValueKind,
    /// Input register.
    pub reg: Register,
}

/// A lowered local assignment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Local {
    /// Semantic local id.
    pub source: sem::LocalId,
    /// Source local name.
    pub name: String,
    /// Local value category.
    pub kind: ValueKind,
    /// Register holding the local value.
    pub reg: Register,
}

/// Lowered executable instruction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Instr {
    /// Label marking a branch target.
    Label {
        /// Label id.
        label: LabelId,
    },
    /// Signed integer constant.
    ConstI64 {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Float constant.
    ConstF64 {
        /// Destination register.
        dst: Register,
        /// Constant value, compared by bit pattern.
        value: sem::FloatBits,
        /// Source span.
        span: Span,
    },
    /// Boolean constant.
    ConstBool {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// Runtime string literal.
    StringLiteral {
        /// Destination register.
        dst: Register,
        /// Decoded string value.
        value: String,
        /// Built-in string class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime symbol literal.
    SymbolLiteral {
        /// Destination register.
        dst: Register,
        /// Symbol name.
        name: String,
        /// Built-in symbol class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Unsigned integer constant.
    ConstU64 {
        /// Destination register.
        dst: Register,
        /// Constant value.
        value: u64,
        /// Source span.
        span: Span,
    },
    /// Class object constant represented by a runtime class id.
    ConstClass {
        /// Destination register.
        dst: Register,
        /// Nominal class represented by this value.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Direct function constant.
    ConstFunction {
        /// Destination register.
        dst: Register,
        /// Source function represented by this value.
        function: FunctionId,
        /// Source span.
        span: Span,
    },
    /// Creates a first-class closure value from a function and captured environment.
    ClosureNew {
        /// Destination register.
        dst: Register,
        /// Closure body function.
        function: FunctionId,
        /// Captured value registers in environment order.
        captures: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Reads one value from the current closure environment.
    CaptureGet {
        /// Destination register.
        dst: Register,
        /// Closure environment aggregate register.
        env: Register,
        /// Capture index.
        index: usize,
        /// Captured value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Boxes a concrete value into the top `<object>` runtime value.
    BoxValue {
        /// Destination register.
        dst: Register,
        /// Source register.
        src: Register,
        /// Source value category.
        from: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Signed integer arithmetic writing the wrapped result and whether the
    /// exact result overflowed `i64`.
    I64Binary {
        /// Destination register for the wrapped result.
        dst: Register,
        /// Destination `bool` register set when the result overflowed.
        overflow: Register,
        /// Arithmetic operator.
        op: I64ArithOp,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 float arithmetic on two operands.
    F64Binary {
        /// Destination register.
        dst: Register,
        /// Binary operator.
        op: F64BinaryOp,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 float operation on one operand.
    F64Unary {
        /// Destination register.
        dst: Register,
        /// Unary operator.
        op: F64UnaryOp,
        /// Operand register.
        src: Register,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 float comparison; every comparison with NaN is false.
    F64Compare {
        /// Destination register.
        dst: Register,
        /// Comparison operator.
        op: sem::CompareOp,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Calls a runtime float library function.
    FloatCall {
        /// Destination register.
        dst: Register,
        /// Called function.
        function: sem::FloatFunction,
        /// `f64` argument registers.
        args: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 inequality; true when either operand is NaN.
    F64NotEqual {
        /// Destination register.
        dst: Register,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Converts an integer to the nearest float, ties to even.
    I64ToF64 {
        /// Destination register.
        dst: Register,
        /// Integer operand register.
        src: Register,
        /// Source span.
        span: Span,
    },
    /// Truncates a float toward zero to an integer; traps on NaN, infinities,
    /// and values outside the `i64` range.
    F64ToI64 {
        /// Destination register.
        dst: Register,
        /// Float operand register.
        src: Register,
        /// Source span.
        span: Span,
    },
    /// Signed integer comparison.
    I64Compare {
        /// Destination register.
        dst: Register,
        /// Comparison operator.
        op: sem::CompareOp,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Runtime value equality.
    ValueEqual {
        /// Destination register.
        dst: Register,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Operand value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Ordinary direct call to another lowered function.
    Call {
        /// Destination registers.
        dsts: Vec<Register>,
        /// Lowered callee function.
        callee: FunctionId,
        /// Argument registers.
        args: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Indirect call through a first-class callable value.
    CallIndirect {
        /// Destination registers.
        dsts: Vec<Register>,
        /// Callable value register.
        callee: Register,
        /// Argument registers.
        args: Vec<Register>,
        /// Argument value categories.
        arg_kinds: Vec<ValueKind>,
        /// Result value categories.
        rets: Vec<ValueKind>,
        /// Source span.
        span: Span,
    },
    /// Runtime object construction with fields already ordered by the class
    /// initialization plan.
    MakeObject {
        /// Destination register.
        dst: Register,
        /// Nominal class to instantiate.
        class: sem::ClassId,
        /// Field value registers in slot order.
        fields: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Runtime immutable vector construction.
    VectorLiteral {
        /// Destination register.
        dst: Register,
        /// Boxed element registers in source order.
        elements: Vec<Register>,
        /// Built-in vector class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime host call for kernel-provided object/model operations.
    RuntimeCall {
        /// Destination register.
        dst: Register,
        /// Runtime operation.
        op: RuntimeOp,
        /// Argument registers.
        args: Vec<Register>,
        /// Result value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Call through an embedding-provided host function.
    HostCall {
        /// Destination register.
        dst: Register,
        /// Registry-local host function.
        function: sem::HostFunctionId,
        /// Argument registers.
        args: Vec<Register>,
        /// Result value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime slot read.
    ReadSlot {
        /// Destination register.
        dst: Register,
        /// Receiver object register.
        receiver: Register,
        /// Statically known receiver class.
        class: sem::ClassId,
        /// Slot to read.
        slot: sem::SlotId,
        /// Slot value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime native slot write.
    WriteSlot {
        /// Destination register receiving the assigned value.
        dst: Register,
        /// Receiver object register.
        receiver: Register,
        /// Assigned value register.
        value: Register,
        /// Statically known receiver class.
        class: sem::ClassId,
        /// Slot to write.
        slot: sem::SlotId,
        /// Slot value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Reads an object's runtime class id.
    ClassOf {
        /// Destination register.
        dst: Register,
        /// Object register.
        object: Register,
        /// Input kind: a runtime object or a boxed top `<object>` value.
        input: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime checked unboxing of an integer or float from a boxed top
    /// `<object>` value; traps unless the value is an instance of `class`.
    UnboxValue {
        /// Destination register.
        dst: Register,
        /// Boxed value register.
        value: Register,
        /// Class the boxed value must belong to.
        class: sem::ClassId,
        /// Unboxed result kind, `i64` or `f64`.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime class membership test.
    InstanceOf {
        /// Destination register.
        dst: Register,
        /// Tested value register.
        value: Register,
        /// Tested value kind.
        input: ValueKind,
        /// Target class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime checked cast from top `<object>` to a nominal object.
    CastObject {
        /// Destination register.
        dst: Register,
        /// Cast value register.
        value: Register,
        /// Cast value kind.
        input: ValueKind,
        /// Target class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Unsigned integer equality.
    EqU64 {
        /// Destination register.
        dst: Register,
        /// Left operand register.
        lhs: Register,
        /// Right operand register.
        rhs: Register,
        /// Source span.
        span: Span,
    },
    /// Conditional branch.
    Branch {
        /// Boolean condition register.
        cond: Register,
        /// Target when `cond` is true.
        then_label: LabelId,
        /// Target when `cond` is false.
        else_label: LabelId,
        /// Source span.
        span: Span,
    },
    /// Unconditional jump.
    Jump {
        /// Target label.
        label: LabelId,
        /// Source span.
        span: Span,
    },
    /// Register-to-register move.
    Move {
        /// Destination register.
        dst: Register,
        /// Source register.
        src: Register,
        /// Source span.
        span: Span,
    },
    /// Function return.
    Return {
        /// Return value registers.
        values: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Runtime trap.
    Trap {
        /// Condition being signalled.
        trap: LokoTrap,
        /// Source span.
        span: Span,
    },
    /// Pushes a dynamic condition handler binding for `class` and writes its
    /// token. Selecting the handler raises [`ControlTag::Restart`] with that
    /// token.
    PushHandler {
        /// Destination `u64` token register.
        dst: Register,
        /// Index into [`Module::restarts`].
        descriptor: usize,
        /// Guard closure register, run on the signalling stack during search.
        guard: Option<Register>,
        /// Source span.
        span: Span,
    },
    /// Pops the innermost dynamic binding, which must be `token`.
    PopBinding {
        /// Token register returned by the matching push.
        token: Register,
        /// Source span.
        span: Span,
    },
    /// Searches live handlers for `condition` on the signalling stack.
    ///
    /// A selected handler's restart is raised at this instruction. The
    /// results are the values a resuming choice (such as a future
    /// `use-value`) supplies; the run fails as unhandled when nothing applies.
    Signal {
        /// Registers receiving resumption values.
        dsts: Vec<Register>,
        /// Condition object register.
        condition: Register,
        /// Source span.
        span: Span,
    },
    /// Opens a protected region nested in the innermost open one.
    EnterRegion {
        /// Clauses tried in order before the enclosing region's.
        catches: Vec<Catch>,
    },
    /// Closes the innermost open protected region.
    LeaveRegion,
    /// Catch target reached only through a protected region clause.
    CatchTarget {
        /// Target label.
        label: LabelId,
        /// Registers receiving the tag payload and the exception reference.
        params: Vec<Register>,
    },
    /// Pushes a block exit point binding and writes its token.
    PushExit {
        /// Destination `u64` token register.
        dst: Register,
        /// Source span.
        span: Span,
    },
    /// Fails the run as an expired exit unless the exit point `token` is live.
    CheckExit {
        /// Exit point token register.
        token: Register,
        /// Source span.
        span: Span,
    },
    /// Raises a control tag with a payload.
    Throw {
        /// Raised tag.
        tag: ControlTag,
        /// Payload registers in tag order.
        payload: Vec<Register>,
        /// Source span.
        span: Span,
    },
    /// Rethrows a caught exception with its original tag, payload and origin.
    Rethrow {
        /// Exception reference register.
        exception: Register,
        /// Source span.
        span: Span,
    },
}

/// Lowering error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LowerError {
    /// Earlier compiler diagnostics prevented lowering.
    Diagnostics {
        /// Diagnostics that must be reported to the caller.
        diagnostics: Vec<Diagnostic>,
    },
    /// Semantic IR contained an invalid placeholder.
    InvalidSemantic {
        /// Error details.
        message: String,
    },
}

impl LowerError {
    /// Stable error code for this lowering failure.
    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        match self {
            Self::Diagnostics { .. } => DiagnosticCode::LowerBlockedByDiagnostics,
            Self::InvalidSemantic { .. } => DiagnosticCode::LowerInvalidSemantic,
        }
    }
}

/// Lowers semantic IR to executable backend-neutral IR.
pub fn lower(module: &sem::Module) -> Result<Module, LowerError> {
    if !module.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics {
            diagnostics: module.diagnostics.clone(),
        });
    }

    Lowering::new(module)?.lower_module()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct LowerIds {
    functions: Vec<FunctionId>,
    function_sigs: Vec<CallableSigId>,
    function_closure_thunks: Vec<Option<FunctionId>>,
    methods: Vec<FunctionId>,
    generics: Vec<FunctionId>,
    generic_sigs: Vec<CallableSigId>,
    generic_closure_thunks: Vec<Option<FunctionId>>,
    accessors: Vec<AccessorFunction>,
    closures: Vec<FunctionId>,
    closure_sigs: Vec<CallableSigId>,
    exit_thunks: Vec<ExitThunk>,
    restarts: Vec<RestartDescriptor>,
    call_sigs: Vec<CallableSignature>,
}

/// One exit thunk function, shared by every block exit with its signature.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExitThunk {
    kinds: Vec<sem::ValueKind>,
    signature: CallableSigId,
    function: FunctionId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AccessorFunction {
    accessor: DefinitionId,
    class: sem::ClassId,
    slot: sem::SlotId,
    function: FunctionId,
    closure_thunk: FunctionId,
    sig: CallableSigId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AccessorTarget {
    accessor: DefinitionId,
    class: sem::ClassId,
    slot: sem::SlotId,
}

struct KnownCallableThunk<'a> {
    id: FunctionId,
    name: String,
    callable: sem::KnownCallable,
    target: FunctionId,
    call_params: &'a [ValueKind],
    rets: Vec<ValueKind>,
    span: Span,
}

#[derive(Default)]
struct CallableUses<'module> {
    functions: Vec<sem::FunctionId>,
    generics: Vec<sem::GenericFunctionId>,
    accessors: Vec<AccessorTarget>,
    exits: Vec<Vec<sem::ValueKind>>,
    restarts: Vec<RestartDescriptor>,
    /// Classes and locals of the module and of the body being walked, for
    /// naming restart sites.
    classes: &'module [sem::Class],
    locals: &'module [sem::Local],
}

struct Lowering<'module> {
    module: &'module sem::Module,
    ids: LowerIds,
}

impl<'module> Lowering<'module> {
    fn new(module: &'module sem::Module) -> Result<Self, LowerError> {
        let mut ids = LowerIds::default();
        let callable_uses = collect_known_callable_uses(module);
        let mut next = 0_u32;

        for function in &module.functions {
            ids.functions.push(FunctionId(next));
            let signature = callable_signature_from_sem(
                &mut ids.call_sigs,
                &function.param_kinds,
                &function.param_callable_sigs,
                &function.result_kinds,
                &function.result_callable_sigs,
            )?;
            let sig = intern_callable_signature(&mut ids.call_sigs, signature);
            ids.function_sigs.push(sig);
            next = next.saturating_add(1);
        }
        for _ in &module.methods {
            ids.methods.push(FunctionId(next));
            next = next.saturating_add(1);
        }
        for generic in &module.generics {
            ids.generics.push(FunctionId(next));
            let signature = generic_callable_signature(module, generic)?;
            let sig = intern_callable_signature(&mut ids.call_sigs, signature);
            ids.generic_sigs.push(sig);
            next = next.saturating_add(1);
        }
        for callable in callable_uses.accessors {
            let signature = accessor_callable_signature(module, callable)?;
            let sig = intern_callable_signature(&mut ids.call_sigs, signature);
            ids.accessors.push(AccessorFunction {
                accessor: callable.accessor,
                class: callable.class,
                slot: callable.slot,
                function: FunctionId(next),
                closure_thunk: FunctionId(u32::MAX),
                sig,
            });
            next = next.saturating_add(1);
        }
        ids.function_closure_thunks
            .resize(module.functions.len(), None);
        for function in callable_uses.functions {
            let slot = ids
                .function_closure_thunks
                .get_mut(function.0 as usize)
                .ok_or_else(|| invalid_semantic("function closure thunk id is out of range"))?;
            if slot.is_none() {
                *slot = Some(FunctionId(next));
                next = next.saturating_add(1);
            }
        }
        ids.generic_closure_thunks
            .resize(module.generics.len(), None);
        for generic in callable_uses.generics {
            let slot = ids
                .generic_closure_thunks
                .get_mut(generic.0 as usize)
                .ok_or_else(|| invalid_semantic("generic closure thunk id is out of range"))?;
            if slot.is_none() {
                *slot = Some(FunctionId(next));
                next = next.saturating_add(1);
            }
        }
        for accessor in &mut ids.accessors {
            accessor.closure_thunk = FunctionId(next);
            next = next.saturating_add(1);
        }
        for closure in &module.closures {
            ids.closures.push(FunctionId(next));
            let signature = callable_signature_from_sem(
                &mut ids.call_sigs,
                &closure.param_kinds,
                &closure.param_callable_sigs,
                &closure.result_kinds,
                &closure.result_callable_sigs,
            )?;
            let sig = intern_callable_signature(&mut ids.call_sigs, signature);
            ids.closure_sigs.push(sig);
            next = next.saturating_add(1);
        }
        for kinds in callable_uses.exits {
            let none = vec![None; kinds.len()];
            let signature =
                callable_signature_from_sem(&mut ids.call_sigs, &kinds, &none, &kinds, &none)?;
            let signature = intern_callable_signature(&mut ids.call_sigs, signature);
            ids.exit_thunks.push(ExitThunk {
                kinds,
                signature,
                function: FunctionId(next),
            });
            next = next.saturating_add(1);
        }

        ids.restarts = callable_uses.restarts;

        Ok(Self { module, ids })
    }

    fn lower_module(&self) -> Result<Module, LowerError> {
        let mut functions = Vec::with_capacity(
            self.module.functions.len()
                + self.module.methods.len()
                + self.module.generics.len()
                + self.ids.accessors.len()
                + self
                    .ids
                    .function_closure_thunks
                    .iter()
                    .filter(|id| id.is_some())
                    .count()
                + self
                    .ids
                    .generic_closure_thunks
                    .iter()
                    .filter(|id| id.is_some())
                    .count()
                + self.ids.accessors.len()
                + self.module.closures.len(),
        );
        let mut main = None;

        for function in &self.module.functions {
            if function.name == "main" {
                main = Some(self.lower_function_id(function.id)?);
            }
            functions.push(self.lower_function(function)?);
        }
        for method in &self.module.methods {
            functions.push(self.lower_method(method)?);
        }
        for generic in &self.module.generics {
            functions.push(self.lower_dispatcher(generic)?);
        }
        for accessor in &self.ids.accessors {
            functions.push(self.lower_accessor(*accessor)?);
        }
        for function in &self.module.functions {
            if self
                .ids
                .function_closure_thunks
                .get(function.id.0 as usize)
                .and_then(|id| *id)
                .is_some()
            {
                functions.push(self.lower_function_closure_thunk(function)?);
            }
        }
        for generic in &self.module.generics {
            if self
                .ids
                .generic_closure_thunks
                .get(generic.id.0 as usize)
                .and_then(|id| *id)
                .is_some()
            {
                functions.push(self.lower_generic_closure_thunk(generic)?);
            }
        }
        for accessor in &self.ids.accessors {
            functions.push(self.lower_accessor_closure_thunk(*accessor)?);
        }
        for closure in &self.module.closures {
            functions.push(self.lower_closure(closure)?);
        }
        for thunk in &self.ids.exit_thunks {
            functions.push(self.lower_exit_thunk(thunk)?);
        }

        let host_functions = self
            .module
            .host_functions
            .iter()
            .map(|function| {
                Ok(HostFunction {
                    id: function.id,
                    name: function.name.clone(),
                    params: self.value_kinds(&function.param_kinds)?,
                    ret: self.value_kind(function.result_kind)?,
                })
            })
            .collect::<Result<Vec<_>, LowerError>>()?;

        Ok(Module {
            runtime: runtime_bridge::runtime_layout_from_sem(self.module),
            restarts: self.ids.restarts.clone(),
            call_sigs: self.ids.call_sigs.clone(),
            host_functions,
            functions,
            main,
        })
    }

    /// Lowers the closure body shared by exit procedures of one signature:
    /// check the exit point is live, then throw the values to its block.
    fn lower_exit_thunk(&self, thunk: &ExitThunk) -> Result<Function, LowerError> {
        let kinds = self.value_kinds(&thunk.kinds)?;
        let env = input_register(0);
        let values = (0..kinds.len())
            .map(|index| input_register(index + 1))
            .collect::<Vec<_>>();
        let token = Register(next_after_inputs(kinds.len() + 1));
        let mut params = vec![Param {
            source: None,
            name: "$env".into(),
            kind: ValueKind::Aggregate,
            reg: env,
        }];
        params.extend(
            kinds
                .iter()
                .zip(&values)
                .enumerate()
                .map(|(index, (kind, reg))| Param {
                    source: None,
                    name: format!("value{index}"),
                    kind: *kind,
                    reg: *reg,
                }),
        );
        let mut register_kinds = vec![ValueKind::Aggregate];
        register_kinds.extend(kinds.iter().copied());
        register_kinds.push(ValueKind::U64);
        let mut payload = vec![token];
        payload.extend(values.iter().copied());
        let span = Span::new(0, 0);
        Ok(Function {
            id: thunk.function,
            name: format!("exit#{}", thunk.signature.0),
            kind: FunctionKind::ExitThunk {
                signature: thunk.signature,
            },
            params,
            ret: kinds.first().copied().unwrap_or(ValueKind::Any),
            rets: kinds,
            register_kinds,
            locals: Vec::new(),
            instructions: vec![
                Instr::CaptureGet {
                    dst: token,
                    env,
                    index: 0,
                    kind: ValueKind::U64,
                    span,
                },
                Instr::CheckExit { token, span },
                Instr::Throw {
                    tag: ControlTag::Exit(thunk.signature),
                    payload,
                    span,
                },
            ],
            span,
        })
    }

    fn lower_function(&self, function: &sem::Function) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_function(self.module, &self.ids, function)?;
        let rets = self.value_kinds(&function.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_block(&function.body, &rets)?;
        Ok(Function {
            id: self.lower_function_id(function.id)?,
            name: function.name.clone(),
            kind: FunctionKind::Function(function.id),
            register_kinds: body.register_kinds,
            params: body.params,
            ret,
            rets,
            locals: body.locals,
            instructions: body.instructions,
            span: function.span,
        })
    }

    fn lower_method(&self, method: &sem::Method) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_method(self.module, &self.ids, method)?;
        let rets = self.value_kinds(&method.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_block(&method.body, &rets)?;
        let generic = self
            .module
            .generics
            .get(method.generic.0 as usize)
            .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
        Ok(Function {
            id: self.lower_method_id(method.id)?,
            name: format!("{}#method{}", generic.name, method.id.0),
            kind: FunctionKind::Method(method.id),
            register_kinds: body.register_kinds,
            params: body.params,
            ret,
            rets,
            locals: body.locals,
            instructions: body.instructions,
            span: method.span,
        })
    }

    fn lower_dispatcher(&self, generic: &sem::GenericFunction) -> Result<Function, LowerError> {
        if generic.dispatch_param >= generic.arity {
            return Err(invalid_semantic(
                "generic dispatch parameter is out of range",
            ));
        }

        let mut body = BodyLowering::for_dispatcher(self.module, &self.ids, generic);
        let rets = self.value_kinds(&generic.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_dispatcher(generic)?;
        Ok(Function {
            id: self.lower_generic_id(generic.id)?,
            name: format!("{}#dispatch", generic.name),
            kind: FunctionKind::Dispatcher(generic.id),
            register_kinds: body.register_kinds,
            params: body.params,
            ret,
            rets,
            locals: body.locals,
            instructions: body.instructions,
            span: generic.span,
        })
    }

    fn lower_accessor(&self, accessor: AccessorFunction) -> Result<Function, LowerError> {
        let class = self
            .module
            .classes
            .get(accessor.class.0 as usize)
            .ok_or_else(|| invalid_semantic("accessor class id is out of range"))?;
        let slot = class
            .effective_slots
            .get(accessor.slot.0 as usize)
            .ok_or_else(|| invalid_semantic("accessor slot id is out of range"))?;
        let receiver = input_register(0);
        let result = input_register(1);
        let ret = self.value_kind(slot.kind)?;
        Ok(Function {
            id: accessor.function,
            name: format!("{}#accessor{}", slot.accessor.name, accessor.function.0),
            kind: FunctionKind::SlotAccessor {
                accessor: accessor.accessor,
                class: accessor.class,
                slot: accessor.slot,
            },
            register_kinds: vec![ValueKind::Object(Some(accessor.class)), ret],
            params: vec![Param {
                source: None,
                name: "receiver".into(),
                kind: ValueKind::Object(Some(accessor.class)),
                reg: receiver,
            }],
            ret,
            rets: vec![ret],
            locals: Vec::new(),
            instructions: vec![
                Instr::ReadSlot {
                    dst: result,
                    receiver,
                    class: accessor.class,
                    slot: accessor.slot,
                    kind: ret,
                    span: slot.span,
                },
                Instr::Return {
                    values: vec![result],
                    span: slot.span,
                },
            ],
            span: slot.span,
        })
    }

    fn lower_function_closure_thunk(
        &self,
        function: &sem::Function,
    ) -> Result<Function, LowerError> {
        let id = self
            .ids
            .function_closure_thunks
            .get(function.id.0 as usize)
            .copied()
            .flatten()
            .ok_or_else(|| invalid_semantic("function closure thunk id is out of range"))?;
        let target = self.lower_function_id(function.id)?;
        let params = function
            .param_kinds
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?;
        self.lower_known_callable_thunk(KnownCallableThunk {
            id,
            name: format!("{}#closure", function.name),
            callable: sem::KnownCallable::DirectFunction(function.id),
            target,
            call_params: &params,
            rets: self.value_kinds(&function.result_kinds)?,
            span: function.span,
        })
    }

    fn lower_generic_closure_thunk(
        &self,
        generic: &sem::GenericFunction,
    ) -> Result<Function, LowerError> {
        let id = self
            .ids
            .generic_closure_thunks
            .get(generic.id.0 as usize)
            .copied()
            .flatten()
            .ok_or_else(|| invalid_semantic("generic closure thunk id is out of range"))?;
        let target = self.lower_generic_id(generic.id)?;
        let params = (0..generic.arity)
            .map(|index| dispatcher_param_kind(self.module, generic, index))
            .collect::<Vec<_>>();
        self.lower_known_callable_thunk(KnownCallableThunk {
            id,
            name: format!("{}#closure", generic.name),
            callable: sem::KnownCallable::GenericFunction(generic.id),
            target,
            call_params: &params,
            rets: self.value_kinds(&generic.result_kinds)?,
            span: generic.span,
        })
    }

    fn lower_accessor_closure_thunk(
        &self,
        accessor: AccessorFunction,
    ) -> Result<Function, LowerError> {
        let class = self
            .module
            .classes
            .get(accessor.class.0 as usize)
            .ok_or_else(|| invalid_semantic("accessor class id is out of range"))?;
        let slot = class
            .effective_slots
            .get(accessor.slot.0 as usize)
            .ok_or_else(|| invalid_semantic("accessor slot id is out of range"))?;
        let params = [ValueKind::Object(Some(accessor.class))];
        self.lower_known_callable_thunk(KnownCallableThunk {
            id: accessor.closure_thunk,
            name: format!("{}#closure", slot.accessor.name),
            callable: sem::KnownCallable::SlotAccessor {
                accessor: accessor.accessor,
                class: accessor.class,
                slot: accessor.slot,
            },
            target: accessor.function,
            call_params: &params,
            rets: vec![self.value_kind(slot.kind)?],
            span: slot.span,
        })
    }

    fn lower_known_callable_thunk(
        &self,
        spec: KnownCallableThunk<'_>,
    ) -> Result<Function, LowerError> {
        let mut params = Vec::with_capacity(spec.call_params.len() + 1);
        params.push(Param {
            source: None,
            name: "$env".into(),
            kind: ValueKind::Aggregate,
            reg: input_register(0),
        });
        params.extend(
            spec.call_params
                .iter()
                .copied()
                .enumerate()
                .map(|(index, kind)| Param {
                    source: None,
                    name: format!("arg{index}"),
                    kind,
                    reg: input_register(index + 1),
                }),
        );
        let call_args = (0..spec.call_params.len())
            .map(|index| input_register(index + 1))
            .collect::<Vec<_>>();
        let results = spec
            .rets
            .iter()
            .enumerate()
            .map(|(index, _)| input_register(spec.call_params.len() + 1 + index))
            .collect::<Vec<_>>();
        let ret = spec.rets.first().copied().unwrap_or(ValueKind::Any);
        Ok(Function {
            id: spec.id,
            name: spec.name,
            kind: FunctionKind::CallableThunk {
                callable: spec.callable,
            },
            register_kinds: params
                .iter()
                .map(|param| param.kind)
                .chain(spec.rets.iter().copied())
                .collect(),
            params,
            ret,
            rets: spec.rets,
            locals: Vec::new(),
            instructions: vec![
                Instr::Call {
                    dsts: results.clone(),
                    callee: spec.target,
                    args: call_args,
                    span: spec.span,
                },
                Instr::Return {
                    values: results,
                    span: spec.span,
                },
            ],
            span: spec.span,
        })
    }

    fn lower_closure(&self, closure: &sem::Closure) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_closure(self.module, &self.ids, closure)?;
        let rets = self.value_kinds(&closure.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_block(&closure.body, &rets)?;
        Ok(Function {
            id: self.lower_closure_id(closure.id)?,
            name: format!("closure#{}", closure.id.0),
            kind: FunctionKind::Closure(closure.id),
            register_kinds: body.register_kinds,
            params: body.params,
            ret,
            rets,
            locals: body.locals,
            instructions: body.instructions,
            span: closure.span,
        })
    }

    fn lower_function_id(&self, id: sem::FunctionId) -> Result<FunctionId, LowerError> {
        self.ids
            .functions
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("function id is out of range"))
    }

    fn lower_method_id(&self, id: sem::MethodId) -> Result<FunctionId, LowerError> {
        self.ids
            .methods
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("method id is out of range"))
    }

    fn lower_generic_id(&self, id: sem::GenericFunctionId) -> Result<FunctionId, LowerError> {
        self.ids
            .generics
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("generic id is out of range"))
    }

    fn lower_closure_id(&self, id: sem::ClosureId) -> Result<FunctionId, LowerError> {
        self.ids
            .closures
            .get(id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("closure id is out of range"))
    }

    fn value_kind(&self, kind: sem::ValueKind) -> Result<ValueKind, LowerError> {
        match kind {
            sem::ValueKind::KnownCallable(callable) => Ok(ValueKind::Closure(Some(
                known_callable_sig(&self.ids, callable)?,
            ))),
            _ => value_kind(kind),
        }
    }

    fn value_kinds(&self, kinds: &[sem::ValueKind]) -> Result<Vec<ValueKind>, LowerError> {
        kinds
            .iter()
            .copied()
            .map(|kind| self.value_kind(kind))
            .collect()
    }
}

fn known_callable_sig(
    ids: &LowerIds,
    callable: sem::KnownCallable,
) -> Result<CallableSigId, LowerError> {
    match callable {
        sem::KnownCallable::DirectFunction(function) => ids
            .function_sigs
            .get(function.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("function signature id is out of range")),
        sem::KnownCallable::GenericFunction(generic) => ids
            .generic_sigs
            .get(generic.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("generic signature id is out of range")),
        sem::KnownCallable::SlotAccessor {
            accessor,
            class,
            slot,
        } => ids
            .accessors
            .iter()
            .find(|candidate| {
                candidate.accessor == accessor && candidate.class == class && candidate.slot == slot
            })
            .map(|accessor| accessor.sig)
            .ok_or_else(|| invalid_semantic("accessor function id is missing")),
    }
}

fn collect_known_callable_uses(module: &sem::Module) -> CallableUses<'_> {
    let mut uses = CallableUses {
        classes: &module.classes,
        ..CallableUses::default()
    };
    for constant in &module.constants {
        collect_known_callable_uses_in_expr(&constant.value, &mut uses);
    }
    for function in &module.functions {
        uses.locals = &function.locals;
        collect_known_callable_uses_in_block(&function.body, &mut uses);
    }
    for closure in &module.closures {
        uses.locals = &closure.locals;
        collect_known_callable_uses_in_block(&closure.body, &mut uses);
    }
    for method in &module.methods {
        uses.locals = &method.locals;
        collect_known_callable_uses_in_block(&method.body, &mut uses);
    }
    uses
}

/// Describes one `handle` site as a restart: named after its class, with the
/// condition as its parameter when the recovery binds it.
fn restart_descriptor(
    uses: &CallableUses<'_>,
    restart: &sem::Restart,
    span: Span,
) -> RestartDescriptor {
    let class_name = uses
        .classes
        .get(restart.specializer.0 as usize)
        .map_or("condition", |class| class.name.as_str());
    let parameters = restart
        .condition_local
        .and_then(|local| uses.locals.get(local.0 as usize))
        .map(|local| RestartParameter {
            name: local.name.clone(),
            class: restart.specializer,
        })
        .into_iter()
        .collect();
    RestartDescriptor {
        name: class_name
            .trim_start_matches('<')
            .trim_end_matches('>')
            .into(),
        class: restart.specializer,
        parameters,
        span: span.start..span.end,
    }
}

fn collect_known_callable_uses_in_block(block: &sem::Block, uses: &mut CallableUses) {
    for let_binding in &block.lets {
        collect_known_callable_uses_in_expr(&let_binding.value, uses);
    }
    collect_known_callable_uses_in_expr(&block.result, uses);
}

fn collect_known_callable_uses_in_expr(expr: &sem::Expr, uses: &mut CallableUses) {
    match expr {
        sem::Expr::KnownCallable { callable, .. } => {
            push_known_callable_use(uses, *callable);
        }
        sem::Expr::VectorLiteral { elements, .. }
        | sem::Expr::Values {
            values: elements, ..
        }
        | sem::Expr::IntrinsicCall { args: elements, .. }
        | sem::Expr::HostCall { args: elements, .. }
        | sem::Expr::DirectCall { args: elements, .. }
        | sem::Expr::GenericCall { args: elements, .. } => {
            for element in elements {
                collect_known_callable_uses_in_expr(element, uses);
            }
        }
        sem::Expr::Closure { captures, .. } => {
            for capture in captures {
                if let sem::ValueKind::KnownCallable(callable) = capture.kind {
                    push_known_callable_use(uses, callable);
                }
            }
        }
        sem::Expr::CallableCall { callable, args, .. } => {
            collect_known_callable_uses_in_expr(callable, uses);
            for arg in args {
                collect_known_callable_uses_in_expr(arg, uses);
            }
        }
        sem::Expr::Signal { condition, .. }
        | sem::Expr::ClassOf {
            object: condition, ..
        }
        | sem::Expr::InstanceOf {
            value: condition, ..
        }
        | sem::Expr::Cast {
            value: condition, ..
        } => {
            collect_known_callable_uses_in_expr(condition, uses);
        }
        sem::Expr::MakeObject { class, fields, .. } => {
            collect_known_callable_uses_in_expr(class, uses);
            for field in fields {
                collect_known_callable_uses_in_expr(&field.value, uses);
            }
        }
        sem::Expr::AccessorCall { receiver, .. } => {
            collect_known_callable_uses_in_expr(receiver, uses);
        }
        sem::Expr::SlotWrite {
            receiver, value, ..
        } => {
            collect_known_callable_uses_in_expr(receiver, uses);
            collect_known_callable_uses_in_expr(value, uses);
        }
        sem::Expr::Negate { value, .. } | sem::Expr::Convert { value, .. } => {
            collect_known_callable_uses_in_expr(value, uses);
        }
        sem::Expr::FloatCall { args, .. } => {
            for arg in args {
                collect_known_callable_uses_in_expr(arg, uses);
            }
        }
        sem::Expr::Binary { lhs, rhs, .. }
        | sem::Expr::Compare { lhs, rhs, .. }
        | sem::Expr::MixedCompare { lhs, rhs, .. }
        | sem::Expr::ValueEqual { lhs, rhs, .. } => {
            collect_known_callable_uses_in_expr(lhs, uses);
            collect_known_callable_uses_in_expr(rhs, uses);
        }
        sem::Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_known_callable_uses_in_expr(condition, uses);
            collect_known_callable_uses_in_expr(then_branch, uses);
            collect_known_callable_uses_in_expr(else_branch, uses);
        }
        sem::Expr::Handle {
            body,
            restart,
            span,
        } => {
            let descriptor = restart_descriptor(uses, restart, *span);
            if !uses.restarts.contains(&descriptor) {
                uses.restarts.push(descriptor);
            }
            collect_known_callable_uses_in_expr(body, uses);
            if let Some(guard) = &restart.guard {
                collect_known_callable_uses_in_expr(guard, uses);
            }
            collect_known_callable_uses_in_expr(&restart.body, uses);
        }
        sem::Expr::Block {
            exit,
            body,
            cleanup,
            ..
        } => {
            if let Some(exit) = exit
                && !uses.exits.contains(&exit.result_kinds)
            {
                uses.exits.push(exit.result_kinds.clone());
            }
            collect_known_callable_uses_in_block(body, uses);
            if let Some(cleanup) = cleanup {
                collect_known_callable_uses_in_block(cleanup, uses);
            }
        }
        sem::Expr::ConstI64 { .. }
        | sem::Expr::ConstF64 { .. }
        | sem::Expr::ConstBool { .. }
        | sem::Expr::StringLiteral { .. }
        | sem::Expr::SymbolLiteral { .. }
        | sem::Expr::Local { .. }
        | sem::Expr::Capture { .. }
        | sem::Expr::ConstantRef { .. }
        | sem::Expr::ClassObject { .. }
        | sem::Expr::GenericFunctionObject { .. }
        | sem::Expr::Error { .. } => {}
    }
}

fn push_known_callable_use(uses: &mut CallableUses, callable: sem::KnownCallable) {
    match callable {
        sem::KnownCallable::DirectFunction(function) => {
            push_unique(&mut uses.functions, function);
        }
        sem::KnownCallable::GenericFunction(generic) => {
            push_unique(&mut uses.generics, generic);
        }
        sem::KnownCallable::SlotAccessor {
            accessor,
            class,
            slot,
        } => {
            push_unique(
                &mut uses.accessors,
                AccessorTarget {
                    accessor,
                    class,
                    slot,
                },
            );
        }
    }
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn callable_signature_from_sem(
    signatures: &mut Vec<CallableSignature>,
    params: &[sem::ValueKind],
    param_callable_sigs: &[Option<sem::CallableSignature>],
    rets: &[sem::ValueKind],
    ret_callable_sigs: &[Option<sem::CallableSignature>],
) -> Result<CallableSignature, LowerError> {
    let params = params
        .iter()
        .copied()
        .enumerate()
        .map(|(index, kind)| {
            value_kind_with_sem_callable_signature(
                signatures,
                kind,
                param_callable_sigs
                    .get(index)
                    .and_then(|signature| signature.as_ref()),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rets = rets
        .iter()
        .copied()
        .enumerate()
        .map(|(index, kind)| {
            value_kind_with_sem_callable_signature(
                signatures,
                kind,
                ret_callable_sigs
                    .get(index)
                    .and_then(|signature| signature.as_ref()),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CallableSignature { params, rets })
}

fn value_kind_with_sem_callable_signature(
    signatures: &mut Vec<CallableSignature>,
    kind: sem::ValueKind,
    callable_signature: Option<&sem::CallableSignature>,
) -> Result<ValueKind, LowerError> {
    if let Some(signature) = callable_signature {
        let sig = intern_sem_callable_signature(signatures, signature)?;
        return Ok(ValueKind::Closure(Some(sig)));
    }
    value_kind(kind)
}

fn intern_sem_callable_signature(
    signatures: &mut Vec<CallableSignature>,
    signature: &sem::CallableSignature,
) -> Result<CallableSigId, LowerError> {
    let lowered = CallableSignature {
        params: signature
            .params
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?,
        rets: signature
            .results
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(intern_callable_signature(signatures, lowered))
}

fn sem_callable_signature_id(
    ids: &LowerIds,
    signature: &sem::CallableSignature,
) -> Result<CallableSigId, LowerError> {
    let lowered = CallableSignature {
        params: signature
            .params
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?,
        rets: signature
            .results
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?,
    };
    ids.call_sigs
        .iter()
        .position(|candidate| *candidate == lowered)
        .map(|index| CallableSigId(u32::try_from(index).unwrap_or(u32::MAX)))
        .ok_or_else(|| invalid_semantic("callable signature id is missing"))
}

fn accessor_callable_signature(
    module: &sem::Module,
    accessor: AccessorTarget,
) -> Result<CallableSignature, LowerError> {
    let class = module
        .classes
        .get(accessor.class.0 as usize)
        .ok_or_else(|| invalid_semantic("accessor class id is out of range"))?;
    let slot = class
        .effective_slots
        .get(accessor.slot.0 as usize)
        .ok_or_else(|| invalid_semantic("accessor slot id is out of range"))?;
    Ok(CallableSignature {
        params: vec![ValueKind::Object(Some(accessor.class))],
        rets: vec![value_kind(slot.kind)?],
    })
}

fn generic_callable_signature(
    module: &sem::Module,
    generic: &sem::GenericFunction,
) -> Result<CallableSignature, LowerError> {
    Ok(CallableSignature {
        params: (0..generic.arity)
            .map(|index| dispatcher_param_kind(module, generic, index))
            .collect(),
        rets: generic
            .result_kinds
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn intern_callable_signature(
    signatures: &mut Vec<CallableSignature>,
    signature: CallableSignature,
) -> CallableSigId {
    if let Some(index) = signatures
        .iter()
        .position(|candidate| *candidate == signature)
    {
        return CallableSigId(u32::try_from(index).unwrap_or(u32::MAX));
    }
    let id = CallableSigId(u32::try_from(signatures.len()).unwrap_or(u32::MAX));
    signatures.push(signature);
    id
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Value {
    reg: Register,
    kind: ValueKind,
    reachable: bool,
}

/// An exit point pushed by a `block` in the current body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct BoundExit {
    token: Value,
    signature: CallableSigId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MatchScratch {
    class_const: Register,
    cond: Register,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct MatchLabels {
    success: LabelId,
    failure: LabelId,
}

struct BodyLowering<'module> {
    module: &'module sem::Module,
    ids: &'module LowerIds,
    source_locals: &'module [sem::Local],
    captures: &'module [sem::Capture],
    env: Option<Value>,
    params: Vec<Param>,
    locals: Vec<Local>,
    instructions: Vec<Instr>,
    local_regs: Vec<Option<Value>>,
    active_constants: Vec<sem::ConstantId>,
    register_kinds: Vec<ValueKind>,
    next_reg: u32,
    next_label: u32,
}

impl<'module> BodyLowering<'module> {
    fn for_function(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        function: &'module sem::Function,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &function.locals);
        for (index, local_id) in function.params.iter().copied().enumerate() {
            let reg = input_register(index);
            this.bind_param(function.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = next_after_inputs(function.params.len());
        Ok(this)
    }

    fn for_method(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        method: &'module sem::Method,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &method.locals);
        for (index, local_id) in method.params.iter().copied().enumerate() {
            let reg = input_register(index);
            this.bind_param(method.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = next_after_inputs(method.params.len());
        Ok(this)
    }

    fn for_closure(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        closure: &'module sem::Closure,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &closure.locals);
        let env = Value {
            reg: input_register(0),
            kind: ValueKind::Aggregate,
            reachable: true,
        };
        this.register_kinds.push(ValueKind::Aggregate);
        this.env = Some(env);
        this.captures = &closure.captures;
        this.params.push(Param {
            source: None,
            name: "$env".into(),
            kind: ValueKind::Aggregate,
            reg: env.reg,
        });
        for (index, local_id) in closure.params.iter().copied().enumerate() {
            let reg = input_register(index + 1);
            this.bind_param(closure.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = next_after_inputs(closure.params.len() + 1);
        Ok(this)
    }

    fn for_dispatcher(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        generic: &'module sem::GenericFunction,
    ) -> Self {
        let params: Vec<Param> = generic
            .params
            .iter()
            .enumerate()
            .map(|(index, name)| Param {
                source: None,
                name: name.clone(),
                kind: dispatcher_param_kind(module, generic, index),
                reg: input_register(index),
            })
            .collect();
        Self {
            module,
            ids,
            source_locals: &[],
            captures: &[],
            env: None,
            register_kinds: params.iter().map(|param| param.kind).collect(),
            params,
            locals: Vec::new(),
            instructions: Vec::new(),
            local_regs: Vec::new(),
            active_constants: Vec::new(),
            next_reg: next_after_inputs(generic.arity),
            next_label: 0,
        }
    }

    fn new(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        source_locals: &'module [sem::Local],
    ) -> Self {
        Self {
            module,
            ids,
            source_locals,
            captures: &[],
            env: None,
            params: Vec::new(),
            locals: Vec::new(),
            instructions: Vec::new(),
            local_regs: vec![None; source_locals.len()],
            active_constants: Vec::new(),
            register_kinds: Vec::new(),
            next_reg: 1,
            next_label: 0,
        }
    }

    fn bind_param(
        &mut self,
        local: Option<&sem::Local>,
        local_id: sem::LocalId,
        reg: Register,
    ) -> Result<(), LowerError> {
        let Some(local) = local else {
            return Err(invalid_semantic("parameter local id is out of range"));
        };
        let kind =
            self.value_kind_with_callable_signature(local.kind, local.callable_signature.as_ref())?;
        let value = Value {
            reg,
            kind,
            reachable: true,
        };
        self.set_local(local_id, value)?;
        assert_eq!(self.register_kinds.len() + 1, reg.0 as usize);
        self.register_kinds.push(kind);
        self.params.push(Param {
            source: Some(local_id),
            name: local.name.clone(),
            kind,
            reg,
        });
        self.locals.push(Local {
            source: local_id,
            name: local.name.clone(),
            kind,
            reg,
        });
        Ok(())
    }

    fn lower_block(
        &mut self,
        block: &sem::Block,
        expected: &[ValueKind],
    ) -> Result<(), LowerError> {
        self.lower_lets(&block.lets)?;
        let results = self.lower_results_expr(&block.result)?;
        if results.len() != expected.len() {
            return Err(invalid_semantic(
                "return arity does not match function signature",
            ));
        }
        let mut values = Vec::with_capacity(results.len());
        for (result, expected) in results.into_iter().zip(expected.iter().copied()) {
            values.push(
                self.coerce_value(result, expected, block.result.span())?
                    .reg,
            );
        }
        self.instructions.push(Instr::Return {
            values,
            span: block.result.span(),
        });
        Ok(())
    }

    fn lower_lets(&mut self, lets: &[sem::Let]) -> Result<(), LowerError> {
        for let_binding in lets {
            if let [local] = let_binding.locals.as_slice() {
                let (local_name, local_kind) = {
                    let local_info = self
                        .current_local(*local)
                        .ok_or_else(|| invalid_semantic("local id is out of range"))?;
                    (local_info.name.clone(), local_info.kind)
                };
                let value = self.lower_expr(&let_binding.value)?;
                let callable_signature = self
                    .current_local(*local)
                    .and_then(|local| local.callable_signature.as_ref());
                let expected =
                    self.value_kind_with_callable_signature(local_kind, callable_signature)?;
                let value = self.coerce_value(value, expected, let_binding.span)?;
                self.set_local(*local, value)?;
                self.locals.push(Local {
                    source: *local,
                    name: local_name,
                    kind: value.kind,
                    reg: value.reg,
                });
                continue;
            }

            let values = self.lower_results_expr(&let_binding.value)?;
            for (local, value) in let_binding.locals.iter().copied().zip(values) {
                let (local_name, local_kind) = {
                    let local_info = self
                        .current_local(local)
                        .ok_or_else(|| invalid_semantic("local id is out of range"))?;
                    (local_info.name.clone(), local_info.kind)
                };
                let callable_signature = self
                    .current_local(local)
                    .and_then(|local| local.callable_signature.as_ref());
                let expected =
                    self.value_kind_with_callable_signature(local_kind, callable_signature)?;
                let value = self.coerce_value(value, expected, let_binding.span)?;
                self.set_local(local, value)?;
                self.locals.push(Local {
                    source: local,
                    name: local_name,
                    kind: value.kind,
                    reg: value.reg,
                });
            }
        }
        Ok(())
    }

    /// Lowers a `block` expression.
    ///
    /// Without an exit or cleanup the body is lowered inline. Otherwise the
    /// body runs in a region. An exit pushes an exit point, binds a closure
    /// over its token, and catches its signature's exit tag: a payload with
    /// this activation's token becomes the block's values, any other exit
    /// continues outward. A catch-all clause handles every other exception.
    /// Each abnormal path pops the exit point, runs the cleanup and only then
    /// continues; the normal exit does the same inline. Cleanup code is
    /// therefore emitted once per path, and an exception it raises replaces
    /// the one in flight.
    fn lower_block_expr_results(
        &mut self,
        exit: Option<&sem::BlockExit>,
        body: &sem::Block,
        cleanup: Option<&sem::Block>,
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        if exit.is_none() && cleanup.is_none() {
            self.lower_lets(&body.lets)?;
            return self.lower_results_expr(&body.result);
        }
        let exit = exit.map(|exit| self.bind_exit(exit, span)).transpose()?;
        let unwind_label = self.alloc_label();
        let exit_label = self.alloc_label();
        let join_label = self.alloc_label();
        let mut catches = Vec::with_capacity(2);
        if let Some(exit) = &exit {
            catches.push(Catch::Tag {
                tag: ControlTag::Exit(exit.signature),
                target: exit_label,
            });
        }
        catches.push(Catch::All {
            target: unwind_label,
        });
        self.instructions.push(Instr::EnterRegion { catches });
        self.lower_lets(&body.lets)?;
        let body_values = self.lower_results_expr(&body.result)?;
        self.instructions.push(Instr::LeaveRegion);
        let results = body_values
            .iter()
            .map(|value| self.alloc(value.kind))
            .collect::<Vec<_>>();

        let mut reachable = body_values.iter().all(|value| value.reachable);
        if reachable {
            reachable = self.leave_block(exit.as_ref(), cleanup, span)?;
        }
        if reachable {
            self.move_results(&results, &body_values, body.result.span());
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        let unwound = self.alloc(ValueKind::Exn);
        self.instructions.push(Instr::CatchTarget {
            label: unwind_label,
            params: vec![unwound.reg],
        });
        if self.leave_block(exit.as_ref(), cleanup, span)? {
            self.instructions.push(Instr::Rethrow {
                exception: unwound.reg,
                span,
            });
        }

        if let Some(exit) = &exit {
            let target = self.alloc(ValueKind::U64);
            let exited = results
                .iter()
                .map(|result| self.alloc(result.kind))
                .collect::<Vec<_>>();
            let thrown = self.alloc(ValueKind::Exn);
            let mut params = vec![target.reg];
            params.extend(exited.iter().map(|value| value.reg));
            params.push(thrown.reg);
            self.instructions.push(Instr::CatchTarget {
                label: exit_label,
                params,
            });
            if self.leave_block(Some(exit), cleanup, span)? {
                let mine = self.alloc(ValueKind::Bool);
                let mine_label = self.alloc_label();
                let onward_label = self.alloc_label();
                self.instructions.push(Instr::EqU64 {
                    dst: mine.reg,
                    lhs: target.reg,
                    rhs: exit.token.reg,
                    span,
                });
                self.instructions.push(Instr::Branch {
                    cond: mine.reg,
                    then_label: mine_label,
                    else_label: onward_label,
                    span,
                });
                self.instructions.push(Instr::Label {
                    label: onward_label,
                });
                self.instructions.push(Instr::Rethrow {
                    exception: thrown.reg,
                    span,
                });
                self.instructions.push(Instr::Label { label: mine_label });
                self.move_results(&results, &exited, span);
                self.instructions.push(Instr::Jump {
                    label: join_label,
                    span,
                });
                reachable = true;
            }
        }

        self.instructions.push(Instr::Label { label: join_label });
        Ok(results
            .into_iter()
            .map(|result| Value {
                reachable,
                ..result
            })
            .collect())
    }

    /// Pushes an exit point and binds the block's exit local to a closure
    /// over its token.
    fn bind_exit(&mut self, exit: &sem::BlockExit, span: Span) -> Result<BoundExit, LowerError> {
        let thunk = self
            .ids
            .exit_thunks
            .iter()
            .find(|thunk| thunk.kinds == exit.result_kinds)
            .ok_or_else(|| invalid_semantic("block exit signature has no thunk"))?;
        let (signature, function) = (thunk.signature, thunk.function);
        let token = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::PushExit {
            dst: token.reg,
            span,
        });
        let procedure = self.alloc(ValueKind::Closure(Some(signature)));
        self.instructions.push(Instr::ClosureNew {
            dst: procedure.reg,
            function,
            captures: vec![token.reg],
            span,
        });
        self.set_local(exit.local, procedure)?;
        let local = self
            .current_local(exit.local)
            .ok_or_else(|| invalid_semantic("block exit local id is out of range"))?;
        self.locals.push(Local {
            source: exit.local,
            name: local.name.clone(),
            kind: procedure.kind,
            reg: procedure.reg,
        });
        Ok(BoundExit { token, signature })
    }

    /// Ends a block's extent on one path: pops its exit point, then runs its
    /// cleanup. Returns whether control continues afterwards.
    fn leave_block(
        &mut self,
        exit: Option<&BoundExit>,
        cleanup: Option<&sem::Block>,
        span: Span,
    ) -> Result<bool, LowerError> {
        if let Some(exit) = exit {
            self.instructions.push(Instr::PopBinding {
                token: exit.token.reg,
                span,
            });
        }
        match cleanup {
            Some(cleanup) => self.lower_cleanup(cleanup),
            None => Ok(true),
        }
    }

    fn move_results(&mut self, results: &[Value], values: &[Value], span: Span) {
        for (result, value) in results.iter().zip(values) {
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: value.reg,
                span,
            });
        }
    }

    /// Lowers a cleanup body for effect, returning whether control can
    /// continue after it.
    fn lower_cleanup(&mut self, cleanup: &sem::Block) -> Result<bool, LowerError> {
        self.lower_lets(&cleanup.lets)?;
        let values = self.lower_results_expr(&cleanup.result)?;
        Ok(values.iter().all(|value| value.reachable))
    }

    fn lower_dispatcher(&mut self, generic: &sem::GenericFunction) -> Result<(), LowerError> {
        for method in self.sorted_methods(generic) {
            if method.specializers.len() != generic.arity {
                return Err(invalid_semantic("method specializer arity is invalid"));
            }
            let class_const = self.alloc(ValueKind::U64);
            let cond = self.alloc(ValueKind::Bool);
            let method_rets = self.value_kinds(&method.result_kinds)?;
            let method_results = method_rets
                .iter()
                .copied()
                .map(|kind| self.alloc(kind))
                .collect::<Vec<_>>();
            let matched = self.alloc_label();
            let unmatched = self.alloc_label();

            for (index, specializer) in method.specializers.iter().copied().enumerate() {
                let Some(specializer) = specializer else {
                    continue;
                };
                let next_check = self.alloc_label();
                self.emit_specializer_match(
                    generic,
                    index,
                    specializer,
                    MatchScratch {
                        class_const: class_const.reg,
                        cond: cond.reg,
                    },
                    MatchLabels {
                        success: next_check,
                        failure: unmatched,
                    },
                    method.span,
                )?;
                self.instructions.push(Instr::Label { label: next_check });
            }
            self.instructions.push(Instr::Jump {
                label: matched,
                span: method.span,
            });
            self.instructions.push(Instr::Label { label: matched });
            let mut call_args = Vec::with_capacity(method.params.len());
            for (index, local_id) in method.params.iter().copied().enumerate() {
                let local = method
                    .locals
                    .get(local_id.0 as usize)
                    .ok_or_else(|| invalid_semantic("method parameter local id is out of range"))?;
                let value = Value {
                    reg: input_register(index),
                    kind: dispatcher_param_kind(self.module, generic, index),
                    reachable: true,
                };
                let value = self.coerce_value(value, self.value_kind(local.kind)?, method.span)?;
                call_args.push(value.reg);
            }
            self.instructions.push(Instr::Call {
                dsts: method_results.iter().map(|result| result.reg).collect(),
                callee: self
                    .ids
                    .methods
                    .get(method.id.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("method id is out of range"))?,
                args: call_args,
                span: method.span,
            });
            let generic_rets = self.value_kinds(&generic.result_kinds)?;
            if method_results.len() != generic_rets.len() {
                return Err(invalid_semantic(
                    "method return arity does not match generic signature",
                ));
            }
            let mut results = Vec::with_capacity(method_results.len());
            for (method_result, generic_ret) in method_results
                .iter()
                .copied()
                .zip(generic_rets.iter().copied())
            {
                results.push(
                    self.coerce_value(method_result, generic_ret, method.span)?
                        .reg,
                );
            }
            self.instructions.push(Instr::Return {
                values: results,
                span: method.span,
            });
            self.instructions.push(Instr::Label { label: unmatched });
        }

        self.instructions.push(Instr::Trap {
            trap: LokoTrap::NoApplicableMethod,
            span: generic.span,
        });
        Ok(())
    }

    fn emit_specializer_match(
        &mut self,
        generic: &sem::GenericFunction,
        index: usize,
        specializer: sem::ClassId,
        scratch: MatchScratch,
        labels: MatchLabels,
        span: Span,
    ) -> Result<(), LowerError> {
        if specializer == sem::OBJECT_CLASS_ID {
            self.instructions.push(Instr::Jump {
                label: labels.success,
                span,
            });
            return Ok(());
        }

        if let Some(scalar @ (sem::ValueKind::I64 | sem::ValueKind::F64)) =
            generic.param_kinds.get(index).copied().flatten()
        {
            let label = if is_scalar_value_class(self.module, value_kind(scalar)?, specializer) {
                labels.success
            } else {
                labels.failure
            };
            self.instructions.push(Instr::Jump { label, span });
            return Ok(());
        }

        let applicable = self
            .module
            .classes
            .iter()
            .filter(|class| self.module.is_same_or_subclass(class.id, specializer))
            .map(|class| class.id)
            .collect::<Vec<_>>();
        if applicable.is_empty() {
            self.instructions.push(Instr::Jump {
                label: labels.failure,
                span,
            });
            return Ok(());
        }

        let class_reg = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::ClassOf {
            dst: class_reg.reg,
            object: input_register(index),
            input: dispatcher_param_kind(self.module, generic, index),
            span,
        });
        for class in applicable {
            let next = self.alloc_label();
            self.instructions.push(Instr::ConstU64 {
                dst: scratch.class_const,
                value: u64::from(class.0),
                span,
            });
            self.instructions.push(Instr::EqU64 {
                dst: scratch.cond,
                lhs: class_reg.reg,
                rhs: scratch.class_const,
                span,
            });
            self.instructions.push(Instr::Branch {
                cond: scratch.cond,
                then_label: labels.success,
                else_label: next,
                span,
            });
            self.instructions.push(Instr::Label { label: next });
        }
        self.instructions.push(Instr::Jump {
            label: labels.failure,
            span,
        });
        Ok(())
    }

    fn sorted_methods(&self, generic: &sem::GenericFunction) -> Vec<&'module sem::Method> {
        let mut methods: Vec<_> = self
            .module
            .methods
            .iter()
            .filter(|method| method.generic == generic.id)
            .collect();
        methods.sort_by(|left, right| {
            if self.module.method_more_specific(left, right) {
                core::cmp::Ordering::Less
            } else if self.module.method_more_specific(right, left) {
                core::cmp::Ordering::Greater
            } else {
                left.id.0.cmp(&right.id.0)
            }
        });
        methods
    }

    fn lower_expr(&mut self, expr: &sem::Expr) -> Result<Value, LowerError> {
        match expr {
            sem::Expr::ConstI64 { value, span } => {
                let result = self.alloc(ValueKind::I64);
                self.instructions.push(Instr::ConstI64 {
                    dst: result.reg,
                    value: *value,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::ConstF64 { value, span } => {
                let result = self.alloc(ValueKind::F64);
                self.instructions.push(Instr::ConstF64 {
                    dst: result.reg,
                    value: *value,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::ConstBool { value, span } => {
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::ConstBool {
                    dst: result.reg,
                    value: *value,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::StringLiteral { value, class, span } => {
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::StringLiteral {
                    dst: result.reg,
                    value: value.clone(),
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::SymbolLiteral { name, class, span } => {
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::SymbolLiteral {
                    dst: result.reg,
                    name: name.clone(),
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::VectorLiteral {
                elements,
                class,
                span,
            } => {
                let mut element_regs = Vec::with_capacity(elements.len());
                for element in elements {
                    let element_span = element.span();
                    let element = self.lower_expr(element)?;
                    let element = self.box_any(element, element_span)?;
                    element_regs.push(element.reg);
                }
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::VectorLiteral {
                    dst: result.reg,
                    elements: element_regs,
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Values { .. } => Err(invalid_semantic(
                "multiple values cannot be used in a scalar expression",
            )),
            sem::Expr::Local {
                local,
                result_kind,
                callable_signature,
                span,
            } => {
                let value = self
                    .local_regs
                    .get(local.0 as usize)
                    .and_then(|value| *value)
                    .ok_or_else(|| invalid_semantic("local was read before assignment"))?;
                let expected = self.value_kind_with_callable_signature(
                    *result_kind,
                    callable_signature.as_ref(),
                )?;
                self.coerce_value(value, expected, *span)
            }
            sem::Expr::Capture {
                capture,
                result_kind,
                callable_signature,
                span,
            } => {
                let env = self
                    .env
                    .ok_or_else(|| invalid_semantic("capture read outside closure body"))?;
                let capture_info = self
                    .captures
                    .get(capture.0 as usize)
                    .ok_or_else(|| invalid_semantic("capture id is out of range"))?;
                let kind = self.value_kind_with_callable_signature(
                    capture_info.kind,
                    capture_info.callable_signature.as_ref(),
                )?;
                let result = self.alloc(kind);
                self.instructions.push(Instr::CaptureGet {
                    dst: result.reg,
                    env: env.reg,
                    index: capture.0 as usize,
                    kind,
                    span: *span,
                });
                let expected = self.value_kind_with_callable_signature(
                    *result_kind,
                    callable_signature.as_ref(),
                )?;
                self.coerce_value(result, expected, *span)
            }
            sem::Expr::ConstantRef { constant, span, .. } => {
                if self
                    .active_constants
                    .iter()
                    .any(|active| active == constant)
                {
                    return Err(invalid_semantic("constant initializer cycle"));
                }
                let constant_info = self
                    .module
                    .constants
                    .get(constant.0 as usize)
                    .ok_or_else(|| invalid_semantic("constant id is out of range"))?;
                let expected = self.value_kind_with_callable_signature(
                    constant_info.result_kind,
                    constant_info.callable_signature.as_ref(),
                )?;
                let value = constant_info.value.clone();
                self.active_constants.push(*constant);
                let lowered = self.lower_expr(&value);
                self.active_constants.pop();
                self.coerce_value(lowered?, expected, *span)
            }
            sem::Expr::ClassObject { class, span } => self.lower_class_meta_object(*class, *span),
            sem::Expr::GenericFunctionObject { generic, span } => {
                self.lower_generic_meta_object(*generic, *span)
            }
            sem::Expr::KnownCallable { callable, span } => {
                self.lower_known_callable(*callable, *span)
            }
            sem::Expr::Closure {
                closure,
                captures,
                span,
                ..
            } => self.lower_closure_value(*closure, captures, *span),
            sem::Expr::IntrinsicCall {
                intrinsic,
                args,
                result_kind,
                span,
            } => self.lower_intrinsic_call(*intrinsic, args, *result_kind, *span),
            sem::Expr::HostCall {
                function,
                args,
                result_kind,
                span,
            } => {
                let function_info = self
                    .module
                    .host_functions
                    .get(function.0 as usize)
                    .ok_or_else(|| invalid_semantic("host function id is out of range"))?;
                let args = self.lower_args_with_expected(
                    args,
                    &function_info.param_kinds,
                    &vec![None; function_info.param_kinds.len()],
                )?;
                let kind = self.value_kind(*result_kind)?;
                let result = self.alloc(kind);
                self.instructions.push(Instr::HostCall {
                    dst: result.reg,
                    function: *function,
                    args,
                    kind,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::DirectCall {
                function,
                args,
                span,
            } => {
                let function_info = self
                    .module
                    .functions
                    .get(function.0 as usize)
                    .ok_or_else(|| invalid_semantic("function id is out of range"))?;
                let args = self.lower_args_with_expected(
                    args,
                    &function_info.param_kinds,
                    &function_info.param_callable_sigs,
                )?;
                if function_info.result_kinds.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value function call used in a scalar expression",
                    ));
                }
                let result = self.alloc(
                    self.value_kind_with_callable_signature(
                        function_info.result_kinds[0],
                        function_info
                            .result_callable_sigs
                            .first()
                            .and_then(|signature| signature.as_ref()),
                    )?,
                );
                self.instructions.push(Instr::Call {
                    dsts: vec![result.reg],
                    callee: self
                        .ids
                        .functions
                        .get(function.0 as usize)
                        .copied()
                        .ok_or_else(|| invalid_semantic("function id is out of range"))?,
                    args,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::GenericCall {
                generic,
                args,
                span,
            } => {
                let generic_info = self
                    .module
                    .generics
                    .get(generic.0 as usize)
                    .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
                let args = self.lower_generic_args(args, generic_info)?;
                if generic_info.result_kinds.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value generic call used in a scalar expression",
                    ));
                }
                let result = self.alloc(self.value_kind(generic_info.result_kinds[0])?);
                self.instructions.push(Instr::Call {
                    dsts: vec![result.reg],
                    callee: self
                        .ids
                        .generics
                        .get(generic.0 as usize)
                        .copied()
                        .ok_or_else(|| invalid_semantic("generic id is out of range"))?,
                    args,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::CallableCall {
                callable,
                args,
                arg_kinds,
                result_kinds,
                span,
                ..
            } => {
                if result_kinds.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value callable call used in a scalar expression",
                    ));
                }
                self.lower_callable_call(callable, args, arg_kinds, result_kinds[0], *span)
            }
            sem::Expr::Signal {
                condition,
                condition_class,
                result_kinds,
                span,
                ..
            } => self.lower_signal(condition, *condition_class, result_kinds, *span),
            sem::Expr::ClassOf {
                object,
                class,
                span,
            } => {
                let object = self.lower_expr(object)?;
                let result = self.alloc(ValueKind::ClassObject(Some(*class)));
                if matches!(object.kind, ValueKind::I64 | ValueKind::F64) {
                    self.emit_class_meta_object(result.reg, *class, *span);
                } else {
                    let class_id = self.alloc(ValueKind::U64);
                    self.instructions.push(Instr::ClassOf {
                        dst: class_id.reg,
                        object: object.reg,
                        input: object.kind,
                        span: *span,
                    });
                    self.instructions.push(Instr::RuntimeCall {
                        dst: result.reg,
                        op: RuntimeOp::ClassMetaObject,
                        args: vec![class_id.reg],
                        kind: result.kind,
                        span: *span,
                    });
                }
                Ok(result)
            }
            sem::Expr::InstanceOf { value, class, span } => {
                let value = self.lower_expr(value)?;
                if matches!(value.kind, ValueKind::I64 | ValueKind::F64) {
                    let result = self.alloc(ValueKind::Bool);
                    self.instructions.push(Instr::ConstBool {
                        dst: result.reg,
                        value: is_scalar_value_class(self.module, value.kind, *class),
                        span: *span,
                    });
                    return Ok(result);
                }
                if let ValueKind::Object(Some(actual)) = value.kind
                    && self.module.is_same_or_subclass(actual, *class)
                {
                    let result = self.alloc(ValueKind::Bool);
                    self.instructions.push(Instr::ConstBool {
                        dst: result.reg,
                        value: true,
                        span: *span,
                    });
                    return Ok(result);
                }
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::InstanceOf {
                    dst: result.reg,
                    value: value.reg,
                    input: value.kind,
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Cast { value, class, span } => {
                let value = self.lower_expr(value)?;
                if *class == sem::OBJECT_CLASS_ID {
                    return self.box_any(value, *span);
                }
                if is_scalar_value_class(self.module, value.kind, *class) {
                    return Ok(value);
                }
                if value.kind == ValueKind::Any
                    && let Some(kind) = scalar_kind_for_class(self.module, *class)
                {
                    return Ok(self.unbox(value, *class, kind, *span));
                }
                if let ValueKind::Object(Some(actual)) = value.kind
                    && self.module.is_same_or_subclass(actual, *class)
                {
                    return Ok(value);
                }
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::CastObject {
                    dst: result.reg,
                    value: value.reg,
                    input: value.kind,
                    class: *class,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::MakeObject {
                class,
                class_id,
                fields,
                span,
                ..
            } => {
                let _ = self.lower_expr(class)?;
                let class_info = self
                    .module
                    .classes
                    .get(class_id.0 as usize)
                    .ok_or_else(|| invalid_semantic("class id is out of range"))?;
                let mut field_regs = Vec::with_capacity(class_info.initialization_plan.steps.len());
                for step in &class_info.initialization_plan.steps {
                    let slot = class_info
                        .effective_slots
                        .get(step.slot.0 as usize)
                        .ok_or_else(|| invalid_semantic("initialization slot is out of range"))?;
                    let Some(field) = fields.iter().find(|field| field.slot == step.slot) else {
                        return Err(invalid_semantic("missing object field"));
                    };
                    let value = self.lower_expr(&field.value)?;
                    let value = if slot.kind == sem::ValueKind::Any {
                        self.box_any(value, field.span)?
                    } else {
                        value
                    };
                    field_regs.push(value.reg);
                }
                let result = self.alloc(ValueKind::Object(Some(*class_id)));
                self.instructions.push(Instr::MakeObject {
                    dst: result.reg,
                    class: *class_id,
                    fields: field_regs,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::AccessorCall {
                receiver,
                class,
                slot,
                result_kind,
                span,
                ..
            } => {
                let receiver = self.lower_expr(receiver)?;
                let kind = self.value_kind(*result_kind)?;
                let result = self.alloc(kind);
                self.instructions.push(Instr::ReadSlot {
                    dst: result.reg,
                    receiver: receiver.reg,
                    class: *class,
                    slot: *slot,
                    kind,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::SlotWrite {
                receiver,
                value,
                class,
                slot,
                result_kind,
                span,
                ..
            } => {
                let receiver = self.lower_expr(receiver)?;
                let value = self.lower_expr(value)?;
                let kind = self.value_kind(*result_kind)?;
                let value = self.coerce_value(value, kind, *span)?;
                let result = self.alloc(kind);
                self.instructions.push(Instr::WriteSlot {
                    dst: result.reg,
                    receiver: receiver.reg,
                    value: value.reg,
                    class: *class,
                    slot: *slot,
                    kind,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::Binary {
                op,
                kind,
                lhs,
                rhs,
                span,
            } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                match kind {
                    sem::NumericKind::Integer => {
                        let op = match op {
                            sem::BinaryOp::Add => I64ArithOp::Add,
                            sem::BinaryOp::Sub => I64ArithOp::Sub,
                            sem::BinaryOp::Mul => I64ArithOp::Mul,
                            sem::BinaryOp::Div => {
                                return Err(invalid_semantic("integer division has no lowering"));
                            }
                        };
                        Ok(self.lower_checked_integer_arith(op, lhs.reg, rhs.reg, *span))
                    }
                    sem::NumericKind::Float => {
                        let result = self.alloc(ValueKind::F64);
                        self.instructions.push(Instr::F64Binary {
                            dst: result.reg,
                            op: match op {
                                sem::BinaryOp::Add => F64BinaryOp::Add,
                                sem::BinaryOp::Sub => F64BinaryOp::Sub,
                                sem::BinaryOp::Mul => F64BinaryOp::Mul,
                                sem::BinaryOp::Div => F64BinaryOp::Div,
                            },
                            lhs: lhs.reg,
                            rhs: rhs.reg,
                            span: *span,
                        });
                        Ok(result)
                    }
                }
            }
            sem::Expr::FloatCall {
                builtin,
                args,
                span,
            } => self.lower_float_call(*builtin, args, *span),
            sem::Expr::Convert { to, value, span } => {
                let value = self.lower_expr(value)?;
                match to {
                    sem::NumericKind::Float => {
                        let result = self.alloc(ValueKind::F64);
                        self.instructions.push(Instr::I64ToF64 {
                            dst: result.reg,
                            src: value.reg,
                            span: *span,
                        });
                        Ok(result)
                    }
                    sem::NumericKind::Integer => self.lower_exact_float_to_integer(value, *span),
                }
            }
            sem::Expr::Negate { kind, value, span } => {
                let value = self.lower_expr(value)?;
                match kind {
                    sem::NumericKind::Integer => {
                        // `execution_tape` has no `i64.neg`; `0 - x` is exact
                        // except for `i64::MIN`, which overflows.
                        let zero = self.alloc(ValueKind::I64);
                        self.instructions.push(Instr::ConstI64 {
                            dst: zero.reg,
                            value: 0,
                            span: *span,
                        });
                        Ok(self.lower_checked_integer_arith(
                            I64ArithOp::Sub,
                            zero.reg,
                            value.reg,
                            *span,
                        ))
                    }
                    sem::NumericKind::Float => {
                        let result = self.alloc(ValueKind::F64);
                        self.instructions.push(Instr::F64Unary {
                            dst: result.reg,
                            op: F64UnaryOp::Neg,
                            src: value.reg,
                            span: *span,
                        });
                        Ok(result)
                    }
                }
            }
            sem::Expr::MixedCompare {
                op,
                lhs_kind,
                lhs,
                rhs,
                span,
            } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                // Operands are evaluated in source order; the comparison is
                // then phrased with the integer on the left.
                let (integer, float, op) = match lhs_kind {
                    sem::NumericKind::Integer => (lhs.reg, rhs.reg, *op),
                    sem::NumericKind::Float => (rhs.reg, lhs.reg, op.reversed()),
                };
                Ok(self.lower_exact_integer_float_compare(op, integer, float, *span))
            }
            sem::Expr::Compare {
                op,
                kind,
                lhs,
                rhs,
                span,
            } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(match kind {
                    sem::NumericKind::Integer => Instr::I64Compare {
                        dst: result.reg,
                        op: *op,
                        lhs: lhs.reg,
                        rhs: rhs.reg,
                        span: *span,
                    },
                    sem::NumericKind::Float => Instr::F64Compare {
                        dst: result.reg,
                        op: *op,
                        lhs: lhs.reg,
                        rhs: rhs.reg,
                        span: *span,
                    },
                });
                Ok(result)
            }
            sem::Expr::ValueEqual {
                lhs,
                rhs,
                kind,
                span,
            } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::ValueEqual {
                    dst: result.reg,
                    lhs: lhs.reg,
                    rhs: rhs.reg,
                    kind: self.value_kind(*kind)?,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::If {
                condition,
                refinement: _,
                then_branch,
                else_branch,
                span,
            } => self.lower_if(condition, then_branch, else_branch, *span),
            sem::Expr::Handle {
                body,
                restart,
                span,
            } => self.lower_handle(body, restart, *span),
            sem::Expr::Block {
                exit,
                body,
                cleanup,
                span,
            } => {
                let mut results =
                    self.lower_block_expr_results(exit.as_ref(), body, cleanup.as_deref(), *span)?;
                if results.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value block used in a scalar expression",
                    ));
                }
                Ok(results.remove(0))
            }
            sem::Expr::Error { .. } => Err(invalid_semantic("semantic expression is invalid")),
        }
    }

    fn lower_results_expr(&mut self, expr: &sem::Expr) -> Result<Vec<Value>, LowerError> {
        match expr {
            sem::Expr::Values { values, .. } => {
                values.iter().map(|value| self.lower_expr(value)).collect()
            }
            sem::Expr::DirectCall {
                function,
                args,
                span,
            } => {
                let function_info = self
                    .module
                    .functions
                    .get(function.0 as usize)
                    .ok_or_else(|| invalid_semantic("function id is out of range"))?;
                if function_info.result_kinds.len() == 1 {
                    return Ok(vec![self.lower_expr(expr)?]);
                }
                let args = self.lower_args_with_expected(
                    args,
                    &function_info.param_kinds,
                    &function_info.param_callable_sigs,
                )?;
                let rets = self.value_kinds(&function_info.result_kinds)?;
                let results = rets
                    .iter()
                    .copied()
                    .map(|kind| self.alloc(kind))
                    .collect::<Vec<_>>();
                self.instructions.push(Instr::Call {
                    dsts: results.iter().map(|result| result.reg).collect(),
                    callee: self
                        .ids
                        .functions
                        .get(function.0 as usize)
                        .copied()
                        .ok_or_else(|| invalid_semantic("function id is out of range"))?,
                    args,
                    span: *span,
                });
                Ok(results)
            }
            sem::Expr::GenericCall {
                generic,
                args,
                span,
            } => {
                let generic_info = self
                    .module
                    .generics
                    .get(generic.0 as usize)
                    .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
                if generic_info.result_kinds.len() == 1 {
                    return Ok(vec![self.lower_expr(expr)?]);
                }
                let args = self.lower_generic_args(args, generic_info)?;
                let rets = self.value_kinds(&generic_info.result_kinds)?;
                let results = rets
                    .iter()
                    .copied()
                    .map(|kind| self.alloc(kind))
                    .collect::<Vec<_>>();
                self.instructions.push(Instr::Call {
                    dsts: results.iter().map(|result| result.reg).collect(),
                    callee: self
                        .ids
                        .generics
                        .get(generic.0 as usize)
                        .copied()
                        .ok_or_else(|| invalid_semantic("generic id is out of range"))?,
                    args,
                    span: *span,
                });
                Ok(results)
            }
            sem::Expr::CallableCall {
                callable,
                args,
                arg_kinds,
                result_kinds,
                span,
                ..
            } => self.lower_callable_results(callable, args, arg_kinds, result_kinds, *span),
            sem::Expr::Signal {
                condition,
                condition_class,
                result_kinds,
                span,
                ..
            } => self.lower_signal_results(condition, *condition_class, result_kinds, *span),
            sem::Expr::If {
                condition,
                refinement: _,
                then_branch,
                else_branch,
                span,
            } => self.lower_results_if(condition, then_branch, else_branch, *span),
            sem::Expr::Handle {
                body,
                restart,
                span,
            } => self.lower_handle_results(body, restart, *span),
            sem::Expr::Block {
                exit,
                body,
                cleanup,
                span,
            } => self.lower_block_expr_results(exit.as_ref(), body, cleanup.as_deref(), *span),
            _ => Ok(vec![self.lower_expr(expr)?]),
        }
    }

    fn lower_results_if(
        &mut self,
        condition: &sem::Expr,
        then_branch: &sem::Expr,
        else_branch: &sem::Expr,
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let condition = self.lower_expr(condition)?;
        if condition.kind != ValueKind::Bool {
            return Err(invalid_semantic("if condition is not bool"));
        }

        let then_label = self.alloc_label();
        let else_label = self.alloc_label();
        let join_label = self.alloc_label();
        self.instructions.push(Instr::Branch {
            cond: condition.reg,
            then_label,
            else_label,
            span,
        });

        self.instructions.push(Instr::Label { label: then_label });
        let then_values = self.lower_results_expr(then_branch)?;
        let results = then_values
            .iter()
            .map(|value| self.alloc(value.kind))
            .collect::<Vec<_>>();
        let then_reachable = then_values.iter().all(|value| value.reachable);
        if then_reachable {
            for (result, value) in results.iter().zip(&then_values) {
                self.instructions.push(Instr::Move {
                    dst: result.reg,
                    src: value.reg,
                    span: then_branch.span(),
                });
            }
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: else_label });
        let else_values = self.lower_results_expr(else_branch)?;
        if else_values.len() != results.len() {
            return Err(invalid_semantic("if branch result arities differ"));
        }
        for (else_value, result) in else_values.iter().zip(&results) {
            if else_value.kind != result.kind {
                return Err(invalid_semantic("if branch result kinds differ"));
            }
        }
        let else_reachable = else_values.iter().all(|value| value.reachable);
        if else_reachable {
            for (result, value) in results.iter().zip(&else_values) {
                self.instructions.push(Instr::Move {
                    dst: result.reg,
                    src: value.reg,
                    span: else_branch.span(),
                });
            }
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: join_label });
        Ok(results
            .into_iter()
            .map(|result| Value {
                reachable: then_reachable || else_reachable,
                ..result
            })
            .collect())
    }

    fn lower_class_meta_object(
        &mut self,
        class: sem::ClassId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let result = self.alloc(ValueKind::ClassObject(Some(class)));
        self.emit_class_meta_object(result.reg, class, span);
        Ok(result)
    }

    fn emit_class_meta_object(&mut self, dst: Register, class: sem::ClassId, span: Span) {
        let class_id = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::ConstU64 {
            dst: class_id.reg,
            value: u64::from(class.0),
            span,
        });
        self.instructions.push(Instr::RuntimeCall {
            dst,
            op: RuntimeOp::ClassMetaObject,
            args: vec![class_id.reg],
            kind: ValueKind::ClassObject(Some(class)),
            span,
        });
    }

    fn lower_generic_meta_object(
        &mut self,
        generic: sem::GenericFunctionId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let generic_id = self.alloc(ValueKind::U64);
        let result = self.alloc(ValueKind::GenericFunctionObject(Some(generic)));
        self.instructions.push(Instr::ConstU64 {
            dst: generic_id.reg,
            value: u64::from(generic.0),
            span,
        });
        self.instructions.push(Instr::RuntimeCall {
            dst: result.reg,
            op: RuntimeOp::GenericMetaObject,
            args: vec![generic_id.reg],
            kind: result.kind,
            span,
        });
        Ok(result)
    }

    fn lower_known_callable(
        &mut self,
        callable: sem::KnownCallable,
        span: Span,
    ) -> Result<Value, LowerError> {
        let (function, sig) = match callable {
            sem::KnownCallable::DirectFunction(function) => {
                let lowered = self
                    .ids
                    .function_closure_thunks
                    .get(function.0 as usize)
                    .and_then(|id| *id)
                    .ok_or_else(|| invalid_semantic("function closure thunk id is out of range"))?;
                let sig = self
                    .ids
                    .function_sigs
                    .get(function.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("function signature id is out of range"))?;
                (lowered, sig)
            }
            sem::KnownCallable::GenericFunction(generic) => {
                let lowered = self
                    .ids
                    .generic_closure_thunks
                    .get(generic.0 as usize)
                    .and_then(|id| *id)
                    .ok_or_else(|| invalid_semantic("generic closure thunk id is out of range"))?;
                let sig = self
                    .ids
                    .generic_sigs
                    .get(generic.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("generic signature id is out of range"))?;
                (lowered, sig)
            }
            sem::KnownCallable::SlotAccessor {
                accessor,
                class,
                slot,
            } => {
                let accessor = self.lower_accessor_function(accessor, class, slot)?;
                (accessor.closure_thunk, accessor.sig)
            }
        };
        self.emit_closure_new(function, sig, &[], span)
    }

    fn lower_closure_value(
        &mut self,
        closure: sem::ClosureId,
        captures: &[sem::Capture],
        span: Span,
    ) -> Result<Value, LowerError> {
        let function = self
            .ids
            .closures
            .get(closure.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("closure id is out of range"))?;
        let sig = self
            .ids
            .closure_sigs
            .get(closure.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("closure signature id is out of range"))?;
        let mut capture_regs = Vec::with_capacity(captures.len());
        for capture in captures {
            let value = self
                .local_regs
                .get(capture.source.0 as usize)
                .and_then(|value| *value)
                .ok_or_else(|| invalid_semantic("captured local was read before assignment"))?;
            let value = self.coerce_value(value, self.value_kind(capture.kind)?, span)?;
            capture_regs.push(value.reg);
        }
        self.emit_closure_new(function, sig, &capture_regs, span)
    }

    fn emit_closure_new(
        &mut self,
        function: FunctionId,
        sig: CallableSigId,
        captures: &[Register],
        span: Span,
    ) -> Result<Value, LowerError> {
        let result = self.alloc(ValueKind::Closure(Some(sig)));
        self.instructions.push(Instr::ClosureNew {
            dst: result.reg,
            function,
            captures: captures.to_vec(),
            span,
        });
        Ok(result)
    }

    fn lower_accessor_function(
        &self,
        accessor: DefinitionId,
        class: sem::ClassId,
        slot: sem::SlotId,
    ) -> Result<AccessorFunction, LowerError> {
        self.ids
            .accessors
            .iter()
            .find(|candidate| {
                candidate.accessor == accessor && candidate.class == class && candidate.slot == slot
            })
            .copied()
            .ok_or_else(|| invalid_semantic("accessor function id is missing"))
    }

    fn lower_callable_call(
        &mut self,
        callable: &sem::Expr,
        args: &[sem::Expr],
        arg_kinds: &[sem::ValueKind],
        result_kind: sem::ValueKind,
        span: Span,
    ) -> Result<Value, LowerError> {
        let callee = self.lower_expr(callable)?;
        if !matches!(callee.kind, ValueKind::Function(_) | ValueKind::Closure(_)) {
            return Err(invalid_semantic(
                "callable call target is not a function or closure value",
            ));
        }
        let (args, arg_kinds) = self.lower_args_with_expected_and_kinds(args, arg_kinds)?;
        let ret = self.value_kind(result_kind)?;
        let result = self.alloc(ret);
        self.instructions.push(Instr::CallIndirect {
            dsts: vec![result.reg],
            callee: callee.reg,
            args,
            arg_kinds,
            rets: vec![ret],
            span,
        });
        Ok(result)
    }

    fn lower_callable_results(
        &mut self,
        callable: &sem::Expr,
        args: &[sem::Expr],
        arg_kinds: &[sem::ValueKind],
        result_kinds: &[sem::ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        if result_kinds.len() == 1 {
            return Ok(vec![self.lower_callable_call(
                callable,
                args,
                arg_kinds,
                result_kinds[0],
                span,
            )?]);
        }
        let callee = self.lower_expr(callable)?;
        if !matches!(callee.kind, ValueKind::Function(_) | ValueKind::Closure(_)) {
            return Err(invalid_semantic(
                "callable call target is not a function or closure value",
            ));
        }
        let (args, arg_kinds) = self.lower_args_with_expected_and_kinds(args, arg_kinds)?;
        let rets = self.value_kinds(result_kinds)?;
        let results = rets
            .iter()
            .copied()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        self.instructions.push(Instr::CallIndirect {
            dsts: results.iter().map(|result| result.reg).collect(),
            callee: callee.reg,
            args,
            arg_kinds,
            rets,
            span,
        });
        Ok(results)
    }

    fn value_kind(&self, kind: sem::ValueKind) -> Result<ValueKind, LowerError> {
        match kind {
            sem::ValueKind::KnownCallable(callable) => Ok(ValueKind::Closure(Some(
                known_callable_sig(self.ids, callable)?,
            ))),
            _ => value_kind(kind),
        }
    }

    fn value_kind_with_callable_signature(
        &self,
        kind: sem::ValueKind,
        callable_signature: Option<&sem::CallableSignature>,
    ) -> Result<ValueKind, LowerError> {
        if let Some(signature) = callable_signature {
            let sig = sem_callable_signature_id(self.ids, signature)?;
            return Ok(ValueKind::Closure(Some(sig)));
        }
        self.value_kind(kind)
    }

    fn value_kinds(&self, kinds: &[sem::ValueKind]) -> Result<Vec<ValueKind>, LowerError> {
        kinds
            .iter()
            .copied()
            .map(|kind| self.value_kind(kind))
            .collect()
    }

    fn lower_intrinsic_call(
        &mut self,
        intrinsic: sem::Intrinsic,
        args: &[sem::Expr],
        result_kind: sem::ValueKind,
        span: Span,
    ) -> Result<Value, LowerError> {
        let op = runtime_op_for_intrinsic(intrinsic);
        let mut regs = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let value = self.lower_expr(arg)?;
            let value = if runtime_op_arg_is_any(op, index) {
                self.box_any(value, arg.span())?
            } else {
                value
            };
            regs.push(value.reg);
        }
        let result = self.alloc(self.value_kind(result_kind)?);
        self.instructions.push(Instr::RuntimeCall {
            dst: result.reg,
            op,
            args: regs,
            kind: result.kind,
            span,
        });
        Ok(result)
    }

    fn lower_signal(
        &mut self,
        condition: &sem::Expr,
        condition_class: sem::ClassId,
        result_kinds: &[sem::ValueKind],
        span: Span,
    ) -> Result<Value, LowerError> {
        let mut results =
            self.lower_signal_results(condition, condition_class, result_kinds, span)?;
        if results.len() != 1 {
            return Err(invalid_semantic(
                "multiple-value signal used in a scalar expression",
            ));
        }
        Ok(results.remove(0))
    }

    fn lower_signal_results(
        &mut self,
        condition: &sem::Expr,
        condition_class: sem::ClassId,
        result_kinds: &[sem::ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let condition = self.lower_expr(condition)?;
        let ValueKind::Object(Some(lowered_class)) = condition.kind else {
            return Err(invalid_semantic("signal condition is not an object"));
        };
        if !self
            .module
            .is_same_or_subclass(lowered_class, condition_class)
        {
            return Err(invalid_semantic(
                "signal condition class does not match its semantic class",
            ));
        }
        let results = self
            .value_kinds(result_kinds)?
            .into_iter()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        self.instructions.push(Instr::Signal {
            dsts: results.iter().map(|result| result.reg).collect(),
            condition: condition.reg,
            span,
        });
        Ok(results)
    }

    fn lower_if(
        &mut self,
        condition: &sem::Expr,
        then_branch: &sem::Expr,
        else_branch: &sem::Expr,
        span: Span,
    ) -> Result<Value, LowerError> {
        let condition = self.lower_expr(condition)?;
        if condition.kind != ValueKind::Bool {
            return Err(invalid_semantic("if condition is not bool"));
        }

        let then_label = self.alloc_label();
        let else_label = self.alloc_label();
        let join_label = self.alloc_label();
        self.instructions.push(Instr::Branch {
            cond: condition.reg,
            then_label,
            else_label,
            span,
        });

        self.instructions.push(Instr::Label { label: then_label });
        let then_value = self.lower_expr(then_branch)?;
        let result = self.alloc(then_value.kind);
        if then_value.reachable {
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: then_value.reg,
                span: then_branch.span(),
            });
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: else_label });
        let else_value = self.lower_expr(else_branch)?;
        if else_value.kind != result.kind {
            return Err(invalid_semantic("if branch result kinds differ"));
        }
        if else_value.reachable {
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: else_value.reg,
                span: else_branch.span(),
            });
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: join_label });
        Ok(Value {
            reg: result.reg,
            kind: result.kind,
            reachable: then_value.reachable || else_value.reachable,
        })
    }

    fn lower_handle(
        &mut self,
        body: &sem::Expr,
        restart: &sem::Restart,
        span: Span,
    ) -> Result<Value, LowerError> {
        let mut results = self.lower_handle_results(body, restart, span)?;
        if results.len() != 1 {
            return Err(invalid_semantic(
                "multiple-value handle used in a scalar expression",
            ));
        }
        Ok(results.remove(0))
    }

    /// Lowers `handle` as a dynamic handler binding around a protected body.
    ///
    /// The body runs in a region whose clauses pop the binding on every
    /// abnormal exit. A restart addressed to this activation's token runs the
    /// recovery expression here; any other exception continues outward.
    fn lower_handle_results(
        &mut self,
        body: &sem::Expr,
        restart: &sem::Restart,
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let result_kinds = self.value_kinds(&restart.result_kinds)?;
        let results = result_kinds
            .iter()
            .copied()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        let guard = restart
            .guard
            .as_deref()
            .map(|guard| self.lower_expr(guard))
            .transpose()?
            .map(|guard| guard.reg);
        let token = self.alloc(ValueKind::U64);
        let descriptor = self
            .ids
            .restarts
            .iter()
            .position(|site| {
                site.span == (span.start..span.end) && site.class == restart.specializer
            })
            .ok_or_else(|| invalid_semantic("handle site has no restart descriptor"))?;
        self.instructions.push(Instr::PushHandler {
            dst: token.reg,
            descriptor,
            guard,
            span,
        });

        let restart_label = self.alloc_label();
        let unwind_label = self.alloc_label();
        let join_label = self.alloc_label();
        self.instructions.push(Instr::EnterRegion {
            catches: vec![
                Catch::Tag {
                    tag: ControlTag::Restart,
                    target: restart_label,
                },
                Catch::All {
                    target: unwind_label,
                },
            ],
        });
        let body_values = self.lower_results_expr(body)?;
        self.instructions.push(Instr::LeaveRegion);
        if body_values.len() != results.len() {
            return Err(invalid_semantic("handle body arity does not match restart"));
        }
        for (body_value, result) in body_values.iter().zip(&results) {
            if body_value.kind != result.kind {
                return Err(invalid_semantic("handle body kind does not match restart"));
            }
        }
        let body_reachable = body_values.iter().all(|value| value.reachable);
        if body_reachable {
            self.instructions.push(Instr::PopBinding {
                token: token.reg,
                span,
            });
            for (result, body_value) in results.iter().zip(&body_values) {
                self.instructions.push(Instr::Move {
                    dst: result.reg,
                    src: body_value.reg,
                    span: body.span(),
                });
            }
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        let unwound = self.alloc(ValueKind::Exn);
        self.instructions.push(Instr::CatchTarget {
            label: unwind_label,
            params: vec![unwound.reg],
        });
        self.instructions.push(Instr::PopBinding {
            token: token.reg,
            span,
        });
        self.instructions.push(Instr::Rethrow {
            exception: unwound.reg,
            span,
        });

        let target = self.alloc(ValueKind::U64);
        let caught = self.alloc(ValueKind::Object(None));
        let restarted = self.alloc(ValueKind::Exn);
        self.instructions.push(Instr::CatchTarget {
            label: restart_label,
            params: vec![target.reg, caught.reg, restarted.reg],
        });
        self.instructions.push(Instr::PopBinding {
            token: token.reg,
            span,
        });
        let mine = self.alloc(ValueKind::Bool);
        let mine_label = self.alloc_label();
        let onward_label = self.alloc_label();
        self.instructions.push(Instr::EqU64 {
            dst: mine.reg,
            lhs: target.reg,
            rhs: token.reg,
            span,
        });
        self.instructions.push(Instr::Branch {
            cond: mine.reg,
            then_label: mine_label,
            else_label: onward_label,
            span,
        });
        self.instructions.push(Instr::Label {
            label: onward_label,
        });
        self.instructions.push(Instr::Rethrow {
            exception: restarted.reg,
            span,
        });

        self.instructions.push(Instr::Label { label: mine_label });
        if let Some(local_id) = restart.condition_local {
            let condition = self.alloc(ValueKind::Object(Some(restart.specializer)));
            self.instructions.push(Instr::Move {
                dst: condition.reg,
                src: caught.reg,
                span: restart.span,
            });
            self.set_local(local_id, condition)?;
            let local = self
                .current_local(local_id)
                .ok_or_else(|| invalid_semantic("restart condition local id is out of range"))?;
            self.locals.push(Local {
                source: local_id,
                name: local.name.clone(),
                kind: condition.kind,
                reg: condition.reg,
            });
        }
        let restart_values = self.lower_results_expr(&restart.body)?;
        if restart_values.len() != results.len() {
            return Err(invalid_semantic("restart arity does not match handle"));
        }
        for (restart_value, result) in restart_values.iter().zip(&results) {
            if restart_value.kind != result.kind {
                return Err(invalid_semantic(
                    "restart result kind does not match handle",
                ));
            }
        }
        let restart_reachable = restart_values.iter().all(|value| value.reachable);
        if restart_reachable {
            for (result, restart_value) in results.iter().zip(&restart_values) {
                self.instructions.push(Instr::Move {
                    dst: result.reg,
                    src: restart_value.reg,
                    span: restart.body.span(),
                });
            }
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }

        self.instructions.push(Instr::Label { label: join_label });
        Ok(results
            .into_iter()
            .map(|result| Value {
                reachable: body_reachable || restart_reachable,
                ..result
            })
            .collect())
    }

    fn lower_args_with_expected(
        &mut self,
        args: &[sem::Expr],
        expected: &[sem::ValueKind],
        expected_callable_sigs: &[Option<sem::CallableSignature>],
    ) -> Result<Vec<Register>, LowerError> {
        let mut lowered = Vec::with_capacity(args.len());
        for (index, (arg, expected)) in args.iter().zip(expected).enumerate() {
            let value = self.lower_expr(arg)?;
            let expected = self.value_kind_with_callable_signature(
                *expected,
                expected_callable_sigs
                    .get(index)
                    .and_then(|signature| signature.as_ref()),
            )?;
            let value = self.coerce_value(value, expected, arg.span())?;
            lowered.push(value.reg);
        }
        Ok(lowered)
    }

    fn lower_args_with_expected_and_kinds(
        &mut self,
        args: &[sem::Expr],
        expected: &[sem::ValueKind],
    ) -> Result<(Vec<Register>, Vec<ValueKind>), LowerError> {
        let mut regs = Vec::with_capacity(args.len());
        let mut kinds = Vec::with_capacity(args.len());
        for (arg, expected) in args.iter().zip(expected) {
            let value = self.lower_expr(arg)?;
            let expected = self.value_kind(*expected)?;
            let value = self.coerce_value(value, expected, arg.span())?;
            regs.push(value.reg);
            kinds.push(value.kind);
        }
        Ok((regs, kinds))
    }

    fn lower_generic_args(
        &mut self,
        args: &[sem::Expr],
        generic: &sem::GenericFunction,
    ) -> Result<Vec<Register>, LowerError> {
        let mut lowered = Vec::with_capacity(args.len());
        for (index, (arg, expected)) in args.iter().zip(&generic.param_kinds).enumerate() {
            let value = self.lower_expr(arg)?;
            let target = dispatcher_param_kind(self.module, generic, index);
            let dispatched = generic.dispatch_params.contains(&index);
            let value = match *expected {
                Some(sem::ValueKind::Any) if dispatched => {
                    self.coerce_value(value, target, arg.span())?
                }
                Some(expected) => {
                    let expected = self.value_kind(expected)?;
                    self.coerce_value(value, expected, arg.span())?
                }
                // Unannotated parameters keep their legacy pass-through for
                // non-object values unless the parameter carries boxed values.
                None if target == ValueKind::Any
                    || (dispatched && matches!(value.kind, ValueKind::Object(_))) =>
                {
                    self.coerce_value(value, target, arg.span())?
                }
                None => value,
            };
            lowered.push(value.reg);
        }
        Ok(lowered)
    }

    fn coerce_value(
        &mut self,
        value: Value,
        expected: ValueKind,
        span: Span,
    ) -> Result<Value, LowerError> {
        if value.kind == expected {
            return Ok(value);
        }
        if matches!(
            (value.kind, expected),
            (ValueKind::Function(Some(_)), ValueKind::Function(None))
                | (ValueKind::Closure(Some(_)), ValueKind::Closure(None))
        ) {
            return Ok(value);
        }
        if let (ValueKind::Object(Some(actual)), ValueKind::Object(Some(expected_class))) =
            (value.kind, expected)
            && self.module.is_same_or_subclass(actual, expected_class)
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if let (ValueKind::Object(Some(actual)), ValueKind::Object(Some(expected_class))) =
            (value.kind, expected)
            && self.module.classes_may_overlap(actual, expected_class)
        {
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                class: expected_class,
                span,
            });
            return Ok(casted);
        }
        if matches!(
            (value.kind, expected),
            (ValueKind::Object(Some(_)), ValueKind::Object(None))
        ) {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if let (Some(actual), ValueKind::Object(Some(expected_class))) =
            (lowered_runtime_class(self.module, value.kind), expected)
            && self.module.is_same_or_subclass(actual, expected_class)
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if matches!(expected, ValueKind::Object(None))
            && lowered_runtime_class(self.module, value.kind).is_some()
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if expected == ValueKind::Any {
            return self.box_any(value, span);
        }
        if value.kind == ValueKind::Any
            && let Some(class) = scalar_concrete_class(self.module, expected)
        {
            return Ok(self.unbox(value, class, expected, span));
        }
        if matches!(value.kind, ValueKind::Any | ValueKind::Object(None))
            && let ValueKind::Object(Some(class)) = expected
        {
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                class,
                span,
            });
            return Ok(casted);
        }
        Err(invalid_semantic(
            "semantic type checking allowed an invalid lowered coercion",
        ))
    }

    fn unbox(&mut self, value: Value, class: sem::ClassId, kind: ValueKind, span: Span) -> Value {
        let result = self.alloc(kind);
        self.instructions.push(Instr::UnboxValue {
            dst: result.reg,
            value: value.reg,
            class,
            kind,
            span,
        });
        result
    }

    fn box_any(&mut self, value: Value, span: Span) -> Result<Value, LowerError> {
        if value.kind == ValueKind::Any {
            return Ok(value);
        }
        if matches!(
            value.kind,
            ValueKind::U64 | ValueKind::Function(_) | ValueKind::Closure(_) | ValueKind::Aggregate
        ) {
            return Err(invalid_semantic(
                "runtime metadata or callable values cannot be boxed as <object>",
            ));
        }
        let boxed = self.alloc(ValueKind::Any);
        self.instructions.push(Instr::BoxValue {
            dst: boxed.reg,
            src: value.reg,
            from: value.kind,
            span,
        });
        Ok(boxed)
    }

    fn alloc(&mut self, kind: ValueKind) -> Value {
        let value = Value {
            reg: Register(self.next_reg),
            kind,
            reachable: true,
        };
        assert_eq!(self.register_kinds.len() + 1, self.next_reg as usize);
        self.register_kinds.push(kind);
        self.next_reg = self.next_reg.saturating_add(1);
        value
    }

    fn lower_float_call(
        &mut self,
        builtin: sem::FloatBuiltin,
        args: &[sem::Expr],
        span: Span,
    ) -> Result<Value, LowerError> {
        let mut regs = Vec::with_capacity(args.len());
        for arg in args {
            regs.push(self.lower_expr(arg)?.reg);
        }
        let operand = |index: usize| {
            regs.get(index)
                .copied()
                .ok_or_else(|| invalid_semantic("float builtin argument is missing"))
        };
        let binary = |op| -> Result<(F64BinaryOp, Register, Register), LowerError> {
            Ok((op, operand(0)?, operand(1)?))
        };
        let unary = match builtin {
            sem::FloatBuiltin::Abs => Some(F64UnaryOp::Abs),
            sem::FloatBuiltin::Floor => Some(F64UnaryOp::Floor),
            sem::FloatBuiltin::Ceiling => Some(F64UnaryOp::Ceil),
            sem::FloatBuiltin::Round => Some(F64UnaryOp::Nearest),
            sem::FloatBuiltin::Truncate => Some(F64UnaryOp::Trunc),
            sem::FloatBuiltin::Sqrt => Some(F64UnaryOp::Sqrt),
            _ => None,
        };
        if let Some(op) = unary {
            let result = self.alloc(ValueKind::F64);
            self.instructions.push(Instr::F64Unary {
                dst: result.reg,
                op,
                src: operand(0)?,
                span,
            });
            return Ok(result);
        }
        let instr = match builtin {
            sem::FloatBuiltin::Abs
            | sem::FloatBuiltin::Floor
            | sem::FloatBuiltin::Ceiling
            | sem::FloatBuiltin::Round
            | sem::FloatBuiltin::Truncate
            | sem::FloatBuiltin::Sqrt => {
                return Err(invalid_semantic(
                    "unary float builtin was not lowered inline",
                ));
            }
            sem::FloatBuiltin::Min => binary(F64BinaryOp::Min)?,
            sem::FloatBuiltin::Max => binary(F64BinaryOp::Max)?,
            sem::FloatBuiltin::Remainder => binary(F64BinaryOp::Rem)?,
            sem::FloatBuiltin::IsNan => {
                // NaN is the only value unequal to itself.
                let x = operand(0)?;
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::F64NotEqual {
                    dst: result.reg,
                    lhs: x,
                    rhs: x,
                    span,
                });
                return Ok(result);
            }
            sem::FloatBuiltin::IsFinite => {
                // `abs(x) < inf` is false exactly for infinities and NaN.
                let magnitude = self.alloc(ValueKind::F64);
                self.instructions.push(Instr::F64Unary {
                    dst: magnitude.reg,
                    op: F64UnaryOp::Abs,
                    src: operand(0)?,
                    span,
                });
                let infinity = self.alloc(ValueKind::F64);
                self.instructions.push(Instr::ConstF64 {
                    dst: infinity.reg,
                    value: sem::FloatBits::from_f64(f64::INFINITY),
                    span,
                });
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::F64Compare {
                    dst: result.reg,
                    op: sem::CompareOp::Lt,
                    lhs: magnitude.reg,
                    rhs: infinity.reg,
                    span,
                });
                return Ok(result);
            }
            sem::FloatBuiltin::Library(function) => {
                let result = self.alloc(ValueKind::F64);
                self.instructions.push(Instr::FloatCall {
                    dst: result.reg,
                    function,
                    args: regs,
                    span,
                });
                return Ok(result);
            }
        };
        let (op, lhs, rhs) = instr;
        let result = self.alloc(ValueKind::F64);
        self.instructions.push(Instr::F64Binary {
            dst: result.reg,
            op,
            lhs,
            rhs,
            span,
        });
        Ok(result)
    }

    /// Lowers `as(<integer>, x)`: `f64.to_i64` truncates and traps on NaN,
    /// infinities, and out-of-range values; converting back and comparing
    /// then traps on any fractional part, so only integral floats convert.
    fn lower_exact_float_to_integer(
        &mut self,
        value: Value,
        span: Span,
    ) -> Result<Value, LowerError> {
        let result = self.alloc(ValueKind::I64);
        self.instructions.push(Instr::F64ToI64 {
            dst: result.reg,
            src: value.reg,
            span,
        });
        let round_trip = self.alloc(ValueKind::F64);
        self.instructions.push(Instr::I64ToF64 {
            dst: round_trip.reg,
            src: result.reg,
            span,
        });
        let exact = self.alloc(ValueKind::Bool);
        self.instructions.push(Instr::F64Compare {
            dst: exact.reg,
            op: sem::CompareOp::Eq,
            lhs: round_trip.reg,
            rhs: value.reg,
            span,
        });
        let done = self.alloc_label();
        let inexact = self.alloc_label();
        self.instructions.push(Instr::Branch {
            cond: exact.reg,
            then_label: done,
            else_label: inexact,
            span,
        });
        self.instructions.push(Instr::Label { label: inexact });
        self.instructions.push(Instr::Trap {
            trap: LokoTrap::InexactFloatToInteger,
            span,
        });
        self.instructions.push(Instr::Label { label: done });
        Ok(result)
    }

    /// Lowers `integer op float` exactly, comparing mathematical values
    /// rather than the integer rounded to a float.
    ///
    /// NaN compares false. A float at or beyond `±2^63` lies outside every
    /// `i64`, so its sign decides. Otherwise the float's integral part `t`
    /// converts exactly; the integer is compared with `t`, and on a tie the
    /// sign of the exact fraction `x - t` decides. Each path leaves
    /// `sign(integer - float)` in one register, which `op` then tests
    /// against zero.
    fn lower_exact_integer_float_compare(
        &mut self,
        op: sem::CompareOp,
        integer: Register,
        float: Register,
        span: Span,
    ) -> Value {
        let result = self.alloc(ValueKind::Bool);
        let sign = self.alloc(ValueKind::I64);
        let flag = self.alloc(ValueKind::Bool);
        let bound = self.alloc(ValueKind::F64);
        let [
            ordered,
            unordered,
            check_low,
            in_range,
            not_less,
            tie,
            not_above,
            less,
            greater,
            equal,
            decide,
            done,
        ] = core::array::from_fn(|_| self.alloc_label());

        let float_compare = |dst, op, lhs, rhs| Instr::F64Compare {
            dst,
            op,
            lhs,
            rhs,
            span,
        };
        let branch = |cond, then_label, else_label| Instr::Branch {
            cond,
            then_label,
            else_label,
            span,
        };
        let set_sign = |value| Instr::ConstI64 {
            dst: sign.reg,
            value,
            span,
        };
        let jump = |label| Instr::Jump { label, span };

        // NaN is the only float unequal to itself.
        self.instructions.extend([
            float_compare(flag.reg, sem::CompareOp::Eq, float, float),
            branch(flag.reg, ordered, unordered),
            Instr::Label { label: unordered },
            Instr::ConstBool {
                dst: result.reg,
                value: false,
                span,
            },
            jump(done),
            // At or above 2^63 the float exceeds every i64.
            Instr::Label { label: ordered },
            Instr::ConstF64 {
                dst: bound.reg,
                value: sem::FloatBits::from_f64(9_223_372_036_854_775_808.0),
                span,
            },
            float_compare(flag.reg, sem::CompareOp::Ge, float, bound.reg),
            branch(flag.reg, less, check_low),
            // Below -2^63 the float is less than every i64.
            Instr::Label { label: check_low },
            Instr::ConstF64 {
                dst: bound.reg,
                value: sem::FloatBits::from_f64(-9_223_372_036_854_775_808.0),
                span,
            },
            float_compare(flag.reg, sem::CompareOp::Lt, float, bound.reg),
            branch(flag.reg, greater, in_range),
        ]);

        // In range: compare with the integral part, then the fraction.
        let truncated = self.alloc(ValueKind::F64);
        let whole = self.alloc(ValueKind::I64);
        let fraction = self.alloc(ValueKind::F64);
        self.instructions.extend([
            Instr::Label { label: in_range },
            Instr::F64Unary {
                dst: truncated.reg,
                op: F64UnaryOp::Trunc,
                src: float,
                span,
            },
            Instr::F64ToI64 {
                dst: whole.reg,
                src: truncated.reg,
                span,
            },
            Instr::I64Compare {
                dst: flag.reg,
                op: sem::CompareOp::Lt,
                lhs: integer,
                rhs: whole.reg,
                span,
            },
            branch(flag.reg, less, not_less),
            Instr::Label { label: not_less },
            Instr::I64Compare {
                dst: flag.reg,
                op: sem::CompareOp::Gt,
                lhs: integer,
                rhs: whole.reg,
                span,
            },
            branch(flag.reg, greater, tie),
            Instr::Label { label: tie },
            Instr::F64Binary {
                dst: fraction.reg,
                op: F64BinaryOp::Sub,
                lhs: float,
                rhs: truncated.reg,
                span,
            },
            Instr::ConstF64 {
                dst: bound.reg,
                value: sem::FloatBits::from_f64(0.0),
                span,
            },
            float_compare(flag.reg, sem::CompareOp::Gt, fraction.reg, bound.reg),
            branch(flag.reg, less, not_above),
            Instr::Label { label: not_above },
            float_compare(flag.reg, sem::CompareOp::Lt, fraction.reg, bound.reg),
            branch(flag.reg, greater, equal),
            Instr::Label { label: less },
            set_sign(-1),
            jump(decide),
            Instr::Label { label: greater },
            set_sign(1),
            jump(decide),
            Instr::Label { label: equal },
            set_sign(0),
            jump(decide),
        ]);

        let zero = self.alloc(ValueKind::I64);
        self.instructions.extend([
            Instr::Label { label: decide },
            Instr::ConstI64 {
                dst: zero.reg,
                value: 0,
                span,
            },
            Instr::I64Compare {
                dst: result.reg,
                op,
                lhs: sign.reg,
                rhs: zero.reg,
                span,
            },
            jump(done),
            Instr::Label { label: done },
        ]);
        result
    }

    /// Lowers fixnum `lhs op rhs`: the overflow-reporting tape instruction,
    /// then a branch to [`Self::lower_integer_overflow`] when it overflowed.
    fn lower_checked_integer_arith(
        &mut self,
        op: I64ArithOp,
        lhs: Register,
        rhs: Register,
        span: Span,
    ) -> Value {
        let result = self.alloc(ValueKind::I64);
        let overflow = self.alloc(ValueKind::Bool);
        self.instructions.push(Instr::I64Binary {
            dst: result.reg,
            overflow: overflow.reg,
            op,
            lhs,
            rhs,
            span,
        });
        let overflowed = self.alloc_label();
        let done = self.alloc_label();
        self.instructions.push(Instr::Branch {
            cond: overflow.reg,
            then_label: overflowed,
            else_label: done,
            span,
        });
        self.instructions.push(Instr::Label { label: overflowed });
        self.lower_integer_overflow(span);
        self.instructions.push(Instr::Label { label: done });
        result
    }

    /// Emits what happens when fixnum arithmetic overflows `i64`.
    ///
    /// This is the single overflow target for `<integer>` arithmetic. It
    /// traps with [`LokoTrap::IntegerOverflow`] until `<integer>` gains
    /// bignum promotion, which replaces this trap.
    fn lower_integer_overflow(&mut self, span: Span) {
        self.instructions.push(Instr::Trap {
            trap: LokoTrap::IntegerOverflow,
            span,
        });
    }

    fn alloc_label(&mut self) -> LabelId {
        let label = LabelId(self.next_label);
        self.next_label = self.next_label.saturating_add(1);
        label
    }

    fn set_local(&mut self, id: sem::LocalId, value: Value) -> Result<(), LowerError> {
        let Some(slot) = self.local_regs.get_mut(id.0 as usize) else {
            return Err(invalid_semantic("local id is out of range"));
        };
        *slot = Some(value);
        Ok(())
    }

    fn current_local(&self, id: sem::LocalId) -> Option<&sem::Local> {
        self.source_locals.get(id.0 as usize)
    }
}

fn value_kind(kind: sem::ValueKind) -> Result<ValueKind, LowerError> {
    match kind {
        sem::ValueKind::Any => Ok(ValueKind::Any),
        sem::ValueKind::I64 => Ok(ValueKind::I64),
        sem::ValueKind::F64 => Ok(ValueKind::F64),
        sem::ValueKind::Bool => Ok(ValueKind::Bool),
        sem::ValueKind::String(class)
        | sem::ValueKind::Symbol(class)
        | sem::ValueKind::Vector(class) => Ok(ValueKind::Object(Some(class))),
        sem::ValueKind::Object(class) => Ok(ValueKind::Object(Some(class))),
        sem::ValueKind::ClassObject(class) => Ok(ValueKind::ClassObject(Some(class))),
        sem::ValueKind::GenericFunctionObject(generic) => {
            Ok(ValueKind::GenericFunctionObject(Some(generic)))
        }
        sem::ValueKind::MethodObject(method) => Ok(ValueKind::MethodObject(Some(method))),
        sem::ValueKind::SlotDescriptorObject { .. } => Ok(ValueKind::SlotDescriptorObject),
        sem::ValueKind::Callable => Ok(ValueKind::Closure(None)),
        sem::ValueKind::KnownCallable(sem::KnownCallable::DirectFunction(_))
        | sem::ValueKind::KnownCallable(sem::KnownCallable::GenericFunction(_))
        | sem::ValueKind::KnownCallable(sem::KnownCallable::SlotAccessor { .. }) => {
            Ok(ValueKind::Closure(None))
        }
        sem::ValueKind::Error => Err(invalid_semantic("semantic value kind is invalid")),
    }
}

fn runtime_op_for_intrinsic(intrinsic: sem::Intrinsic) -> RuntimeOp {
    match intrinsic {
        sem::Intrinsic::ObjectClass => RuntimeOp::ObjectClass,
        sem::Intrinsic::ClassName => RuntimeOp::ClassName,
        sem::Intrinsic::ClassDirectSuperclass => RuntimeOp::ClassDirectSuperclass,
        sem::Intrinsic::ClassDirectSlots => RuntimeOp::ClassDirectSlots,
        sem::Intrinsic::ClassEffectiveSlots => RuntimeOp::ClassEffectiveSlots,
        sem::Intrinsic::SlotName => RuntimeOp::SlotName,
        sem::Intrinsic::SlotType => RuntimeOp::SlotType,
        sem::Intrinsic::SlotInitKeyword => RuntimeOp::SlotInitKeyword,
        sem::Intrinsic::SlotMutable => RuntimeOp::SlotMutable,
        sem::Intrinsic::SlotGetter => RuntimeOp::SlotGetter,
        sem::Intrinsic::SlotSetter => RuntimeOp::SlotSetter,
        sem::Intrinsic::GenericFunctionName => RuntimeOp::GenericFunctionName,
        sem::Intrinsic::GenericFunctionMethods => RuntimeOp::GenericFunctionMethods,
        sem::Intrinsic::MethodGenericFunction => RuntimeOp::MethodGenericFunction,
        sem::Intrinsic::MethodSpecializers => RuntimeOp::MethodSpecializers,
        sem::Intrinsic::VectorSize => RuntimeOp::VectorSize,
        sem::Intrinsic::VectorRef => RuntimeOp::VectorRef,
        sem::Intrinsic::FloatToString => RuntimeOp::FloatToString,
        sem::Intrinsic::IntegerToString => RuntimeOp::IntegerToString,
        sem::Intrinsic::Concatenate => RuntimeOp::Concatenate,
    }
}

fn runtime_op_arg_is_any(op: RuntimeOp, index: usize) -> bool {
    match op {
        RuntimeOp::ObjectClass
        | RuntimeOp::ClassName
        | RuntimeOp::ClassDirectSuperclass
        | RuntimeOp::ClassDirectSlots
        | RuntimeOp::ClassEffectiveSlots
        | RuntimeOp::SlotName
        | RuntimeOp::SlotType
        | RuntimeOp::SlotInitKeyword
        | RuntimeOp::SlotMutable
        | RuntimeOp::SlotGetter
        | RuntimeOp::SlotSetter
        | RuntimeOp::GenericFunctionName
        | RuntimeOp::GenericFunctionMethods
        | RuntimeOp::MethodGenericFunction
        | RuntimeOp::MethodSpecializers
        | RuntimeOp::VectorSize => index == 0,
        RuntimeOp::VectorRef => index == 0,
        RuntimeOp::ClassMetaObject
        | RuntimeOp::GenericMetaObject
        | RuntimeOp::MethodMetaObject
        | RuntimeOp::SlotDescriptorObject
        | RuntimeOp::FloatToString
        | RuntimeOp::IntegerToString
        | RuntimeOp::Concatenate => false,
    }
}

/// Returns the lowered kind of generic parameter `index` as dispatchers and
/// callers see it. `<object>` and unannotated dispatch parameters carry raw
/// object handles unless [`sem::Module::generic_param_boxes_scalars`] says
/// integers or floats may apply, in which case they carry boxed values.
fn dispatcher_param_kind(
    module: &sem::Module,
    generic: &sem::GenericFunction,
    index: usize,
) -> ValueKind {
    let object_handle = generic.dispatch_params.contains(&index)
        && !module.generic_param_boxes_scalars(generic.id, index);
    match generic.param_kinds.get(index).copied().flatten() {
        Some(sem::ValueKind::Any) if object_handle => ValueKind::Object(None),
        Some(sem::ValueKind::Any) => ValueKind::Any,
        Some(
            sem::ValueKind::Object(class)
            | sem::ValueKind::String(class)
            | sem::ValueKind::Symbol(class)
            | sem::ValueKind::Vector(class),
        ) => ValueKind::Object(Some(class)),
        Some(sem::ValueKind::I64) => ValueKind::I64,
        Some(sem::ValueKind::F64) => ValueKind::F64,
        Some(sem::ValueKind::Bool) => ValueKind::Bool,
        Some(sem::ValueKind::Callable) => ValueKind::Closure(None),
        None if object_handle => ValueKind::Object(None),
        None => ValueKind::Any,
        Some(_) => ValueKind::Object(None),
    }
}

/// Returns the unboxed scalar kind whose values `class` describes: `i64`
/// for `<integer>`, `f64` for `<float>` and `<double-float>`.
fn scalar_kind_for_class(module: &sem::Module, class: sem::ClassId) -> Option<ValueKind> {
    match module.classes.get(class.0 as usize)?.origin {
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Integer,
        } => Some(ValueKind::I64),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Float | sem::BuiltinValueClass::DoubleFloat,
        } => Some(ValueKind::F64),
        _ => None,
    }
}

/// Returns the concrete built-in class of the unboxed scalar kind `kind`.
fn scalar_concrete_class(module: &sem::Module, kind: ValueKind) -> Option<sem::ClassId> {
    match kind {
        ValueKind::I64 => builtin_class(module, sem::BuiltinValueClass::Integer),
        ValueKind::F64 => builtin_class(module, sem::BuiltinValueClass::DoubleFloat),
        _ => None,
    }
}

/// Returns whether every value of the unboxed scalar category `kind` is an
/// instance of `class`: the scalar's concrete class (`<integer>` for `i64`,
/// `<double-float>` for `f64`) or one of its superclasses, such as
/// `<rational>` or `<float>`, `<real>`, `<number>`, and `<object>`.
fn is_scalar_value_class(module: &sem::Module, kind: ValueKind, class: sem::ClassId) -> bool {
    let concrete = match kind {
        ValueKind::I64 => sem::BuiltinValueClass::Integer,
        ValueKind::F64 => sem::BuiltinValueClass::DoubleFloat,
        _ => return false,
    };
    builtin_class(module, concrete).is_some_and(|actual| module.is_same_or_subclass(actual, class))
}

fn lowered_runtime_class(module: &sem::Module, kind: ValueKind) -> Option<sem::ClassId> {
    match kind {
        ValueKind::Object(Some(class)) => Some(class),
        ValueKind::ClassObject(_) => builtin_class(module, sem::BuiltinValueClass::Class),
        ValueKind::GenericFunctionObject(_) => {
            builtin_class(module, sem::BuiltinValueClass::GenericFunction)
        }
        ValueKind::MethodObject(_) => builtin_class(module, sem::BuiltinValueClass::Method),
        ValueKind::SlotDescriptorObject => {
            builtin_class(module, sem::BuiltinValueClass::SlotDescriptor)
        }
        ValueKind::Any
        | ValueKind::I64
        | ValueKind::F64
        | ValueKind::U64
        | ValueKind::Bool
        | ValueKind::Function(_)
        | ValueKind::Closure(_)
        | ValueKind::Aggregate
        | ValueKind::Exn
        | ValueKind::Object(None) => None,
    }
}

fn builtin_class(module: &sem::Module, kind: sem::BuiltinValueClass) -> Option<sem::ClassId> {
    module
        .classes
        .iter()
        .find(|class| class.origin == sem::ClassOrigin::BuiltinValue { kind })
        .map(|class| class.id)
}

fn input_register(index: usize) -> Register {
    Register(u32::try_from(index + 1).unwrap_or(u32::MAX))
}

fn next_after_inputs(input_count: usize) -> u32 {
    u32::try_from(input_count + 1).unwrap_or(u32::MAX)
}

fn invalid_semantic(message: &str) -> LowerError {
    LowerError::InvalidSemantic {
        message: message.into(),
    }
}

/// Stable dump name for a control tag.
#[must_use]
pub fn control_tag_name(tag: ControlTag) -> String {
    match tag {
        ControlTag::Restart => "restart".into(),
        ControlTag::Exit(signature) => format!("exit#{}", signature.0),
    }
}

fn runtime_op_name(op: RuntimeOp) -> &'static str {
    match op {
        RuntimeOp::ClassMetaObject => "class-object",
        RuntimeOp::GenericMetaObject => "generic-object",
        RuntimeOp::MethodMetaObject => "method-object",
        RuntimeOp::SlotDescriptorObject => "slot-descriptor-object",
        RuntimeOp::ObjectClass => "object-class",
        RuntimeOp::ClassName => "class-name",
        RuntimeOp::ClassDirectSuperclass => "class-direct-superclass",
        RuntimeOp::ClassDirectSlots => "class-direct-slots",
        RuntimeOp::ClassEffectiveSlots => "class-effective-slots",
        RuntimeOp::SlotName => "slot-name",
        RuntimeOp::SlotType => "slot-type",
        RuntimeOp::SlotInitKeyword => "slot-init-keyword",
        RuntimeOp::SlotMutable => "slot-mutable?",
        RuntimeOp::SlotGetter => "slot-getter",
        RuntimeOp::SlotSetter => "slot-setter",
        RuntimeOp::GenericFunctionName => "generic-function-name",
        RuntimeOp::GenericFunctionMethods => "generic-function-methods",
        RuntimeOp::MethodGenericFunction => "method-generic-function",
        RuntimeOp::MethodSpecializers => "method-specializers",
        RuntimeOp::VectorSize => "vector-size",
        RuntimeOp::VectorRef => "vector-ref",
        RuntimeOp::FloatToString => "float-to-string",
        RuntimeOp::IntegerToString => "integer-to-string",
        RuntimeOp::Concatenate => "concatenate",
    }
}

impl fmt::Display for Register {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%r{}", self.0)
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "lowered module")?;
        for (index, function) in self.functions.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            self.fmt_function(f, function)?;
        }
        Ok(())
    }
}

impl Module {
    fn fmt_function(&self, f: &mut fmt::Formatter<'_>, function: &Function) -> fmt::Result {
        write!(f, "fn l{} {}(", function.id.0, function.name)?;
        for (index, param) in function.params.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{} {}: ", param.reg, param.name)?;
            self.fmt_kind(f, param.kind)?;
        }
        write!(f, ") -> ")?;
        self.fmt_kind_list(f, &function.rets)?;
        self.fmt_kind_comment(f, function.kind)?;
        writeln!(f)?;

        for local in &function.locals {
            write!(
                f,
                "  local l{} {} = {}: ",
                local.source.0, local.name, local.reg
            )?;
            self.fmt_kind(f, local.kind)?;
            writeln!(f)?;
        }
        for instr in &function.instructions {
            self.fmt_instr(f, instr)?;
        }
        Ok(())
    }

    fn fmt_kind_comment(&self, f: &mut fmt::Formatter<'_>, kind: FunctionKind) -> fmt::Result {
        match kind {
            FunctionKind::Function(id) => writeln!(f, " ; source function {}", id.0),
            FunctionKind::Method(id) => writeln!(f, " ; method {}", id.0),
            FunctionKind::Dispatcher(id) => writeln!(f, " ; dispatcher for generic {}", id.0),
            FunctionKind::SlotAccessor { class, slot, .. } => {
                write!(f, " ; slot accessor ")?;
                self.fmt_slot(f, class, slot)?;
                writeln!(f)
            }
            FunctionKind::CallableThunk { callable } => {
                writeln!(f, " ; callable thunk {callable:?}")
            }
            FunctionKind::Closure(id) => writeln!(f, " ; closure {}", id.0),
            FunctionKind::ExitThunk { signature } => {
                writeln!(f, " ; exit thunk for signature {}", signature.0)
            }
        }
    }

    fn fmt_instr(&self, f: &mut fmt::Formatter<'_>, instr: &Instr) -> fmt::Result {
        match instr {
            Instr::Label { label } => writeln!(f, "{label}:"),
            Instr::ConstI64 { dst, value, span } => {
                write!(f, "  {dst} = const.i64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstF64 { dst, value, span } => {
                write!(f, "  {dst} = const.f64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstBool { dst, value, span } => {
                write!(f, "  {dst} = const.bool {value}")?;
                fmt_span(f, *span)
            }
            Instr::StringLiteral {
                dst,
                value,
                class,
                span,
            } => {
                write!(f, "  {dst} = literal.string ")?;
                fmt_quoted(f, value)?;
                write!(f, ": ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::SymbolLiteral {
                dst,
                name,
                class,
                span,
            } => {
                write!(f, "  {dst} = literal.symbol #{name}: ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::ConstU64 { dst, value, span } => {
                write!(f, "  {dst} = const.u64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstClass { dst, class, span } => {
                write!(f, "  {dst} = const.class ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::ConstFunction {
                dst,
                function,
                span,
            } => {
                write!(f, "  {dst} = const.function l{}", function.0)?;
                fmt_span(f, *span)
            }
            Instr::ClosureNew {
                dst,
                function,
                captures,
                span,
            } => {
                write!(f, "  {dst} = closure.new l{}", function.0)?;
                self.fmt_arg_list(f, captures)?;
                fmt_span(f, *span)
            }
            Instr::CaptureGet {
                dst,
                env,
                index,
                kind,
                span,
            } => {
                write!(f, "  {dst} = capture.get {env}[{index}]: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::BoxValue {
                dst,
                src,
                from,
                span,
            } => {
                write!(f, "  {dst} = box.any {src}: ")?;
                self.fmt_kind(f, *from)?;
                fmt_span(f, *span)
            }
            Instr::I64Binary {
                dst,
                overflow,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  {dst}, {overflow} = {} {lhs}, {rhs}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::F64Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  {dst} = {} {lhs}, {rhs}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::F64Unary { dst, op, src, span } => {
                write!(f, "  {dst} = {} {src}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::I64ToF64 { dst, src, span } => {
                write!(f, "  {dst} = i64.to_f64 {src}")?;
                fmt_span(f, *span)
            }
            Instr::FloatCall {
                dst,
                function,
                args,
                span,
            } => {
                write!(f, "  {dst} = float.call {}", function.symbol())?;
                self.fmt_arg_list(f, args)?;
                fmt_span(f, *span)
            }
            Instr::F64NotEqual {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  {dst} = f64.ne {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::F64ToI64 { dst, src, span } => {
                write!(f, "  {dst} = f64.to_i64 {src}")?;
                fmt_span(f, *span)
            }
            Instr::F64Compare {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                let op = match op {
                    sem::CompareOp::Eq => "f64.eq",
                    sem::CompareOp::Lt => "f64.lt",
                    sem::CompareOp::Le => "f64.le",
                    sem::CompareOp::Gt => "f64.gt",
                    sem::CompareOp::Ge => "f64.ge",
                };
                write!(f, "  {dst} = {op} {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::I64Compare {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                let op = match op {
                    sem::CompareOp::Eq => "i64.eq",
                    sem::CompareOp::Lt => "i64.lt",
                    sem::CompareOp::Le => "i64.le",
                    sem::CompareOp::Gt => "i64.gt",
                    sem::CompareOp::Ge => "i64.ge",
                };
                write!(f, "  {dst} = {op} {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::ValueEqual {
                dst,
                lhs,
                rhs,
                kind,
                span,
            } => {
                write!(f, "  {dst} = value.eq {lhs}, {rhs}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::Call {
                dsts,
                callee,
                args,
                span,
            } => {
                write!(f, "  ")?;
                self.fmt_result_list(f, dsts)?;
                write!(f, " = call l{}", callee.0)?;
                self.fmt_arg_list(f, args)?;
                fmt_span(f, *span)
            }
            Instr::CallIndirect {
                dsts,
                callee,
                args,
                rets,
                span,
                ..
            } => {
                write!(f, "  ")?;
                self.fmt_result_list(f, dsts)?;
                write!(f, " = call.indirect {callee}")?;
                self.fmt_arg_list(f, args)?;
                write!(f, ": ")?;
                self.fmt_kind_list(f, rets)?;
                fmt_span(f, *span)
            }
            Instr::MakeObject {
                dst,
                class,
                fields,
                span,
            } => {
                write!(f, "  {dst} = make ")?;
                self.fmt_class(f, *class)?;
                self.fmt_arg_list(f, fields)?;
                fmt_span(f, *span)
            }
            Instr::VectorLiteral {
                dst,
                elements,
                class,
                span,
            } => {
                write!(f, "  {dst} = literal.vector ")?;
                self.fmt_class(f, *class)?;
                self.fmt_arg_list(f, elements)?;
                fmt_span(f, *span)
            }
            Instr::RuntimeCall {
                dst,
                op,
                args,
                kind,
                span,
            } => {
                write!(f, "  {dst} = runtime.{}", runtime_op_name(*op))?;
                self.fmt_arg_list(f, args)?;
                write!(f, ": ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::HostCall {
                dst,
                function,
                args,
                kind,
                span,
            } => {
                let name = self
                    .host_functions
                    .get(function.0 as usize)
                    .map_or("?", |function| function.name.as_str());
                write!(f, "  {dst} = host.call h{} {name}", function.0)?;
                self.fmt_arg_list(f, args)?;
                write!(f, ": ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::ReadSlot {
                dst,
                receiver,
                class,
                slot,
                kind,
                span,
            } => {
                write!(f, "  {dst} = slot.read {receiver}, ")?;
                self.fmt_slot(f, *class, *slot)?;
                write!(f, ": ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::WriteSlot {
                dst,
                receiver,
                value,
                class,
                slot,
                kind,
                span,
            } => {
                write!(f, "  {dst} = slot.write {receiver}, ")?;
                self.fmt_slot(f, *class, *slot)?;
                write!(f, ", {value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::ClassOf {
                dst, object, span, ..
            } => {
                write!(f, "  {dst} = class_of {object}")?;
                fmt_span(f, *span)
            }
            Instr::UnboxValue {
                dst,
                value,
                class,
                kind,
                span,
            } => {
                write!(f, "  {dst} = unbox ")?;
                self.fmt_class(f, *class)?;
                write!(f, ", {value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::InstanceOf {
                dst,
                value,
                input,
                class,
                span,
            } => {
                write!(f, "  {dst} = instance? {value}: ")?;
                self.fmt_kind(f, *input)?;
                write!(f, ", ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::CastObject {
                dst,
                value,
                input,
                class,
                span,
            } => {
                write!(f, "  {dst} = as ")?;
                self.fmt_class(f, *class)?;
                write!(f, ", {value}: ")?;
                self.fmt_kind(f, *input)?;
                fmt_span(f, *span)
            }
            Instr::EqU64 {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  {dst} = u64.eq {lhs}, {rhs}")?;
                fmt_span(f, *span)
            }
            Instr::Branch {
                cond,
                then_label,
                else_label,
                span,
            } => {
                write!(f, "  br {cond}, {then_label}, {else_label}")?;
                fmt_span(f, *span)
            }
            Instr::Jump { label, span } => {
                write!(f, "  jmp {label}")?;
                fmt_span(f, *span)
            }
            Instr::Move { dst, src, span } => {
                write!(f, "  {dst} = mov {src}")?;
                fmt_span(f, *span)
            }
            Instr::Return { values, span } => {
                write!(f, "  return ")?;
                self.fmt_result_list(f, values)?;
                fmt_span(f, *span)
            }
            Instr::Trap { trap, span } => {
                write!(f, "  trap {} ({})", trap.code(), trap.description())?;
                fmt_span(f, *span)
            }
            Instr::PushHandler {
                dst,
                descriptor,
                guard,
                span,
            } => {
                write!(f, "  {dst} = condition.push-handler restart#{descriptor}")?;
                if let Some(site) = self.restarts.get(*descriptor) {
                    write!(f, " ")?;
                    self.fmt_class(f, site.class)?;
                }
                if let Some(guard) = guard {
                    write!(f, " when {guard}")?;
                }
                fmt_span(f, *span)
            }
            Instr::PopBinding { token, span } => {
                write!(f, "  control.pop {token}")?;
                fmt_span(f, *span)
            }
            Instr::Signal {
                dsts,
                condition,
                span,
            } => {
                write!(f, "  ")?;
                if !dsts.is_empty() {
                    self.fmt_result_list(f, dsts)?;
                    write!(f, " = ")?;
                }
                write!(f, "condition.signal {condition}")?;
                fmt_span(f, *span)
            }
            Instr::EnterRegion { catches } => {
                write!(f, "  region.enter [")?;
                for (index, catch) in catches.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    match catch {
                        Catch::Tag { tag, target } => {
                            write!(f, "{} -> {target}", control_tag_name(*tag))?;
                        }
                        Catch::All { target } => write!(f, "all -> {target}")?,
                    }
                }
                writeln!(f, "]")
            }
            Instr::LeaveRegion => writeln!(f, "  region.leave"),
            Instr::CatchTarget { label, params } => {
                write!(f, "{label}(")?;
                for (index, param) in params.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{param}")?;
                }
                writeln!(f, "):")
            }
            Instr::Rethrow { exception, span } => {
                write!(f, "  rethrow {exception}")?;
                fmt_span(f, *span)
            }
            Instr::PushExit { dst, span } => {
                write!(f, "  {dst} = control.push-exit")?;
                fmt_span(f, *span)
            }
            Instr::CheckExit { token, span } => {
                write!(f, "  control.check-exit {token}")?;
                fmt_span(f, *span)
            }
            Instr::Throw { tag, payload, span } => {
                write!(f, "  throw {} ", control_tag_name(*tag))?;
                self.fmt_result_list(f, payload)?;
                fmt_span(f, *span)
            }
        }
    }

    fn fmt_kind(&self, f: &mut fmt::Formatter<'_>, kind: ValueKind) -> fmt::Result {
        match kind {
            ValueKind::Any => write!(f, "any"),
            ValueKind::I64 => write!(f, "i64"),
            ValueKind::F64 => write!(f, "f64"),
            ValueKind::U64 => write!(f, "u64"),
            ValueKind::Bool => write!(f, "bool"),
            ValueKind::Aggregate => write!(f, "agg"),
            ValueKind::Exn => write!(f, "exn"),
            ValueKind::Function(Some(sig)) => {
                if let Some(signature) = self.callable_signature(sig) {
                    write!(f, "function(")?;
                    self.fmt_callable_signature(f, signature)?;
                    write!(f, ")")
                } else {
                    write!(f, "function#{}", sig.0)
                }
            }
            ValueKind::Function(None) => write!(f, "function"),
            ValueKind::Closure(Some(sig)) => {
                if let Some(signature) = self.callable_signature(sig) {
                    write!(f, "callable(")?;
                    self.fmt_callable_signature(f, signature)?;
                    write!(f, ")")
                } else {
                    write!(f, "callable#{}", sig.0)
                }
            }
            ValueKind::Closure(None) => write!(f, "callable"),
            ValueKind::Object(Some(class)) => {
                write!(f, "object(")?;
                self.fmt_class(f, class)?;
                write!(f, ")")
            }
            ValueKind::Object(None) => write!(f, "object"),
            ValueKind::ClassObject(Some(class)) => {
                write!(f, "class(")?;
                self.fmt_class(f, class)?;
                write!(f, ")")
            }
            ValueKind::ClassObject(None) => write!(f, "class"),
            ValueKind::GenericFunctionObject(Some(generic)) => {
                if let Some(generic_info) = self.runtime.generic(generic) {
                    write!(f, "generic({})", generic_info.name)
                } else {
                    write!(f, "generic#{}", generic.0)
                }
            }
            ValueKind::GenericFunctionObject(None) => write!(f, "generic"),
            ValueKind::MethodObject(Some(method)) => write!(f, "method#{}", method.0),
            ValueKind::MethodObject(None) => write!(f, "method"),
            ValueKind::SlotDescriptorObject => write!(f, "slot-descriptor"),
        }
    }

    fn fmt_callable_signature(
        &self,
        f: &mut fmt::Formatter<'_>,
        signature: &CallableSignature,
    ) -> fmt::Result {
        for (index, param) in signature.params.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            self.fmt_kind(f, *param)?;
        }
        write!(f, " -> ")?;
        self.fmt_kind_list(f, &signature.rets)
    }

    fn fmt_class(&self, f: &mut fmt::Formatter<'_>, class: sem::ClassId) -> fmt::Result {
        if let Some(class_info) = self.runtime.class(class) {
            write!(f, "{}", class_info.name)
        } else {
            write!(f, "class#{}", class.0)
        }
    }

    fn fmt_slot(
        &self,
        f: &mut fmt::Formatter<'_>,
        class: sem::ClassId,
        slot: sem::SlotId,
    ) -> fmt::Result {
        self.fmt_class(f, class)?;
        write!(f, ".")?;
        if let Some(slot_info) = self
            .runtime
            .classes
            .get(class.0 as usize)
            .and_then(|class_info| class_info.slots.get(slot.0 as usize))
        {
            write!(f, "{}", slot_info.name)
        } else {
            write!(f, "slot#{}", slot.0)
        }
    }

    fn fmt_arg_list(&self, f: &mut fmt::Formatter<'_>, args: &[Register]) -> fmt::Result {
        write!(f, "(")?;
        for (index, arg) in args.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{arg}")?;
        }
        write!(f, ")")
    }

    fn fmt_result_list(&self, f: &mut fmt::Formatter<'_>, regs: &[Register]) -> fmt::Result {
        match regs {
            [] => write!(f, "()"),
            [reg] => write!(f, "{reg}"),
            _ => self.fmt_arg_list(f, regs),
        }
    }

    fn fmt_kind_list(&self, f: &mut fmt::Formatter<'_>, kinds: &[ValueKind]) -> fmt::Result {
        match kinds {
            [] => write!(f, "()"),
            [kind] => self.fmt_kind(f, *kind),
            _ => {
                write!(f, "(")?;
                for (index, kind) in kinds.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    self.fmt_kind(f, *kind)?;
                }
                write!(f, ")")
            }
        }
    }
}

fn fmt_span(f: &mut fmt::Formatter<'_>, span: Span) -> fmt::Result {
    if span.is_empty() {
        writeln!(f)
    } else {
        writeln!(f, " ; span {}..{}", span.start, span.end)
    }
}

fn fmt_quoted(f: &mut fmt::Formatter<'_>, value: &str) -> fmt::Result {
    write!(f, "\"")?;
    for ch in value.chars() {
        match ch {
            '\n' => write!(f, "\\n")?,
            '\t' => write!(f, "\\t")?,
            '"' => write!(f, "\\\"")?,
            '\\' => write!(f, "\\\\")?,
            ch => write!(f, "{ch}")?,
        }
    }
    write!(f, "\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, grammar_frontend, identity::SourceFileId};
    use interloko_runtime as runtime;

    fn lower_source(source: &str) -> Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let ast = ast::lower(&tree);
        let sem = sem::lower(&ast);
        lower(&sem).expect("lowering should succeed")
    }

    fn runtime_class<'a>(module: &'a Module, name: &str) -> &'a runtime::RuntimeClass {
        module
            .runtime
            .classes
            .iter()
            .find(|class| class.name == name)
            .unwrap_or_else(|| panic!("missing runtime class {name}"))
    }

    #[test]
    fn loko_trap_codes_round_trip_and_are_distinct() {
        for (index, trap) in LokoTrap::ALL.iter().copied().enumerate() {
            assert_eq!(LokoTrap::from_code(trap.code()), Some(trap));
            assert!(
                LokoTrap::ALL[..index]
                    .iter()
                    .all(|earlier| earlier.code() != trap.code())
            );
        }
        assert_eq!(LokoTrap::from_code(0), None);
    }

    #[test]
    fn lowers_direct_calls_arithmetic_and_locals() {
        let module = lower_source(
            "define function add(a, b)\n  let c = a + b;\n  c\nend;\n\
             define function main()\n  add(40, 2)\nend;",
        );
        assert_eq!(module.functions.len(), 2);
        assert_eq!(module.main, Some(FunctionId(1)));
        let dump = module.dump();
        assert!(dump.contains("local l2 c = %r3: i64"));
        assert!(dump.contains("%r3, %r4 = i64.add_ovf %r1, %r2"));
        assert!(dump.contains("br %r4, bb0, bb1"));
        assert!(dump.contains("trap 3 (integer overflow)"));
        assert!(dump.contains("call l0(%r1, %r2)"));
    }

    #[test]
    fn lowers_known_callable_apply_to_existing_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function inc(x)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  let g = area;\n  apply(f, 2) + apply(g, make(<circle>, radius: 3))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 inc"));
        assert!(dump.contains("fn l1 main"));
        assert!(dump.contains("closure.new l"));
        assert!(dump.contains("call.indirect"));
        assert!(dump.contains("call l3("));
        assert!(dump.contains("local l0 f"));
        assert!(dump.contains("local l1 g"));
    }

    #[test]
    fn interns_callable_signatures_separately_from_function_targets() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function dec(x)\n  x - 1\nend;\n\
             define function main()\n  let f = inc;\n  let g = dec;\n  apply(f, 2) + apply(g, 2)\nend;",
        );

        assert!(module.call_sigs.contains(&CallableSignature {
            params: vec![ValueKind::I64],
            rets: vec![ValueKind::I64],
        }));
        assert_eq!(
            module.call_sigs[0],
            CallableSignature {
                params: vec![ValueKind::I64],
                rets: vec![ValueKind::I64],
            }
        );
        assert_eq!(
            module.functions[2].locals[0].kind,
            ValueKind::Closure(Some(CallableSigId(0)))
        );
        assert_eq!(
            module.functions[2].locals[1].kind,
            ValueKind::Closure(Some(CallableSigId(0)))
        );
        let dump = module.dump();
        assert!(dump.contains("closure.new l3"));
        assert!(dump.contains("closure.new l4"));
    }

    #[test]
    fn lowers_runtime_callable_parameter_apply_to_indirect_call() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function callit(f :: <callable>) => <integer>\n  apply(f, 41)\nend;\n\
             define function main()\n  callit(inc)\nend;",
        );

        assert!(module.call_sigs.contains(&CallableSignature {
            params: vec![ValueKind::I64],
            rets: vec![ValueKind::I64],
        }));
        assert_eq!(module.functions[1].params[0].kind, ValueKind::Closure(None));
        let dump = module.dump();
        assert!(dump.contains("fn l1 callit(%r1 f: callable) -> i64"));
        assert!(dump.contains("= call.indirect %r1(%r2): i64"));
        assert!(dump.contains("closure.new l3"));
    }

    #[test]
    fn lowers_slot_accessor_apply_to_indirect_call() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let getter = radius;\n  apply(getter, make(<circle>, radius: 3))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 main"));
        assert!(dump.contains("radius#accessor"));
        assert!(dump.contains(" ; slot accessor <circle>.radius"));
        assert!(dump.contains("local l0 getter"));
        assert!(dump.contains("closure.new l"));
        assert!(dump.contains("call.indirect"));
        assert!(dump.contains("slot.read %r1, <circle>.radius: i64"));
    }

    #[test]
    fn lowers_generic_dispatch_object_construction_and_slots() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(module.functions.len(), 5);
        assert_eq!(module.main, Some(FunctionId(0)));
        let dump = module.dump();
        assert!(dump.contains("runtime.class-object"));
        assert!(dump.contains("make <circle>(%r"));
        assert!(dump.contains("slot.read %r1, <circle>.radius: i64"));
        assert!(dump.contains("area#dispatch(%r1 x: object) -> i64"));
        assert!(dump.contains("class_of %r1"));
        assert!(dump.contains("br %r"));
    }

    #[test]
    fn lowers_parent_dispatch_as_applicable_to_child_classes() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 3;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 4;\nend;\n\
             define generic score(x :: <shape>) => <integer>;\n\
             define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
             define function main()\n  score(make(<circle>))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("score#dispatch(%r1 x: object(<shape>)) -> i64"));
        assert!(dump.contains("const.u64 1"));
        assert!(dump.contains("call l1(%r1)"));
    }

    #[test]
    fn lowers_object_construction_in_initialization_plan_order() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>, x: 7).y\nend;",
        );
        let main = module
            .function(module.main.expect("main should exist"))
            .expect("main should be lowered");
        let make = main
            .instructions
            .iter()
            .find_map(|instr| {
                if let Instr::MakeObject { class, fields, .. } = instr {
                    Some((*class, fields.as_slice()))
                } else {
                    None
                }
            })
            .expect("make should be lowered");

        assert_eq!(make.0, sem::ClassId(2));
        assert_eq!(make.1.len(), 2);
        assert_eq!(const_i64(main, make.1[0]), Some(7));
        assert_eq!(const_i64(main, make.1[1]), Some(2));
    }

    #[test]
    fn lowers_typed_generic_contracts_and_boxes_dispatcher_results() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic keep(x :: <object>) => <object>;\n\
             define method keep(c :: <circle>) => <circle>\n  c\nend;\n\
             define function main()\n  let ignored = keep(make(<circle>, radius: 5));\n  42\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l1 keep#method0(%r1 c: object(<circle>)) -> object(<circle>)"));
        assert!(dump.contains("keep#dispatch(%r1 x: object) -> any"));
        assert!(dump.contains("box.any"));
    }

    #[test]
    fn lowers_typed_runtime_slot_reads() {
        let module = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<flag>, valid?: #t).valid? then 1 else 0 end\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("make <flag>(%r"));
        assert!(dump.contains("slot.read %r"));
        assert!(dump.contains("<flag>.valid?: bool"));
    }

    #[test]
    fn lowers_object_top_slots_with_boxing() {
        let module = lower_source(
            "define class <box> (<object>)\n  slot value;\nend;\n\
             define function main()\n  make(<box>, value: 7).value\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("box.any"));
        assert!(dump.contains("<box>.value: any"));
        assert_eq!(
            runtime_class(&module, "<box>").slots[0].kind,
            runtime::ValueKind::Any
        );
    }

    #[test]
    fn lowers_known_class_objects_to_metaobjects() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function circle-class()\n  <circle>\nend;\n\
             define function main()\n  let klass = circle-class();\n  make(klass, radius: 9).radius\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 circle-class() -> class(<circle>)"));
        assert!(dump.contains("runtime.class-object"));
        assert!(dump.contains("call l0()"));
        assert!(dump.contains("make <circle>"));
    }

    #[test]
    fn lowers_class_of_to_existing_class_id_operation() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = make(<circle>, radius: 1);\n  let klass = class-of(c);\n  make(klass, radius: 10).radius\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("class_of"));
        assert!(dump.contains("local l1 klass = %r"));
        assert!(dump.contains("class(<circle>)"));
    }

    #[test]
    fn lowers_typed_direct_function_signatures() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function pass(c :: <circle>) => <circle>\n  c\nend;\n\
             define function circle-class() => class <circle>\n  <circle>\nend;\n\
             define function build(klass :: class <circle>, radius :: <integer>) => <circle>\n  make(klass, radius: radius)\nend;\n\
             define function main()\n  build(circle-class(), pass(make(<circle>, radius: 5)).radius).radius\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 pass(%r1 c: object(<circle>)) -> object(<circle>)"));
        assert!(dump.contains("fn l1 circle-class() -> class(<circle>)"));
        assert!(dump.contains("klass: class(<circle>)"));
        assert!(dump.contains("make <circle>"));
    }

    #[test]
    fn lowers_object_top_function_boundaries_with_boxing() {
        let module = lower_source(
            "define function hold(x :: <object>) => <object>\n  x\nend;\n\
             define function number-object() => <object>\n  7\nend;\n\
             define function main()\n  let ignored = hold(7);\n  let also = number-object();\n  42\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 hold(%r1 x: any) -> any"));
        assert!(dump.contains("fn l1 number-object() -> any"));
        assert!(dump.matches("box.any").count() >= 2);
    }

    #[test]
    fn lowers_instance_tests_casts_and_refined_locals() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function radius-or-zero(x :: <object>) => <integer>\n  if instance?(x, <circle>) then x.radius else 0 end\nend;\n\
             define function main()\n  radius-or-zero(make(<circle>, radius: 8))\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("instance? %r1: any, <circle>"));
        assert!(dump.contains("as <circle>, %r1: any"));
        assert!(dump.contains("slot.read"));
    }

    #[test]
    fn lowers_booleans_comparisons_and_if_join() {
        let module = lower_source(
            "define function check()\n  #t\nend;\n\
             define function main()\n  if 10 < 20 then 1 else 2 end\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("fn l0 check() -> bool"));
        assert!(dump.contains("const.bool true"));
        assert!(dump.contains("i64.lt"));
        assert!(dump.contains("br "));
        assert!(dump.contains("jmp bb"));
        assert!(dump.contains(" = mov "));
    }

    #[test]
    fn lowers_signal_handle_and_restart_join() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );

        let dump = module.dump();
        assert!(dump.contains("make <bad>(%r"));
        assert!(dump.contains("jmp bb0"));
        assert!(dump.contains("local l0 e = %r"));
        assert!(dump.contains("slot.read %r"));
        assert!(dump.contains("<bad>.fallback"));
        assert!(dump.contains(" = mov "));
    }

    fn const_i64(function: &Function, reg: Register) -> Option<i64> {
        function.instructions.iter().find_map(|instr| match instr {
            Instr::ConstI64 { dst, value, .. } if *dst == reg => Some(*value),
            _ => None,
        })
    }
}
