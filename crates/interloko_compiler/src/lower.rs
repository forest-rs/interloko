//! Backend-neutral executable IR lowered from semantic Interloko IR.
//!
//! This module owns the register-shaped executable IR that backends consume. It
//! explicitly does not own syntax parsing, semantic name resolution, runtime
//! object storage, or `execution_tape` bytecode encoding.
//!
//! Registers are [`execution_tape::program::Reg`]s, numbered as the tape
//! function numbers them: r0 is the effect register and a function's inputs
//! come first, from r1.
//!
//! # Typing facts
//!
//! Under the [`Proved`](crate::lower::CodegenPolicy::Proved) codegen policy,
//! lowering acts on what the soft typing pass ([`crate::typing`]) proved
//! about each check site: a proved cast between object registers reuses its
//! operand, a decided `instance?` test is a constant and an `if` on it loses
//! its dead branch, integer arithmetic that cannot overflow and conversions
//! that cannot leave their range drop their checks, a shift count proved in
//! range is not tested, a numeric dispatch whose method typing selected runs
//! that method's operation inline, any other dispatch whose method typing
//! selected calls it through a relinkable link, and a read sem narrowed
//! after a test reuses its register when flow typing narrowed it too. Each change is
//! recorded in [`check_codegen`](crate::lower::Module::check_codegen).
//! Why the facts hold in code that may run under newer code units is in the
//! `facts` submodule's documentation.
//!
//! # Failing checks
//!
//! A runtime type check that fails signals a `<type-error>` rather than
//! trapping. Each check carries the
//! [site](interloko_runtime::control::TypeErrorSite) it signals: its kind
//! (the stable code), the expected type, and its site, which is the typing
//! decision id `typing:<owner>:s<n>` when typing classifies the check.
//! Host-call checks (casts and unboxing) carry it on their
//! instruction, and tape emission builds the failing path around the call,
//! adding only a jump to the passing path. Checks lowering tests itself
//! branch to a [`SignalTypeError`](crate::lower::Instr::SignalTypeError):
//! overflow and shift-count checks already branched to a trap, while a
//! limited-integer conversion now tests the range (three instructions and a
//! branch, where a trapping conversion was one) and an exact float
//! conversion saturates before it compares.
//!
//! A `use-value` restart continues the check with a replacement that
//! passes it: a scalar result's register type is all such a check
//! establishes, and a replacement object is checked again. Proved checks
//! have no failing path, which is why flow typing types a check that may
//! fail by its expected type rather than by its operand.
//!
//! # Failing sends
//!
//! A send that finds no method for its arguments signals a
//! `<no-applicable-method>` rather than trapping. Every such send reaches its
//! generic's dispatcher: the runtime answers a dynamic send, and the
//! generic's closure thunk, with the dispatcher when its table selects
//! nothing, and a linked or inlined send only exists where typing proved a
//! method applies. The dispatcher ends where its last method failed to
//! match, so the passing path is unchanged; there it boxes its arguments
//! into a `<vector>`, makes the condition
//! ([`NoApplicableMethod`](crate::lower::Instr::NoApplicableMethod)) and
//! signals it. A `use-value` restart supplies the send's results, which the
//! dispatcher checks against the generic's declared result types, as an
//! indirect entry checks its arguments, before it returns them.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use execution_tape::program::Reg;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::DefinitionId;
use crate::runtime as runtime_bridge;
use crate::sem;
use crate::syntax::Span;
use crate::typing;
use interloko_model::condition::TypeErrorKind;
use interloko_model::sealing::ClosedWorld;
use interloko_model::type_object::TypeObject;
use interloko_model::types::IntRange;
use interloko_runtime::RuntimeLayout;
use interloko_runtime::code::{Contracts, DispatchCase, GenericDispatch};
use interloko_runtime::control::{RestartDescriptor, RestartParameter, TypeErrorSite};

mod facts;
mod reads;

use facts::TypingFacts;

/// Whether lowering may act on facts the soft typing pass proved.
///
/// This is a session input of its own, separate from
/// [`crate::typing::TypingConfig`]: the typing configuration only chooses
/// how loudly decisions are reported, and severities never affect lowering.
/// Under [`Self::Proved`], lowering drops or specializes the runtime checks
/// typing proved and records each one in [`Module::check_codegen`]. When
/// typing did not run, there is nothing to act on and `Proved` lowers what
/// `Off` does.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum CodegenPolicy {
    /// Lowering ignores typing facts and keeps every runtime check, so the
    /// generated code is what it would be without typing.
    Off,
    /// Lowering may drop or specialize runtime checks that typing proved.
    /// The default: results and runtime outcome codes are those of `Off`.
    #[default]
    Proved,
}

impl CodegenPolicy {
    /// Every policy, in order of how much lowering acts on.
    pub const ALL: &'static [Self] = &[Self::Off, Self::Proved];

    /// Returns the stable label used in dumps and tools.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Proved => "proved",
        }
    }

    /// Returns the policy whose label ([`Self::as_str`]) is `label`.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|policy| policy.as_str() == label)
    }
}

/// What generated code does with a runtime check, compared with lowering
/// that ignores typing facts.
///
/// Lowering already folds checks its own value kinds decide, such as a cast
/// to a superclass of the operand's declared class; those are part of what
/// [`CodegenPolicy::Off`] emits and report [`Self::Kept`]. The other
/// variants name what a proved typing fact changed.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CheckCodegen {
    /// The generated code is what lowering emits without typing facts.
    Kept,
    /// The check is gone: an `instance?` test became a constant (and an
    /// `if` on it lost its dead branch), a cast reuses its operand, a shift
    /// count is not tested, or the check sat in a branch that was dropped.
    Elided,
    /// The operation stays without its check: integer arithmetic that
    /// cannot overflow wraps, and a conversion that cannot leave its range
    /// reinterprets the bits.
    Specialized,
    /// A numeric dispatch whose method typing selected is replaced by that
    /// method's operation: no method selection, indirect call, or
    /// dispatcher.
    Inlined,
    /// A dispatch whose method typing selected calls that method through a
    /// link ([`Instr::LinkedSend`]): no method selection or indirect call.
    /// The runtime repoints the link when newer code is installed, so the
    /// send still reaches the newest method.
    Linked,
}

impl CheckCodegen {
    /// Returns the stable label used in dumps and EOP records.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Elided => "elided",
            Self::Specialized => "specialized",
            Self::Inlined => "inlined",
            Self::Linked => "linked",
        }
    }
}

/// What lowering did with one check site because of a typing fact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CheckCodegenRecord {
    /// Definition whose body holds the check.
    pub owner: DefinitionId,
    /// Check site within `owner`.
    pub site: sem::SiteId,
    /// What the generated code does with the check; never
    /// [`CheckCodegen::Kept`].
    pub codegen: CheckCodegen,
}

/// Durable function identifier within a lowered module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FunctionId(
    /// Zero-based lowered function index.
    pub u32,
);

/// Durable label identifier within one lowered function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LabelId(
    /// Zero-based label index.
    pub u32,
);

/// The boxed entry of a callable value [`Instr::CallableNew`] makes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoxedEntry {
    /// A closure of this function over the value's captures.
    Function(FunctionId),
    /// The boxed entry, a [`ValueKind::Entry`], of another callable value,
    /// which an adapter shares.
    Shared(Reg),
}

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
    /// `<i32>` integer value.
    I32,
    /// `<u32>` integer value.
    U32,
    /// IEEE 754 binary32 float value.
    F32,
    /// Unsigned integer value used by runtime metadata.
    U64,
    /// Boolean value used by control-flow conditions.
    Bool,
    /// Internal direct function reference, optionally tied to a callable signature.
    Function(Option<CallableSigId>),
    /// First-class callable value, optionally tied to a callable signature:
    /// a tape aggregate of the value's typed entry, its boxed entry and its
    /// shape ([`Instr::CallableNew`]).
    Closure(Option<CallableSigId>),
    /// A tape closure: one entry of a callable value, or a restart guard.
    Entry,
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
    /// A tape string, such as a keyword's name: not a Loko `<string>`.
    Str,
    /// A tape array, such as the keyword tail of a boxed call.
    Array(ArrayKind),
}

/// The elements of a [`ValueKind::Array`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ArrayKind {
    /// Tape strings: the keywords a boxed call passes, in source order.
    Str,
    /// Boxed `<object>` values: the values of those keywords.
    Any,
}

impl ArrayKind {
    /// The kind of one element.
    #[must_use]
    pub const fn element(self) -> ValueKind {
        match self {
            Self::Str => ValueKind::Str,
            Self::Any => ValueKind::Any,
        }
    }
}

/// Typed call ABI for a first-class callable value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableSignature {
    /// Argument value categories in call order: the required parameters,
    /// then one boxed [`ValueKind::Any`] per keyword.
    pub params: Vec<ValueKind>,
    /// Return value categories in order.
    pub rets: Vec<ValueKind>,
    /// The `#key` parameters the last `keywords.len()` arguments pass, in
    /// declaration order, each the keyword-absent marker
    /// ([`RuntimeOp::KeywordAbsent`]) when the call does not pass it. The
    /// value's typed entry fills defaults and checks the types.
    pub keywords: Vec<CallableKeyword>,
}

/// One `#key` parameter of a [`CallableSignature`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableKeyword {
    /// The keyword, without its colon.
    pub name: String,
    /// The value category the callee declares for it, which its typed
    /// entry checks.
    pub kind: ValueKind,
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
    /// Runtime method selection for each generic, in generic id order;
    /// `None` leaves sends to the generic's dispatcher.
    pub dispatch: Vec<Option<GenericDispatch>>,
    /// The contract of each generic, method and source function, which the
    /// lowered code trusts at its calls; see [`sem::Module::contract`].
    pub contracts: Contracts,
    /// The closed-world assumptions the lowered code relies on: those of
    /// every typing decision it acts on, sealed classes and method sets
    /// with source names, in first-use order. Empty under
    /// [`CodegenPolicy::Off`].
    pub assumptions: Vec<ClosedWorld>,
    /// Every check site a typing fact changed, sorted by owner and site.
    /// Empty under [`CodegenPolicy::Off`]; see [`Self::check_codegen`].
    pub check_codegen: Vec<CheckCodegenRecord>,
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

    /// Returns what the generated code does with check `site` of `owner`.
    ///
    /// A site no typing fact changed is [`CheckCodegen::Kept`], including
    /// one that has no generated code, such as a check in a constant no
    /// code reads.
    #[must_use]
    pub fn check_codegen(&self, owner: DefinitionId, site: sem::SiteId) -> CheckCodegen {
        self.check_codegen
            .binary_search_by(|record| (record.owner, record.site).cmp(&(owner, site)))
            .ok()
            .and_then(|index| self.check_codegen.get(index))
            .map_or(CheckCodegen::Kept, |record| record.codegen)
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
    /// The boxed entry of the callable values whose typed entry is `typed`
    /// (a callable thunk, a closure body or an exit thunk): it takes and
    /// returns values under the boxed callable ABI, checks and unboxes the
    /// arguments, and calls the typed entry's code.
    BoxedEntry {
        /// The function of the typed entry it boxes.
        typed: FunctionId,
    },
    /// Opens a callable value, an aggregate whose shape only this function's
    /// parameter contract proves to the tape: it returns the value's typed
    /// entry, its boxed entry and its shape.
    CallableParts,
    /// The typed entry of a callable value that came from untyped code into
    /// a binding of `signature`: it boxes the arguments, calls the value's
    /// boxed entry, and checks and unboxes the results.
    Adapter {
        /// The signature the adapted value is called with.
        signature: CallableSigId,
    },
}

/// Runtime host operation emitted by lowered IR.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RuntimeOp {
    /// Builds a class metaobject from a class id.
    ClassMetaObject,
    /// Reads the class id a class metaobject names.
    ClassMetaId,
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
    /// Returns a vector element as `<object>`, or a default when the index
    /// is outside the vector.
    VectorElementOr,
    /// Returns `$unsupplied`.
    Unsupplied,
    /// Returns the marker a call through a callable value passes for a
    /// keyword it does not pass, which only the value's entry sees.
    KeywordAbsent,
    /// Returns whether a boxed value is that marker.
    IsKeywordAbsent,
    /// Interns the symbol a tape string names.
    InternSymbol,
    /// Renders a float as literal text.
    FloatToString,
    /// Renders a `<single-float>` as suffixed literal text.
    SingleFloatToString,
    /// Renders an integer in decimal.
    IntegerToString,
    /// Joins two strings into a new string.
    Concatenate,
    /// Tests a value against a type object.
    InstanceOfType,
    /// Builds a `<limited-integer>` type object from whether it has each
    /// bound, the bounds, and the named type it is written as.
    LimitedIntegerType,
    /// Returns a `<limited-integer>`'s base class.
    LimitedIntegerBase,
    /// Returns a `<limited-integer>`'s lower bound as `<object>`, or `#f`.
    LimitedIntegerMin,
    /// Returns a `<limited-integer>`'s upper bound as `<object>`, or `#f`.
    LimitedIntegerMax,
    /// Returns the class a `<subclass-type>` bounds.
    SubclassTypeClass,
    /// Returns a type object's name as Loko writes the type.
    TypeName,
}

/// Condition that compiled Loko code signals with an explicit tape `trap`.
///
/// The tape carries [`LokoTrap::code`] as its trap code; runtime reporting
/// recovers the condition with [`LokoTrap::from_code`]. Failed runtime type
/// checks do not trap: they signal a `<type-error>` (see
/// [`Instr::SignalTypeError`]), and a send that finds no method signals a
/// `<no-applicable-method>` (see [`Instr::NoApplicableMethod`]). The codes
/// those used to trap with, 1 to 4, are retired rather than reused.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum LokoTrap {
    /// `make` on a class value named no class the call can construct: not
    /// the call's static bound nor one of its subclasses.
    MakeClassOutsideBound,
    /// `make` on a class value named a class requiring an init keyword the
    /// call does not pass.
    MakeMissingInitKeyword,
}

impl LokoTrap {
    /// Every Loko trap in code order.
    pub const ALL: &'static [Self] = &[Self::MakeClassOutsideBound, Self::MakeMissingInitKeyword];

    /// Returns the tape trap code for this condition.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::MakeClassOutsideBound => 5,
            Self::MakeMissingInitKeyword => 6,
        }
    }

    /// Returns the condition a tape trap code stands for, if any.
    #[must_use]
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            5 => Some(Self::MakeClassOutsideBound),
            6 => Some(Self::MakeMissingInitKeyword),
            _ => None,
        }
    }

    /// Returns a short human-readable name for this condition.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::MakeClassOutsideBound => "make on a class outside the call's bound",
            Self::MakeMissingInitKeyword => "make on a class needing a missing init keyword",
        }
    }

    /// Returns the stable diagnostic code that reports this condition.
    #[must_use]
    pub const fn diagnostic_code(self) -> DiagnosticCode {
        match self {
            Self::MakeClassOutsideBound | Self::MakeMissingInitKeyword => {
                DiagnosticCode::RuntimeTrap
            }
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

    /// Returns the mnemonic of the same operation without its overflow
    /// report, as [`Instr::I64Unchecked`] emits it.
    #[must_use]
    pub const fn unchecked_mnemonic(self) -> &'static str {
        match self {
            Self::Add => "i64.add",
            Self::Sub => "i64.sub",
            Self::Mul => "i64.mul",
        }
    }
}

/// Signed 64-bit bitwise operation; exact on `<integer>`s.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum I64BitOp {
    /// `a & b`.
    And,
    /// `a | b`.
    Or,
    /// `a ^ b`.
    Xor,
}

impl I64BitOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::And => "i64.and",
            Self::Or => "i64.or",
            Self::Xor => "i64.xor",
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
    /// Retry a frame with its function's current definition. The payload is
    /// how many more retry regions to cross, then the function to call.
    Retry,
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

/// One-operand instruction of `execution_tape`'s `0xFD` prefix: 32-bit
/// float operations and the conversions to and from 32-bit values.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Num32UnaryOp {
    /// `f32.neg`: Sign-bit negation.
    F32Neg,
    /// `f32.abs`: Sign-bit clear.
    F32Abs,
    /// `f32.floor`: Round toward negative infinity.
    F32Floor,
    /// `f32.ceil`: Round toward positive infinity.
    F32Ceil,
    /// `f32.trunc`: Round toward zero.
    F32Trunc,
    /// `f32.nearest`: Round to nearest, ties to even.
    F32Nearest,
    /// `f32.sqrt`: Correctly rounded square root.
    F32Sqrt,
    /// `i32.to_i64`: Sign extension; exact.
    I32ToI64,
    /// `u32.to_i64`: Zero extension; exact.
    U32ToI64,
    /// `i64.to_i32_wrap`: Keeps the low 32 bits; lowering emits it only
    /// where typing proved the value fits.
    I64ToI32Wrap,
    /// `i32.to_u32_wrap`: Reinterprets the bits; lowering emits it only
    /// where typing proved the value is not negative.
    I32ToU32Wrap,
    /// `u32.to_i32_wrap`: Reinterprets the bits; lowering emits it only
    /// where typing proved the value is at most `i32::MAX`.
    U32ToI32Wrap,
    /// `f32.to_f64`: Widening; exact.
    F32ToF64,
    /// `f64.to_f32`: Narrowing; rounds to nearest, ties to even.
    F64ToF32,
    /// `f32.to_i32`: Truncation; traps on NaN, infinities and out-of-range values.
    F32ToI32,
    /// `f32.to_u32`: Truncation; traps on NaN, infinities and out-of-range values.
    F32ToU32,
    /// `i32.to_f32`: Rounds to nearest, ties to even.
    I32ToF32,
    /// `u32.to_f32`: Rounds to nearest, ties to even.
    U32ToF32,
    /// `i64.to_f32`: Rounds once to nearest, ties to even.
    I64ToF32,
    /// `i32.to_f64`: Exact.
    I32ToF64,
    /// `u32.to_f64`: Exact.
    U32ToF64,
}

impl Num32UnaryOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::F32Neg => "f32.neg",
            Self::F32Abs => "f32.abs",
            Self::F32Floor => "f32.floor",
            Self::F32Ceil => "f32.ceil",
            Self::F32Trunc => "f32.trunc",
            Self::F32Nearest => "f32.nearest",
            Self::F32Sqrt => "f32.sqrt",
            Self::I32ToI64 => "i32.to_i64",
            Self::U32ToI64 => "u32.to_i64",
            Self::I64ToI32Wrap => "i64.to_i32_wrap",
            Self::I32ToU32Wrap => "i32.to_u32_wrap",
            Self::U32ToI32Wrap => "u32.to_i32_wrap",
            Self::F32ToF64 => "f32.to_f64",
            Self::F64ToF32 => "f64.to_f32",
            Self::F32ToI32 => "f32.to_i32",
            Self::F32ToU32 => "f32.to_u32",
            Self::I32ToF32 => "i32.to_f32",
            Self::U32ToF32 => "u32.to_f32",
            Self::I64ToF32 => "i64.to_f32",
            Self::I32ToF64 => "i32.to_f64",
            Self::U32ToF64 => "u32.to_f64",
        }
    }
}

/// Two-operand `i32`, `u32` or `f32` instruction of `execution_tape`'s
/// `0xFD` prefix. Comparisons write a `bool`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Num32BinaryOp {
    /// `i32.add`: Wrapping addition.
    I32Add,
    /// `i32.sub`: Wrapping subtraction.
    I32Sub,
    /// `i32.mul`: Wrapping multiplication.
    I32Mul,
    /// `i32.and`: Bitwise and.
    I32And,
    /// `i32.or`: Bitwise or.
    I32Or,
    /// `i32.xor`: Bitwise exclusive or.
    I32Xor,
    /// `i32.shl`: Left shift by the count masked to 0..=31.
    I32Shl,
    /// `i32.shr`: Arithmetic right shift by the count masked to 0..=31.
    I32Shr,
    /// `i32.eq`: Equality.
    I32Eq,
    /// `i32.lt`: Less than.
    I32Lt,
    /// `i32.gt`: Greater than.
    I32Gt,
    /// `i32.le`: Less than or equal.
    I32Le,
    /// `i32.ge`: Greater than or equal.
    I32Ge,
    /// `u32.add`: Wrapping addition.
    U32Add,
    /// `u32.sub`: Wrapping subtraction.
    U32Sub,
    /// `u32.mul`: Wrapping multiplication.
    U32Mul,
    /// `u32.and`: Bitwise and.
    U32And,
    /// `u32.or`: Bitwise or.
    U32Or,
    /// `u32.xor`: Bitwise exclusive or.
    U32Xor,
    /// `u32.shl`: Left shift by the count masked to 0..=31.
    U32Shl,
    /// `u32.shr`: Logical right shift by the count masked to 0..=31.
    U32Shr,
    /// `u32.eq`: Equality.
    U32Eq,
    /// `u32.lt`: Less than.
    U32Lt,
    /// `u32.gt`: Greater than.
    U32Gt,
    /// `u32.le`: Less than or equal.
    U32Le,
    /// `u32.ge`: Greater than or equal.
    U32Ge,
    /// `f32.add`: Addition.
    F32Add,
    /// `f32.sub`: Subtraction.
    F32Sub,
    /// `f32.mul`: Multiplication.
    F32Mul,
    /// `f32.div`: Division; never traps.
    F32Div,
    /// `f32.rem`: Truncated remainder with the sign of `a`.
    F32Rem,
    /// `f32.min`: NaN-propagating minimum.
    F32Min,
    /// `f32.max`: NaN-propagating maximum.
    F32Max,
    /// `f32.eq`: Equality; false for NaN.
    F32Eq,
    /// `f32.ne`: Inequality; true for NaN.
    F32Ne,
    /// `f32.lt`: Less than.
    F32Lt,
    /// `f32.gt`: Greater than.
    F32Gt,
    /// `f32.le`: Less than or equal.
    F32Le,
    /// `f32.ge`: Greater than or equal.
    F32Ge,
}

impl Num32BinaryOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::I32Add => "i32.add",
            Self::I32Sub => "i32.sub",
            Self::I32Mul => "i32.mul",
            Self::I32And => "i32.and",
            Self::I32Or => "i32.or",
            Self::I32Xor => "i32.xor",
            Self::I32Shl => "i32.shl",
            Self::I32Shr => "i32.shr",
            Self::I32Eq => "i32.eq",
            Self::I32Lt => "i32.lt",
            Self::I32Gt => "i32.gt",
            Self::I32Le => "i32.le",
            Self::I32Ge => "i32.ge",
            Self::U32Add => "u32.add",
            Self::U32Sub => "u32.sub",
            Self::U32Mul => "u32.mul",
            Self::U32And => "u32.and",
            Self::U32Or => "u32.or",
            Self::U32Xor => "u32.xor",
            Self::U32Shl => "u32.shl",
            Self::U32Shr => "u32.shr",
            Self::U32Eq => "u32.eq",
            Self::U32Lt => "u32.lt",
            Self::U32Gt => "u32.gt",
            Self::U32Le => "u32.le",
            Self::U32Ge => "u32.ge",
            Self::F32Add => "f32.add",
            Self::F32Sub => "f32.sub",
            Self::F32Mul => "f32.mul",
            Self::F32Div => "f32.div",
            Self::F32Rem => "f32.rem",
            Self::F32Min => "f32.min",
            Self::F32Max => "f32.max",
            Self::F32Eq => "f32.eq",
            Self::F32Ne => "f32.ne",
            Self::F32Lt => "f32.lt",
            Self::F32Gt => "f32.gt",
            Self::F32Le => "f32.le",
            Self::F32Ge => "f32.ge",
        }
    }
}

/// Overflow-reporting `i32` or `u32` arithmetic of `execution_tape`'s
/// `0xFD` prefix.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Num32CheckedOp {
    /// `i32.add_ovf`: Signed addition writing the wrapped result and an overflow flag.
    I32AddOvf,
    /// `i32.sub_ovf`: Signed subtraction writing the wrapped result and an overflow flag.
    I32SubOvf,
    /// `i32.mul_ovf`: Signed multiplication writing the wrapped result and an overflow flag.
    I32MulOvf,
    /// `u32.add_ovf`: Unsigned addition writing the wrapped result and an overflow flag.
    U32AddOvf,
    /// `u32.sub_ovf`: Unsigned subtraction writing the wrapped result and an overflow flag.
    U32SubOvf,
    /// `u32.mul_ovf`: Unsigned multiplication writing the wrapped result and an overflow flag.
    U32MulOvf,
}

impl Num32CheckedOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::I32AddOvf => "i32.add_ovf",
            Self::I32SubOvf => "i32.sub_ovf",
            Self::I32MulOvf => "i32.mul_ovf",
            Self::U32AddOvf => "u32.add_ovf",
            Self::U32SubOvf => "u32.sub_ovf",
            Self::U32MulOvf => "u32.mul_ovf",
        }
    }
}

/// Non-trapping numeric conversion of `execution_tape`'s `0xFD` prefix that
/// writes the converted value and whether the conversion was inexact.
///
/// Integer narrowing keeps the low bits (or reinterprets them) when the value
/// does not fit; float-to-integer conversion writes the saturating
/// truncation and reports NaN, infinities, fractions and out-of-range values
/// as inexact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConvertCheckedOp {
    /// `i64.to_i32_ovf`: Narrowing, inexact outside `i32`.
    I64ToI32Ovf,
    /// `i64.to_u32_ovf`: Narrowing, inexact outside `u32`.
    I64ToU32Ovf,
    /// `i32.to_u32_ovf`: Reinterpretation, inexact for negative values.
    I32ToU32Ovf,
    /// `u32.to_i32_ovf`: Reinterpretation, inexact above `i32::MAX`.
    U32ToI32Ovf,
    /// `f32.to_i32_exact`: Inexact unless the float is an integral `i32`.
    F32ToI32Exact,
    /// `f32.to_u32_exact`: Inexact unless the float is an integral `u32`.
    F32ToU32Exact,
    /// `f64.to_i32_exact`: Inexact unless the float is an integral `i32`.
    F64ToI32Exact,
    /// `f64.to_u32_exact`: Inexact unless the float is an integral `u32`.
    F64ToU32Exact,
    /// `f64.to_i64_exact`: Inexact unless the float is an integral `i64`;
    /// `2^63` is inexact.
    F64ToI64Exact,
}

impl ConvertCheckedOp {
    /// Returns the `execution_tape` mnemonic used in lowered dumps.
    #[must_use]
    pub const fn mnemonic(self) -> &'static str {
        match self {
            Self::I64ToI32Ovf => "i64.to_i32_ovf",
            Self::I64ToU32Ovf => "i64.to_u32_ovf",
            Self::I32ToU32Ovf => "i32.to_u32_ovf",
            Self::U32ToI32Ovf => "u32.to_i32_ovf",
            Self::F32ToI32Exact => "f32.to_i32_exact",
            Self::F32ToU32Exact => "f32.to_u32_exact",
            Self::F64ToI32Exact => "f64.to_i32_exact",
            Self::F64ToU32Exact => "f64.to_u32_exact",
            Self::F64ToI64Exact => "f64.to_i64_exact",
        }
    }

    /// The kind of the converted value.
    #[must_use]
    pub const fn result_kind(self) -> ValueKind {
        match self {
            Self::I64ToI32Ovf | Self::U32ToI32Ovf | Self::F32ToI32Exact | Self::F64ToI32Exact => {
                ValueKind::I32
            }
            Self::I64ToU32Ovf | Self::I32ToU32Ovf | Self::F32ToU32Exact | Self::F64ToU32Exact => {
                ValueKind::U32
            }
            Self::F64ToI64Exact => ValueKind::I64,
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

impl Function {
    /// The kind of `register`, or `None` for the effect register r0 and
    /// registers past the function's last.
    #[must_use]
    pub fn register_kind(&self, register: Reg) -> Option<ValueKind> {
        let index = register.0.checked_sub(1)?;
        self.register_kinds
            .get(usize::try_from(index).ok()?)
            .copied()
    }
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
    pub reg: Reg,
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
    pub reg: Reg,
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
        dst: Reg,
        /// Constant value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Float constant.
    ConstF64 {
        /// Destination register.
        dst: Reg,
        /// Constant value, compared by bit pattern.
        value: sem::FloatBits,
        /// Source span.
        span: Span,
    },
    /// `<i32>`, `<u32>` or `<single-float>` constant.
    Const32 {
        /// Destination register.
        dst: Reg,
        /// Constant value; floats compare by bit pattern.
        value: sem::Number32,
        /// Source span.
        span: Span,
    },
    /// One-operand `0xFD` instruction: 32-bit float math or a conversion.
    Num32Unary {
        /// Destination register.
        dst: Reg,
        /// Operation.
        op: Num32UnaryOp,
        /// Operand register.
        src: Reg,
        /// Source span.
        span: Span,
    },
    /// Two-operand `i32`, `u32` or `f32` instruction.
    Num32Binary {
        /// Destination register.
        dst: Reg,
        /// Operation.
        op: Num32BinaryOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// 32-bit integer arithmetic writing the wrapped result and whether the
    /// exact result left the type's range.
    Num32Checked {
        /// Destination register for the wrapped result.
        dst: Reg,
        /// Destination `bool` register set when the result overflowed.
        overflow: Reg,
        /// Operation.
        op: Num32CheckedOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Numeric conversion writing the converted value and whether it was
    /// inexact, so a range or exactness check is one branch on the flag.
    ConvertChecked {
        /// Destination register for the converted value.
        dst: Reg,
        /// Destination `bool` register set when the conversion was inexact.
        inexact: Reg,
        /// Operation.
        op: ConvertCheckedOp,
        /// Operand register.
        src: Reg,
        /// Source span.
        span: Span,
    },
    /// Boolean constant.
    ConstBool {
        /// Destination register.
        dst: Reg,
        /// Constant value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// Runtime string literal.
    StringLiteral {
        /// Destination register.
        dst: Reg,
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
        dst: Reg,
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
        dst: Reg,
        /// Constant value.
        value: u64,
        /// Source span.
        span: Span,
    },
    /// Class object constant represented by a runtime class id.
    ConstClass {
        /// Destination register.
        dst: Reg,
        /// Nominal class represented by this value.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Direct function constant.
    ConstFunction {
        /// Destination register.
        dst: Reg,
        /// Source function represented by this value.
        function: FunctionId,
        /// Source span.
        span: Span,
    },
    /// Creates a first-class closure value from a function and captured environment.
    ClosureNew {
        /// Destination register.
        dst: Reg,
        /// Closure body function.
        function: FunctionId,
        /// Captured value registers in environment order.
        captures: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Creates a first-class callable value: an aggregate of its typed
    /// entry, a closure of `typed` over `captures` that takes and returns
    /// values at the value's own signature, its boxed entry, which takes
    /// and returns every value but a callable as a boxed `<object>`, and
    /// `shape`, a hash of the typed entry's signature that an untyped
    /// caller compares its own call's against to call the typed entry
    /// directly.
    CallableNew {
        /// Destination register.
        dst: Reg,
        /// The typed entry's function.
        typed: FunctionId,
        /// The boxed entry.
        boxed: BoxedEntry,
        /// The typed entry's signature shape.
        shape: u64,
        /// Captured value registers in environment order, shared by both
        /// entries.
        captures: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Opens a callable value the tape knows the shape of
    /// ([`FunctionKind::CallableParts`] opens any other): reads its typed
    /// entry, boxed entry and shape into the registers given.
    OpenCallable {
        /// Destination [`ValueKind::Entry`] register of the typed entry.
        typed: Option<Reg>,
        /// Destination [`ValueKind::Entry`] register of the boxed entry.
        boxed: Option<Reg>,
        /// Destination `u64` register of the shape.
        shape: Option<Reg>,
        /// Callable value register.
        callable: Reg,
        /// Source span.
        span: Span,
    },
    /// Reads one value from the current closure environment.
    CaptureGet {
        /// Destination register.
        dst: Reg,
        /// Closure environment aggregate register.
        env: Reg,
        /// Capture index.
        index: usize,
        /// Captured value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Makes the cell of a [shared](sem::Local::shared) local, holding
    /// `value`; the cell is a runtime object the frame and the closures
    /// capturing the local share.
    CellNew {
        /// Destination register, of kind `Object(None)`.
        dst: Reg,
        /// The local's first value.
        value: Reg,
        /// The local's value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Reads the value a shared local's cell holds.
    CellGet {
        /// Destination register.
        dst: Reg,
        /// Cell register.
        cell: Reg,
        /// The local's value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Stores a value in a shared local's cell.
    CellSet {
        /// Cell register.
        cell: Reg,
        /// Stored value.
        value: Reg,
        /// The local's value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Boxes a concrete value into the top `<object>` runtime value.
    BoxValue {
        /// Destination register.
        dst: Reg,
        /// Source register.
        src: Reg,
        /// Source value category.
        from: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Signed integer arithmetic writing the wrapped result and whether the
    /// exact result overflowed `i64`.
    I64Binary {
        /// Destination register for the wrapped result.
        dst: Reg,
        /// Destination `bool` register set when the result overflowed.
        overflow: Reg,
        /// Arithmetic operator.
        op: I64ArithOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Signed integer arithmetic without an overflow report. Lowering emits
    /// it only where typing proved the exact result fits in `i64`, so it
    /// never wraps.
    I64Unchecked {
        /// Destination register.
        dst: Reg,
        /// Arithmetic operator.
        op: I64ArithOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Signed integer bitwise operation.
    I64Bitwise {
        /// Destination register.
        dst: Reg,
        /// Bitwise operator.
        op: I64BitOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 float arithmetic on two operands.
    F64Binary {
        /// Destination register.
        dst: Reg,
        /// Binary operator.
        op: F64BinaryOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 float operation on one operand.
    F64Unary {
        /// Destination register.
        dst: Reg,
        /// Unary operator.
        op: F64UnaryOp,
        /// Operand register.
        src: Reg,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 float comparison; every comparison with NaN is false.
    F64Compare {
        /// Destination register.
        dst: Reg,
        /// Comparison operator.
        op: sem::CompareOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Calls a runtime float library function.
    FloatCall {
        /// Destination register.
        dst: Reg,
        /// Called function.
        function: sem::FloatFunction,
        /// `f64` argument registers.
        args: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// IEEE 754 inequality; true when either operand is NaN.
    F64NotEqual {
        /// Destination register.
        dst: Reg,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Converts an integer to the nearest float, ties to even.
    I64ToF64 {
        /// Destination register.
        dst: Reg,
        /// Integer operand register.
        src: Reg,
        /// Source span.
        span: Span,
    },
    /// Truncates a float toward zero to an integer; traps on NaN, infinities,
    /// and values outside the `i64` range.
    F64ToI64 {
        /// Destination register.
        dst: Reg,
        /// Float operand register.
        src: Reg,
        /// Source span.
        span: Span,
    },
    /// Signed integer comparison.
    I64Compare {
        /// Destination register.
        dst: Reg,
        /// Comparison operator.
        op: sem::CompareOp,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Runtime value equality.
    ValueEqual {
        /// Destination register.
        dst: Reg,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Operand value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Identity of two boxed values, `==`.
    Identical {
        /// Destination register.
        dst: Reg,
        /// Left operand register, a boxed `<object>`.
        lhs: Reg,
        /// Right operand register, a boxed `<object>`.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Ordinary direct call to another lowered function.
    Call {
        /// Destination registers.
        dsts: Vec<Reg>,
        /// Lowered callee function.
        callee: FunctionId,
        /// Argument registers.
        args: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Asks the runtime which function a generic send calls with these
    /// arguments: the selected method of the newest code unit, or that
    /// unit's dispatcher. Either takes the send's arguments unchanged.
    SelectMethod {
        /// Destination function register.
        dst: Reg,
        /// Generic function, by its id in the shared runtime layout.
        generic: sem::GenericFunctionId,
        /// The send's argument registers, in dispatcher parameter kinds.
        args: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// A generic send typing proved the argument classes of: a call to the
    /// selected method through the program's link for that method, which the
    /// runtime repoints at the method's newest code on every install. The
    /// method takes the send's arguments and returns its results unchanged.
    LinkedSend {
        /// Destination registers.
        dsts: Vec<Reg>,
        /// The selected method, whose link the send calls through.
        method: sem::MethodId,
        /// The selected method's lowered function, the link's initial
        /// target.
        callee: FunctionId,
        /// The send's argument registers, in dispatcher parameter kinds.
        args: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Indirect call through a first-class callable value.
    CallIndirect {
        /// Destination registers.
        dsts: Vec<Reg>,
        /// Callable value register.
        callee: Reg,
        /// Argument registers.
        args: Vec<Reg>,
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
        dst: Reg,
        /// Nominal class to instantiate.
        class: sem::ClassId,
        /// Field value registers in slot order.
        fields: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Runtime immutable vector construction.
    VectorLiteral {
        /// Destination register.
        dst: Reg,
        /// Boxed element registers in source order.
        elements: Vec<Reg>,
        /// Built-in vector class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime host call for kernel-provided object/model operations.
    RuntimeCall {
        /// Destination register.
        dst: Reg,
        /// Runtime operation.
        op: RuntimeOp,
        /// Argument registers.
        args: Vec<Reg>,
        /// Result value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Call through an embedding-provided host function.
    HostCall {
        /// Destination register.
        dst: Reg,
        /// Registry-local host function.
        function: sem::HostFunctionId,
        /// Argument registers.
        args: Vec<Reg>,
        /// Result value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime slot read.
    ReadSlot {
        /// Destination register.
        dst: Reg,
        /// Receiver object register.
        receiver: Reg,
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
        dst: Reg,
        /// Receiver object register.
        receiver: Reg,
        /// Assigned value register.
        value: Reg,
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
        dst: Reg,
        /// Object register.
        object: Reg,
        /// Input kind: a runtime object or a boxed top `<object>` value.
        input: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Runtime checked unboxing of an integer or float from a boxed top
    /// `<object>` value. Unless the value is an instance of `class`, it
    /// signals `type_error`, and a `use-value` restart supplies the
    /// unboxed result.
    UnboxValue {
        /// Destination register.
        dst: Reg,
        /// Boxed value register.
        value: Reg,
        /// Class the boxed value must belong to.
        class: sem::ClassId,
        /// Unboxed result kind, `i64` or `f64`.
        kind: ValueKind,
        /// The `<type-error>` a failed check signals.
        type_error: Box<TypeErrorSite>,
        /// Source span.
        span: Span,
    },
    /// Runtime class membership test.
    InstanceOf {
        /// Destination register.
        dst: Reg,
        /// Tested value register.
        value: Reg,
        /// Tested value kind.
        input: ValueKind,
        /// Target class.
        class: sem::ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime checked cast from top `<object>` to a nominal object.
    ///
    /// Unless the value is an instance of `class`, it signals `type_error`.
    /// A `use-value` restart supplies a replacement object, which the cast
    /// checks again, signalling again until one passes.
    CastObject {
        /// Destination register.
        dst: Reg,
        /// Cast value register.
        value: Reg,
        /// Cast value kind.
        input: ValueKind,
        /// Target class.
        class: sem::ClassId,
        /// The `<type-error>` a failed check signals.
        type_error: Box<TypeErrorSite>,
        /// Source span.
        span: Span,
    },
    /// Runtime checked cast to a class object naming `bound` or one of its
    /// subclasses, failing as a [`Self::CastObject`] does.
    CastClassObject {
        /// Destination register.
        dst: Reg,
        /// Cast value register.
        value: Reg,
        /// Cast value kind.
        input: ValueKind,
        /// Class the value must name, or name a subclass of.
        bound: sem::ClassId,
        /// The `<type-error>` a failed check signals.
        type_error: Box<TypeErrorSite>,
        /// Source span.
        span: Span,
    },
    /// Loads a tape string constant.
    ConstStr {
        /// Destination [`ValueKind::Str`] register.
        dst: Reg,
        /// The string.
        value: String,
        /// Source span.
        span: Span,
    },
    /// Tape string equality.
    StrEq {
        /// Destination boolean register.
        dst: Reg,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Makes a tape array of `values`.
    ArrayNew {
        /// Destination [`ValueKind::Array`] register.
        dst: Reg,
        /// The elements' kind.
        kind: ArrayKind,
        /// Element registers, in order.
        values: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Reads a tape array's length, as a `u64`.
    ArrayLen {
        /// Destination `u64` register.
        dst: Reg,
        /// Array register.
        array: Reg,
        /// Source span.
        span: Span,
    },
    /// Reads one element of a tape array; an index outside it traps.
    ArrayGet {
        /// Destination register, of the array's element kind.
        dst: Reg,
        /// Array register.
        array: Reg,
        /// `u64` index register.
        index: Reg,
        /// Source span.
        span: Span,
    },
    /// Unsigned integer addition, wrapping.
    AddU64 {
        /// Destination register.
        dst: Reg,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Unsigned integer equality.
    EqU64 {
        /// Destination register.
        dst: Reg,
        /// Left operand register.
        lhs: Reg,
        /// Right operand register.
        rhs: Reg,
        /// Source span.
        span: Span,
    },
    /// Conditional branch.
    Branch {
        /// Boolean condition register.
        cond: Reg,
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
        dst: Reg,
        /// Source register.
        src: Reg,
        /// Source span.
        span: Span,
    },
    /// Function return.
    Return {
        /// Return value registers.
        values: Vec<Reg>,
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
    /// Signals the `<type-error>` of a check that failed on `values`, then
    /// continues at `resume` with the value a `use-value` restart supplies
    /// in `dst`. Ends its block.
    ///
    /// Lowering emits this for checks it tests itself (overflow, ranges,
    /// shift counts) when `dst`'s register type is all the check
    /// establishes, so the replacement needs no check of its own.
    SignalTypeError {
        /// Reg receiving the replacement.
        dst: Reg,
        /// The rejected values: one value, or an operation's operands.
        values: Vec<Reg>,
        /// The `<type-error>` to signal.
        type_error: Box<TypeErrorSite>,
        /// Where execution continues with the replacement.
        resume: LabelId,
        /// Source span.
        span: Span,
    },
    /// Pushes a dynamic condition handler binding for `class` and writes its
    /// token. Selecting the handler raises [`ControlTag::Restart`] with that
    /// token.
    PushHandler {
        /// Destination `u64` token register.
        dst: Reg,
        /// Index into [`Module::restarts`].
        descriptor: usize,
        /// Guard closure register, run on the signalling stack during search.
        guard: Option<Reg>,
        /// Source span.
        span: Span,
    },
    /// Pops the innermost dynamic binding, which must be `token`.
    PopBinding {
        /// Token register returned by the matching push.
        token: Reg,
        /// Source span.
        span: Span,
    },
    /// Unboxes the runtime object a boxed `<object>` holds into `dst`, or,
    /// when it holds a scalar, continues at `otherwise` with `dst` unset.
    UnboxObject {
        /// Destination object register.
        dst: Reg,
        /// Boxed `<object>` register.
        value: Reg,
        /// Where execution continues when the value is not a runtime
        /// object.
        otherwise: LabelId,
        /// Source span.
        span: Span,
    },
    /// Makes the `<no-applicable-method>` a send to `generic` signals when
    /// no method accepts `arguments`: a generic's dispatcher, after its last
    /// method failed to match.
    NoApplicableMethod {
        /// Destination condition register.
        dst: Reg,
        /// The generic the send called.
        generic: sem::GenericFunctionId,
        /// The send's arguments, boxed into a `<vector>`.
        arguments: Reg,
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
        dsts: Vec<Reg>,
        /// Condition object register.
        condition: Reg,
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
        params: Vec<Reg>,
    },
    /// Pushes a block exit point binding and writes its token.
    PushExit {
        /// Destination `u64` token register.
        dst: Reg,
        /// Source span.
        span: Span,
    },
    /// Fails the run as an expired exit unless the exit point `token` is live.
    CheckExit {
        /// Exit point token register.
        token: Reg,
        /// Source span.
        span: Span,
    },
    /// Raises a control tag with a payload.
    Throw {
        /// Raised tag.
        tag: ControlTag,
        /// Payload registers in tag order.
        payload: Vec<Reg>,
        /// Source span.
        span: Span,
    },
    /// Rethrows a caught exception with its original tag, payload and origin.
    Rethrow {
        /// Exception reference register.
        exception: Reg,
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
    /// Semantic IR broke an invariant lowering relies on. Semantic analysis
    /// accepted the program, so this is an internal compiler error.
    InvalidSemantic {
        /// Error details.
        message: String,
        /// Span of the definition being lowered, when known.
        span: Option<Span>,
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

    /// Returns the internal-compiler-error diagnostic that reports this
    /// failure, or `None` when lowering stopped for diagnostics that were
    /// already reported.
    #[must_use]
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        match self {
            Self::Diagnostics { .. } => None,
            Self::InvalidSemantic { message, span } => Some(
                Diagnostic::error(span.unwrap_or_default(), self.code())
                    .with_arg(DiagnosticArg::text("detail", message.clone())),
            ),
        }
    }

    /// Attributes an internal failure to the definition at `definition`,
    /// unless it already names a span.
    fn within(mut self, definition: Span) -> Self {
        if let Self::InvalidSemantic {
            span: span @ None, ..
        } = &mut self
        {
            *span = Some(definition);
        }
        self
    }
}

/// Lowers semantic IR to executable backend-neutral IR.
///
/// `typing` is the soft typing pass's result for `module`. Under
/// [`CodegenPolicy::Off`] it is not read, and the result is what lowering
/// emits without typing; under [`CodegenPolicy::Proved`], checks it proved
/// are dropped or specialized and recorded in [`Module::check_codegen`].
pub fn lower(
    module: &sem::Module,
    typing: &crate::typing::TypingArtifact,
    policy: CodegenPolicy,
) -> Result<Module, LowerError> {
    if !module.diagnostics.is_empty() {
        return Err(LowerError::Diagnostics {
            diagnostics: module.diagnostics.clone(),
        });
    }

    let facts = TypingFacts::new(typing, policy);
    Lowering::new(module, &facts)?.lower_module()
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
    /// The boxed entry of each function's callable values, parallel to
    /// `function_closure_thunks`.
    function_boxed_thunks: Vec<Option<FunctionId>>,
    /// The boxed entry of each generic's callable values, parallel to
    /// `generic_closure_thunks`.
    generic_boxed_thunks: Vec<Option<FunctionId>>,
    /// The boxed entry of each closure, parallel to `closures`; a restart
    /// guard, which is never a callable value, has none.
    closure_boxed: Vec<Option<FunctionId>>,
    /// The adapter of each signature an untyped value may enter, in
    /// discovery order.
    adapters: Vec<(CallableSigId, FunctionId)>,
    /// The function that opens callable values
    /// ([`FunctionKind::CallableParts`]).
    callable_parts: Option<FunctionId>,
    restarts: Vec<RestartDescriptor>,
    call_sigs: Vec<CallableSignature>,
    /// Runtime method selection of each generic, in generic id order, which
    /// both [`Module::dispatch`] and linked sends follow.
    dispatch: Vec<Option<GenericDispatch>>,
}

/// One exit thunk function, shared by every block exit with its signature.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExitThunk {
    kinds: Vec<sem::ValueKind>,
    signature: CallableSigId,
    function: FunctionId,
    boxed: FunctionId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AccessorFunction {
    accessor: DefinitionId,
    class: sem::ClassId,
    slot: sem::SlotId,
    function: FunctionId,
    closure_thunk: FunctionId,
    boxed_thunk: FunctionId,
    sig: CallableSigId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct AccessorTarget {
    accessor: DefinitionId,
    class: sem::ClassId,
    slot: sem::SlotId,
}

/// What a callable value of a function, generic or slot accessor calls,
/// for its typed and boxed entries.
struct KnownCallableThunk {
    id: FunctionId,
    name: String,
    callable: sem::KnownCallable,
    target: FunctionId,
    /// The value's signature.
    sig: CallableSigId,
    call_params: Vec<ValueKind>,
    /// Ranges of `limited(<integer>, ...)` parameters, parallel to
    /// `call_params`, which the thunk checks.
    param_ranges: Vec<Option<IntRange>>,
    /// The definition numbering the thunk's entry checks and, per
    /// parameter, the site of its check; checks without one always stay.
    entry_sites: Option<(DefinitionId, Vec<Option<sem::SiteId>>)>,
    rets: Vec<ValueKind>,
    /// The target's `#key` parameters, its last ones, which `call_params`
    /// takes boxed.
    keywords: Vec<KnownKeyword>,
    span: Span,
}

/// One `#key` parameter of a [`KnownCallableThunk`]'s target.
#[derive(Clone, Debug)]
struct KnownKeyword {
    name: String,
    /// The kind the target takes it at.
    kind: ValueKind,
    range: Option<IntRange>,
    /// The default function, called for `$unsupplied`; without one, the
    /// keyword takes `$unsupplied` itself.
    default: Option<sem::FunctionId>,
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
    facts: &'module TypingFacts<'module>,
    /// Owner of each closure's check sites; see [`facts::closure_owners`].
    closure_owners: Vec<Option<DefinitionId>>,
}

impl<'module> Lowering<'module> {
    fn new(
        module: &'module sem::Module,
        facts: &'module TypingFacts<'module>,
    ) -> Result<Self, LowerError> {
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
            let signature = with_keywords(signature, &keyword_names(&function.keywords));
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
                boxed_thunk: FunctionId(u32::MAX),
                sig,
            });
            next = next.saturating_add(1);
        }
        // Thunks are emitted in function and generic id order, so their ids
        // are assigned in that order too, not in order of first use.
        let mut used_functions = callable_uses.functions;
        used_functions.sort_unstable_by_key(|function| function.0);
        let mut used_generics = callable_uses.generics;
        used_generics.sort_unstable_by_key(|generic| generic.0);
        ids.function_closure_thunks
            .resize(module.functions.len(), None);
        for function in used_functions {
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
        for generic in used_generics {
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
                boxed: FunctionId(u32::MAX),
            });
            next = next.saturating_add(1);
        }

        intern_recorded_signatures(&mut ids.call_sigs, module)?;
        // Every callable value has a boxed entry too, in the same order.
        let mut take = || {
            let id = FunctionId(next);
            next = next.saturating_add(1);
            id
        };
        ids.function_boxed_thunks = ids
            .function_closure_thunks
            .iter()
            .map(|thunk| thunk.map(|_| take()))
            .collect();
        ids.generic_boxed_thunks = ids
            .generic_closure_thunks
            .iter()
            .map(|thunk| thunk.map(|_| take()))
            .collect();
        for accessor in &mut ids.accessors {
            accessor.boxed_thunk = take();
        }
        ids.closure_boxed = module
            .closures
            .iter()
            .map(|closure| (!closure.restart_guard).then(&mut take))
            .collect();
        for thunk in &mut ids.exit_thunks {
            thunk.boxed = take();
        }
        // A boxed entry adapts each typed callable it is passed, and an
        // adapter each typed callable it returns.
        let mut pending = Vec::new();
        let mut boxed_params = |sig: CallableSigId| {
            if let Some(signature) = ids.call_sigs.get(sig.0 as usize) {
                pending.extend(signature.params.iter().filter_map(typed_callable));
                // A typed entry adapts each typed callable keyword too.
                pending.extend(
                    signature
                        .keywords
                        .iter()
                        .filter_map(|keyword| typed_callable(&keyword.kind)),
                );
            }
        };
        for (thunk, sig) in ids.function_closure_thunks.iter().zip(&ids.function_sigs) {
            if thunk.is_some() {
                boxed_params(*sig);
            }
        }
        for (thunk, sig) in ids.generic_closure_thunks.iter().zip(&ids.generic_sigs) {
            if thunk.is_some() {
                boxed_params(*sig);
            }
        }
        for (boxed, sig) in ids.closure_boxed.iter().zip(&ids.closure_sigs) {
            if boxed.is_some() {
                boxed_params(*sig);
            }
        }
        while let Some(sig) = pending.pop() {
            if ids.adapters.iter().any(|(adapted, _)| *adapted == sig) {
                continue;
            }
            ids.adapters.push((sig, take()));
            if let Some(signature) = ids.call_sigs.get(sig.0 as usize) {
                pending.extend(signature.rets.iter().filter_map(typed_callable));
            }
        }
        // The last id, so a module that never opens a callable value whose
        // shape the tape cannot see leaves it out without renumbering.
        ids.callable_parts = Some(take());
        ids.restarts = callable_uses.restarts;

        let mut this = Self {
            module,
            ids,
            facts,
            // Closure sites are named by their owner under either policy.
            closure_owners: facts::closure_owners(module),
        };
        this.ids.dispatch = module
            .generics
            .iter()
            .map(|generic| this.dispatch_plan(generic))
            .collect::<Result<Vec<_>, LowerError>>()?;
        Ok(this)
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
        let mut checks = Vec::new();

        for function in &self.module.functions {
            if function.name == "main" {
                main = Some(self.lower_function_id(function.id)?);
            }
            functions.push(
                self.lower_function(function, &mut checks)
                    .map_err(|error| error.within(function.span))?,
            );
        }
        for method in &self.module.methods {
            functions.push(
                self.lower_method(method, &mut checks)
                    .map_err(|error| error.within(method.span))?,
            );
        }
        for generic in &self.module.generics {
            functions.push(
                self.lower_dispatcher(generic)
                    .map_err(|error| error.within(generic.span))?,
            );
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
                functions.push(self.lower_function_closure_thunk(function, &mut checks)?);
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
            functions.push(
                self.lower_closure(closure, &mut checks)
                    .map_err(|error| error.within(closure.span))?,
            );
        }
        for thunk in &self.ids.exit_thunks {
            functions.push(self.lower_exit_thunk(thunk)?);
        }
        for function in &self.module.functions {
            if let Some(Some(boxed)) = self.ids.function_boxed_thunks.get(function.id.0 as usize) {
                functions.push(self.lower_function_boxed_thunk(function, *boxed)?);
            }
        }
        for generic in &self.module.generics {
            if let Some(Some(boxed)) = self.ids.generic_boxed_thunks.get(generic.id.0 as usize) {
                functions.push(self.lower_generic_boxed_thunk(generic, *boxed)?);
            }
        }
        for accessor in &self.ids.accessors {
            functions.push(self.lower_accessor_boxed_thunk(*accessor)?);
        }
        for closure in &self.module.closures {
            if let Some(Some(boxed)) = self.ids.closure_boxed.get(closure.id.0 as usize) {
                functions.push(
                    self.lower_closure_boxed(closure, *boxed)
                        .map_err(|error| error.within(closure.span))?,
                );
            }
        }
        for thunk in &self.ids.exit_thunks {
            functions.push(self.lower_exit_boxed_thunk(thunk)?);
        }
        for (signature, id) in &self.ids.adapters {
            functions.push(self.lower_adapter(*signature, *id)?);
        }
        if let Some(parts) = self.ids.callable_parts
            && functions.iter().any(|function| {
                function
                    .instructions
                    .iter()
                    .any(|instr| matches!(instr, Instr::Call { callee, .. } if *callee == parts))
            })
        {
            functions.push(self.lower_callable_parts());
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

        let dispatch = self.ids.dispatch.clone();
        let check_codegen = merge_check_records(checks);
        Ok(Module {
            runtime: runtime_bridge::runtime_layout_from_sem(self.module),
            restarts: self.ids.restarts.clone(),
            call_sigs: self.ids.call_sigs.clone(),
            host_functions,
            functions,
            main,
            dispatch,
            contracts: contracts(self.module),
            assumptions: self.facts.closed_world(self.module, &check_codegen),
            check_codegen,
        })
    }

    /// How the runtime can select `generic`'s method without its dispatcher.
    ///
    /// Selection needs the class of every specialized argument, so a generic
    /// that dispatches on an unboxed scalar parameter, which the dispatcher
    /// decides statically, has no plan. A method is called directly only
    /// when the dispatcher would pass its arguments and results through
    /// unchanged; otherwise the send falls back to the dispatcher.
    fn dispatch_plan(
        &self,
        generic: &sem::GenericFunction,
    ) -> Result<Option<GenericDispatch>, LowerError> {
        let generic_rets = self.value_kinds(&generic.result_kinds)?;
        let mut cases = Vec::new();
        let mut positions = Vec::new();
        for method in sorted_methods(self.module, generic) {
            if method.specializers.len() != generic.arity || method.params.len() != generic.arity {
                return Ok(None);
            }
            let mut specializers = Vec::with_capacity(generic.arity);
            let mut direct = self.value_kinds(&method.result_kinds)? == generic_rets;
            for (index, specializer) in method.specializers.iter().copied().enumerate() {
                let dispatched = dispatcher_param_kind(self.module, generic, index);
                let specializer = specializer.filter(|class| *class != sem::OBJECT_CLASS_ID);
                if specializer.is_some() {
                    if !matches!(dispatched, ValueKind::Object(_) | ValueKind::Any) {
                        return Ok(None);
                    }
                    if !positions.contains(&index) {
                        positions.push(index);
                    }
                }
                let local = method
                    .locals
                    .get(method.params[index].0 as usize)
                    .ok_or_else(|| invalid_semantic("method parameter local id is out of range"))?;
                let param = self.value_kind(local.kind)?;
                let passes_through = param == dispatched
                    || (matches!(dispatched, ValueKind::Object(_))
                        && specializer
                            .is_some_and(|class| param == ValueKind::Object(Some(class))));
                direct &= passes_through;
                specializers.push(specializer);
            }
            cases.push(DispatchCase {
                method: method.id,
                specializers,
                direct,
            });
        }
        positions.sort_unstable();
        Ok(Some(GenericDispatch { cases, positions }))
    }

    /// Lowers the closure body shared by exit procedures of one signature:
    /// check the exit point is live, then throw the values to its block.
    fn lower_exit_thunk(&self, thunk: &ExitThunk) -> Result<Function, LowerError> {
        let kinds = self.value_kinds(&thunk.kinds)?;
        let env = register_at(0);
        let values = (0..kinds.len())
            .map(|index| register_at(index + 1))
            .collect::<Vec<_>>();
        let token = register_at(kinds.len() + 1);
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
        let span = Span::new(0, 0);
        // An exit procedure is a first-class callable, so whoever invokes it
        // may not know the block's result classes.
        let mut checks = Vec::new();
        let mut next_label = 0;
        let mut payload = vec![token];
        // Exit thunks are shared by every block with the same result kinds,
        // so their checks belong to no one body's sites and always stay.
        let name = format!("exit#{}", thunk.signature.0);
        for (index, (value, kind)) in values.iter().zip(&kinds).enumerate() {
            let Some(target) = entry_target(*kind) else {
                payload.push(*value);
                continue;
            };
            let mut code = CodeBuffer {
                register_kinds: &mut register_kinds,
                next_label: &mut next_label,
                instructions: &mut checks,
            };
            payload.push(entry_check(
                &mut code,
                target,
                *value,
                *kind,
                entry_site_name(&name, index),
                span,
            ));
        }
        Ok(Function {
            id: thunk.function,
            name,
            kind: FunctionKind::ExitThunk {
                signature: thunk.signature,
            },
            params,
            ret: kinds.first().copied().unwrap_or(ValueKind::Any),
            rets: kinds,
            register_kinds,
            locals: Vec::new(),
            instructions: [
                Instr::CaptureGet {
                    dst: token,
                    env,
                    index: 0,
                    kind: ValueKind::U64,
                    span,
                },
                Instr::CheckExit { token, span },
            ]
            .into_iter()
            .chain(checks)
            .chain([Instr::Throw {
                tag: ControlTag::Exit(thunk.signature),
                payload,
                span,
            }])
            .collect(),
            span,
        })
    }

    /// Lowers the boxed entry of the exit procedures of one signature:
    /// [`Self::lower_exit_thunk`] for arguments that arrive boxed, each
    /// checked and unboxed to the block's kind.
    fn lower_exit_boxed_thunk(&self, thunk: &ExitThunk) -> Result<Function, LowerError> {
        let kinds = self.value_kinds(&thunk.kinds)?;
        let span = Span::new(0, 0);
        let mut body = BodyLowering::new(self.module, &self.ids, &[]);
        let env = body.push_param("$env", ValueKind::Aggregate);
        let wires = kinds
            .iter()
            .enumerate()
            .map(|(index, _)| body.push_param(&format!("value{index}"), BOXED_WIRE))
            .collect::<Vec<_>>();
        let (keywords, keyword_values) = body.push_keyword_tail();
        body.reject_keywords(keywords, keyword_values, span)?;
        let token = body.alloc(ValueKind::U64);
        body.instructions.extend([
            Instr::CaptureGet {
                dst: token.reg,
                env: env.reg,
                index: 0,
                kind: ValueKind::U64,
                span,
            },
            Instr::CheckExit {
                token: token.reg,
                span,
            },
        ]);
        // An exit procedure is a first-class callable, so whoever invokes it
        // may not know the block's result kinds. Exit thunks are shared by
        // every block with the same result kinds, so their checks belong to
        // no one body's sites and always stay.
        let name = format!("exit#{}", thunk.signature.0);
        let mut payload = vec![token.reg];
        for (index, (wire, kind)) in wires.into_iter().zip(&kinds).enumerate() {
            let site = entry_site_name(&name, index);
            payload.push(body.unwire(wire, *kind, None, &site, span)?.reg);
        }
        body.instructions.push(Instr::Throw {
            tag: ControlTag::Exit(thunk.signature),
            payload,
            span,
        });
        let rets = vec![BOXED_WIRE; kinds.len()];
        Ok(Function {
            id: thunk.boxed,
            name: format!("{name}#boxed"),
            kind: FunctionKind::BoxedEntry {
                typed: thunk.function,
            },
            params: body.params,
            ret: rets.first().copied().unwrap_or(ValueKind::Any),
            rets,
            register_kinds: body.register_kinds,
            locals: Vec::new(),
            instructions: body.instructions,
            span,
        })
    }

    fn lower_function(
        &self,
        function: &sem::Function,
        checks: &mut Vec<CheckCodegenRecord>,
    ) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_function(self.module, &self.ids, function)?
            .typed(self.facts, Some(function.definition));
        let rets = self.value_kinds(&function.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_retryable_block(&function.body, &rets, function.span)?;
        checks.append(&mut body.checks);
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

    fn lower_method(
        &self,
        method: &sem::Method,
        checks: &mut Vec<CheckCodegenRecord>,
    ) -> Result<Function, LowerError> {
        let mut body = BodyLowering::for_method(self.module, &self.ids, method)?
            .typed(self.facts, Some(method.definition));
        let rets = self.value_kinds(&method.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_retryable_block(&method.body, &rets, method.span)?;
        checks.append(&mut body.checks);
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
        // Typing classifies none of a dispatcher's checks; they are named
        // by the generic and their span.
        body.site_owner = Some(generic.definition);
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
        let receiver = register_at(0);
        let result = register_at(1);
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

    fn function_thunk(&self, function: &sem::Function) -> Result<KnownCallableThunk, LowerError> {
        let id = self
            .ids
            .function_closure_thunks
            .get(function.id.0 as usize)
            .copied()
            .flatten()
            .ok_or_else(|| invalid_semantic("function closure thunk id is out of range"))?;
        let sig = self
            .ids
            .function_sigs
            .get(function.id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("function signature id is out of range"))?;
        let mut call_params = function
            .param_kinds
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?;
        let required = call_params.len().saturating_sub(function.keywords.len());
        let keywords = function
            .keywords
            .iter()
            .enumerate()
            .map(|(index, keyword)| {
                // A typed callable keyword is taken at its signature, which
                // its check adapts an untyped callable to.
                let kind = match function
                    .param_callable_sigs
                    .get(required + index)
                    .and_then(Option::as_ref)
                {
                    Some(signature) => {
                        ValueKind::Closure(Some(sem_callable_signature_id(&self.ids, signature)?))
                    }
                    None => call_params[required + index],
                };
                Ok(KnownKeyword {
                    name: keyword.name.clone(),
                    kind,
                    range: function
                        .param_ranges
                        .get(required + index)
                        .copied()
                        .flatten(),
                    default: keyword.default,
                })
            })
            .collect::<Result<Vec<_>, LowerError>>()?;
        for param in &mut call_params[required..] {
            *param = ValueKind::Any;
        }
        let sites = function
            .params
            .iter()
            .map(|param| {
                function
                    .entry_checks
                    .iter()
                    .find(|check| check.param == *param)
                    .map(|check| check.site)
            })
            .collect();
        Ok(KnownCallableThunk {
            id,
            name: format!("{}#closure", function.name),
            callable: sem::KnownCallable::DirectFunction(function.id),
            target: self.lower_function_id(function.id)?,
            sig,
            call_params,
            param_ranges: function.param_ranges.clone(),
            entry_sites: Some((function.definition, sites)),
            rets: self.value_kinds(&function.result_kinds)?,
            keywords,
            span: function.span,
        })
    }

    fn generic_thunk(
        &self,
        generic: &sem::GenericFunction,
    ) -> Result<KnownCallableThunk, LowerError> {
        let id = self
            .ids
            .generic_closure_thunks
            .get(generic.id.0 as usize)
            .copied()
            .flatten()
            .ok_or_else(|| invalid_semantic("generic closure thunk id is out of range"))?;
        let sig = self
            .ids
            .generic_sigs
            .get(generic.id.0 as usize)
            .copied()
            .ok_or_else(|| invalid_semantic("generic signature id is out of range"))?;
        let required = generic.arity.saturating_sub(generic.keywords.len());
        let keywords = generic
            .keywords
            .iter()
            .enumerate()
            .map(|(index, keyword)| {
                Ok(KnownKeyword {
                    name: keyword.name.clone(),
                    kind: value_kind(
                        generic
                            .param_kinds
                            .get(required + index)
                            .copied()
                            .flatten()
                            .unwrap_or(sem::ValueKind::Any),
                    )?,
                    range: generic
                        .param_ranges
                        .get(required + index)
                        .copied()
                        .flatten(),
                    default: keyword.default,
                })
            })
            .collect::<Result<Vec<_>, LowerError>>()?;
        Ok(KnownCallableThunk {
            id,
            name: format!("{}#closure", generic.name),
            callable: sem::KnownCallable::GenericFunction(generic.id),
            target: self.lower_generic_id(generic.id)?,
            sig,
            call_params: (0..generic.arity)
                .map(|index| {
                    if index < required {
                        dispatcher_param_kind(self.module, generic, index)
                    } else {
                        ValueKind::Any
                    }
                })
                .collect(),
            param_ranges: generic.param_ranges.clone(),
            entry_sites: None,
            rets: self.value_kinds(&generic.result_kinds)?,
            keywords,
            span: generic.span,
        })
    }

    fn accessor_thunk(&self, accessor: AccessorFunction) -> Result<KnownCallableThunk, LowerError> {
        let class = self
            .module
            .classes
            .get(accessor.class.0 as usize)
            .ok_or_else(|| invalid_semantic("accessor class id is out of range"))?;
        let slot = class
            .effective_slots
            .get(accessor.slot.0 as usize)
            .ok_or_else(|| invalid_semantic("accessor slot id is out of range"))?;
        // An accessor has no body numbering sites, so its receiver check
        // always stays.
        Ok(KnownCallableThunk {
            id: accessor.closure_thunk,
            name: format!("{}#closure", slot.accessor.name),
            callable: sem::KnownCallable::SlotAccessor {
                accessor: accessor.accessor,
                class: accessor.class,
                slot: accessor.slot,
            },
            target: accessor.function,
            sig: accessor.sig,
            call_params: vec![ValueKind::Object(Some(accessor.class))],
            param_ranges: vec![None],
            entry_sites: None,
            rets: vec![self.value_kind(slot.kind)?],
            keywords: Vec::new(),
            span: slot.span,
        })
    }

    fn lower_function_closure_thunk(
        &self,
        function: &sem::Function,
        checks: &mut Vec<CheckCodegenRecord>,
    ) -> Result<Function, LowerError> {
        self.lower_known_callable_thunk(&self.function_thunk(function)?, checks)
    }

    fn lower_function_boxed_thunk(
        &self,
        function: &sem::Function,
        boxed: FunctionId,
    ) -> Result<Function, LowerError> {
        self.lower_known_callable_boxed_thunk(&self.function_thunk(function)?, boxed)
    }

    fn lower_generic_closure_thunk(
        &self,
        generic: &sem::GenericFunction,
    ) -> Result<Function, LowerError> {
        self.lower_known_callable_thunk(&self.generic_thunk(generic)?, &mut Vec::new())
    }

    fn lower_generic_boxed_thunk(
        &self,
        generic: &sem::GenericFunction,
        boxed: FunctionId,
    ) -> Result<Function, LowerError> {
        self.lower_known_callable_boxed_thunk(&self.generic_thunk(generic)?, boxed)
    }

    fn lower_accessor_closure_thunk(
        &self,
        accessor: AccessorFunction,
    ) -> Result<Function, LowerError> {
        self.lower_known_callable_thunk(&self.accessor_thunk(accessor)?, &mut Vec::new())
    }

    fn lower_accessor_boxed_thunk(
        &self,
        accessor: AccessorFunction,
    ) -> Result<Function, LowerError> {
        self.lower_known_callable_boxed_thunk(&self.accessor_thunk(accessor)?, accessor.boxed_thunk)
    }

    /// Lowers the typed entry of the callable values of a function, generic
    /// or slot accessor: it takes the arguments at the target's own kinds,
    /// checks the parameter types the target relies on that a typed
    /// caller's register types do not establish, and calls the target.
    fn lower_known_callable_thunk(
        &self,
        spec: &KnownCallableThunk,
        checks: &mut Vec<CheckCodegenRecord>,
    ) -> Result<Function, LowerError> {
        if !spec.keywords.is_empty() {
            return self.lower_keyword_thunk(spec, checks);
        }
        let mut params = Vec::with_capacity(spec.call_params.len() + 1);
        params.push(Param {
            source: None,
            name: "$env".into(),
            kind: ValueKind::Aggregate,
            reg: register_at(0),
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
                    reg: register_at(index + 1),
                }),
        );
        let mut call_args = (0..spec.call_params.len())
            .map(|index| register_at(index + 1))
            .collect::<Vec<_>>();
        let results = spec
            .rets
            .iter()
            .enumerate()
            .map(|(index, _)| register_at(spec.call_params.len() + 1 + index))
            .collect::<Vec<_>>();
        let ret = spec.rets.first().copied().unwrap_or(ValueKind::Any);
        let mut register_kinds = params
            .iter()
            .map(|param| param.kind)
            .chain(spec.rets.iter().copied())
            .collect::<Vec<_>>();
        // The thunk is the callable's indirect entry, so it checks the
        // parameter types its target relies on, unless typing proved one.
        // A generic's dispatch already checks every argument's class, but
        // not a limited parameter's range, which its sends check.
        let mut instructions = Vec::new();
        let mut next_label = 0;
        let sends = matches!(spec.callable, sem::KnownCallable::GenericFunction(_));
        for (index, (arg, kind)) in call_args.iter_mut().zip(&spec.call_params).enumerate() {
            let target = match spec.param_ranges.get(index).copied().flatten() {
                Some(range) if *kind == ValueKind::I64 => sem::EntryTarget::Range(range),
                _ if sends => continue,
                _ => match entry_target(*kind) {
                    Some(target) => target,
                    None => continue,
                },
            };
            if let Some((owner, sites)) = &spec.entry_sites
                && let Some(site) = sites.get(index).copied().flatten()
                && self
                    .facts
                    .decision(Some(*owner), site)
                    .is_some_and(|decision| decision.verdict == typing::TypingVerdict::Proved)
            {
                checks.push(CheckCodegenRecord {
                    owner: *owner,
                    site,
                    codegen: CheckCodegen::Elided,
                });
                continue;
            }
            let site = match &spec.entry_sites {
                Some((owner, sites)) => {
                    check_site_name(Some(*owner), sites.get(index).copied().flatten(), spec.span)
                }
                None => entry_site_name(&spec.name, index),
            };
            let mut code = CodeBuffer {
                register_kinds: &mut register_kinds,
                next_label: &mut next_label,
                instructions: &mut instructions,
            };
            *arg = entry_check(&mut code, target, *arg, *kind, site, spec.span);
        }
        let call = match spec.callable {
            // A first-class generic sends like a call site.
            sem::KnownCallable::GenericFunction(generic) => {
                let callee = register_at(register_kinds.len());
                register_kinds.push(ValueKind::Function(None));
                vec![
                    Instr::SelectMethod {
                        dst: callee,
                        generic,
                        args: call_args.clone(),
                        span: spec.span,
                    },
                    Instr::CallIndirect {
                        dsts: results.clone(),
                        callee,
                        args: call_args,
                        arg_kinds: spec.call_params.clone(),
                        rets: spec.rets.clone(),
                        span: spec.span,
                    },
                ]
            }
            _ => vec![Instr::Call {
                dsts: results.clone(),
                callee: spec.target,
                args: call_args,
                span: spec.span,
            }],
        };
        instructions.extend(call);
        instructions.push(Instr::Return {
            values: results,
            span: spec.span,
        });
        Ok(Function {
            id: spec.id,
            name: spec.name.clone(),
            kind: FunctionKind::CallableThunk {
                callable: spec.callable,
            },
            register_kinds,
            params,
            ret,
            rets: spec.rets.clone(),
            locals: Vec::new(),
            instructions,
            span: spec.span,
        })
    }

    /// Lowers the typed entry of the callable values of a function or
    /// generic that takes keywords. It takes the required arguments at the
    /// target's kinds, checked as [`Self::lower_known_callable_thunk`]
    /// checks them, and each keyword boxed: the keyword-absent marker
    /// ([`RuntimeOp::KeywordAbsent`]) takes the keyword's default, from its
    /// default function called with the required arguments and the
    /// keywords before it, in declaration order, or `$unsupplied` for a
    /// keyword without one, and any other value, `$unsupplied` included,
    /// is checked and unboxed to the keyword's kind.
    /// Then it calls, or sends to, the target with every argument.
    fn lower_keyword_thunk(
        &self,
        spec: &KnownCallableThunk,
        checks: &mut Vec<CheckCodegenRecord>,
    ) -> Result<Function, LowerError> {
        let span = spec.span;
        let mut body = BodyLowering::new(self.module, &self.ids, &[]);
        let _env = body.push_param("$env", ValueKind::Aggregate);
        let required = spec.call_params.len().saturating_sub(spec.keywords.len());
        // Every parameter first, so they are the function's first registers.
        let params = spec.call_params[..required]
            .iter()
            .enumerate()
            .map(|(index, kind)| body.push_param(&format!("arg{index}"), *kind))
            .collect::<Vec<_>>();
        let boxed = spec
            .keywords
            .iter()
            .enumerate()
            .map(|(index, keyword)| {
                body.push_param(&format!("{}#{index}", keyword.name), ValueKind::Any)
            })
            .collect::<Vec<_>>();
        // A check typing proved is dropped, as the plain typed entry drops
        // it; the others are named by their typing sites.
        let site_of = |index: usize| -> (Option<(DefinitionId, sem::SiteId)>, String) {
            match &spec.entry_sites {
                Some((owner, sites)) => {
                    let site = sites.get(index).copied().flatten();
                    (
                        site.map(|site| (*owner, site)),
                        check_site_name(Some(*owner), site, span),
                    )
                }
                None => (None, entry_site_name(&spec.name, index)),
            }
        };
        let proved = |site: Option<(DefinitionId, sem::SiteId)>| {
            site.is_some_and(|(owner, site)| {
                self.facts
                    .decision(Some(owner), site)
                    .is_some_and(|decision| decision.verdict == typing::TypingVerdict::Proved)
            })
        };
        let mut args = Vec::with_capacity(spec.call_params.len());
        let sends = matches!(spec.callable, sem::KnownCallable::GenericFunction(_));
        for (index, (param, kind)) in params.into_iter().zip(&spec.call_params).enumerate() {
            let target = match spec.param_ranges.get(index).copied().flatten() {
                Some(range) if *kind == ValueKind::I64 => Some(sem::EntryTarget::Range(range)),
                _ if sends => None,
                _ => entry_target(*kind),
            };
            let Some(target) = target else {
                args.push(param);
                continue;
            };
            let (site, name) = site_of(index);
            if proved(site) {
                if let Some((owner, site)) = site {
                    checks.push(CheckCodegenRecord {
                        owner,
                        site,
                        codegen: CheckCodegen::Elided,
                    });
                }
                args.push(param);
                continue;
            }
            let checked = entry_check(&mut body.code(), target, param.reg, *kind, name, span);
            body.next_reg = register_at(body.register_kinds.len());
            args.push(Value {
                reg: checked,
                ..param
            });
        }
        for (index, (keyword, value)) in spec.keywords.iter().zip(boxed).enumerate() {
            let result = body.alloc(keyword.kind);
            let is_absent = body.alloc(ValueKind::Bool);
            let (absent, given, join) =
                (body.alloc_label(), body.alloc_label(), body.alloc_label());
            body.instructions.extend([
                Instr::RuntimeCall {
                    dst: is_absent.reg,
                    op: RuntimeOp::IsKeywordAbsent,
                    args: vec![value.reg],
                    kind: ValueKind::Bool,
                    span,
                },
                Instr::Branch {
                    cond: is_absent.reg,
                    then_label: absent,
                    else_label: given,
                    span,
                },
                Instr::Label { label: given },
            ]);
            let (_, site) = site_of(required + index);
            let checked = body.unwire(value, keyword.kind, keyword.range, &site, span)?;
            body.instructions.extend([
                Instr::Move {
                    dst: result.reg,
                    src: checked.reg,
                    span,
                },
                Instr::Jump { label: join, span },
                Instr::Label { label: absent },
            ]);
            let default = match keyword.default {
                Some(function) => {
                    let callee = self
                        .module
                        .functions
                        .get(function.0 as usize)
                        .ok_or_else(|| invalid_semantic("keyword default id is out of range"))?;
                    let mut default_args = Vec::with_capacity(callee.param_kinds.len());
                    for (arg, kind) in args.iter().zip(&callee.param_kinds) {
                        let kind = self.value_kind(*kind)?;
                        default_args.push(body.coerce_value(*arg, kind, span)?.reg);
                    }
                    let default = body.alloc(
                        self.value_kind(
                            callee
                                .result_kinds
                                .first()
                                .copied()
                                .unwrap_or(sem::ValueKind::Any),
                        )?,
                    );
                    body.instructions.push(Instr::Call {
                        dsts: vec![default.reg],
                        callee: self.lower_function_id(function)?,
                        args: default_args,
                        span,
                    });
                    match keyword.kind {
                        // The default returns an untyped callable, which
                        // enters the keyword's signature here.
                        ValueKind::Closure(Some(sig)) if default.kind != keyword.kind => {
                            body.adapt(default, sig, span)?
                        }
                        kind => body.coerce_value(default, kind, span)?,
                    }
                }
                // A keyword without a default is an `<object>`, which takes
                // `$unsupplied`.
                None => {
                    let unsupplied = body.alloc(ValueKind::Any);
                    body.instructions.push(Instr::RuntimeCall {
                        dst: unsupplied.reg,
                        op: RuntimeOp::Unsupplied,
                        args: Vec::new(),
                        kind: ValueKind::Any,
                        span,
                    });
                    body.coerce_value(unsupplied, keyword.kind, span)?
                }
            };
            body.instructions.extend([
                Instr::Move {
                    dst: result.reg,
                    src: default.reg,
                    span,
                },
                Instr::Jump { label: join, span },
                Instr::Label { label: join },
            ]);
            args.push(result);
        }
        let results = spec
            .rets
            .iter()
            .map(|kind| body.alloc(*kind))
            .collect::<Vec<_>>();
        let dsts = results.iter().map(|result| result.reg).collect::<Vec<_>>();
        match spec.callable {
            sem::KnownCallable::GenericFunction(generic) => {
                let generic = self
                    .module
                    .generics
                    .get(generic.0 as usize)
                    .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
                let mut sent = Vec::with_capacity(args.len());
                for (index, arg) in args.into_iter().enumerate() {
                    let kind = dispatcher_param_kind(self.module, generic, index);
                    sent.push(body.coerce_value(arg, kind, span)?);
                }
                body.send(generic, sent, dsts, span, None)?;
            }
            _ => body.instructions.push(Instr::Call {
                dsts,
                callee: spec.target,
                args: args.iter().map(|arg| arg.reg).collect(),
                span,
            }),
        }
        body.instructions.push(Instr::Return {
            values: results.iter().map(|result| result.reg).collect(),
            span,
        });
        Ok(Function {
            id: spec.id,
            name: spec.name.clone(),
            kind: FunctionKind::CallableThunk {
                callable: spec.callable,
            },
            register_kinds: body.register_kinds,
            params: body.params,
            ret: spec.rets.first().copied().unwrap_or(ValueKind::Any),
            rets: spec.rets.clone(),
            locals: Vec::new(),
            instructions: body.instructions,
            span,
        })
    }

    /// Lowers the boxed entry of the callable values of a function, generic
    /// or slot accessor: under the boxed callable ABI ([`BOXED_WIRE`]),
    /// it checks and unboxes each argument as the target takes it
    /// ([`BodyLowering::unwire`]), adapting a typed callable to its
    /// signature, calls the target, and boxes the results.
    ///
    /// A generic's dispatch already checks the class of an argument it
    /// dispatches on, and its methods those of the runtime objects they
    /// take, so an argument the dispatcher takes as a runtime object stays
    /// boxed for [`BodyLowering::send`], which signals
    /// `<no-applicable-method>` when the box holds no object, as a send
    /// does.
    fn lower_known_callable_boxed_thunk(
        &self,
        spec: &KnownCallableThunk,
        id: FunctionId,
    ) -> Result<Function, LowerError> {
        let mut body = BodyLowering::new(self.module, &self.ids, &[]);
        let mut call_params = self.adapted_params(&spec.call_params, spec.sig);
        // Keywords come in the tail, not as positional wires.
        call_params.truncate(call_params.len().saturating_sub(spec.keywords.len()));
        let env = body.push_param("$env", ValueKind::Aggregate);
        let wires = call_params
            .iter()
            .enumerate()
            .map(|(index, _)| body.push_param(&format!("arg{index}"), BOXED_WIRE))
            .collect::<Vec<_>>();
        let (keyword_names, keyword_values) = body.push_keyword_tail();
        if spec.keywords.is_empty() {
            body.reject_keywords(keyword_names, keyword_values, spec.span)?;
        }
        let generic = match spec.callable {
            sem::KnownCallable::GenericFunction(generic) => Some(
                self.module
                    .generics
                    .get(generic.0 as usize)
                    .ok_or_else(|| invalid_semantic("generic id is out of range"))?,
            ),
            _ => None,
        };
        let mut args = Vec::with_capacity(wires.len());
        for (index, (wire, kind)) in wires.into_iter().zip(&call_params).enumerate() {
            if let Some(generic) = generic
                && match kind {
                    ValueKind::Object(None) => true,
                    ValueKind::Object(Some(_)) => generic.dispatch_params.contains(&index),
                    _ => false,
                }
            {
                args.push(wire);
                continue;
            }
            let site = match &spec.entry_sites {
                Some((owner, sites)) => {
                    check_site_name(Some(*owner), sites.get(index).copied().flatten(), spec.span)
                }
                None => entry_site_name(&spec.name, index),
            };
            let range = spec.param_ranges.get(index).copied().flatten();
            args.push(body.unwire(wire, *kind, range, &site, spec.span)?);
        }
        let results = spec
            .rets
            .iter()
            .map(|kind| body.alloc(*kind))
            .collect::<Vec<_>>();
        let dsts = results.iter().map(|result| result.reg).collect::<Vec<_>>();
        if spec.keywords.is_empty() {
            match generic {
                // A first-class generic sends like a call site.
                Some(generic) => body.send(generic, args, dsts, spec.span, None)?,
                None => body.instructions.push(Instr::Call {
                    dsts,
                    callee: spec.target,
                    args: args.iter().map(|arg| arg.reg).collect(),
                    span: spec.span,
                }),
            }
        } else {
            // The typed entry fills the keywords' defaults and checks them.
            let names = spec
                .keywords
                .iter()
                .map(|keyword| keyword.name.clone())
                .collect::<Vec<_>>();
            let keywords = body.parse_keywords(keyword_names, keyword_values, &names, spec.span)?;
            let mut call_args = vec![env.reg];
            for (index, (arg, kind)) in args.into_iter().zip(&spec.call_params).enumerate() {
                // A generic's object-handle parameter stays boxed above,
                // for dispatch; the typed entry takes the object.
                let arg = if arg.kind == BOXED_WIRE && matches!(kind, ValueKind::Object(None)) {
                    let site = entry_site_name(&spec.name, index);
                    let object = ValueKind::Object(Some(sem::OBJECT_CLASS_ID));
                    body.unwire(arg, object, None, &site, spec.span)?
                } else {
                    arg
                };
                call_args.push(body.coerce_value(arg, *kind, spec.span)?.reg);
            }
            call_args.extend(keywords.iter().map(|keyword| keyword.reg));
            body.instructions.push(Instr::Call {
                dsts,
                callee: spec.id,
                args: call_args,
                span: spec.span,
            });
        }
        let values = results
            .into_iter()
            .map(|result| body.wire_value(result, spec.span).map(|value| value.reg))
            .collect::<Result<Vec<_>, _>>()?;
        body.instructions.push(Instr::Return {
            values,
            span: spec.span,
        });
        let rets = vec![BOXED_WIRE; spec.rets.len()];
        Ok(Function {
            id,
            name: format!("{}#boxed", spec.name),
            kind: FunctionKind::BoxedEntry { typed: spec.id },
            register_kinds: body.register_kinds,
            params: body.params,
            ret: rets.first().copied().unwrap_or(ValueKind::Any),
            rets,
            locals: Vec::new(),
            instructions: body.instructions,
            span: spec.span,
        })
    }

    /// `params`, with each typed callable `sig` takes in place of the
    /// untyped one, so a boxed entry adapts it.
    fn adapted_params(&self, params: &[ValueKind], sig: CallableSigId) -> Vec<ValueKind> {
        let declared = self
            .ids
            .call_sigs
            .get(sig.0 as usize)
            .map_or(&[][..], |signature| signature.params.as_slice());
        params
            .iter()
            .enumerate()
            .map(|(index, kind)| match declared.get(index) {
                Some(typed @ ValueKind::Closure(Some(_))) => *typed,
                _ => *kind,
            })
            .collect()
    }

    fn lower_closure(
        &self,
        closure: &sem::Closure,
        checks: &mut Vec<CheckCodegenRecord>,
    ) -> Result<Function, LowerError> {
        let owner = self
            .closure_owners
            .get(closure.id.0 as usize)
            .copied()
            .flatten();
        let mut body =
            BodyLowering::for_closure(self.module, &self.ids, closure)?.typed(self.facts, owner);
        body.check_closure_entry(closure)?;
        let rets = self.value_kinds(&closure.result_kinds)?;
        let ret = rets.first().copied().unwrap_or(ValueKind::Any);
        body.lower_block(&closure.body, &rets)?;
        checks.append(&mut body.checks);
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

    /// Lowers the boxed entry of a closure's callable values: under the
    /// boxed callable ABI, it checks and unboxes each argument at the
    /// closure's entry check sites ([`BodyLowering::unwire`]), adapting a
    /// typed callable to its signature, calls the closure body with the
    /// same environment, and boxes the results.
    fn lower_closure_boxed(
        &self,
        closure: &sem::Closure,
        id: FunctionId,
    ) -> Result<Function, LowerError> {
        let owner = self
            .closure_owners
            .get(closure.id.0 as usize)
            .copied()
            .flatten();
        let typed = self.lower_closure_id(closure.id)?;
        let sig = self
            .ids
            .closure_sigs
            .get(closure.id.0 as usize)
            .and_then(|sig| self.ids.call_sigs.get(sig.0 as usize))
            .ok_or_else(|| invalid_semantic("closure signature id is out of range"))?;
        let mut body = BodyLowering::new(self.module, &self.ids, &[]).typed(self.facts, owner);
        let env = body.push_param("$env", ValueKind::Aggregate);
        let wires = sig
            .params
            .iter()
            .enumerate()
            .map(|(index, _)| body.push_param(&format!("arg{index}"), BOXED_WIRE))
            .collect::<Vec<_>>();
        let (keywords, keyword_values) = body.push_keyword_tail();
        body.reject_keywords(keywords, keyword_values, closure.span)?;
        let mut args = vec![env.reg];
        for (index, (wire, kind)) in wires.into_iter().zip(&sig.params).enumerate() {
            let site = closure
                .params
                .get(index)
                .and_then(|param| {
                    closure
                        .entry_checks
                        .iter()
                        .find(|check| check.param == *param)
                })
                .map(|check| check.site);
            let name = match (owner, site) {
                (Some(_), Some(_)) => check_site_name(owner, site, closure.span),
                _ => entry_site_name(&format!("closure#{}", closure.id.0), index),
            };
            let range = closure.param_ranges.get(index).copied().flatten();
            args.push(body.unwire(wire, *kind, range, &name, closure.span)?.reg);
        }
        let results = sig
            .rets
            .iter()
            .map(|kind| body.alloc(*kind))
            .collect::<Vec<_>>();
        body.instructions.push(Instr::Call {
            dsts: results.iter().map(|result| result.reg).collect(),
            callee: typed,
            args,
            span: closure.span,
        });
        let values = results
            .into_iter()
            .map(|result| body.wire_value(result, closure.span).map(|value| value.reg))
            .collect::<Result<Vec<_>, _>>()?;
        body.instructions.push(Instr::Return {
            values,
            span: closure.span,
        });
        let rets = vec![BOXED_WIRE; sig.rets.len()];
        Ok(Function {
            id,
            name: format!("closure#{}#boxed", closure.id.0),
            kind: FunctionKind::BoxedEntry { typed },
            register_kinds: body.register_kinds,
            params: body.params,
            ret: rets.first().copied().unwrap_or(ValueKind::Any),
            rets,
            locals: Vec::new(),
            instructions: body.instructions,
            span: closure.span,
        })
    }

    /// Lowers the adapter that gives a callable value from untyped code the
    /// typed entry `signature` promises ([`BodyLowering::adapt`]): it boxes
    /// the arguments, calls the value's boxed entry, which its environment
    /// holds, and checks and unboxes each result to the signature's kind,
    /// adapting a typed callable result in turn. A result of another kind
    /// signals `type-error.entry` at `entry:<adapter>:<index>`; a class
    /// result is only checked to be an object, as callers cast it.
    fn lower_adapter(
        &self,
        signature: CallableSigId,
        id: FunctionId,
    ) -> Result<Function, LowerError> {
        let sig = self
            .ids
            .call_sigs
            .get(signature.0 as usize)
            .ok_or_else(|| invalid_semantic("adapter signature id is out of range"))?;
        let span = Span::new(0, 0);
        let name = signature_label(self.module, &self.ids.call_sigs, signature);
        let mut body = BodyLowering::new(self.module, &self.ids, &[]);
        let env = body.push_param("$env", ValueKind::Aggregate);
        let params = sig
            .params
            .iter()
            .enumerate()
            .map(|(index, kind)| body.push_param(&format!("arg{index}"), *kind))
            .collect::<Vec<_>>();
        let boxed = body.alloc(ValueKind::Entry);
        body.instructions.push(Instr::CaptureGet {
            dst: boxed.reg,
            env: env.reg,
            index: 0,
            kind: ValueKind::Entry,
            span,
        });
        // The keywords, `$unsupplied` for those the call does not pass, go
        // in the tail, which the value's boxed entry reads.
        let mut params = params;
        let keyword_values = params.split_off(params.len().saturating_sub(sig.keywords.len()));
        let mut wired = params
            .into_iter()
            .map(|param| body.wire_value(param, span))
            .collect::<Result<Vec<_>, _>>()?;
        let names = sig
            .keywords
            .iter()
            .map(|keyword| keyword.name.clone())
            .collect::<Vec<_>>();
        let (tail_names, tail_values) = body.keyword_tail(&names, &keyword_values, span);
        wired.extend([tail_names, tail_values]);
        let rets = vec![BOXED_WIRE; sig.rets.len()];
        let results = rets
            .iter()
            .map(|kind| body.alloc(*kind))
            .collect::<Vec<_>>();
        body.instructions.push(Instr::CallIndirect {
            dsts: results.iter().map(|result| result.reg).collect(),
            callee: boxed.reg,
            args: wired.iter().map(|arg| arg.reg).collect(),
            arg_kinds: wired.iter().map(|arg| arg.kind).collect(),
            rets,
            span,
        });
        // A class result need only be an object here: typed callers cast
        // the class results they rely on themselves, at sites typing
        // classifies.
        let mut values = Vec::with_capacity(results.len());
        for (index, (result, kind)) in results.into_iter().zip(&sig.rets).enumerate() {
            let site = entry_site_name(&name, index);
            let kind = match kind {
                ValueKind::Object(Some(_)) => ValueKind::Object(Some(sem::OBJECT_CLASS_ID)),
                kind => *kind,
            };
            values.push(body.unwire(result, kind, None, &site, span)?.reg);
        }
        body.instructions.push(Instr::Return { values, span });
        Ok(Function {
            id,
            name,
            kind: FunctionKind::Adapter { signature },
            register_kinds: body.register_kinds,
            params: body.params,
            ret: sig.rets.first().copied().unwrap_or(ValueKind::Any),
            rets: sig.rets.clone(),
            locals: Vec::new(),
            instructions: body.instructions,
            span,
        })
    }

    /// Lowers [`FunctionKind::CallableParts`].
    fn lower_callable_parts(&self) -> Function {
        let span = Span::new(0, 0);
        let mut body = BodyLowering::new(self.module, &self.ids, &[]);
        let callable = body.push_param("callable", ValueKind::Closure(None));
        let parts = [ValueKind::Entry, ValueKind::Entry, ValueKind::U64]
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                let part = body.alloc(kind);
                body.instructions.push(Instr::CaptureGet {
                    dst: part.reg,
                    env: callable.reg,
                    index,
                    kind,
                    span,
                });
                part.reg
            })
            .collect::<Vec<_>>();
        body.instructions.push(Instr::Return {
            values: parts,
            span,
        });
        Function {
            id: self.ids.callable_parts.unwrap_or(FunctionId(u32::MAX)),
            name: String::from("callable#parts"),
            kind: FunctionKind::CallableParts,
            register_kinds: body.register_kinds,
            params: body.params,
            ret: ValueKind::Entry,
            rets: vec![ValueKind::Entry, ValueKind::Entry, ValueKind::U64],
            locals: Vec::new(),
            instructions: body.instructions,
            span,
        }
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
        uses.locals = &constant.locals;
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
    block.for_each_expr(|expr| collect_known_callable_uses_in_expr(expr, uses));
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
        | sem::Expr::InstanceOfLimited {
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
        sem::Expr::MakeRuntimeClass {
            class,
            init_args,
            candidates,
            ..
        } => {
            collect_known_callable_uses_in_expr(class, uses);
            for arg in init_args {
                collect_known_callable_uses_in_expr(&arg.value, uses);
            }
            for field in candidates.iter().flat_map(|candidate| &candidate.defaults) {
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
        sem::Expr::AssignLocal { value, .. } | sem::Expr::AssignCapture { value, .. } => {
            collect_known_callable_uses_in_expr(value, uses);
        }
        sem::Expr::Loop { test, body, .. } => {
            collect_known_callable_uses_in_expr(test, uses);
            collect_known_callable_uses_in_block(body, uses);
        }
        sem::Expr::Negate { value, .. }
        | sem::Expr::Convert { value, .. }
        | sem::Expr::CheckRange { value, .. } => {
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
        | sem::Expr::ValueEqual { lhs, rhs, .. }
        | sem::Expr::Identical { lhs, rhs, .. } => {
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
        sem::Expr::Discard { value, .. } => collect_known_callable_uses_in_expr(value, uses),
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
        | sem::Expr::Const32 { .. }
        | sem::Expr::ConstBool { .. }
        | sem::Expr::StringLiteral { .. }
        | sem::Expr::SymbolLiteral { .. }
        | sem::Expr::Local { .. }
        | sem::Expr::Capture { .. }
        | sem::Expr::ConstantRef { .. }
        | sem::Expr::ClassObject { .. }
        | sem::Expr::TypeObject { .. }
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
    Ok(CallableSignature {
        params,
        rets,
        keywords: Vec::new(),
    })
}

/// `signature`, whose last `names.len()` parameters are `#key` parameters
/// named `names`, with those taken as boxed values and recorded as its
/// keywords, at their declared kinds.
fn with_keywords(mut signature: CallableSignature, names: &[String]) -> CallableSignature {
    let required = signature.params.len().saturating_sub(names.len());
    let kinds = signature.params.split_off(required);
    signature
        .params
        .extend(kinds.iter().map(|_| ValueKind::Any));
    signature.keywords = names
        .iter()
        .zip(kinds)
        .map(|(name, kind)| CallableKeyword {
            name: name.clone(),
            kind,
        })
        .collect();
    signature
}

/// The names of `keywords`.
fn keyword_names(keywords: &[sem::KeywordParam]) -> Vec<String> {
    keywords
        .iter()
        .map(|keyword| keyword.name.clone())
        .collect()
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
    let mut lowered = callable_signature_from_sem(
        signatures,
        &signature.params,
        &signature.param_sigs,
        &signature.results,
        &signature.result_sigs,
    )?;
    lowered
        .params
        .extend(signature.keywords.iter().map(|_| ValueKind::Any));
    lowered.keywords = lowered_keywords(&signature.keywords)?;
    Ok(intern_callable_signature(signatures, lowered))
}

/// `keywords` at their lowered kinds.
fn lowered_keywords(keywords: &[sem::CallableKeyword]) -> Result<Vec<CallableKeyword>, LowerError> {
    keywords
        .iter()
        .map(|keyword| {
            Ok(CallableKeyword {
                name: keyword.name.clone(),
                kind: value_kind(keyword.kind)?,
            })
        })
        .collect()
}

/// Returns the id [`intern_sem_callable_signature`] gave `signature`; every
/// signature sem records is interned before bodies are lowered, by
/// [`intern_recorded_signatures`].
fn sem_callable_signature_id(
    ids: &LowerIds,
    signature: &sem::CallableSignature,
) -> Result<CallableSigId, LowerError> {
    let kind = |kind: sem::ValueKind, nested: Option<&sem::CallableSignature>| match nested {
        Some(nested) => {
            sem_callable_signature_id(ids, nested).map(|sig| ValueKind::Closure(Some(sig)))
        }
        None => value_kind(kind),
    };
    let lowered = CallableSignature {
        params: signature
            .params
            .iter()
            .enumerate()
            .map(|(index, param)| kind(*param, signature.param_sig(index)))
            .chain(signature.keywords.iter().map(|_| Ok(ValueKind::Any)))
            .collect::<Result<Vec<_>, _>>()?,
        rets: signature
            .results
            .iter()
            .enumerate()
            .map(|(index, result)| kind(*result, signature.result_sig(index)))
            .collect::<Result<Vec<_>, _>>()?,
        keywords: lowered_keywords(&signature.keywords)?,
    };
    ids.call_sigs
        .iter()
        .position(|candidate| *candidate == lowered)
        .map(|index| CallableSigId(u32::try_from(index).unwrap_or(u32::MAX)))
        .ok_or_else(|| invalid_semantic("callable signature id is missing"))
}

/// Interns every callable signature sem records on a value lowering may
/// type by it: locals, captures, constants and slots, beside the function
/// and closure signatures interned with their code. Bodies then only look
/// signatures up.
fn intern_recorded_signatures(
    signatures: &mut Vec<CallableSignature>,
    module: &sem::Module,
) -> Result<(), LowerError> {
    let locals = module
        .functions
        .iter()
        .flat_map(|function| &function.locals)
        .chain(module.methods.iter().flat_map(|method| &method.locals))
        .chain(module.closures.iter().flat_map(|closure| &closure.locals))
        .map(|local| local.callable_signature.as_ref());
    let captures = module
        .closures
        .iter()
        .flat_map(|closure| &closure.captures)
        .map(|capture| capture.callable_signature.as_ref());
    let constants = module
        .constants
        .iter()
        .map(|constant| constant.callable_signature.as_ref());
    let slots = module
        .classes
        .iter()
        .flat_map(|class| {
            class
                .direct_slots
                .iter()
                .map(|slot| &slot.callable_signature)
        })
        .chain(module.classes.iter().flat_map(|class| {
            class
                .effective_slots
                .iter()
                .map(|slot| &slot.callable_signature)
        }))
        .map(Option::as_ref);
    let generic_results = module
        .generics
        .iter()
        .flat_map(|generic| generic.result_callable_sigs.iter().map(Option::as_ref));
    for signature in locals
        .chain(captures)
        .chain(constants)
        .chain(slots)
        .chain(generic_results)
        .flatten()
    {
        intern_sem_callable_signature(signatures, signature)?;
    }
    Ok(())
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
        keywords: Vec::new(),
    })
}

fn generic_callable_signature(
    module: &sem::Module,
    generic: &sem::GenericFunction,
) -> Result<CallableSignature, LowerError> {
    let required = generic.arity.saturating_sub(generic.keywords.len());
    let signature = CallableSignature {
        params: (0..generic.arity)
            .map(|index| {
                if index < required {
                    Ok(dispatcher_param_kind(module, generic, index))
                } else {
                    value_kind(
                        generic
                            .param_kinds
                            .get(index)
                            .copied()
                            .flatten()
                            .unwrap_or(sem::ValueKind::Any),
                    )
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
        rets: generic
            .result_kinds
            .iter()
            .copied()
            .map(value_kind)
            .collect::<Result<Vec<_>, _>>()?,
        keywords: Vec::new(),
    };
    Ok(with_keywords(signature, &keyword_names(&generic.keywords)))
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
    reg: Reg,
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
    class_const: Reg,
    cond: Reg,
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
    next_reg: Reg,
    next_label: u32,
    /// Typing facts lowering may act on, or `None` for generated code with
    /// no check sites of its own.
    facts: Option<&'module TypingFacts<'module>>,
    /// Definition whose typing decides the sites being lowered: the body's
    /// own definition, or a constant's while its value is lowered inline.
    owner: Option<DefinitionId>,
    /// Definition whose sites the checks being lowered are, which names
    /// them in the `<type-error>`s they signal: [`Self::owner`], except in
    /// a numeric method's body inlined at a send, whose sites are the
    /// method's although its typing does not apply there.
    site_owner: Option<DefinitionId>,
    /// Sites a typing fact changed, in lowering order.
    checks: Vec<CheckCodegenRecord>,
    /// The closure whose body this is, which names its locals' and
    /// captures' typing bindings.
    closure: Option<sem::ClosureId>,
    /// Scalars unboxed from a boxed local an `instance?` test refined, with
    /// the boxed register they came from, innermost last. Boxing one of them
    /// again reuses the box it came from.
    unboxed_locals: Vec<(Reg, Reg)>,
    /// The exit procedures of the blocks whose bodies are being lowered,
    /// innermost last, by the local each is bound to: a call of one is
    /// lowered as the exit it makes ([`Self::lower_exit_call`]).
    exits: Vec<(sem::LocalId, BoundExit)>,
    /// Callable value registers the tape knows the shape of: parameters,
    /// results of calls, captures and new callable values, each written
    /// once, whose contract says so ([`Self::open_callable`]).
    proven_callables: Vec<Reg>,
    /// Join registers of callable values, with whether every value moved
    /// into them so far is one whose shape the tape knows: the tape keeps a
    /// fact every incoming path agrees on.
    callable_joins: Vec<(Reg, bool)>,
    /// Reads of assigned locals in the body being lowered that may use the
    /// local's register without copying it; see [`reads`].
    direct_reads: Vec<*const sem::Expr>,
}

impl<'module> BodyLowering<'module> {
    /// Pushes a boxed entry's keyword tail ([`BOXED_WIRE`]): the names of
    /// the keywords the call passes, as tape strings, and their values,
    /// boxed, in the same order.
    fn push_keyword_tail(&mut self) -> (Value, Value) {
        (
            self.push_param("$keywords", ValueKind::Array(ArrayKind::Str)),
            self.push_param("$keyword-values", ValueKind::Array(ArrayKind::Any)),
        )
    }

    /// Makes the keyword tail of a boxed call passing `names` with `values`,
    /// already boxed.
    fn keyword_tail(&mut self, names: &[String], values: &[Value], span: Span) -> (Value, Value) {
        let name_regs = names
            .iter()
            .map(|name| {
                let reg = self.alloc(ValueKind::Str);
                self.instructions.push(Instr::ConstStr {
                    dst: reg.reg,
                    value: name.clone(),
                    span,
                });
                reg.reg
            })
            .collect();
        let tail_names = self.alloc(ValueKind::Array(ArrayKind::Str));
        self.instructions.push(Instr::ArrayNew {
            dst: tail_names.reg,
            kind: ArrayKind::Str,
            values: name_regs,
            span,
        });
        let tail_values = self.alloc(ValueKind::Array(ArrayKind::Any));
        self.instructions.push(Instr::ArrayNew {
            dst: tail_values.reg,
            kind: ArrayKind::Any,
            values: values.iter().map(|value| value.reg).collect(),
            span,
        });
        (tail_names, tail_values)
    }

    /// Signals `<unknown-keyword-error>` for the keyword the tape string
    /// `name` names; a handler recovers at its own `handle`, so the call
    /// does not go on.
    fn signal_unknown_keyword(&mut self, name: Reg, span: Span) -> Result<(), LowerError> {
        let class = self
            .module
            .condition_class(sem::ConditionClass::UnknownKeywordError)
            .ok_or_else(|| invalid_semantic("the <unknown-keyword-error> class is missing"))?;
        let slot = self
            .module
            .classes
            .get(class.0 as usize)
            .and_then(|class| class.effective_slots.first())
            .ok_or_else(|| invalid_semantic("<unknown-keyword-error> has no keyword slot"))?;
        let slot_kind = value_kind(slot.kind)?;
        let symbol = self.alloc(slot_kind);
        self.instructions.push(Instr::RuntimeCall {
            dst: symbol.reg,
            op: RuntimeOp::InternSymbol,
            args: vec![name],
            kind: slot_kind,
            span,
        });
        let condition = self.alloc(ValueKind::Object(Some(class)));
        self.instructions.push(Instr::MakeObject {
            dst: condition.reg,
            class,
            fields: vec![symbol.reg],
            span,
        });
        self.instructions.push(Instr::Signal {
            dsts: Vec::new(),
            condition: condition.reg,
            span,
        });
        Ok(())
    }

    /// Signals `<unknown-keyword-error>` for each keyword a boxed call
    /// passes to a callable that takes none.
    fn reject_keywords(
        &mut self,
        names: Value,
        values: Value,
        span: Span,
    ) -> Result<(), LowerError> {
        self.parse_keywords(names, values, &[], span).map(|_| ())
    }

    /// Reads a boxed call's keyword tail into one boxed value per keyword
    /// in `declared`, the keyword-absent marker ([`RuntimeOp::KeywordAbsent`])
    /// for each the call does not pass, and
    /// signals `<unknown-keyword-error>` for each it passes that is not
    /// declared. A keyword passed twice takes its last value; calls that
    /// name their keywords never pass one twice.
    fn parse_keywords(
        &mut self,
        names: Value,
        values: Value,
        declared: &[String],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let slots = if declared.is_empty() {
            Vec::new()
        } else {
            let absent = self.alloc(ValueKind::Any);
            self.instructions.push(Instr::RuntimeCall {
                dst: absent.reg,
                op: RuntimeOp::KeywordAbsent,
                args: Vec::new(),
                kind: ValueKind::Any,
                span,
            });
            declared
                .iter()
                .map(|_| {
                    let slot = self.alloc(ValueKind::Any);
                    self.instructions.push(Instr::Move {
                        dst: slot.reg,
                        src: absent.reg,
                        span,
                    });
                    slot
                })
                .collect()
        };
        let constants = declared
            .iter()
            .map(|name| {
                let reg = self.alloc(ValueKind::Str);
                self.instructions.push(Instr::ConstStr {
                    dst: reg.reg,
                    value: name.clone(),
                    span,
                });
                reg
            })
            .collect::<Vec<_>>();
        let len = self.alloc(ValueKind::U64);
        let index = self.alloc(ValueKind::U64);
        let one = self.alloc(ValueKind::U64);
        let finished = self.alloc(ValueKind::Bool);
        let (head, body, next, done) = (
            self.alloc_label(),
            self.alloc_label(),
            self.alloc_label(),
            self.alloc_label(),
        );
        self.instructions.extend([
            Instr::ArrayLen {
                dst: len.reg,
                array: names.reg,
                span,
            },
            Instr::ConstU64 {
                dst: index.reg,
                value: 0,
                span,
            },
            Instr::ConstU64 {
                dst: one.reg,
                value: 1,
                span,
            },
            Instr::Jump { label: head, span },
            Instr::Label { label: head },
            Instr::EqU64 {
                dst: finished.reg,
                lhs: index.reg,
                rhs: len.reg,
                span,
            },
            Instr::Branch {
                cond: finished.reg,
                then_label: done,
                else_label: body,
                span,
            },
            Instr::Label { label: body },
        ]);
        let name = self.alloc(ValueKind::Str);
        let value = self.alloc(ValueKind::Any);
        self.instructions.extend([
            Instr::ArrayGet {
                dst: name.reg,
                array: names.reg,
                index: index.reg,
                span,
            },
            Instr::ArrayGet {
                dst: value.reg,
                array: values.reg,
                index: index.reg,
                span,
            },
        ]);
        for (slot, constant) in slots.iter().zip(&constants) {
            let same = self.alloc(ValueKind::Bool);
            let (matched, unmatched) = (self.alloc_label(), self.alloc_label());
            self.instructions.extend([
                Instr::StrEq {
                    dst: same.reg,
                    lhs: name.reg,
                    rhs: constant.reg,
                    span,
                },
                Instr::Branch {
                    cond: same.reg,
                    then_label: matched,
                    else_label: unmatched,
                    span,
                },
                Instr::Label { label: matched },
                Instr::Move {
                    dst: slot.reg,
                    src: value.reg,
                    span,
                },
                Instr::Jump { label: next, span },
                Instr::Label { label: unmatched },
            ]);
        }
        // An adapter passes every keyword of its signature, marking those
        // its caller left out: only a keyword actually passed is unknown.
        let absent = self.alloc(ValueKind::Bool);
        let unknown = self.alloc_label();
        self.instructions.extend([
            Instr::RuntimeCall {
                dst: absent.reg,
                op: RuntimeOp::IsKeywordAbsent,
                args: vec![value.reg],
                kind: ValueKind::Bool,
                span,
            },
            Instr::Branch {
                cond: absent.reg,
                then_label: next,
                else_label: unknown,
                span,
            },
            Instr::Label { label: unknown },
        ]);
        self.signal_unknown_keyword(name.reg, span)?;
        self.instructions.extend([
            Instr::Jump { label: next, span },
            Instr::Label { label: next },
            Instr::AddU64 {
                dst: index.reg,
                lhs: index.reg,
                rhs: one.reg,
                span,
            },
            Instr::Jump { label: head, span },
            Instr::Label { label: done },
        ]);
        Ok(slots)
    }

    fn for_function(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        function: &'module sem::Function,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &function.locals);
        for (index, local_id) in function.params.iter().copied().enumerate() {
            let reg = register_at(index);
            this.bind_param(function.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = register_at(function.params.len());
        Ok(this)
    }

    fn for_method(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        method: &'module sem::Method,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &method.locals);
        for (index, local_id) in method.params.iter().copied().enumerate() {
            let reg = register_at(index);
            this.bind_param(method.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = register_at(method.params.len());
        Ok(this)
    }

    fn for_closure(
        module: &'module sem::Module,
        ids: &'module LowerIds,
        closure: &'module sem::Closure,
    ) -> Result<Self, LowerError> {
        let mut this = Self::new(module, ids, &closure.locals);
        let env = Value {
            reg: register_at(0),
            kind: ValueKind::Aggregate,
            reachable: true,
        };
        this.register_kinds.push(ValueKind::Aggregate);
        this.env = Some(env);
        this.captures = &closure.captures;
        this.closure = Some(closure.id);
        this.params.push(Param {
            source: None,
            name: "$env".into(),
            kind: ValueKind::Aggregate,
            reg: env.reg,
        });
        for (index, local_id) in closure.params.iter().copied().enumerate() {
            let reg = register_at(index + 1);
            this.bind_param(closure.locals.get(local_id.0 as usize), local_id, reg)?;
        }
        this.next_reg = register_at(closure.params.len() + 1);
        Ok(this)
    }

    /// Checks a closure's parameters on entry, unless typing proved a check:
    /// a closure is only ever entered through `call.indirect`, whose caller
    /// may not know its parameter classes or ranges; see [`entry_target`].
    fn check_closure_entry(&mut self, closure: &sem::Closure) -> Result<(), LowerError> {
        for (index, local_id) in closure.params.iter().copied().enumerate() {
            let (reg, kind) = (self.params[index + 1].reg, self.params[index + 1].kind);
            let target = match closure.param_ranges.get(index).copied().flatten() {
                Some(range) if kind == ValueKind::I64 => sem::EntryTarget::Range(range),
                _ => match entry_target(kind) {
                    Some(target) => target,
                    None => continue,
                },
            };
            let site = closure
                .entry_checks
                .iter()
                .find(|check| check.param == local_id)
                .map(|check| check.site);
            if let Some(site) = site
                && self.proved(site)
            {
                self.record(site, CheckCodegen::Elided);
                continue;
            }
            let name = match (self.site_owner, site) {
                (Some(_), Some(_)) => check_site_name(self.site_owner, site, closure.span),
                _ => entry_site_name(&format!("closure#{}", closure.id.0), index),
            };
            let checked = entry_check(&mut self.code(), target, reg, kind, name, closure.span);
            self.next_reg = register_at(self.register_kinds.len());
            self.set_local(
                local_id,
                Value {
                    reg: checked,
                    kind,
                    reachable: true,
                },
            )?;
        }
        Ok(())
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
                reg: register_at(index),
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
            next_reg: register_at(generic.arity),
            next_label: 0,
            facts: None,
            owner: None,
            site_owner: None,
            checks: Vec::new(),
            closure: None,
            unboxed_locals: Vec::new(),
            exits: Vec::new(),
            proven_callables: Vec::new(),
            callable_joins: Vec::new(),
            direct_reads: Vec::new(),
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
            next_reg: register_at(0),
            next_label: 0,
            facts: None,
            owner: None,
            site_owner: None,
            checks: Vec::new(),
            closure: None,
            unboxed_locals: Vec::new(),
            exits: Vec::new(),
            proven_callables: Vec::new(),
            callable_joins: Vec::new(),
            direct_reads: Vec::new(),
        }
    }

    /// Lets this body act on `facts` about the sites `owner` decides.
    const fn typed(
        mut self,
        facts: &'module TypingFacts<'module>,
        owner: Option<DefinitionId>,
    ) -> Self {
        self.facts = Some(facts);
        self.owner = owner;
        self.site_owner = owner;
        self
    }

    /// The `<type-error>` a failing check of `kind` signals: at `site` of
    /// the current site owner, or, for a check typing does not classify, at
    /// `span` in it.
    fn type_error(
        &self,
        kind: TypeErrorKind,
        expected: TypeObject,
        site: Option<sem::SiteId>,
        span: Span,
    ) -> Box<TypeErrorSite> {
        type_error_site(
            kind,
            expected,
            check_site_name(self.site_owner, site, span),
            span,
        )
    }

    /// The typing decision for `site` in the current owner, if lowering may
    /// act on one.
    fn fact(&self, site: sem::SiteId) -> Option<&'module typing::TypingDecision> {
        self.facts?.decision(self.owner, site)
    }

    /// Whether typing proved the check at `site` passes, or that no value
    /// reaches it.
    fn proved(&self, site: sem::SiteId) -> bool {
        self.fact(site)
            .is_some_and(|decision| decision.verdict == typing::TypingVerdict::Proved)
    }

    /// The outcome typing proved for the `instance?` test at `site`, if any.
    ///
    /// A test no value reaches is left alone: either outcome would do, and
    /// dropping a branch on it gains nothing.
    fn decided_test(&self, site: sem::SiteId) -> Option<bool> {
        let decision = self.fact(site)?;
        if decision.rule == typing::TypingRule::Unreachable {
            return None;
        }
        match decision.verdict {
            typing::TypingVerdict::Proved => Some(true),
            typing::TypingVerdict::Refuted => Some(false),
            typing::TypingVerdict::Checked => None,
        }
    }

    /// The constant a decided `instance?` test at `site` becomes; its
    /// operand has already been evaluated for effect.
    fn decided_test_value(&mut self, site: sem::SiteId, outcome: bool, span: Span) -> Value {
        self.record(site, CheckCodegen::Elided);
        let result = self.alloc(ValueKind::Bool);
        self.instructions.push(Instr::ConstBool {
            dst: result.reg,
            value: outcome,
            span,
        });
        result
    }

    /// A read of a local or capture whose use gives it a narrower class
    /// than its register's, such as after an `instance?` test: the value
    /// at that class with no cast, when flow typing established the class
    /// itself. The fact describes the value read, which the register holds
    /// until the read is used: a shared local's read is a fresh copy of its
    /// cell, other captures are immutable, and an assignment that could
    /// change an assigned local's register first is read from a copy
    /// ([`reads`]).
    fn proved_read(
        &self,
        value: Value,
        expected: ValueKind,
        binding: typing::Binding,
        span: Span,
    ) -> Option<Value> {
        let ValueKind::Object(Some(class)) = expected else {
            return None;
        };
        if value.kind == expected || !matches!(value.kind, ValueKind::Object(_)) {
            return None;
        }
        let ty = self.facts?.read_type(self.owner, binding, span)?;
        let cone = interloko_model::types::Type::of(
            interloko_model::types::Atom::Cone(class),
            self.module,
        );
        ty.is_subtype(&cone, self.module).then_some(Value {
            kind: expected,
            ..value
        })
    }

    /// Records that a typing fact changed what `site` of the current owner
    /// emits.
    fn record(&mut self, site: sem::SiteId, codegen: CheckCodegen) {
        if let Some(owner) = self.owner {
            self.checks.push(CheckCodegenRecord {
                owner,
                site,
                codegen,
            });
        }
    }

    fn bind_param(
        &mut self,
        local: Option<&sem::Local>,
        local_id: sem::LocalId,
        reg: Reg,
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
        assert_eq!(register_at(self.register_kinds.len()), reg);
        self.register_kinds.push(kind);
        if matches!(kind, ValueKind::Closure(_)) {
            self.proven_callables.push(reg);
        }
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
        self.direct_reads = reads::direct_reads(block);
        self.lower_statements(&block.statements)?;
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

    /// Lowers a function or method body inside a retry region.
    ///
    /// A retry exception counts down through the retry regions it crosses.
    /// The region it reaches at zero calls the function it carries with this
    /// frame's own arguments and returns the results, so the frame's caller
    /// sees an ordinary return from the newer code. Parameters live in the
    /// input registers, which the body never writes.
    /// Opens a protected region catching `catches`, starting it at a block
    /// of its own.
    ///
    /// The tape keeps a register's facts (such as the known shape of a
    /// callable value) along an exceptional edge only when the block that
    /// raises does not write the register, since it cannot tell whether a
    /// write in that block came before the raise. A region that began in
    /// the middle of a block would share that block with code before it, so
    /// a callable made just before a `handle` or `block` would lose its
    /// shape on the way to the handler and could not be called after it.
    fn enter_region(&mut self, catches: Vec<Catch>, span: Span) {
        let start = self.alloc_label();
        self.instructions.push(Instr::Jump { label: start, span });
        self.instructions.push(Instr::Label { label: start });
        self.instructions.push(Instr::EnterRegion { catches });
    }

    fn lower_retryable_block(
        &mut self,
        block: &sem::Block,
        expected: &[ValueKind],
        span: Span,
    ) -> Result<(), LowerError> {
        let retry_label = self.alloc_label();
        self.instructions.push(Instr::EnterRegion {
            catches: vec![Catch::Tag {
                tag: ControlTag::Retry,
                target: retry_label,
            }],
        });
        self.lower_block(block, expected)?;
        self.instructions.push(Instr::LeaveRegion);

        let remaining = self.alloc(ValueKind::I64);
        let callee = self.alloc(ValueKind::Function(None));
        let exception = self.alloc(ValueKind::Exn);
        self.instructions.push(Instr::CatchTarget {
            label: retry_label,
            params: vec![remaining.reg, callee.reg, exception.reg],
        });
        let zero = self.alloc(ValueKind::I64);
        let here = self.alloc(ValueKind::Bool);
        let here_label = self.alloc_label();
        let onward_label = self.alloc_label();
        self.instructions.push(Instr::ConstI64 {
            dst: zero.reg,
            value: 0,
            span,
        });
        self.instructions.push(Instr::I64Compare {
            dst: here.reg,
            op: sem::CompareOp::Eq,
            lhs: remaining.reg,
            rhs: zero.reg,
            span,
        });
        self.instructions.push(Instr::Branch {
            cond: here.reg,
            then_label: here_label,
            else_label: onward_label,
            span,
        });

        self.instructions.push(Instr::Label {
            label: onward_label,
        });
        let one = self.alloc(ValueKind::I64);
        let next = self.alloc(ValueKind::I64);
        let overflow = self.alloc(ValueKind::Bool);
        self.instructions.push(Instr::ConstI64 {
            dst: one.reg,
            value: 1,
            span,
        });
        self.instructions.push(Instr::I64Binary {
            dst: next.reg,
            overflow: overflow.reg,
            op: I64ArithOp::Sub,
            lhs: remaining.reg,
            rhs: one.reg,
            span,
        });
        self.instructions.push(Instr::Throw {
            tag: ControlTag::Retry,
            payload: vec![next.reg, callee.reg],
            span,
        });

        self.instructions.push(Instr::Label { label: here_label });
        let args = self.params.iter().map(|param| param.reg).collect();
        let arg_kinds = self.params.iter().map(|param| param.kind).collect();
        let results = expected
            .iter()
            .map(|kind| self.alloc(*kind).reg)
            .collect::<Vec<_>>();
        self.instructions.push(Instr::CallIndirect {
            dsts: results.clone(),
            callee: callee.reg,
            args,
            arg_kinds,
            rets: expected.to_vec(),
            span,
        });
        self.instructions.push(Instr::Return {
            values: results,
            span,
        });
        Ok(())
    }

    /// Sends to a generic function: the runtime selects the method, unless
    /// typing proved which one at `site` ([`Self::send_link`]).
    ///
    /// A dispatched parameter no method specializes on a scalar class takes
    /// a runtime object, not a boxed `<object>`. An argument that is a
    /// boxed `<object>` ([`ValueKind::Any`]) is unboxed to the object it
    /// holds after every argument is evaluated; when one holds a scalar
    /// instead, no method can apply, and the send signals its
    /// `<no-applicable-method>` here with the arguments as given.
    fn send(
        &mut self,
        generic: &sem::GenericFunction,
        args: Vec<Value>,
        dsts: Vec<Reg>,
        span: Span,
        site: Option<sem::SiteId>,
    ) -> Result<(), LowerError> {
        let boxed = args
            .iter()
            .enumerate()
            .filter(|(index, arg)| {
                arg.kind == ValueKind::Any
                    && matches!(
                        dispatcher_param_kind(self.module, generic, *index),
                        ValueKind::Object(_)
                    )
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut regs = args.iter().map(|arg| arg.reg).collect::<Vec<_>>();
        if boxed.is_empty() {
            return self.send_regs(generic, regs, dsts, span, site);
        }
        let (scalar, join) = (self.alloc_label(), self.alloc_label());
        for index in boxed {
            let object = self.alloc(dispatcher_param_kind(self.module, generic, index));
            self.instructions.push(Instr::UnboxObject {
                dst: object.reg,
                value: args[index].reg,
                otherwise: scalar,
                span,
            });
            regs[index] = object.reg;
        }
        self.send_regs(generic, regs, dsts.clone(), span, site)?;
        self.instructions.push(Instr::Jump { label: join, span });
        self.instructions.push(Instr::Label { label: scalar });
        let results = self.emit_no_applicable_method(generic, &args, span)?;
        for (dst, result) in dsts.into_iter().zip(results) {
            self.instructions.push(Instr::Move {
                dst,
                src: result,
                span,
            });
        }
        self.instructions.push(Instr::Jump { label: join, span });
        self.instructions.push(Instr::Label { label: join });
        Ok(())
    }

    /// Sends `args`, already of the kinds the dispatcher takes.
    fn send_regs(
        &mut self,
        generic: &sem::GenericFunction,
        args: Vec<Reg>,
        dsts: Vec<Reg>,
        span: Span,
        site: Option<sem::SiteId>,
    ) -> Result<(), LowerError> {
        if let Some(site) = site
            && let Some((method, callee)) = self.send_link(generic, site)
        {
            self.record(site, CheckCodegen::Linked);
            self.instructions.push(Instr::LinkedSend {
                dsts,
                method,
                callee,
                args,
                span,
            });
            return Ok(());
        }
        let callee = self.alloc(ValueKind::Function(None));
        self.instructions.push(Instr::SelectMethod {
            dst: callee.reg,
            generic: generic.id,
            args: args.clone(),
            span,
        });
        let arg_kinds = (0..generic.arity)
            .map(|index| dispatcher_param_kind(self.module, generic, index))
            .collect();
        let rets = self.value_kinds(&generic.result_kinds)?;
        self.instructions.push(Instr::CallIndirect {
            dsts,
            callee: callee.reg,
            args,
            arg_kinds,
            rets,
            span,
        });
        Ok(())
    }

    /// The method a send to `generic` at `site` may call through its link,
    /// with that method's function: typing proved the dispatch, the
    /// argument classes are exact or, once their sealed cones are split, a
    /// few exact classes ([`typing::exact_class_tuples`]), the runtime table
    /// selects the same method typing did for each of them, and the method
    /// takes the send's arguments and results unchanged. The runtime layout, which every unit of a run shares, fixes
    /// which method those classes select, so the link need only follow the
    /// method's newest code.
    ///
    /// The call relies on the generic's contract alone, as a dynamic send
    /// does: the send's results keep the generic's kinds and types. A newer
    /// unit may replace the method, so nothing here trusts the method's
    /// narrower contract; the runtime repoints the link on install.
    fn send_link(
        &self,
        generic: &sem::GenericFunction,
        site: sem::SiteId,
    ) -> Option<(sem::MethodId, FunctionId)> {
        let decision = self.fact(site)?;
        if decision.verdict != typing::TypingVerdict::Proved
            || decision.rule != typing::TypingRule::DispatchCovered
            || decision.actual.len() != generic.arity
        {
            return None;
        }
        let method = decision.selected_method?;
        let plan = self.ids.dispatch.get(generic.id.0 as usize)?.as_ref()?;
        // Every class the arguments may have, once sealed cones are split,
        // must select the method directly, as a request would.
        let (tuples, _) = typing::exact_class_tuples(self.module, &decision.actual)?;
        for exact in &tuples {
            let case = plan.select(
                |index| exact.get(index).copied(),
                |class, specializer| self.module.is_same_or_subclass(class, specializer),
            )?;
            if case.method != method || !case.direct {
                return None;
            }
        }
        let callee = *self.ids.methods.get(method.0 as usize)?;
        Some((method, callee))
    }

    /// Lowers a body's statements: binds each `let`'s locals, and evaluates
    /// each expression statement for effect.
    fn lower_statements(&mut self, statements: &[sem::Statement]) -> Result<(), LowerError> {
        for statement in statements {
            let let_binding = match statement {
                sem::Statement::Let(let_binding) => let_binding,
                sem::Statement::Expr(expr) => {
                    let _ = self.lower_results_expr(expr)?;
                    continue;
                }
            };
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
                let value = self.home(*local, value, let_binding.span);
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
                let value = self.home(local, value, let_binding.span);
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

    /// The register `local` lives in, bound to `value`.
    ///
    /// A local is otherwise the register of the value bound to it, which
    /// other locals and pending operands may share. An assigned local gets
    /// a register of its own that only its binding and assignments write,
    /// so an assignment changes nothing but the local, and a read that an
    /// assignment may overtake copies it ([`Self::read_local`]). A local
    /// bound to an assigned local's register gets a copy too. The tape does
    /// not know the shape of a callable value in an assigned local's
    /// register: assignments may store any of them.
    fn home(&mut self, local: sem::LocalId, value: Value, span: Span) -> Value {
        if self.current_local(local).is_some_and(|local| local.shared) {
            let cell = self.alloc(ValueKind::Object(None));
            self.instructions.push(Instr::CellNew {
                dst: cell.reg,
                value: value.reg,
                kind: value.kind,
                span,
            });
            return Value {
                reachable: value.reachable,
                ..cell
            };
        }
        let assigned = self
            .current_local(local)
            .is_some_and(|local| local.assigned);
        if !assigned && !self.is_assigned_register(value.reg) {
            return value;
        }
        let home = self.alloc(value.kind);
        if matches!(value.kind, ValueKind::Closure(_)) {
            self.callable_joins.push((home.reg, false));
        }
        self.instructions.push(Instr::Move {
            dst: home.reg,
            src: value.reg,
            span,
        });
        Value {
            reachable: value.reachable,
            ..home
        }
    }

    /// Whether `reg` is an assigned local's register.
    fn is_assigned_register(&self, reg: Reg) -> bool {
        self.source_locals
            .iter()
            .zip(&self.local_regs)
            .any(|(local, value)| {
                local.assigned && !local.shared && value.is_some_and(|value| value.reg == reg)
            })
    }

    /// Reads the value the cell in `cell` holds, of category `kind`.
    fn cell_get(&mut self, cell: Reg, kind: ValueKind, span: Span) -> Value {
        let value = self.alloc(kind);
        self.instructions.push(Instr::CellGet {
            dst: value.reg,
            cell,
            kind,
            span,
        });
        if matches!(kind, ValueKind::Closure(_)) {
            // Any callable of the local's type may have been stored.
            self.callable_joins.push((value.reg, false));
        }
        value
    }

    /// The value of `local` at the read `expr`: its register, or for an
    /// assigned local whose register an assignment may change before the
    /// read is used, a copy of its register taken now ([`reads`]).
    fn read_local(
        &mut self,
        expr: &sem::Expr,
        local: sem::LocalId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let value = self
            .local_regs
            .get(local.0 as usize)
            .and_then(|value| *value)
            .ok_or_else(|| invalid_semantic("local was read before assignment"))?;
        if let Some(local) = self.current_local(local).filter(|local| local.shared) {
            let kind = self.value_kind_with_callable_signature(
                local.kind,
                local.callable_signature.as_ref(),
            )?;
            return Ok(self.cell_get(value.reg, kind, span));
        }
        let direct = self
            .direct_reads
            .iter()
            .any(|read| core::ptr::eq(*read, expr));
        if direct
            || !self
                .current_local(local)
                .is_some_and(|local| local.assigned)
        {
            return Ok(value);
        }
        let copy = self.alloc(value.kind);
        self.note_callable_move(copy.reg, value.reg);
        self.instructions.push(Instr::Move {
            dst: copy.reg,
            src: value.reg,
            span,
        });
        Ok(copy)
    }

    /// The cell a closure's capture of a shared local holds.
    fn capture_cell(&mut self, env: Value, capture: sem::CaptureId, span: Span) -> Reg {
        let cell = self.alloc(ValueKind::Object(None));
        self.instructions.push(Instr::CaptureGet {
            dst: cell.reg,
            env: env.reg,
            index: capture.0 as usize,
            kind: ValueKind::Object(None),
            span,
        });
        cell.reg
    }

    /// Lowers `name := value` through a closure's capture of a shared
    /// local: the value, converted to the local's kind, goes into the cell
    /// the capture holds, and is the result.
    fn lower_assign_capture(
        &mut self,
        capture: sem::CaptureId,
        kind: sem::ValueKind,
        callable_signature: Option<&sem::CallableSignature>,
        value: &sem::Expr,
        span: Span,
    ) -> Result<Value, LowerError> {
        let expected = self.value_kind_with_callable_signature(kind, callable_signature)?;
        let value = self.lower_expr(value)?;
        let value = self.coerce_value(value, expected, span)?;
        let env = self
            .env
            .ok_or_else(|| invalid_semantic("capture assigned outside closure body"))?;
        if !self
            .captures
            .get(capture.0 as usize)
            .is_some_and(|capture| capture.shared)
        {
            return Err(invalid_semantic("assigned capture is not shared"));
        }
        let cell = self.capture_cell(env, capture, span);
        self.instructions.push(Instr::CellSet {
            cell,
            value: value.reg,
            kind: value.kind,
            span,
        });
        Ok(value)
    }

    /// Lowers `local := value`: the value, converted to the local's kind,
    /// moves into the local's register ([`Self::home`]) and is the result.
    fn lower_assign_local(
        &mut self,
        local: sem::LocalId,
        kind: sem::ValueKind,
        callable_signature: Option<&sem::CallableSignature>,
        value: &sem::Expr,
        span: Span,
    ) -> Result<Value, LowerError> {
        let expected = self.value_kind_with_callable_signature(kind, callable_signature)?;
        let value = self.lower_expr(value)?;
        let value = self.coerce_value(value, expected, span)?;
        let home = self
            .local_regs
            .get(local.0 as usize)
            .and_then(|value| *value)
            .ok_or_else(|| invalid_semantic("local was assigned before its binding"))?;
        if self.current_local(local).is_some_and(|local| local.shared) {
            self.instructions.push(Instr::CellSet {
                cell: home.reg,
                value: value.reg,
                kind: value.kind,
                span,
            });
            return Ok(value);
        }
        if home.kind != value.kind
            || self
                .unboxed_locals
                .iter()
                .any(|(unboxed, _)| *unboxed == home.reg)
        {
            return Err(invalid_semantic(
                "assigned local has no register of its own",
            ));
        }
        self.note_callable_move(home.reg, value.reg);
        self.instructions.push(Instr::Move {
            dst: home.reg,
            src: value.reg,
            span,
        });
        Ok(value)
    }

    /// Lowers `while (test) body end` or `until (test) body end` to a loop
    /// of tape jumps: the head evaluates the test and branches to the body
    /// or past the loop, and the body jumps back to the head. A loop
    /// produces no values.
    fn lower_loop(
        &mut self,
        kind: sem::LoopKind,
        test: &sem::Expr,
        body: &sem::Block,
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let head = self.alloc_label();
        let iteration = self.alloc_label();
        let exit = self.alloc_label();
        self.instructions.push(Instr::Jump { label: head, span });
        self.instructions.push(Instr::Label { label: head });
        let test = self.lower_expr(test)?;
        if test.kind != ValueKind::Bool {
            return Err(invalid_semantic("loop test is not bool"));
        }
        let (then_label, else_label) = if kind.continues_on() {
            (iteration, exit)
        } else {
            (exit, iteration)
        };
        self.instructions.push(Instr::Branch {
            cond: test.reg,
            then_label,
            else_label,
            span,
        });
        self.instructions.push(Instr::Label { label: iteration });
        self.lower_statements(&body.statements)?;
        let values = self.lower_results_expr(&body.result)?;
        if values.iter().all(|value| value.reachable) {
            self.instructions.push(Instr::Jump { label: head, span });
        }
        self.instructions.push(Instr::Label { label: exit });
        Ok(Vec::new())
    }

    /// Lowers a `block` expression whose results have the semantic kinds
    /// `result_kinds`: the join of the body's results and the exit's.
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
        result_kinds: &[sem::ValueKind],
        exit: Option<&sem::BlockExit>,
        body: &sem::Block,
        cleanup: Option<&sem::Block>,
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        if exit.is_none() && cleanup.is_none() {
            self.lower_statements(&body.statements)?;
            return self.lower_results_expr(&body.result);
        }
        // Values leaving through the exit arrive with the exit's own kinds.
        let exit_kinds = exit
            .map(|exit| self.value_kinds(&exit.result_kinds))
            .transpose()?
            .unwrap_or_default();
        let exit_local = exit.map(|exit| exit.local);
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
        self.enter_region(catches, span);
        if let (Some(sem_exit), Some(bound)) = (exit_local, exit) {
            self.exits.push((sem_exit, bound));
        }
        self.lower_statements(&body.statements)?;
        let body_values = self.lower_results_expr(&body.result);
        if exit_local.is_some() {
            self.exits.pop();
        }
        let body_values = body_values?;
        self.instructions.push(Instr::LeaveRegion);
        let results = self
            .value_kinds(result_kinds)?
            .into_iter()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        if body_values.len() != results.len() {
            return Err(invalid_semantic(
                "block body arity does not match its result",
            ));
        }

        // The body's values move to the block's results before the cleanup
        // runs: a value may be a local's register, which the cleanup may
        // assign.
        let mut reachable = body_values.iter().all(|value| value.reachable);
        if reachable {
            self.move_results(&results, &body_values, body.result.span())?;
            reachable = self.leave_block(exit.as_ref(), cleanup, span)?;
        }
        if reachable {
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
            let exited = exit_kinds
                .iter()
                .map(|kind| self.alloc(*kind))
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
                self.move_results(&results, &exited, span)?;
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
        let (signature, function, boxed) = (thunk.signature, thunk.function, thunk.boxed);
        let token = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::PushExit {
            dst: token.reg,
            span,
        });
        let procedure = self.emit_callable_new(
            function,
            BoxedEntry::Function(boxed),
            signature,
            &[token.reg],
            span,
        )?;
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

    /// Moves one arm's values into the result registers of a join (an `if`,
    /// a `handle`, a `block`), converting each to the join's kind: an arm may
    /// produce a narrower kind than the join semantic analysis gave it.
    fn move_results(
        &mut self,
        results: &[Value],
        values: &[Value],
        span: Span,
    ) -> Result<(), LowerError> {
        for (result, value) in results.iter().zip(values) {
            let value = self.coerce_value(*value, result.kind, span)?;
            self.note_callable_move(result.reg, value.reg);
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: value.reg,
                span,
            });
        }
        Ok(())
    }

    /// Records that a join moves `src` into `dst`, for
    /// [`Self::callable_shape_known`].
    fn note_callable_move(&mut self, dst: Reg, src: Reg) {
        let index = (dst.0 as usize).wrapping_sub(1);
        if !matches!(self.register_kinds.get(index), Some(ValueKind::Closure(_))) {
            return;
        }
        let known = self.callable_shape_known(src);
        match self
            .callable_joins
            .iter_mut()
            .find(|(join, _)| *join == dst)
        {
            Some((_, all)) => *all &= known,
            None => self.callable_joins.push((dst, known)),
        }
    }

    /// Whether the tape knows the shape of the callable value in `reg`: a
    /// parameter, a capture, a new callable value or a call's result, whose
    /// contract proves it, or a join of only such values. Any other, such
    /// as a slot's value, opens through [`FunctionKind::CallableParts`].
    ///
    /// The tape keeps what an instruction proves only within a straight
    /// run of code, so a value an instruction made counts only until code
    /// that may branch, such as a check ([`keeps_projections`]); a
    /// parameter's contract holds throughout.
    fn callable_shape_known(&self, reg: Reg) -> bool {
        if let Some((_, known)) = self.callable_joins.iter().find(|(join, _)| *join == reg) {
            return *known;
        }
        if self.params.iter().any(|param| param.reg == reg) {
            return self.proven_callables.contains(&reg);
        }
        let defined = self.instructions.iter().rposition(|instr| match instr {
            Instr::Call { dsts, .. }
            | Instr::CallIndirect { dsts, .. }
            | Instr::LinkedSend { dsts, .. } => dsts.contains(&reg),
            Instr::CallableNew { dst, .. } | Instr::Move { dst, .. } => *dst == reg,
            _ => false,
        });
        let Some(defined) = defined else {
            return self.proven_callables.contains(&reg);
        };
        let proven = self.proven_callables.contains(&reg)
            || matches!(
                self.instructions[defined],
                Instr::Call { .. } | Instr::CallIndirect { .. } | Instr::LinkedSend { .. }
            );
        proven && self.instructions[defined..].iter().all(keeps_projections)
    }

    /// Lowers a cleanup body for effect, returning whether control can
    /// continue after it.
    fn lower_cleanup(&mut self, cleanup: &sem::Block) -> Result<bool, LowerError> {
        self.lower_statements(&cleanup.statements)?;
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
                    reg: register_at(index),
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

        self.signal_no_applicable_method(generic)
    }

    /// Ends `generic`'s dispatcher where no method matched its arguments:
    /// signals the `<no-applicable-method>` that names them, then returns
    /// what a `use-value` restart supplies instead, once each value passes
    /// the generic's declared result type. A class or class-object result
    /// is checked as an indirect entry checks a parameter, and a limited
    /// one for its range; a replacement that fails signals a `<type-error>`
    /// whose own replacement is checked again. A result sem knows to be a
    /// particular function, generic or accessor has that callable as its
    /// only value, which no check of a closure could establish, so the
    /// restart supplies no value for it and the dispatcher returns the
    /// callable itself. Other results are whatever their register kind
    /// holds, which the driver checks.
    fn signal_no_applicable_method(
        &mut self,
        generic: &sem::GenericFunction,
    ) -> Result<(), LowerError> {
        let arguments = (0..generic.arity)
            .map(|index| Value {
                reg: register_at(index),
                kind: dispatcher_param_kind(self.module, generic, index),
                reachable: true,
            })
            .collect::<Vec<_>>();
        let values = self.emit_no_applicable_method(generic, &arguments, generic.span)?;
        self.instructions.push(Instr::Return {
            values,
            span: generic.span,
        });
        Ok(())
    }

    /// Signals the `<no-applicable-method>` of a send to `generic` with
    /// `arguments`, and returns the registers of the results that stand in
    /// for the send's, as [`Self::signal_no_applicable_method`] describes.
    fn emit_no_applicable_method(
        &mut self,
        generic: &sem::GenericFunction,
        arguments: &[Value],
        span: Span,
    ) -> Result<Vec<Reg>, LowerError> {
        let vector_class = self
            .module
            .builtin_value_class(sem::BuiltinValueClass::Vector)
            .ok_or_else(|| invalid_semantic("module has no <vector> class"))?;
        let condition_class = self
            .module
            .condition_class(sem::ConditionClass::NoApplicableMethod)
            .ok_or_else(|| invalid_semantic("module has no <no-applicable-method> class"))?;
        let mut elements = Vec::with_capacity(arguments.len());
        for argument in arguments {
            elements.push(self.box_any(*argument, span)?.reg);
        }
        let arguments = self.alloc(ValueKind::Object(Some(vector_class)));
        self.instructions.push(Instr::VectorLiteral {
            dst: arguments.reg,
            elements,
            class: vector_class,
            span,
        });
        let condition = self.alloc(ValueKind::Object(Some(condition_class)));
        self.instructions.push(Instr::NoApplicableMethod {
            dst: condition.reg,
            generic: generic.id,
            arguments: arguments.reg,
            span,
        });
        // The results a replacement supplies: every one but a known
        // callable, whose only value is the callable.
        let mut results = Vec::with_capacity(generic.result_kinds.len());
        for kind in &generic.result_kinds {
            results.push(match kind {
                sem::ValueKind::KnownCallable(_) => None,
                _ => Some(self.alloc(self.value_kind(*kind)?)),
            });
        }
        self.instructions.push(Instr::Signal {
            dsts: results.iter().flatten().map(|result| result.reg).collect(),
            condition: condition.reg,
            span,
        });
        // Typing classifies none of a dispatcher's checks.
        let site = check_site_name(self.site_owner, None, span);
        let mut values = Vec::with_capacity(results.len());
        for (index, result) in results.iter().enumerate() {
            let Some(result) = result else {
                let sem::ValueKind::KnownCallable(callable) = generic.result_kinds[index] else {
                    return Err(invalid_semantic("generic result kind changed"));
                };
                values.push(self.lower_known_callable(callable, span)?.reg);
                continue;
            };
            let target = match generic.result_ranges.get(index).copied().flatten() {
                Some(range) if result.kind == ValueKind::I64 => {
                    Some(sem::EntryTarget::Range(range))
                }
                _ => entry_target(result.kind),
            };
            let Some(target) = target else {
                values.push(result.reg);
                continue;
            };
            let checked = entry_check(
                &mut self.code(),
                target,
                result.reg,
                result.kind,
                site.clone(),
                span,
            );
            self.next_reg = register_at(self.register_kinds.len());
            values.push(checked);
        }
        Ok(values)
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

        if let Some(scalar) = generic
            .param_kinds
            .get(index)
            .copied()
            .flatten()
            .filter(|kind| {
                kind.is_number()
                    || matches!(
                        kind,
                        sem::ValueKind::Bool
                            | sem::ValueKind::Callable
                            | sem::ValueKind::KnownCallable(_)
                    )
            })
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
            object: register_at(index),
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
        sorted_methods(self.module, generic)
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
            sem::Expr::Const32 { value, span } => {
                let result = self.alloc(match value {
                    sem::Number32::I32(_) => ValueKind::I32,
                    sem::Number32::U32(_) => ValueKind::U32,
                    sem::Number32::F32(_) => ValueKind::F32,
                });
                self.instructions.push(Instr::Const32 {
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
                let value = self.read_local(expr, *local, *span)?;
                let expected = self.value_kind_with_callable_signature(
                    *result_kind,
                    callable_signature.as_ref(),
                )?;
                let binding = match self.closure {
                    Some(closure) => typing::Binding::ClosureLocal {
                        closure,
                        local: *local,
                    },
                    None => typing::Binding::Local(*local),
                };
                if let Some(value) = self.proved_read(value, expected, binding, *span) {
                    return Ok(value);
                }
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
                let result = if capture_info.shared {
                    let cell = self.capture_cell(env, *capture, *span);
                    self.cell_get(cell, kind, *span)
                } else {
                    let result = self.alloc(kind);
                    self.instructions.push(Instr::CaptureGet {
                        dst: result.reg,
                        env: env.reg,
                        index: capture.0 as usize,
                        kind,
                        span: *span,
                    });
                    if matches!(kind, ValueKind::Closure(_)) {
                        self.proven_callables.push(result.reg);
                    }
                    result
                };
                let expected = self.value_kind_with_callable_signature(
                    *result_kind,
                    callable_signature.as_ref(),
                )?;
                if let Some(closure) = self.closure
                    && let Some(result) = self.proved_read(
                        result,
                        expected,
                        typing::Binding::Capture {
                            closure,
                            capture: *capture,
                        },
                        *span,
                    )
                {
                    return Ok(result);
                }
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
                self.active_constants.push(*constant);
                let lowered = self.lower_constant_value(constant_info);
                self.active_constants.pop();
                self.coerce_value(lowered?, expected, *span)
            }
            sem::Expr::ClassObject { class, span } => self.lower_class_meta_object(*class, *span),
            sem::Expr::TypeObject {
                object,
                class,
                span,
            } => self.lower_type_object(*object, *class, *span),
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
                site,
            } => {
                let generic_info = self
                    .module
                    .generics
                    .get(generic.0 as usize)
                    .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
                if let Some(result) = self.lower_numeric_send(expr)? {
                    let kind = self.value_kind(generic_info.result_kinds[0])?;
                    return self.coerce_value(result, kind, *span);
                }
                if let Some(result) =
                    self.lower_kernel_collection_send(generic_info, args, *site, *span)?
                {
                    let kind = self.value_kind(generic_info.result_kinds[0])?;
                    return self.coerce_value(result, kind, *span);
                }
                let args = self.lower_generic_args(args, generic_info)?;
                if generic_info.result_kinds.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value generic call used in a scalar expression",
                    ));
                }
                let result = self.alloc(self.value_kind(generic_info.result_kinds[0])?);
                self.send(generic_info, args, vec![result.reg], *span, Some(*site))?;
                Ok(result)
            }
            sem::Expr::CallableCall {
                callable,
                args,
                arg_kinds,
                keywords,
                result_kinds,
                span,
                ..
            } => {
                if result_kinds.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value callable call used in a scalar expression",
                    ));
                }
                let mut results = self.lower_callable_results(
                    callable,
                    args,
                    arg_kinds,
                    keywords,
                    result_kinds,
                    *span,
                )?;
                Ok(results.remove(0))
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
                if is_unboxed(object.kind) {
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
            sem::Expr::InstanceOf {
                value,
                class,
                site,
                span,
            } => {
                let value = self.lower_expr(value)?;
                if let Some(outcome) = self.decided_test(*site) {
                    return Ok(self.decided_test_value(*site, outcome, *span));
                }
                if is_unboxed(value.kind) {
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
            sem::Expr::InstanceOfLimited {
                value,
                range,
                site,
                span,
            } => {
                let value = self.lower_expr(value)?;
                if let Some(outcome) = self.decided_test(*site) {
                    return Ok(self.decided_test_value(*site, outcome, *span));
                }
                Ok(self.lower_limited_membership(value, *range, *span))
            }
            sem::Expr::Cast {
                value,
                class,
                site,
                span,
                ..
            } => {
                // A proved numeric send already has its method's result, so
                // a cast to that result's class has nothing to unbox.
                let value = match self.lower_numeric_send(value)? {
                    Some(result)
                        if *class == sem::OBJECT_CLASS_ID
                            || is_scalar_value_class(self.module, result.kind, *class) =>
                    {
                        result
                    }
                    Some(result) => self.box_any(result, *span)?,
                    None => self.lower_expr(value)?,
                };
                if *class == sem::OBJECT_CLASS_ID {
                    return self.box_any(value, *span);
                }
                if is_scalar_value_class(self.module, value.kind, *class) {
                    return Ok(value);
                }
                if value.kind == ValueKind::Any
                    && let Some(kind) = scalar_kind_for_class(self.module, *class)
                {
                    let type_error = self.type_error(
                        TypeErrorKind::Cast,
                        class_expected(*class),
                        Some(*site),
                        *span,
                    );
                    return Ok(self.unbox(value, *class, kind, type_error, *span));
                }
                // A static upcast needs no check, but the value now has the
                // cast's class, as semantic analysis says: joins compare it.
                if let ValueKind::Object(Some(actual)) = value.kind
                    && self.module.is_same_or_subclass(actual, *class)
                {
                    return Ok(Value {
                        kind: ValueKind::Object(Some(*class)),
                        ..value
                    });
                }
                // A proved cast between object registers reuses its operand.
                // A boxed `<object>` still needs the host to unwrap it, and
                // the unwrap checks the class on the way, so it stays.
                if matches!(value.kind, ValueKind::Object(_)) && self.proved(*site) {
                    self.record(*site, CheckCodegen::Elided);
                    return Ok(Value {
                        kind: ValueKind::Object(Some(*class)),
                        ..value
                    });
                }
                let type_error = self.type_error(
                    TypeErrorKind::Cast,
                    class_expected(*class),
                    Some(*site),
                    *span,
                );
                let result = self.alloc(ValueKind::Object(Some(*class)));
                self.instructions.push(Instr::CastObject {
                    dst: result.reg,
                    value: value.reg,
                    input: value.kind,
                    class: *class,
                    type_error,
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
                    field_regs.push(self.field_value(slot.kind, value, field.span)?.reg);
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
            sem::Expr::MakeRuntimeClass {
                class,
                bound,
                init_args,
                candidates,
                span,
            } => self.lower_make_runtime_class(class, *bound, init_args, candidates, *span),
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
                site,
                span,
            } => {
                // Sem rejects constant shift counts outside 0..=31.
                let constant_count = matches!(**rhs, sem::Expr::Const32 { .. });
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                // Typing proved the result fits, or the shift count is in
                // range: the check can go.
                let unchecked = site.filter(|site| self.proved(*site));
                if op.is_wrapping() || op.is_bitwise() {
                    if op.is_shift() && !constant_count {
                        match unchecked {
                            Some(site) => self.record(site, CheckCodegen::Elided),
                            None => {
                                let type_error = self.type_error(
                                    TypeErrorKind::Range,
                                    shift_count_expected(),
                                    *site,
                                    *span,
                                );
                                return self.lower_checked_shift(
                                    *op, *kind, lhs, rhs.reg, type_error, *span,
                                );
                            }
                        }
                    }
                    return self.lower_bit_operation(*op, *kind, lhs, rhs.reg, *span);
                }
                let not_arithmetic = || invalid_semantic("integer division has no lowering");
                match kind {
                    sem::NumericKind::Integer => {
                        let op = match op {
                            sem::BinaryOp::Add => I64ArithOp::Add,
                            sem::BinaryOp::Sub => I64ArithOp::Sub,
                            sem::BinaryOp::Mul => I64ArithOp::Mul,
                            _ => return Err(not_arithmetic()),
                        };
                        if let Some(site) = unchecked {
                            self.record(site, CheckCodegen::Specialized);
                            return Ok(
                                self.lower_unchecked_integer_arith(op, lhs.reg, rhs.reg, *span)
                            );
                        }
                        let type_error = self.overflow_error(ValueKind::I64, *site, *span);
                        Ok(self.lower_checked_integer_arith(
                            op,
                            lhs.reg,
                            rhs.reg,
                            vec![lhs.reg, rhs.reg],
                            type_error,
                            *span,
                        ))
                    }
                    sem::NumericKind::Float => {
                        let op = match op {
                            sem::BinaryOp::Add => F64BinaryOp::Add,
                            sem::BinaryOp::Sub => F64BinaryOp::Sub,
                            sem::BinaryOp::Mul => F64BinaryOp::Mul,
                            sem::BinaryOp::Div => F64BinaryOp::Div,
                            _ => return Err(not_arithmetic()),
                        };
                        let result = self.alloc(ValueKind::F64);
                        self.instructions.push(Instr::F64Binary {
                            dst: result.reg,
                            op,
                            lhs: lhs.reg,
                            rhs: rhs.reg,
                            span: *span,
                        });
                        Ok(result)
                    }
                    sem::NumericKind::SingleFloat => {
                        let op = match op {
                            sem::BinaryOp::Add => Num32BinaryOp::F32Add,
                            sem::BinaryOp::Sub => Num32BinaryOp::F32Sub,
                            sem::BinaryOp::Mul => Num32BinaryOp::F32Mul,
                            sem::BinaryOp::Div => Num32BinaryOp::F32Div,
                            _ => return Err(not_arithmetic()),
                        };
                        Ok(self.num32_binary(op, lhs.reg, rhs.reg, ValueKind::F32, *span))
                    }
                    sem::NumericKind::I32 | sem::NumericKind::U32 => {
                        let unsigned = *kind == sem::NumericKind::U32;
                        if let Some(site) = unchecked {
                            let op = match (op, unsigned) {
                                (sem::BinaryOp::Add, false) => Num32BinaryOp::I32Add,
                                (sem::BinaryOp::Sub, false) => Num32BinaryOp::I32Sub,
                                (sem::BinaryOp::Mul, false) => Num32BinaryOp::I32Mul,
                                (sem::BinaryOp::Add, true) => Num32BinaryOp::U32Add,
                                (sem::BinaryOp::Sub, true) => Num32BinaryOp::U32Sub,
                                (sem::BinaryOp::Mul, true) => Num32BinaryOp::U32Mul,
                                _ => return Err(not_arithmetic()),
                            };
                            self.record(site, CheckCodegen::Specialized);
                            return Ok(self.num32_binary(op, lhs.reg, rhs.reg, lhs.kind, *span));
                        }
                        let op = match (op, unsigned) {
                            (sem::BinaryOp::Add, false) => Num32CheckedOp::I32AddOvf,
                            (sem::BinaryOp::Sub, false) => Num32CheckedOp::I32SubOvf,
                            (sem::BinaryOp::Mul, false) => Num32CheckedOp::I32MulOvf,
                            (sem::BinaryOp::Add, true) => Num32CheckedOp::U32AddOvf,
                            (sem::BinaryOp::Sub, true) => Num32CheckedOp::U32SubOvf,
                            (sem::BinaryOp::Mul, true) => Num32CheckedOp::U32MulOvf,
                            _ => return Err(not_arithmetic()),
                        };
                        let type_error = self.overflow_error(lhs.kind, *site, *span);
                        let operands = vec![lhs.reg, rhs.reg];
                        Ok(self.lower_checked_num32_arith(
                            op, lhs, rhs.reg, operands, type_error, *span,
                        ))
                    }
                }
            }
            sem::Expr::FloatCall {
                builtin,
                args,
                span,
            } => self.lower_float_call(*builtin, args, *span),
            sem::Expr::Convert {
                to,
                value,
                site,
                span,
                ..
            } => {
                let value = self.lower_expr(value)?;
                if self.proved(*site)
                    && let Some(converted) = self.convert_in_range(value, *to, *span)
                {
                    self.record(*site, CheckCodegen::Specialized);
                    return Ok(converted);
                }
                self.convert_number(value, *to, Some(*site), *span)
            }
            sem::Expr::CheckRange {
                value,
                range,
                site,
                span,
                ..
            } => {
                let value = self.lower_expr(value)?;
                let value = match value.kind {
                    ValueKind::I32 | ValueKind::U32 => self.widen_integer(value, *span),
                    _ => value,
                };
                if self.proved(*site) {
                    self.record(*site, CheckCodegen::Elided);
                    return Ok(value);
                }
                let type_error = self.type_error(
                    TypeErrorKind::Range,
                    range_expected(*range),
                    Some(*site),
                    *span,
                );
                Ok(self.lower_range_check(value, *range, type_error, *span))
            }
            sem::Expr::Negate {
                kind,
                value,
                site,
                span,
            } => {
                let value = self.lower_expr(value)?;
                let unchecked = site.filter(|site| self.proved(*site));
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
                        if let Some(site) = unchecked {
                            self.record(site, CheckCodegen::Specialized);
                            return Ok(self.lower_unchecked_integer_arith(
                                I64ArithOp::Sub,
                                zero.reg,
                                value.reg,
                                *span,
                            ));
                        }
                        let type_error = self.overflow_error(ValueKind::I64, *site, *span);
                        Ok(self.lower_checked_integer_arith(
                            I64ArithOp::Sub,
                            zero.reg,
                            value.reg,
                            vec![value.reg],
                            type_error,
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
                    sem::NumericKind::SingleFloat => Ok(self.num32_unary(
                        Num32UnaryOp::F32Neg,
                        value.reg,
                        ValueKind::F32,
                        *span,
                    )),
                    sem::NumericKind::I32 => {
                        // `0 - x` overflows only for `i32::MIN`.
                        let zero = self.alloc(ValueKind::I32);
                        self.instructions.push(Instr::Const32 {
                            dst: zero.reg,
                            value: sem::Number32::I32(0),
                            span: *span,
                        });
                        if let Some(site) = unchecked {
                            self.record(site, CheckCodegen::Specialized);
                            return Ok(self.num32_binary(
                                Num32BinaryOp::I32Sub,
                                zero.reg,
                                value.reg,
                                ValueKind::I32,
                                *span,
                            ));
                        }
                        let type_error = self.overflow_error(ValueKind::I32, *site, *span);
                        Ok(self.lower_checked_num32_arith(
                            Num32CheckedOp::I32SubOvf,
                            zero,
                            value.reg,
                            vec![value.reg],
                            type_error,
                            *span,
                        ))
                    }
                    sem::NumericKind::U32 => Err(invalid_semantic(
                        "`<u32>` negation is promoted to `<integer>` before lowering",
                    )),
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
                    sem::NumericKind::I32
                    | sem::NumericKind::U32
                    | sem::NumericKind::SingleFloat => {
                        return Err(invalid_semantic(
                            "exact mixed comparisons take an `<integer>` and a binary64 float",
                        ));
                    }
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
                    kind @ (sem::NumericKind::I32
                    | sem::NumericKind::U32
                    | sem::NumericKind::SingleFloat) => Instr::Num32Binary {
                        dst: result.reg,
                        op: num32_compare(*kind, *op),
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
            sem::Expr::Identical { lhs, rhs, span } => {
                let lhs = self.lower_expr(lhs)?;
                if lhs.kind == ValueKind::Bool {
                    let rhs = self.lower_expr(rhs)?;
                    if rhs.kind == ValueKind::Bool {
                        return Ok(self.booleans_identical(lhs, rhs, *span));
                    }
                    let lhs = self.box_any(lhs, *span)?;
                    let rhs = self.box_any(rhs, *span)?;
                    let result = self.alloc(ValueKind::Bool);
                    self.instructions.push(Instr::Identical {
                        dst: result.reg,
                        lhs: lhs.reg,
                        rhs: rhs.reg,
                        span: *span,
                    });
                    return Ok(result);
                }
                let lhs = self.box_any(lhs, *span)?;
                let rhs = self.lower_expr(rhs)?;
                let rhs = self.box_any(rhs, *span)?;
                let result = self.alloc(ValueKind::Bool);
                self.instructions.push(Instr::Identical {
                    dst: result.reg,
                    lhs: lhs.reg,
                    rhs: rhs.reg,
                    span: *span,
                });
                Ok(result)
            }
            sem::Expr::AssignLocal {
                local,
                result_kind,
                callable_signature,
                value,
                span,
            } => self.lower_assign_local(
                *local,
                *result_kind,
                callable_signature.as_ref(),
                value,
                *span,
            ),
            sem::Expr::AssignCapture {
                capture,
                result_kind,
                callable_signature,
                value,
                span,
            } => self.lower_assign_capture(
                *capture,
                *result_kind,
                callable_signature.as_ref(),
                value,
                *span,
            ),
            sem::Expr::If { .. }
            | sem::Expr::Handle { .. }
            | sem::Expr::Block { .. }
            | sem::Expr::Discard { .. }
            | sem::Expr::Loop { .. } => {
                let mut results = self.lower_results_expr(expr)?;
                if results.len() != 1 {
                    return Err(invalid_semantic(
                        "multiple-value join used in a scalar expression",
                    ));
                }
                Ok(results.remove(0))
            }
            sem::Expr::Error { .. } => Err(invalid_semantic("semantic expression is invalid")),
        }
    }

    /// `lhs == rhs` for two unboxed booleans, without boxing either: it is
    /// `rhs` where `lhs` is true and the negation of `rhs` where it is false.
    fn booleans_identical(&mut self, lhs: Value, rhs: Value, span: Span) -> Value {
        let result = self.alloc(ValueKind::Bool);
        let lhs_true = self.alloc_label();
        let lhs_false = self.alloc_label();
        let rhs_true = self.alloc_label();
        let rhs_false = self.alloc_label();
        let join = self.alloc_label();
        self.instructions.push(Instr::Branch {
            cond: lhs.reg,
            then_label: lhs_true,
            else_label: lhs_false,
            span,
        });
        self.instructions.push(Instr::Label { label: lhs_true });
        self.instructions.push(Instr::Move {
            dst: result.reg,
            src: rhs.reg,
            span,
        });
        self.instructions.push(Instr::Jump { label: join, span });
        self.instructions.push(Instr::Label { label: lhs_false });
        self.instructions.push(Instr::Branch {
            cond: rhs.reg,
            then_label: rhs_true,
            else_label: rhs_false,
            span,
        });
        for (label, value) in [(rhs_true, false), (rhs_false, true)] {
            self.instructions.push(Instr::Label { label });
            self.instructions.push(Instr::ConstBool {
                dst: result.reg,
                value,
                span,
            });
            self.instructions.push(Instr::Jump { label: join, span });
        }
        self.instructions.push(Instr::Label { label: join });
        result
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
                site,
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
                self.send(
                    generic_info,
                    args,
                    results.iter().map(|result| result.reg).collect(),
                    *span,
                    Some(*site),
                )?;
                Ok(results)
            }
            sem::Expr::CallableCall {
                callable,
                args,
                arg_kinds,
                keywords,
                result_kinds,
                span,
                ..
            } => self.lower_callable_results(
                callable,
                args,
                arg_kinds,
                keywords,
                result_kinds,
                *span,
            ),
            sem::Expr::Signal {
                condition,
                condition_class,
                result_kinds,
                span,
                ..
            } => self.lower_signal_results(condition, *condition_class, result_kinds, *span),
            sem::Expr::If {
                condition,
                refinement,
                then_branch,
                else_branch,
                span,
            } => {
                let result_kinds = self.module.expr_kinds(expr);
                self.lower_results_if(
                    &result_kinds,
                    condition,
                    *refinement,
                    then_branch,
                    else_branch,
                    *span,
                )
            }
            sem::Expr::Discard { value, .. } => {
                let _ = self.lower_results_expr(value)?;
                Ok(Vec::new())
            }
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
            } => {
                let result_kinds = self.module.expr_kinds(expr);
                self.lower_block_expr_results(
                    &result_kinds,
                    exit.as_ref(),
                    body,
                    cleanup.as_deref(),
                    *span,
                )
            }
            sem::Expr::Loop {
                kind,
                test,
                body,
                span,
            } => self.lower_loop(*kind, test, body, *span),
            _ => Ok(vec![self.lower_expr(expr)?]),
        }
    }

    /// Lowers an `if` whose result has the semantic join `result_kinds` of
    /// its branches; each branch converts its values to that join.
    ///
    /// A `refinement` that unboxes a local applies to the true branch: see
    /// [`Self::enter_refinement`].
    fn lower_results_if(
        &mut self,
        result_kinds: &[sem::ValueKind],
        condition: &sem::Expr,
        refinement: Option<sem::Refinement>,
        then_branch: &sem::Expr,
        else_branch: &sem::Expr,
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let decided = match condition {
            sem::Expr::InstanceOf { site, .. } | sem::Expr::InstanceOfLimited { site, .. } => {
                self.decided_test(*site)
            }
            _ => None,
        };
        let test_span = condition.span();
        let condition = self.lower_expr(condition)?;
        if condition.kind != ValueKind::Bool {
            return Err(invalid_semantic("if condition is not bool"));
        }
        if let Some(outcome) = decided {
            if !outcome {
                return self.lower_one_branch(result_kinds, else_branch, then_branch);
            }
            let entered = self.enter_refinement(refinement, then_branch, test_span)?;
            let values = self.lower_one_branch(result_kinds, then_branch, else_branch);
            self.leave_refinement(entered);
            return values;
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
        let entered = self.enter_refinement(refinement, then_branch, test_span)?;
        let then_values = self.lower_results_expr(then_branch)?;
        let results = self
            .value_kinds(result_kinds)?
            .into_iter()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        if then_values.len() != results.len() {
            return Err(invalid_semantic("if branch result arities differ"));
        }
        let then_reachable = then_values.iter().all(|value| value.reachable);
        if then_reachable {
            self.move_results(&results, &then_values, then_branch.span())?;
            self.instructions.push(Instr::Jump {
                label: join_label,
                span,
            });
        }
        self.leave_refinement(entered);

        self.instructions.push(Instr::Label { label: else_label });
        let else_values = self.lower_results_expr(else_branch)?;
        if else_values.len() != results.len() {
            return Err(invalid_semantic("if branch result arities differ"));
        }
        let else_reachable = else_values.iter().all(|value| value.reachable);
        if else_reachable {
            self.move_results(&results, &else_values, else_branch.span())?;
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

    /// Applies a true-branch `refinement` that unboxes a local: the boxed
    /// register unboxes once, at the top of `branch`, and every read of the
    /// local there uses the scalar. Without this, each read would unbox on
    /// its own. Returns what [`Self::leave_refinement`] restores.
    ///
    /// The `instance?` test that guards the branch passed (or typing proved
    /// it), so the value is an instance of the scalar's class and the unbox
    /// cannot fail; see `true_branch_refinement` in [`crate::sem`] for why
    /// every `<integer>` unboxes to `i64`. A branch that never reads the
    /// local does not unbox it.
    fn enter_refinement(
        &mut self,
        refinement: Option<sem::Refinement>,
        branch: &sem::Expr,
        span: Span,
    ) -> Result<Option<(sem::LocalId, Value)>, LowerError> {
        let Some(refinement) = refinement else {
            return Ok(None);
        };
        let kind = self.value_kind(refinement.kind)?;
        let index = refinement.local.0 as usize;
        let Some(boxed) = self.local_regs.get(index).copied().flatten() else {
            return Ok(None);
        };
        if boxed.kind != ValueKind::Any
            || !is_unboxed(kind)
            || !facts::reads_local(branch, refinement.local)
        {
            return Ok(None);
        }
        let class = scalar_concrete_class(self.module, kind)
            .ok_or_else(|| invalid_semantic("refined scalar kind has no class"))?;
        // The test that refined the local proved the class, so this cannot
        // fail.
        let type_error = self.coercion_error(class_expected(class), span);
        let unboxed = self.unbox(boxed, class, kind, type_error, span);
        self.unboxed_locals.push((unboxed.reg, boxed.reg));
        self.local_regs[index] = Some(unboxed);
        Ok(Some((refinement.local, boxed)))
    }

    /// Ends a refinement [`Self::enter_refinement`] applied: reads of the
    /// local see its boxed register again.
    fn leave_refinement(&mut self, entered: Option<(sem::LocalId, Value)>) {
        if let Some((local, boxed)) = entered {
            self.unboxed_locals.pop();
            if let Some(slot) = self.local_regs.get_mut(local.0 as usize) {
                *slot = Some(boxed);
            }
        }
    }

    /// Lowers the one branch of an `if` that typing proved its test takes.
    ///
    /// The other branch is not emitted; its checks are recorded as elided.
    /// Closures it creates are still lowered as functions of their own, and
    /// report what their code does.
    fn lower_one_branch(
        &mut self,
        result_kinds: &[sem::ValueKind],
        live: &sem::Expr,
        dead: &sem::Expr,
    ) -> Result<Vec<Value>, LowerError> {
        let mut dropped = Vec::new();
        facts::for_each_site(dead, &mut |site| dropped.push(site));
        for site in dropped {
            self.record(site, CheckCodegen::Elided);
        }
        let values = self.lower_results_expr(live)?;
        let results = self
            .value_kinds(result_kinds)?
            .into_iter()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        if values.len() != results.len() {
            return Err(invalid_semantic("if branch result arities differ"));
        }
        let reachable = values.iter().all(|value| value.reachable);
        if reachable {
            self.move_results(&results, &values, live.span())?;
        } else {
            // As after an `if` neither of whose branches continues: code
            // after it starts at a label nothing jumps to.
            let join_label = self.alloc_label();
            self.instructions.push(Instr::Label { label: join_label });
        }
        Ok(results
            .into_iter()
            .map(|result| Value {
                reachable,
                ..result
            })
            .collect())
    }

    /// Lowers `make` on a class value: evaluates the class and the init
    /// arguments once, then tests the value against each candidate class in
    /// turn and allocates the one it names, completing its fields with that
    /// class's defaults. A value naming no candidate, or a candidate that
    /// needs an init keyword the call does not pass, traps.
    fn lower_make_runtime_class(
        &mut self,
        class: &sem::Expr,
        bound: sem::ClassId,
        init_args: &[sem::InitArg],
        candidates: &[sem::MakeCandidate],
        span: Span,
    ) -> Result<Value, LowerError> {
        let class_value = self.lower_expr(class)?;
        let bound_info = self
            .module
            .classes
            .get(bound.0 as usize)
            .ok_or_else(|| invalid_semantic("class id is out of range"))?;
        let mut explicit = Vec::with_capacity(init_args.len());
        for arg in init_args {
            let slot = bound_info
                .effective_slots
                .get(arg.slot.0 as usize)
                .ok_or_else(|| invalid_semantic("init argument slot is out of range"))?;
            let value = self.lower_expr(&arg.value)?;
            explicit.push((arg.slot, self.field_value(slot.kind, value, arg.span)?));
        }
        let class_id = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::RuntimeCall {
            dst: class_id.reg,
            op: RuntimeOp::ClassMetaId,
            args: vec![class_value.reg],
            kind: ValueKind::U64,
            span,
        });
        let result = self.alloc(ValueKind::Object(Some(bound)));
        let done = self.alloc_label();
        for candidate in candidates {
            let candidate_id = self.alloc(ValueKind::U64);
            self.instructions.push(Instr::ConstU64 {
                dst: candidate_id.reg,
                value: u64::from(candidate.class.0),
                span,
            });
            let names_candidate = self.alloc(ValueKind::Bool);
            self.instructions.push(Instr::EqU64 {
                dst: names_candidate.reg,
                lhs: class_id.reg,
                rhs: candidate_id.reg,
                span,
            });
            let chosen = self.alloc_label();
            let next = self.alloc_label();
            self.instructions.push(Instr::Branch {
                cond: names_candidate.reg,
                then_label: chosen,
                else_label: next,
                span,
            });
            self.instructions.push(Instr::Label { label: chosen });
            if candidate.missing_keyword.is_some() {
                self.instructions.push(Instr::Trap {
                    trap: LokoTrap::MakeMissingInitKeyword,
                    span,
                });
            } else {
                let object = self.lower_make_candidate(candidate, &explicit, span)?;
                self.instructions.push(Instr::Move {
                    dst: result.reg,
                    src: object.reg,
                    span,
                });
                self.instructions.push(Instr::Jump { label: done, span });
            }
            self.instructions.push(Instr::Label { label: next });
        }
        self.instructions.push(Instr::Trap {
            trap: LokoTrap::MakeClassOutsideBound,
            span,
        });
        self.instructions.push(Instr::Label { label: done });
        Ok(result)
    }

    /// Allocates one candidate of a `make` on a class value, taking explicit
    /// fields from the already evaluated init arguments.
    fn lower_make_candidate(
        &mut self,
        candidate: &sem::MakeCandidate,
        explicit: &[(sem::SlotId, Value)],
        span: Span,
    ) -> Result<Value, LowerError> {
        let class_info = self
            .module
            .classes
            .get(candidate.class.0 as usize)
            .ok_or_else(|| invalid_semantic("class id is out of range"))?;
        let mut field_regs = Vec::with_capacity(class_info.initialization_plan.steps.len());
        for step in &class_info.initialization_plan.steps {
            if let Some((_, value)) = explicit.iter().find(|(slot, _)| *slot == step.slot) {
                field_regs.push(value.reg);
                continue;
            }
            let slot = class_info
                .effective_slots
                .get(step.slot.0 as usize)
                .ok_or_else(|| invalid_semantic("initialization slot is out of range"))?;
            let Some(field) = candidate
                .defaults
                .iter()
                .find(|field| field.slot == step.slot)
            else {
                return Err(invalid_semantic("missing object field"));
            };
            let value = self.lower_expr(&field.value)?;
            field_regs.push(self.field_value(slot.kind, value, field.span)?.reg);
        }
        let object = self.alloc(ValueKind::Object(Some(candidate.class)));
        self.instructions.push(Instr::MakeObject {
            dst: object.reg,
            class: candidate.class,
            fields: field_regs,
            span,
        });
        Ok(object)
    }

    /// Adapts a field value to its slot's register kind: boxed for an
    /// `<object>` slot, widened for a limited integer in an `<integer>` slot.
    fn field_value(
        &mut self,
        slot_kind: sem::ValueKind,
        value: Value,
        span: Span,
    ) -> Result<Value, LowerError> {
        Ok(if slot_kind == sem::ValueKind::Any {
            self.box_any(value, span)?
        } else if slot_kind == sem::ValueKind::I64
            && matches!(value.kind, ValueKind::I32 | ValueKind::U32)
        {
            self.widen_integer(value, span)
        } else {
            value
        })
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

    /// Builds the `<limited-integer>` type object `object` from constants.
    fn lower_type_object(
        &mut self,
        object: TypeObject,
        class: sem::ClassId,
        span: Span,
    ) -> Result<Value, LowerError> {
        let TypeObject::LimitedInteger { range, named } = object else {
            return Err(invalid_semantic(
                "only limited integer types are written as type objects",
            ));
        };
        let mut args = Vec::with_capacity(5);
        for bound in [range.min, range.max] {
            let present = self.alloc(ValueKind::Bool);
            self.instructions.push(Instr::ConstBool {
                dst: present.reg,
                value: bound.is_some(),
                span,
            });
            let value = self.alloc(ValueKind::I64);
            self.instructions.push(Instr::ConstI64 {
                dst: value.reg,
                value: bound.unwrap_or(0),
                span,
            });
            args.extend([present.reg, value.reg]);
        }
        let named_index = named
            .and_then(|named| {
                sem::LimitedInteger::ALL
                    .iter()
                    .position(|kind| *kind == named)
            })
            .map_or(0, |index| index + 1);
        let named = self.alloc(ValueKind::U64);
        self.instructions.push(Instr::ConstU64 {
            dst: named.reg,
            value: u64::try_from(named_index).unwrap_or(0),
            span,
        });
        args.push(named.reg);
        let result = self.alloc(ValueKind::Object(Some(class)));
        self.instructions.push(Instr::RuntimeCall {
            dst: result.reg,
            op: RuntimeOp::LimitedIntegerType,
            args,
            kind: result.kind,
            span,
        });
        Ok(result)
    }

    fn emit_class_meta_object(&mut self, dst: Reg, class: sem::ClassId, span: Span) {
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
        let (function, boxed, sig) = match callable {
            sem::KnownCallable::DirectFunction(function) => {
                let lowered = self
                    .ids
                    .function_closure_thunks
                    .get(function.0 as usize)
                    .and_then(|id| *id)
                    .ok_or_else(|| invalid_semantic("function closure thunk id is out of range"))?;
                let boxed = self
                    .ids
                    .function_boxed_thunks
                    .get(function.0 as usize)
                    .and_then(|id| *id)
                    .ok_or_else(|| invalid_semantic("function boxed thunk id is out of range"))?;
                let sig = self
                    .ids
                    .function_sigs
                    .get(function.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("function signature id is out of range"))?;
                (lowered, boxed, sig)
            }
            sem::KnownCallable::GenericFunction(generic) => {
                let lowered = self
                    .ids
                    .generic_closure_thunks
                    .get(generic.0 as usize)
                    .and_then(|id| *id)
                    .ok_or_else(|| invalid_semantic("generic closure thunk id is out of range"))?;
                let boxed = self
                    .ids
                    .generic_boxed_thunks
                    .get(generic.0 as usize)
                    .and_then(|id| *id)
                    .ok_or_else(|| invalid_semantic("generic boxed thunk id is out of range"))?;
                let sig = self
                    .ids
                    .generic_sigs
                    .get(generic.0 as usize)
                    .copied()
                    .ok_or_else(|| invalid_semantic("generic signature id is out of range"))?;
                (lowered, boxed, sig)
            }
            sem::KnownCallable::SlotAccessor {
                accessor,
                class,
                slot,
            } => {
                let accessor = self.lower_accessor_function(accessor, class, slot)?;
                (accessor.closure_thunk, accessor.boxed_thunk, accessor.sig)
            }
        };
        self.emit_callable_new(function, BoxedEntry::Function(boxed), sig, &[], span)
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
            // A copy of a local that is assigned would go stale: sem shares
            // every local a closure refers to and `:=` assigns.
            if !capture.shared
                && self
                    .current_local(capture.source)
                    .is_some_and(|local| local.assigned)
            {
                return Err(invalid_semantic(
                    "a closure captures an assigned local that is not shared",
                ));
            }
            // A shared local's capture is its cell.
            let value = if capture.shared {
                value
            } else {
                self.coerce_value(value, self.value_kind(capture.kind)?, span)?
            };
            capture_regs.push(value.reg);
        }
        // A restart guard is never a callable value: the handler search
        // calls its closure directly.
        match self
            .ids
            .closure_boxed
            .get(closure.0 as usize)
            .copied()
            .flatten()
        {
            Some(boxed) => self.emit_callable_new(
                function,
                BoxedEntry::Function(boxed),
                sig,
                &capture_regs,
                span,
            ),
            None => {
                let result = self.alloc(ValueKind::Entry);
                self.instructions.push(Instr::ClosureNew {
                    dst: result.reg,
                    function,
                    captures: capture_regs,
                    span,
                });
                Ok(result)
            }
        }
    }

    /// Makes a callable value of signature `sig` whose typed entry is
    /// `typed` over `captures` ([`Instr::CallableNew`]).
    fn emit_callable_new(
        &mut self,
        typed: FunctionId,
        boxed: BoxedEntry,
        sig: CallableSigId,
        captures: &[Reg],
        span: Span,
    ) -> Result<Value, LowerError> {
        let signature = self
            .ids
            .call_sigs
            .get(sig.0 as usize)
            .ok_or_else(|| invalid_semantic("callable signature id is out of range"))?;
        let shape = callable_shape(
            &self.ids.call_sigs,
            &signature.params,
            &signature.keywords,
            &signature.rets,
        );
        let result = self.alloc(ValueKind::Closure(Some(sig)));
        self.proven_callables.push(result.reg);
        self.instructions.push(Instr::CallableNew {
            dst: result.reg,
            typed,
            boxed,
            shape,
            captures: captures.to_vec(),
            span,
        });
        Ok(result)
    }

    /// Opens the callable value `callable`: its typed entry, its boxed entry
    /// and its shape, those of them `wanted` asks for. A register whose
    /// shape the tape knows is opened in place; any other through
    /// [`FunctionKind::CallableParts`], whose parameter contract proves it.
    fn open_callable(
        &mut self,
        callable: Reg,
        wanted: [bool; 3],
        span: Span,
    ) -> [Option<Value>; 3] {
        let kinds = [ValueKind::Entry, ValueKind::Entry, ValueKind::U64];
        if self.callable_shape_known(callable) {
            let parts = [0, 1, 2].map(|index| wanted[index].then(|| self.alloc(kinds[index])));
            self.instructions.push(Instr::OpenCallable {
                typed: parts[0].map(|part| part.reg),
                boxed: parts[1].map(|part| part.reg),
                shape: parts[2].map(|part| part.reg),
                callable,
                span,
            });
            return parts;
        }
        let parts = kinds.map(|kind| self.alloc(kind));
        self.instructions.push(Instr::Call {
            dsts: parts.iter().map(|part| part.reg).collect(),
            callee: self.ids.callable_parts.unwrap_or(FunctionId(u32::MAX)),
            args: vec![callable],
            span,
        });
        [0, 1, 2].map(|index| wanted[index].then_some(parts[index]))
    }

    /// Gives `value`, a callable from untyped code, the typed entry `sig`
    /// promises: a callable value whose typed entry is `sig`'s adapter
    /// ([`Lowering::lower_adapter`]) over `value`'s boxed entry, which it
    /// shares. This is where an untyped callable enters typed code, so
    /// typed calls through it check what they rely on there.
    fn adapt(&mut self, value: Value, sig: CallableSigId, span: Span) -> Result<Value, LowerError> {
        let adapter = self
            .ids
            .adapters
            .iter()
            .find(|(adapted, _)| *adapted == sig)
            .map(|(_, adapter)| *adapter)
            .ok_or_else(|| {
                invalid_semantic("a typed callable entering from untyped code has no adapter")
            })?;
        let [_, Some(boxed), _] = self.open_callable(value.reg, [false, true, false], span) else {
            return Err(invalid_semantic(
                "opening a callable dropped its boxed entry",
            ));
        };
        self.emit_callable_new(
            adapter,
            BoxedEntry::Shared(boxed.reg),
            sig,
            &[boxed.reg],
            span,
        )
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

    /// Applies a callable sem knows to be `generic`, such as
    /// `apply(area, x)`: a send, as a call through the generic's value would
    /// make, but with the arguments as given, so a boxed `<object>` reaches
    /// dispatch (see [`Self::send`]) instead of a cast to the value's
    /// parameter kind. The callable is still evaluated, for its effects.
    fn lower_known_generic_call(
        &mut self,
        callable: &sem::Expr,
        generic: sem::GenericFunctionId,
        args: &[sem::Expr],
        result_kinds: &[sem::ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let generic = self
            .module
            .generics
            .get(generic.0 as usize)
            .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
        let _ = self.lower_expr(callable)?;
        let mut values = self.lower_generic_args(args, generic)?;
        // A send checks a limited parameter's range at its call site, which
        // sem inserts for a direct send; an applied one is checked here.
        for ((value, arg), range) in values.iter_mut().zip(args).zip(&generic.param_ranges) {
            if let Some(range) = *range
                && value.kind == ValueKind::I64
            {
                let type_error = self.type_error(
                    TypeErrorKind::Range,
                    range_expected(range),
                    None,
                    arg.span(),
                );
                *value = self.lower_range_check(*value, range, type_error, arg.span());
            }
        }
        let args = values;
        let results = self
            .value_kinds(&generic.result_kinds)?
            .into_iter()
            .map(|kind| self.alloc(kind))
            .collect::<Vec<_>>();
        self.send(
            generic,
            args,
            results.iter().map(|result| result.reg).collect(),
            span,
            None,
        )?;
        let kinds = self.value_kinds(result_kinds)?;
        if kinds.len() != results.len() {
            return Err(invalid_semantic(
                "applied generic result arity does not match its signature",
            ));
        }
        results
            .into_iter()
            .zip(kinds)
            .map(|(result, kind)| self.coerce_value(result, kind, span))
            .collect()
    }

    /// Calls the callable value `callable` with `args`, each first taken at
    /// its `arg_kinds` kind, and returns its results at `result_kinds`.
    ///
    /// A known generic sends ([`Self::lower_known_generic_call`]), and a
    /// block's body calls its own exit procedure as the exit it makes
    /// ([`Self::lower_exit_call`]). A callable of a known signature is
    /// called through its typed entry ([`Self::call_typed_entry`]), and an
    /// untyped one through whichever entry the call's shape picks
    /// ([`Self::call_untyped`]).
    fn lower_callable_results(
        &mut self,
        callable: &sem::Expr,
        args: &[sem::Expr],
        arg_kinds: &[sem::ValueKind],
        keywords: &[String],
        result_kinds: &[sem::ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        // A generic that takes keywords is called through its typed entry,
        // which fills the defaults of those passed as `$unsupplied`.
        if let sem::ValueKind::KnownCallable(sem::KnownCallable::GenericFunction(generic)) =
            self.module.expr_kind(callable)
            && keywords.is_empty()
        {
            return self.lower_known_generic_call(callable, generic, args, result_kinds, span);
        }
        if let Some(exit) = self.bound_exit(callable) {
            return self.lower_exit_call(exit, args, arg_kinds, result_kinds, span);
        }
        let callee = self.lower_expr(callable)?;
        let mut values = Vec::with_capacity(args.len());
        for (arg, expected) in args.iter().zip(arg_kinds) {
            let value = self.lower_expr(arg)?;
            let expected = self.value_kind(*expected)?;
            values.push(self.coerce_value(value, expected, arg.span())?);
        }
        let kinds = self.value_kinds(result_kinds)?;
        // Sem passes a typed callable its signature's kinds and every
        // keyword of it, in declaration order; a call of any other shape is
        // an untyped call's, whatever the value is known to be here.
        let arg_kinds = values.iter().map(|value| value.kind).collect::<Vec<_>>();
        let typed_keywords = |sig: CallableSigId| {
            self.ids
                .call_sigs
                .get(sig.0 as usize)
                .is_some_and(|signature| {
                    signature.params.len() == arg_kinds.len()
                        && signature
                            .params
                            .iter()
                            .zip(&arg_kinds)
                            .all(|(param, actual)| typed_arg_fits(*param, *actual))
                        && signature.keywords.len() == keywords.len()
                        && signature
                            .keywords
                            .iter()
                            .zip(keywords)
                            .all(|(declared, passed)| declared.name == *passed)
                })
        };
        match callee.kind {
            ValueKind::Closure(Some(sig)) if typed_keywords(sig) => {
                self.call_typed_entry(callee, values, sig, &kinds, span)
            }
            ValueKind::Closure(_) => self.call_untyped(callee, values, keywords, &kinds, span),
            _ => Err(invalid_semantic(
                "callable call target is not a callable value",
            )),
        }
    }

    /// Calls `callee`, a callable value of signature `sig`, through its
    /// typed entry, with `args` at their kinds, and returns its results at
    /// `kinds`. Static typing and the adapters at the places untyped
    /// callables enter typed code ([`Self::adapt`]) make the entry's
    /// signature agree with `sig`.
    fn call_typed_entry(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        sig: CallableSigId,
        kinds: &[ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let signature = self
            .ids
            .call_sigs
            .get(sig.0 as usize)
            .cloned()
            .ok_or_else(|| invalid_semantic("callable signature id is out of range"))?;
        let mut regs = Vec::with_capacity(args.len());
        for (arg, kind) in args.into_iter().zip(&signature.params) {
            regs.push(self.coerce_value(arg, *kind, span)?.reg);
        }
        let [Some(entry), _, _] = self.open_callable(callee.reg, [true, false, false], span) else {
            return Err(invalid_semantic(
                "opening a callable dropped its typed entry",
            ));
        };
        let results = signature
            .rets
            .iter()
            .map(|kind| self.alloc(*kind))
            .collect::<Vec<_>>();
        self.instructions.push(Instr::CallIndirect {
            dsts: results.iter().map(|result| result.reg).collect(),
            callee: entry.reg,
            args: regs,
            arg_kinds: signature.params,
            rets: signature.rets,
            span,
        });
        for result in &results {
            if matches!(result.kind, ValueKind::Closure(_)) {
                self.proven_callables.push(result.reg);
            }
        }
        results
            .into_iter()
            .zip(kinds)
            .map(|(result, kind)| self.coerce_value(result, *kind, span))
            .collect()
    }

    /// Calls `callee`, an untyped callable value, with `args` at their
    /// kinds, and returns its results at `kinds`.
    ///
    /// When the value's typed entry has exactly this call's shape
    /// ([`callable_shape`]), the call is a typed one. Otherwise it crosses
    /// under the boxed callable ABI ([`BOXED_WIRE`]): the arguments are
    /// boxed, the boxed entry checks and unboxes what the callee relies on,
    /// and each result comes back boxed and is unboxed to the kind the call
    /// site takes, a number or boolean of another kind signalling a
    /// `type-error.cast` here. Sem casts a class result itself, at a site
    /// typing classifies.
    fn call_untyped(
        &mut self,
        callee: Value,
        mut args: Vec<Value>,
        keywords: &[String],
        kinds: &[ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let results = kinds
            .iter()
            .map(|kind| self.alloc(*kind))
            .collect::<Vec<_>>();
        let arg_kinds = args.iter().map(|arg| arg.kind).collect::<Vec<_>>();
        let shape = callable_shape(&self.ids.call_sigs, &arg_kinds, &[], kinds);
        // A value whose signature lowering knows has a shape known here
        // too, and the tape checks a call of its typed entry against it.
        let known_mismatch = match callee.kind {
            ValueKind::Closure(Some(sig)) => {
                self.ids
                    .call_sigs
                    .get(sig.0 as usize)
                    .is_none_or(|signature| {
                        callable_shape(
                            &self.ids.call_sigs,
                            &signature.params,
                            &signature.keywords,
                            &signature.rets,
                        ) != shape
                    })
            }
            _ => false,
        };
        // A call passing keywords knows their names but not the types the
        // callee declares, which its typed entry's shape includes, so it
        // always crosses boxed.
        if !keywords.is_empty() || known_mismatch {
            let keyword_values = args.split_off(args.len().saturating_sub(keywords.len()));
            let mut wired = Vec::with_capacity(args.len() + 2);
            for arg in args {
                wired.push(self.wire_value(arg, span)?);
            }
            let mut boxed_values = Vec::with_capacity(keyword_values.len());
            for value in keyword_values {
                boxed_values.push(self.wire_value(value, span)?);
            }
            let (tail_names, tail_values) = self.keyword_tail(keywords, &boxed_values, span);
            wired.extend([tail_names, tail_values]);
            self.call_boxed_entry(callee, wired, &results, span)?;
            return Ok(results);
        }
        let [Some(typed_entry), _, Some(actual)] =
            self.open_callable(callee.reg, [true, false, true], span)
        else {
            return Err(invalid_semantic("opening a callable dropped a part"));
        };
        let (expected, same) = (self.alloc(ValueKind::U64), self.alloc(ValueKind::Bool));
        let (typed, boxed, join) = (self.alloc_label(), self.alloc_label(), self.alloc_label());
        self.instructions.extend([
            Instr::ConstU64 {
                dst: expected.reg,
                value: shape,
                span,
            },
            Instr::EqU64 {
                dst: same.reg,
                lhs: actual.reg,
                rhs: expected.reg,
                span,
            },
            Instr::Branch {
                cond: same.reg,
                then_label: typed,
                else_label: boxed,
                span,
            },
            Instr::Label { label: typed },
            Instr::CallIndirect {
                dsts: results.iter().map(|result| result.reg).collect(),
                callee: typed_entry.reg,
                args: args.iter().map(|arg| arg.reg).collect(),
                arg_kinds: arg_kinds.clone(),
                rets: kinds.to_vec(),
                span,
            },
            Instr::Jump { label: join, span },
            Instr::Label { label: boxed },
        ]);
        let mut wired = Vec::with_capacity(args.len() + 2);
        for arg in args {
            wired.push(self.wire_value(arg, span)?);
        }
        let (tail_names, tail_values) = self.keyword_tail(&[], &[], span);
        wired.extend([tail_names, tail_values]);
        self.call_boxed_entry(callee, wired, &results, span)?;
        self.instructions.extend([
            Instr::Jump { label: join, span },
            Instr::Label { label: join },
        ]);
        Ok(results)
    }

    /// Calls `callee`'s boxed entry with `wired`, the boxed arguments and
    /// the keyword tail, and moves each result, unboxed, into `results`.
    fn call_boxed_entry(
        &mut self,
        callee: Value,
        wired: Vec<Value>,
        results: &[Value],
        span: Span,
    ) -> Result<(), LowerError> {
        let [_, Some(boxed_entry), _] = self.open_callable(callee.reg, [false, true, false], span)
        else {
            return Err(invalid_semantic(
                "opening a callable dropped its boxed entry",
            ));
        };
        let rets = vec![BOXED_WIRE; results.len()];
        let boxed_results = rets
            .iter()
            .map(|kind| self.alloc(*kind))
            .collect::<Vec<_>>();
        self.instructions.push(Instr::CallIndirect {
            dsts: boxed_results.iter().map(|result| result.reg).collect(),
            callee: boxed_entry.reg,
            args: wired.iter().map(|arg| arg.reg).collect(),
            arg_kinds: wired.iter().map(|arg| arg.kind).collect(),
            rets,
            span,
        });
        for (result, boxed_result) in results.iter().zip(boxed_results) {
            let value = self.coerce_value(boxed_result, result.kind, span)?;
            self.note_callable_move(result.reg, value.reg);
            self.instructions.push(Instr::Move {
                dst: result.reg,
                src: value.reg,
                span,
            });
        }
        Ok(())
    }

    /// The exit point `callable` names, when it is the local a `block`
    /// whose body this is binds its exit procedure to.
    fn bound_exit(&self, callable: &sem::Expr) -> Option<BoundExit> {
        let sem::Expr::Local { local, .. } = callable else {
            return None;
        };
        self.exits
            .iter()
            .rev()
            .find(|(bound, _)| bound == local)
            .map(|(_, exit)| *exit)
    }

    /// Lowers a call of the exit procedure of a block whose body this is,
    /// named by its local, as the exit it makes: the exit procedure's own
    /// code ([`Lowering::lower_exit_thunk`]) inline, with the values at the
    /// block's kinds sem already checked them against, rather than an
    /// indirect call of it. The call never returns, so its results are
    /// unreachable registers nothing writes.
    fn lower_exit_call(
        &mut self,
        exit: BoundExit,
        args: &[sem::Expr],
        arg_kinds: &[sem::ValueKind],
        result_kinds: &[sem::ValueKind],
        span: Span,
    ) -> Result<Vec<Value>, LowerError> {
        let mut payload = vec![exit.token.reg];
        for (arg, expected) in args.iter().zip(arg_kinds) {
            let value = self.lower_expr(arg)?;
            let expected = self.value_kind(*expected)?;
            payload.push(self.coerce_value(value, expected, arg.span())?.reg);
        }
        let after = self.alloc_label();
        self.instructions.extend([
            Instr::CheckExit {
                token: exit.token.reg,
                span,
            },
            Instr::Throw {
                tag: ControlTag::Exit(exit.signature),
                payload,
                span,
            },
            // Code after the exit starts at a label nothing jumps to.
            Instr::Label { label: after },
        ]);
        self.value_kinds(result_kinds)?
            .into_iter()
            .map(|kind| {
                Ok(Value {
                    reachable: false,
                    ..self.alloc(kind)
                })
            })
            .collect()
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
        let mut op = runtime_op_for_intrinsic(intrinsic);
        let mut regs = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let value = self.lower_expr(arg)?;
            let value = if runtime_op_arg_is_any(op, index) {
                self.box_any(value, arg.span())?
            } else if matches!(value.kind, ValueKind::I32 | ValueKind::U32) {
                // Integer arguments are `<integer>`s.
                self.widen_integer(value, arg.span())
            } else {
                value
            };
            if op == RuntimeOp::FloatToString && value.kind == ValueKind::F32 {
                op = RuntimeOp::SingleFloatToString;
            }
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
        self.enter_region(
            vec![
                Catch::Tag {
                    tag: ControlTag::Restart,
                    target: restart_label,
                },
                Catch::All {
                    target: unwind_label,
                },
            ],
            span,
        );
        let body_values = self.lower_results_expr(body)?;
        self.instructions.push(Instr::LeaveRegion);
        if body_values.len() != results.len() {
            return Err(invalid_semantic("handle body arity does not match restart"));
        }
        let body_reachable = body_values.iter().all(|value| value.reachable);
        if body_reachable {
            self.instructions.push(Instr::PopBinding {
                token: token.reg,
                span,
            });
            self.move_results(&results, &body_values, body.span())?;
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
        let restart_reachable = restart_values.iter().all(|value| value.reachable);
        if restart_reachable {
            self.move_results(&results, &restart_values, restart.body.span())?;
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
    ) -> Result<Vec<Reg>, LowerError> {
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

    fn lower_generic_args(
        &mut self,
        args: &[sem::Expr],
        generic: &sem::GenericFunction,
    ) -> Result<Vec<Value>, LowerError> {
        let mut lowered = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let value = self.lower_expr(arg)?;
            lowered.push(self.coerce_generic_arg(value, generic, index, arg.span())?);
        }
        Ok(lowered)
    }

    /// Converts argument `index` of a send to `generic` to the kind the
    /// send passes it at, except that a boxed `<object>` for a parameter
    /// the dispatcher takes as a runtime object stays boxed: [`Self::send`]
    /// unboxes it once every argument is evaluated.
    fn coerce_generic_arg(
        &mut self,
        value: Value,
        generic: &sem::GenericFunction,
        index: usize,
        span: Span,
    ) -> Result<Value, LowerError> {
        let Some(expected) = generic.param_kinds.get(index) else {
            return Err(invalid_semantic(
                "generic call has more arguments than parameters",
            ));
        };
        let target = dispatcher_param_kind(self.module, generic, index);
        let dispatched = generic.dispatch_params.contains(&index);
        match *expected {
            Some(sem::ValueKind::Any) | None
                if value.kind == ValueKind::Any && matches!(target, ValueKind::Object(_)) =>
            {
                Ok(value)
            }
            Some(sem::ValueKind::Any) if dispatched => self.coerce_value(value, target, span),
            Some(expected) => {
                let expected = self.value_kind(expected)?;
                self.coerce_value(value, expected, span)
            }
            // Unannotated parameters keep their legacy pass-through for
            // non-object values unless the parameter carries boxed values.
            None if target == ValueKind::Any
                || (dispatched && matches!(value.kind, ValueKind::Object(_))) =>
            {
                self.coerce_value(value, target, span)
            }
            None => Ok(value),
        }
    }

    /// The numeric method a send at `site` may run inline instead of
    /// dispatching: typing proved the dispatch and selected the method from
    /// every argument's exact class.
    ///
    /// Only a built-in numeric generic's own methods qualify. The generic is
    /// sealed over `<real>` ([`sem::GenericFunctionOrigin::admits_method`]),
    /// so no method can be more specific for numbers, and its methods come
    /// from the promotion rules, so every code unit has the same ones; the
    /// runtime layout, which an install under live frames must keep, fixes
    /// the method set. Inlining therefore runs what dispatch would.
    fn inline_numeric_method(
        &self,
        generic: &sem::GenericFunction,
        site: sem::SiteId,
    ) -> Option<&'module sem::Method> {
        let decision = self.fact(site)?;
        if decision.verdict != typing::TypingVerdict::Proved
            || decision.rule != typing::TypingRule::DispatchCovered
            || generic.result_kinds.len() != 1
            || !matches!(generic.origin, sem::GenericFunctionOrigin::Numeric { .. })
        {
            return None;
        }
        let method = self
            .module
            .methods
            .get(decision.selected_method?.0 as usize)?;
        (method.generic == generic.id
            && matches!(method.origin, sem::MethodOrigin::Numeric { .. })
            && method.params.len() == generic.arity)
            .then_some(method)
    }

    /// Lowers a send of a built-in collection generic to its kernel
    /// method's operation, run inline, when typing proved the send selects
    /// that method and every argument already has its parameter's kind;
    /// otherwise returns `None`, having emitted nothing.
    ///
    /// The generic is sealed over the classes its kernel methods specialize
    /// on ([`sem::GenericFunctionOrigin::admits_method`]), so no method a
    /// program or a later install adds can be more specific for these
    /// arguments, and inlining runs what dispatch would.
    fn lower_kernel_collection_send(
        &mut self,
        generic: &sem::GenericFunction,
        args: &[sem::Expr],
        site: sem::SiteId,
        span: Span,
    ) -> Result<Option<Value>, LowerError> {
        if !matches!(
            generic.origin,
            sem::GenericFunctionOrigin::Collection { .. }
        ) {
            return Ok(None);
        }
        let Some(decision) = self.fact(site) else {
            return Ok(None);
        };
        if decision.verdict != typing::TypingVerdict::Proved
            || decision.rule != typing::TypingRule::DispatchCovered
        {
            return Ok(None);
        }
        let Some(method) = decision
            .selected_method
            .and_then(|method| self.module.methods.get(method.0 as usize))
        else {
            return Ok(None);
        };
        let sem::Expr::IntrinsicCall {
            intrinsic,
            result_kind,
            ..
        } = &method.body.result
        else {
            return Ok(None);
        };
        let sem::MethodOrigin::Collection { function } = method.origin else {
            return Ok(None);
        };
        if method.generic != generic.id || method.params.len() != args.len() {
            return Ok(None);
        }
        for (param, arg) in method.params.iter().zip(args) {
            let Some(local) = method.locals.get(param.0 as usize) else {
                return Ok(None);
            };
            let actual = self.module.expr_kind(arg);
            // An `<object>` parameter, a keyword, takes any value boxed.
            let fits = actual == local.kind
                || local.kind == sem::ValueKind::Any
                || (local.kind == sem::ValueKind::I64
                    && matches!(actual, sem::ValueKind::I32 | sem::ValueKind::U32));
            if !fits {
                return Ok(None);
            }
        }
        // A call that passes no keyword runs the operation that does not
        // test for `$unsupplied`.
        let (required, keywords) = args.split_at(function.arity().min(args.len()));
        let unsupplied = sem::BUILTIN_METHODS
            .iter()
            .find(|row| row.function == function && row.operation == *intrinsic)
            .and_then(|row| row.unsupplied)
            .filter(|_| keywords.iter().all(sem::Expr::is_unsupplied));
        let result = match unsupplied {
            Some(operation) => {
                self.lower_intrinsic_call(operation, required, *result_kind, span)?
            }
            None => self.lower_intrinsic_call(*intrinsic, args, *result_kind, span)?,
        };
        self.record(site, CheckCodegen::Inlined);
        Ok(Some(result))
    }

    /// Lowers `expr` if it is a send typing proved runs a built-in numeric
    /// method (see [`Self::inline_numeric_method`]), and `None`, having
    /// emitted nothing, otherwise.
    ///
    /// An inlined send's value is the method's own result, such as an
    /// unboxed `i64`, not yet converted to the generic's `<object>`: the
    /// caller converts it where it needs to. An operand that is itself such
    /// a send passes its result on the same way, so a chain of proved sends
    /// boxes only at its end. A send whose method cannot run inline after
    /// all dispatches, and its value has the generic's kind.
    fn lower_numeric_send(&mut self, expr: &sem::Expr) -> Result<Option<Value>, LowerError> {
        let sem::Expr::GenericCall {
            generic,
            args,
            site,
            span,
        } = expr
        else {
            return Ok(None);
        };
        let generic_info = self
            .module
            .generics
            .get(generic.0 as usize)
            .ok_or_else(|| invalid_semantic("generic id is out of range"))?;
        let Some(method) = self.inline_numeric_method(generic_info, *site) else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            let value = match self.lower_numeric_send(arg)? {
                Some(value) => value,
                None => self.lower_expr(arg)?,
            };
            values.push(value);
        }
        if let Some(result) = self.lower_inline_numeric(method, &values, *span)? {
            self.record(*site, CheckCodegen::Inlined);
            return Ok(Some(result));
        }
        let mut regs = Vec::with_capacity(values.len());
        for (index, (value, arg)) in values.into_iter().zip(args).enumerate() {
            regs.push(self.coerce_generic_arg(value, generic_info, index, arg.span())?);
        }
        let result = self.alloc(self.value_kind(generic_info.result_kinds[0])?);
        self.send(generic_info, regs, vec![result.reg], *span, Some(*site))?;
        Ok(Some(result))
    }

    /// Runs `method`'s body inline on `args`: each argument converts to its
    /// parameter's kind (unboxing a boxed number), the promotion rules'
    /// operation runs directly, and its result has the method's own kind;
    /// the caller converts it to the generic's. The body's own checks, such
    /// as overflow, stay.
    ///
    /// Returns `None`, having emitted nothing, when an argument has a kind
    /// that cannot convert to its parameter's.
    fn lower_inline_numeric(
        &mut self,
        method: &'module sem::Method,
        args: &[Value],
        span: Span,
    ) -> Result<Option<Value>, LowerError> {
        let mut params = Vec::with_capacity(args.len());
        for (param, arg) in method.params.iter().zip(args) {
            let local = method
                .locals
                .get(param.0 as usize)
                .ok_or_else(|| invalid_semantic("method parameter local id is out of range"))?;
            let kind = self.value_kind(local.kind)?;
            let converts = arg.kind == kind
                || arg.kind == ValueKind::Any
                || (kind == ValueKind::I64 && matches!(arg.kind, ValueKind::I32 | ValueKind::U32));
            if !converts {
                return Ok(None);
            }
            params.push((*param, kind));
        }
        // The generated body has no source of its own; what it does (and
        // any overflow it reports) belongs to the call.
        let Some(body) = method
            .body
            .statements
            .is_empty()
            .then(|| respan_numeric_body(&method.body.result, span))
            .flatten()
        else {
            return Ok(None);
        };
        let mut bound = Vec::with_capacity(args.len());
        for ((param, kind), arg) in params.into_iter().zip(args) {
            bound.push((param, self.coerce_value(*arg, kind, span)?));
        }
        let locals = core::mem::replace(&mut self.source_locals, &method.locals);
        let regs = core::mem::replace(&mut self.local_regs, vec![None; method.locals.len()]);
        // The method's own checks are decided by its typing, which knows
        // nothing of this call; they stay, and stay the method's sites.
        let owner = self.owner.take();
        let site_owner = self.site_owner.replace(method.definition);
        let result = (|| {
            for (param, value) in bound {
                self.set_local(param, value)?;
            }
            self.lower_expr(&body)
        })();
        self.source_locals = locals;
        self.local_regs = regs;
        self.owner = owner;
        self.site_owner = site_owner;
        result.map(Some)
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
        // A callable read from somewhere lowering types only by its register
        // (a slot, a generic's result) takes the signature sem checked it
        // against. The label is static: calls through any callable value
        // check the class results they rely on, and the tape checks the
        // register kinds.
        if matches!(
            (value.kind, expected),
            (ValueKind::Closure(None), ValueKind::Closure(Some(_)))
        ) {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        // Sem accepts a callable for another signature when its parameters
        // are at least as wide and its results at most as wide, and every
        // pair shares a register type, so the value only changes its static
        // signature. Calls through it check the class results they rely on.
        if let (ValueKind::Function(Some(actual)), ValueKind::Function(Some(wanted)))
        | (ValueKind::Closure(Some(actual)), ValueKind::Closure(Some(wanted))) =
            (value.kind, expected)
            && let (Some(actual), Some(wanted)) = (
                self.ids.call_sigs.get(actual.0 as usize),
                self.ids.call_sigs.get(wanted.0 as usize),
            )
            && callable_registers_agree(actual, wanted)
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
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
            let type_error = self.coercion_error(class_expected(expected_class), span);
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                class: expected_class,
                type_error,
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
        // `<i32>` and `<u32>` are `<integer>`s: they widen exactly, and an
        // `<integer>` narrows to them only when it is in range.
        if expected == ValueKind::I64 && matches!(value.kind, ValueKind::I32 | ValueKind::U32) {
            return Ok(self.widen_integer(value, span));
        }
        if value.kind == ValueKind::Any
            && matches!(expected, ValueKind::I32 | ValueKind::U32)
            && let Some(class) = scalar_concrete_class(self.module, expected)
        {
            let type_error = self.coercion_error(class_expected(class), span);
            let integer = self.unbox(value, class, ValueKind::I64, type_error, span);
            let type_error = Box::new(TypeErrorSite {
                kind: TypeErrorKind::Range,
                expected: kind_expected(self.module, expected),
                ..*self.coercion_error(kind_expected(self.module, expected), span)
            });
            return Ok(self.narrow_integer(integer, expected, type_error, span));
        }
        // A callable comes out of its box untyped; a typed one is adapted,
        // so calls through it check what crossed.
        if value.kind == ValueKind::Any
            && let ValueKind::Closure(sig) = expected
            && let Some(class) = scalar_concrete_class(self.module, expected)
        {
            let type_error = self.coercion_error(class_expected(class), span);
            let callable = self.unbox(value, class, ValueKind::Closure(None), type_error, span);
            return match sig {
                Some(sig) => self.adapt(callable, sig, span),
                None => Ok(callable),
            };
        }
        if value.kind == ValueKind::Any
            && let Some(class) = scalar_concrete_class(self.module, expected)
        {
            let type_error = self.coercion_error(class_expected(class), span);
            return Ok(self.unbox(value, class, expected, type_error, span));
        }
        // A class object of a narrower bound already names a class within
        // the wider one; any other value, such as a dispatched argument a
        // method specialized only on `<class>`, is checked.
        if let (ValueKind::ClassObject(Some(actual)), ValueKind::ClassObject(Some(bound))) =
            (value.kind, expected)
            && self.module.is_same_or_subclass(actual, bound)
        {
            return Ok(Value {
                kind: expected,
                ..value
            });
        }
        if let ValueKind::ClassObject(Some(bound)) = expected
            && matches!(
                value.kind,
                ValueKind::Any | ValueKind::Object(_) | ValueKind::ClassObject(_)
            )
        {
            let type_error = self.coercion_error(class_object_expected(bound), span);
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastClassObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                bound,
                type_error,
                span,
            });
            return Ok(casted);
        }
        if matches!(value.kind, ValueKind::Any | ValueKind::Object(None))
            && let ValueKind::Object(Some(class)) = expected
        {
            let type_error = self.coercion_error(class_expected(class), span);
            let casted = self.alloc(expected);
            self.instructions.push(Instr::CastObject {
                dst: casted.reg,
                value: value.reg,
                input: value.kind,
                class,
                type_error,
                span,
            });
            return Ok(casted);
        }
        Err(invalid_semantic(
            "semantic type checking allowed an invalid lowered coercion",
        ))
    }

    /// The `<type-error>` a check that coerces a value to what its context
    /// takes signals. Typing classifies no such check, so it is named by
    /// its span.
    fn coercion_error(&self, expected: TypeObject, span: Span) -> Box<TypeErrorSite> {
        self.type_error(TypeErrorKind::Cast, expected, None, span)
    }

    fn unbox(
        &mut self,
        value: Value,
        class: sem::ClassId,
        kind: ValueKind,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Value {
        let result = self.alloc(kind);
        self.instructions.push(Instr::UnboxValue {
            dst: result.reg,
            value: value.reg,
            class,
            kind,
            type_error,
            span,
        });
        result
    }

    /// Adds a parameter of `kind` named `name` to a function built without a
    /// source body, such as a thunk.
    fn push_param(&mut self, name: &str, kind: ValueKind) -> Value {
        let value = self.alloc(kind);
        if matches!(kind, ValueKind::Closure(_)) {
            self.proven_callables.push(value.reg);
        }
        self.params.push(Param {
            source: None,
            name: name.into(),
            kind,
            reg: value.reg,
        });
        value
    }

    /// Puts `value` in the kind it crosses a call through a callable value
    /// at ([`BOXED_WIRE`]): boxed.
    fn wire_value(&mut self, value: Value, span: Span) -> Result<Value, LowerError> {
        self.box_any(value, span)
    }

    /// Takes `value`, an argument that crossed a callable value's boxed
    /// entry at its [`BOXED_WIRE`] kind, as a parameter of `kind` and
    /// `range`: the entry check of [`sem::EntryCheck`].
    ///
    /// A boxed `<object>` is unboxed to a number or boolean, or cast to a
    /// class or class object. A callable for a typed callable parameter is
    /// adapted to its signature ([`Self::adapt`]): this is where an untyped
    /// callable enters typed code. A value of another class or kind signals a
    /// `type-error.entry` at `site`, and an `<integer>` outside `range` or
    /// outside an `<i32>` or `<u32>` a `type-error.range`. A `use-value`
    /// replacement continues the check as any check's does: a number of the
    /// register's kind is taken as it is (and then range-checked), and an
    /// object or a limited integer is checked again.
    fn unwire(
        &mut self,
        value: Value,
        kind: ValueKind,
        range: Option<IntRange>,
        site: &str,
        span: Span,
    ) -> Result<Value, LowerError> {
        let error = |kind: TypeErrorKind, expected: TypeObject| {
            type_error_site(kind, expected, String::from(site), span)
        };
        match (value.kind, kind) {
            (ValueKind::Any, ValueKind::Closure(sig)) => {
                let class = scalar_concrete_class(self.module, kind)
                    .ok_or_else(|| invalid_semantic("the <function> class is missing"))?;
                let callable = self.unbox(
                    value,
                    class,
                    ValueKind::Closure(None),
                    error(TypeErrorKind::Entry, kind_expected(self.module, kind)),
                    span,
                );
                // The host hands back an aggregate the tape knows only as
                // one, so the value opens as a slot's callable does.
                match sig {
                    Some(sig) => self.adapt(callable, sig, span),
                    None => Ok(callable),
                }
            }
            (ValueKind::Any, ValueKind::Any) => Ok(value),
            (
                ValueKind::Any,
                ValueKind::I64 | ValueKind::F64 | ValueKind::F32 | ValueKind::Bool,
            ) => {
                let class = scalar_concrete_class(self.module, kind)
                    .ok_or_else(|| invalid_semantic("a number class is missing"))?;
                let expected = match range {
                    Some(range) if kind == ValueKind::I64 => range_expected(range),
                    _ => kind_expected(self.module, kind),
                };
                let unboxed = self.unbox(
                    value,
                    class,
                    kind,
                    error(TypeErrorKind::Entry, expected),
                    span,
                );
                let Some(range) = range.filter(|_| kind == ValueKind::I64) else {
                    return Ok(unboxed);
                };
                let type_error = error(TypeErrorKind::Range, expected);
                let checked = self
                    .code()
                    .range_check(unboxed.reg, range, type_error, span);
                self.next_reg = register_at(self.register_kinds.len());
                Ok(Value {
                    reg: checked,
                    ..unboxed
                })
            }
            (ValueKind::Any, ValueKind::I32 | ValueKind::U32) => {
                let class = scalar_concrete_class(self.module, kind)
                    .ok_or_else(|| invalid_semantic("the integer class is missing"))?;
                let integer = self.unbox(
                    value,
                    class,
                    ValueKind::I64,
                    error(TypeErrorKind::Entry, kind_expected(self.module, kind)),
                    span,
                );
                let type_error = error(TypeErrorKind::Range, kind_expected(self.module, kind));
                Ok(self.narrow_integer(integer, kind, type_error, span))
            }
            (ValueKind::Any, ValueKind::Object(Some(class))) => {
                let type_error = error(TypeErrorKind::Entry, class_expected(class));
                let casted = self.alloc(kind);
                self.instructions.push(Instr::CastObject {
                    dst: casted.reg,
                    value: value.reg,
                    input: value.kind,
                    class,
                    type_error,
                    span,
                });
                Ok(casted)
            }
            (ValueKind::Any, ValueKind::ClassObject(Some(bound))) => {
                let type_error = error(TypeErrorKind::Entry, class_object_expected(bound));
                let casted = self.alloc(kind);
                self.instructions.push(Instr::CastClassObject {
                    dst: casted.reg,
                    value: value.reg,
                    input: value.kind,
                    bound,
                    type_error,
                    span,
                });
                Ok(casted)
            }
            _ => Err(invalid_semantic(
                "a callable's parameter kind has no boxed callable ABI form",
            )),
        }
    }

    fn box_any(&mut self, value: Value, span: Span) -> Result<Value, LowerError> {
        if value.kind == ValueKind::Any {
            return Ok(value);
        }
        // A refined local's scalar is the value its box holds.
        if let Some((_, boxed)) = self
            .unboxed_locals
            .iter()
            .rev()
            .find(|(unboxed, _)| *unboxed == value.reg)
        {
            return Ok(Value {
                reg: *boxed,
                kind: ValueKind::Any,
                ..value
            });
        }
        // A boxed `<i32>` or `<u32>` is the `<integer>` it is.
        let value = if matches!(value.kind, ValueKind::I32 | ValueKind::U32) {
            self.widen_integer(value, span)
        } else {
            value
        };
        if matches!(
            value.kind,
            ValueKind::U64 | ValueKind::Function(_) | ValueKind::Entry | ValueKind::Aggregate
        ) {
            return Err(invalid_semantic(
                "runtime metadata cannot be boxed as <object>",
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

    /// Emits one `0xFD` instruction with a single operand.
    fn num32_unary(&mut self, op: Num32UnaryOp, src: Reg, kind: ValueKind, span: Span) -> Value {
        let result = self.alloc(kind);
        self.instructions.push(Instr::Num32Unary {
            dst: result.reg,
            op,
            src,
            span,
        });
        result
    }

    /// Lowers `instance?(value, <i32>)`, `<u32>` or another limited integer
    /// type: whether the value is an `<integer>` in the type's range.
    fn lower_limited_membership(&mut self, value: Value, range: IntRange, span: Span) -> Value {
        let const_bool = |this: &mut Self, value| {
            let result = this.alloc(ValueKind::Bool);
            this.instructions.push(Instr::ConstBool {
                dst: result.reg,
                value,
                span,
            });
            result
        };
        let const32 = |this: &mut Self, value| {
            let constant = this.alloc(match value {
                sem::Number32::I32(_) => ValueKind::I32,
                _ => ValueKind::U32,
            });
            this.instructions.push(Instr::Const32 {
                dst: constant.reg,
                value,
                span,
            });
            constant
        };
        match (value.kind, range.limited()) {
            (ValueKind::I32, _) if range.includes(IntRange::I32) => const_bool(self, true),
            (ValueKind::U32, _) if range.includes(IntRange::U32) => const_bool(self, true),
            (ValueKind::I32, Some(sem::LimitedInteger::U32)) => {
                let zero = const32(self, sem::Number32::I32(0));
                self.num32_binary(
                    Num32BinaryOp::I32Ge,
                    value.reg,
                    zero.reg,
                    ValueKind::Bool,
                    span,
                )
            }
            (ValueKind::U32, Some(sem::LimitedInteger::I32)) => {
                let max = const32(self, sem::Number32::U32(i32::MAX.cast_unsigned()));
                self.num32_binary(
                    Num32BinaryOp::U32Le,
                    value.reg,
                    max.reg,
                    ValueKind::Bool,
                    span,
                )
            }
            (ValueKind::I32 | ValueKind::U32, _) => {
                let integer = self.widen_integer(value, span);
                self.lower_integer_in_range(integer, range, span)
            }
            (ValueKind::I64, _) => self.lower_integer_in_range(value, range, span),
            (ValueKind::Any, _) => {
                // Test the class first: only an `<integer>` unboxes.
                let result = self.alloc(ValueKind::Bool);
                let is_integer = self.alloc(ValueKind::Bool);
                let integer_class = builtin_class(self.module, sem::BuiltinValueClass::Integer)
                    .unwrap_or(sem::ClassId(u32::MAX));
                self.instructions.push(Instr::InstanceOf {
                    dst: is_integer.reg,
                    value: value.reg,
                    input: value.kind,
                    class: integer_class,
                    span,
                });
                let [check, outside, done] = core::array::from_fn(|_| self.alloc_label());
                self.instructions.extend([
                    Instr::Branch {
                        cond: is_integer.reg,
                        then_label: check,
                        else_label: outside,
                        span,
                    },
                    Instr::Label { label: check },
                ]);
                // The `instance?` test just passed, so this cannot fail.
                let type_error = self.coercion_error(class_expected(integer_class), span);
                let integer = self.unbox(value, integer_class, ValueKind::I64, type_error, span);
                let inside = self.lower_integer_in_range(integer, range, span);
                self.instructions.extend([
                    Instr::Move {
                        dst: result.reg,
                        src: inside.reg,
                        span,
                    },
                    Instr::Jump { label: done, span },
                    Instr::Label { label: outside },
                    Instr::ConstBool {
                        dst: result.reg,
                        value: false,
                        span,
                    },
                    Instr::Jump { label: done, span },
                    Instr::Label { label: done },
                ]);
                result
            }
            // Floats and objects are never `<integer>`s.
            _ => const_bool(self, false),
        }
    }

    /// Tests whether an `<integer>` lies in a limited type's range: one
    /// comparison per bound, the second only when the first holds.
    fn lower_integer_in_range(&mut self, value: Value, range: IntRange, span: Span) -> Value {
        let result = self.alloc(ValueKind::Bool);
        let done = self.alloc_label();
        let bounds = [
            range.min.map(|min| (min, sem::CompareOp::Ge)),
            range.max.map(|max| (max, sem::CompareOp::Le)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if bounds.is_empty() {
            self.instructions.push(Instr::ConstBool {
                dst: result.reg,
                value: true,
                span,
            });
        }
        for (index, (bound, op)) in bounds.iter().copied().enumerate() {
            let constant = self.alloc(ValueKind::I64);
            self.instructions.extend([
                Instr::ConstI64 {
                    dst: constant.reg,
                    value: bound,
                    span,
                },
                Instr::I64Compare {
                    dst: result.reg,
                    op,
                    lhs: value.reg,
                    rhs: constant.reg,
                    span,
                },
            ]);
            if index + 1 < bounds.len() {
                let next = self.alloc_label();
                self.instructions.extend([
                    Instr::Branch {
                        cond: result.reg,
                        then_label: next,
                        else_label: done,
                        span,
                    },
                    Instr::Label { label: next },
                ]);
            }
        }
        self.instructions.extend([
            Instr::Jump { label: done, span },
            Instr::Label { label: done },
        ]);
        result
    }

    /// Checks that the `<integer>` `value` is in a limited type's `range`;
    /// see [`CodeBuffer::range_check`].
    fn lower_range_check(
        &mut self,
        value: Value,
        range: IntRange,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Value {
        let checked = self.code().range_check(value.reg, range, type_error, span);
        self.next_reg = register_at(self.register_kinds.len());
        Value {
            reg: checked,
            kind: ValueKind::I64,
            reachable: true,
        }
    }

    /// This body's registers, labels and instructions, to emit code shared
    /// with functions lowered without a body.
    fn code(&mut self) -> CodeBuffer<'_> {
        CodeBuffer {
            register_kinds: &mut self.register_kinds,
            next_label: &mut self.next_label,
            instructions: &mut self.instructions,
        }
    }

    /// Lowers a wrapping operator (`+%`, `-%`, `*%`) or a bitwise function
    /// (`logand`, `logior`, `logxor`, `shift-left`, `shift-right`); none of
    /// them can overflow.
    fn lower_bit_operation(
        &mut self,
        op: sem::BinaryOp,
        kind: sem::NumericKind,
        lhs: Value,
        rhs: Reg,
        span: Span,
    ) -> Result<Value, LowerError> {
        use sem::BinaryOp as B;
        use sem::NumericKind as N;
        if kind == N::Integer {
            let op = match op {
                B::LogAnd => I64BitOp::And,
                B::LogIor => I64BitOp::Or,
                B::LogXor => I64BitOp::Xor,
                _ => {
                    return Err(invalid_semantic(
                        "`<integer>` has no wrapping or shift lowering",
                    ));
                }
            };
            let result = self.alloc(ValueKind::I64);
            self.instructions.push(Instr::I64Bitwise {
                dst: result.reg,
                op,
                lhs: lhs.reg,
                rhs,
                span,
            });
            return Ok(result);
        }
        let op = match (kind, op) {
            (N::I32, B::WrappingAdd) => Num32BinaryOp::I32Add,
            (N::I32, B::WrappingSub) => Num32BinaryOp::I32Sub,
            (N::I32, B::WrappingMul) => Num32BinaryOp::I32Mul,
            (N::I32, B::LogAnd) => Num32BinaryOp::I32And,
            (N::I32, B::LogIor) => Num32BinaryOp::I32Or,
            (N::I32, B::LogXor) => Num32BinaryOp::I32Xor,
            (N::I32, B::ShiftLeft) => Num32BinaryOp::I32Shl,
            (N::I32, B::ShiftRight) => Num32BinaryOp::I32Shr,
            (N::U32, B::WrappingAdd) => Num32BinaryOp::U32Add,
            (N::U32, B::WrappingSub) => Num32BinaryOp::U32Sub,
            (N::U32, B::WrappingMul) => Num32BinaryOp::U32Mul,
            (N::U32, B::LogAnd) => Num32BinaryOp::U32And,
            (N::U32, B::LogIor) => Num32BinaryOp::U32Or,
            (N::U32, B::LogXor) => Num32BinaryOp::U32Xor,
            (N::U32, B::ShiftLeft) => Num32BinaryOp::U32Shl,
            (N::U32, B::ShiftRight) => Num32BinaryOp::U32Shr,
            _ => {
                return Err(invalid_semantic(
                    "wrapping and bit operations need one fixed-width integer type",
                ));
            }
        };
        Ok(self.num32_binary(op, lhs.reg, rhs, lhs.kind, span))
    }

    /// Lowers a shift whose count is checked: unless the count is in
    /// 0..=31 it signals `type_error`, and a `use-value` restart supplies
    /// the shift's result. The tape masks counts with `& 31`; Loko never
    /// relies on that, so `shift-left(x, 32)` cannot quietly mean `x`.
    fn lower_checked_shift(
        &mut self,
        op: sem::BinaryOp,
        kind: sem::NumericKind,
        lhs: Value,
        count: Reg,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Result<Value, LowerError> {
        // Reading an `i32` count as `u32` sends negative counts above 31, so
        // one unsigned comparison tests both bounds.
        let unsigned = if kind == sem::NumericKind::U32 {
            count
        } else {
            self.num32_unary(Num32UnaryOp::I32ToU32Wrap, count, ValueKind::U32, span)
                .reg
        };
        let bound = self.alloc(ValueKind::U32);
        self.instructions.push(Instr::Const32 {
            dst: bound.reg,
            value: sem::Number32::U32(32),
            span,
        });
        let in_range = self.num32_binary(
            Num32BinaryOp::U32Lt,
            unsigned,
            bound.reg,
            ValueKind::Bool,
            span,
        );
        let [outside, shift, done] = core::array::from_fn(|_| self.alloc_label());
        self.instructions.push(Instr::Branch {
            cond: in_range.reg,
            then_label: shift,
            else_label: outside,
            span,
        });
        self.instructions.push(Instr::Label { label: shift });
        let result = self.lower_bit_operation(op, kind, lhs, count, span)?;
        self.instructions.extend([
            Instr::Jump { label: done, span },
            Instr::Label { label: outside },
            Instr::SignalTypeError {
                dst: result.reg,
                values: vec![count],
                type_error,
                resume: done,
                span,
            },
            Instr::Label { label: done },
        ]);
        Ok(result)
    }

    /// Emits one `0xFD` instruction with two operands.
    fn num32_binary(
        &mut self,
        op: Num32BinaryOp,
        lhs: Reg,
        rhs: Reg,
        kind: ValueKind,
        span: Span,
    ) -> Value {
        let result = self.alloc(kind);
        self.instructions.push(Instr::Num32Binary {
            dst: result.reg,
            op,
            lhs,
            rhs,
            span,
        });
        result
    }

    /// Lowers checked `<i32>` or `<u32>` arithmetic: the overflow-reporting
    /// instruction, then the same overflow signal as `<integer>`
    /// arithmetic, because a limited type promises its range.
    fn lower_checked_num32_arith(
        &mut self,
        op: Num32CheckedOp,
        lhs: Value,
        rhs: Reg,
        operands: Vec<Reg>,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Value {
        let result = self.alloc(lhs.kind);
        let overflow = self.alloc(ValueKind::Bool);
        self.instructions.push(Instr::Num32Checked {
            dst: result.reg,
            overflow: overflow.reg,
            op,
            lhs: lhs.reg,
            rhs,
            span,
        });
        self.signal_if(overflow.reg, result.reg, operands, type_error, span);
        result
    }

    /// Emits the checked conversion `op` of `src`, returning the converted
    /// value and the register of its inexact flag.
    fn convert_checked(&mut self, op: ConvertCheckedOp, src: Reg, span: Span) -> (Value, Reg) {
        let result = self.alloc(op.result_kind());
        let inexact = self.alloc(ValueKind::Bool);
        self.instructions.push(Instr::ConvertChecked {
            dst: result.reg,
            inexact: inexact.reg,
            op,
            src,
            span,
        });
        (result, inexact.reg)
    }

    /// Converts a number to `to`; `site` is the conversion's check site.
    ///
    /// Widening (`<i32>` or `<u32>` to `<integer>`, `<single-float>` to
    /// `<double-float>`) is exact. Integer to float rounds to nearest, ties
    /// to even, once. Binary64 to binary32 rounds the same way. Narrowing an
    /// integer signals a `<type-error>` unless the value is in range, and
    /// float to integer is exact: it signals unless the float is integral
    /// and in range, so rounding is always an explicit `round`/`floor`/...
    /// choice. A `use-value` restart supplies the converted value.
    fn convert_number(
        &mut self,
        value: Value,
        to: sem::NumericKind,
        site: Option<sem::SiteId>,
        span: Span,
    ) -> Result<Value, LowerError> {
        use sem::NumericKind as N;
        let from = match value.kind {
            ValueKind::I64 => N::Integer,
            ValueKind::F64 => N::Float,
            ValueKind::I32 => N::I32,
            ValueKind::U32 => N::U32,
            ValueKind::F32 => N::SingleFloat,
            _ => return Err(invalid_semantic("conversion operand is not a number")),
        };
        let unary =
            |this: &mut Self, op, value: Value, kind| this.num32_unary(op, value.reg, kind, span);
        let range_error = |this: &Self, to: ValueKind| {
            this.type_error(
                TypeErrorKind::Range,
                kind_expected(this.module, to),
                site,
                span,
            )
        };
        Ok(match (from, to) {
            (from, to) if from == to => value,
            (N::I32 | N::U32, N::Integer) => self.widen_integer(value, span),
            (N::Integer, N::I32) => {
                let type_error = range_error(self, ValueKind::I32);
                self.narrow_integer(value, ValueKind::I32, type_error, span)
            }
            (N::Integer, N::U32) => {
                let type_error = range_error(self, ValueKind::U32);
                self.narrow_integer(value, ValueKind::U32, type_error, span)
            }
            (N::I32, N::U32) | (N::U32, N::I32) => {
                let to = if to == N::U32 {
                    ValueKind::U32
                } else {
                    ValueKind::I32
                };
                let type_error = range_error(self, to);
                self.change_sign(value, to, type_error, span)
            }
            (N::Integer, N::Float) => {
                let result = self.alloc(ValueKind::F64);
                self.instructions.push(Instr::I64ToF64 {
                    dst: result.reg,
                    src: value.reg,
                    span,
                });
                result
            }
            (N::I32, N::Float) => unary(self, Num32UnaryOp::I32ToF64, value, ValueKind::F64),
            (N::U32, N::Float) => unary(self, Num32UnaryOp::U32ToF64, value, ValueKind::F64),
            (N::Integer, N::SingleFloat) => {
                unary(self, Num32UnaryOp::I64ToF32, value, ValueKind::F32)
            }
            (N::I32, N::SingleFloat) => unary(self, Num32UnaryOp::I32ToF32, value, ValueKind::F32),
            (N::U32, N::SingleFloat) => unary(self, Num32UnaryOp::U32ToF32, value, ValueKind::F32),
            (N::SingleFloat, N::Float) => {
                unary(self, Num32UnaryOp::F32ToF64, value, ValueKind::F64)
            }
            (N::Float, N::SingleFloat) => {
                unary(self, Num32UnaryOp::F64ToF32, value, ValueKind::F32)
            }
            (N::SingleFloat | N::Float, N::Integer | N::I32 | N::U32) => {
                let expected = match to {
                    N::I32 => ValueKind::I32,
                    N::U32 => ValueKind::U32,
                    _ => ValueKind::I64,
                };
                let type_error = range_error(self, expected);
                self.lower_exact_float_to_integer(value, to, type_error, span)
            }
            (from, to) => {
                return Err(invalid_semantic(&alloc::format!(
                    "no conversion from {} to {}",
                    from.type_name(),
                    to.type_name()
                )));
            }
        })
    }

    /// Converts an integer that typing proved is in `to`'s range, without the
    /// range check, or returns `None`, having emitted nothing, when the
    /// conversion has no check to drop.
    fn convert_in_range(
        &mut self,
        value: Value,
        to: sem::NumericKind,
        span: Span,
    ) -> Option<Value> {
        use sem::NumericKind as N;
        let unary =
            |this: &mut Self, op, value: Value, kind| this.num32_unary(op, value.reg, kind, span);
        Some(match (value.kind, to) {
            (ValueKind::I64, N::I32) => {
                unary(self, Num32UnaryOp::I64ToI32Wrap, value, ValueKind::I32)
            }
            // The inexact flag is known clear, so it is left unread.
            (ValueKind::I64, N::U32) => {
                self.convert_checked(ConvertCheckedOp::I64ToU32Ovf, value.reg, span)
                    .0
            }
            (ValueKind::I32, N::U32) => {
                unary(self, Num32UnaryOp::I32ToU32Wrap, value, ValueKind::U32)
            }
            (ValueKind::U32, N::I32) => {
                unary(self, Num32UnaryOp::U32ToI32Wrap, value, ValueKind::I32)
            }
            _ => return None,
        })
    }

    /// Widens an `<i32>` or `<u32>` to the `<integer>` it is; exact.
    fn widen_integer(&mut self, value: Value, span: Span) -> Value {
        let op = match value.kind {
            ValueKind::U32 => Num32UnaryOp::U32ToI64,
            _ => Num32UnaryOp::I32ToI64,
        };
        self.num32_unary(op, value.reg, ValueKind::I64, span)
    }

    /// Narrows an `<integer>` to `<i32>` or `<u32>`, signalling
    /// `type_error` unless the value is in the type's range.
    fn narrow_integer(
        &mut self,
        value: Value,
        to: ValueKind,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Value {
        let op = if to == ValueKind::I32 {
            ConvertCheckedOp::I64ToI32Ovf
        } else {
            ConvertCheckedOp::I64ToU32Ovf
        };
        let (narrow, inexact) = self.convert_checked(op, value.reg, span);
        self.signal_if(inexact, narrow.reg, vec![value.reg], type_error, span);
        narrow
    }

    /// Converts between `<i32>` and `<u32>`, signalling `type_error` unless
    /// the value is in `to`'s range: non-negative either way.
    fn change_sign(
        &mut self,
        value: Value,
        to: ValueKind,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Value {
        let op = if to == ValueKind::U32 {
            ConvertCheckedOp::I32ToU32Ovf
        } else {
            ConvertCheckedOp::U32ToI32Ovf
        };
        let (converted, inexact) = self.convert_checked(op, value.reg, span);
        self.signal_if(inexact, converted.reg, vec![value.reg], type_error, span);
        converted
    }

    fn alloc(&mut self, kind: ValueKind) -> Value {
        let value = Value {
            reg: self.next_reg,
            kind,
            reachable: true,
        };
        assert_eq!(register_at(self.register_kinds.len()), self.next_reg);
        self.register_kinds.push(kind);
        self.next_reg = register_at(self.register_kinds.len());
        value
    }

    fn lower_float_call(
        &mut self,
        builtin: sem::FloatBuiltin,
        args: &[sem::Expr],
        span: Span,
    ) -> Result<Value, LowerError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.lower_expr(arg)?);
        }
        if values
            .first()
            .is_some_and(|value| value.kind == ValueKind::F32)
        {
            return self.lower_single_float_call(builtin, &values, span);
        }
        let regs = values.iter().map(|value| value.reg).collect::<Vec<_>>();
        let operand = |index: usize| {
            regs.get(index)
                .copied()
                .ok_or_else(|| invalid_semantic("float builtin argument is missing"))
        };
        let binary = |op| -> Result<(F64BinaryOp, Reg, Reg), LowerError> {
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

    /// Lowers a float builtin on `<single-float>` arguments: inline `f32`
    /// instructions where the tape has them, and otherwise the binary64
    /// library function on exactly widened arguments, rounded once back.
    fn lower_single_float_call(
        &mut self,
        builtin: sem::FloatBuiltin,
        args: &[Value],
        span: Span,
    ) -> Result<Value, LowerError> {
        let operand = |index: usize| {
            args.get(index)
                .map(|value| value.reg)
                .ok_or_else(|| invalid_semantic("float builtin argument is missing"))
        };
        let unary = |this: &mut Self, op| -> Result<Value, LowerError> {
            Ok(this.num32_unary(op, operand(0)?, ValueKind::F32, span))
        };
        let binary = |this: &mut Self, op| -> Result<Value, LowerError> {
            Ok(this.num32_binary(op, operand(0)?, operand(1)?, ValueKind::F32, span))
        };
        match builtin {
            sem::FloatBuiltin::Abs => unary(self, Num32UnaryOp::F32Abs),
            sem::FloatBuiltin::Floor => unary(self, Num32UnaryOp::F32Floor),
            sem::FloatBuiltin::Ceiling => unary(self, Num32UnaryOp::F32Ceil),
            sem::FloatBuiltin::Round => unary(self, Num32UnaryOp::F32Nearest),
            sem::FloatBuiltin::Truncate => unary(self, Num32UnaryOp::F32Trunc),
            sem::FloatBuiltin::Sqrt => unary(self, Num32UnaryOp::F32Sqrt),
            sem::FloatBuiltin::Min => binary(self, Num32BinaryOp::F32Min),
            sem::FloatBuiltin::Max => binary(self, Num32BinaryOp::F32Max),
            sem::FloatBuiltin::Remainder => binary(self, Num32BinaryOp::F32Rem),
            sem::FloatBuiltin::IsNan => {
                // NaN is the only value unequal to itself.
                let x = operand(0)?;
                Ok(self.num32_binary(Num32BinaryOp::F32Ne, x, x, ValueKind::Bool, span))
            }
            sem::FloatBuiltin::IsFinite => {
                // `abs(x) < inf` is false exactly for infinities and NaN.
                let magnitude = unary(self, Num32UnaryOp::F32Abs)?;
                let infinity = self.alloc(ValueKind::F32);
                self.instructions.push(Instr::Const32 {
                    dst: infinity.reg,
                    value: sem::Number32::F32(sem::SingleFloatBits::from_f32(f32::INFINITY)),
                    span,
                });
                Ok(self.num32_binary(
                    Num32BinaryOp::F32Lt,
                    magnitude.reg,
                    infinity.reg,
                    ValueKind::Bool,
                    span,
                ))
            }
            sem::FloatBuiltin::Library(function) => {
                let wide = args
                    .iter()
                    .map(|arg| {
                        self.num32_unary(Num32UnaryOp::F32ToF64, arg.reg, ValueKind::F64, span)
                            .reg
                    })
                    .collect();
                let result = self.alloc(ValueKind::F64);
                self.instructions.push(Instr::FloatCall {
                    dst: result.reg,
                    function,
                    args: wide,
                    span,
                });
                Ok(self.num32_unary(Num32UnaryOp::F64ToF32, result.reg, ValueKind::F32, span))
            }
        }
    }

    /// Lowers an exact float-to-integer conversion such as
    /// `as(<integer>, x)`, signalling `type_error` unless `x` is integral
    /// and in `to`'s range. A failure reports `x` as given, so a
    /// `<single-float>` reports itself rather than its widening.
    ///
    /// Binary32 converts to `<i32>` and `<u32>` directly; to `<integer>` it
    /// first widens to binary64, which is exact.
    fn lower_exact_float_to_integer(
        &mut self,
        value: Value,
        to: sem::NumericKind,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Value {
        use sem::NumericKind as N;
        let single = value.kind == ValueKind::F32;
        let (operand, op) = match to {
            N::I32 if single => (value.reg, ConvertCheckedOp::F32ToI32Exact),
            N::U32 if single => (value.reg, ConvertCheckedOp::F32ToU32Exact),
            N::I32 => (value.reg, ConvertCheckedOp::F64ToI32Exact),
            N::U32 => (value.reg, ConvertCheckedOp::F64ToU32Exact),
            _ if single => {
                let wide =
                    self.num32_unary(Num32UnaryOp::F32ToF64, value.reg, ValueKind::F64, span);
                (wide.reg, ConvertCheckedOp::F64ToI64Exact)
            }
            _ => (value.reg, ConvertCheckedOp::F64ToI64Exact),
        };
        let (result, inexact) = self.convert_checked(op, operand, span);
        self.signal_if(inexact, result.reg, vec![value.reg], type_error, span);
        result
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
        integer: Reg,
        float: Reg,
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
    /// then [`Self::signal_if`], reporting `operands`.
    fn lower_checked_integer_arith(
        &mut self,
        op: I64ArithOp,
        lhs: Reg,
        rhs: Reg,
        operands: Vec<Reg>,
        type_error: Box<TypeErrorSite>,
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
        self.signal_if(overflow.reg, result.reg, operands, type_error, span);
        result
    }

    /// Lowers `<integer>` arithmetic typing proved cannot overflow `i64`.
    fn lower_unchecked_integer_arith(
        &mut self,
        op: I64ArithOp,
        lhs: Reg,
        rhs: Reg,
        span: Span,
    ) -> Value {
        let result = self.alloc(ValueKind::I64);
        self.instructions.push(Instr::I64Unchecked {
            dst: result.reg,
            op,
            lhs,
            rhs,
            span,
        });
        result
    }

    /// Branches on `failed`: when it holds, signals `type_error` for
    /// `values`, whose `use-value` restart supplies `result`; otherwise on
    /// to the code that follows.
    ///
    /// This is the single failure target for overflowing integer arithmetic
    /// and for inexact conversions. Bignum promotion will replace it for
    /// `<integer>` arithmetic, whose results then always fit.
    fn signal_if(
        &mut self,
        failed: Reg,
        result: Reg,
        values: Vec<Reg>,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) {
        let signal = self.alloc_label();
        let done = self.alloc_label();
        self.instructions.extend([
            Instr::Branch {
                cond: failed,
                then_label: signal,
                else_label: done,
                span,
            },
            Instr::Label { label: signal },
            Instr::SignalTypeError {
                dst: result,
                values,
                type_error,
                resume: done,
                span,
            },
            Instr::Label { label: done },
        ]);
    }

    /// The `<type-error>` integer arithmetic whose result has kind `kind`
    /// signals when it overflows.
    fn overflow_error(
        &self,
        kind: ValueKind,
        site: Option<sem::SiteId>,
        span: Span,
    ) -> Box<TypeErrorSite> {
        self.type_error(
            TypeErrorKind::Overflow,
            kind_expected(self.module, kind),
            site,
            span,
        )
    }

    fn alloc_label(&mut self) -> LabelId {
        let label = LabelId(self.next_label);
        self.next_label = self.next_label.saturating_add(1);
        label
    }

    /// Lowers a constant's initializer where the constant is read, in a
    /// frame of its own: its locals are the initializer's, not the reading
    /// body's, and none of the body's captures, block exits or closure
    /// bindings are in scope. Its check sites are the constant's.
    fn lower_constant_value(
        &mut self,
        constant: &'module sem::Constant,
    ) -> Result<Value, LowerError> {
        let source_locals = core::mem::replace(&mut self.source_locals, &constant.locals);
        let captures = core::mem::take(&mut self.captures);
        let env = self.env.take();
        let local_regs =
            core::mem::replace(&mut self.local_regs, vec![None; constant.locals.len()]);
        let locals = core::mem::take(&mut self.locals);
        let exits = core::mem::take(&mut self.exits);
        let closure = self.closure.take();
        let owner = self.owner.replace(constant.definition);
        let site_owner = self.site_owner.replace(constant.definition);
        let lowered = self.lower_expr(&constant.value);
        self.source_locals = source_locals;
        self.captures = captures;
        self.env = env;
        self.local_regs = local_regs;
        self.locals = locals;
        self.exits = exits;
        self.closure = closure;
        self.owner = owner;
        self.site_owner = site_owner;
        lowered
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
        sem::ValueKind::I32 => Ok(ValueKind::I32),
        sem::ValueKind::U32 => Ok(ValueKind::U32),
        sem::ValueKind::F32 => Ok(ValueKind::F32),
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

/// The contracts lowering trusts, in the order a code unit records them.
fn contracts(module: &sem::Module) -> Contracts {
    Contracts {
        generics: module
            .generics
            .iter()
            .map(|generic| module.generic_contract(generic))
            .collect(),
        methods: module
            .methods
            .iter()
            .map(|method| module.method_contract(method))
            .collect(),
        functions: module
            .functions
            .iter()
            .map(|function| (function.name.clone(), module.function_contract(function)))
            .collect(),
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
        sem::Intrinsic::VectorElementOr => RuntimeOp::VectorElementOr,
        sem::Intrinsic::Unsupplied => RuntimeOp::Unsupplied,
        sem::Intrinsic::KeywordAbsent => RuntimeOp::KeywordAbsent,
        sem::Intrinsic::FloatToString => RuntimeOp::FloatToString,
        sem::Intrinsic::IntegerToString => RuntimeOp::IntegerToString,
        sem::Intrinsic::Concatenate => RuntimeOp::Concatenate,
        sem::Intrinsic::InstanceOfType => RuntimeOp::InstanceOfType,
        sem::Intrinsic::LimitedIntegerBase => RuntimeOp::LimitedIntegerBase,
        sem::Intrinsic::LimitedIntegerMin => RuntimeOp::LimitedIntegerMin,
        sem::Intrinsic::LimitedIntegerMax => RuntimeOp::LimitedIntegerMax,
        sem::Intrinsic::SubclassTypeClass => RuntimeOp::SubclassTypeClass,
        sem::Intrinsic::TypeName => RuntimeOp::TypeName,
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
        | RuntimeOp::VectorSize
        | RuntimeOp::LimitedIntegerBase
        | RuntimeOp::LimitedIntegerMin
        | RuntimeOp::LimitedIntegerMax
        | RuntimeOp::SubclassTypeClass
        | RuntimeOp::TypeName => index == 0,
        RuntimeOp::VectorRef => index == 0,
        RuntimeOp::VectorElementOr => index != 1,
        RuntimeOp::Unsupplied | RuntimeOp::InternSymbol => false,
        RuntimeOp::IsKeywordAbsent => true,
        RuntimeOp::KeywordAbsent => false,
        RuntimeOp::InstanceOfType => true,
        RuntimeOp::LimitedIntegerType => false,
        RuntimeOp::ClassMetaObject
        | RuntimeOp::ClassMetaId
        | RuntimeOp::GenericMetaObject
        | RuntimeOp::MethodMetaObject
        | RuntimeOp::SlotDescriptorObject
        | RuntimeOp::FloatToString
        | RuntimeOp::SingleFloatToString
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
        Some(sem::ValueKind::I32) => ValueKind::I32,
        Some(sem::ValueKind::U32) => ValueKind::U32,
        Some(sem::ValueKind::F32) => ValueKind::F32,
        Some(sem::ValueKind::Bool) => ValueKind::Bool,
        Some(sem::ValueKind::Callable) => ValueKind::Closure(None),
        None if object_handle => ValueKind::Object(None),
        None => ValueKind::Any,
        Some(_) => ValueKind::Object(None),
    }
}

/// Returns the `0xFD` comparison of `op` on operands of a 32-bit `kind`.
const fn num32_compare(kind: sem::NumericKind, op: sem::CompareOp) -> Num32BinaryOp {
    use sem::CompareOp as C;
    match (kind, op) {
        (sem::NumericKind::I32, C::Eq) => Num32BinaryOp::I32Eq,
        (sem::NumericKind::I32, C::Lt) => Num32BinaryOp::I32Lt,
        (sem::NumericKind::I32, C::Le) => Num32BinaryOp::I32Le,
        (sem::NumericKind::I32, C::Gt) => Num32BinaryOp::I32Gt,
        (sem::NumericKind::I32, C::Ge) => Num32BinaryOp::I32Ge,
        (sem::NumericKind::U32, C::Eq) => Num32BinaryOp::U32Eq,
        (sem::NumericKind::U32, C::Lt) => Num32BinaryOp::U32Lt,
        (sem::NumericKind::U32, C::Le) => Num32BinaryOp::U32Le,
        (sem::NumericKind::U32, C::Gt) => Num32BinaryOp::U32Gt,
        (sem::NumericKind::U32, C::Ge) => Num32BinaryOp::U32Ge,
        (_, C::Eq) => Num32BinaryOp::F32Eq,
        (_, C::Lt) => Num32BinaryOp::F32Lt,
        (_, C::Le) => Num32BinaryOp::F32Le,
        (_, C::Gt) => Num32BinaryOp::F32Gt,
        (_, C::Ge) => Num32BinaryOp::F32Ge,
    }
}

/// Returns whether `kind` is an unboxed number, boolean or callable, whose
/// class is known statically: a callable is a `<function>`.
const fn is_unboxed(kind: ValueKind) -> bool {
    matches!(kind, ValueKind::Bool | ValueKind::Closure(_)) || is_unboxed_number(kind)
}

/// Returns whether `kind` is an unboxed number, whose class is known
/// statically.
const fn is_unboxed_number(kind: ValueKind) -> bool {
    matches!(
        kind,
        ValueKind::I64 | ValueKind::F64 | ValueKind::I32 | ValueKind::U32 | ValueKind::F32
    )
}

/// Returns the unboxed kind whose values `class` describes: `i64` for
/// `<integer>`, `f64` for `<double-float>`, `f32` for `<single-float>`,
/// `bool` for `<boolean>` and an untyped callable for `<function>`.
fn scalar_kind_for_class(module: &sem::Module, class: sem::ClassId) -> Option<ValueKind> {
    match module.classes.get(class.0 as usize)?.origin {
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Integer,
        } => Some(ValueKind::I64),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::DoubleFloat,
        } => Some(ValueKind::F64),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::SingleFloat,
        } => Some(ValueKind::F32),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Boolean,
        } => Some(ValueKind::Bool),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Function,
        } => Some(ValueKind::Closure(None)),
        _ => None,
    }
}

/// Returns the concrete built-in class of the unboxed kind `kind`.
///
/// `<i32>` and `<u32>` values are `<integer>`s, and callables `<function>`s.
fn scalar_concrete_class(module: &sem::Module, kind: ValueKind) -> Option<sem::ClassId> {
    match kind {
        ValueKind::I64 | ValueKind::I32 | ValueKind::U32 => {
            builtin_class(module, sem::BuiltinValueClass::Integer)
        }
        ValueKind::F64 => builtin_class(module, sem::BuiltinValueClass::DoubleFloat),
        ValueKind::F32 => builtin_class(module, sem::BuiltinValueClass::SingleFloat),
        ValueKind::Bool => builtin_class(module, sem::BuiltinValueClass::Boolean),
        ValueKind::Closure(_) => builtin_class(module, sem::BuiltinValueClass::Function),
        _ => None,
    }
}

/// Returns whether every value of the unboxed scalar category `kind` is an
/// instance of `class`: the scalar's concrete class (`<integer>` for `i64`,
/// `<double-float>` for `f64`) or one of its superclasses, such as
/// `<rational>` or `<float>`, `<real>`, `<number>`, and `<object>`;
/// `<boolean>` for `bool` and `<function>` for a callable.
fn is_scalar_value_class(module: &sem::Module, kind: ValueKind, class: sem::ClassId) -> bool {
    let concrete = match kind {
        ValueKind::I64 | ValueKind::I32 | ValueKind::U32 => sem::BuiltinValueClass::Integer,
        ValueKind::F64 => sem::BuiltinValueClass::DoubleFloat,
        ValueKind::F32 => sem::BuiltinValueClass::SingleFloat,
        ValueKind::Bool => sem::BuiltinValueClass::Boolean,
        ValueKind::Closure(_) => sem::BuiltinValueClass::Function,
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
        | ValueKind::I32
        | ValueKind::U32
        | ValueKind::F32
        | ValueKind::U64
        | ValueKind::Bool
        | ValueKind::Function(_)
        | ValueKind::Closure(_)
        | ValueKind::Entry
        | ValueKind::Aggregate
        | ValueKind::Exn
        | ValueKind::Str
        | ValueKind::Array(_)
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

/// A generic's methods in the order its dispatcher tests them, most
/// specific first.
fn sorted_methods<'module>(
    module: &'module sem::Module,
    generic: &sem::GenericFunction,
) -> Vec<&'module sem::Method> {
    let mut methods: Vec<_> = module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
        .collect();
    methods.sort_by(|left, right| {
        if module.method_more_specific(left, right) {
            core::cmp::Ordering::Less
        } else if module.method_more_specific(right, left) {
            core::cmp::Ordering::Greater
        } else {
            left.id.0.cmp(&right.id.0)
        }
    });
    methods
}

/// Returns whether callables of signatures `lhs` and `rhs` take and return
/// their values in the same register types, which is all the tape checks of
/// a callable's signature.
fn callable_registers_agree(lhs: &CallableSignature, rhs: &CallableSignature) -> bool {
    lhs.params.len() == rhs.params.len()
        && lhs.rets.len() == rhs.rets.len()
        && lhs
            .params
            .iter()
            .zip(&rhs.params)
            .chain(lhs.rets.iter().zip(&rhs.rets))
            .all(|(lhs, rhs)| shares_register_type(*lhs, *rhs))
}

/// Returns whether values of lowered kinds `lhs` and `rhs` are held in the
/// same tape register type: every object and metaobject kind shares one, as
/// do functions and closures of any signature.
fn shares_register_type(lhs: ValueKind, rhs: ValueKind) -> bool {
    let object = |kind| {
        matches!(
            kind,
            ValueKind::Object(_)
                | ValueKind::ClassObject(_)
                | ValueKind::GenericFunctionObject(_)
                | ValueKind::MethodObject(_)
                | ValueKind::SlotDescriptorObject
        )
    };
    match (lhs, rhs) {
        (ValueKind::Function(_), ValueKind::Function(_))
        | (ValueKind::Closure(_), ValueKind::Closure(_)) => true,
        _ => lhs == rhs || (object(lhs) && object(rhs)),
    }
}

/// Builds the `<type-error>` site a failing check signals.
fn type_error_site(
    kind: TypeErrorKind,
    expected: TypeObject,
    site: String,
    span: Span,
) -> Box<TypeErrorSite> {
    Box::new(TypeErrorSite {
        kind,
        expected,
        site,
        span: span.start..span.end,
    })
}

/// Names where a check is for the `<type-error>` it signals: the EOP id of
/// its typing decision, `typing:<owner>:s<n>`, when typing classifies it,
/// and otherwise `check:<owner>@<start>..<end>`, by its owner and span.
fn check_site_name(owner: Option<DefinitionId>, site: Option<sem::SiteId>, span: Span) -> String {
    match (owner, site) {
        (Some(owner), Some(site)) => format!("typing:{owner}:{site}"),
        (Some(owner), None) => format!("check:{owner}@{}..{}", span.start, span.end),
        (None, _) => format!("check@{}..{}", span.start, span.end),
    }
}

/// The expected type of a check that a value is an instance of `class`.
const fn class_expected(class: sem::ClassId) -> TypeObject {
    TypeObject::Class(class)
}

/// The expected type of a check that a value is a class object naming
/// `bound` or one of its subclasses: Dylan's `subclass(<bound>)`.
const fn class_object_expected(bound: sem::ClassId) -> TypeObject {
    TypeObject::Subclass(bound)
}

/// The expected type of a check whose result has register kind `kind`:
/// `<integer>`, `<i32>`, `<u32>`, a float class, `<boolean>` or
/// `<function>`.
fn kind_expected(module: &sem::Module, kind: ValueKind) -> TypeObject {
    let class = match kind {
        ValueKind::Closure(_) => sem::BuiltinValueClass::Function,
        ValueKind::I32 => return TypeObject::named(sem::LimitedInteger::I32),
        ValueKind::U32 => return TypeObject::named(sem::LimitedInteger::U32),
        ValueKind::F64 => sem::BuiltinValueClass::DoubleFloat,
        ValueKind::F32 => sem::BuiltinValueClass::SingleFloat,
        ValueKind::Bool => sem::BuiltinValueClass::Boolean,
        _ => sem::BuiltinValueClass::Integer,
    };
    module
        .builtin_value_class(class)
        .map_or(TypeObject::Class(sem::OBJECT_CLASS_ID), TypeObject::Class)
}

/// The expected type of a shift count: `limited(<integer>, min: 0, max: 31)`.
const fn shift_count_expected() -> TypeObject {
    range_expected(IntRange {
        min: Some(0),
        max: Some(31),
    })
}

/// The expected type of a check that an `<integer>` is in `range`, such as
/// `limited(<integer>, min: 0, max: 255)`: spelled out even when it equals
/// `<i32>`'s, since the checked value is an `<integer>`.
const fn range_expected(range: IntRange) -> TypeObject {
    TypeObject::limited(range)
}

/// The registers, labels and instructions of a function being lowered, for
/// code emitted both into bodies and into functions built without one,
/// such as thunks.
struct CodeBuffer<'a> {
    /// Kinds of r1 onward; a new register is the next one.
    register_kinds: &'a mut Vec<ValueKind>,
    /// The next unused label.
    next_label: &'a mut u32,
    /// The instructions so far.
    instructions: &'a mut Vec<Instr>,
}

impl CodeBuffer<'_> {
    fn alloc(&mut self, kind: ValueKind) -> Reg {
        let reg = register_at(self.register_kinds.len());
        self.register_kinds.push(kind);
        reg
    }

    fn label(&mut self) -> LabelId {
        let label = LabelId(*self.next_label);
        *self.next_label = self.next_label.saturating_add(1);
        label
    }

    /// Branches to `inside` when the `<integer>` in `value` is in `range`
    /// and to `outside` otherwise: one comparison and branch per bound.
    fn branch_on_range(
        &mut self,
        value: Reg,
        range: IntRange,
        inside: LabelId,
        outside: LabelId,
        span: Span,
    ) {
        let bounds = [
            range.min.map(|min| (min, sem::CompareOp::Ge)),
            range.max.map(|max| (max, sem::CompareOp::Le)),
        ];
        for (bound, op) in bounds.into_iter().flatten() {
            let constant = self.alloc(ValueKind::I64);
            let holds = self.alloc(ValueKind::Bool);
            let next = self.label();
            self.instructions.extend([
                Instr::ConstI64 {
                    dst: constant,
                    value: bound,
                    span,
                },
                Instr::I64Compare {
                    dst: holds,
                    op,
                    lhs: value,
                    rhs: constant,
                    span,
                },
                Instr::Branch {
                    cond: holds,
                    then_label: next,
                    else_label: outside,
                    span,
                },
                Instr::Label { label: next },
            ]);
        }
        self.instructions.push(Instr::Jump {
            label: inside,
            span,
        });
    }

    /// Checks that the `<integer>` in `value` is in a limited type's
    /// `range`, signalling `type_error` when it is not, and returns the
    /// register of the checked value.
    ///
    /// The check works on a copy of the value, so an operand that is a
    /// local's register keeps its value. A `use-value` replacement is
    /// written to the copy and the signal resumes at the test: unlike
    /// `<i32>`, whose register type establishes the range, an `<integer>`
    /// replacement may be outside it, so a bad one signals again.
    fn range_check(
        &mut self,
        value: Reg,
        range: IntRange,
        type_error: Box<TypeErrorSite>,
        span: Span,
    ) -> Reg {
        let checked = self.alloc(ValueKind::I64);
        let [test, outside, inside] = core::array::from_fn(|_| self.label());
        self.instructions.extend([
            Instr::Move {
                dst: checked,
                src: value,
                span,
            },
            Instr::Jump { label: test, span },
            Instr::Label { label: test },
        ]);
        self.branch_on_range(checked, range, inside, outside, span);
        self.instructions.extend([
            Instr::Label { label: outside },
            Instr::SignalTypeError {
                dst: checked,
                values: vec![checked],
                type_error,
                resume: test,
                span,
            },
            Instr::Label { label: inside },
        ]);
        checked
    }
}

/// The register kind every value crosses a callable value's boxed entry at:
/// the boxed callable ABI. Every value, a callable too, is a boxed
/// `<object>`, whatever the caller and the callee declare, so the tape's
/// signature check at `call.indirect` always passes and the callee's entry
/// ([`sem::EntryCheck`]) and the caller's result checks decide what each
/// side may rely on. Only untyped calls whose shape differs from the
/// value's typed entry take it ([`BodyLowering::call_untyped`]).
const BOXED_WIRE: ValueKind = ValueKind::Any;

/// The shape of a call taking `params` and returning `rets`, which a
/// callable value records for its typed entry ([`Instr::CallableNew`]): a
/// hash of what a caller must agree on to call that entry directly. It
/// covers each value's tape register type and, for a typed callable
/// parameter, its full signature, since the entry relies on the value it
/// is passed having that signature's typed entry. A callable result is any
/// callable value, which an untyped caller takes as one.
///
/// A signature's keywords, names and declared kinds, are part of its
/// shape: its typed entry takes them boxed after the required parameters.
///
/// The hash is of the signature's text, so units compiled apart agree on
/// it. Two different signatures with the same hash would let the caller
/// call the typed entry with registers the tape refuses, a trap, never a
/// misread value.
fn callable_shape(
    signatures: &[CallableSignature],
    params: &[ValueKind],
    keywords: &[CallableKeyword],
    rets: &[ValueKind],
) -> u64 {
    let mut text = String::new();
    for kind in params {
        shape_text(signatures, *kind, true, &mut text);
    }
    if !keywords.is_empty() {
        text.push_str("#key");
        for keyword in keywords {
            text.push_str(&keyword.name);
            text.push(':');
            shape_text(signatures, keyword.kind, true, &mut text);
            text.push(';');
        }
    }
    text.push_str("->");
    for kind in rets {
        shape_text(signatures, *kind, false, &mut text);
    }
    // FNV-1a.
    text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Writes `kind`'s part of a [`callable_shape`]: its register type, and a
/// typed callable's signature when `full`.
fn shape_text(signatures: &[CallableSignature], kind: ValueKind, full: bool, text: &mut String) {
    let code = match kind {
        ValueKind::Any => "a",
        ValueKind::I64 => "i",
        ValueKind::F64 => "f",
        ValueKind::I32 => "w",
        ValueKind::U32 => "u",
        ValueKind::F32 => "s",
        ValueKind::U64 => "q",
        ValueKind::Bool => "b",
        ValueKind::Object(_)
        | ValueKind::ClassObject(_)
        | ValueKind::GenericFunctionObject(_)
        | ValueKind::MethodObject(_)
        | ValueKind::SlotDescriptorObject => "o",
        ValueKind::Closure(Some(sig)) if full => {
            text.push_str("c(");
            if let Some(signature) = signatures.get(sig.0 as usize) {
                for kind in &signature.params {
                    shape_text(signatures, *kind, true, text);
                }
                text.push_str("->");
                for kind in &signature.rets {
                    shape_text(signatures, *kind, true, text);
                }
            }
            ")"
        }
        ValueKind::Closure(_) => "c",
        ValueKind::Entry => "e",
        ValueKind::Function(_) => "F",
        ValueKind::Aggregate => "g",
        ValueKind::Exn => "x",
        ValueKind::Str => "t",
        ValueKind::Array(ArrayKind::Str) => "[t]",
        ValueKind::Array(ArrayKind::Any) => "[a]",
    };
    text.push_str(code);
    text.push(',');
}

/// Writes signature `sig` as source writes it, such as
/// `callable(<integer>) => <circle>`, which names its adapter
/// ([`Lowering::lower_adapter`]) and the sites of its checks.
fn signature_label(
    module: &sem::Module,
    signatures: &[CallableSignature],
    sig: CallableSigId,
) -> String {
    let kind = |kind: &ValueKind| match *kind {
        ValueKind::I64 => String::from("<integer>"),
        ValueKind::Closure(Some(inner)) => signature_label(module, signatures, inner),
        ValueKind::Closure(None) => String::from("<callable>"),
        ValueKind::Bool => format!("{}", kind_expected(module, *kind).display(module)),
        ValueKind::Object(Some(class)) => format!("{}", class_expected(class).display(module)),
        ValueKind::ClassObject(Some(class)) => {
            format!("{}", class_object_expected(class).display(module))
        }
        kind if is_unboxed_number(kind) => {
            format!("{}", kind_expected(module, kind).display(module))
        }
        _ => String::from("<object>"),
    };
    let Some(signature) = signatures.get(sig.0 as usize) else {
        return String::from("<callable>");
    };
    let params = signature.params.iter().map(kind).collect::<Vec<_>>();
    let rets = signature.rets.iter().map(kind).collect::<Vec<_>>();
    match rets.as_slice() {
        [ret] => format!("callable({}) => {ret}", params.join(", ")),
        _ => format!("callable({}) => ({})", params.join(", "), rets.join(", ")),
    }
}

/// The signature of `kind`, when it is a typed callable.
fn typed_callable(kind: &ValueKind) -> Option<CallableSigId> {
    match kind {
        ValueKind::Closure(Some(sig)) => Some(*sig),
        _ => None,
    }
}

/// Returns what a typed entry (a callable thunk, a closure body, an exit
/// procedure, a restart guard) must check a `kind` parameter is, if
/// anything; see [`sem::EntryTarget::for_guard_kind`]. A dispatcher checks
/// a `use-value` replacement for a failed send's result the same way.
///
/// A `call.indirect` caller holding an untyped `<callable>` supplies whatever
/// classes it has, and the tape's `object` register type erases them, so an
/// entry that may be called that way (a callable thunk, a closure, an exit
/// procedure) checks each object parameter narrower than `<object>`, and
/// each class object parameter, before its body relies on the class.
/// Unboxed numbers, booleans and callables are distinct register types the
/// tape checks itself.
fn entry_target(kind: ValueKind) -> Option<sem::EntryTarget> {
    match kind {
        ValueKind::Object(Some(class)) if class != sem::OBJECT_CLASS_ID => {
            Some(sem::EntryTarget::Instance(class))
        }
        ValueKind::ClassObject(Some(class)) => Some(sem::EntryTarget::ClassObject(class)),
        _ => None,
    }
}

/// Emits the check that `value`, of `kind`, is what `target` requires into
/// `code` and returns the register of the checked value. A class check
/// failing signals a [`TypeErrorKind::Entry`] error at `site`, and a range
/// check a [`TypeErrorKind::Range`] one, whose replacement is checked again.
/// A value already in a number or boolean register needs no
/// [`sem::EntryTarget::Unbox`] check; a boxed one is checked by
/// [`BodyLowering::unwire`].
fn entry_check(
    code: &mut CodeBuffer<'_>,
    target: sem::EntryTarget,
    value: Reg,
    kind: ValueKind,
    site: String,
    span: Span,
) -> Reg {
    match target {
        sem::EntryTarget::Unbox(_) => value,
        sem::EntryTarget::Range(range) => {
            let type_error =
                type_error_site(TypeErrorKind::Range, range_expected(range), site, span);
            code.range_check(value, range, type_error, span)
        }
        sem::EntryTarget::Instance(class) => {
            let dst = code.alloc(kind);
            code.instructions.push(Instr::CastObject {
                dst,
                value,
                input: kind,
                class,
                type_error: type_error_site(
                    TypeErrorKind::Entry,
                    class_expected(class),
                    site,
                    span,
                ),
                span,
            });
            dst
        }
        sem::EntryTarget::ClassObject(bound) => {
            let dst = code.alloc(kind);
            code.instructions.push(Instr::CastClassObject {
                dst,
                value,
                input: kind,
                bound,
                type_error: type_error_site(
                    TypeErrorKind::Entry,
                    class_object_expected(bound),
                    site,
                    span,
                ),
                span,
            });
            dst
        }
    }
}

/// Whether an argument of `actual` kind goes to a typed entry's parameter
/// of `param` kind as it is: the same kind, any callable for an untyped
/// one, or a runtime object for one, whose class the entry checks.
fn typed_arg_fits(param: ValueKind, actual: ValueKind) -> bool {
    param == actual
        || matches!(
            (param, actual),
            (ValueKind::Closure(None), ValueKind::Closure(_))
                | (ValueKind::Object(_), ValueKind::Object(_))
                | (ValueKind::ClassObject(_), ValueKind::ClassObject(_))
        )
}

/// Whether the tape code for `instr` runs straight through, so what the
/// tape proved about registers before it still holds after it: no label,
/// branch, or check that branches to signal a failure.
fn keeps_projections(instr: &Instr) -> bool {
    matches!(
        instr,
        Instr::ConstI64 { .. }
            | Instr::ConstF64 { .. }
            | Instr::Const32 { .. }
            | Instr::ConstBool { .. }
            | Instr::ConstU64 { .. }
            | Instr::ConstStr { .. }
            | Instr::ConstClass { .. }
            | Instr::ConstFunction { .. }
            | Instr::StringLiteral { .. }
            | Instr::SymbolLiteral { .. }
            | Instr::VectorLiteral { .. }
            | Instr::ClosureNew { .. }
            | Instr::CallableNew { .. }
            | Instr::OpenCallable { .. }
            | Instr::CaptureGet { .. }
            | Instr::BoxValue { .. }
            | Instr::Move { .. }
            | Instr::ArrayNew { .. }
            | Instr::ArrayLen { .. }
            | Instr::ArrayGet { .. }
            | Instr::StrEq { .. }
            | Instr::AddU64 { .. }
            | Instr::EqU64 { .. }
            | Instr::Call { .. }
            | Instr::CallIndirect { .. }
            | Instr::RuntimeCall { .. }
            | Instr::HostCall { .. }
            | Instr::MakeObject { .. }
    )
}

/// Names an entry check typing does not classify: source parameter `index`
/// (counting from zero, without a closure's environment) of the lowered
/// function `function`, as `entry:<function>:<index>`.
fn entry_site_name(function: &str, index: usize) -> String {
    format!("entry:{function}:{index}")
}

/// Copies a numeric method's body (see [`sem::MethodOrigin::Numeric`]) with
/// every span set to `span`, or returns `None` when it holds anything the
/// promotion rules do not build.
fn respan_numeric_body(expr: &sem::Expr, span: Span) -> Option<sem::Expr> {
    let operand = |expr: &sem::Expr| respan_numeric_body(expr, span).map(Box::new);
    Some(match expr {
        sem::Expr::Local {
            local,
            result_kind,
            callable_signature,
            ..
        } => sem::Expr::Local {
            local: *local,
            result_kind: *result_kind,
            callable_signature: callable_signature.clone(),
            span,
        },
        sem::Expr::Convert {
            to,
            value,
            site,
            explicit,
            ..
        } => sem::Expr::Convert {
            to: *to,
            value: operand(value)?,
            site: *site,
            explicit: *explicit,
            span,
        },
        sem::Expr::Binary {
            op,
            kind,
            lhs,
            rhs,
            site,
            ..
        } => sem::Expr::Binary {
            op: *op,
            kind: *kind,
            lhs: operand(lhs)?,
            rhs: operand(rhs)?,
            site: *site,
            span,
        },
        sem::Expr::Negate {
            kind, value, site, ..
        } => sem::Expr::Negate {
            kind: *kind,
            value: operand(value)?,
            site: *site,
            span,
        },
        sem::Expr::Compare {
            op, kind, lhs, rhs, ..
        } => sem::Expr::Compare {
            op: *op,
            kind: *kind,
            lhs: operand(lhs)?,
            rhs: operand(rhs)?,
            span,
        },
        sem::Expr::MixedCompare {
            op,
            lhs_kind,
            lhs,
            rhs,
            ..
        } => sem::Expr::MixedCompare {
            op: *op,
            lhs_kind: *lhs_kind,
            lhs: operand(lhs)?,
            rhs: operand(rhs)?,
            span,
        },
        _ => return None,
    })
}

/// Sorts check records by owner and site, one per site.
///
/// A constant's value is lowered wherever it is read, so its sites can be
/// recorded more than once. Lowering a constant does not depend on where it
/// is read, so the copies agree; were they ever to differ, the site reports
/// the least change.
fn merge_check_records(mut checks: Vec<CheckCodegenRecord>) -> Vec<CheckCodegenRecord> {
    checks.sort_by_key(|record| (record.owner, record.site));
    let mut merged: Vec<CheckCodegenRecord> = Vec::with_capacity(checks.len());
    for record in checks {
        match merged.last_mut() {
            Some(last) if (last.owner, last.site) == (record.owner, record.site) => {
                if last.codegen != record.codegen {
                    last.codegen = CheckCodegen::Kept;
                }
            }
            _ => merged.push(record),
        }
    }
    merged.retain(|record| record.codegen != CheckCodegen::Kept);
    merged
}

/// The register whose kind is entry `index` of a function's register kinds.
///
/// Register zero is the tape effect register, so kinds start at r1 and a
/// function's inputs take the first registers.
fn register_at(index: usize) -> Reg {
    Reg(u32::try_from(index + 1).unwrap_or(u32::MAX))
}

fn invalid_semantic(message: &str) -> LowerError {
    LowerError::InvalidSemantic {
        message: message.into(),
        span: None,
    }
}

/// Stable dump name for a control tag.
#[must_use]
pub fn control_tag_name(tag: ControlTag) -> String {
    match tag {
        ControlTag::Restart => "restart".into(),
        ControlTag::Exit(signature) => format!("exit#{}", signature.0),
        ControlTag::Retry => "retry".into(),
    }
}

fn runtime_op_name(op: RuntimeOp) -> &'static str {
    match op {
        RuntimeOp::ClassMetaObject => "class-object",
        RuntimeOp::ClassMetaId => "class-id",
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
        RuntimeOp::VectorElementOr => "vector-element-or",
        RuntimeOp::Unsupplied => "unsupplied",
        RuntimeOp::IsKeywordAbsent => "keyword-absent?",
        RuntimeOp::KeywordAbsent => "keyword-absent",
        RuntimeOp::InternSymbol => "intern-symbol",
        RuntimeOp::FloatToString | RuntimeOp::SingleFloatToString => "float-to-string",
        RuntimeOp::IntegerToString => "integer-to-string",
        RuntimeOp::Concatenate => "concatenate",
        RuntimeOp::InstanceOfType => "instance?",
        RuntimeOp::LimitedIntegerType => "limited",
        RuntimeOp::LimitedIntegerBase => "limited-integer-base",
        RuntimeOp::LimitedIntegerMin => "limited-integer-min",
        RuntimeOp::LimitedIntegerMax => "limited-integer-max",
        RuntimeOp::SubclassTypeClass => "subclass-type-class",
        RuntimeOp::TypeName => "type-name",
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
            write!(f, "%{} {}: ", param.reg, param.name)?;
            self.fmt_kind(f, param.kind)?;
        }
        write!(f, ") -> ")?;
        self.fmt_kind_list(f, &function.rets)?;
        self.fmt_kind_comment(f, function.kind)?;
        writeln!(f)?;

        for local in &function.locals {
            write!(
                f,
                "  local l{} {} = %{}: ",
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
            FunctionKind::BoxedEntry { typed } => writeln!(f, " ; boxed entry of l{}", typed.0),
            FunctionKind::CallableParts => writeln!(f, " ; opens callable values"),
            FunctionKind::Adapter { signature } => {
                writeln!(f, " ; adapter to signature {}", signature.0)
            }
        }
    }

    fn fmt_instr(&self, f: &mut fmt::Formatter<'_>, instr: &Instr) -> fmt::Result {
        match instr {
            Instr::Label { label } => writeln!(f, "{label}:"),
            Instr::ConstI64 { dst, value, span } => {
                write!(f, "  %{dst} = const.i64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstF64 { dst, value, span } => {
                write!(f, "  %{dst} = const.f64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::Const32 { dst, value, span } => {
                let mnemonic = match value {
                    sem::Number32::I32(_) => "const.i32",
                    sem::Number32::U32(_) => "const.u32",
                    sem::Number32::F32(_) => "const.f32",
                };
                write!(f, "  %{dst} = {mnemonic} {value}")?;
                fmt_span(f, *span)
            }
            Instr::Num32Unary { dst, op, src, span } => {
                write!(f, "  %{dst} = {} %{src}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::Num32Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = {} %{lhs}, %{rhs}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::Num32Checked {
                dst,
                overflow,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(
                    f,
                    "  %{dst}, %{overflow} = {} %{lhs}, %{rhs}",
                    op.mnemonic()
                )?;
                fmt_span(f, *span)
            }
            Instr::ConvertChecked {
                dst,
                inexact,
                op,
                src,
                span,
            } => {
                write!(f, "  %{dst}, %{inexact} = {} %{src}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::ConstBool { dst, value, span } => {
                write!(f, "  %{dst} = const.bool {value}")?;
                fmt_span(f, *span)
            }
            Instr::StringLiteral {
                dst,
                value,
                class,
                span,
            } => {
                write!(f, "  %{dst} = literal.string ")?;
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
                write!(f, "  %{dst} = literal.symbol #{name}: ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::ConstU64 { dst, value, span } => {
                write!(f, "  %{dst} = const.u64 {value}")?;
                fmt_span(f, *span)
            }
            Instr::ConstClass { dst, class, span } => {
                write!(f, "  %{dst} = const.class ")?;
                self.fmt_class(f, *class)?;
                fmt_span(f, *span)
            }
            Instr::ConstFunction {
                dst,
                function,
                span,
            } => {
                write!(f, "  %{dst} = const.function l{}", function.0)?;
                fmt_span(f, *span)
            }
            Instr::ClosureNew {
                dst,
                function,
                captures,
                span,
            } => {
                write!(f, "  %{dst} = closure.new l{}", function.0)?;
                self.fmt_arg_list(f, captures)?;
                fmt_span(f, *span)
            }
            Instr::CallableNew {
                dst,
                typed,
                boxed,
                shape,
                captures,
                span,
            } => {
                write!(f, "  %{dst} = callable.new l{}", typed.0)?;
                match boxed {
                    BoxedEntry::Function(function) => write!(f, " boxed l{}", function.0)?,
                    BoxedEntry::Shared(entry) => write!(f, " boxed %{entry}")?,
                }
                write!(f, " shape {shape:#018x}")?;
                self.fmt_arg_list(f, captures)?;
                fmt_span(f, *span)
            }
            Instr::OpenCallable {
                typed,
                boxed,
                shape,
                callable,
                span,
            } => {
                let part = |reg: &Option<Reg>| {
                    reg.map_or_else(|| String::from("_"), |reg| format!("%{reg}"))
                };
                write!(
                    f,
                    "  {}, {}, {} = callable.open %{callable}",
                    part(typed),
                    part(boxed),
                    part(shape)
                )?;
                fmt_span(f, *span)
            }
            Instr::CaptureGet {
                dst,
                env,
                index,
                kind,
                span,
            } => {
                write!(f, "  %{dst} = capture.get %{env}[{index}]: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::CellNew {
                dst,
                value,
                kind,
                span,
            } => {
                write!(f, "  %{dst} = cell.new %{value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::CellGet {
                dst,
                cell,
                kind,
                span,
            } => {
                write!(f, "  %{dst} = cell.get %{cell}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::CellSet {
                cell,
                value,
                kind,
                span,
            } => {
                write!(f, "  cell.set %{cell}, %{value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::BoxValue {
                dst,
                src,
                from,
                span,
            } => {
                write!(f, "  %{dst} = box.any %{src}: ")?;
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
                write!(
                    f,
                    "  %{dst}, %{overflow} = {} %{lhs}, %{rhs}",
                    op.mnemonic()
                )?;
                fmt_span(f, *span)
            }
            Instr::I64Unchecked {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = {} %{lhs}, %{rhs}", op.unchecked_mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::F64Binary {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = {} %{lhs}, %{rhs}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::I64Bitwise {
                dst,
                op,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = {} %{lhs}, %{rhs}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::F64Unary { dst, op, src, span } => {
                write!(f, "  %{dst} = {} %{src}", op.mnemonic())?;
                fmt_span(f, *span)
            }
            Instr::I64ToF64 { dst, src, span } => {
                write!(f, "  %{dst} = i64.to_f64 %{src}")?;
                fmt_span(f, *span)
            }
            Instr::FloatCall {
                dst,
                function,
                args,
                span,
            } => {
                write!(f, "  %{dst} = float.call {}", function.symbol())?;
                self.fmt_arg_list(f, args)?;
                fmt_span(f, *span)
            }
            Instr::F64NotEqual {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = f64.ne %{lhs}, %{rhs}")?;
                fmt_span(f, *span)
            }
            Instr::F64ToI64 { dst, src, span } => {
                write!(f, "  %{dst} = f64.to_i64 %{src}")?;
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
                write!(f, "  %{dst} = {op} %{lhs}, %{rhs}")?;
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
                write!(f, "  %{dst} = {op} %{lhs}, %{rhs}")?;
                fmt_span(f, *span)
            }
            Instr::ValueEqual {
                dst,
                lhs,
                rhs,
                kind,
                span,
            } => {
                write!(f, "  %{dst} = value.eq %{lhs}, %{rhs}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::Identical {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = identical %{lhs}, %{rhs}")?;
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
            Instr::SelectMethod {
                dst,
                generic,
                args,
                span,
            } => {
                write!(f, "  %{dst} = select-method g{}", generic.0)?;
                self.fmt_arg_list(f, args)?;
                fmt_span(f, *span)
            }
            Instr::LinkedSend {
                dsts,
                method,
                callee,
                args,
                span,
            } => {
                write!(f, "  ")?;
                self.fmt_result_list(f, dsts)?;
                write!(f, " = linked-send m{} l{}", method.0, callee.0)?;
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
                write!(f, " = call.indirect %{callee}")?;
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
                write!(f, "  %{dst} = make ")?;
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
                write!(f, "  %{dst} = literal.vector ")?;
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
                write!(f, "  %{dst} = runtime.{}", runtime_op_name(*op))?;
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
                write!(f, "  %{dst} = host.call h{} {name}", function.0)?;
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
                write!(f, "  %{dst} = slot.read %{receiver}, ")?;
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
                write!(f, "  %{dst} = slot.write %{receiver}, ")?;
                self.fmt_slot(f, *class, *slot)?;
                write!(f, ", %{value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_span(f, *span)
            }
            Instr::ClassOf {
                dst, object, span, ..
            } => {
                write!(f, "  %{dst} = class_of %{object}")?;
                fmt_span(f, *span)
            }
            Instr::UnboxValue {
                dst,
                value,
                class,
                kind,
                type_error,
                span,
            } => {
                write!(f, "  %{dst} = unbox ")?;
                self.fmt_class(f, *class)?;
                write!(f, ", %{value}: ")?;
                self.fmt_kind(f, *kind)?;
                fmt_type_error(f, &self.runtime, type_error)?;
                fmt_span(f, *span)
            }
            Instr::InstanceOf {
                dst,
                value,
                input,
                class,
                span,
            } => {
                write!(f, "  %{dst} = instance? %{value}: ")?;
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
                type_error,
                span,
            } => {
                write!(f, "  %{dst} = as ")?;
                self.fmt_class(f, *class)?;
                write!(f, ", %{value}: ")?;
                self.fmt_kind(f, *input)?;
                fmt_type_error(f, &self.runtime, type_error)?;
                fmt_span(f, *span)
            }
            Instr::CastClassObject {
                dst,
                value,
                input,
                bound,
                type_error,
                span,
            } => {
                write!(f, "  %{dst} = as class ")?;
                self.fmt_class(f, *bound)?;
                write!(f, ", %{value}: ")?;
                self.fmt_kind(f, *input)?;
                fmt_type_error(f, &self.runtime, type_error)?;
                fmt_span(f, *span)
            }
            Instr::EqU64 {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = u64.eq %{lhs}, %{rhs}")?;
                fmt_span(f, *span)
            }
            Instr::AddU64 {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = u64.add %{lhs}, %{rhs}")?;
                fmt_span(f, *span)
            }
            Instr::ConstStr { dst, value, span } => {
                write!(f, "  %{dst} = const.str {value:?}")?;
                fmt_span(f, *span)
            }
            Instr::StrEq {
                dst,
                lhs,
                rhs,
                span,
            } => {
                write!(f, "  %{dst} = str.eq %{lhs}, %{rhs}")?;
                fmt_span(f, *span)
            }
            Instr::ArrayNew {
                dst,
                kind,
                values,
                span,
            } => {
                let element = match kind {
                    ArrayKind::Str => "str",
                    ArrayKind::Any => "any",
                };
                write!(f, "  %{dst} = array.new {element}[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "%{value}")?;
                }
                write!(f, "]")?;
                fmt_span(f, *span)
            }
            Instr::ArrayLen { dst, array, span } => {
                write!(f, "  %{dst} = array.len %{array}")?;
                fmt_span(f, *span)
            }
            Instr::ArrayGet {
                dst,
                array,
                index,
                span,
            } => {
                write!(f, "  %{dst} = array.get %{array}[%{index}]")?;
                fmt_span(f, *span)
            }
            Instr::Branch {
                cond,
                then_label,
                else_label,
                span,
            } => {
                write!(f, "  br %{cond}, {then_label}, {else_label}")?;
                fmt_span(f, *span)
            }
            Instr::Jump { label, span } => {
                write!(f, "  jmp {label}")?;
                fmt_span(f, *span)
            }
            Instr::Move { dst, src, span } => {
                write!(f, "  %{dst} = mov %{src}")?;
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
            Instr::SignalTypeError {
                dst,
                values,
                type_error,
                resume,
                span,
            } => {
                write!(
                    f,
                    "  {dst} = signal {} {} at {} (",
                    type_error.kind.code(),
                    type_error.expected.display(&self.runtime),
                    type_error.site
                )?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{value}")?;
                }
                write!(f, ") -> {resume}")?;
                fmt_span(f, *span)
            }
            Instr::PushHandler {
                dst,
                descriptor,
                guard,
                span,
            } => {
                write!(f, "  %{dst} = condition.push-handler restart#{descriptor}")?;
                if let Some(site) = self.restarts.get(*descriptor) {
                    write!(f, " ")?;
                    self.fmt_class(f, site.class)?;
                }
                if let Some(guard) = guard {
                    write!(f, " when %{guard}")?;
                }
                fmt_span(f, *span)
            }
            Instr::PopBinding { token, span } => {
                write!(f, "  control.pop %{token}")?;
                fmt_span(f, *span)
            }
            Instr::UnboxObject {
                dst,
                value,
                otherwise,
                span,
            } => {
                write!(f, "  %{dst} = unbox object %{value} else {otherwise}")?;
                fmt_span(f, *span)
            }
            Instr::NoApplicableMethod {
                dst,
                generic,
                arguments,
                span,
            } => {
                write!(
                    f,
                    "  %{dst} = condition.no-applicable-method g{} %{arguments}",
                    generic.0
                )?;
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
                write!(f, "condition.signal %{condition}")?;
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
                    write!(f, "%{param}")?;
                }
                writeln!(f, "):")
            }
            Instr::Rethrow { exception, span } => {
                write!(f, "  rethrow %{exception}")?;
                fmt_span(f, *span)
            }
            Instr::PushExit { dst, span } => {
                write!(f, "  %{dst} = control.push-exit")?;
                fmt_span(f, *span)
            }
            Instr::CheckExit { token, span } => {
                write!(f, "  control.check-exit %{token}")?;
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
            ValueKind::I32 => write!(f, "i32"),
            ValueKind::U32 => write!(f, "u32"),
            ValueKind::F32 => write!(f, "f32"),
            ValueKind::U64 => write!(f, "u64"),
            ValueKind::Bool => write!(f, "bool"),
            ValueKind::Aggregate => write!(f, "agg"),
            ValueKind::Exn => write!(f, "exn"),
            ValueKind::Str => write!(f, "str"),
            ValueKind::Array(ArrayKind::Str) => write!(f, "array(str)"),
            ValueKind::Array(ArrayKind::Any) => write!(f, "array(any)"),
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
            ValueKind::Entry => write!(f, "entry"),
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

    fn fmt_arg_list(&self, f: &mut fmt::Formatter<'_>, args: &[Reg]) -> fmt::Result {
        write!(f, "(")?;
        for (index, arg) in args.iter().enumerate() {
            if index != 0 {
                write!(f, ", ")?;
            }
            write!(f, "%{arg}")?;
        }
        write!(f, ")")
    }

    fn fmt_result_list(&self, f: &mut fmt::Formatter<'_>, regs: &[Reg]) -> fmt::Result {
        match regs {
            [] => write!(f, "()"),
            [reg] => write!(f, "%{reg}"),
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

/// Writes what a failing check signals, as ` else type-error.cast <circle>
/// at typing:def…:s0`.
fn fmt_type_error(
    f: &mut fmt::Formatter<'_>,
    runtime: &RuntimeLayout,
    type_error: &TypeErrorSite,
) -> fmt::Result {
    write!(
        f,
        " else {} {} at {}",
        type_error.kind.code(),
        type_error.expected.display(runtime),
        type_error.site
    )
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
        lower_unoptimized(&sem).expect("lowering should succeed")
    }

    /// Lowers `sem` as [`CodegenPolicy::Off`] does, without typing facts.
    fn lower_unoptimized(sem: &sem::Module) -> Result<Module, LowerError> {
        let typing = typing::analyze(sem, typing::TypingConfig::default());
        lower(sem, &typing, CodegenPolicy::Off)
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
        assert!(dump.contains("br %r4, bb1, bb2"));
        assert!(dump.contains("signal type-error.overflow <integer> at typing:def"));
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
        assert!(dump.contains("callable.new l"));
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
            keywords: Vec::new(),
        }));
        assert_eq!(
            module.call_sigs[0],
            CallableSignature {
                params: vec![ValueKind::I64],
                rets: vec![ValueKind::I64],
                keywords: Vec::new(),
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
        assert!(dump.contains("callable.new l3"));
        assert!(dump.contains("callable.new l4"));
    }

    #[test]
    fn lowers_a_blocks_own_exit_calls_as_throws() {
        // `return` called in its block's body throws to the block itself;
        // handed to `deliver` it is a callable value, entered through its
        // exit procedure.
        let module = lower_source(
            "define function deliver(f :: <callable>, n :: <integer>) => <integer>\n  apply(f, n)\nend;\n\
             define function pick(n :: <integer>) => <integer>\n  \
               block (return)\n    if (n > 3) return(n) + 1 else deliver(return, 7) end\n  end\nend;",
        );
        let function = |name: &str| {
            module
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("function was lowered")
        };
        let calls_through = |function: &Function, local: &str| {
            let reg = function
                .locals
                .iter()
                .find(|candidate| candidate.name == local)
                .expect("local")
                .reg;
            function.instructions.iter().any(
                |instr| matches!(instr, Instr::OpenCallable { callable, .. } if *callable == reg),
            )
        };
        let pick = function("pick");
        assert!(!calls_through(pick, "return"));
        assert_eq!(
            pick.instructions
                .iter()
                .filter(|instr| matches!(
                    instr,
                    Instr::Throw {
                        tag: ControlTag::Exit(_),
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(calls_through(function("deliver"), "f"));
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
            keywords: Vec::new(),
        }));
        assert_eq!(module.functions[1].params[0].kind, ValueKind::Closure(None));
        let dump = module.dump();
        assert!(dump.contains("fn l1 callit(%r1 f: callable) -> i64"));
        // A call whose shape matches the value's typed entry calls it as it
        // is; any other crosses boxed, and its result is unboxed where it
        // lands.
        assert!(dump.contains("%r4, _, %r5 = callable.open %r1"));
        assert!(dump.contains("%r3 = call.indirect %r4(%r2): i64"));
        // The boxed call passes an empty keyword tail.
        assert!(dump.contains("%r8 = box.any %r2: i64"));
        assert!(dump.contains("%r9 = array.new str[]"));
        assert!(dump.contains("%r12 = call.indirect %r11(%r8, %r9, %r10): any"));
        assert!(dump.contains("unbox <integer>, %r12: i64 else type-error.cast <integer>"));
        assert!(dump.contains("callable.new l3 boxed l4"));
        assert!(dump.contains("fn l3 inc#closure(%r1 $env: agg, %r2 arg0: i64) -> i64"));
        assert!(dump.contains(
            "fn l4 inc#closure#boxed(%r1 $env: agg, %r2 arg0: any, \
             %r3 $keywords: array(str), %r4 $keyword-values: array(any)) -> any"
        ));
        assert!(dump.contains("unbox <integer>, %r2: i64 else type-error.entry <integer>"));
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
        assert!(dump.contains("callable.new l"));
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
             define function main()\n  if (make(<flag>, valid?: #t).valid?) 1 else 0 end\nend;",
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
             define function radius-or-zero(x :: <object>) => <integer>\n  if (instance?(x, <circle>)) x.radius else 0 end\nend;\n\
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
             define function main()\n  if (10 < 20) 1 else 2 end\nend;",
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

    #[test]
    fn joins_lower_at_the_kind_semantic_analysis_gave_them() {
        // Each branch is a `<shape>` after its cast, but holds a different
        // subclass; the `if`, the `handle` and the `block` all produce the
        // `<shape>` semantic analysis joined them at.
        let module = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\nend;\n\
             define class <square> (<shape>)\nend;\n\
             define class <oops> (<object>)\nend;\n\
             define function by-if(c :: <boolean>)\n  \
               if (c) as(<shape>, make(<circle>)) else as(<shape>, make(<square>)) end\nend;\n\
             define function by-handle(c :: <boolean>)\n  \
               handle if (c) signal(make(<oops>)) else make(<circle>) end\n  \
               with restart e :: <oops> as(<shape>, make(<square>)) end\nend;\n\
             define function by-block(c :: <boolean>) => <shape>\n  \
               block (return) if (c) return(make(<square>)) else make(<circle>) end end\nend;",
        );

        for name in ["by-if", "by-handle", "by-block"] {
            let function = module
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("function was lowered");
            assert_eq!(
                function.ret,
                ValueKind::Object(Some(sem::ClassId(1))),
                "{name}"
            );
            // Every branch is statically a `<shape>`: no runtime cast.
            assert!(
                !function
                    .instructions
                    .iter()
                    .any(|instr| matches!(instr, Instr::CastObject { .. })),
                "{name}"
            );
        }
    }

    #[test]
    fn internal_lowering_failures_report_the_definition_as_a_diagnostic() {
        let tree = grammar_frontend::parse_file(
            SourceFileId::ANONYMOUS,
            "define function main()\n  if (#t) 1 else 2 end\nend;",
        )
        .syntax;
        let mut sem = sem::lower(&ast::lower(&tree));
        // Semantic analysis guarantees a boolean condition; break that.
        let main = &mut sem.functions[0];
        let main_span = main.span;
        let sem::Expr::If { condition, .. } = &mut main.body.result else {
            panic!("main is an if");
        };
        let span = condition.span();
        **condition = sem::Expr::ConstI64 { value: 1, span };

        let error = lower_unoptimized(&sem).expect_err("an integer condition cannot lower");
        assert_eq!(
            error,
            LowerError::InvalidSemantic {
                message: "if condition is not bool".into(),
                span: Some(main_span),
            }
        );
        let diagnostic = error.diagnostic().expect("an internal failure is reported");
        assert!(diagnostic.is_error());
        assert_eq!(diagnostic.code, DiagnosticCode::LowerInvalidSemantic);
        assert_eq!(diagnostic.span, main_span);
        assert_eq!(
            diagnostic.args,
            vec![DiagnosticArg::text("detail", "if condition is not bool")]
        );
        assert_eq!(
            LowerError::Diagnostics {
                diagnostics: Vec::new()
            }
            .diagnostic(),
            None
        );
    }

    /// Lowers `source` under `policy`, with its typing checked.
    fn lower_with_policy(source: &str, policy: CodegenPolicy) -> Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let sem = sem::lower(&ast::lower(&tree));
        assert_eq!(sem.diagnostics, Vec::new());
        let typing = typing::analyze(&sem, typing::TypingConfig::default());
        lower(&sem, &typing, policy).expect("lowering should succeed")
    }

    fn function_dump(module: &Module, name: &str) -> String {
        let function = module
            .functions
            .iter()
            .find(|function| function.name == name)
            .unwrap_or_else(|| panic!("no function {name}"));
        struct Dump<'a>(&'a Module, &'a Function);
        impl fmt::Display for Dump<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt_function(f, self.1)
            }
        }
        format!("{}", Dump(module, function))
    }

    /// Small integers whose sums typing proves fit, a proved cast and a
    /// decided `instance?` test.
    const PROVABLE: &str = "define class <shape> (<object>)\nend;\n\
        define class <circle> (<shape>)\nend;\n\
        define function area(c :: <circle>) => <integer>\n  3\nend;\n\
        define function main() => <integer>\n  \
          let s :: <shape> = make(<circle>);\n  \
          let n :: <integer> = 4;\n  \
          if (instance?(s, <circle>)) (n + 1) + area(as(<circle>, s)) else 0 end\nend;";

    #[test]
    fn off_lowering_reads_no_typing_fact() {
        let off = lower_with_policy(PROVABLE, CodegenPolicy::Off);
        assert_eq!(off.check_codegen, Vec::new());
        let main = function_dump(&off, "main");
        assert!(main.contains("instance?"), "{main}");
        assert!(main.contains(" = as <circle>"), "{main}");
        assert_eq!(main.matches("i64.add_ovf").count(), 2, "{main}");
    }

    #[test]
    fn proved_lowering_drops_what_typing_proved_and_records_it() {
        let proved = lower_with_policy(PROVABLE, CodegenPolicy::Proved);
        let main = function_dump(&proved, "main");
        assert!(!main.contains("instance?"), "{main}");
        assert!(!main.contains(" = as "), "{main}");
        // `n + 1` cannot overflow; adding `area`'s result still may.
        assert!(main.contains("= i64.add %r"), "{main}");
        assert_eq!(main.matches("i64.add_ovf").count(), 1, "{main}");
        let mut codegen = proved
            .check_codegen
            .iter()
            .map(|record| (record.site.0, record.codegen))
            .collect::<Vec<_>>();
        codegen.sort_by_key(|(site, _)| *site);
        // s0 is the test and s1 `n + 1`. s2, the cast, was a no-op without
        // typing too: the cast lowering drops is the one at the read of `s`
        // that sem refined to <circle>, which has no site. s3, the outer
        // sum, keeps its check.
        assert_eq!(
            codegen,
            [(0, CheckCodegen::Elided), (1, CheckCodegen::Specialized)]
        );
        let owner = proved.check_codegen[0].owner;
        assert_eq!(
            proved.check_codegen(owner, sem::SiteId(1)),
            CheckCodegen::Specialized
        );
        assert_eq!(
            proved.check_codegen(owner, sem::SiteId(3)),
            CheckCodegen::Kept
        );
    }

    #[test]
    fn proved_lowering_without_typing_lowers_as_off() {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, PROVABLE).syntax;
        let sem = sem::lower(&ast::lower(&tree));
        let mut typing = typing::analyze(&sem, typing::TypingConfig::default());
        typing.status = typing::TypingStatus::BlockedBySemanticDiagnostics { count: 1 };
        let blocked = lower(&sem, &typing, CodegenPolicy::Proved).expect("lowers");
        assert_eq!(blocked, lower_with_policy(PROVABLE, CodegenPolicy::Off));
    }

    #[test]
    fn a_closure_two_bodies_create_keeps_its_checks() {
        // A slot default is copied into every `make`, so its closure is
        // created by both functions and is typed twice; lowering trusts
        // neither. The same closure written in one body is specialized.
        let source = "define class <box> (<object>)\n  \
              slot f :: <callable>, init-value: function (x :: <integer>) => <integer>\n    \
                if (x < 10) if (x > 0) x + 1 else 0 end else 0 end\n  end;\n\
            end;\n\
            define function one() => <box>\n  make(<box>)\nend;\n\
            define function two() => <box>\n  make(<box>)\nend;\n\
            define function own() => <callable>\n  \
              function (x :: <integer>) => <integer>\n    \
                if (x < 10) if (x > 0) x + 1 else 0 end else 0 end\n  end\nend;";
        let proved = lower_with_policy(source, CodegenPolicy::Proved);
        let closures = proved
            .functions
            .iter()
            .filter(|function| matches!(function.kind, FunctionKind::Closure(_)))
            .map(|function| function_dump(&proved, &function.name))
            .collect::<Vec<_>>();
        assert_eq!(closures.len(), 2);
        assert!(closures[0].contains("i64.add_ovf"), "{}", closures[0]);
        assert!(!closures[1].contains("i64.add_ovf"), "{}", closures[1]);
    }

    #[test]
    fn numeric_dispatch_runs_the_selected_method_inline() {
        let source = "define function main() => <integer>\n  \
            as(<integer>, as(<object>, 20) + 1)\nend;";
        let off = function_dump(&lower_with_policy(source, CodegenPolicy::Off), "main");
        assert!(off.contains("select-method"), "{off}");
        let proved_module = lower_with_policy(source, CodegenPolicy::Proved);
        let proved = function_dump(&proved_module, "main");
        assert!(!proved.contains("select-method"), "{proved}");
        // Only the retry region calls indirectly.
        assert_eq!(proved.matches("call.indirect").count(), 1, "{proved}");
        assert!(proved.contains("unbox"), "{proved}");
        assert!(proved.contains("i64.add_ovf"), "{proved}");
        assert!(proved.contains("box.any"), "{proved}");
        assert!(
            proved_module
                .check_codegen
                .iter()
                .any(|record| record.codegen == CheckCodegen::Inlined)
        );
    }

    #[test]
    fn a_scalar_instance_test_unboxes_its_local_once() {
        let source = "define function poly(x :: <object>) => <integer>\n  \
            if (instance?(x, <integer>)) x * x + x else 0 end\nend;\n\
            define function main() => <integer>\n  poly(6)\nend;";
        for policy in [CodegenPolicy::Off, CodegenPolicy::Proved] {
            let poly = function_dump(&lower_with_policy(source, policy), "poly");
            assert_eq!(poly.matches("unbox <integer>").count(), 1, "{poly}");
            assert!(poly.contains("i64.mul_ovf"), "{poly}");
            assert!(poly.contains("i64.add_ovf"), "{poly}");
            assert!(!poly.contains("select-method"), "{poly}");
            assert!(!poly.contains("box.any"), "{poly}");
        }
    }

    #[test]
    fn a_refined_local_unboxes_only_where_it_is_read_and_reboxes_to_its_box() {
        let source = "define function keep(y :: <object>) => <object>\n  y\nend;\n\
            define function unread(x :: <object>) => <integer>\n  \
            if (instance?(x, <integer>)) 1 else 0 end\nend;\n\
            define function both(x :: <object>) => <object>\n  \
            if (instance?(x, <double-float>)) keep(x + 1.0) + keep(x) else x end\nend;\n\
            define function main() => <integer>\n  unread(2)\nend;";
        let module = lower_with_policy(source, CodegenPolicy::Proved);
        let unread = function_dump(&module, "unread");
        assert!(!unread.contains("unbox"), "{unread}");
        // `keep(x)` passes the box `x` arrived in: the parameter register.
        let both = function_dump(&module, "both");
        assert_eq!(
            both.matches("unbox <double-float>, %r1").count(),
            1,
            "{both}"
        );
        assert!(both.contains("call l0(%r1)"), "{both}");
    }

    #[test]
    fn a_chain_of_proved_numeric_sends_boxes_only_at_its_end() {
        let source = "define function main() => <integer>\n  \
            as(<integer>, (as(<object>, 20) + 1) + 2)\nend;";
        let proved = function_dump(&lower_with_policy(source, CodegenPolicy::Proved), "main");
        assert!(!proved.contains("select-method"), "{proved}");
        assert_eq!(proved.matches("i64.add_ovf").count(), 2, "{proved}");
        // Only `as(<object>, 20)` boxes, and only it unboxes; the sums pass
        // from one send to the next, and to the cast, as `i64`s.
        assert_eq!(proved.matches("box.any").count(), 1, "{proved}");
        assert_eq!(proved.matches("unbox").count(), 1, "{proved}");
    }

    fn const_i64(function: &Function, reg: Reg) -> Option<i64> {
        function.instructions.iter().find_map(|instr| match instr {
            Instr::ConstI64 { dst, value, .. } if *dst == reg => Some(*value),
            _ => None,
        })
    }
}
