//! Semantic IR for the first generic-dispatch Interloko slice.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::ast::{self, KERNEL_VALUES};
use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticArgValue, DiagnosticCode};
use crate::identity::{AstNodeId, DefinitionId, DefinitionKind, SourceFileId};
use crate::promotion;
use crate::syntax::{self, Span};

mod built_in;
mod collection;
mod iteration;
mod keywords;
mod library;
pub use collection::{BUILTIN_METHODS, BuiltinMethod, CollectionFunction, MethodKey};
mod text;
use keywords::{KeywordDefault, KeywordSignature};
pub use text::{NAMED_TEXT_FUNCTIONS, TextClasses, text_function_name, text_functions_for_name};

/// The built-in collections, by how their operations work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CollectionKind {
    /// A `<vector>` or `<stretchy-vector>`, keyed by `<integer>` index.
    Vector,
    /// A `<table>`, keyed by any object.
    Table,
    /// A `<string>`, keyed by `<string-position>`.
    String,
}

use interloko_model::contract::{CallableContract, Contract, ContractPosition, ValueContract};
use interloko_model::sealing::Sealing;
pub use interloko_model::type_object::TypeObject;
use interloko_model::types::{Atom, ClassHierarchy, IntRange, Metaobject, Type, TypeNames};

pub use interloko_model::condition::ConditionClass;
pub use interloko_model::{
    BuiltinValueClass, ClassId, FloatBits, GenericFunctionId, LimitedInteger, MethodId, Number32,
    OBJECT_CLASS_ID, SingleFloatBits, SlotId,
};
pub use interloko_runtime::{FloatFunction, IntegerDivision, Rounding, TextFunction};

/// The keyword names of `signature`, in declaration order.
fn keyword_names(signature: &KeywordSignature) -> Vec<String> {
    signature
        .keywords
        .iter()
        .map(|slot| slot.name.clone())
        .collect()
}

/// `signature`, whose last `names.len()` parameters are keyword
/// parameters named `names`, with those moved to its keywords.
fn split_keywords(mut signature: CallableSignature, names: Vec<String>) -> CallableSignature {
    let required = signature.params.len().saturating_sub(names.len());
    let kinds = signature.params.split_off(required);
    signature.param_sigs.truncate(required);
    signature.param_ranges.truncate(required);
    signature.keywords = names
        .into_iter()
        .zip(kinds)
        .map(|(name, kind)| CallableKeyword { name, kind })
        .collect();
    signature
}

/// `call` after the hidden `let`s a keyword call evaluates its arguments
/// into, when it needs any.
fn with_lets(lets: Vec<Let>, call: Expr, span: Span) -> Expr {
    if lets.is_empty() {
        return call;
    }
    Expr::Block {
        exit: None,
        body: Box::new(Block {
            statements: lets.into_iter().map(Statement::Let).collect(),
            result: call,
            span,
        }),
        cleanup: None,
        span,
    }
}

/// Rewrites each argument type mismatch in `diagnostics`, reported for a
/// call of a callee taking `signature`'s keywords, that is about a keyword
/// argument to name the keyword, rather than its parameter's position.
fn name_keyword_mismatches(diagnostics: &mut [Diagnostic], signature: &KeywordSignature) {
    for diagnostic in diagnostics {
        if !matches!(
            diagnostic.code,
            DiagnosticCode::FunctionArgumentTypeMismatch
                | DiagnosticCode::GenericArgumentTypeMismatch
        ) {
            continue;
        }
        let Some(position) = diagnostic.args.iter().find_map(|arg| match arg.value {
            DiagnosticArgValue::Unsigned(index) if arg.name == "argument_index" => {
                usize::try_from(index)
                    .ok()?
                    .checked_sub(1 + signature.required)
            }
            _ => None,
        }) else {
            continue;
        };
        let Some(slot) = signature.keywords.get(position) else {
            continue;
        };
        diagnostic.code = DiagnosticCode::KeywordArgumentTypeMismatch;
        for arg in &mut diagnostic.args {
            match arg.name.as_str() {
                "argument_index" => *arg = DiagnosticArg::text("keyword", slot.name.clone()),
                "function" | "generic" => arg.name = "callee".into(),
                _ => {}
            }
        }
    }
}

/// The diagnostic for a definition of `name`, which the kernel reserves.
fn reserved_name(name: &str, span: Span) -> Diagnostic {
    Diagnostic::error(span, DiagnosticCode::ReservedName)
        .with_arg(DiagnosticArg::text("name", name.to_string()))
}

/// Durable direct function identifier within a semantic module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FunctionId(
    /// Zero-based function index.
    pub u32,
);

/// Durable anonymous closure identifier within a semantic module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClosureId(
    /// Zero-based closure index.
    pub u32,
);

/// Durable top-level constant identifier within a semantic module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConstantId(
    /// Zero-based constant index.
    pub u32,
);

/// Durable local identifier within a semantic function or method.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalId(
    /// Zero-based local index.
    pub u32,
);

/// Durable capture identifier within a semantic closure.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct CaptureId(
    /// Zero-based capture index.
    pub u32,
);

/// Check site identifier within its owning definition.
///
/// Every expression that leaves a runtime check has one: generic calls,
/// casts, instance tests, numeric conversions, and integer arithmetic.
/// Semantic lowering numbers them densely, in evaluation order, per tree
/// root: a function, method, or constant body (closures inside it share its
/// numbering), or a slot initializer. A site id therefore depends only on its
/// owner's own body, so `(owner definition, site)` names one check across
/// edits to other definitions. Typing decisions, dispatch-site records, and
/// runtime blame key on that pair.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SiteId(
    /// Zero-based site index within the owner.
    pub u32,
);

impl SiteId {
    /// Placeholder carried until [`assign_check_sites`] numbers the owner.
    const PENDING: Self = Self(u32::MAX);
}

impl core::fmt::Display for SiteId {
    /// Renders the id as `s<index>`, such as `s3`.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "s{}", self.0)
    }
}

/// Stable class identifier within a [`HostMetaRegistry`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct HostClassId(
    /// Zero-based host-backed class index.
    pub u32,
);

/// Stable slot identifier within a host-backed class declaration.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct HostSlotId(
    /// Zero-based host-backed slot index within the host class.
    pub u32,
);

/// Stable host-backed function identifier within a [`HostMetaRegistry`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct HostFunctionId(
    /// Zero-based host-backed function index.
    pub u32,
);

/// Statically known callable value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KnownCallable {
    /// A direct source function.
    DirectFunction(FunctionId),
    /// A generic function dispatcher, a slot's getter among them.
    GenericFunction(GenericFunctionId),
}

/// Semantic value category for this slice.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// Top value category corresponding to source `<object>`.
    Any,
    /// Signed integer value.
    I64,
    /// IEEE 754 binary64 float value corresponding to source `<float>`.
    F64,
    /// `<i32>` integer: an `<integer>` known to fit in 32 signed bits.
    I32,
    /// `<u32>` integer: an `<integer>` known to fit in 32 unsigned bits.
    U32,
    /// IEEE 754 binary32 float value, source `<single-float>`.
    F32,
    /// Boolean value.
    Bool,
    /// `<character>`: an unboxed Unicode scalar value.
    Char,
    /// `<string-position>`: an unboxed byte offset into a string, which the
    /// string operations check wherever it is used.
    Position,
    /// Runtime string object.
    String(ClassId),
    /// Runtime interned symbol object.
    Symbol(ClassId),
    /// Runtime immutable vector object.
    Vector(ClassId),
    /// Runtime object with statically known nominal class.
    Object(ClassId),
    /// Class metaobject for this class or one of its subclasses: a class
    /// literal names its class exactly, but `class-of` records only the
    /// static bound of its operand.
    ClassObject(ClassId),
    /// Generic-function metaobject naming a statically known generic.
    GenericFunctionObject(GenericFunctionId),
    /// Method metaobject naming a statically known method.
    MethodObject(MethodId),
    /// Slot descriptor metaobject naming a statically known effective slot.
    SlotDescriptorObject {
        /// Class whose effective slot is described.
        class: ClassId,
        /// Effective slot described by this value.
        slot: SlotId,
    },
    /// First-class callable value with a runtime closure representation.
    Callable,
    /// Compile-time-known callable object.
    KnownCallable(KnownCallable),
    /// Invalid or unknown value.
    Error,
}

impl ValueKind {
    /// Returns whether this is an unboxed number: `<integer>`, `<i32>`,
    /// `<u32>`, `<double-float>` or `<single-float>`.
    #[must_use]
    pub const fn is_number(self) -> bool {
        matches!(
            self,
            Self::I64 | Self::F64 | Self::I32 | Self::U32 | Self::F32
        )
    }
}

/// Callable argument and result categories carried by a typed callable value.
///
/// A parameter or result that is itself a typed callable carries its own
/// signature in [`Self::param_sigs`] or [`Self::result_sigs`], so a
/// `callable(callable(<integer>) => <integer>) => <integer>` keeps the inner
/// signature rather than reading as a plain `<callable>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableSignature {
    /// Parameter value categories in call order.
    pub params: Vec<ValueKind>,
    /// Signatures of typed callable parameters, parallel to `params`;
    /// `None` where a parameter is not one.
    pub param_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` parameters, parallel to
    /// `params`; `None` where a parameter is not one.
    pub param_ranges: Vec<Option<IntRange>>,
    /// Result value categories in return order.
    pub results: Vec<ValueKind>,
    /// Signatures of typed callable results, parallel to `results`; `None`
    /// where a result is not one.
    pub result_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` results, parallel to `results`;
    /// `None` where a result is not one.
    pub result_ranges: Vec<Option<IntRange>>,
    /// `#key` parameters after `params`, in declaration order. A call
    /// through the value passes each as a boxed `<object>`, the
    /// keyword-absent marker ([`Intrinsic::KeywordAbsent`]) when it does not
    /// pass it, and the value's entry fills its default and checks its
    /// type.
    pub keywords: Vec<CallableKeyword>,
    /// Whether the last of `params` is a `#rest` parameter, a `<vector>`:
    /// a call passes the arguments after the required ones in a vector
    /// it makes, rather than one for each parameter.
    pub rest: bool,
}

/// How an [`Expr::CallableCall`] passes the value its arguments.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum CallArguments {
    /// One argument for each of the value's parameters, as its signature
    /// declares them, or for each the untyped call passes.
    #[default]
    Each,
    /// The value's signature takes `#rest`: the last argument is the
    /// `<vector>` of the arguments after the required ones, which the
    /// call made.
    Rest,
    /// `apply`'s: the one argument is a `<vector>` of every argument,
    /// however many the value takes.
    Spread,
}

/// One `#key` parameter of a [`CallableSignature`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableKeyword {
    /// The keyword, without its colon.
    pub name: String,
    /// The parameter's value category.
    pub kind: ValueKind,
}

impl CallableSignature {
    /// Returns a signature none of whose parameters or results carries a
    /// signature of its own.
    #[must_use]
    pub fn flat(params: Vec<ValueKind>, results: Vec<ValueKind>) -> Self {
        Self {
            param_sigs: vec![None; params.len()],
            param_ranges: vec![None; params.len()],
            result_sigs: vec![None; results.len()],
            result_ranges: vec![None; results.len()],
            params,
            results,
            keywords: Vec::new(),
            rest: false,
        }
    }

    /// The number of arguments a call must pass at least: every
    /// parameter's, or every one before the `#rest` parameter.
    #[must_use]
    pub fn required(&self) -> usize {
        self.params.len() - usize::from(self.rest)
    }

    /// Returns the signature of the zero-based parameter `index`, when it is
    /// a typed callable.
    #[must_use]
    pub fn param_sig(&self, index: usize) -> Option<&Self> {
        self.param_sigs.get(index).and_then(Option::as_ref)
    }

    /// Returns the signature of the zero-based result `index`, when it is a
    /// typed callable.
    #[must_use]
    pub fn result_sig(&self, index: usize) -> Option<&Self> {
        self.result_sigs.get(index).and_then(Option::as_ref)
    }
    /// Returns the range of the zero-based parameter `index`, when it is
    /// a limited integer.
    #[must_use]
    pub fn param_range(&self, index: usize) -> Option<IntRange> {
        self.param_ranges.get(index).copied().flatten()
    }

    /// Returns the range of the zero-based result `index`, when it is a
    /// limited integer.
    #[must_use]
    pub fn result_range(&self, index: usize) -> Option<IntRange> {
        self.result_ranges.get(index).copied().flatten()
    }

    fn param_value(&self, index: usize) -> SignatureValue<'_> {
        SignatureValue {
            kind: self.params.get(index).copied().unwrap_or(ValueKind::Error),
            signature: self.param_sig(index),
            range: self.param_range(index),
        }
    }

    fn result_value(&self, index: usize) -> SignatureValue<'_> {
        SignatureValue {
            kind: self.results.get(index).copied().unwrap_or(ValueKind::Error),
            signature: self.result_sig(index),
            range: self.result_range(index),
        }
    }
}

/// The discriminator of the definitions a built-in's value is made of: its
/// hidden kernel function, or its built-in generic and kernel methods.
const BUILT_IN_DISCRIMINATOR: &str = "built-in";

/// The tag of the syntax a built-in's value calls it with.
const BUILT_IN_TAG: u64 = 0x0062_7569_6c74_696e;

/// The parameters of a built-in's value, in order.
const BUILT_IN_PARAMS: &[&str] = &["a", "b", "c", "d"];

/// The hidden locals `apply` evaluates into: its function, the vector of
/// its arguments, its sequence, and each element as it is collected. A
/// space cannot appear in a source name.
const APPLY_FUNCTION: &str = "apply function";
const APPLY_ARGUMENTS: &str = "apply arguments";
const APPLY_SEQUENCE: &str = "apply sequence";
const APPLY_ELEMENT: &str = "apply element";

/// How diagnostics name the callee of a call of an operand's value, such
/// as `f(1)(2)`, which has no name of its own.
const VALUE_CALLEE: &str = "called value";

fn slot_setter_name(slot_name: &str) -> String {
    format!("{slot_name}-setter")
}

fn intrinsic_for_name(name: &str) -> Option<Intrinsic> {
    Some(match name {
        "object-class" => Intrinsic::ObjectClass,
        "class-name" => Intrinsic::ClassName,
        "class-direct-superclass" => Intrinsic::ClassDirectSuperclass,
        "class-direct-slots" => Intrinsic::ClassDirectSlots,
        "class-effective-slots" => Intrinsic::ClassEffectiveSlots,
        "slot-name" => Intrinsic::SlotName,
        "slot-type" => Intrinsic::SlotType,
        "slot-init-keyword" => Intrinsic::SlotInitKeyword,
        "slot-mutable?" => Intrinsic::SlotMutable,
        "slot-getter" => Intrinsic::SlotGetter,
        "slot-setter" => Intrinsic::SlotSetter,
        "generic-function-name" => Intrinsic::GenericFunctionName,
        "generic-function-methods" => Intrinsic::GenericFunctionMethods,
        "method-generic-function" => Intrinsic::MethodGenericFunction,
        "method-specializers" => Intrinsic::MethodSpecializers,
        "key-exists?" => Intrinsic::TableKeyExists,
        "pair" => Intrinsic::ListPair,
        "head" => Intrinsic::ListHead,
        "tail" => Intrinsic::ListTail,
        "head-setter" => Intrinsic::ListHeadSetter,
        "tail-setter" => Intrinsic::ListTailSetter,
        "remove-key!" => Intrinsic::TableRemoveKey,
        // The kernel's iteration protocol methods call these; a `%` cannot
        // appear in a source name, so no program can call or shadow them.
        "%collection-size" => Intrinsic::CollectionSize,
        "%vector-element" => Intrinsic::VectorElement,
        "%vector-element-setter" => Intrinsic::VectorElementSetter,
        "%vector-constant?" => Intrinsic::VectorConstant,
        "%table-element-setter" => Intrinsic::TableElementSetter,
        "%table-version" => Intrinsic::TableVersion,
        "%table-positions" => Intrinsic::TablePositions,
        "%table-next-position" => Intrinsic::TableNextPosition,
        "%table-position-key" => Intrinsic::TablePositionKey,
        "%table-position-value" => Intrinsic::TablePositionValue,
        "%start-position" => Intrinsic::Text(TextFunction::StartPosition),
        "%end-position" => Intrinsic::Text(TextFunction::EndPosition),
        "%next-position" => Intrinsic::Text(TextFunction::NextPosition),
        "%string-element" => Intrinsic::Text(TextFunction::StringElement),
        name if name.starts_with('%') => {
            return IntegerDivision::ALL
                .iter()
                .find(|function| integer_division_name(**function) == name)
                .map(|function| Intrinsic::IntegerDivision(*function));
        }
        "float-to-string" => Intrinsic::FloatToString,
        "integer-to-string" => Intrinsic::IntegerToString,
        "concatenate" => Intrinsic::Concatenate,
        "instance?" => Intrinsic::InstanceOfType,
        "limited-integer-base" => Intrinsic::LimitedIntegerBase,
        "limited-integer-min" => Intrinsic::LimitedIntegerMin,
        "limited-integer-max" => Intrinsic::LimitedIntegerMax,
        "subclass-type-class" => Intrinsic::SubclassTypeClass,
        "type-name" => Intrinsic::TypeName,
        "byte-offset" => Intrinsic::PositionToInteger,
        _ => return None,
    })
}

const fn intrinsic_arity(intrinsic: Intrinsic) -> usize {
    match intrinsic {
        Intrinsic::ObjectClass
        | Intrinsic::ClassName
        | Intrinsic::ClassDirectSuperclass
        | Intrinsic::ClassDirectSlots
        | Intrinsic::ClassEffectiveSlots
        | Intrinsic::SlotName
        | Intrinsic::SlotType
        | Intrinsic::SlotInitKeyword
        | Intrinsic::SlotMutable
        | Intrinsic::SlotGetter
        | Intrinsic::SlotSetter
        | Intrinsic::GenericFunctionName
        | Intrinsic::GenericFunctionMethods
        | Intrinsic::MethodGenericFunction
        | Intrinsic::MethodSpecializers
        | Intrinsic::CollectionSize
        | Intrinsic::LimitedIntegerBase
        | Intrinsic::LimitedIntegerMin
        | Intrinsic::LimitedIntegerMax
        | Intrinsic::SubclassTypeClass
        | Intrinsic::TypeName => 1,
        Intrinsic::VectorElement
        | Intrinsic::StretchyAdd
        | Intrinsic::TableElement
        | Intrinsic::TableKeyExists
        | Intrinsic::TableRemoveKey
        | Intrinsic::Concatenate
        | Intrinsic::InstanceOfType => 2,
        Intrinsic::VectorElementOr
        | Intrinsic::VectorElementSetter
        | Intrinsic::TableElementOr
        | Intrinsic::TableElementSetter => 3,
        Intrinsic::MakeVector | Intrinsic::MakeStretchyVector => 2,
        Intrinsic::MakeTable => 0,
        Intrinsic::Text(function) => function.params().len(),
        Intrinsic::CharCompare(_)
        | Intrinsic::PositionCompare(_)
        | Intrinsic::IntegerDivision(_)
        | Intrinsic::StringStep => 2,
        Intrinsic::CharToInteger | Intrinsic::PositionToInteger => 1,
        Intrinsic::TableVersion | Intrinsic::TablePositions => 1,
        Intrinsic::TablePositionValue | Intrinsic::TablePositionKey => 3,
        Intrinsic::VectorConstant => 1,
        Intrinsic::TableNextPosition => 3,
        Intrinsic::Unsupplied | Intrinsic::KeywordAbsent | Intrinsic::EmptyList => 0,
        Intrinsic::FloatToString | Intrinsic::IntegerToString => 1,
        Intrinsic::ListHead | Intrinsic::ListTail => 1,
        Intrinsic::ListPair | Intrinsic::ListHeadSetter | Intrinsic::ListTailSetter => 2,
    }
}

fn intrinsic_expected_name(intrinsic: Intrinsic, index: usize) -> &'static str {
    match (intrinsic, index) {
        (Intrinsic::ObjectClass, 0) => "<object>",
        (
            Intrinsic::ClassName
            | Intrinsic::ClassDirectSuperclass
            | Intrinsic::ClassDirectSlots
            | Intrinsic::ClassEffectiveSlots,
            0,
        ) => "<class>",
        (
            Intrinsic::SlotName
            | Intrinsic::SlotType
            | Intrinsic::SlotInitKeyword
            | Intrinsic::SlotMutable
            | Intrinsic::SlotGetter
            | Intrinsic::SlotSetter,
            0,
        ) => "<slot-descriptor>",
        (Intrinsic::GenericFunctionName | Intrinsic::GenericFunctionMethods, 0) => {
            "<generic-function>"
        }
        (Intrinsic::MethodGenericFunction | Intrinsic::MethodSpecializers, 0) => "<method>",
        (Intrinsic::CollectionSize, 0) => "<vector> or <table>",
        (Intrinsic::VectorElement | Intrinsic::VectorElementOr, 0)
        | (Intrinsic::VectorElementSetter, 1) => "<vector>",
        (Intrinsic::StretchyAdd, 0) => "<stretchy-vector>",
        (
            Intrinsic::TableVersion
            | Intrinsic::TablePositions
            | Intrinsic::TableNextPosition
            | Intrinsic::TablePositionValue
            | Intrinsic::TablePositionKey,
            0,
        ) => "<table>",
        (
            Intrinsic::TableNextPosition
            | Intrinsic::TablePositionValue
            | Intrinsic::TablePositionKey,
            _,
        ) => "<integer>",
        (Intrinsic::VectorConstant, 0) => "<vector>",
        (
            Intrinsic::TableElement
            | Intrinsic::TableElementOr
            | Intrinsic::TableKeyExists
            | Intrinsic::TableRemoveKey,
            0,
        )
        | (Intrinsic::TableElementSetter, 1) => "<table>",
        (Intrinsic::VectorElement | Intrinsic::VectorElementOr, 1)
        | (Intrinsic::VectorElementSetter, 2)
        | (Intrinsic::MakeVector | Intrinsic::MakeStretchyVector, 0)
        | (Intrinsic::IntegerToString, 0) => "<integer>",
        (Intrinsic::FloatToString, 0) => "<float>",
        (Intrinsic::Concatenate, _) => "<string>",
        (Intrinsic::InstanceOfType, 1) | (Intrinsic::TypeName, 0) => "<type>",
        (
            Intrinsic::LimitedIntegerBase
            | Intrinsic::LimitedIntegerMin
            | Intrinsic::LimitedIntegerMax,
            0,
        ) => "<limited-integer>",
        (Intrinsic::SubclassTypeClass, 0) => "<subclass-type>",
        (Intrinsic::Text(function), index) => function
            .params()
            .get(index)
            .map_or("<object>", |kind| text::text_kind_name(*kind)),
        (Intrinsic::CharCompare(_) | Intrinsic::CharToInteger, _) => "<character>",
        (Intrinsic::PositionCompare(_) | Intrinsic::PositionToInteger, _) => "<string-position>",
        (Intrinsic::IntegerDivision(_), _) => "<integer>",
        (Intrinsic::ListHead | Intrinsic::ListTail, 0) => "<list>",
        (Intrinsic::ListHeadSetter | Intrinsic::ListTailSetter, 1) => "<pair>",
        _ => "<object>",
    }
}

/// Registry of host-backed metaobjects supplied by the embedding Rust host.
///
/// This registry is intentionally structural and explicit. It does not inspect
/// Rust values. Later `facet` integration can populate this same registry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostMetaRegistry {
    classes: Vec<HostClass>,
    functions: Vec<HostFunction>,
}

impl HostMetaRegistry {
    /// Creates an empty host-backed metaobject registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            classes: Vec::new(),
            functions: Vec::new(),
        }
    }

    /// Adds a host-backed class declaration and returns its registry-local id.
    pub fn add_class(&mut self, class: HostClass) -> HostClassId {
        let id = HostClassId(u32::try_from(self.classes.len()).unwrap_or(u32::MAX));
        self.classes.push(class);
        id
    }

    /// Adds a host-backed function declaration and returns its registry-local id.
    pub fn add_function(&mut self, function: HostFunction) -> HostFunctionId {
        let id = HostFunctionId(u32::try_from(self.functions.len()).unwrap_or(u32::MAX));
        self.functions.push(function);
        id
    }

    /// Returns registered host-backed classes in registry order.
    #[must_use]
    pub fn classes(&self) -> &[HostClass] {
        &self.classes
    }

    /// Returns registered host-backed functions in registry order.
    #[must_use]
    pub fn functions(&self) -> &[HostFunction] {
        &self.functions
    }
}

/// Host-backed class declaration supplied by a Rust adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostClass {
    /// Loko-visible class name, such as `<rust-circle>`.
    pub name: String,
    /// Host-backed slots in source-visible slot order.
    pub slots: Vec<HostSlot>,
}

impl HostClass {
    /// Creates an empty host-backed class declaration.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            slots: Vec::new(),
        }
    }

    /// Adds a read-only host-backed slot declaration.
    #[must_use]
    pub fn with_slot(mut self, slot: HostSlot) -> Self {
        self.slots.push(slot);
        self
    }
}

/// Host-backed read-only slot declaration supplied by a Rust adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostSlot {
    /// Loko-visible slot/accessor name.
    pub name: String,
    /// Declared value category.
    pub kind: ValueKind,
    /// Human-readable type name used by dumps.
    pub type_name: String,
}

impl HostSlot {
    /// Creates a host-backed read-only slot declaration.
    #[must_use]
    pub fn new(name: &str, kind: ValueKind, type_name: &str) -> Self {
        Self {
            name: name.into(),
            kind,
            type_name: type_name.into(),
        }
    }
}

/// Host-backed function declaration supplied by a Rust adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostFunction {
    /// Loko-visible function name.
    pub name: String,
    /// Declared parameter value categories.
    pub param_kinds: Vec<HostValueKind>,
    /// Declared result value category.
    pub result_kind: HostValueKind,
}

impl HostFunction {
    /// Creates a host-backed function declaration.
    #[must_use]
    pub fn new(name: &str, param_kinds: Vec<HostValueKind>, result_kind: HostValueKind) -> Self {
        Self {
            name: name.into(),
            param_kinds,
            result_kind,
        }
    }
}

/// Scalar value category that may cross a host-function boundary.
///
/// These categories deliberately contain no compiler-local class ids. The
/// compiler resolves [`Self::String`] to the active environment's built-in
/// `<string>` class while it constructs semantic IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostValueKind {
    /// Signed 64-bit integer.
    Integer,
    /// IEEE 754 binary64 float.
    Float,
    /// `<i32>` integer.
    I32,
    /// `<u32>` integer.
    U32,
    /// IEEE 754 binary32 float, `<single-float>`.
    SingleFloat,
    /// Boolean value.
    Boolean,
    /// Owned UTF-8 string value.
    String,
}

/// Resolved host-backed function in a semantic module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedHostFunction {
    /// Registry-local function id.
    pub id: HostFunctionId,
    /// Loko-visible function name.
    pub name: String,
    /// Resolved parameter categories.
    pub param_kinds: Vec<ValueKind>,
    /// Resolved result category.
    pub result_kind: ValueKind,
}

/// Origin of a semantic class metaobject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClassOrigin {
    /// Built-in root class for every Interloko object.
    ObjectRoot,
    /// Class defined in Loko source.
    Native,
    /// Built-in value class supplied by the language kernel.
    BuiltinValue {
        /// Built-in value kind represented by this class.
        kind: BuiltinValueClass,
    },
    /// Condition class supplied by the language kernel, such as
    /// `<type-error>`. Its instances are native objects: programs make,
    /// signal, handle, and subclass it as they do a source class.
    Condition {
        /// The kernel condition class.
        class: ConditionClass,
    },
    /// Class declared by the host-backed metaobject registry.
    HostBacked {
        /// Registry-local host-backed class id.
        host_class: HostClassId,
    },
}

/// Origin of a semantic slot metaobject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotOrigin {
    /// Slot defined in Loko source.
    Native,
    /// Slot declared by the host-backed metaobject registry.
    HostBacked {
        /// Registry-local host-backed class id.
        host_class: HostClassId,
        /// Registry-local host-backed slot id.
        host_slot: HostSlotId,
    },
}

/// Slot allocation policy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotAllocation {
    /// Slot storage is allocated per constructed instance.
    Instance,
}

impl From<ast::SlotAllocation> for SlotAllocation {
    fn from(value: ast::SlotAllocation) -> Self {
        match value {
            ast::SlotAllocation::Instance => Self::Instance,
        }
    }
}

/// Slot mutability metadata.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotMutability {
    /// Ordinary slot metadata. Slot writes are not implemented yet.
    Mutable,
    /// `constant slot` metadata. Values are set only during construction in
    /// this slice.
    Constant,
}

impl From<ast::SlotMutability> for SlotMutability {
    fn from(value: ast::SlotMutability) -> Self {
        match value {
            ast::SlotMutability::Mutable => Self::Mutable,
            ast::SlotMutability::Constant => Self::Constant,
        }
    }
}

/// Optional generic function parameter contract.
///
/// `None` preserves the legacy unannotated generic behavior; `Some(kind)` is an
/// explicit source contract.
pub type GenericParamKind = Option<ValueKind>;

/// One method specializer in parameter order.
///
/// `None` is reserved for invalid or legacy compatibility records. Source
/// `<object>` is represented by [`OBJECT_CLASS_ID`].
pub type MethodSpecializer = Option<ClassId>;

/// The built-in generic an operator's name, written `\+` as a value,
/// names: the one its operands send to when they are not all numbers
/// typing knows. Unary `-` is `negative`, so `\-` is subtraction.
fn operator_generic(name: &str) -> Option<promotion::NumericOperator> {
    use promotion::NumericOperator::{Arithmetic, Compare};
    [
        Arithmetic(BinaryOp::Add),
        Arithmetic(BinaryOp::Sub),
        Arithmetic(BinaryOp::Mul),
        Arithmetic(BinaryOp::Div),
        Arithmetic(BinaryOp::Pow),
        Compare(CompareOp::Eq),
        Compare(CompareOp::Lt),
        Compare(CompareOp::Le),
        Compare(CompareOp::Gt),
        Compare(CompareOp::Ge),
    ]
    .into_iter()
    .find(|operator| operator.name() == name)
}

/// Whether `specializer` names a class that did not resolve: the
/// placeholder a method's parameter takes after its type was reported, so
/// later checks of the method stay quiet rather than cascade.
const fn is_unresolved_specializer(specializer: MethodSpecializer) -> bool {
    matches!(specializer, Some(ClassId(u32::MAX)))
}

/// Origin of a semantic generic function metaobject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GenericFunctionOrigin {
    /// Generic declared directly in Loko source.
    Source,
    /// Compiler-synthesized generic for a writable native slot setter.
    SlotSetter {
        /// Class that introduced the writable slot.
        class: ClassId,
        /// Effective slot written by the setter method.
        slot: SlotId,
    },
    /// Built-in numeric operator dispatched on operands that are not
    /// statically numbers. Its methods come from [`crate::promotion`] and it
    /// is sealed: no method can be added on `<real>` operands.
    Numeric {
        /// Operator the generic implements.
        operator: promotion::NumericOperator,
    },
    /// Generic a slot's getter is, as in Dylan, whose methods read the slots
    /// of that name of the classes declaring one. It is sealed over those
    /// classes: a method for one of them or a subclass would override a
    /// slot's getter, which reads of a receiver of known class skip.
    SlotGetter {
        /// The generic, whose getter methods fix what it is sealed over.
        generic: GenericFunctionId,
    },
    /// Built-in generic a built-in function with several overloads is as
    /// a value, such as `sqrt` or `as-uppercase` (see the `built_in`
    /// module): one kernel method per overload, each the built-in's call of
    /// its parameters. It is sealed over its kernel methods' classes, so a
    /// program's method may join it for classes of its own.
    BuiltIn {
        /// The generic, whose kernel methods fix what it is sealed over.
        generic: GenericFunctionId,
    },
    /// Built-in open collection generic, `size`, `element`,
    /// `element-setter`, `add!` or `forward-iteration-protocol`, with the
    /// kernel's methods from [`BUILTIN_METHODS`] (or, for the protocol, the
    /// `iteration` module). It is sealed only over the kernel classes those
    /// methods specialize on.
    Collection {
        /// The generic.
        function: CollectionFunction,
    },
}

impl GenericFunctionOrigin {
    /// Returns whether methods can be added to the generic for `specializers`.
    ///
    /// A numeric generic is sealed over `<real>`: its methods for numbers
    /// are fixed by the promotion rules, so a method whose specializers all
    /// lie within `<real>` would override them and is refused.
    #[must_use]
    pub fn admits_method(self, module: &Module, specializers: &[MethodSpecializer]) -> bool {
        match self {
            Self::Source => true,
            Self::SlotSetter { .. } => false,
            Self::SlotGetter { generic } => module
                .slot_getter_overridden(generic, specializers)
                .is_none(),
            Self::BuiltIn { generic } => {
                // The kernel's classes are the built-in's own, which a call
                // runs inline where typing knows them, so only a class of
                // the program's may take a method.
                let kernel_only = module.specializes_on_kernel_classes(specializers);
                // A method whose specializers may all meet a kernel
                // method's would override it for those arguments.
                !kernel_only
                    && !module.methods.iter().any(|kernel| {
                        kernel.generic == generic
                            && kernel.origin == MethodOrigin::BuiltIn
                            && kernel.specializers.len() == specializers.len()
                            && kernel.specializers.iter().zip(specializers).all(
                                |(kernel, specializer)| match (kernel, specializer) {
                                    (Some(kernel), Some(class)) => {
                                        module.classes_may_overlap(*kernel, *class)
                                    }
                                    _ => true,
                                },
                            )
                    })
            }
            Self::Numeric { .. } => {
                let Some(real) = module.builtin_value_class(BuiltinValueClass::Real) else {
                    return false;
                };
                !specializers.iter().all(|specializer| {
                    specializer.is_some_and(|class| module.is_same_or_subclass(class, real))
                })
            }
            Self::Collection {
                function: CollectionFunction::ForwardIterationProtocol,
            } => {
                // Sealed over the kernel classes with methods of their own,
                // and over `<sequence>` itself: a program's sequence class
                // may replace the default for its own instances, but not
                // for every sequence.
                let collection = specializers.first().copied().flatten();
                let sequence = module.builtin_value_class(BuiltinValueClass::Sequence);
                collection != sequence
                    && !iteration::SEALED_ITERATION_CLASSES.iter().any(|kernel| {
                        module
                            .builtin_value_class(*kernel)
                            .zip(collection)
                            .is_some_and(|(kernel, class)| {
                                module.is_same_or_subclass(class, kernel)
                            })
                    })
            }
            Self::Collection { function } => {
                // A method on a kernel class the generic has a method for,
                // or on a subclass, would override the kernel's: one of
                // the rows, or one the kernel library writes.
                let collection = specializers
                    .get(function.collection_index())
                    .copied()
                    .flatten();
                let overrides = |kernel: ClassId| {
                    collection.is_some_and(|class| module.is_same_or_subclass(class, kernel))
                };
                !collection::builtin_methods(function).any(|method| {
                    module
                        .builtin_value_class(method.collection)
                        .is_some_and(overrides)
                }) && !module.methods.iter().any(|method| {
                    method.origin == (MethodOrigin::Collection { function })
                        && method
                            .specializers
                            .get(function.collection_index())
                            .copied()
                            .flatten()
                            .is_some_and(|kernel| kernel != OBJECT_CLASS_ID && overrides(kernel))
                })
            }
        }
    }
}

/// Semantic module with resolved nominal IDs and calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Source file identity.
    pub source_file: SourceFileId,
    /// Nominal class definitions.
    pub classes: Vec<Class>,
    /// Generic function declarations.
    pub generics: Vec<GenericFunction>,
    /// Method definitions.
    pub methods: Vec<Method>,
    /// Direct function definitions.
    pub functions: Vec<Function>,
    /// Anonymous closure definitions.
    pub closures: Vec<Closure>,
    /// Immutable top-level constants.
    pub constants: Vec<Constant>,
    /// Host-backed functions made visible to this compilation.
    pub host_functions: Vec<ResolvedHostFunction>,
    /// Semantic diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Static result of selecting a generic method for exact runtime classes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MethodSelection {
    /// No method is applicable.
    None,
    /// Exactly one most-specific method applies.
    Selected(MethodId),
    /// Multiple incomparable applicable methods remain.
    Ambiguous,
}

impl Module {
    /// The class whose slot's getter a method of `generic` specialized on
    /// `specializers` would override, when there is one: a getter takes one
    /// receiver, so a method of a one-parameter generic named for a slot,
    /// specialized on a class with that slot, declared or inherited, would
    /// take its receivers from the slot's getter, which a read of a
    /// receiver of known class skips. This holds whether the getter is the
    /// generic's method, as a slot's getter is of its own generic, or
    /// serves its class beside the generic, as a slot named for a kernel
    /// collection generic does.
    #[must_use]
    pub fn slot_getter_overridden(
        &self,
        generic: GenericFunctionId,
        specializers: &[MethodSpecializer],
    ) -> Option<ClassId> {
        let generic = self.generics.get(generic.0 as usize)?;
        if generic.arity.saturating_sub(generic.keywords.len()) != 1 {
            return None;
        }
        let class = specializers.first().copied().flatten()?;
        self.classes
            .get(class.0 as usize)?
            .effective_slots
            .iter()
            .find(|slot| slot.accessor.name == generic.name)
            .map(|slot| slot.source_class)
    }

    /// Whether every one of `specializers` is a class of the kernel's:
    /// `<object>` or a built-in value class.
    #[must_use]
    pub fn specializes_on_kernel_classes(&self, specializers: &[MethodSpecializer]) -> bool {
        specializers.iter().all(|specializer| {
            specializer.is_none_or(|class| {
                self.classes.get(class.0 as usize).is_none_or(|class| {
                    matches!(
                        class.origin,
                        ClassOrigin::ObjectRoot | ClassOrigin::BuiltinValue { .. }
                    )
                })
            })
        })
    }

    /// Returns the classes whose direct superclass is `class`, in class id
    /// order.
    #[must_use]
    pub fn direct_subclasses(&self, class: ClassId) -> Vec<ClassId> {
        self.classes
            .iter()
            .filter(|other| other.superclass == Some(class))
            .map(|other| other.id)
            .collect()
    }

    /// Returns true when `actual` is the same class as `expected` or inherits
    /// from it through Interloko's single nominal superclass chain.
    #[must_use]
    pub fn is_same_or_subclass(&self, actual: ClassId, expected: ClassId) -> bool {
        is_same_or_subclass_in_classes(&self.classes, actual, expected)
    }

    /// Returns the number of superclass edges from `actual` to `expected`.
    ///
    /// `Some(0)` means the classes are identical. Larger distances mean
    /// `actual` is a deeper subclass of `expected`. `None` means the classes
    /// are unrelated by the single-inheritance chain.
    #[must_use]
    pub fn inheritance_distance(&self, actual: ClassId, expected: ClassId) -> Option<usize> {
        inheritance_distance_in_classes(&self.classes, actual, expected)
    }

    /// Returns the depth of `class` in the nominal superclass tree.
    #[must_use]
    pub fn class_depth(&self, class: ClassId) -> usize {
        class_depth_in_classes(&self.classes, class)
    }

    /// Returns the superclass chain from `class` to the root.
    #[must_use]
    pub fn superclass_chain(&self, class: ClassId) -> Vec<ClassId> {
        superclass_chain_in_classes(&self.classes, class)
    }

    /// Returns true when two class specializers can describe at least one
    /// common runtime class in Interloko's current single-inheritance lattice.
    #[must_use]
    pub fn classes_may_overlap(&self, left: ClassId, right: ClassId) -> bool {
        self.is_same_or_subclass(left, right) || self.is_same_or_subclass(right, left)
    }

    /// Returns true when `method` is applicable to an exact runtime class tuple.
    #[must_use]
    pub fn method_applies_to_classes(&self, method: &Method, actuals: &[ClassId]) -> bool {
        method_applies_to_classes_in_classes(&self.classes, method, actuals)
    }

    /// Returns whether generic parameter `index` carries boxed `<object>`
    /// values, so integer and float arguments are boxed at calls and the
    /// dispatcher tests their classes. Otherwise an `<object>` or
    /// unannotated parameter carries raw runtime-object handles.
    #[must_use]
    pub fn generic_param_boxes_scalars(&self, generic: GenericFunctionId, index: usize) -> bool {
        self.generics
            .get(generic.0 as usize)
            .is_some_and(|generic| {
                let methods = self
                    .methods
                    .iter()
                    .map(|method| (method.generic, method.specializers.as_slice()));
                generic_param_boxes_scalars_in(&self.classes, methods, generic, index)
            })
    }

    /// Returns true when `left` is strictly more specific than `right`.
    #[must_use]
    pub fn method_more_specific(&self, left: &Method, right: &Method) -> bool {
        method_more_specific_in_classes(&self.classes, left, right)
    }

    /// Selects the unique most-specific method for an exact runtime class tuple.
    #[must_use]
    pub fn select_method_for_classes(
        &self,
        generic: GenericFunctionId,
        actuals: &[ClassId],
    ) -> MethodSelection {
        select_method_for_classes_in_methods(&self.classes, &self.methods, generic, actuals)
    }

    /// Returns the kernel condition class `condition`, which every module
    /// defines.
    #[must_use]
    pub fn condition_class(&self, condition: ConditionClass) -> Option<ClassId> {
        self.classes
            .iter()
            .find(|class| class.origin == ClassOrigin::Condition { class: condition })
            .map(|class| class.id)
    }

    /// Returns the class metaobject for a built-in value class, when this
    /// module's environment defines it.
    #[must_use]
    pub fn builtin_value_class(&self, kind: BuiltinValueClass) -> Option<ClassId> {
        self.classes
            .iter()
            .find(|class| class.origin == ClassOrigin::BuiltinValue { kind })
            .map(|class| class.id)
    }

    /// Returns the value category used for values whose runtime class is the
    /// built-in value class `class`.
    ///
    /// Returns `None` for `<object>`, source classes, and host-backed classes,
    /// whose values are categorized as [`ValueKind::Object`] or
    /// [`ValueKind::Any`] by context rather than by class.
    #[must_use]
    pub fn builtin_value_kind(&self, class: ClassId) -> Option<ValueKind> {
        match self.classes.get(class.0 as usize)?.origin {
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::String,
            } => Some(ValueKind::String(class)),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Symbol,
            } => Some(ValueKind::Symbol(class)),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Integer,
            } => Some(ValueKind::I64),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::DoubleFloat,
            } => Some(ValueKind::F64),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::SingleFloat,
            } => Some(ValueKind::F32),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Boolean,
            } => Some(ValueKind::Bool),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Character,
            } => Some(ValueKind::Char),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::StringPosition,
            } => Some(ValueKind::Position),
            // A value declared `<function>` is any callable value.
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Function,
            } => Some(ValueKind::Callable),
            // Abstract numeric classes have no single representation.
            ClassOrigin::BuiltinValue {
                kind:
                    BuiltinValueClass::Number
                    | BuiltinValueClass::Real
                    | BuiltinValueClass::Rational
                    | BuiltinValueClass::Float,
            } => None,
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Vector,
            } => Some(ValueKind::Vector(class)),
            ClassOrigin::BuiltinValue {
                kind:
                    BuiltinValueClass::Class
                    | BuiltinValueClass::GenericFunction
                    | BuiltinValueClass::Method
                    | BuiltinValueClass::SlotDescriptor
                    | BuiltinValueClass::Type
                    | BuiltinValueClass::LimitedIntegerType
                    | BuiltinValueClass::SubclassType
                    | BuiltinValueClass::StringBuilder
                    | BuiltinValueClass::StretchyVector
                    | BuiltinValueClass::Table
                    | BuiltinValueClass::Sequence
                    | BuiltinValueClass::List
                    | BuiltinValueClass::Pair
                    | BuiltinValueClass::EmptyList,
            } => Some(ValueKind::Object(class)),
            ClassOrigin::ObjectRoot
            | ClassOrigin::Native
            | ClassOrigin::Condition { .. }
            | ClassOrigin::HostBacked { .. } => None,
        }
    }

    /// Returns the nominal class that bounds every runtime value of `kind`.
    ///
    /// Returns `None` for [`ValueKind::Any`] (unbounded) and
    /// [`ValueKind::Error`]. Every callable is a `<function>`.
    #[must_use]
    pub fn value_kind_class(&self, kind: ValueKind) -> Option<ClassId> {
        match kind {
            ValueKind::Object(class)
            | ValueKind::String(class)
            | ValueKind::Symbol(class)
            | ValueKind::Vector(class) => Some(class),
            // `<i32>` and `<u32>` are limited `<integer>` types, not classes.
            ValueKind::I64 | ValueKind::I32 | ValueKind::U32 => {
                self.builtin_value_class(BuiltinValueClass::Integer)
            }
            ValueKind::F64 => self.builtin_value_class(BuiltinValueClass::DoubleFloat),
            ValueKind::F32 => self.builtin_value_class(BuiltinValueClass::SingleFloat),
            ValueKind::ClassObject(_) => self.builtin_value_class(BuiltinValueClass::Class),
            ValueKind::GenericFunctionObject(_) => {
                self.builtin_value_class(BuiltinValueClass::GenericFunction)
            }
            ValueKind::MethodObject(_) => self.builtin_value_class(BuiltinValueClass::Method),
            ValueKind::SlotDescriptorObject { .. } => {
                self.builtin_value_class(BuiltinValueClass::SlotDescriptor)
            }
            ValueKind::Bool => self.builtin_value_class(BuiltinValueClass::Boolean),
            ValueKind::Char => self.builtin_value_class(BuiltinValueClass::Character),
            ValueKind::Position => self.builtin_value_class(BuiltinValueClass::StringPosition),
            ValueKind::Callable | ValueKind::KnownCallable(_) => {
                self.builtin_value_class(BuiltinValueClass::Function)
            }
            ValueKind::Any | ValueKind::Error => None,
        }
    }

    /// Returns the static type of every value `kind` can hold.
    ///
    /// A [`ValueKind`] is a representation: a register type plus boxing. This
    /// maps it to the set of values that representation carries, which the
    /// soft typing pass ([`crate::typing`]) then only ever narrows. Notes on
    /// the less obvious arms:
    ///
    /// - [`ValueKind::I64`] is `<integer>`, which is unbounded; the 64-bit
    ///   register is a limit of the current runtime, not of the type.
    /// - [`ValueKind::ClassObject`] is any `<class>`: `object-class` records
    ///   the static class bound of its operand there, not the runtime class,
    ///   so it does not name a singleton.
    /// - Every callable is [`Atom::Callable`], the `<function>` class,
    ///   whatever its signature: [`ValueKind::KnownCallable`] is too, even
    ///   when it names a generic function.
    /// - [`ValueKind::Error`] is [`Type::top`].
    #[must_use]
    pub fn kind_type(&self, kind: ValueKind) -> Type {
        let builtin = |class: BuiltinValueClass| {
            self.builtin_value_class(class)
                .map_or_else(Type::top, |class| Type::of(Atom::Cone(class), self))
        };
        match kind {
            ValueKind::Any | ValueKind::Error => Type::top(),
            ValueKind::I64 => Type::of(Atom::Integer(IntRange::INTEGER), self),
            ValueKind::I32 => Type::of(Atom::Integer(IntRange::I32), self),
            ValueKind::U32 => Type::of(Atom::Integer(IntRange::U32), self),
            ValueKind::F64 => builtin(BuiltinValueClass::DoubleFloat),
            ValueKind::F32 => builtin(BuiltinValueClass::SingleFloat),
            ValueKind::Bool => Type::union([Atom::Boolean(false), Atom::Boolean(true)], self),
            ValueKind::Char => builtin(BuiltinValueClass::Character),
            ValueKind::Position => builtin(BuiltinValueClass::StringPosition),
            ValueKind::String(class)
            | ValueKind::Symbol(class)
            | ValueKind::Vector(class)
            | ValueKind::Object(class) => Type::of(Atom::Cone(class), self),
            ValueKind::ClassObject(_) => builtin(BuiltinValueClass::Class),
            ValueKind::GenericFunctionObject(generic) => {
                Type::of(Atom::Singleton(Metaobject::Generic(generic)), self)
            }
            ValueKind::MethodObject(method) => {
                Type::of(Atom::Singleton(Metaobject::Method(method)), self)
            }
            ValueKind::SlotDescriptorObject { class, slot } => Type::of(
                Atom::Singleton(Metaobject::SlotDescriptor { class, slot }),
                self,
            ),
            ValueKind::Callable | ValueKind::KnownCallable(_) => Type::of(Atom::Callable, self),
        }
    }

    /// The locals of `definition`'s own frame, indexed by [`LocalId`], and
    /// its parameters among them: a function's or a method's, or a constant
    /// initializer's, which has no parameters.
    #[must_use]
    pub fn definition_locals(&self, definition: DefinitionId) -> Option<(&[Local], &[LocalId])> {
        if let Some(function) = self
            .functions
            .iter()
            .find(|function| function.definition == definition)
        {
            return Some((&function.locals, &function.params));
        }
        if let Some(method) = self
            .methods
            .iter()
            .find(|method| method.definition == definition)
        {
            return Some((&method.locals, &method.params));
        }
        if let Some(constant) = self
            .constants
            .iter()
            .find(|constant| constant.definition == definition)
        {
            return Some((constant.locals.as_slice(), [].as_slice()));
        }
        self.slot_initializer(definition)
            .map(|initializer| (initializer.locals.as_slice(), [].as_slice()))
    }

    /// The default code of the slot `definition` names, when its default is
    /// more than a literal.
    #[must_use]
    pub fn slot_initializer(&self, definition: DefinitionId) -> Option<&SlotInitializer> {
        self.direct_slot(definition)?.1.initializer.as_ref()
    }

    /// The class that declares the slot `definition` names, and the slot.
    #[must_use]
    pub fn direct_slot(&self, definition: DefinitionId) -> Option<(&Class, &Slot)> {
        self.classes.iter().find_map(|class| {
            class
                .direct_slots
                .iter()
                .find(|slot| slot.definition == definition)
                .map(|slot| (class, slot))
        })
    }

    /// The source name of the slot default `definition` names,
    /// `<class>.slot`, when that slot's default runs code.
    #[must_use]
    pub fn slot_default_name(&self, definition: DefinitionId) -> Option<String> {
        let (class, slot) = self.direct_slot(definition)?;
        slot.initializer
            .as_ref()
            .map(|_| format!("{}.{}", class.name, slot.name))
    }

    /// Returns the type contract of the generic, method or function
    /// `definition` names, or `None` for any other definition.
    ///
    /// A contract is what code compiled against the definition relies on:
    /// the types lowering trusts at each call without checking them. It maps
    /// the value kinds this module resolved through [`Self::kind_type`]:
    ///
    /// - a generic's parameters are its declared types, `<object>` when
    ///   undeclared, and its results are its declared result types or, for
    ///   an open result, the kinds inferred from its methods, which every
    ///   send trusts;
    /// - a method's parameters are its specialized parameter kinds and its
    ///   results are its generic's: a method is only ever called by a send
    ///   or dispatcher of its generic, which trusts the generic's result
    ///   types, never the method's own narrower ones;
    /// - a function's parameters and results are its declared or inferred
    ///   kinds.
    ///
    /// A declared `limited(<integer>, ...)` parameter or result is recorded
    /// as its range, since callers check arguments against a parameter's
    /// range and rely on a result's.
    ///
    /// A callable value also records the signature its callers call it
    /// with: a declared `callable(...) => ...` type, or for a
    /// [`ValueKind::KnownCallable`], the known function's, generic's or
    /// accessor's own signature, expanded the same way.
    ///
    /// The contract never comes from a finer analysis of a body (such as
    /// [`crate::typing`]), so a body edit changes a contract only when it
    /// changes the kinds lowering trusts.
    #[must_use]
    pub fn contract(&self, definition: DefinitionId) -> Option<Contract> {
        if let Some(generic) = self
            .generics
            .iter()
            .find(|generic| generic.definition == definition)
        {
            return Some(self.generic_contract(generic));
        }
        if let Some(method) = self
            .methods
            .iter()
            .find(|method| method.definition == definition)
        {
            return Some(self.method_contract(method));
        }
        self.functions
            .iter()
            .find(|function| function.definition == definition)
            .map(|function| self.function_contract(function))
    }

    /// Returns `generic`'s contract; see [`Self::contract`].
    #[must_use]
    pub fn generic_contract(&self, generic: &GenericFunction) -> Contract {
        let mut outer = vec![Some(KnownCallable::GenericFunction(generic.id))];
        self.generic_signature(generic, &mut outer)
    }

    /// Returns `method`'s contract; see [`Self::contract`].
    #[must_use]
    pub fn method_contract(&self, method: &Method) -> Contract {
        let mut outer = vec![None];
        let params = method
            .params
            .iter()
            .map(|param| match method.locals.get(param.0 as usize) {
                Some(local) => self.value_contract(
                    local.kind,
                    local.callable_signature.as_ref(),
                    local.range,
                    &mut outer,
                ),
                None => Type::top().into(),
            })
            .collect();
        let results = match self.generics.get(method.generic.0 as usize) {
            Some(generic) => self.value_contracts(
                &generic.result_kinds,
                &generic.result_callable_sigs,
                &generic.result_ranges,
                &mut outer,
            ),
            None => self.value_contracts(&method.result_kinds, &[], &[], &mut outer),
        };
        Contract {
            params,
            results,
            rest: false,
        }
    }

    /// Returns `function`'s contract; see [`Self::contract`].
    #[must_use]
    pub fn function_contract(&self, function: &Function) -> Contract {
        let mut outer = vec![Some(KnownCallable::DirectFunction(function.id))];
        self.function_signature(function, &mut outer)
    }

    fn generic_signature(
        &self,
        generic: &GenericFunction,
        outer: &mut Vec<Option<KnownCallable>>,
    ) -> Contract {
        Contract {
            params: generic
                .param_kinds
                .iter()
                .enumerate()
                .map(|(index, kind)| {
                    kind.map_or_else(
                        || Type::top().into(),
                        |kind| {
                            let range = generic.param_ranges.get(index).copied().flatten();
                            self.value_contract(kind, None, range, outer)
                        },
                    )
                })
                .collect(),
            results: self.value_contracts(
                &generic.result_kinds,
                &generic.result_callable_sigs,
                &generic.result_ranges,
                outer,
            ),
            rest: false,
        }
    }

    fn function_signature(
        &self,
        function: &Function,
        outer: &mut Vec<Option<KnownCallable>>,
    ) -> Contract {
        Contract {
            params: self.value_contracts(
                &function.param_kinds,
                &function.param_callable_sigs,
                &function.param_ranges,
                outer,
            ),
            results: self.value_contracts(
                &function.result_kinds,
                &function.result_callable_sigs,
                &function.result_ranges,
                outer,
            ),
            rest: function.rest,
        }
    }

    fn value_contracts(
        &self,
        kinds: &[ValueKind],
        signatures: &[Option<CallableSignature>],
        ranges: &[Option<IntRange>],
        outer: &mut Vec<Option<KnownCallable>>,
    ) -> Vec<ValueContract> {
        kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let signature = signatures.get(index).and_then(Option::as_ref);
                let range = ranges.get(index).copied().flatten();
                self.value_contract(*kind, signature, range, outer)
            })
            .collect()
    }

    /// What callers assume of one value of `kind`, called with `signature`
    /// when it is a typed callable, and within `range` when it is a limited
    /// integer. `outer` holds the signatures being expanded, innermost last,
    /// naming the known callable each belongs to.
    fn value_contract(
        &self,
        kind: ValueKind,
        signature: Option<&CallableSignature>,
        range: Option<IntRange>,
        outer: &mut Vec<Option<KnownCallable>>,
    ) -> ValueContract {
        let mut ty = self.kind_type(kind);
        if let Some(range) = range {
            ty = ty.meet(&Type::of(Atom::Integer(range), self), self);
        }
        // Applying a known callable trusts its own signature, whatever
        // signature the value also records.
        let signature = match (kind, signature) {
            (ValueKind::KnownCallable(callable), _) => {
                match outer.iter().rposition(|known| *known == Some(callable)) {
                    Some(index) => Some(CallableContract::Enclosing(outer.len() - 1 - index)),
                    None => {
                        outer.push(Some(callable));
                        let contract = self.known_callable_contract(callable, outer);
                        outer.pop();
                        contract.map(|contract| CallableContract::Signature(Box::new(contract)))
                    }
                }
            }
            (_, Some(signature)) => {
                outer.push(None);
                let contract = Contract {
                    params: self.value_contracts(
                        &signature.params,
                        &signature.param_sigs,
                        &signature.param_ranges,
                        outer,
                    ),
                    results: self.value_contracts(
                        &signature.results,
                        &signature.result_sigs,
                        &signature.result_ranges,
                        outer,
                    ),
                    rest: signature.rest,
                };
                outer.pop();
                Some(CallableContract::Signature(Box::new(contract)))
            }
            (_, None) => None,
        };
        ValueContract { ty, signature }
    }

    /// The signature a call of `callable` trusts, as sem resolves it when a
    /// value of [`ValueKind::KnownCallable`] is applied.
    fn known_callable_contract(
        &self,
        callable: KnownCallable,
        outer: &mut Vec<Option<KnownCallable>>,
    ) -> Option<Contract> {
        match callable {
            KnownCallable::DirectFunction(function) => self
                .functions
                .iter()
                .find(|candidate| candidate.id == function)
                .map(|function| self.function_signature(function, outer)),
            KnownCallable::GenericFunction(generic) => self
                .generics
                .get(generic.0 as usize)
                .map(|generic| self.generic_signature(generic, outer)),
        }
    }

    /// Returns the scalar value category of `expr`.
    ///
    /// This reads the categories semantic lowering already recorded on the
    /// expression tree (call result contracts, refined local kinds, cast
    /// targets) and does not re-check anything. An `if`, and a `block` with an
    /// exit, report the join of their arms: the arm category that holds the
    /// other's values. Expressions that produce zero or several values, and
    /// joins whose arms do not join, report [`ValueKind::Error`].
    #[must_use]
    pub fn expr_kind(&self, expr: &Expr) -> ValueKind {
        match expr {
            Expr::ConstI64 { .. } | Expr::CheckRange { .. } => ValueKind::I64,
            Expr::ConstF64 { .. } => ValueKind::F64,
            Expr::ConstChar { .. } => ValueKind::Char,
            Expr::Const32 { value, .. } => number32_kind(*value),
            Expr::Binary { kind, .. } | Expr::Negate { kind, .. } => kind.value_kind(),
            Expr::Convert { to, .. } => to.value_kind(),
            Expr::FloatCall { builtin, args, .. } => {
                float_call_kind(*builtin, args, |arg| self.expr_kind(arg))
            }
            Expr::ConstBool { .. }
            | Expr::Compare { .. }
            | Expr::MixedCompare { .. }
            | Expr::ValueEqual { .. }
            | Expr::Identical { .. }
            | Expr::InstanceOf { .. }
            | Expr::InstanceOfLimited { .. } => ValueKind::Bool,
            Expr::StringLiteral { class, .. } => ValueKind::String(*class),
            Expr::SymbolLiteral { class, .. } => ValueKind::Symbol(*class),
            Expr::VectorLiteral { class, .. } => ValueKind::Vector(*class),
            Expr::Values { result_kinds, .. }
            | Expr::CallableCall { result_kinds, .. }
            | Expr::Signal { result_kinds, .. } => scalar_result_kind(result_kinds),
            Expr::Local { result_kind, .. }
            | Expr::Capture { result_kind, .. }
            | Expr::ConstantRef { result_kind, .. }
            | Expr::SlotDefault { result_kind, .. }
            | Expr::Closure { result_kind, .. }
            | Expr::IntrinsicCall { result_kind, .. }
            | Expr::HostCall { result_kind, .. }
            | Expr::AccessorCall { result_kind, .. }
            | Expr::SlotWrite { result_kind, .. }
            | Expr::AssignLocal { result_kind, .. }
            | Expr::AssignCapture { result_kind, .. } => *result_kind,
            Expr::Loop { .. } => scalar_result_kind(&[]),
            Expr::ClassObject { class, .. } | Expr::ClassOf { class, .. } => {
                ValueKind::ClassObject(*class)
            }
            Expr::TypeObject { class, .. } => ValueKind::Object(*class),
            Expr::KnownCallable { callable, .. } => ValueKind::KnownCallable(*callable),
            Expr::GenericFunctionObject { generic, .. } => {
                ValueKind::GenericFunctionObject(*generic)
            }
            Expr::DirectCall { function, .. } => self
                .functions
                .get(function.0 as usize)
                .map_or(ValueKind::Error, |function| {
                    scalar_result_kind(&function.result_kinds)
                }),
            Expr::GenericCall { generic, .. } => self
                .generics
                .get(generic.0 as usize)
                .map_or(ValueKind::Error, |generic| {
                    scalar_result_kind(&generic.result_kinds)
                }),
            Expr::Handle { restart, .. } => scalar_result_kind(&restart.result_kinds),
            Expr::Block { .. } => scalar_result_kind(&self.expr_kinds(expr)),
            Expr::Discard { .. } => ValueKind::Error,
            Expr::Cast { class, .. } if *class == OBJECT_CLASS_ID => ValueKind::Any,
            Expr::Cast { class, .. } => self
                .builtin_value_kind(*class)
                .unwrap_or(ValueKind::Object(*class)),
            Expr::MakeObject { class_id, .. } => ValueKind::Object(*class_id),
            Expr::MakeRuntimeClass { bound, .. } => ValueKind::Object(*bound),
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => join_kind(
                &self.classes,
                self.expr_kind(then_branch),
                self.expr_kind(else_branch),
                |expected, actual| self.kind_holds(expected, actual),
            )
            .unwrap_or(ValueKind::Error),
            Expr::Error { .. } => ValueKind::Error,
        }
    }

    /// Returns the value categories `expr` produces, in order.
    ///
    /// This is [`Module::expr_kind`] for expressions that may produce several
    /// values: `values(...)`, calls, `signal`, `handle`, and the `if` and
    /// `block` joins over them. A join whose arms do not join reports one
    /// [`ValueKind::Error`].
    #[must_use]
    pub fn expr_kinds(&self, expr: &Expr) -> Vec<ValueKind> {
        let join = |lhs: &[ValueKind], rhs: &[ValueKind]| {
            join_arm_kinds(&self.classes, lhs, rhs, |expected, actual| {
                self.kind_holds(expected, actual)
            })
            .unwrap_or_else(|| vec![ValueKind::Error])
        };
        match expr {
            Expr::Values { result_kinds, .. }
            | Expr::CallableCall { result_kinds, .. }
            | Expr::Signal { result_kinds, .. } => result_kinds.clone(),
            Expr::DirectCall { function, .. } => {
                self.functions.get(function.0 as usize).map_or_else(
                    || vec![ValueKind::Error],
                    |function| function.result_kinds.clone(),
                )
            }
            Expr::GenericCall { generic, .. } => self.generics.get(generic.0 as usize).map_or_else(
                || vec![ValueKind::Error],
                |generic| generic.result_kinds.clone(),
            ),
            Expr::Handle { restart, .. } => restart.result_kinds.clone(),
            Expr::Discard { .. } => Vec::new(),
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => join(&self.expr_kinds(then_branch), &self.expr_kinds(else_branch)),
            Expr::Block {
                exit: Some(exit),
                body,
                ..
            } => join(&self.expr_kinds(&body.result), &exit.result_kinds),
            Expr::Block { body, .. } => self.expr_kinds(&body.result),
            Expr::Loop { .. } => Vec::new(),
            _ => vec![self.expr_kind(expr)],
        }
    }

    /// Returns whether values of category `actual` may flow where `expected`
    /// is required; the relation semantic checking used to build this module.
    fn kind_holds(&self, expected: ValueKind, actual: ValueKind) -> bool {
        kind_holds(
            expected,
            actual,
            |class, ancestor| self.is_same_or_subclass(class, ancestor),
            |kind| self.value_kind_class(kind),
        )
    }
}

/// The class structure of a class table, for the type lattice: a module's,
/// or the one sem is still building.
#[derive(Copy, Clone)]
struct ClassTable<'a>(&'a [Class]);

impl ClassHierarchy for ClassTable<'_> {
    fn superclass(&self, class: ClassId) -> Option<ClassId> {
        self.0.get(class.0 as usize)?.superclass
    }

    /// Source classes cannot inherit from built-in value classes or
    /// host-backed classes, so such a class is closed when no other built-in
    /// class inherits from it either.
    ///
    /// The check is structural rather than a list of names: abstract
    /// built-ins such as `<number>`, `<real>`, `<rational>`, and `<float>`
    /// have built-in subclasses and so are never closed, while concrete
    /// leaves such as `<integer>` and `<double-float>` are.
    ///
    /// A source class is closed when it is sealed, not abstract, and has no
    /// subclasses: its cone is exactly its own instances.
    fn is_closed(&self, class: ClassId) -> bool {
        let closed_origin = self
            .0
            .get(class.0 as usize)
            .is_some_and(|class| match class.origin {
                ClassOrigin::BuiltinValue { .. } | ClassOrigin::HostBacked { .. } => true,
                ClassOrigin::Native => class.sealing.is_sealed() && !class.is_abstract,
                ClassOrigin::ObjectRoot | ClassOrigin::Condition { .. } => false,
            });
        closed_origin && !self.0.iter().any(|other| other.superclass == Some(class))
    }

    fn builtin_class(&self, kind: BuiltinValueClass) -> Option<ClassId> {
        self.0
            .iter()
            .find(|class| class.origin == ClassOrigin::BuiltinValue { kind })
            .map(|class| class.id)
    }

    /// A sealed source class's direct subclasses, in class id order.
    fn sealed_subclasses(&self, class: ClassId) -> Option<Vec<ClassId>> {
        self.0.get(class.0 as usize)?.sealing.is_sealed().then(|| {
            self.0
                .iter()
                .filter(|other| other.superclass == Some(class))
                .map(|other| other.id)
                .collect()
        })
    }

    fn is_abstract(&self, class: ClassId) -> bool {
        self.0
            .get(class.0 as usize)
            .is_some_and(|class| class.is_abstract)
    }

    fn is_same_or_subclass(&self, class: ClassId, ancestor: ClassId) -> bool {
        is_same_or_subclass_in_classes(self.0, class, ancestor)
    }
}

impl ClassHierarchy for Module {
    fn superclass(&self, class: ClassId) -> Option<ClassId> {
        ClassTable(&self.classes).superclass(class)
    }

    fn is_closed(&self, class: ClassId) -> bool {
        ClassTable(&self.classes).is_closed(class)
    }

    fn builtin_class(&self, class: BuiltinValueClass) -> Option<ClassId> {
        ClassTable(&self.classes).builtin_class(class)
    }

    fn sealed_subclasses(&self, class: ClassId) -> Option<Vec<ClassId>> {
        ClassTable(&self.classes).sealed_subclasses(class)
    }

    fn is_abstract(&self, class: ClassId) -> bool {
        ClassTable(&self.classes).is_abstract(class)
    }

    fn is_same_or_subclass(&self, class: ClassId, ancestor: ClassId) -> bool {
        ClassTable(&self.classes).is_same_or_subclass(class, ancestor)
    }
}

impl TypeNames for Module {
    fn write_class(&self, out: &mut dyn core::fmt::Write, class: ClassId) -> core::fmt::Result {
        match self.classes.get(class.0 as usize) {
            Some(class) => out.write_str(&class.name),
            None => write!(out, "class#{}", class.0),
        }
    }

    fn write_generic(
        &self,
        out: &mut dyn core::fmt::Write,
        generic: GenericFunctionId,
    ) -> core::fmt::Result {
        match self.generics.get(generic.0 as usize) {
            Some(generic) => out.write_str(&generic.name),
            None => write!(out, "g{}", generic.0),
        }
    }

    /// Writes `generic(<specializer>, ...)`, with `?` for a missing
    /// specializer.
    fn write_method(&self, out: &mut dyn core::fmt::Write, method: MethodId) -> core::fmt::Result {
        let Some(method) = self.methods.get(method.0 as usize) else {
            return write!(out, "m{}", method.0);
        };
        self.write_generic(out, method.generic)?;
        out.write_str("(")?;
        for (index, specializer) in method.specializers.iter().enumerate() {
            if index > 0 {
                out.write_str(", ")?;
            }
            match specializer {
                Some(class) => self.write_class(out, *class)?,
                None => out.write_str("?")?,
            }
        }
        out.write_str(")")
    }

    /// Writes `<class>.slot-name`.
    fn write_slot(
        &self,
        out: &mut dyn core::fmt::Write,
        class: ClassId,
        slot: SlotId,
    ) -> core::fmt::Result {
        self.write_class(out, class)?;
        match self
            .classes
            .get(class.0 as usize)
            .and_then(|owner| owner.effective_slots.get(slot.0 as usize))
        {
            Some(slot) => write!(out, ".{}", slot.name),
            None => write!(out, ".s{}", slot.0),
        }
    }
}

/// Reports each method a program adds to a built-in collection generic that
/// would override a kernel method ([`GenericFunctionOrigin::admits_method`]).
fn check_collection_method_overrides(module: &mut Module) {
    let mut diagnostics = Vec::new();
    for method in &module.methods {
        let Some(generic) = module.generics.get(method.generic.0 as usize) else {
            continue;
        };
        let index = match generic.origin {
            GenericFunctionOrigin::Collection { function }
                if method.origin != (MethodOrigin::Collection { function }) =>
            {
                function.collection_index()
            }
            GenericFunctionOrigin::BuiltIn { .. } if method.origin != MethodOrigin::BuiltIn => 0,
            _ => continue,
        };
        if generic.origin.admits_method(module, &method.specializers) {
            continue;
        }
        let mut class = String::new();
        if let Some(specializer) = method.specializers.get(index).copied().flatten() {
            let _ = module.write_class(&mut class, specializer);
        }
        let code = if matches!(generic.origin, GenericFunctionOrigin::BuiltIn { .. })
            && module.specializes_on_kernel_classes(&method.specializers)
        {
            DiagnosticCode::BuiltInMethodOnKernelClass
        } else {
            DiagnosticCode::CollectionMethodOverridesKernel
        };
        diagnostics.push(
            Diagnostic::error(method.span, code)
                .with_arg(DiagnosticArg::text("function", generic.name.clone()))
                .with_arg(DiagnosticArg::text("class", class)),
        );
    }
    module.diagnostics.extend(diagnostics);
}

/// Reports each method of a slot's getter generic that would override a
/// slot's getter: one specialized on a class declaring the slot, or on a
/// subclass. Getters are sealed over their slots' classes, so a read of a
/// receiver of known class reads the slot directly
/// ([`GenericFunctionOrigin::SlotGetter`]).
fn check_slot_getter_overrides(module: &mut Module) {
    let mut diagnostics = Vec::new();
    for method in &module.methods {
        if matches!(method.origin, MethodOrigin::SlotGetter { .. }) {
            continue;
        }
        let Some(class) = module.slot_getter_overridden(method.generic, &method.specializers)
        else {
            continue;
        };
        let (mut generic, mut owner) = (String::new(), String::new());
        let _ = module.write_generic(&mut generic, method.generic);
        let _ = module.write_class(&mut owner, class);
        diagnostics.push(
            Diagnostic::error(method.span, DiagnosticCode::MethodOverridesSlotGetter)
                .with_arg(DiagnosticArg::text("generic", generic))
                .with_arg(DiagnosticArg::text("class", owner)),
        );
    }
    module.diagnostics.extend(diagnostics);
}

/// Numbers the check sites of every tree root in `module`; see [`SiteId`].
fn assign_check_sites(module: &mut Module) {
    let Module {
        classes,
        methods,
        functions,
        closures,
        constants,
        ..
    } = module;
    let mut referenced = Vec::new();
    // Where each function's body numbering ended, for its entry checks.
    let mut function_next = Vec::with_capacity(functions.len());
    for function in functions.iter_mut() {
        let mut numbering = SiteNumbering::new(closures, &mut referenced);
        numbering.block(&mut function.body);
        function_next.push(numbering.next);
    }
    for method in methods.iter_mut() {
        SiteNumbering::new(closures, &mut referenced).block(&mut method.body);
    }
    for constant in constants {
        SiteNumbering::new(closures, &mut referenced).expr(&mut constant.value);
    }
    for class in classes {
        let initializers = class
            .direct_slots
            .iter_mut()
            .map(|slot| &mut slot.init_value)
            .chain(
                class
                    .effective_slots
                    .iter_mut()
                    .map(|slot| &mut slot.init_value),
            );
        for value in initializers.flatten() {
            SiteNumbering::new(closures, &mut referenced).expr(value);
        }
        // A default with code of its own numbers its sites as the slot's.
        for initializer in class
            .direct_slots
            .iter_mut()
            .filter_map(|slot| slot.initializer.as_mut())
        {
            SiteNumbering::new(closures, &mut referenced).expr(&mut initializer.value);
        }
    }
    // A function used as a value is also entered indirectly; its entry
    // checks follow its body's sites, so they leave those numbers alone.
    for (function, next) in functions.iter_mut().zip(function_next) {
        if !referenced.contains(&function.id) {
            continue;
        }
        let mut next = next;
        function.entry_checks = entry_checks(&function.params, &function.locals, false, || {
            let site = SiteId(next);
            next = next.saturating_add(1);
            site
        });
    }
}

/// The checks an indirect entry makes on `params`, numbered by `site`; a
/// restart `guard` takes its parameter unboxed.
fn entry_checks(
    params: &[LocalId],
    locals: &[Local],
    guard: bool,
    mut site: impl FnMut() -> SiteId,
) -> Vec<EntryCheck> {
    params
        .iter()
        .filter_map(|param| {
            let local = locals.get(param.0 as usize)?;
            let target = match local.range {
                Some(range) if local.kind == ValueKind::I64 => EntryTarget::Range(range),
                _ if guard => EntryTarget::for_guard_kind(local.kind)?,
                _ => EntryTarget::for_kind(local.kind)?,
            };
            Some(EntryCheck {
                param: *param,
                target,
                site: site(),
            })
        })
        .collect()
}

/// Numbers check sites in evaluation order: operands before the check.
struct SiteNumbering<'a> {
    closures: &'a mut [Closure],
    /// Functions used as values, which are also entered indirectly.
    referenced: &'a mut Vec<FunctionId>,
    next: u32,
}

impl<'a> SiteNumbering<'a> {
    fn new(closures: &'a mut [Closure], referenced: &'a mut Vec<FunctionId>) -> Self {
        Self {
            closures,
            referenced,
            next: 0,
        }
    }

    fn take(&mut self) -> SiteId {
        let site = SiteId(self.next);
        self.next = self.next.saturating_add(1);
        site
    }

    fn block(&mut self, block: &mut Block) {
        for statement in &mut block.statements {
            match statement {
                Statement::Let(binding) => self.expr(&mut binding.value),
                Statement::Expr(expr) => self.expr(expr),
            }
        }
        self.expr(&mut block.result);
    }

    fn exprs(&mut self, exprs: &mut [Expr]) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: &mut Expr) {
        match expr {
            Expr::GenericCall { args, site, .. } => {
                self.exprs(args);
                *site = self.take();
            }
            Expr::Cast { value, site, .. }
            | Expr::InstanceOf { value, site, .. }
            | Expr::InstanceOfLimited { value, site, .. }
            | Expr::Convert { value, site, .. }
            | Expr::CheckRange { value, site, .. } => {
                self.expr(value);
                *site = self.take();
            }
            Expr::Binary {
                kind,
                lhs,
                rhs,
                site,
                ..
            } => {
                self.expr(lhs);
                self.expr(rhs);
                *site = (!kind.is_float()).then(|| self.take());
            }
            Expr::Negate {
                kind, value, site, ..
            } => {
                self.expr(value);
                *site = (!kind.is_float()).then(|| self.take());
            }
            Expr::Closure { closure, .. } => {
                // The body moves out while it is numbered, since nested
                // closures are numbered through the same table.
                let index = closure.0 as usize;
                let Some(body) = self
                    .closures
                    .get_mut(index)
                    .map(|closure| core::mem::replace(&mut closure.body, Block::placeholder()))
                else {
                    return;
                };
                let mut body = body;
                self.block(&mut body);
                // A closure is only entered indirectly; its entry checks
                // follow its body's sites.
                if let Some(closure) = self.closures.get_mut(index) {
                    closure.body = body;
                    let mut next = self.next;
                    let guard = closure.restart_guard;
                    closure.entry_checks =
                        entry_checks(&closure.params, &closure.locals, guard, || {
                            let site = SiteId(next);
                            next = next.saturating_add(1);
                            site
                        });
                    self.next = next;
                }
            }
            Expr::KnownCallable {
                callable: KnownCallable::DirectFunction(function),
                ..
            } => {
                if !self.referenced.contains(function) {
                    self.referenced.push(*function);
                }
            }
            Expr::MakeObject {
                class,
                init_args,
                fields,
                ..
            } => {
                self.expr(class);
                for field in fields.iter_mut() {
                    self.expr(&mut field.value);
                }
                // Explicit arguments are recorded twice; keep the recorded
                // copy identical to the evaluated field.
                for arg in init_args {
                    if let Some(field) = fields.iter().find(|field| {
                        field.source == FieldInitSource::Explicit && field.slot == arg.slot
                    }) {
                        arg.value = field.value.clone();
                    }
                }
            }
            Expr::MakeRuntimeClass {
                class,
                init_args,
                candidates,
                ..
            } => {
                self.expr(class);
                for arg in init_args {
                    self.expr(&mut arg.value);
                }
                for candidate in candidates {
                    for field in &mut candidate.defaults {
                        self.expr(&mut field.value);
                    }
                }
            }
            Expr::VectorLiteral {
                elements: items, ..
            }
            | Expr::Values { values: items, .. }
            | Expr::IntrinsicCall { args: items, .. }
            | Expr::HostCall { args: items, .. }
            | Expr::DirectCall { args: items, .. }
            | Expr::FloatCall { args: items, .. } => self.exprs(items),
            Expr::CallableCall { callable, args, .. } => {
                self.expr(callable);
                self.exprs(args);
            }
            Expr::Signal {
                condition: inner, ..
            }
            | Expr::ClassOf { object: inner, .. }
            | Expr::AccessorCall {
                receiver: inner, ..
            } => self.expr(inner),
            Expr::SlotWrite {
                receiver, value, ..
            } => {
                self.expr(receiver);
                self.expr(value);
            }
            Expr::Compare { lhs, rhs, .. }
            | Expr::MixedCompare { lhs, rhs, .. }
            | Expr::ValueEqual { lhs, rhs, .. }
            | Expr::Identical { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(condition);
                self.expr(then_branch);
                self.expr(else_branch);
            }
            Expr::Discard { value, .. } => self.expr(value),
            Expr::Handle { body, restart, .. } => {
                self.expr(body);
                if let Some(guard) = &mut restart.guard {
                    self.expr(guard);
                }
                self.expr(&mut restart.body);
            }
            Expr::Block { body, cleanup, .. } => {
                self.block(body);
                if let Some(cleanup) = cleanup {
                    self.block(cleanup);
                }
            }
            Expr::AssignLocal { value, .. } | Expr::AssignCapture { value, .. } => self.expr(value),
            Expr::Loop { test, body, .. } => {
                self.expr(test);
                self.block(body);
            }
            Expr::ConstI64 { .. }
            | Expr::ConstF64 { .. }
            | Expr::Const32 { .. }
            | Expr::ConstBool { .. }
            | Expr::ConstChar { .. }
            | Expr::StringLiteral { .. }
            | Expr::SymbolLiteral { .. }
            | Expr::Local { .. }
            | Expr::Capture { .. }
            | Expr::ConstantRef { .. }
            | Expr::SlotDefault { .. }
            | Expr::ClassObject { .. }
            | Expr::TypeObject { .. }
            | Expr::KnownCallable { .. }
            | Expr::GenericFunctionObject { .. }
            | Expr::Error { .. } => {}
        }
    }
}

impl Block {
    /// An empty block standing in for one that is temporarily moved out.
    const fn placeholder() -> Self {
        Self {
            statements: Vec::new(),
            result: Expr::Error {
                span: Span { start: 0, end: 0 },
            },
            span: Span { start: 0, end: 0 },
        }
    }
}

/// Returns whether instances of `class` are unboxed values rather than
/// runtime objects: `<number>` and every numeric class below it,
/// `<boolean>`, and `<function>`.
fn is_unboxed_value_class_in(classes: &[Class], class: ClassId) -> bool {
    matches!(
        classes.get(class.0 as usize).map(|class| class.origin),
        Some(ClassOrigin::BuiltinValue { kind }) if kind.is_unboxed()
    )
}

/// Returns whether generic parameter `index` carries boxed `<object>`
/// values rather than raw runtime-object handles.
///
/// An `<object>` or unannotated parameter is a raw object handle, which
/// keeps dispatch over object classes allocation-free. When some method
/// specializes the parameter on `<object>` or on an unboxed value's class
/// such as `<integer>`, `<float>` or `<function>`, such values can apply,
/// so the parameter carries boxed values instead and they are boxed at the
/// call through the ordinary `<object>` boxing path.
fn generic_param_boxes_scalars_in<'a>(
    classes: &[Class],
    methods: impl IntoIterator<Item = (GenericFunctionId, &'a [MethodSpecializer])>,
    generic: &GenericFunction,
    index: usize,
) -> bool {
    matches!(
        generic.param_kinds.get(index).copied().flatten(),
        None | Some(ValueKind::Any)
    ) && methods
        .into_iter()
        .filter(|(method_generic, _)| *method_generic == generic.id)
        .filter_map(|(_, specializers)| specializers.get(index).copied().flatten())
        .any(|class| class == OBJECT_CLASS_ID || is_unboxed_value_class_in(classes, class))
}

/// Makes `block` produce no values, as a `=> ()` body does in Dylan: its
/// result expression still runs, for its effects, and what it produces is
/// discarded.
fn discard_results(block: &mut Block) {
    if matches!(&block.result, Expr::Values { values, .. } if values.is_empty()) {
        return;
    }
    let span = block.result.span();
    let result = core::mem::replace(
        &mut block.result,
        Expr::Values {
            values: Vec::new(),
            result_kinds: Vec::new(),
            span,
        },
    );
    block.statements.push(Statement::Expr(result));
}

/// Returns the result category of a float builtin call: `<boolean>` for
/// predicates, otherwise the arguments' shared float format.
fn float_call_kind(
    builtin: FloatBuiltin,
    args: &[Expr],
    kind_of: impl Fn(&Expr) -> ValueKind,
) -> ValueKind {
    match builtin.result_kind() {
        ValueKind::F64 if args.first().map(kind_of) == Some(ValueKind::F32) => ValueKind::F32,
        kind => kind,
    }
}

/// Returns whether two result lists hold integers of different types at the
/// same position, which the one-way widening in `kinds_match` would accept
/// but a join cannot.
fn integer_types_differ(lhs: &[ValueKind], rhs: &[ValueKind]) -> bool {
    lhs.iter().zip(rhs).any(|(lhs, rhs)| {
        lhs != rhs
            && NumericKind::of(*lhs).is_some_and(NumericKind::is_integer)
            && NumericKind::of(*rhs).is_some_and(NumericKind::is_integer)
    })
}

/// Returns whether every value of category `actual` is also a value of
/// category `expected`, so `actual` may flow where `expected` is required.
///
/// `class_of` names the nominal class that bounds a category's values, and
/// `is_same_or_subclass` walks the class chain. [`ValueKind::Error`] matches
/// anything, because the erroneous expression has already been reported.
fn kind_holds(
    expected: ValueKind,
    actual: ValueKind,
    is_same_or_subclass: impl Fn(ClassId, ClassId) -> bool,
    class_of: impl Fn(ValueKind) -> Option<ClassId>,
) -> bool {
    matches!(expected, ValueKind::Any)
        || (matches!(expected, ValueKind::Callable)
            && matches!(actual, ValueKind::Callable | ValueKind::KnownCallable(_)))
        || matches!((expected, actual), (ValueKind::Object(expected), ValueKind::Object(actual)) if is_same_or_subclass(actual, expected))
        // A class object names its bound or a subclass of it.
        || matches!((expected, actual), (ValueKind::ClassObject(expected), ValueKind::ClassObject(actual)) if is_same_or_subclass(actual, expected))
        || class_of(actual).is_some_and(|actual| matches!(expected, ValueKind::Object(expected) if is_same_or_subclass(actual, expected)))
        // A `<stretchy-vector>` is a `<vector>`.
        || matches!((expected, actual), (ValueKind::Vector(expected), ValueKind::Object(actual)) if is_same_or_subclass(actual, expected))
        || expected == actual
        // `<i32>` and `<u32>` are subtypes of `<integer>`; they widen
        // exactly.
        || matches!(
            (expected, actual),
            (ValueKind::I64, ValueKind::I32 | ValueKind::U32)
        )
        || matches!(expected, ValueKind::Error)
        || matches!(actual, ValueKind::Error)
}

/// Returns the category of a value that comes from one of two control-flow
/// arms, such as the branches of an `if`: the arm category that holds the
/// other's values, or `None` when neither does.
///
/// Lowering gives the joined expression this category and converts each arm
/// to it, so `holds` must be the same relation semantic checking accepts
/// elsewhere. An erroneous arm makes the join erroneous.
fn join_arm_kind(
    lhs: ValueKind,
    rhs: ValueKind,
    holds: impl Fn(ValueKind, ValueKind) -> bool,
) -> Option<ValueKind> {
    if lhs == rhs {
        return Some(lhs);
    }
    if lhs == ValueKind::Error || rhs == ValueKind::Error {
        return Some(ValueKind::Error);
    }
    // An unboxed number does not become an abstract numeric object such as
    // `<number>`, which has no single representation to convert it to.
    let unboxed = |kind| {
        matches!(
            kind,
            ValueKind::I64 | ValueKind::F64 | ValueKind::I32 | ValueKind::U32 | ValueKind::F32
        )
    };
    if (unboxed(lhs) && matches!(rhs, ValueKind::Object(_)))
        || (unboxed(rhs) && matches!(lhs, ValueKind::Object(_)))
    {
        return None;
    }
    if holds(lhs, rhs) {
        Some(lhs)
    } else if holds(rhs, lhs) {
        Some(rhs)
    } else {
        None
    }
}

/// Joins two result lists position by position with [`join_arm_kind`],
/// and runtime objects neither holds with [`join_object_kinds`]. Lists of
/// different lengths do not join.
fn join_arm_kinds(
    classes: &[Class],
    lhs: &[ValueKind],
    rhs: &[ValueKind],
    holds: impl Fn(ValueKind, ValueKind) -> bool,
) -> Option<Vec<ValueKind>> {
    if lhs.len() != rhs.len() {
        return None;
    }
    lhs.iter()
        .zip(rhs)
        .map(|(lhs, rhs)| join_kind(classes, *lhs, *rhs, &holds))
        .collect()
}

/// Joins two kinds as conditional arms join: the one that holds the other's
/// values ([`join_arm_kind`]), else for two runtime objects their least
/// common superclass ([`join_object_kinds`]).
fn join_kind(
    classes: &[Class],
    lhs: ValueKind,
    rhs: ValueKind,
    holds: impl Fn(ValueKind, ValueKind) -> bool,
) -> Option<ValueKind> {
    join_arm_kind(lhs, rhs, holds).or_else(|| join_object_kinds(classes, lhs, rhs))
}

/// Joins two kinds of runtime objects, such as two sibling classes, at the
/// class the type lattice bounds their union by: their least common
/// superclass, `<object>` at the least. The joined value is still a
/// runtime object handle, so neither arm changes representation.
fn join_object_kinds(classes: &[Class], lhs: ValueKind, rhs: ValueKind) -> Option<ValueKind> {
    let class_of = |kind| match kind {
        ValueKind::Object(class)
        | ValueKind::String(class)
        | ValueKind::Symbol(class)
        | ValueKind::Vector(class) => Some(class),
        _ => None,
    };
    let table = ClassTable(classes);
    let cone = |class| Type::of(Atom::Cone(class), &table);
    let class = cone(class_of(lhs)?)
        .join(&cone(class_of(rhs)?), &table)
        .class_bound(&table)?;
    // A built-in string, symbol or vector class keeps its own kind.
    Some(
        match classes.get(class.0 as usize).map(|class| class.origin) {
            Some(ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::String,
            }) => ValueKind::String(class),
            Some(ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Symbol,
            }) => ValueKind::Symbol(class),
            Some(ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Vector,
            }) => ValueKind::Vector(class),
            _ => ValueKind::Object(class),
        },
    )
}

/// Why a callable's signature cannot stand in for an expected one.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SignatureMismatch {
    /// The arities differ, a pair of kinds is held in different register
    /// types, or a pair is unrelated.
    Incompatible,
    /// A parameter or result would match the other way round: a parameter
    /// narrower than the one expected, or a result wider, either in its
    /// kind or in the signature of a callable it holds.
    Variance {
        /// The parameter or result.
        position: ContractPosition,
        /// Its type in the expected signature, as source names it.
        expected: String,
        /// Its type in the callable's own signature, as source names it.
        actual: String,
    },
}

/// One parameter or result of a callable signature: its kind, and its own
/// signature when it is a typed callable.
#[derive(Copy, Clone)]
struct SignatureValue<'a> {
    kind: ValueKind,
    signature: Option<&'a CallableSignature>,
    range: Option<IntRange>,
}

/// Returns whether values of kinds `lhs` and `rhs` are held in the same
/// tape register type, so a callable signature may name either without
/// conversion: every class and metaobject shares the `object` type, and
/// every callable the closure type.
fn shares_register_type(lhs: ValueKind, rhs: ValueKind) -> bool {
    let object = |kind| {
        matches!(
            kind,
            ValueKind::String(_)
                | ValueKind::Symbol(_)
                | ValueKind::Vector(_)
                | ValueKind::Object(_)
                | ValueKind::ClassObject(_)
                | ValueKind::GenericFunctionObject(_)
                | ValueKind::MethodObject(_)
                | ValueKind::SlotDescriptorObject { .. }
        )
    };
    let callable = |kind| matches!(kind, ValueKind::Callable | ValueKind::KnownCallable(_));
    lhs == rhs
        || matches!(lhs, ValueKind::Error)
        || matches!(rhs, ValueKind::Error)
        || (object(lhs) && object(rhs))
        || (callable(lhs) && callable(rhs))
}

/// Returns the value category of a limited integer type.
const fn limited_integer_kind(limited: LimitedInteger) -> ValueKind {
    match limited {
        LimitedInteger::I32 => ValueKind::I32,
        LimitedInteger::U32 => ValueKind::U32,
    }
}

/// Returns the value category of a 32-bit constant.
const fn number32_kind(value: Number32) -> ValueKind {
    match value {
        Number32::I32(_) => ValueKind::I32,
        Number32::U32(_) => ValueKind::U32,
        Number32::F32(_) => ValueKind::F32,
    }
}

/// Gives an unsuffixed numeric literal the 32-bit type its context expects.
///
/// Literals are untyped until their context says otherwise: an integer
/// literal expected as an `<i32>` or `<u32>` becomes one when its value is a
/// member of that type, and a float literal expected as a `<single-float>`
/// becomes the nearest binary32 value unless that overflows. Anything else
/// is returned unchanged, so the ordinary type check still reports it.
fn adapt_literal(expr: Expr, expected: Option<&[ValueKind]>) -> Expr {
    let Some(&[expected]) = expected else {
        return expr;
    };
    let adapted = match (&expr, expected) {
        (Expr::ConstI64 { value, .. }, ValueKind::I32) => {
            i32::try_from(*value).ok().map(Number32::I32)
        }
        (Expr::ConstI64 { value, .. }, ValueKind::U32) => {
            u32::try_from(*value).ok().map(Number32::U32)
        }
        (Expr::ConstF64 { value, .. }, ValueKind::F32) => {
            // Narrowing rounds to nearest. Literal text is decimal, so this
            // can differ from rounding the text directly to binary32 only in
            // double-rounding corner cases, which an `f32` suffix avoids.
            let single = value.to_f64() as f32;
            single
                .is_finite()
                .then_some(Number32::F32(SingleFloatBits::from_f32(single)))
        }
        _ => None,
    };
    match adapted {
        Some(value) => Expr::Const32 {
            value,
            span: expr.span(),
        },
        None => expr,
    }
}

/// Adapts each literal argument to its parameter's expected kind; see
/// [`adapt_literal`].
fn adapt_literal_args<'a>(
    args: &mut [Expr],
    expected: impl IntoIterator<Item = Option<&'a ValueKind>>,
) {
    for (arg, expected) in args.iter_mut().zip(expected) {
        if let Some(expected) = expected {
            let literal = core::mem::replace(arg, Expr::Error { span: arg.span() });
            *arg = adapt_literal(literal, Some(core::slice::from_ref(expected)));
        }
    }
}

/// Builds the body of a numeric generic's method from the promotion rules,
/// so dispatched and statically typed operators compute the same thing.
fn numeric_method_body(
    operator: promotion::NumericOperator,
    operands: Vec<(NumericKind, Expr)>,
) -> Expr {
    let span = Span::default();
    let mut operands = operands.into_iter();
    let Some((lhs_kind, lhs)) = operands.next() else {
        return Expr::Error { span };
    };
    let Some((rhs_kind, rhs)) = operands.next() else {
        return negate_number(lhs, lhs_kind, span);
    };
    match operator {
        promotion::NumericOperator::Arithmetic(op) => {
            match promotion::arithmetic(op, lhs_kind, rhs_kind) {
                promotion::ArithmeticRule::Promote { kind } => Expr::Binary {
                    site: None,
                    op,
                    kind,
                    lhs: Box::new(promote_operand(lhs, lhs_kind, kind)),
                    rhs: Box::new(promote_operand(rhs, rhs_kind, kind)),
                    span,
                },
                promotion::ArithmeticRule::Undefined => Expr::Error { span },
            }
        }
        promotion::NumericOperator::Compare(op) => {
            compare_numbers(op, (lhs, lhs_kind), (rhs, rhs_kind), span)
        }
        promotion::NumericOperator::Negate => Expr::Error { span },
    }
}

/// Builds `-value` for an operand of kind `kind`, by [`promotion::negation`].
fn negate_number(value: Expr, kind: NumericKind, span: Span) -> Expr {
    let to = promotion::negation(kind);
    Expr::Negate {
        site: None,
        kind: to,
        value: Box::new(promote_operand(value, kind, to)),
        span,
    }
}

/// Builds a comparison of two numbers by [`promotion::comparison`].
fn compare_numbers(
    op: CompareOp,
    (lhs, lhs_kind): (Expr, NumericKind),
    (rhs, rhs_kind): (Expr, NumericKind),
    span: Span,
) -> Expr {
    match promotion::comparison(lhs_kind, rhs_kind) {
        promotion::ComparisonRule::Promote { kind } => Expr::Compare {
            op,
            kind,
            lhs: Box::new(promote_operand(lhs, lhs_kind, kind)),
            rhs: Box::new(promote_operand(rhs, rhs_kind, kind)),
            span,
        },
        promotion::ComparisonRule::ExactMixed => {
            // The float side widens to binary64 exactly; the integer side is
            // already an `<integer>`.
            let widen = |operand, kind: NumericKind| {
                if kind.is_float() {
                    (
                        promote_operand(operand, kind, NumericKind::Float),
                        NumericKind::Float,
                    )
                } else {
                    (operand, kind)
                }
            };
            let (lhs, lhs_kind) = widen(lhs, lhs_kind);
            let (rhs, _) = widen(rhs, rhs_kind);
            Expr::MixedCompare {
                op,
                lhs_kind,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            }
        }
    }
}

/// Converts an operand of numeric kind `from` to the promoted kind `to`.
/// One clause's part of a `for` loop ([`Lowering::lower_for`]).
struct ForPlan {
    /// `#t` while the clause has another value.
    test: Expr,
    /// Hidden statements at the top of each iteration, such as reading the
    /// element, before the variables are bound.
    each: Vec<Statement>,
    /// The hidden local the variable is bound to each iteration.
    value: String,
    /// Whether the value is an `<object>` an annotated variable checks
    /// with `as`.
    checked: bool,
    /// Moves the clause's state on, after the body.
    step: Expr,
    /// The hidden local `step` assigns.
    state: LocalId,
}

impl ForPlan {
    /// A clause that could not be planned, for which a diagnostic was
    /// reported: it ends the loop at once.
    fn error(binding: &ast::LetBinding, span: Span) -> Self {
        Self {
            test: Expr::Error { span },
            each: Vec::new(),
            value: hidden_for_name(binding, "collection"),
            checked: false,
            step: Expr::Error { span },
            state: LocalId(u32::MAX),
        }
    }

    /// The value the clause binds `binding` to, as the body's first `let`
    /// writes it: the hidden local, cast to the variable's annotation when
    /// it is an `<object>` that needs checking.
    fn binding_value(
        &self,
        id: crate::identity::AstNodeId,
        binding: &ast::LetBinding,
    ) -> ast::Expr {
        let name = ast::Expr::Name {
            id,
            name: self.value.clone(),
            span: binding.span,
        };
        match (&binding.type_annotation, self.checked) {
            (Some(target), true) => ast::Expr::Cast {
                id,
                target: target.clone(),
                value: Box::new(name),
                span: binding.span,
            },
            _ => name,
        }
    }
}

/// The name of one of a `for` loop's hidden locals, for the variable
/// `binding`. A `#` cannot appear in a source name, so no source can name
/// it.
fn hidden_for_name(binding: &ast::LetBinding, role: &str) -> String {
    format!("{}#for-{role}", binding.name)
}

/// A read of the hidden local `local`.
const fn hidden_local(local: LocalId, kind: ValueKind, span: Span) -> Expr {
    Expr::Local {
        local,
        result_kind: kind,
        callable_signature: None,
        span,
    }
}

/// The `let` binding the hidden local `local` to `value`.
fn hidden_let(local: LocalId, value: Expr, span: Span) -> Statement {
    Statement::Let(Let {
        local,
        locals: vec![local],
        value,
        span,
    })
}

/// `local := current + by`, for the `<integer>` hidden local `local`.
fn hidden_increment(local: LocalId, current: Expr, by: Expr, span: Span) -> Expr {
    Expr::AssignLocal {
        local,
        result_kind: ValueKind::I64,
        callable_signature: None,
        value: Box::new(Expr::Binary {
            site: None,
            op: BinaryOp::Add,
            kind: NumericKind::Integer,
            lhs: Box::new(current),
            rhs: Box::new(by),
            span,
        }),
        span,
    }
}

fn promote_operand(operand: Expr, from: NumericKind, to: NumericKind) -> Expr {
    if from == to || matches!(operand, Expr::Error { .. }) {
        return operand;
    }
    let span = operand.span();
    Expr::Convert {
        site: SiteId::PENDING,
        to,
        value: Box::new(operand),
        explicit: false,
        span,
    }
}

fn is_same_or_subclass_in_classes(classes: &[Class], actual: ClassId, expected: ClassId) -> bool {
    inheritance_distance_in_classes(classes, actual, expected).is_some()
}

fn inheritance_distance_in_classes(
    classes: &[Class],
    actual: ClassId,
    expected: ClassId,
) -> Option<usize> {
    let mut current = Some(actual);
    let mut distance = 0;
    let mut visited = 0;
    while let Some(class) = current {
        if class == expected {
            return Some(distance);
        }
        visited += 1;
        if visited > classes.len() {
            return None;
        }
        current = classes.get(class.0 as usize)?.superclass;
        distance += 1;
    }
    None
}

fn class_depth_in_classes(classes: &[Class], class: ClassId) -> usize {
    let mut current = classes
        .get(class.0 as usize)
        .and_then(|class| class.superclass);
    let mut depth = 0;
    let mut visited = 0;
    while let Some(parent) = current {
        depth += 1;
        visited += 1;
        if visited > classes.len() {
            break;
        }
        current = classes
            .get(parent.0 as usize)
            .and_then(|class| class.superclass);
    }
    depth
}

fn superclass_chain_in_classes(classes: &[Class], class: ClassId) -> Vec<ClassId> {
    let mut chain = Vec::new();
    let mut current = Some(class);
    let mut visited = 0;
    while let Some(class) = current {
        chain.push(class);
        visited += 1;
        if visited > classes.len() {
            break;
        }
        current = classes
            .get(class.0 as usize)
            .and_then(|class| class.superclass);
    }
    chain
}

fn method_applies_to_classes_in_classes(
    classes: &[Class],
    method: &Method,
    actuals: &[ClassId],
) -> bool {
    method.specializers.len() == actuals.len()
        && method
            .specializers
            .iter()
            .zip(actuals)
            .all(|(specializer, actual)| {
                specializer.is_none_or(|expected| {
                    is_same_or_subclass_in_classes(classes, *actual, expected)
                })
            })
}

fn method_more_specific_in_classes(classes: &[Class], left: &Method, right: &Method) -> bool {
    if left.specializers.len() != right.specializers.len() {
        return false;
    }
    let mut stricter = false;
    for (left, right) in left.specializers.iter().zip(&right.specializers) {
        match (*left, *right) {
            (Some(left), Some(right)) if left == right => {}
            (Some(left), Some(right)) if is_same_or_subclass_in_classes(classes, left, right) => {
                stricter = true;
            }
            (Some(_), None) => {
                stricter = true;
            }
            (None, None) => {}
            _ => return false,
        }
    }
    stricter
}

fn select_method_for_classes_in_methods(
    classes: &[Class],
    methods: &[Method],
    generic: GenericFunctionId,
    actuals: &[ClassId],
) -> MethodSelection {
    let applicable = methods
        .iter()
        .filter(|method| {
            method.generic == generic
                && method_applies_to_classes_in_classes(classes, method, actuals)
        })
        .collect::<Vec<_>>();
    if applicable.is_empty() {
        return MethodSelection::None;
    }
    let maximal = applicable
        .iter()
        .copied()
        .filter(|candidate| {
            !applicable.iter().copied().any(|other| {
                other.id != candidate.id
                    && method_more_specific_in_classes(classes, other, candidate)
            })
        })
        .collect::<Vec<_>>();
    match maximal.as_slice() {
        [method] => MethodSelection::Selected(method.id),
        _ => MethodSelection::Ambiguous,
    }
}

/// Nominal class definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Class {
    /// Class identifier.
    pub id: ClassId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Source name.
    pub name: String,
    /// Native or host-backed class origin.
    pub origin: ClassOrigin,
    /// Resolved direct superclass, or `None` for the `<object>` root/default.
    pub superclass: Option<ClassId>,
    /// Whether the class's direct subclasses are exactly those its source
    /// defines. Only source classes can be sealed; kernel and host classes
    /// are [`Sealing::Open`], and those that are closed are closed by
    /// origin instead ([`ClassHierarchy::is_closed`]).
    pub sealing: Sealing,
    /// Whether the class has no direct instances, so `make` cannot
    /// construct it.
    pub is_abstract: bool,
    /// Whether effective slot finalization completed for this class.
    pub finalized: bool,
    /// Direct source slot definitions.
    pub direct_slots: Vec<Slot>,
    /// Computed effective slots in deterministic instance-layout order.
    pub effective_slots: Vec<EffectiveSlot>,
    /// Computed initialization plan derived from finalized effective slots.
    pub initialization_plan: InitializationPlan,
    /// Source span.
    pub span: Span,
}

/// Slot definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slot {
    /// Slot identifier within the owning class.
    pub id: SlotId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Slot name.
    pub name: String,
    /// Declared source type as source spells it, such as `<circle>`,
    /// `class <shape>` or `callable(<integer>) => <integer>`, with missing
    /// declarations normalized to `<object>`.
    pub type_name: String,
    /// Declared slot value category.
    pub kind: ValueKind,
    /// Declared signature when the slot holds a typed callable, which
    /// reads of the slot are called with and writes must satisfy.
    pub callable_signature: Option<CallableSignature>,
    /// Declared range when the slot holds a `limited(<integer>, ...)`,
    /// which every write checks and reads may assume.
    pub range: Option<IntRange>,
    /// Native or host-backed slot origin.
    pub origin: SlotOrigin,
    /// Getter accessor defined by this slot.
    pub accessor: Accessor,
    /// External construction keyword without the trailing colon.
    pub init_keyword: String,
    /// Whether the slot declared `required-init-keyword:`.
    pub required_init_keyword: bool,
    /// Optional default initializer used when construction omits this slot:
    /// what each `make` that takes the default evaluates. A default that is
    /// a literal is the literal; any other is an [`Expr::SlotDefault`] that
    /// runs [`Self::initializer`].
    pub init_value: Option<Expr>,
    /// The default's own code, when it is more than a literal: a frame of
    /// its own, evaluated afresh at each `make` that takes it.
    pub initializer: Option<SlotInitializer>,
    /// Allocation policy.
    pub allocation: SlotAllocation,
    /// Mutability metadata.
    pub mutability: SlotMutability,
    /// Source span.
    pub span: Span,
}

/// A slot default's code, run by each `make` that takes the default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotInitializer {
    /// The default's value expression.
    pub value: Expr,
    /// Its own locals, indexed by [`LocalId`]: those its `let`s, loops,
    /// `block` exits and handlers bind.
    pub locals: Vec<Local>,
}

/// Computed slot definition after class finalization.
///
/// Direct [`Slot`] values are source facts. Effective slots are computed MOP
/// facts: inherited parent slots come first, followed by the class's own direct
/// slots. The `id` and `accessor.slot` fields are class-local layout positions;
/// `definition`, `source_class`, `source_slot`, and `accessor.definition`
/// preserve the original source slot/accessor identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveSlot {
    /// Layout slot identifier within the finalized class.
    pub id: SlotId,
    /// Stable identity of the original direct slot definition.
    pub definition: DefinitionId,
    /// Slot name.
    pub name: String,
    /// Declared source type as source spells it; see [`Slot::type_name`].
    pub type_name: String,
    /// Slot value category.
    pub kind: ValueKind,
    /// Declared signature when the slot holds a typed callable; see
    /// [`Slot::callable_signature`].
    pub callable_signature: Option<CallableSignature>,
    /// Declared range of a limited integer slot; see [`Slot::range`].
    pub range: Option<IntRange>,
    /// Native or host-backed slot origin.
    pub origin: SlotOrigin,
    /// Class that originally introduced this slot.
    pub source_class: ClassId,
    /// Direct slot id on `source_class`.
    pub source_slot: SlotId,
    /// Getter accessor for this effective slot on the finalized class.
    pub accessor: Accessor,
    /// External construction keyword without the trailing colon.
    pub init_keyword: String,
    /// Whether the original slot declared `required-init-keyword:`.
    pub required_init_keyword: bool,
    /// Optional default initializer used when construction omits this slot.
    pub init_value: Option<Expr>,
    /// Allocation policy.
    pub allocation: SlotAllocation,
    /// Mutability metadata.
    pub mutability: SlotMutability,
    /// Source span of the original direct slot.
    pub span: Span,
}

/// Computed class initialization plan.
///
/// This is a MOP fact derived from finalized effective slots. It describes the
/// slot initialization order and accepted construction keywords. User-defined
/// `initialize` methods are intentionally not part of this record yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializationPlan {
    /// Class initialized by this plan.
    pub class: ClassId,
    /// Slot initialization steps in deterministic layout order.
    pub steps: Vec<InitializationStep>,
}

impl Default for InitializationPlan {
    fn default() -> Self {
        Self {
            class: ClassId(u32::MAX),
            steps: Vec::new(),
        }
    }
}

/// One slot step in a class initialization plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitializationStep {
    /// Slot filled by this step.
    pub slot: SlotId,
    /// Class that originally introduced this slot.
    pub source_class: ClassId,
    /// Direct slot id on `source_class`.
    pub source_slot: SlotId,
    /// External construction keyword without the trailing colon.
    pub init_keyword: String,
    /// Slot value category.
    pub kind: ValueKind,
    /// Declared source type name.
    pub type_name: String,
    /// Whether this step came from a parent effective slot.
    pub inherited: bool,
    /// Whether an explicit initializer is required when no default exists.
    pub required_init_keyword: bool,
    /// Whether this step has a default `init-value:`.
    pub has_default: bool,
}

/// Getter accessor defined by a slot declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Accessor {
    /// Stable accessor identity. This is the slot definition identity.
    pub definition: DefinitionId,
    /// Accessor call name.
    pub name: String,
    /// Owning class.
    pub class: ClassId,
    /// Slot read by this accessor.
    pub slot: SlotId,
    /// Accessor result category.
    pub result_kind: ValueKind,
}

/// Generic function declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenericFunction {
    /// Generic function identifier.
    pub id: GenericFunctionId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Source name.
    pub name: String,
    /// Parameter names in source order.
    pub params: Vec<String>,
    /// Explicit parameter contracts in source order.
    pub param_kinds: Vec<GenericParamKind>,
    /// Ranges of `limited(<integer>, ...)` parameters, parallel to
    /// `param_kinds`; `None` where a parameter is not one. Every send checks
    /// its argument against the range, so methods may assume it.
    pub param_ranges: Vec<Option<IntRange>>,
    /// Arity.
    pub arity: usize,
    /// Zero-based parameter index used for nominal method dispatch.
    pub dispatch_param: usize,
    /// Zero-based parameter indexes considered by generic dispatch.
    pub dispatch_params: Vec<usize>,
    /// Result value categories in return order.
    pub result_kinds: Vec<ValueKind>,
    /// Typed callable signatures for callable results, when declared.
    pub result_callable_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` results, parallel to
    /// `result_kinds`; every method checks its result against them.
    pub result_ranges: Vec<Option<IntRange>>,
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Source span.
    pub span: Span,
    /// Source or synthesized origin.
    pub origin: GenericFunctionOrigin,
    /// Whether the generic's methods are exactly those its source defines,
    /// so no session mutation or installed code may add one. Built-in
    /// generics are [`Sealing::Open`]; what they admit is
    /// [`GenericFunctionOrigin::admits_method`]'s.
    pub sealing: Sealing,
    /// `define strict generic`: every method of the generic is held to
    /// strict typing. Built-in generics are never strict.
    pub strict: bool,
    /// The `#key` parameters, the last of `params`, in declaration order;
    /// dispatch looks only at the parameters before them.
    pub keywords: Vec<KeywordParam>,
}

/// Origin of a semantic method metaobject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MethodOrigin {
    /// Method declared directly in Loko source.
    Source,
    /// Compiler-synthesized method for a writable native slot setter.
    SlotSetter {
        /// Class that introduced the writable slot.
        class: ClassId,
        /// Effective slot written by the setter method.
        slot: SlotId,
    },
    /// Compiler-generated method of a built-in numeric generic, from the
    /// rules in [`crate::promotion`].
    Numeric {
        /// Operator the method implements.
        operator: promotion::NumericOperator,
    },
    /// Kernel method of a built-in generic a built-in function with
    /// several overloads is: one overload's call of its parameters.
    BuiltIn,
    /// Compiler-synthesized getter method of a slot: a read of the slot.
    SlotGetter {
        /// Class that declared the slot.
        class: ClassId,
        /// Effective slot the method reads.
        slot: SlotId,
    },
    /// Kernel method of a built-in collection generic, from
    /// [`BUILTIN_METHODS`]. Its body is the row's operation on its
    /// parameters.
    Collection {
        /// The generic.
        function: CollectionFunction,
    },
    /// Method inserted into the running session by an EOP mutation, identified
    /// by a per-session monotonic mutation sequence number. The sequence is
    /// stable for the lifetime of the session and lets workbench records
    /// distinguish multiple mutations without relying on wall-clock time.
    Session {
        /// Per-session mutation sequence number assigned when the method was
        /// inserted.
        sequence: u32,
    },
}

/// Method definition specialized on one generic parameter vector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Method {
    /// Method identifier.
    pub id: MethodId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Implemented generic function.
    pub generic: GenericFunctionId,
    /// Specialized class for the first argument, retained as a compatibility
    /// mirror for older inspection paths.
    pub specializer: ClassId,
    /// Specialized classes in call order. Source `<object>` is represented by
    /// [`OBJECT_CLASS_ID`]; `None` is reserved for invalid compatibility
    /// records.
    pub specializers: Vec<MethodSpecializer>,
    /// Dispatch parameter local, retained as a compatibility mirror for older
    /// single-dispatch paths.
    pub param: LocalId,
    /// Parameter local IDs in call order.
    pub params: Vec<LocalId>,
    /// Local table.
    pub locals: Vec<Local>,
    /// Method body.
    pub body: Block,
    /// Result value categories in return order.
    pub result_kinds: Vec<ValueKind>,
    /// Ranges the body checks its results against, parallel to
    /// `result_kinds`: the method's own `limited(<integer>, ...)` result
    /// types, or else its generic's.
    pub result_ranges: Vec<Option<IntRange>>,
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Source span.
    pub span: Span,
    /// Source or synthesized origin.
    pub origin: MethodOrigin,
    /// `define strict method`: typing must prove the checks the compiler
    /// inserts into the body (see [`crate::typing`]). A method of a strict
    /// generic is held to strict typing too; see
    /// [`crate::typing::strictness`].
    pub strict: bool,
}

/// Semantic function body and local table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Function identifier.
    pub id: FunctionId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Function name.
    pub name: String,
    /// Parameter local IDs in source order.
    pub params: Vec<LocalId>,
    /// Resolved parameter value categories in source order.
    pub param_kinds: Vec<ValueKind>,
    /// Typed callable signatures for callable parameters, when declared.
    pub param_callable_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` parameters, parallel to
    /// `param_kinds`. Every direct call checks its argument against the
    /// range, and an indirect entry checks what it is passed, so the body
    /// may assume it.
    pub param_ranges: Vec<Option<IntRange>>,
    /// Local table containing parameters followed by `let` bindings.
    pub locals: Vec<Local>,
    /// Function body.
    pub body: Block,
    /// Result value categories in return order.
    pub result_kinds: Vec<ValueKind>,
    /// Typed callable signatures for callable results, when declared.
    pub result_callable_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` results, parallel to
    /// `result_kinds`; the body checks its results against them, so
    /// callers may assume them.
    pub result_ranges: Vec<Option<IntRange>>,
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Checks the function's indirect entry makes, when the function is
    /// used as a value; empty otherwise. Direct calls trust its contract.
    pub entry_checks: Vec<EntryCheck>,
    /// The `#key` parameters, the last of `params`, in declaration order.
    pub keywords: Vec<KeywordParam>,
    /// Whether the last of `params` is Dylan's `#rest` parameter, a
    /// `<vector>` of the arguments after the required ones. A direct call
    /// makes the vector; a call through the function's value passes its
    /// boxed entry every argument in one.
    pub rest: bool,
    /// `define strict function`: typing must prove the checks the compiler
    /// inserts into the body (see [`crate::typing`]).
    pub strict: bool,
    /// Whether the kernel made the function, the value a built-in such as
    /// `digit?` is, rather than the program defining it.
    pub kernel: bool,
    /// Source span.
    pub span: Span,
}

/// One `#key` parameter of a function or generic, which follows its
/// required parameters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeywordParam {
    /// The keyword, without its colon.
    pub name: String,
    /// What a call that does not pass the keyword gets: the result of the
    /// owner's default function for it, or `$unsupplied`.
    pub default: Option<FunctionId>,
}

/// A check a callable's indirect entry makes on one parameter before its
/// body relies on the parameter's declared type.
///
/// A callable value has two entries. Its typed entry takes the arguments
/// at its own signature's register types; static typing, and the adapter
/// an untyped callable gets where it enters typed code, let a caller of a
/// known signature call it directly, and an untyped caller calls it when
/// its own call has the same shape. That entry checks what a register type
/// does not establish: an instance of a class, a class object naming it or
/// a subclass, or an `<integer>` in a `limited(<integer>, ...)` range.
/// Every other untyped call goes through the boxed entry, under the boxed
/// callable ABI, where each argument arrives as a boxed `<object>`, so it
/// also checks a number or boolean of the parameter's kind
/// ([`EntryTarget::Unbox`]); lowering unboxes a callable parameter's
/// `<function>` itself. Only an `<object>` parameter takes the box as it
/// is.
///
/// Each check is a site of the definition whose sites number its body,
/// after the body's own, so soft typing classifies it like any cast.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EntryCheck {
    /// The checked parameter.
    pub param: LocalId,
    /// What the parameter's value must be.
    pub target: EntryTarget,
    /// The check site.
    pub site: SiteId,
}

/// What an [`EntryCheck`] establishes about its parameter.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EntryTarget {
    /// An instance of the class or a subclass.
    Instance(ClassId),
    /// A class object naming the class or a subclass.
    ClassObject(ClassId),
    /// An `<integer>` in the range.
    Range(IntRange),
    /// A number, boolean or character of the kind: an `<integer>`,
    /// `<i32>`, `<u32>`, `<double-float>`, `<single-float>`, `<boolean>` or
    /// `<character>`, unboxed from the boxed `<object>` an untyped caller
    /// passed to the boxed entry; the typed entry's register type
    /// establishes it, except a character's, which only the unboxing does.
    /// An `<i32>` or `<u32>` is an `<integer>` in its range.
    Unbox(ValueKind),
}

impl EntryTarget {
    /// The check a parameter of `kind` needs at an indirect entry, if any; a
    /// limited integer's is [`Self::Range`] instead.
    #[must_use]
    pub fn for_kind(kind: ValueKind) -> Option<Self> {
        match kind {
            ValueKind::Object(class)
            | ValueKind::String(class)
            | ValueKind::Symbol(class)
            | ValueKind::Vector(class)
                if class != OBJECT_CLASS_ID =>
            {
                Some(Self::Instance(class))
            }
            ValueKind::ClassObject(class) => Some(Self::ClassObject(class)),
            ValueKind::I64
            | ValueKind::F64
            | ValueKind::I32
            | ValueKind::U32
            | ValueKind::F32
            | ValueKind::Bool
            | ValueKind::Char
            | ValueKind::Position => Some(Self::Unbox(kind)),
            _ => None,
        }
    }

    /// The check a restart guard's parameter needs: the handler search
    /// calls a guard with the condition in its own register type, so only
    /// a class is checked.
    #[must_use]
    pub fn for_guard_kind(kind: ValueKind) -> Option<Self> {
        match Self::for_kind(kind) {
            Some(Self::Unbox(_)) => None,
            target => target,
        }
    }
}

/// Semantic anonymous function body and closure capture table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Closure {
    /// Closure identifier.
    pub id: ClosureId,
    /// Parameter local IDs in source order.
    pub params: Vec<LocalId>,
    /// Resolved parameter value categories in source order.
    pub param_kinds: Vec<ValueKind>,
    /// Typed callable signatures for callable parameters, when declared.
    pub param_callable_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` parameters, parallel to
    /// `param_kinds`; the closure's entry checks them.
    pub param_ranges: Vec<Option<IntRange>>,
    /// Captures read from the closure environment.
    pub captures: Vec<Capture>,
    /// Local table containing parameters followed by `let` bindings.
    pub locals: Vec<Local>,
    /// Closure body.
    pub body: Block,
    /// Result value categories in return order.
    pub result_kinds: Vec<ValueKind>,
    /// Typed callable signatures for callable results, when declared.
    pub result_callable_sigs: Vec<Option<CallableSignature>>,
    /// Ranges of `limited(<integer>, ...)` results, parallel to
    /// `result_kinds`; the body checks its results against them.
    pub result_ranges: Vec<Option<IntRange>>,
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Checks the closure's entry makes; a closure is only ever entered
    /// indirectly. Its sites belong to the body that creates it.
    pub entry_checks: Vec<EntryCheck>,
    /// Whether the last of `params` is Dylan's `#rest` parameter, a
    /// `<vector>` of the arguments after the required ones.
    pub rest: bool,
    /// Whether this is a `handle` clause's restart guard, which the handler
    /// search calls with the condition at the clause's class and which never
    /// becomes a callable value. Every other closure is a callable value,
    /// with a typed entry and a boxed one ([`EntryCheck`]).
    pub restart_guard: bool,
    /// The callable signature expected where the closure is written, when
    /// it took types from it; see [`ExpectedSignature`].
    pub expected: Option<ExpectedSignature>,
    /// Source span.
    pub span: Span,
}

/// The callable signature expected where an anonymous method is written,
/// and what the method takes from it.
///
/// A method passed as an argument for a typed callable parameter or
/// keyword, given as a typed callable slot's value, bound to a typed
/// callable `let` or constant, assigned to a typed callable binding, or
/// returned as a typed callable result is typed bidirectionally: each
/// parameter it writes no type for takes the expected parameter's, and if
/// it writes no result types its results are the expected ones, so a body
/// returning more values keeps the first. A type it does write stays, and
/// one that does not fit the expected signature is an error. A method whose
/// arity differs from the signature's, or that is expected to take
/// keywords, takes nothing from it.
///
/// What a method takes is checked as if written: its entry checks the
/// parameters ([`EntryCheck`]) and its body the results, so typing may rely
/// on them, citing the signature
/// ([`crate::typing::ReasonKind::Expected`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedSignature {
    /// The signature expected where the method is written.
    pub signature: CallableSignature,
    /// Whether each parameter, in order, takes its type from
    /// [`Self::signature`] because it writes none.
    pub params: Vec<bool>,
    /// Whether the results are [`Self::signature`]'s because the method
    /// writes no result types.
    pub results: bool,
}

impl ExpectedSignature {
    /// Whether anything is taken from the signature.
    #[must_use]
    pub fn takes_anything(&self) -> bool {
        self.results || self.params.iter().any(|taken| *taken)
    }
}

/// One source local captured by an anonymous function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capture {
    /// Capture identifier within the closure.
    pub id: CaptureId,
    /// Local in the enclosing function or closure context.
    pub source: LocalId,
    /// Captured local name.
    pub name: String,
    /// Captured value category.
    pub kind: ValueKind,
    /// Typed callable signature when the capture is a typed callable value.
    pub callable_signature: Option<CallableSignature>,
    /// The captured local's declared range; see [`Local::range`].
    pub range: Option<IntRange>,
    /// Whether the captured local is [shared](Local::shared): the capture
    /// holds its cell, which the closure reads and may assign, rather than
    /// a copy of its value.
    pub shared: bool,
    /// Source span of the captured binding.
    pub span: Span,
}

/// Immutable top-level constant value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Constant {
    /// Constant identifier.
    pub id: ConstantId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Constant name.
    pub name: String,
    /// Lowered initializer expression.
    pub value: Expr,
    /// The initializer's own locals, indexed by [`LocalId`]: those its
    /// `let`s, `block` exits and handlers bind. The initializer is a frame
    /// of its own, as a function body is, wherever it is evaluated.
    pub locals: Vec<Local>,
    /// Result value category.
    pub result_kind: ValueKind,
    /// Typed callable signature when the constant is a typed callable value.
    pub callable_signature: Option<CallableSignature>,
    /// `define strict constant`: typing must prove the checks the compiler
    /// inserts into the initializer (see [`crate::typing`]).
    pub strict: bool,
    /// Source span.
    pub span: Span,
}

/// Semantic local binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Local {
    /// Local identifier.
    pub id: LocalId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Local name.
    pub name: String,
    /// Value category.
    pub kind: ValueKind,
    /// Typed callable signature when this local is a typed callable value.
    pub callable_signature: Option<CallableSignature>,
    /// Declared range when this local is a `limited(<integer>, ...)`: every
    /// value bound to it was checked against the range, so reads may assume
    /// it. For a parameter, callers check their arguments, and an indirect
    /// entry checks what it is passed.
    pub range: Option<IntRange>,
    /// Whether some `:=` assigns this local after its binding. An assigned
    /// local holds different values over its lifetime; only a `let` local
    /// or a parameter may be assigned.
    pub assigned: bool,
    /// Whether this `let` local or parameter lives in a shared cell: one
    /// that is assigned and that a closure refers to, so the frame and
    /// every closure capturing it read and write the one cell. A parameter
    /// moves into its cell on entry. Decided by name
    /// for the whole definition ([`ast::shared_local_names`]).
    pub shared: bool,
    /// Source span of the binding name.
    pub span: Span,
}

/// Semantic function or method body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// `let` bindings and expressions evaluated for effect, in order.
    pub statements: Vec<Statement>,
    /// Final result expression.
    pub result: Expr,
    /// Source span.
    pub span: Span,
}

impl Block {
    /// The body's `let` bindings, in order.
    pub fn lets(&self) -> impl Iterator<Item = &Let> {
        self.statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::Let(binding) => Some(binding),
                Statement::Expr(_) => None,
            })
    }

    /// Visits the expressions this body evaluates directly, in order: each
    /// statement's, then the result.
    pub fn for_each_expr<'e>(&'e self, mut visit: impl FnMut(&'e Expr)) {
        for statement in &self.statements {
            match statement {
                Statement::Let(binding) => visit(&binding.value),
                Statement::Expr(expr) => visit(expr),
            }
        }
        visit(&self.result);
    }

    /// The locals an assignment in this body assigns; see
    /// [`Expr::assigned_locals`].
    #[must_use]
    pub fn assigned_locals(&self) -> Vec<LocalId> {
        let mut out = Vec::new();
        self.for_each_expr(|expr| expr.collect_assigned(&mut out));
        out
    }
}

/// One statement of a semantic body, before its result expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Statement {
    /// A `let` binding.
    Let(Let),
    /// An expression evaluated for its effects; its values are discarded.
    Expr(Expr),
}

/// When a conditional loop runs its body.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoopKind {
    /// `while`: the body runs while the test is `#t`.
    While,
    /// `until`: the body runs until the test is `#t`.
    Until,
}

impl LoopKind {
    /// The test outcome that runs the body once more.
    #[must_use]
    pub const fn continues_on(self) -> bool {
        matches!(self, Self::While)
    }
}

impl From<ast::LoopKind> for LoopKind {
    fn from(value: ast::LoopKind) -> Self {
        match value {
            ast::LoopKind::While => Self::While,
            ast::LoopKind::Until => Self::Until,
        }
    }
}

/// Semantic `let` binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Let {
    /// Newly introduced local.
    pub local: LocalId,
    /// Newly introduced locals in binding order.
    pub locals: Vec<LocalId>,
    /// Bound expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Local true-branch refinement produced by an `instance?` condition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Refinement {
    /// Refined local.
    pub local: LocalId,
    /// Narrowed local kind in the true branch: an object class, or the
    /// unboxed scalar kind of a boxed `<object>` that passed a numeric test.
    pub kind: ValueKind,
}

/// Object field initializer resolved to a slot ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldInit {
    /// Resolved slot.
    pub slot: SlotId,
    /// Field value.
    pub value: Expr,
    /// Source of this field value.
    pub source: FieldInitSource,
    /// Source span.
    pub span: Span,
}

/// Source for one planned object field value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FieldInitSource {
    /// The value came from an explicit `make` keyword argument.
    Explicit,
    /// The value came from the slot's default `init-value:`.
    Default,
}

/// One class a [`Expr::MakeRuntimeClass`] may allocate, with its plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MakeCandidate {
    /// Candidate class: the bound or one of its native subclasses.
    pub class: ClassId,
    /// Default initializers, in initialization-plan order, for the slots no
    /// explicit init argument fills.
    pub defaults: Vec<FieldInit>,
    /// The first init keyword this class requires that the call does not
    /// pass. Such a class cannot be made here, so choosing it traps.
    pub missing_keyword: Option<String>,
}

/// Explicit keyword argument supplied to `make`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitArg {
    /// Keyword without the trailing colon.
    pub keyword: String,
    /// Resolved slot receiving this argument.
    pub slot: SlotId,
    /// Argument value.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Semantic binary operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Division. Defined for floats only; integer division has no `/`.
    Div,
    /// `+%`: addition modulo 2^32, defined only on two operands of one
    /// fixed-width type.
    WrappingAdd,
    /// `-%`: subtraction modulo 2^32, like [`Self::WrappingAdd`].
    WrappingSub,
    /// `*%`: multiplication modulo 2^32, like [`Self::WrappingAdd`].
    WrappingMul,
    /// `x ^ n`: exponentiation. Exact on integers, for a non-negative
    /// exponent; floats compute `pow`.
    Pow,
    /// `logand(x, y)`: bitwise and, exact on integers of any type.
    LogAnd,
    /// `logior(x, y)`: bitwise inclusive or, exact on integers of any type.
    LogIor,
    /// `logxor(x, y)`: bitwise exclusive or, exact on integers of any type.
    LogXor,
    /// `shift-left(x, n)`: shifts a fixed-width `x` left by `n` in 0..=31,
    /// discarding the bits shifted out.
    ShiftLeft,
    /// `shift-right(x, n)`: shifts a fixed-width `x` right by `n` in
    /// 0..=31: arithmetic for `<i32>`, logical for `<u32>`, so both round
    /// toward negative infinity.
    ShiftRight,
}

impl BinaryOp {
    /// Returns whether this is an explicit wrapping (modular) operator.
    #[must_use]
    pub const fn is_wrapping(self) -> bool {
        matches!(
            self,
            Self::WrappingAdd | Self::WrappingSub | Self::WrappingMul
        )
    }

    /// Returns whether this is a bitwise function on integers.
    #[must_use]
    pub const fn is_bitwise(self) -> bool {
        matches!(
            self,
            Self::LogAnd | Self::LogIor | Self::LogXor | Self::ShiftLeft | Self::ShiftRight
        )
    }

    /// Returns whether this is a bit shift.
    #[must_use]
    pub const fn is_shift(self) -> bool {
        matches!(self, Self::ShiftLeft | Self::ShiftRight)
    }

    /// Returns the bitwise function called `name`, if any. Like the float
    /// library, these resolve after every user definition of the name.
    #[must_use]
    pub fn bitwise_function(name: &str) -> Option<Self> {
        Some(match name {
            "logand" => Self::LogAnd,
            "logior" => Self::LogIor,
            "logxor" => Self::LogXor,
            "shift-left" => Self::ShiftLeft,
            "shift-right" => Self::ShiftRight,
            _ => return None,
        })
    }

    /// Returns the operator's source spelling.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::WrappingAdd => "+%",
            Self::WrappingSub => "-%",
            Self::WrappingMul => "*%",
            Self::Pow => "^",
            Self::LogAnd => "logand",
            Self::LogIor => "logior",
            Self::LogXor => "logxor",
            Self::ShiftLeft => "shift-left",
            Self::ShiftRight => "shift-right",
        }
    }
}

impl From<ast::BinaryOp> for BinaryOp {
    fn from(value: ast::BinaryOp) -> Self {
        match value {
            ast::BinaryOp::Add => Self::Add,
            ast::BinaryOp::Sub => Self::Sub,
            ast::BinaryOp::Mul => Self::Mul,
            ast::BinaryOp::Div => Self::Div,
            ast::BinaryOp::WrappingAdd => Self::WrappingAdd,
            ast::BinaryOp::WrappingSub => Self::WrappingSub,
            ast::BinaryOp::WrappingMul => Self::WrappingMul,
            ast::BinaryOp::Pow => Self::Pow,
        }
    }
}

/// Unboxed numeric category of a primitive arithmetic or ordering operation.
///
/// Mixed operands are promoted by the rules in [`crate::promotion`] before
/// they reach a primitive operation, so both operands of an [`Expr::Binary`]
/// or [`Expr::Compare`] share one kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NumericKind {
    /// Signed 64-bit integers, source `<integer>`.
    Integer,
    /// IEEE 754 binary64 floats, source `<double-float>`.
    Float,
    /// `<i32>` integers.
    I32,
    /// `<u32>` integers.
    U32,
    /// IEEE 754 binary32 floats, source `<single-float>`.
    SingleFloat,
}

impl NumericKind {
    /// Returns the semantic value category of this numeric kind.
    #[must_use]
    pub const fn value_kind(self) -> ValueKind {
        match self {
            Self::Integer => ValueKind::I64,
            Self::Float => ValueKind::F64,
            Self::I32 => ValueKind::I32,
            Self::U32 => ValueKind::U32,
            Self::SingleFloat => ValueKind::F32,
        }
    }

    /// Returns the numeric kind of `kind`, when it is numeric.
    #[must_use]
    pub const fn of(kind: ValueKind) -> Option<Self> {
        match kind {
            ValueKind::I64 => Some(Self::Integer),
            ValueKind::F64 => Some(Self::Float),
            ValueKind::I32 => Some(Self::I32),
            ValueKind::U32 => Some(Self::U32),
            ValueKind::F32 => Some(Self::SingleFloat),
            _ => None,
        }
    }

    /// Returns whether this kind holds integers.
    #[must_use]
    pub const fn is_integer(self) -> bool {
        matches!(self, Self::Integer | Self::I32 | Self::U32)
    }

    /// Returns whether this kind holds IEEE 754 floats.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::Float | Self::SingleFloat)
    }

    /// Returns the source name of this kind's type.
    #[must_use]
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::Integer => "<integer>",
            Self::Float => "<double-float>",
            Self::I32 => "<i32>",
            Self::U32 => "<u32>",
            Self::SingleFloat => "<single-float>",
        }
    }
}

/// Semantic comparison operator.
///
/// On floats every operator follows IEEE 754: comparisons involving NaN are
/// false and `0.0 = -0.0` is true.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CompareOp {
    /// Equality.
    Eq,
    /// Strictly less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Strictly greater than.
    Gt,
    /// Greater than or equal.
    Ge,
}

impl CompareOp {
    /// Returns the operator that gives the same answer with the operands
    /// swapped: `a < b` is `b > a`.
    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Eq => Self::Eq,
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
        }
    }
}

impl From<ast::CompareOp> for CompareOp {
    fn from(value: ast::CompareOp) -> Self {
        match value {
            ast::CompareOp::Eq => Self::Eq,
            ast::CompareOp::Lt => Self::Lt,
            ast::CompareOp::Le => Self::Le,
            ast::CompareOp::Gt => Self::Gt,
            ast::CompareOp::Ge => Self::Ge,
        }
    }
}

/// Built-in function on `<float>` values.
///
/// Builtins resolve by name only after user functions, generics, host
/// functions, and accessors, so a program may define its own `abs` or `max`
/// without conflict. Every argument must be a `<float>`; integers are not
/// converted implicitly. Arguments of one format compute in it; mixed
/// formats widen to `<double-float>`, like arithmetic. Operations with a
/// tape instruction lower inline, in either format; [`FloatBuiltin::Library`]
/// functions call the runtime host in binary64, so a `<single-float>`
/// argument widens exactly and the result rounds once back to binary32.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FloatBuiltin {
    /// `abs(x)`: magnitude, clearing the sign bit.
    Abs,
    /// `min(x, y)`: IEEE 754-2019 `minimum`; NaN-propagating, `-0.0 < 0.0`.
    Min,
    /// `max(x, y)`: IEEE 754-2019 `maximum`; NaN-propagating, `-0.0 < 0.0`.
    Max,
    /// `remainder(x, y)`: `x - y * truncate(x / y)`, with the sign of `x`
    /// (Dylan `remainder`, C `fmod`).
    Remainder,
    /// `nan?(x)`: whether `x` is a NaN.
    IsNan,
    /// `finite?(x)`: whether `x` is neither infinite nor NaN.
    IsFinite,
    /// `floor(x)`: largest integral float not greater than `x`.
    Floor,
    /// `ceiling(x)`: smallest integral float not less than `x`.
    Ceiling,
    /// `round(x)`: nearest integral float, ties to even (Dylan `round`).
    Round,
    /// `truncate(x)`: integral part, rounding toward zero.
    Truncate,
    /// `sqrt(x)`: correctly rounded square root; NaN for negative `x`.
    Sqrt,
    /// A runtime library function such as `exp` or `sin`.
    Library(FloatFunction),
}

impl FloatBuiltin {
    /// Every float builtin, inline operations first.
    pub const ALL: &'static [Self] = &[
        Self::Abs,
        Self::Min,
        Self::Max,
        Self::Remainder,
        Self::IsNan,
        Self::IsFinite,
        Self::Floor,
        Self::Ceiling,
        Self::Round,
        Self::Truncate,
        Self::Sqrt,
        Self::Library(FloatFunction::Cbrt),
        Self::Library(FloatFunction::Exp),
        Self::Library(FloatFunction::Log),
        Self::Library(FloatFunction::Log2),
        Self::Library(FloatFunction::Log10),
        Self::Library(FloatFunction::Sin),
        Self::Library(FloatFunction::Cos),
        Self::Library(FloatFunction::Tan),
        Self::Library(FloatFunction::Asin),
        Self::Library(FloatFunction::Acos),
        Self::Library(FloatFunction::Atan),
        Self::Library(FloatFunction::Atan2),
        Self::Library(FloatFunction::Sinh),
        Self::Library(FloatFunction::Cosh),
        Self::Library(FloatFunction::Tanh),
        Self::Library(FloatFunction::Pow),
        Self::Library(FloatFunction::Hypot),
    ];

    /// Returns the Loko name that calls this builtin.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Abs => "abs",
            Self::Min => "min",
            Self::Max => "max",
            Self::Remainder => "remainder",
            Self::IsNan => "nan?",
            Self::IsFinite => "finite?",
            Self::Floor => "floor",
            Self::Ceiling => "ceiling",
            Self::Round => "round",
            Self::Truncate => "truncate",
            Self::Sqrt => "sqrt",
            Self::Library(function) => match function {
                FloatFunction::Cbrt => "cbrt",
                FloatFunction::Exp => "exp",
                FloatFunction::Log => "log",
                FloatFunction::Log2 => "log2",
                FloatFunction::Log10 => "log10",
                FloatFunction::Sin => "sin",
                FloatFunction::Cos => "cos",
                FloatFunction::Tan => "tan",
                FloatFunction::Asin => "asin",
                FloatFunction::Acos => "acos",
                FloatFunction::Atan => "atan",
                FloatFunction::Atan2 => "atan2",
                FloatFunction::Sinh => "sinh",
                FloatFunction::Cosh => "cosh",
                FloatFunction::Tanh => "tanh",
                FloatFunction::Pow => "pow",
                FloatFunction::Hypot => "hypot",
            },
        }
    }

    /// Returns the builtin called `name`, if any.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|builtin| builtin.name() == name)
    }

    /// Returns the number of `<float>` arguments.
    #[must_use]
    pub const fn arity(self) -> usize {
        match self {
            Self::Min | Self::Max | Self::Remainder => 2,
            Self::Abs
            | Self::IsNan
            | Self::IsFinite
            | Self::Floor
            | Self::Ceiling
            | Self::Round
            | Self::Truncate
            | Self::Sqrt => 1,
            Self::Library(function) => function.arity(),
        }
    }

    /// Returns the result category for binary64 arguments: `<boolean>` for
    /// predicates, otherwise `<double-float>`. Single-float arguments give a
    /// single-float result.
    #[must_use]
    pub const fn result_kind(self) -> ValueKind {
        match self {
            Self::IsNan | Self::IsFinite => ValueKind::Bool,
            _ => ValueKind::F64,
        }
    }
}

/// Built-in read-only runtime operation exposed by the Interloko kernel.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Intrinsic {
    /// Returns a value's runtime class metaobject as `<object>`.
    ObjectClass,
    /// Returns the name of a class metaobject.
    ClassName,
    /// Returns the direct superclass metaobject for a class, or `#f`.
    ClassDirectSuperclass,
    /// Returns direct slot-descriptor metaobjects for a class.
    ClassDirectSlots,
    /// Returns finalized effective slot-descriptor metaobjects for a class.
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
    /// Returns method metaobjects attached to a generic function.
    GenericFunctionMethods,
    /// Returns the generic-function metaobject implemented by a method.
    MethodGenericFunction,
    /// Returns class metaobjects for a method's specializers.
    MethodSpecializers,
    /// `size(c)` of a `<vector>` (a `<stretchy-vector>` too) or a
    /// `<table>`: its number of elements or entries.
    CollectionSize,
    /// `element(v, i)` of a vector: its element at `i` as `<object>`. An
    /// index outside the vector signals a `type-error.range` whose expected
    /// type is the vector's indices, and whose `use-value` replacement is
    /// an index, checked again.
    VectorElement,
    /// `element(v, i, default: d)` of a vector: its element at `i`, or `d`
    /// when `i` is outside it.
    VectorElementOr,
    /// `element-setter(x, v, i)`, which `v[i] := x` writes: stores `x` at
    /// `i` and returns it. An index outside the vector signals as
    /// [`Self::VectorElement`] does, and a store into a literal vector,
    /// which is constant, signals an `<immutable-collection-error>`.
    VectorElementSetter,
    /// `add!(v, x)`: adds `x` at the end of a `<stretchy-vector>` and
    /// returns the vector.
    StretchyAdd,
    /// `element(t, k)` of a `<table>`: the value stored under `k`. A key the
    /// table lacks signals a `<not-found-error>`, whose `use-value`
    /// replacement is the value.
    TableElement,
    /// `element(t, k, default: d)` of a `<table>`: the value stored under
    /// `k`, or `d`.
    TableElementOr,
    /// `element-setter(x, t, k)`, which `t[k] := x` writes: stores `x` under
    /// `k` and returns it.
    TableElementSetter,
    /// `key-exists?(t, k)`: whether a `<table>` has the key `k`.
    TableKeyExists,
    /// `remove-key!(t, k)`: removes `k`'s entry from a `<table>`, returning
    /// whether it had one.
    TableRemoveKey,
    /// `make(<vector>, size: n, fill: x)`: a new `<vector>` of `n`
    /// elements, each `x`. It has no name of its own.
    MakeVector,
    /// `make(<stretchy-vector>, size: n, fill: x)`: a new
    /// `<stretchy-vector>` of `n` elements, each `x`. It has no name of its
    /// own.
    MakeStretchyVector,
    /// `make(<table>)`: a new empty `<table>`. It has no name of its own.
    MakeTable,
    /// A `<table>`'s iteration version, which changes whenever its keys do.
    /// It has no name of its own: `for` reads it.
    TableVersion,
    /// A `<table>`'s number of entry positions, the limit of a `for` over
    /// it. It has no name of its own.
    TablePositions,
    /// `(table, from, version)`: the table's first entry position at or
    /// after `from`, or its number of positions. A table whose version is
    /// no longer `version` signals a `<collection-modified-error>`. It has
    /// no name of its own.
    TableNextPosition,
    /// `(table, position, version)`: the value of a `<table>`'s entry at a
    /// position. A table whose version is no longer `version`, or a
    /// position holding no entry, signals a `<collection-modified-error>`.
    /// It has no name of its own.
    TablePositionValue,
    /// The key of a `<table>`'s entry at a position, as
    /// [`Self::TablePositionValue`] reads its value.
    TablePositionKey,
    /// Whether a vector is constant, as a literal is. It has no name of
    /// its own.
    VectorConstant,
    /// Renders a float as its shortest round-trip literal text, such as
    /// `1.0`, `2.5e-7`, `inf`, or `nan`; a `<single-float>` carries its
    /// `f32` suffix, as in `0.1f32`.
    FloatToString,
    /// Renders an integer in decimal.
    IntegerToString,
    /// `$unsupplied`, the value of a keyword argument a call does not pass
    /// when the keyword has no default: one object, which nothing else is
    /// `==`. It has no name of its own.
    Unsupplied,
    /// The marker a call through a callable value passes for a keyword it
    /// does not pass, distinct from `$unsupplied`: only the value's entry
    /// sees it, and replaces it with the keyword's default. It has no name.
    KeywordAbsent,
    /// Returns a new string holding two strings joined in order.
    Concatenate,
    /// `instance?(value, type)` with a type object computed at run time,
    /// such as a `<type-error>`'s expected type: whether the value is an
    /// instance of it. A type the program writes, such as `<circle>`, is
    /// tested by [`Expr::InstanceOf`] or [`Expr::InstanceOfLimited`].
    InstanceOfType,
    /// Returns a `<limited-integer>`'s base class, `<integer>`.
    LimitedIntegerBase,
    /// Returns a `<limited-integer>`'s lower bound, or `#f` when it has
    /// none.
    LimitedIntegerMin,
    /// Returns a `<limited-integer>`'s upper bound, or `#f` when it has
    /// none.
    LimitedIntegerMax,
    /// Returns the class a `<subclass-type>` bounds.
    SubclassTypeClass,
    /// Returns a type object's name as Loko writes the type: `<circle>`,
    /// `<i32>`, `limited(<integer>, min: 0, max: 255)` or
    /// `subclass(<shape>)`.
    TypeName,
    /// A text operation the runtime host serves, such as `digit?` or
    /// `as(<character>, i)`.
    Text(TextFunction),
    /// One step of a `for` over a `<string>`: the character at the
    /// position in its second argument, a hidden local of the loop, which it
    /// advances past that character. Only the plan of a `for` over a known
    /// `<string>` makes it, with positions it made itself
    /// from the start of the string to its end, so the position always
    /// starts a character; the step reads and advances it with one decode.
    StringStep,
    /// Compares two `<character>`s by scalar value, inline.
    CharCompare(CompareOp),
    /// `as(<integer>, c)`: a `<character>`'s scalar value, inline.
    CharToInteger,
    /// Compares two `<string-position>`s by byte offset, inline.
    PositionCompare(CompareOp),
    /// `byte-offset(p)`: a `<string-position>`'s byte offset, inline.
    PositionToInteger,
    /// `pair(head, tail)`: a new `<pair>` of two objects.
    ListPair,
    /// `head(list)`: a pair's head, or `#()` for the empty list.
    ListHead,
    /// `tail(list)`: a pair's tail, or `#()` for the empty list.
    ListTail,
    /// `head-setter(value, pair)`: stores a pair's head and returns it.
    ListHeadSetter,
    /// `tail-setter(value, pair)`: stores a pair's tail and returns it.
    ListTailSetter,
    /// `#()`, the empty list. It has no name of its own: the literal
    /// writes it.
    EmptyList,
    /// One result of an integer division, which the runtime host computes.
    /// Only the division builtins (`floor/` and the others) call it, with a
    /// divisor that is neither zero nor `-1`.
    IntegerDivision(IntegerDivision),
}

/// Semantic expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    /// Integer constant.
    ConstI64 {
        /// Constant value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Float constant.
    ConstF64 {
        /// Constant value, compared by bit pattern.
        value: FloatBits,
        /// Source span.
        span: Span,
    },
    /// `<i32>`, `<u32>` or `<single-float>` constant.
    Const32 {
        /// Constant value; floats compare by bit pattern.
        value: Number32,
        /// Source span.
        span: Span,
    },
    /// Boolean constant.
    ConstBool {
        /// Constant value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// `<character>` constant.
    ConstChar {
        /// Constant Unicode scalar value.
        value: char,
        /// Source span.
        span: Span,
    },
    /// String literal value.
    StringLiteral {
        /// Decoded literal value.
        value: String,
        /// Built-in string class.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// Symbol literal value.
    SymbolLiteral {
        /// Interned symbol name.
        name: String,
        /// Built-in symbol class.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// Immutable vector literal.
    VectorLiteral {
        /// Ordered element expressions.
        elements: Vec<Expr>,
        /// Built-in vector class.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// Multiple values produced for return or destructuring contexts.
    Values {
        /// Ordered value expressions.
        values: Vec<Expr>,
        /// Result value categories in order.
        result_kinds: Vec<ValueKind>,
        /// Source span.
        span: Span,
    },
    /// Resolved local reference.
    Local {
        /// Local identifier.
        local: LocalId,
        /// Value category at this use site, including local refinements.
        result_kind: ValueKind,
        /// Typed callable signature at this use site, when known.
        callable_signature: Option<CallableSignature>,
        /// Source span.
        span: Span,
    },
    /// Resolved closure capture reference.
    Capture {
        /// Capture identifier.
        capture: CaptureId,
        /// Value category at this use site.
        result_kind: ValueKind,
        /// Typed callable signature at this use site, when known.
        callable_signature: Option<CallableSignature>,
        /// Source span.
        span: Span,
    },
    /// Immutable top-level constant reference.
    ConstantRef {
        /// Referenced constant.
        constant: ConstantId,
        /// Constant value category.
        result_kind: ValueKind,
        /// Typed callable signature when the constant is a typed callable value.
        callable_signature: Option<CallableSignature>,
        /// Source span.
        span: Span,
    },
    /// A slot's default, evaluated by running its
    /// [`SlotInitializer`] in a frame of its own.
    SlotDefault {
        /// Class that declares the slot.
        class: ClassId,
        /// The slot's id among that class's direct slots.
        slot: SlotId,
        /// Slot value category.
        result_kind: ValueKind,
        /// Typed callable signature when the slot holds a typed callable.
        callable_signature: Option<CallableSignature>,
        /// Source span of the default.
        span: Span,
    },
    /// Resolved class object value.
    ClassObject {
        /// Nominal class represented by this value.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// A type object the program writes that is not a class:
    /// `limited(<integer>, min: 0, max: 255)`, `<i32>` or `<u32>`.
    TypeObject {
        /// The type.
        object: TypeObject,
        /// The built-in class of the type object, `<limited-integer>`.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// Compile-time-known callable value.
    KnownCallable {
        /// Known callable identity.
        callable: KnownCallable,
        /// Source span.
        span: Span,
    },
    /// Anonymous function value with an explicit capture vector.
    Closure {
        /// Closure body.
        closure: ClosureId,
        /// Captures read from the enclosing context in environment order.
        captures: Vec<Capture>,
        /// Result value category.
        result_kind: ValueKind,
        /// Typed callable signature for this closure value.
        callable_signature: CallableSignature,
        /// Source span.
        span: Span,
    },
    /// Resolved generic-function metaobject value.
    GenericFunctionObject {
        /// Generic function represented by this value.
        generic: GenericFunctionId,
        /// Source span.
        span: Span,
    },
    /// Built-in runtime intrinsic call.
    IntrinsicCall {
        /// Intrinsic operation.
        intrinsic: Intrinsic,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Result value category.
        result_kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Call through a function declared by the embedding host.
    HostCall {
        /// Registry-local host function.
        function: HostFunctionId,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Resolved result category.
        result_kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Resolved direct function call.
    DirectCall {
        /// Callee function.
        function: FunctionId,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// Resolved generic call.
    GenericCall {
        /// Generic function.
        generic: GenericFunctionId,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Dispatch check site.
        site: SiteId,
        /// Source span.
        span: Span,
    },
    /// Call through a first-class callable value.
    CallableCall {
        /// Callable expression.
        callable: Box<Expr>,
        /// Argument expressions.
        args: Vec<Expr>,
        /// How `args` pass the value its arguments.
        arguments: CallArguments,
        /// Expected argument categories for this indirect call.
        arg_kinds: Vec<ValueKind>,
        /// The keywords the last `keywords.len()` arguments pass, each a
        /// boxed `<object>`: for a typed callable, every keyword of its
        /// signature in declaration order, the keyword-absent marker
        /// ([`Intrinsic::KeywordAbsent`]) for one the call does not pass;
        /// for an untyped one, those the call passes, in source order.
        keywords: Vec<String>,
        /// Result value categories in return order.
        result_kinds: Vec<ValueKind>,
        /// Typed callable signatures for callable results, when known.
        result_callable_sigs: Vec<Option<CallableSignature>>,
        /// Source span.
        span: Span,
    },
    /// Recoverable condition signal.
    Signal {
        /// Condition object expression.
        condition: Box<Expr>,
        /// Statically known condition class.
        condition_class: ClassId,
        /// Result categories supplied by the active restart.
        result_kinds: Vec<ValueKind>,
        /// Typed callable signatures for signal results, when known.
        result_callable_sigs: Vec<Option<CallableSignature>>,
        /// Source span.
        span: Span,
    },
    /// Reads the class object for a known runtime object.
    ClassOf {
        /// Runtime object expression.
        object: Box<Expr>,
        /// Statically known object class.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime class membership test.
    InstanceOf {
        /// Tested value.
        value: Box<Expr>,
        /// Target class.
        class: ClassId,
        /// Instance test site.
        site: SiteId,
        /// Whether an `as` makes the test, to pick the conversion a boxed
        /// number takes by its class, rather than source asking with
        /// `instance?`. Typing reports it as part of the `as`.
        conversion: bool,
        /// Source span.
        span: Span,
    },
    /// Runtime membership test for a limited integer type, such as `<u32>`
    /// or `limited(<integer>, min: 0)`: whether the value is an `<integer>`
    /// in the type's range.
    InstanceOfLimited {
        /// Tested value.
        value: Box<Expr>,
        /// The type's range.
        range: IntRange,
        /// Instance test site.
        site: SiteId,
        /// Source span.
        span: Span,
    },
    /// Runtime checked object cast.
    Cast {
        /// Cast value.
        value: Box<Expr>,
        /// Target class.
        class: ClassId,
        /// Cast check site.
        site: SiteId,
        /// Whether source asked for the check with `as(...)`, rather than
        /// the compiler inserting it where a value meets a declaration.
        /// Strict typing keeps an explicit check, which is how strict code
        /// checks deliberately, but must prove an inserted one.
        explicit: bool,
        /// Source span.
        span: Span,
    },
    /// Runtime object construction of a class named by a class literal.
    MakeObject {
        /// Class object expression.
        class: Box<Expr>,
        /// Statically known nominal class to instantiate.
        class_id: ClassId,
        /// Explicit init arguments in source order.
        init_args: Vec<InitArg>,
        /// Planned field initializers in initialization-plan order.
        fields: Vec<FieldInit>,
        /// Source span.
        span: Span,
    },
    /// Runtime object construction of the class a class value names at run
    /// time, such as `make(class-of(x))`.
    ///
    /// The value may name `bound` or any subclass, so the object allocated is
    /// the value's own class, and the result is only known to be in `bound`'s
    /// cone. Init arguments are checked against `bound` statically; each
    /// candidate class completes them with its own defaults. A class value
    /// outside the candidates, or a candidate needing an init keyword the
    /// call does not pass, traps.
    MakeRuntimeClass {
        /// Class object expression.
        class: Box<Expr>,
        /// Static bound on the class the value names.
        bound: ClassId,
        /// Explicit init arguments in source order, evaluated once before the
        /// class is chosen.
        init_args: Vec<InitArg>,
        /// Classes the value may name, in class id order.
        candidates: Vec<MakeCandidate>,
        /// Source span.
        span: Span,
    },
    /// Getter accessor call.
    AccessorCall {
        /// Stable accessor identity.
        accessor: DefinitionId,
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Receiver class.
        class: ClassId,
        /// Slot being read.
        slot: SlotId,
        /// Slot value category.
        result_kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Native slot write performed by a synthesized setter method.
    SlotWrite {
        /// Stable setter method identity.
        setter: DefinitionId,
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Assigned value expression.
        value: Box<Expr>,
        /// Receiver class.
        class: ClassId,
        /// Slot being written.
        slot: SlotId,
        /// Slot value category.
        result_kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Binary arithmetic expression.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Numeric kind shared by both operands and the result.
        kind: NumericKind,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Check site of integer arithmetic, which can overflow; `None` for
        /// float arithmetic, which cannot trap.
        site: Option<SiteId>,
        /// Source span.
        span: Span,
    },
    /// Numeric comparison.
    Compare {
        /// Operator.
        op: CompareOp,
        /// Numeric kind shared by both operands.
        kind: NumericKind,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Exact comparison of one `<integer>` with one binary64 float, as
    /// mathematical values; see [`crate::promotion`].
    MixedCompare {
        /// Operator.
        op: CompareOp,
        /// Numeric kind of the left operand, [`NumericKind::Integer`] or
        /// [`NumericKind::Float`]; the right operand has the other kind.
        lhs_kind: NumericKind,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Conversion between numeric kinds, from source such as
    /// `as(<double-float>, n)` or `as(<integer>, x)`, or inserted by the
    /// promotion rules.
    ///
    /// Widening is exact. Integer to float, and binary64 to binary32, round
    /// to nearest, ties to even. Narrowing an integer signals a
    /// `<type-error>` unless the value is in the target's range, and float
    /// to integer is exact: it signals one unless the operand is an
    /// integral value in range, so rounding is always an explicit
    /// `round`/`floor`/... choice.
    Convert {
        /// Numeric kind of the result; the operand has another kind.
        to: NumericKind,
        /// Converted operand.
        value: Box<Expr>,
        /// Conversion check site.
        site: SiteId,
        /// Whether source asked for the check with `as(...)`, rather than
        /// the compiler inserting it where a value meets a declaration.
        /// Strict typing keeps an explicit check, which is how strict code
        /// checks deliberately, but must prove an inserted one.
        explicit: bool,
        /// Source span.
        span: Span,
    },
    /// Check that an integer is in a limited type's range: from source
    /// `as(limited(<integer>, ...), n)`, or inserted where a value reaches a
    /// declaration of a limited type (an argument for a limited parameter, a
    /// `let` binding, a slot write, a constant, or a definition's result).
    ///
    /// The operand is an `<integer>`, `<i32>` or `<u32>`, and the result is
    /// the `<integer>` it is. A value outside the range signals a
    /// `<type-error>` whose `use-value` replacement is checked again, so the
    /// result is always in range.
    CheckRange {
        /// Checked operand.
        value: Box<Expr>,
        /// Range the result is in.
        range: IntRange,
        /// Range check site.
        site: SiteId,
        /// Whether source asked for the check with `as(...)`, rather than
        /// the compiler inserting it where a value meets a declaration.
        /// Strict typing keeps an explicit check, which is how strict code
        /// checks deliberately, but must prove an inserted one.
        explicit: bool,
        /// Source span.
        span: Span,
    },
    /// Call of a built-in float function.
    FloatCall {
        /// Called builtin.
        builtin: FloatBuiltin,
        /// `<float>` argument expressions.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// Arithmetic negation, from source unary `-`.
    ///
    /// Float negation flips the sign bit, so `-0.0` and `-(0.0)` are both
    /// negative zero; it is not `0.0 - x`.
    Negate {
        /// Numeric kind of the operand and result.
        kind: NumericKind,
        /// Negated operand.
        value: Box<Expr>,
        /// Check site of integer negation, which can overflow; `None` for
        /// float negation.
        site: Option<SiteId>,
        /// Source span.
        span: Span,
    },
    /// Equality for runtime value objects where identity/content equality is defined.
    ValueEqual {
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Operand value category.
        kind: ValueKind,
        /// Source span.
        span: Span,
    },
    /// Identity, `==`: whether two values are the same object. Both operands
    /// are boxed values; integer operands lower to an integer compare instead.
    Identical {
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Assignment of a `let` local, `name := value`, whose value is the
    /// local's new value.
    ///
    /// The value already has the local's category and, for a limited
    /// local, passed its range check.
    AssignLocal {
        /// Assigned local.
        local: LocalId,
        /// The local's declared category, which is the result's.
        result_kind: ValueKind,
        /// The local's typed callable signature, when it has one.
        callable_signature: Option<CallableSignature>,
        /// Assigned value.
        value: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Assignment of a shared enclosing local through a closure's capture,
    /// `name := value`, whose value is the local's new value.
    ///
    /// The capture holds the local's cell; see [`Local::shared`]. The value
    /// is already checked as for [`Self::AssignLocal`].
    AssignCapture {
        /// Assigned capture, which is [shared](Capture::shared).
        capture: CaptureId,
        /// The local's declared category, which is the result's.
        result_kind: ValueKind,
        /// The local's typed callable signature, when it has one.
        callable_signature: Option<CallableSignature>,
        /// Assigned value.
        value: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Conditional loop: evaluates `test` and, while it asks for another
    /// iteration, the body. Produces no values.
    Loop {
        /// Whether the body runs while or until the test is `#t`.
        kind: LoopKind,
        /// `<boolean>` test, evaluated before each iteration.
        test: Box<Expr>,
        /// Body evaluated for effect; its locals are scoped to one
        /// iteration.
        body: Box<Block>,
        /// Source span.
        span: Span,
    },
    /// Conditional expression.
    If {
        /// Boolean condition.
        condition: Box<Expr>,
        /// Optional local refinement applied to the true branch.
        refinement: Option<Refinement>,
        /// Expression evaluated when the condition is true.
        then_branch: Box<Expr>,
        /// Expression evaluated when the condition is false.
        else_branch: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Evaluates `value` for its effects and produces no values.
    ///
    /// An arm of a conditional without a default body, such as `if (c) f()
    /// end`, is discarded this way: the conditional produces no values.
    Discard {
        /// Expression whose values are dropped.
        value: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Recoverable condition handler expression.
    Handle {
        /// Handled body expression.
        body: Box<Expr>,
        /// Restart clause for the handled body.
        restart: Restart,
        /// Source span.
        span: Span,
    },
    /// Dylan-style `block` expression whose value is its body's.
    ///
    /// The cleanup body runs on every exit from the body, normal or not.
    Block {
        /// Exit procedure bound for the body, when present.
        exit: Option<BlockExit>,
        /// Protected body; its locals are scoped to the block.
        body: Box<Block>,
        /// Cleanup body, evaluated for effect.
        cleanup: Option<Box<Block>>,
        /// Source span.
        span: Span,
    },
    /// Placeholder for semantically invalid code.
    Error {
        /// Source span.
        span: Span,
    },
}

impl Expr {
    /// Whether this is `$unsupplied`, which a call passes for a keyword
    /// argument it does not pass and that has no default.
    #[must_use]
    pub fn is_unsupplied(&self) -> bool {
        matches!(
            self,
            Self::IntrinsicCall {
                intrinsic: Intrinsic::Unsupplied,
                ..
            }
        )
    }

    /// Visits this expression's direct subexpressions in evaluation order,
    /// including those of the bodies it contains. A closure's body is code
    /// of its own and is not visited.
    pub fn for_each_child<'e>(&'e self, mut visit: impl FnMut(&'e Self)) {
        match self {
            Self::VectorLiteral {
                elements: items, ..
            }
            | Self::Values { values: items, .. }
            | Self::IntrinsicCall { args: items, .. }
            | Self::HostCall { args: items, .. }
            | Self::DirectCall { args: items, .. }
            | Self::GenericCall { args: items, .. }
            | Self::FloatCall { args: items, .. } => items.iter().for_each(visit),
            Self::CallableCall { callable, args, .. } => {
                visit(callable);
                args.iter().for_each(visit);
            }
            Self::Convert { value: inner, .. }
            | Self::CheckRange { value: inner, .. }
            | Self::Negate { value: inner, .. }
            | Self::Signal {
                condition: inner, ..
            }
            | Self::ClassOf { object: inner, .. }
            | Self::InstanceOf { value: inner, .. }
            | Self::InstanceOfLimited { value: inner, .. }
            | Self::Cast { value: inner, .. }
            | Self::AccessorCall {
                receiver: inner, ..
            }
            | Self::AssignLocal { value: inner, .. }
            | Self::AssignCapture { value: inner, .. }
            | Self::Discard { value: inner, .. } => visit(inner),
            Self::MakeObject { class, fields, .. } => {
                visit(class);
                fields.iter().for_each(|field| visit(&field.value));
            }
            Self::MakeRuntimeClass {
                class,
                init_args,
                candidates,
                ..
            } => {
                visit(class);
                init_args.iter().for_each(|arg| visit(&arg.value));
                candidates
                    .iter()
                    .flat_map(|candidate| &candidate.defaults)
                    .for_each(|field| visit(&field.value));
            }
            Self::SlotWrite {
                receiver: lhs,
                value: rhs,
                ..
            }
            | Self::Binary { lhs, rhs, .. }
            | Self::Compare { lhs, rhs, .. }
            | Self::MixedCompare { lhs, rhs, .. }
            | Self::ValueEqual { lhs, rhs, .. }
            | Self::Identical { lhs, rhs, .. } => {
                visit(lhs);
                visit(rhs);
            }
            Self::Loop { test, body, .. } => {
                visit(test);
                body.for_each_expr(visit);
            }
            Self::If {
                condition,
                then_branch,
                else_branch,
                ..
            } => {
                visit(condition);
                visit(then_branch);
                visit(else_branch);
            }
            Self::Handle { body, restart, .. } => {
                visit(body);
                if let Some(guard) = &restart.guard {
                    visit(guard);
                }
                visit(&restart.body);
            }
            Self::Block { body, cleanup, .. } => {
                body.for_each_expr(&mut visit);
                if let Some(cleanup) = cleanup {
                    cleanup.for_each_expr(&mut visit);
                }
            }
            Self::Closure { .. }
            | Self::ConstI64 { .. }
            | Self::ConstF64 { .. }
            | Self::Const32 { .. }
            | Self::ConstBool { .. }
            | Self::ConstChar { .. }
            | Self::StringLiteral { .. }
            | Self::SymbolLiteral { .. }
            | Self::Local { .. }
            | Self::Capture { .. }
            | Self::ConstantRef { .. }
            | Self::SlotDefault { .. }
            | Self::ClassObject { .. }
            | Self::TypeObject { .. }
            | Self::KnownCallable { .. }
            | Self::GenericFunctionObject { .. }
            | Self::Error { .. } => {}
        }
    }

    /// The locals an assignment in this expression assigns, outside any
    /// closure in it (a closure cannot assign what it captures), each once.
    #[must_use]
    pub fn assigned_locals(&self) -> Vec<LocalId> {
        let mut out = Vec::new();
        self.collect_assigned(&mut out);
        out
    }

    fn collect_assigned(&self, out: &mut Vec<LocalId>) {
        let assigned = match self {
            Self::AssignLocal { local, .. } => Some(local),
            // A string loop's step advances its position local in place.
            Self::IntrinsicCall {
                intrinsic: Intrinsic::StringStep,
                args,
                ..
            } => match args.as_slice() {
                [_, Self::Local { local, .. }] => Some(local),
                _ => None,
            },
            _ => None,
        };
        if let Some(local) = assigned
            && !out.contains(local)
        {
            out.push(*local);
        }
        self.for_each_child(|child| child.collect_assigned(out));
    }

    /// Returns this expression's source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::ConstI64 { span, .. }
            | Self::ConstF64 { span, .. }
            | Self::Const32 { span, .. }
            | Self::CheckRange { span, .. }
            | Self::ConstBool { span, .. }
            | Self::ConstChar { span, .. }
            | Self::StringLiteral { span, .. }
            | Self::SymbolLiteral { span, .. }
            | Self::VectorLiteral { span, .. }
            | Self::Values { span, .. }
            | Self::Local { span, .. }
            | Self::Capture { span, .. }
            | Self::ConstantRef { span, .. }
            | Self::SlotDefault { span, .. }
            | Self::ClassObject { span, .. }
            | Self::TypeObject { span, .. }
            | Self::KnownCallable { span, .. }
            | Self::Closure { span, .. }
            | Self::GenericFunctionObject { span, .. }
            | Self::IntrinsicCall { span, .. }
            | Self::HostCall { span, .. }
            | Self::DirectCall { span, .. }
            | Self::GenericCall { span, .. }
            | Self::CallableCall { span, .. }
            | Self::Signal { span, .. }
            | Self::ClassOf { span, .. }
            | Self::InstanceOf { span, .. }
            | Self::InstanceOfLimited { span, .. }
            | Self::Cast { span, .. }
            | Self::MakeObject { span, .. }
            | Self::MakeRuntimeClass { span, .. }
            | Self::AccessorCall { span, .. }
            | Self::SlotWrite { span, .. }
            | Self::Binary { span, .. }
            | Self::Negate { span, .. }
            | Self::Convert { span, .. }
            | Self::FloatCall { span, .. }
            | Self::Compare { span, .. }
            | Self::MixedCompare { span, .. }
            | Self::ValueEqual { span, .. }
            | Self::Identical { span, .. }
            | Self::AssignLocal { span, .. }
            | Self::AssignCapture { span, .. }
            | Self::Loop { span, .. }
            | Self::If { span, .. }
            | Self::Discard { span, .. }
            | Self::Handle { span, .. }
            | Self::Block { span, .. }
            | Self::Error { span } => *span,
        }
    }
}

/// Exit procedure bound by a `block` expression.
///
/// The local holds a first-class `<callable>` taking the block's result
/// values. Calling it transfers them to the block, running crossed cleanups;
/// calling it after the block's extent has ended is a runtime error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockExit {
    /// Local bound to the exit procedure.
    pub local: LocalId,
    /// Block result categories, which are also the exit's parameters.
    pub result_kinds: Vec<ValueKind>,
}

/// Semantic restart clause for a lexical handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Restart {
    /// Nominal condition class handled by this restart.
    pub specializer: ClassId,
    /// Optional handler-local bound to the signaled condition object.
    pub condition_local: Option<LocalId>,
    /// Guard closure taking the condition and returning `<boolean>`. It runs
    /// on the signalling stack during handler search; `#f` declines.
    pub guard: Option<Box<Expr>>,
    /// Handler body evaluated when a matching condition is signaled.
    pub body: Box<Expr>,
    /// Result categories of the whole `handle`: the join of the protected
    /// body's results and the handler body's results.
    pub result_kinds: Vec<ValueKind>,
    /// Typed callable signatures for those results, when known.
    pub result_callable_sigs: Vec<Option<CallableSignature>>,
    /// Source span.
    pub span: Span,
}

/// Lowers an AST module to semantic IR.
#[must_use]
pub fn lower(module: &ast::Module) -> Module {
    lower_with_host_registry(module, &HostMetaRegistry::new())
}

/// Lowers an AST module with host-backed metaobject declarations.
#[must_use]
pub fn lower_with_host_registry(module: &ast::Module, registry: &HostMetaRegistry) -> Module {
    let host_functions = registry
        .functions
        .iter()
        .map(|function| function.name.as_str())
        .collect::<Vec<_>>();
    let keywords::Desugared {
        module,
        functions,
        generics,
    } = keywords::desugar(module, &host_functions);
    let mut lowering = Lowering::new(&module, registry, functions, generics);
    lowering.lower_module(&module)
}

#[derive(Clone, Debug)]
struct FunctionSymbol {
    name: String,
    id: FunctionId,
    /// The parameters' kinds, a `#rest` parameter's last.
    param_kinds: Vec<ValueKind>,
    /// Whether the last parameter is a `#rest` parameter.
    rest: bool,
    param_callable_sigs: Vec<Option<CallableSignature>>,
    param_ranges: Vec<Option<IntRange>>,
    return_kinds: Vec<ValueKind>,
    return_callable_sigs: Vec<Option<CallableSignature>>,
    return_ranges: Vec<Option<IntRange>>,
    return_annotation: Option<Vec<ValueKind>>,
    span: Span,
}

impl FunctionSymbol {
    fn arity(&self) -> usize {
        self.param_kinds.len()
    }

    /// The number of arguments a call passes at least: every parameter's,
    /// or every one before the `#rest` parameter.
    fn required(&self) -> usize {
        self.arity() - usize::from(self.rest)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedType {
    kind: ValueKind,
    callable_signature: Option<CallableSignature>,
    /// The range a `limited(<integer>, ...)` type declares; `None` for
    /// every other type, `<integer>` itself included.
    range: Option<IntRange>,
}

/// A definition's resolved result annotation; see
/// `Lowering::resolve_declared_results`.
struct DeclaredResults {
    /// The declared kinds, or `None` for an open result.
    kinds: Option<Vec<ValueKind>>,
    callable_sigs: Vec<Option<CallableSignature>>,
    ranges: Vec<Option<IntRange>>,
}

/// Resolved types of a list of values, such as a result annotation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ResolvedResults {
    kinds: Vec<ValueKind>,
    callable_sigs: Vec<Option<CallableSignature>>,
    ranges: Vec<Option<IntRange>>,
}

/// The values an integer `kind` holds: `<i32>`'s and `<u32>`'s ranges, or
/// every `<integer>`.
const fn integer_range_of(kind: NumericKind) -> IntRange {
    match kind {
        NumericKind::I32 => IntRange::I32,
        NumericKind::U32 => IntRange::U32,
        _ => IntRange::INTEGER,
    }
}

/// Spells an `<integer>` range as source writes a limited type of kind
/// `<integer>`: `limited(<integer>, min: 0, max: 255)`, leaving out an
/// unbounded side. A range equal to `<i32>`'s is spelled out too, since
/// `<i32>` names another representation.
pub(crate) fn limited_type_name(range: IntRange) -> String {
    let mut name = String::from("limited(<integer>");
    if let Some(min) = range.min {
        name.push_str(&format!(", min: {min}"));
    }
    if let Some(max) = range.max {
        name.push_str(&format!(", max: {max}"));
    }
    name.push(')');
    name
}

/// Where in a conditional an arm lowered to nested [`Expr::If`]s was
/// written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Conditional {
    /// The first arm of `if (test) body elseif … else body end`.
    If,
    /// A later arm of an `if`: `elseif (test) body`.
    Elseif,
    /// An arm of `case test => body; … otherwise => body end`.
    Case,
    /// An arm of `select (target) key, key => body; … end`.
    Select,
}

impl Conditional {
    /// The arms after this one.
    const fn rest(self) -> Self {
        match self {
            Self::If | Self::Elseif => Self::Elseif,
            Self::Case => Self::Case,
            Self::Select => Self::Select,
        }
    }

    /// The keyword the arm's test follows.
    const fn test_keyword(self) -> &'static str {
        match self {
            Self::If => "if",
            Self::Elseif => "elseif",
            Self::Case => "case",
            Self::Select => "select",
        }
    }

    /// The keyword that opens the whole conditional.
    const fn construct(self) -> &'static str {
        match self {
            Self::If | Self::Elseif => "if",
            Self::Case => "case",
            Self::Select => "select",
        }
    }
}

/// One arm of a conditional being lowered: its test and its body.
#[derive(Clone, Copy)]
struct ArmRef<'a> {
    test: ArmTest<'a>,
    body: &'a ast::Block,
    span: Span,
}

/// How an arm of a conditional tests whether it runs.
#[derive(Clone, Copy)]
enum ArmTest<'a> {
    /// A written `<boolean>` test.
    Expr(&'a ast::Expr),
    /// A `select` arm's keys, matched against the target in turn.
    Keys(&'a [ast::Expr]),
}

fn arm_refs(arms: &[ast::CondArm]) -> Vec<ArmRef<'_>> {
    arms.iter()
        .map(|arm| ArmRef {
            test: ArmTest::Expr(&arm.test),
            body: &arm.body,
            span: arm.span,
        })
        .collect()
}

/// How a `select`'s keys match its target: `target == key`, or
/// `callee(target, key)` with `by`.
struct SelectMatcher {
    id: AstNodeId,
    /// Reads the target: a local, a literal, or the hidden target local.
    target: ast::Expr,
    /// The `by` test's name, or the hidden local holding it.
    callee: Option<String>,
}

/// Visits `expr` and every expression its own code evaluates, in evaluation
/// order. Closure bodies are not visited; a constant's value is not either.
pub fn visit_evaluated(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    use Expr as E;
    match expr {
        E::VectorLiteral {
            elements: items, ..
        }
        | E::Values { values: items, .. }
        | E::IntrinsicCall { args: items, .. }
        | E::HostCall { args: items, .. }
        | E::DirectCall { args: items, .. }
        | E::GenericCall { args: items, .. }
        | E::FloatCall { args: items, .. } => {
            for item in items {
                visit_evaluated(item, visit);
            }
        }
        E::CallableCall { callable, args, .. } => {
            visit_evaluated(callable, visit);
            for arg in args {
                visit_evaluated(arg, visit);
            }
        }
        E::Convert { value: inner, .. }
        | E::CheckRange { value: inner, .. }
        | E::Negate { value: inner, .. }
        | E::Signal {
            condition: inner, ..
        }
        | E::ClassOf { object: inner, .. }
        | E::InstanceOf { value: inner, .. }
        | E::InstanceOfLimited { value: inner, .. }
        | E::Cast { value: inner, .. }
        | E::AccessorCall {
            receiver: inner, ..
        } => visit_evaluated(inner, visit),
        E::MakeObject { class, fields, .. } => {
            visit_evaluated(class, visit);
            for field in fields {
                visit_evaluated(&field.value, visit);
            }
        }
        E::MakeRuntimeClass {
            class,
            init_args,
            candidates,
            ..
        } => {
            visit_evaluated(class, visit);
            for arg in init_args {
                visit_evaluated(&arg.value, visit);
            }
            for field in candidates.iter().flat_map(|candidate| &candidate.defaults) {
                visit_evaluated(&field.value, visit);
            }
        }
        E::SlotWrite {
            receiver, value, ..
        } => {
            visit_evaluated(receiver, visit);
            visit_evaluated(value, visit);
        }
        E::AssignLocal { value, .. } | E::AssignCapture { value, .. } => {
            visit_evaluated(value, visit)
        }
        E::Loop { test, body, .. } => {
            visit_evaluated(test, visit);
            visit_evaluated_block(body, visit);
        }
        E::Binary { lhs, rhs, .. }
        | E::Compare { lhs, rhs, .. }
        | E::MixedCompare { lhs, rhs, .. }
        | E::ValueEqual { lhs, rhs, .. }
        | E::Identical { lhs, rhs, .. } => {
            visit_evaluated(lhs, visit);
            visit_evaluated(rhs, visit);
        }
        E::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit_evaluated(condition, visit);
            visit_evaluated(then_branch, visit);
            visit_evaluated(else_branch, visit);
        }
        E::Discard { value, .. } => visit_evaluated(value, visit),
        E::Handle { body, restart, .. } => {
            visit_evaluated(body, visit);
            if let Some(guard) = &restart.guard {
                visit_evaluated(guard, visit);
            }
            visit_evaluated(&restart.body, visit);
        }
        E::Block { body, cleanup, .. } => {
            visit_evaluated_block(body, visit);
            if let Some(cleanup) = cleanup {
                visit_evaluated_block(cleanup, visit);
            }
        }
        E::Closure { .. }
        | E::ConstI64 { .. }
        | E::ConstF64 { .. }
        | E::Const32 { .. }
        | E::ConstBool { .. }
        | E::ConstChar { .. }
        | E::StringLiteral { .. }
        | E::SymbolLiteral { .. }
        | E::Local { .. }
        | E::Capture { .. }
        | E::ConstantRef { .. }
        | E::SlotDefault { .. }
        | E::ClassObject { .. }
        | E::TypeObject { .. }
        | E::KnownCallable { .. }
        | E::GenericFunctionObject { .. }
        | E::Error { .. } => {}
    }
    visit(expr);
}

fn visit_evaluated_block(block: &Block, visit: &mut impl FnMut(&Expr)) {
    for statement in &block.statements {
        match statement {
            Statement::Let(binding) => visit_evaluated(&binding.value, visit),
            Statement::Expr(expr) => visit_evaluated(expr, visit),
        }
    }
    visit_evaluated(&block.result, visit);
}

/// The kind a value declared as the built-in class `kind`, whose id is
/// `class`, has, or `None` for an abstract numeric class, which has no
/// single representation.
///
/// Every annotation that names a built-in class as the type of a value
/// resolves through this one match, so a new kernel class must say here how
/// its values are held before anything compiles.
const fn builtin_value_kind(kind: BuiltinValueClass, class: ClassId) -> Option<ValueKind> {
    match kind {
        BuiltinValueClass::String => Some(ValueKind::String(class)),
        BuiltinValueClass::Symbol => Some(ValueKind::Symbol(class)),
        BuiltinValueClass::Vector => Some(ValueKind::Vector(class)),
        BuiltinValueClass::Integer => Some(ValueKind::I64),
        BuiltinValueClass::DoubleFloat => Some(ValueKind::F64),
        BuiltinValueClass::SingleFloat => Some(ValueKind::F32),
        BuiltinValueClass::Boolean => Some(ValueKind::Bool),
        BuiltinValueClass::Character => Some(ValueKind::Char),
        BuiltinValueClass::StringPosition => Some(ValueKind::Position),
        // A value declared `<function>` is any callable value.
        BuiltinValueClass::Function => Some(ValueKind::Callable),
        BuiltinValueClass::Number
        | BuiltinValueClass::Real
        | BuiltinValueClass::Rational
        | BuiltinValueClass::Float => None,
        BuiltinValueClass::Class
        | BuiltinValueClass::GenericFunction
        | BuiltinValueClass::Method
        | BuiltinValueClass::SlotDescriptor
        | BuiltinValueClass::Type
        | BuiltinValueClass::LimitedIntegerType
        | BuiltinValueClass::SubclassType
        | BuiltinValueClass::StringBuilder
        | BuiltinValueClass::StretchyVector
        | BuiltinValueClass::Table
        | BuiltinValueClass::Sequence
        | BuiltinValueClass::List
        | BuiltinValueClass::Pair
        | BuiltinValueClass::EmptyList => Some(ValueKind::Object(class)),
    }
}

/// Whether a slot default `value` is a literal, cheap and the same wherever
/// it is computed, which each `make` computes in place: a number, boolean,
/// symbol, metaobject, known function or constant, or check-free float
/// arithmetic over them, and the range check a limited slot puts on one.
/// Any other default runs as code of its own.
fn default_inlines(value: &Expr) -> bool {
    match value {
        Expr::CheckRange {
            value,
            explicit: false,
            ..
        } => default_inlines(value),
        Expr::ConstI64 { .. }
        | Expr::ConstF64 { .. }
        | Expr::Const32 { .. }
        | Expr::ConstBool { .. }
        | Expr::SymbolLiteral { .. }
        | Expr::ClassObject { .. }
        | Expr::TypeObject { .. }
        | Expr::GenericFunctionObject { .. }
        | Expr::KnownCallable { .. }
        | Expr::ConstantRef { .. } => true,
        Expr::Negate {
            value, site: None, ..
        } => default_inlines(value),
        Expr::Binary {
            lhs,
            rhs,
            site: None,
            ..
        }
        | Expr::Compare { lhs, rhs, .. } => default_inlines(lhs) && default_inlines(rhs),
        _ => false,
    }
}

/// The hidden local `as` binds a boxed value to while it tests the value's
/// number class. Its name has a space, so source cannot name it.
const AS_VALUE_LOCAL: &str = "as value";

/// The type an `as` converts or casts its value to.
#[derive(Copy, Clone, Debug)]
enum AsTarget {
    /// A class, as `as(<double-float>, x)` names.
    Class(ClassId),
    /// `<i32>` or `<u32>`.
    Limited(LimitedInteger),
    /// `limited(<integer>, ...)`, or every `<integer>` for `None`.
    Range(Option<IntRange>),
}

/// The hidden local a `select` binds its target to when the target is not
/// already a local, suffixed with where the `select` starts. Its name has a
/// space, so source cannot name it.
const SELECT_TARGET_LOCAL: &str = "select target";

/// The hidden locals a division such as `floor/` binds its operands to,
/// suffixed with where the call starts. Their names have a space, so source
/// cannot name them.
const DIVISION_DIVIDEND_LOCAL: &str = "division dividend";
const DIVISION_DIVISOR_LOCAL: &str = "division divisor";

/// The kernel-only name a division's lowering calls for one of its
/// results, such as `%floor/-quotient`. A `%` cannot appear in a source
/// name, so no program can call or shadow it.
fn integer_division_name(function: IntegerDivision) -> String {
    let part = if function.remainder {
        "remainder"
    } else {
        "quotient"
    };
    format!("%{}-{part}", function.rounding.name())
}

/// The hidden local `copy-sequence` and `starts-with?` bind their string
/// to, suffixed with where the call starts. Its name has a space, so source
/// cannot name it.
const TEXT_STRING_LOCAL: &str = "text string";

/// The hidden local a `select` binds a `by` test that is not a name to.
const SELECT_TEST_LOCAL: &str = "select test";

/// The refinement `boxed == literal` proves where it is true: a boxed
/// `<object>` local identical to an integer or boolean literal holds that
/// scalar, so it unboxes.
fn identical_refinement(lhs: &Expr, rhs: &Expr) -> Option<Refinement> {
    [(lhs, rhs), (rhs, lhs)]
        .into_iter()
        .find_map(|(value, literal)| {
            let Expr::Local {
                local,
                result_kind: ValueKind::Any,
                ..
            } = value
            else {
                return None;
            };
            let kind = match literal {
                Expr::ConstI64 { .. }
                | Expr::Const32 {
                    value: Number32::I32(_) | Number32::U32(_),
                    ..
                } => ValueKind::I64,
                Expr::ConstBool { .. } => ValueKind::Bool,
                Expr::ConstChar { .. } => ValueKind::Char,
                _ => return None,
            };
            Some(Refinement {
                local: *local,
                kind,
            })
        })
}

/// An expression producing no values, as a conditional without a default
/// body does when no test is true.
fn no_values(span: Span) -> Expr {
    Expr::Values {
        values: Vec::new(),
        result_kinds: Vec::new(),
        span,
    }
}

/// The results to lower `expr` to where its values are discarded, as in a
/// statement: none for a conditional or a handle, which then discards each
/// arm's values, so the arms need not agree, and lowers each arm the same
/// way; no expectation for anything else, whose values are dropped as they
/// are. Only these forms take the empty expectation, since elsewhere it
/// means the expression must produce no values, which would, say, type a
/// block's exit or an untyped call as taking none.
fn effect_expectation(expr: &ast::Expr) -> Option<&'static [ValueKind]> {
    matches!(
        expr,
        ast::Expr::If { .. }
            | ast::Expr::Case { .. }
            | ast::Expr::Select { .. }
            | ast::Expr::Handle { .. }
    )
    .then_some(&[])
}

/// Evaluates `expr` for its effects and produces no values.
fn discard_values(lowering: &Lowering, expr: Expr) -> Expr {
    if lowering.expr_kinds(&expr).is_empty() {
        return expr;
    }
    Expr::Discard {
        span: expr.span(),
        value: Box::new(expr),
    }
}

fn scalar_result_kind(kinds: &[ValueKind]) -> ValueKind {
    match kinds {
        [kind] => *kind,
        _ => ValueKind::Error,
    }
}

fn provisional_open_result_kinds() -> Vec<ValueKind> {
    vec![ValueKind::I64]
}

fn result_kinds_contain_error(kinds: &[ValueKind]) -> bool {
    kinds.iter().any(|kind| matches!(kind, ValueKind::Error))
}

#[derive(Clone, Debug)]
struct ConstantSymbol {
    name: String,
    id: ConstantId,
    kind: ValueKind,
    callable_signature: Option<CallableSignature>,
    annotation: Option<ValueKind>,
    /// The range a `limited(<integer>, ...)` annotation declares, which
    /// the initializer is checked against.
    range: Option<IntRange>,
    span: Span,
}

#[derive(Clone, Debug)]
struct GenericSymbol {
    name: String,
    id: GenericFunctionId,
    param_kinds: Vec<GenericParamKind>,
    param_ranges: Vec<Option<IntRange>>,
    return_kinds: Vec<ValueKind>,
    return_callable_sigs: Vec<Option<CallableSignature>>,
    return_ranges: Vec<Option<IntRange>>,
    return_annotation: Option<Vec<ValueKind>>,
    dispatch_param: usize,
    span: Span,
}

impl GenericSymbol {
    fn arity(&self) -> usize {
        self.param_kinds.len()
    }
}

/// How one result of a call through a `<callable>` value, or of a body, is
/// checked back to the kind its consumer relies on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CallableResultCheck {
    /// The value's register type establishes the kind: once lowering has
    /// unboxed it, for a callable value's number or boolean result.
    Trusted(ValueKind),
    /// An `<integer>` result is converted, with a range check, to a limited
    /// integer.
    Convert(NumericKind),
    /// An object result is cast to the class.
    Cast(ClassId),
    /// An `<integer>` result is checked to be in the range.
    Range(IntRange),
    /// A value of kind `from` is cast, as a declaration narrows it, to the
    /// class, and then checked to be in the range when it is limited.
    Narrow {
        from: ValueKind,
        class: ClassId,
        range: Option<IntRange>,
    },
}

impl CallableResultCheck {
    /// Returns the kind the call itself produces, before the check.
    fn wire_kind(&self) -> ValueKind {
        match *self {
            Self::Trusted(kind) => kind,
            Self::Convert(_) | Self::Range(_) => ValueKind::I64,
            Self::Cast(_) => ValueKind::Object(OBJECT_CLASS_ID),
            Self::Narrow { from, .. } => from,
        }
    }

    /// Wraps `value`, of [`Self::wire_kind`], in its conversion or cast: an
    /// ordinary check site soft typing records and may later prove.
    fn apply(self, value: Expr, span: Span) -> Expr {
        match self {
            Self::Trusted(_) => value,
            Self::Convert(to) => Expr::Convert {
                site: SiteId::PENDING,
                to,
                value: Box::new(value),
                explicit: false,
                span,
            },
            Self::Cast(class) => Expr::Cast {
                value: Box::new(value),
                class,
                site: SiteId::PENDING,
                explicit: false,
                span,
            },
            Self::Range(range) => Expr::CheckRange {
                value: Box::new(value),
                range,
                site: SiteId::PENDING,
                explicit: false,
                span,
            },
            Self::Narrow { class, range, .. } => {
                let cast = Self::Cast(class).apply(value, span);
                match range {
                    Some(range) => Self::Range(range).apply(cast, span),
                    None => cast,
                }
            }
        }
    }
}

struct FunctionContext {
    locals: Vec<Local>,
    visible_locals: Vec<bool>,
    capture_sources: Vec<Local>,
    captures: Vec<Capture>,
    active_restarts: Vec<ActiveRestart>,
    /// Definitions of the block exit procedures bound in this body or, for
    /// a closure, an enclosing one.
    exit_definitions: Vec<DefinitionId>,
    /// The `let` locals and parameters, which `:=` may assign.
    assignable: Vec<LocalId>,
    /// Names of the definition's `let` locals and parameters that live in
    /// shared cells ([`ast::shared_local_names`]); closures inherit them.
    shared_names: Vec<String>,
    /// Shared enclosing locals a closure body assigns through its captures.
    assigned_captures: Vec<LocalId>,
    /// Definitions of the `for` loop variables whose bodies are being
    /// lowered, which are bound as `let`s but which `:=` may not assign.
    loop_variables: Vec<DefinitionId>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveRestart {
    specializer: ClassId,
    result_kinds: Vec<ValueKind>,
    result_callable_sigs: Vec<Option<CallableSignature>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum FinalizeState {
    Unvisited,
    Visiting,
    Done,
}

impl FunctionContext {
    fn new() -> Self {
        Self {
            locals: Vec::new(),
            visible_locals: Vec::new(),
            capture_sources: Vec::new(),
            captures: Vec::new(),
            active_restarts: Vec::new(),
            exit_definitions: Vec::new(),
            assignable: Vec::new(),
            shared_names: Vec::new(),
            assigned_captures: Vec::new(),
            loop_variables: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn for_closure(capture_sources: Vec<Local>, exit_definitions: Vec<DefinitionId>) -> Self {
        Self {
            locals: Vec::new(),
            visible_locals: Vec::new(),
            capture_sources,
            captures: Vec::new(),
            active_restarts: Vec::new(),
            exit_definitions,
            assignable: Vec::new(),
            shared_names: Vec::new(),
            assigned_captures: Vec::new(),
            loop_variables: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// Whether `target` names a block's exit procedure: the local a `block`
    /// bound it to, or a closure's capture of that local.
    fn is_block_exit(&self, target: &Expr) -> bool {
        let definition = match target {
            Expr::Local { local, .. } => self
                .locals
                .get(local.0 as usize)
                .map(|local| local.definition),
            Expr::Capture { capture, .. } => {
                self.captures.get(capture.0 as usize).and_then(|capture| {
                    self.capture_sources
                        .iter()
                        .find(|source| source.name == capture.name)
                        .map(|source| source.definition)
                })
            }
            _ => None,
        };
        definition.is_some_and(|definition| self.exit_definitions.contains(&definition))
    }

    fn add_local(
        &mut self,
        definition: DefinitionId,
        name: String,
        kind: ValueKind,
        callable_signature: Option<CallableSignature>,
        span: Span,
    ) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        if name == keywords::UNSUPPLIED {
            self.diagnostics.push(reserved_name(&name, span));
        }
        self.locals.push(Local {
            id,
            definition,
            name,
            kind,
            callable_signature,
            range: None,
            assigned: false,
            shared: false,
            span,
        });
        self.visible_locals.push(true);
        id
    }

    /// Declares that `local` holds only values in `range`; see
    /// [`Local::range`].
    fn limit_local(&mut self, local: LocalId, range: Option<IntRange>) {
        if let Some(local) = self.locals.get_mut(local.0 as usize) {
            local.range = range;
        }
    }

    /// Lets `:=` assign `local`, a `let` local or a parameter, unless it is
    /// a `for` loop variable. It lives in a shared cell when its name is
    /// one of [`Self::shared_names`].
    fn allow_assignment(&mut self, local: LocalId) {
        let loop_variable = self
            .locals
            .get(local.0 as usize)
            .is_some_and(|binding| self.loop_variables.contains(&binding.definition));
        if loop_variable {
            return;
        }
        self.assignable.push(local);
        let shared = self
            .locals
            .get(local.0 as usize)
            .is_some_and(|binding| self.shared_names.contains(&binding.name));
        if let Some(binding) = self.locals.get_mut(local.0 as usize) {
            binding.shared = shared;
        }
    }

    /// Records that `:=` assigns `local`.
    fn note_assignment(&mut self, local: LocalId) {
        if let Some(binding) = self.locals.get_mut(local.0 as usize) {
            binding.assigned = true;
        }
    }

    fn find_local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .enumerate()
            .filter(|(index, _)| self.visible_locals.get(*index).copied().unwrap_or(false))
            .map(|(_, local)| local)
            .find(|local| local.name == name)
            .map(|local| local.id)
    }

    /// Whether `name` is a visible local or an enclosing local this closure
    /// could capture.
    fn binds(&self, name: &str) -> bool {
        self.find_local(name).is_some()
            || self.captures.iter().any(|capture| capture.name == name)
            || self
                .capture_sources
                .iter()
                .any(|source| source.name == name)
    }

    fn visible_local_sources(&self) -> Vec<Local> {
        self.locals
            .iter()
            .enumerate()
            .filter(|(index, _)| self.visible_locals.get(*index).copied().unwrap_or(false))
            .map(|(_, local)| local.clone())
            .collect()
    }

    /// Whether `name` would capture a visible enclosing `<callable>` local.
    /// Whether `name` is a local of this body or one it may capture, which
    /// shadows any module-level definition of the name.
    fn binds_lexically(&self, name: &str) -> bool {
        self.find_local(name).is_some()
            || self.captures.iter().any(|capture| capture.name == name)
            || self
                .capture_sources
                .iter()
                .any(|source| source.name == name)
    }

    fn find_capture_or_add(&mut self, name: &str) -> Option<CaptureId> {
        if let Some(capture) = self.captures.iter().find(|capture| capture.name == name) {
            return Some(capture.id);
        }
        let source = self
            .capture_sources
            .iter()
            .find(|source| source.name == name)?
            .clone();
        let id = CaptureId(u32::try_from(self.captures.len()).unwrap_or(u32::MAX));
        self.captures.push(Capture {
            id,
            source: source.id,
            name: source.name,
            kind: source.kind,
            callable_signature: source.callable_signature,
            range: source.range,
            shared: source.shared,
            span: source.span,
        });
        Some(id)
    }

    fn capture_kind(&self, id: CaptureId) -> ValueKind {
        self.captures
            .get(id.0 as usize)
            .map_or(ValueKind::Error, |capture| capture.kind)
    }

    fn capture_callable_signature(&self, id: CaptureId) -> Option<CallableSignature> {
        self.captures
            .get(id.0 as usize)
            .and_then(|capture| capture.callable_signature.clone())
    }

    fn local_kind(&self, id: LocalId) -> ValueKind {
        self.locals
            .get(id.0 as usize)
            .map_or(ValueKind::Error, |local| local.kind)
    }

    fn local_callable_signature(&self, id: LocalId) -> Option<CallableSignature> {
        self.locals
            .get(id.0 as usize)
            .and_then(|local| local.callable_signature.clone())
    }

    fn refine_local(&mut self, id: LocalId, kind: ValueKind) -> Option<ValueKind> {
        let local = self.locals.get_mut(id.0 as usize)?;
        let previous = local.kind;
        local.kind = kind;
        Some(previous)
    }

    fn scope_start(&self) -> usize {
        self.locals.len()
    }

    fn hide_locals_from(&mut self, start: usize) {
        for visible in self.visible_locals.iter_mut().skip(start) {
            *visible = false;
        }
    }

    fn push_restart(&mut self, restart: ActiveRestart) {
        self.active_restarts.push(restart);
    }

    fn pop_restart(&mut self) {
        self.active_restarts.pop();
    }

    /// Innermost lexically enclosing restart that accepts `class`.
    fn matching_restart(&self, class: ClassId, module: &Lowering) -> Option<ActiveRestart> {
        self.active_restarts
            .iter()
            .rev()
            .find(|restart| module.is_same_or_subclass(class, restart.specializer))
            .cloned()
    }
}

struct Lowering {
    source_file: SourceFileId,
    /// The keyword parameters of each function that takes them, by name.
    keyword_functions: Vec<(String, KeywordSignature)>,
    /// The keyword parameters of each generic that takes them, by name.
    keyword_generics: Vec<(String, KeywordSignature)>,
    host_registry: HostMetaRegistry,
    host_functions: Vec<ResolvedHostFunction>,
    classes: Vec<Class>,
    generics: Vec<GenericFunction>,
    methods: Vec<Method>,
    functions: Vec<FunctionSymbol>,
    closures: Vec<Closure>,
    constants: Vec<Constant>,
    constant_symbols: Vec<ConstantSymbol>,
    generic_symbols: Vec<GenericSymbol>,
    /// Method specializers from the latest complete lowering pass. Method
    /// bodies are lowered one at a time, so `methods` is partial while they
    /// lower; boxed-parameter decisions read this snapshot instead.
    dispatch_specializers: Vec<(GenericFunctionId, Vec<MethodSpecializer>)>,
    /// Built-in numeric generics created so far, in creation order. A
    /// generic is created the first time an operator needs dispatch and
    /// persists across lowering passes; its methods are regenerated with
    /// every pass.
    numeric_generics: Vec<(promotion::NumericOperator, GenericFunctionId)>,
    /// Built-in collection generics created so far, in creation order. A
    /// generic is created the first time a call or a method names it, and
    /// persists across lowering passes; its kernel methods are regenerated
    /// with every pass.
    collection_generics: Vec<(CollectionFunction, GenericFunctionId)>,
    /// How many functions the program defines: the hidden kernel functions
    /// of [`Self::built_in_functions`] follow them, and no call of a name
    /// finds those.
    program_functions: usize,
    /// The hidden kernel functions single-overload built-ins named as
    /// values are, by built-in name, created the first time a value names
    /// one. They persist across lowering passes, since their bodies depend
    /// on nothing the program defines.
    built_in_functions: Vec<(String, Function)>,
    /// The built-in generics built-ins with several overloads are, by
    /// built-in name, created the first time a value, a method or a call
    /// typing cannot resolve names one. Each persists across lowering
    /// passes; its kernel methods are regenerated with every pass.
    built_in_generics: Vec<(String, GenericFunctionId)>,
    /// The generics slots' getters are methods of, by getter name: each
    /// created for its name, or the program's own generic of the name.
    slot_getters: Vec<(String, GenericFunctionId)>,
    /// The kernel library, parsed the first time a definition of it is
    /// needed; see the `library` module.
    library: Option<library::Library>,
    /// The kernel library's functions in the module, by name, created the
    /// first time a call or value names one. Each persists across
    /// lowering passes, and its body is lowered again with every pass
    /// ([`Self::lower_library_functions`]), since it may make closures.
    library_functions: Vec<(String, FunctionId)>,
    /// The library functions' bodies from the latest pass, parallel to
    /// [`Self::library_functions`].
    library_bodies: Vec<Function>,
    /// Whether the code being lowered is the kernel library's, which sees
    /// no definition of the program, only the kernel's.
    kernel_scope: bool,
    /// The names of the generics the program's own methods join, in
    /// source order, a name once for each method.
    program_method_names: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}

impl Lowering {
    fn new(
        module: &ast::Module,
        registry: &HostMetaRegistry,
        keyword_functions: Vec<(String, KeywordSignature)>,
        keyword_generics: Vec<(String, KeywordSignature)>,
    ) -> Self {
        let mut lowering = Self {
            source_file: module.source_file,
            keyword_functions,
            keyword_generics,
            host_registry: registry.clone(),
            host_functions: Vec::new(),
            classes: Vec::new(),
            generics: Vec::new(),
            methods: Vec::new(),
            functions: Vec::new(),
            closures: Vec::new(),
            constants: Vec::new(),
            constant_symbols: Vec::new(),
            generic_symbols: Vec::new(),
            dispatch_specializers: Vec::new(),
            numeric_generics: Vec::new(),
            collection_generics: Vec::new(),
            program_functions: module.functions.len(),
            built_in_functions: Vec::new(),
            built_in_generics: Vec::new(),
            slot_getters: Vec::new(),
            library: None,
            library_functions: Vec::new(),
            library_bodies: Vec::new(),
            kernel_scope: false,
            program_method_names: module
                .methods
                .iter()
                .map(|method| method.name.clone())
                .collect(),
            diagnostics: module.diagnostics.clone(),
        };
        lowering.collect_classes(module);
        lowering.collect_host_functions();
        lowering.collect_generics(module);
        lowering.collect_functions(module);
        lowering.collect_generic_keywords();
        lowering.collect_constant_symbols(module);
        lowering.mark_slot_defaults(module);
        lowering.finalize_classes();
        lowering.collect_slot_setter_generics();
        lowering.collect_slot_getter_generics();
        lowering.check_host_function_conflicts();
        lowering
    }

    fn collect_host_functions(&mut self) {
        let declarations = self.host_registry.functions.clone();
        for (index, function) in declarations.into_iter().enumerate() {
            let id = HostFunctionId(u32::try_from(index).unwrap_or(u32::MAX));
            if self
                .host_functions
                .iter()
                .any(|candidate| candidate.name == function.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(Span::default(), DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "host function"))
                        .with_arg(DiagnosticArg::text("name", function.name)),
                );
                continue;
            }
            let param_kinds = function
                .param_kinds
                .iter()
                .copied()
                .map(|kind| self.resolve_host_value_kind(kind))
                .collect();
            let result_kind = self.resolve_host_value_kind(function.result_kind);
            self.host_functions.push(ResolvedHostFunction {
                id,
                name: function.name,
                param_kinds,
                result_kind,
            });
        }
    }

    fn resolve_host_value_kind(&self, kind: HostValueKind) -> ValueKind {
        match kind {
            HostValueKind::Integer => ValueKind::I64,
            HostValueKind::Float => ValueKind::F64,
            HostValueKind::I32 => ValueKind::I32,
            HostValueKind::U32 => ValueKind::U32,
            HostValueKind::SingleFloat => ValueKind::F32,
            HostValueKind::Boolean => ValueKind::Bool,
            HostValueKind::String => ValueKind::String(
                self.builtin_value_class(BuiltinValueClass::String)
                    .unwrap_or(ClassId(u32::MAX)),
            ),
        }
    }

    fn check_host_function_conflicts(&mut self) {
        let conflicts = self
            .host_functions
            .iter()
            .filter(|host| {
                intrinsic_for_name(&host.name).is_some()
                    || self.functions.iter().any(|item| item.name == host.name)
                    || self
                        .generic_symbols
                        .iter()
                        .any(|item| item.name == host.name)
                    || self
                        .constant_symbols
                        .iter()
                        .any(|item| item.name == host.name)
                    || self.classes.iter().any(|class| {
                        class
                            .effective_slots
                            .iter()
                            .any(|slot| slot.accessor.name == host.name)
                    })
            })
            .map(|host| host.name.clone())
            .collect::<Vec<_>>();
        for name in conflicts {
            self.diagnostics.push(
                Diagnostic::error(Span::default(), DiagnosticCode::DuplicateDefinition)
                    .with_arg(DiagnosticArg::text("kind", "host function"))
                    .with_arg(DiagnosticArg::text("name", name)),
            );
        }
    }

    fn collect_classes(&mut self, module: &ast::Module) {
        self.classes.push(Class {
            id: OBJECT_CLASS_ID,
            definition: DefinitionId::new(
                self.source_file,
                DefinitionKind::Class,
                "<object>",
                "object-root",
            ),
            name: "<object>".into(),
            origin: ClassOrigin::ObjectRoot,
            superclass: None,
            sealing: Sealing::Open,
            is_abstract: false,
            finalized: false,
            direct_slots: Vec::new(),
            effective_slots: Vec::new(),
            initialization_plan: InitializationPlan::default(),
            span: Span::default(),
        });

        for (index, class) in module.classes.iter().enumerate() {
            if let Some(previous) = self.classes.iter().find(|item| item.name == class.name) {
                self.diagnostics.push(
                    Diagnostic::error(class.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "class"))
                        .with_arg(DiagnosticArg::text("name", class.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let class_id = ClassId(u32::try_from(index + 1).unwrap_or(u32::MAX));
            self.classes.push(Class {
                id: class_id,
                definition: class.definition,
                name: class.name.clone(),
                origin: ClassOrigin::Native,
                superclass: Some(OBJECT_CLASS_ID),
                sealing: class.sealing,
                is_abstract: class.is_abstract,
                finalized: false,
                direct_slots: Vec::new(),
                effective_slots: Vec::new(),
                initialization_plan: InitializationPlan::default(),
                span: class.span,
            });
        }

        for (class_index, class) in module.classes.iter().enumerate() {
            let lowered_class_index = class_index + 1;
            let class_id = ClassId(u32::try_from(lowered_class_index).unwrap_or(u32::MAX));
            let slots = class
                .slots
                .iter()
                .enumerate()
                .map(|(slot_index, slot)| {
                    let id = SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX));
                    let resolved = self.resolve_function_type_detail(
                        slot.type_annotation.as_ref(),
                        ValueKind::Any,
                    );
                    let kind = resolved.kind;
                    Slot {
                        id,
                        definition: slot.definition,
                        name: slot.name.clone(),
                        type_name: slot
                            .type_annotation
                            .as_ref()
                            .map_or_else(|| "<object>".into(), ToString::to_string),
                        kind,
                        callable_signature: resolved.callable_signature,
                        range: resolved.range,
                        origin: SlotOrigin::Native,
                        accessor: Accessor {
                            definition: slot.definition,
                            name: slot.name.clone(),
                            class: class_id,
                            slot: id,
                            result_kind: kind,
                        },
                        init_keyword: slot
                            .init_keyword
                            .clone()
                            .unwrap_or_else(|| slot.name.clone()),
                        required_init_keyword: slot.required_init_keyword,
                        init_value: None,
                        initializer: None,
                        allocation: slot.allocation.into(),
                        mutability: slot.mutability.into(),
                        span: slot.span,
                    }
                })
                .collect::<Vec<_>>();
            self.check_duplicate_init_keywords(class, &slots);
            if let Some(lowered_class) = self.classes.get_mut(lowered_class_index) {
                lowered_class.direct_slots = slots;
            }
        }

        let host_classes = self.host_registry.classes.clone();
        for (host_index, host_class) in host_classes.iter().enumerate() {
            if let Some(previous) = self
                .classes
                .iter()
                .find(|item| item.name == host_class.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(Span::default(), DiagnosticCode::HostClassConflict)
                        .with_arg(DiagnosticArg::text("class", host_class.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let class_id = ClassId(u32::try_from(self.classes.len()).unwrap_or(u32::MAX));
            let host_class_id = HostClassId(u32::try_from(host_index).unwrap_or(u32::MAX));
            let class_definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Class,
                &host_class.name,
                "host-backed",
            );
            let slots = host_class
                .slots
                .iter()
                .enumerate()
                .map(|(slot_index, host_slot)| {
                    let id = SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX));
                    let host_slot_id = HostSlotId(u32::try_from(slot_index).unwrap_or(u32::MAX));
                    let definition = DefinitionId::new(
                        self.source_file,
                        DefinitionKind::Slot,
                        &host_slot.name,
                        &format!("host-backed:{}", host_class.name),
                    );
                    Slot {
                        id,
                        definition,
                        name: host_slot.name.clone(),
                        type_name: host_slot.type_name.clone(),
                        kind: host_slot.kind,
                        callable_signature: None,
                        range: None,
                        origin: SlotOrigin::HostBacked {
                            host_class: host_class_id,
                            host_slot: host_slot_id,
                        },
                        accessor: Accessor {
                            definition,
                            name: host_slot.name.clone(),
                            class: class_id,
                            slot: id,
                            result_kind: host_slot.kind,
                        },
                        init_keyword: host_slot.name.clone(),
                        required_init_keyword: false,
                        init_value: None,
                        initializer: None,
                        allocation: SlotAllocation::Instance,
                        mutability: SlotMutability::Mutable,
                        span: Span::default(),
                    }
                })
                .collect();
            self.classes.push(Class {
                id: class_id,
                definition: class_definition,
                name: host_class.name.clone(),
                origin: ClassOrigin::HostBacked {
                    host_class: host_class_id,
                },
                superclass: Some(OBJECT_CLASS_ID),
                sealing: Sealing::Open,
                is_abstract: false,
                finalized: false,
                direct_slots: slots,
                effective_slots: Vec::new(),
                initialization_plan: InitializationPlan::default(),
                span: Span::default(),
            });
        }

        self.collect_builtin_value_classes();
        self.collect_condition_classes();
        self.resolve_superclasses(module);
    }

    fn collect_builtin_value_classes(&mut self) {
        for (kind, name) in BuiltinValueClass::ALL {
            if let Some(previous) = self.classes.iter().find(|item| item.name == *name) {
                self.diagnostics.push(
                    Diagnostic::error(previous.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "class"))
                        .with_arg(DiagnosticArg::text("name", (*name).to_string()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let class_id = ClassId(u32::try_from(self.classes.len()).unwrap_or(u32::MAX));
            self.classes.push(Class {
                id: class_id,
                definition: DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Class,
                    name,
                    "builtin-value",
                ),
                name: (*name).to_string(),
                origin: ClassOrigin::BuiltinValue { kind: *kind },
                superclass: Some(OBJECT_CLASS_ID),
                // No source class can inherit from a built-in class, so a
                // concrete one's direct subclasses are the built-in ones it
                // has. Saying so for `<vector>`, which `<stretchy-vector>`
                // extends, lets typing resolve a send on a `<vector>` to the
                // kernel method both share; every other concrete built-in
                // is a leaf, closed already. `<list>` is Dylan's sealed
                // abstract class, exactly a `<pair>` or the empty list, so
                // typing reads it as those two pieces.
                sealing: if (!kind.is_abstract()
                    && BuiltinValueClass::ALL
                        .iter()
                        .any(|(other, _)| other.builtin_superclass() == Some(*kind)))
                    || *kind == BuiltinValueClass::List
                {
                    Sealing::Sealed
                } else {
                    Sealing::Open
                },
                // A `<list>` is always one of its two pieces: typing's cone
                // over it has no instances of its own.
                is_abstract: *kind == BuiltinValueClass::List,
                finalized: false,
                direct_slots: Vec::new(),
                effective_slots: Vec::new(),
                initialization_plan: InitializationPlan::default(),
                span: Span::default(),
            });
        }
        // `ALL` is in declaration order, not superclass order, so built-in
        // superclasses are linked once every built-in class exists.
        for (kind, _) in BuiltinValueClass::ALL {
            let (Some(class), Some(superclass)) = (
                self.builtin_value_class(*kind),
                kind.builtin_superclass()
                    .and_then(|parent| self.builtin_value_class(parent)),
            ) else {
                continue;
            };
            if let Some(class) = self.classes.get_mut(class.0 as usize) {
                class.superclass = Some(superclass);
            }
        }
    }

    /// Adds the kernel condition classes after the built-in value classes,
    /// so the ids of every other class stay as they were. Their slots are
    /// read-only, with required init keywords, so they add no setter
    /// generics.
    fn collect_condition_classes(&mut self) {
        for condition in ConditionClass::ALL {
            let name = condition.name();
            if let Some(previous) = self.classes.iter().find(|item| item.name == name) {
                self.diagnostics.push(
                    Diagnostic::error(previous.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "class"))
                        .with_arg(DiagnosticArg::text("name", name.to_string()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let class_id = ClassId(u32::try_from(self.classes.len()).unwrap_or(u32::MAX));
            let superclass = match condition.superclass() {
                Some(parent) => self.condition_class(parent),
                None => Some(OBJECT_CLASS_ID),
            };
            let slots = condition
                .slots()
                .iter()
                .enumerate()
                .map(|(index, slot)| {
                    let id = SlotId(u32::try_from(index).unwrap_or(u32::MAX));
                    let definition = DefinitionId::new(
                        self.source_file,
                        DefinitionKind::Slot,
                        slot.name,
                        &format!("kernel:{name}"),
                    );
                    let kind = self.resolve_type_name(slot.type_name, false, Span::default());
                    Slot {
                        id,
                        definition,
                        name: slot.name.into(),
                        type_name: slot.type_name.into(),
                        kind,
                        callable_signature: None,
                        range: None,
                        origin: SlotOrigin::Native,
                        accessor: Accessor {
                            definition,
                            name: slot.name.into(),
                            class: class_id,
                            slot: id,
                            result_kind: kind,
                        },
                        init_keyword: slot.init_keyword.into(),
                        required_init_keyword: true,
                        init_value: None,
                        initializer: None,
                        allocation: SlotAllocation::Instance,
                        mutability: SlotMutability::Constant,
                        span: Span::default(),
                    }
                })
                .collect();
            self.classes.push(Class {
                id: class_id,
                definition: DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Class,
                    name,
                    "kernel-condition",
                ),
                name: name.into(),
                origin: ClassOrigin::Condition { class: *condition },
                superclass,
                sealing: Sealing::Open,
                is_abstract: false,
                finalized: false,
                direct_slots: slots,
                effective_slots: Vec::new(),
                initialization_plan: InitializationPlan::default(),
                span: Span::default(),
            });
        }
    }

    /// The id of a kernel condition class. Source slot types resolve before
    /// the kernel classes are added after the built-in value classes, so
    /// until then the id is the one it will get.
    fn condition_class(&self, condition: ConditionClass) -> Option<ClassId> {
        self.classes
            .iter()
            .find(|class| class.origin == ClassOrigin::Condition { class: condition })
            .map(|class| class.id)
            .or_else(|| {
                let builtin = self.builtin_value_class(BuiltinValueClass::ALL.last()?.0)?;
                let position = ConditionClass::ALL
                    .iter()
                    .position(|class| *class == condition)?;
                let index = usize::try_from(builtin.0).ok()?.checked_add(1 + position)?;
                Some(ClassId(u32::try_from(index).ok()?))
            })
    }

    fn resolve_superclasses(&mut self, module: &ast::Module) {
        for (class_index, class) in module.classes.iter().enumerate() {
            let lowered_class_index = class_index + 1;
            let Some(superclass) = class.superclass.as_deref() else {
                continue;
            };
            if superclass == "<object>" {
                continue;
            }
            // `<sequence>` is the one built-in class a program extends: its
            // instances iterate through their own `size` and `element`.
            if matches!(superclass, "<integer>" | "<boolean>")
                || BuiltinValueClass::from_name(superclass)
                    .is_some_and(|class| class != BuiltinValueClass::Sequence)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        class.superclass_span,
                        DiagnosticCode::InvalidPrimitiveSuperclass,
                    )
                    .with_arg(DiagnosticArg::text("class", class.name.clone()))
                    .with_arg(DiagnosticArg::text("superclass", superclass)),
                );
                continue;
            }
            let Some((parent_id, parent_origin)) = self
                .classes
                .iter()
                .find(|candidate| candidate.name == superclass)
                .map(|parent| (parent.id, parent.origin))
            else {
                self.diagnostics.push(
                    Diagnostic::error(class.superclass_span, DiagnosticCode::UnresolvedSuperclass)
                        .with_arg(DiagnosticArg::text("superclass", superclass))
                        .with_arg(DiagnosticArg::text("class", class.name.clone())),
                );
                continue;
            };
            if matches!(parent_origin, ClassOrigin::HostBacked { .. }) {
                self.diagnostics.push(
                    Diagnostic::error(
                        class.superclass_span,
                        DiagnosticCode::InvalidHostBackedSuperclass,
                    )
                    .with_arg(DiagnosticArg::text("class", class.name.clone()))
                    .with_arg(DiagnosticArg::text("superclass", superclass)),
                );
                continue;
            }
            if let Some(lowered_class) = self.classes.get_mut(lowered_class_index) {
                lowered_class.superclass = Some(parent_id);
            }
        }
    }

    /// Gives every slot with a default its [`Expr::SlotDefault`], so class
    /// finalization and initialization plans see the default before
    /// [`Self::lower_slot_defaults`] lowers it, once methods are known.
    fn mark_slot_defaults(&mut self, module: &ast::Module) {
        for (class_index, class) in module.classes.iter().enumerate() {
            let lowered_class_index = class_index + 1;
            for (slot_index, slot) in class.slots.iter().enumerate() {
                let Some(init_value) = &slot.init_value else {
                    continue;
                };
                if let Some(lowered) = self
                    .classes
                    .get_mut(lowered_class_index)
                    .and_then(|class| class.direct_slots.get_mut(slot_index))
                {
                    lowered.init_value = Some(Expr::SlotDefault {
                        class: ClassId(u32::try_from(lowered_class_index).unwrap_or(u32::MAX)),
                        slot: SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX)),
                        result_kind: lowered.kind,
                        callable_signature: lowered.callable_signature.clone(),
                        span: init_value.span(),
                    });
                }
            }
        }
    }

    /// Lowers every slot default and hands each class's effective slots the
    /// default their declaring slot now has.
    fn lower_slot_defaults(&mut self, module: &ast::Module) {
        let mut lowered_values = Vec::new();
        for (class_index, class) in module.classes.iter().enumerate() {
            let lowered_class_index = class_index + 1;
            for (slot_index, slot) in class.slots.iter().enumerate() {
                let Some(init_value) = &slot.init_value else {
                    continue;
                };
                let mut ctx = FunctionContext::new();
                let (slot_kind, slot_signature, slot_range) = self
                    .classes
                    .get(lowered_class_index)
                    .and_then(|class| class.direct_slots.get(slot_index))
                    .map_or((ValueKind::Error, None, None), |slot| {
                        (slot.kind, slot.callable_signature.clone(), slot.range)
                    });
                ctx.shared_names = ast::shared_local_names_in(init_value);
                let value = self
                    .lower_method_against(&mut ctx, init_value, slot_signature.as_ref())
                    .unwrap_or_else(|| self.lower_expr(&mut ctx, init_value));
                let value = adapt_literal(value, Some(core::slice::from_ref(&slot_kind)));
                let value = self.narrow(value, slot_kind, slot_signature.as_ref());
                self.diagnostics.extend(ctx.diagnostics);
                let value_kind = self.expr_kind(&value);
                if !self.kinds_match(slot_kind, value_kind) {
                    self.diagnostics.push(
                        Diagnostic::error(
                            init_value.span(),
                            DiagnosticCode::SlotDefaultTypeMismatch,
                        )
                        .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", self.kind_name(slot_kind)))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(value_kind))),
                    );
                } else if let Some(expected) = &slot_signature {
                    let diagnostic = self.callable_value_diagnostic(
                        init_value.span(),
                        expected,
                        value_kind,
                        self.expr_callable_signature(&value).as_ref(),
                        |expected, actual| {
                            Diagnostic::error(
                                init_value.span(),
                                DiagnosticCode::SlotDefaultTypeMismatch,
                            )
                            .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                            .with_arg(DiagnosticArg::text("expected", expected))
                            .with_arg(DiagnosticArg::text("actual", actual))
                        },
                    );
                    self.diagnostics.extend(diagnostic);
                }
                // The default checks its value against the slot's range, as
                // `make` checks an init argument.
                let value = self.limit_value(value, slot_kind, slot_range);
                // A literal is read in place at each `make`; anything else
                // runs as code of its own, in a frame of its own.
                let (value, initializer) = if ctx.locals.is_empty() && default_inlines(&value) {
                    (value, None)
                } else {
                    let span = value.span();
                    let default = Expr::SlotDefault {
                        class: ClassId(u32::try_from(lowered_class_index).unwrap_or(u32::MAX)),
                        slot: SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX)),
                        result_kind: slot_kind,
                        callable_signature: slot_signature,
                        span,
                    };
                    let initializer = SlotInitializer {
                        value,
                        locals: ctx.locals,
                    };
                    (default, Some(initializer))
                };
                lowered_values.push((lowered_class_index, slot_index, value, initializer));
            }
        }

        for (class_index, slot_index, value, initializer) in lowered_values {
            if let Some(slot) = self
                .classes
                .get_mut(class_index)
                .and_then(|class| class.direct_slots.get_mut(slot_index))
            {
                slot.init_value = Some(value);
                slot.initializer = initializer;
            }
        }
        let defaults = self
            .classes
            .iter()
            .map(|class| {
                class
                    .direct_slots
                    .iter()
                    .map(|slot| slot.init_value.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for class in &mut self.classes {
            for slot in &mut class.effective_slots {
                if let Some(default) = defaults
                    .get(slot.source_class.0 as usize)
                    .and_then(|slots| slots.get(slot.source_slot.0 as usize))
                {
                    slot.init_value.clone_from(default);
                }
            }
        }
    }

    fn finalize_classes(&mut self) {
        let mut states = vec![FinalizeState::Unvisited; self.classes.len()];
        for class_index in 0..self.classes.len() {
            self.finalize_class(class_index, &mut states);
        }
    }

    fn finalize_class(&mut self, class_index: usize, states: &mut [FinalizeState]) {
        match states.get(class_index).copied() {
            Some(FinalizeState::Done) => return,
            Some(FinalizeState::Visiting) => {
                let class = &self.classes[class_index];
                self.diagnostics.push(
                    Diagnostic::error(class.span, DiagnosticCode::ClassInheritanceCycle)
                        .with_arg(DiagnosticArg::text("class", class.name.clone())),
                );
                return;
            }
            Some(FinalizeState::Unvisited) => {}
            None => return,
        }

        states[class_index] = FinalizeState::Visiting;
        let parent_slots = self.classes[class_index].superclass.and_then(|parent| {
            let parent_index = parent.0 as usize;
            self.finalize_class(parent_index, states);
            self.classes
                .get(parent_index)
                .map(|class| class.effective_slots.clone())
        });

        let owner = self.classes[class_index].id;
        let mut effective_slots = Vec::new();
        if let Some(parent_slots) = parent_slots {
            for slot in &parent_slots {
                let id = SlotId(u32::try_from(effective_slots.len()).unwrap_or(u32::MAX));
                effective_slots.push(Self::inherited_effective_slot(owner, id, slot));
            }
        }

        let direct_slots = self.classes[class_index].direct_slots.clone();
        for slot in &direct_slots {
            let id = SlotId(u32::try_from(effective_slots.len()).unwrap_or(u32::MAX));
            effective_slots.push(Self::direct_effective_slot(owner, id, slot));
        }

        self.check_duplicate_effective_slots(class_index, &effective_slots);
        let initialization_plan = Self::initialization_plan(owner, &effective_slots);
        if let Some(class) = self.classes.get_mut(class_index) {
            class.effective_slots = effective_slots;
            class.initialization_plan = initialization_plan;
            class.finalized = true;
        }
        states[class_index] = FinalizeState::Done;
    }

    fn initialization_plan(owner: ClassId, slots: &[EffectiveSlot]) -> InitializationPlan {
        InitializationPlan {
            class: owner,
            steps: slots
                .iter()
                .map(|slot| InitializationStep {
                    slot: slot.id,
                    source_class: slot.source_class,
                    source_slot: slot.source_slot,
                    init_keyword: slot.init_keyword.clone(),
                    kind: slot.kind,
                    type_name: slot.type_name.clone(),
                    inherited: slot.source_class != owner,
                    required_init_keyword: slot.required_init_keyword,
                    has_default: slot.init_value.is_some(),
                })
                .collect(),
        }
    }

    fn direct_effective_slot(owner: ClassId, id: SlotId, slot: &Slot) -> EffectiveSlot {
        EffectiveSlot {
            id,
            definition: slot.definition,
            name: slot.name.clone(),
            type_name: slot.type_name.clone(),
            kind: slot.kind,
            callable_signature: slot.callable_signature.clone(),
            range: slot.range,
            origin: slot.origin,
            source_class: owner,
            source_slot: slot.id,
            accessor: Accessor {
                definition: slot.accessor.definition,
                name: slot.accessor.name.clone(),
                class: owner,
                slot: id,
                result_kind: slot.accessor.result_kind,
            },
            init_keyword: slot.init_keyword.clone(),
            required_init_keyword: slot.required_init_keyword,
            init_value: slot.init_value.clone(),
            allocation: slot.allocation,
            mutability: slot.mutability,
            span: slot.span,
        }
    }

    fn inherited_effective_slot(owner: ClassId, id: SlotId, slot: &EffectiveSlot) -> EffectiveSlot {
        let mut inherited = slot.clone();
        inherited.id = id;
        inherited.accessor.class = owner;
        inherited.accessor.slot = id;
        inherited
    }

    fn check_duplicate_effective_slots(&mut self, class_index: usize, slots: &[EffectiveSlot]) {
        let class_name = self.classes[class_index].name.clone();
        for (index, slot) in slots.iter().enumerate() {
            if slots[..index]
                .iter()
                .any(|previous| previous.name == slot.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(slot.span, DiagnosticCode::DuplicateEffectiveSlot)
                        .with_arg(DiagnosticArg::text("class", class_name.clone()))
                        .with_arg(DiagnosticArg::text("slot", slot.name.clone())),
                );
            }
            if slots[..index]
                .iter()
                .any(|previous| previous.init_keyword == slot.init_keyword)
            {
                self.diagnostics.push(
                    Diagnostic::error(slot.span, DiagnosticCode::DuplicateEffectiveInitKeyword)
                        .with_arg(DiagnosticArg::text("class", class_name.clone()))
                        .with_arg(DiagnosticArg::text(
                            "keyword",
                            format!("{}:", slot.init_keyword),
                        )),
                );
            }
            if slots[..index]
                .iter()
                .any(|previous| previous.accessor.name == slot.accessor.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(slot.span, DiagnosticCode::DuplicateEffectiveAccessor)
                        .with_arg(DiagnosticArg::text("class", class_name.clone()))
                        .with_arg(DiagnosticArg::text("accessor", slot.accessor.name.clone())),
                );
            }
        }
    }

    fn check_duplicate_init_keywords(&mut self, class: &ast::Class, slots: &[Slot]) {
        for (index, slot) in slots.iter().enumerate() {
            if slots[..index]
                .iter()
                .any(|previous| previous.init_keyword == slot.init_keyword)
            {
                self.diagnostics.push(
                    Diagnostic::error(class.span, DiagnosticCode::DuplicateSlotInitKeyword)
                        .with_arg(DiagnosticArg::text("class", class.name.clone()))
                        .with_arg(DiagnosticArg::text(
                            "keyword",
                            format!("{}:", slot.init_keyword),
                        )),
                );
            }
        }
    }

    fn collect_generics(&mut self, module: &ast::Module) {
        for (index, generic) in module.generics.iter().enumerate() {
            if let Some(previous) = self
                .generic_symbols
                .iter()
                .find(|symbol| symbol.name == generic.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(generic.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "generic"))
                        .with_arg(DiagnosticArg::text("name", generic.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let id = GenericFunctionId(u32::try_from(index).unwrap_or(u32::MAX));
            let resolved_params = generic
                .params
                .iter()
                .map(|param| {
                    param
                        .type_annotation
                        .as_ref()
                        .map(|annotation| self.resolve_type_annotation_detail(annotation))
                })
                .collect::<Vec<_>>();
            let param_ranges = resolved_params
                .iter()
                .map(|resolved| resolved.as_ref().and_then(|resolved| resolved.range))
                .collect::<Vec<_>>();
            let param_kinds = resolved_params
                .into_iter()
                .map(|resolved| resolved.map(|resolved| resolved.kind))
                .collect::<Vec<_>>();
            for (param_index, kind) in param_kinds.iter().enumerate() {
                if let Some(kind) = kind
                    && !matches!(
                        kind,
                        ValueKind::Any
                            | ValueKind::Object(_)
                            | ValueKind::String(_)
                            | ValueKind::Symbol(_)
                            | ValueKind::I64
                            | ValueKind::F64
                            | ValueKind::I32
                            | ValueKind::U32
                            | ValueKind::F32
                            | ValueKind::Bool
                            | ValueKind::Char
                            | ValueKind::Position
                            | ValueKind::Vector(_)
                            | ValueKind::Callable
                            | ValueKind::Error
                    )
                    && let Some(param) = generic.params.get(param_index)
                    && let Some(annotation) = param.type_annotation.as_ref()
                {
                    // Methods specialize a generic's parameters on classes,
                    // so each must be declared as a type whose values have
                    // one: any class, scalars and `<function>` included.
                    self.diagnostics.push(
                        Diagnostic::error(annotation.span(), DiagnosticCode::AstAnnotationNotClass)
                            .with_arg(DiagnosticArg::text("position", "generic parameter"))
                            .with_arg(DiagnosticArg::text("name", param.name.clone()))
                            .with_arg(DiagnosticArg::text("type", annotation.to_string())),
                    );
                }
            }
            let DeclaredResults {
                kinds: return_annotation,
                callable_sigs: return_callable_sigs,
                ranges: return_ranges,
            } = self.resolve_declared_results(generic.return_type.as_ref());
            let return_kinds = return_annotation
                .clone()
                .unwrap_or_else(provisional_open_result_kinds);
            self.generic_symbols.push(GenericSymbol {
                name: generic.name.clone(),
                id,
                param_kinds: param_kinds.clone(),
                param_ranges: param_ranges.clone(),
                return_kinds: return_kinds.clone(),
                return_callable_sigs: return_callable_sigs.clone(),
                return_ranges: return_ranges.clone(),
                return_annotation: return_annotation.clone(),
                dispatch_param: 0,
                span: generic.span,
            });
            self.generics.push(GenericFunction {
                id,
                definition: generic.definition,
                name: generic.name.clone(),
                params: generic
                    .params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect(),
                param_kinds,
                arity: generic.params.len(),
                dispatch_param: 0,
                // Dispatch looks only at the required parameters, not at
                // keywords.
                dispatch_params: (0..self.keyword_start(&generic.name, generic.params.len()))
                    .collect(),
                param_ranges,
                result_kinds: return_kinds,
                result_callable_sigs: return_callable_sigs,
                result_ranges: return_ranges,
                return_annotation,
                span: generic.span,
                origin: GenericFunctionOrigin::Source,
                sealing: generic.sealing,
                strict: generic.strict,
                // Filled once the default functions are known.
                keywords: Vec::new(),
            });
        }
    }

    /// The classes' slots whose getters are methods of a getter generic:
    /// each class's own direct slots, as the class and its effective slot.
    /// A subclass reaches its superclass's slot through dispatch.
    fn getter_slots(&self) -> Vec<(ClassId, EffectiveSlot)> {
        self.classes
            .iter()
            .flat_map(|class| {
                class
                    .effective_slots
                    .iter()
                    .filter(move |slot| slot.source_class == class.id)
                    .map(move |slot| (class.id, slot.clone()))
            })
            .collect()
    }

    /// Joins each slot's getter to the program's own generic of its name,
    /// as in Dylan, so the generic reads the slot of each class declaring
    /// one. Any other getter's generic is made the first time something
    /// needs it ([`Self::slot_getter_named`]).
    fn collect_slot_getter_generics(&mut self) {
        let mut names = Vec::<String>::new();
        for (_, slot) in self.getter_slots() {
            if !names.contains(&slot.accessor.name) {
                names.push(slot.accessor.name);
            }
        }
        for name in names {
            // A slot named `size` joins the kernel's `size`, as a method of
            // a collection class of the program's does, when it holds an
            // `<integer>` as `size` answers.
            if CollectionFunction::from_name(&name) == Some(CollectionFunction::Size) {
                if !self.shadows_collection_generic(&name)
                    && self
                        .getter_slots()
                        .iter()
                        .any(|(_, slot)| slot.accessor.name == name && slot.kind == ValueKind::I64)
                {
                    let id = self.collection_generic(CollectionFunction::Size);
                    self.slot_getters.push((name, id));
                }
                continue;
            }
            let Some(symbol) = self
                .resolve_generic(&name)
                .filter(|_| CollectionFunction::from_name(&name).is_none())
                .cloned()
            else {
                continue;
            };
            // A getter takes the one receiver, so a generic of other
            // parameters cannot take it as a method.
            if symbol.arity() != 1 {
                for (_, slot) in self.getter_slots() {
                    if slot.accessor.name == name {
                        self.diagnostics.push(Self::getter_arity_mismatch(
                            &name,
                            symbol.arity(),
                            slot.span,
                        ));
                    }
                }
                continue;
            }
            self.slot_getters.push((name, symbol.id));
        }
    }

    /// The arity error of a slot whose getter's name a generic of
    /// `arity` parameters takes.
    fn getter_arity_mismatch(name: &str, arity: usize, span: Span) -> Diagnostic {
        Diagnostic::error(span, DiagnosticCode::GenericMethodArityMismatch)
            .with_arg(DiagnosticArg::text("method", name))
            .with_arg(DiagnosticArg::unsigned(
                "expected_count",
                u64::try_from(arity).unwrap_or(u64::MAX),
            ))
            .with_arg(DiagnosticArg::unsigned("actual_count", 1))
    }

    /// The generic the slot getter `name` is, made on first use, as in
    /// Dylan: its methods read the slots of that name of the classes
    /// declaring one, and it dispatches on any other receiver's class. A
    /// name a function, constant or host function of the program takes
    /// keeps its getter for receivers of the slots' classes only, and so
    /// does a built-in collection generic's name, whose kernel generic
    /// serves every other receiver, except that an `<integer>` slot named
    /// `size` joins the kernel's `size` ([`Self::collect_slot_getter_generics`]).
    /// `None` when no slot has the getter.
    fn slot_getter_named(&mut self, name: &str) -> Option<GenericFunctionId> {
        if self.kernel_scope {
            return None;
        }
        if let Some(generic) = self.slot_getter(name) {
            return Some(generic);
        }
        if !self.has_accessor(name)
            || CollectionFunction::from_name(name).is_some()
            || self.resolve_function(name).is_some()
            || self.resolve_host_function(name).is_some()
            || self.resolve_constant(name).is_some()
            || self.resolve_generic(name).is_some()
        {
            return None;
        }
        let slots = self
            .getter_slots()
            .into_iter()
            .filter(|(_, slot)| slot.accessor.name == name)
            .map(|(_, slot)| slot)
            .collect::<Vec<_>>();
        // A callable result keeps the signature the slots agree on.
        let signature = slots
            .first()
            .and_then(|first| first.callable_signature.clone())
            .filter(|signature| {
                slots
                    .iter()
                    .all(|slot| slot.callable_signature.as_ref() == Some(signature))
            });
        let shared = slots
            .iter()
            .map(|slot| (slot.kind, slot.range, slot.span))
            .collect::<Vec<_>>();
        let (first, _, span) = *shared.first()?;
        // The result is the slots' kind where they agree, and an
        // `<object>` where they do not, in the hull of their ranges.
        let kind = if shared.iter().all(|(kind, _, _)| *kind == first) {
            first
        } else {
            ValueKind::Any
        };
        let hull = shared
            .iter()
            .map(|(_, range, _)| *range)
            .reduce(|left, right| left.zip(right).map(|(left, right)| left.hull(right)))
            .flatten()
            .filter(|_| kind == ValueKind::I64);
        let id = GenericFunctionId(u32::try_from(self.generics.len()).unwrap_or(u32::MAX));
        let definition = DefinitionId::new(
            self.source_file,
            DefinitionKind::Generic,
            name,
            "slot-getter",
        );
        self.generic_symbols.push(GenericSymbol {
            name: name.into(),
            id,
            param_kinds: vec![None],
            param_ranges: vec![None],
            return_kinds: vec![kind],
            return_callable_sigs: vec![signature.clone()],
            return_ranges: vec![hull],
            return_annotation: Some(vec![kind]),
            dispatch_param: 0,
            span,
        });
        self.generics.push(GenericFunction {
            id,
            definition,
            name: name.into(),
            params: vec!["object".into()],
            param_kinds: vec![None],
            param_ranges: vec![None],
            arity: 1,
            dispatch_param: 0,
            dispatch_params: vec![0],
            result_kinds: vec![kind],
            result_callable_sigs: vec![signature],
            result_ranges: vec![hull],
            return_annotation: Some(vec![kind]),
            span,
            origin: GenericFunctionOrigin::SlotGetter { generic: id },
            sealing: Sealing::Open,
            strict: false,
            keywords: Vec::new(),
        });
        self.slot_getters.push((name.into(), id));
        Some(id)
    }

    /// Why the getter of `slot`, declared by `class`, cannot be a method of
    /// `generic`, whose contract a program may declare: its receiver
    /// parameter must admit `class`, and its result must admit what the
    /// slot holds, its range and callable signature included.
    fn getter_contract_mismatch(
        &self,
        generic: GenericFunctionId,
        class: ClassId,
        slot: &EffectiveSlot,
    ) -> Option<Diagnostic> {
        let symbol = self.generic_symbols.get(generic.0 as usize)?;
        let name = slot.accessor.name.clone();
        if let Some(expected) = symbol.param_kinds.first().copied().flatten()
            && !self.generic_specializer_matches(expected, Some(class))
        {
            return Some(
                Diagnostic::error(
                    slot.span,
                    DiagnosticCode::GenericMethodSpecializerTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("method", name))
                .with_arg(DiagnosticArg::unsigned("argument_index", 1))
                .with_arg(DiagnosticArg::text(
                    "actual",
                    self.method_specializer_label(Some(class)),
                ))
                .with_arg(DiagnosticArg::text("expected", self.kind_name(expected))),
            );
        }
        let expected = symbol.return_annotation.clone()?;
        let range_holds = match (symbol.return_ranges.first().copied().flatten(), slot.range) {
            (None, _) => true,
            (Some(declared), Some(held)) => declared.includes(held),
            (Some(_), None) => false,
        };
        let signature_holds = match symbol.return_callable_sigs.first().cloned().flatten() {
            None => true,
            Some(declared) => {
                self.callable_signatures_match(Some(&declared), slot.callable_signature.as_ref())
            }
        };
        if self.result_kinds_match(&expected, &[slot.kind]) && range_holds && signature_holds {
            return None;
        }
        let declared = match (
            symbol.return_ranges.first().copied().flatten(),
            symbol.return_callable_sigs.first().cloned().flatten(),
        ) {
            (Some(range), _) => limited_type_name(range),
            (None, Some(signature)) => self.callable_signature_name(&signature),
            (None, None) => self.result_kinds_name(&expected),
        };
        Some(
            Diagnostic::error(slot.span, DiagnosticCode::GenericMethodResultTypeMismatch)
                .with_arg(DiagnosticArg::text("method", name))
                .with_arg(DiagnosticArg::text("actual", slot.type_name.clone()))
                .with_arg(DiagnosticArg::text("expected", declared)),
        )
    }

    /// Whether values of `kind` are of a class with a slot whose getter is
    /// `name`, declared or inherited.
    fn declares_slot(&self, kind: ValueKind, name: &str) -> bool {
        self.value_kind_class(kind).is_some_and(|class| {
            self.classes.get(class.0 as usize).is_some_and(|class| {
                class
                    .effective_slots
                    .iter()
                    .any(|slot| slot.accessor.name == name)
            })
        })
    }

    /// The getter generic named `name`, if slots' getters are its methods.
    fn slot_getter(&self, name: &str) -> Option<GenericFunctionId> {
        self.slot_getters
            .iter()
            .find(|(getter, _)| getter == name)
            .map(|(_, id)| *id)
    }

    /// Generates each slot's getter method on its getter generic: a read of
    /// the slot of a receiver of its class.
    fn collect_slot_getter_methods(&mut self) {
        for (class, slot) in self.getter_slots() {
            let Some(generic) = self.slot_getter(&slot.accessor.name) else {
                continue;
            };
            // A kernel collection generic takes only a getter of its result
            // type.
            if self
                .collection_generics
                .iter()
                .any(|(_, id)| *id == generic)
                && slot.kind != ValueKind::I64
            {
                continue;
            }
            // A program's generic declares what its methods take and
            // return, the getter's included.
            if let Some(diagnostic) = self.getter_contract_mismatch(generic, class, &slot) {
                self.diagnostics.push(diagnostic);
                continue;
            }
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Method,
                &slot.accessor.name,
                &format!("slot-getter:{}", slot.definition.raw()),
            );
            let mut ctx = FunctionContext::new();
            let object = ctx.add_local(
                self.local_definition(definition, "object"),
                "object".into(),
                ValueKind::Object(class),
                None,
                slot.span,
            );
            let body = Block {
                statements: Vec::new(),
                result: Expr::AccessorCall {
                    accessor: slot.accessor.definition,
                    receiver: Box::new(Expr::Local {
                        local: object,
                        result_kind: ValueKind::Object(class),
                        callable_signature: None,
                        span: slot.span,
                    }),
                    class,
                    slot: slot.id,
                    result_kind: slot.accessor.result_kind,
                    span: slot.span,
                },
                span: slot.span,
            };
            self.methods.push(Method {
                id,
                definition,
                generic,
                specializer: class,
                specializers: vec![Some(class)],
                param: object,
                params: vec![object],
                locals: ctx.locals,
                body,
                result_kinds: vec![slot.kind],
                result_ranges: vec![slot.range],
                return_annotation: Some(vec![slot.kind]),
                span: slot.span,
                origin: MethodOrigin::SlotGetter {
                    class,
                    slot: slot.id,
                },
                strict: false,
            });
        }
    }

    fn collect_slot_setter_generics(&mut self) {
        let slots = self
            .classes
            .iter()
            .filter(|class| matches!(class.origin, ClassOrigin::Native))
            .flat_map(|class| {
                class.effective_slots.iter().filter_map(move |slot| {
                    if slot.source_class != class.id
                        || !matches!(slot.origin, SlotOrigin::Native)
                        || !matches!(slot.mutability, SlotMutability::Mutable)
                    {
                        return None;
                    }
                    Some((
                        class.id,
                        slot.id,
                        slot.definition,
                        slot.name.clone(),
                        (slot.kind, slot.range),
                        slot.span,
                    ))
                })
            })
            .collect::<Vec<_>>();

        for (class, slot, slot_definition, slot_name, (slot_kind, _), slot_span) in &slots {
            let (class, slot, slot_definition, slot_kind, slot_span) =
                (*class, *slot, *slot_definition, *slot_kind, *slot_span);
            let setter_name = slot_setter_name(slot_name);
            if self
                .generic_symbols
                .iter()
                .any(|symbol| symbol.name == setter_name)
            {
                continue;
            }
            // Every class declaring a slot of this name shares the setter
            // generic, and each one's setter method checks the value
            // against its own slot's range unless the send already did. A
            // send checks a range only when every such slot declares it, and
            // a send's result is in the hull of their ranges.
            let ranges = slots
                .iter()
                .filter(|other| other.3 == *slot_name)
                .map(|other| other.4.1)
                .collect::<Vec<_>>();
            let shared = ranges
                .iter()
                .all(|range| *range == ranges[0])
                .then_some(ranges[0])
                .flatten();
            let hull = ranges
                .iter()
                .copied()
                .reduce(|left, right| left.zip(right).map(|(left, right)| left.hull(right)))
                .flatten();
            let id = GenericFunctionId(u32::try_from(self.generics.len()).unwrap_or(u32::MAX));
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Generic,
                &setter_name,
                &format!("slot-setter:{}", slot_definition.raw()),
            );
            // The new value is the slots' kind where they agree, and an
            // `<object>` each method checks for its own where they do not.
            let value_kind = if slots
                .iter()
                .filter(|other| other.3 == *slot_name)
                .all(|other| other.4.0 == slot_kind)
            {
                slot_kind
            } else {
                ValueKind::Any
            };
            let param_kinds = vec![Some(value_kind), Some(ValueKind::Any)];
            let origin = GenericFunctionOrigin::SlotSetter { class, slot };
            let param_ranges = vec![shared, None];
            self.generic_symbols.push(GenericSymbol {
                name: setter_name.clone(),
                id,
                param_kinds: param_kinds.clone(),
                param_ranges: param_ranges.clone(),
                return_kinds: vec![value_kind],
                return_callable_sigs: vec![None],
                return_ranges: vec![hull],
                return_annotation: Some(vec![value_kind]),
                dispatch_param: 1,
                span: slot_span,
            });
            self.generics.push(GenericFunction {
                id,
                definition,
                name: setter_name,
                params: vec!["new-value".into(), "object".into()],
                param_kinds,
                param_ranges,
                arity: 2,
                dispatch_param: 1,
                dispatch_params: vec![0, 1],
                result_kinds: vec![value_kind],
                result_callable_sigs: vec![None],
                result_ranges: vec![hull],
                return_annotation: Some(vec![value_kind]),
                span: slot_span,
                origin,
                sealing: Sealing::Open,
                strict: false,
                keywords: Vec::new(),
            });
        }
    }

    /// Returns the built-in generic for `operator`, creating it on first use.
    fn numeric_generic(&mut self, operator: promotion::NumericOperator) -> GenericFunctionId {
        if let Some((_, id)) = self
            .numeric_generics
            .iter()
            .find(|(existing, _)| *existing == operator)
        {
            return *id;
        }
        let id = GenericFunctionId(u32::try_from(self.generics.len()).unwrap_or(u32::MAX));
        let name = operator.name();
        let arity = operator.arity();
        let result_kinds = vec![match operator {
            promotion::NumericOperator::Compare(_) => ValueKind::Bool,
            promotion::NumericOperator::Arithmetic(_) | promotion::NumericOperator::Negate => {
                ValueKind::Any
            }
        }];
        self.generic_symbols.push(GenericSymbol {
            name: name.into(),
            id,
            param_kinds: vec![None; arity],
            param_ranges: vec![None; arity],
            return_kinds: result_kinds.clone(),
            return_callable_sigs: vec![None],
            return_ranges: vec![None],
            return_annotation: Some(result_kinds.clone()),
            dispatch_param: 0,
            span: Span::default(),
        });
        self.generics.push(GenericFunction {
            id,
            definition: DefinitionId::new(
                self.source_file,
                DefinitionKind::Generic,
                name,
                "numeric",
            ),
            name: name.into(),
            params: ["x", "y"]
                .iter()
                .take(arity)
                .map(|name| (*name).into())
                .collect(),
            param_kinds: vec![None; arity],
            param_ranges: vec![None; arity],
            arity,
            dispatch_param: 0,
            dispatch_params: (0..arity).collect(),
            result_kinds: result_kinds.clone(),
            result_callable_sigs: vec![None],
            result_ranges: vec![None],
            return_annotation: Some(result_kinds),
            span: Span::default(),
            origin: GenericFunctionOrigin::Numeric { operator },
            sealing: Sealing::Open,
            strict: false,
            keywords: Vec::new(),
        });
        self.numeric_generics.push((operator, id));
        id
    }

    /// Lowers an operator whose operands are not all statically numbers to
    /// a call of its built-in numeric generic.
    fn numeric_dispatch(
        &mut self,
        operator: promotion::NumericOperator,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        Expr::GenericCall {
            site: SiteId::PENDING,
            generic: self.numeric_generic(operator),
            args,
            span,
        }
    }

    /// Returns whether an operator on operands of these kinds dispatches
    /// through a numeric generic: every operand is a number, `<object>`, or
    /// an instance of a non-built-in class, and at least one is not a
    /// number.
    fn dispatches_numerically(kinds: &[ValueKind]) -> bool {
        kinds.iter().all(|kind| {
            NumericKind::of(*kind).is_some()
                || matches!(kind, ValueKind::Any | ValueKind::Object(_))
        }) && kinds.iter().any(|kind| NumericKind::of(*kind).is_none())
    }

    /// Generates the methods of every numeric generic created so far, one
    /// per operand-kind combination the promotion rules define.
    fn collect_numeric_methods(&mut self) {
        for (operator, generic) in self.numeric_generics.clone() {
            for operand_kinds in operator.method_operands() {
                let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
                let signature = operand_kinds
                    .iter()
                    .map(|kind| match kind {
                        NumericKind::Integer => "integer",
                        NumericKind::Float => "float",
                        NumericKind::SingleFloat => "single-float",
                        NumericKind::I32 => "i32",
                        NumericKind::U32 => "u32",
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let definition = DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Method,
                    operator.name(),
                    &format!("numeric:{signature}"),
                );
                let mut ctx = FunctionContext::new();
                let mut params = Vec::new();
                let mut specializers = Vec::new();
                let mut operands = Vec::new();
                for (index, kind) in operand_kinds.iter().copied().enumerate() {
                    let name = if index == 0 { "x" } else { "y" };
                    let local = ctx.add_local(
                        self.local_definition(definition, name),
                        name.into(),
                        kind.value_kind(),
                        None,
                        Span::default(),
                    );
                    params.push(local);
                    specializers.push(self.value_kind_class(kind.value_kind()));
                    operands.push((
                        kind,
                        Expr::Local {
                            local,
                            result_kind: kind.value_kind(),
                            callable_signature: None,
                            span: Span::default(),
                        },
                    ));
                }
                let result = numeric_method_body(operator, operands);
                let result_kinds = vec![self.expr_kind(&result)];
                self.methods.push(Method {
                    id,
                    definition,
                    generic,
                    specializer: specializers
                        .first()
                        .copied()
                        .flatten()
                        .unwrap_or(OBJECT_CLASS_ID),
                    specializers,
                    param: params.first().copied().unwrap_or(LocalId(u32::MAX)),
                    params,
                    locals: ctx.locals,
                    body: Block {
                        statements: Vec::new(),
                        result,
                        span: Span::default(),
                    },
                    result_ranges: vec![None; result_kinds.len()],
                    result_kinds: result_kinds.clone(),
                    return_annotation: Some(result_kinds),
                    span: Span::default(),
                    origin: MethodOrigin::Numeric { operator },
                    strict: false,
                });
            }
            if operator == promotion::NumericOperator::Compare(CompareOp::Eq) {
                self.collect_equality_methods(generic);
            }
        }
    }

    /// Adds `=`'s methods beyond numbers, as Dylan's `=` has them: on any
    /// two objects it is `==`, two strings compare their text and two
    /// characters their scalar values. So `a = b` answers a boolean for
    /// any objects and never signals.
    fn collect_equality_methods(&mut self, generic: GenericFunctionId) {
        let operator = promotion::NumericOperator::Compare(CompareOp::Eq);
        let kinds = [
            (OBJECT_CLASS_ID, ValueKind::Any, "object"),
            (
                self.builtin_value_class(BuiltinValueClass::String)
                    .unwrap_or(OBJECT_CLASS_ID),
                ValueKind::String(
                    self.builtin_value_class(BuiltinValueClass::String)
                        .unwrap_or(OBJECT_CLASS_ID),
                ),
                "string",
            ),
            (
                self.builtin_value_class(BuiltinValueClass::Character)
                    .unwrap_or(OBJECT_CLASS_ID),
                ValueKind::Char,
                "character",
            ),
        ];
        for (class, kind, label) in kinds {
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Method,
                operator.name(),
                &format!("equality:{label},{label}"),
            );
            let span = Span::default();
            let mut ctx = FunctionContext::new();
            let params = ["x", "y"]
                .iter()
                .map(|name| {
                    ctx.add_local(
                        self.local_definition(definition, name),
                        (*name).into(),
                        kind,
                        None,
                        span,
                    )
                })
                .collect::<Vec<_>>();
            let read = |local: LocalId| {
                Box::new(Expr::Local {
                    local,
                    result_kind: kind,
                    callable_signature: None,
                    span,
                })
            };
            let result = match kind {
                ValueKind::Any => Expr::Identical {
                    lhs: read(params[0]),
                    rhs: read(params[1]),
                    span,
                },
                ValueKind::Char => Expr::IntrinsicCall {
                    intrinsic: Intrinsic::CharCompare(CompareOp::Eq),
                    args: vec![*read(params[0]), *read(params[1])],
                    result_kind: ValueKind::Bool,
                    span,
                },
                _ => Expr::ValueEqual {
                    lhs: read(params[0]),
                    rhs: read(params[1]),
                    kind,
                    span,
                },
            };
            self.methods.push(Method {
                id,
                definition,
                generic,
                specializer: class,
                specializers: vec![Some(class), Some(class)],
                param: params[0],
                params,
                locals: ctx.locals,
                body: Block {
                    statements: Vec::new(),
                    result,
                    span,
                },
                result_ranges: vec![None],
                result_kinds: vec![ValueKind::Bool],
                return_annotation: Some(vec![ValueKind::Bool]),
                span,
                origin: MethodOrigin::Numeric { operator },
                strict: false,
            });
        }
    }

    fn method_specializer_for_kind(&self, kind: ValueKind) -> MethodSpecializer {
        Some(self.value_kind_class(kind).unwrap_or(OBJECT_CLASS_ID))
    }

    fn collect_slot_setter_methods(&mut self) {
        let slots = self
            .classes
            .iter()
            .filter(|class| matches!(class.origin, ClassOrigin::Native))
            .flat_map(|class| {
                class.effective_slots.iter().filter_map(move |slot| {
                    if slot.source_class != class.id
                        || !matches!(slot.origin, SlotOrigin::Native)
                        || !matches!(slot.mutability, SlotMutability::Mutable)
                    {
                        return None;
                    }
                    Some((class.id, slot.clone()))
                })
            })
            .collect::<Vec<_>>();

        for (class, slot) in slots {
            let setter_name = slot_setter_name(&slot.name);
            let Some((generic, sent, returned)) =
                self.resolve_generic(&setter_name).map(|symbol| {
                    (
                        symbol.id,
                        symbol.param_ranges.first().copied().flatten(),
                        symbol.return_ranges.first().copied().flatten(),
                    )
                })
            else {
                continue;
            };
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Method,
                &setter_name,
                &format!("slot-setter:{}", slot.definition.raw()),
            );
            let mut ctx = FunctionContext::new();
            let value_param = ctx.add_local(
                DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Local,
                    "new-value",
                    &format!("slot-setter:{}:value", slot.definition.raw()),
                ),
                "new-value".into(),
                slot.kind,
                None,
                slot.span,
            );
            // The send checked the new value against the generic's range,
            // which other classes' slots of this name may not share.
            ctx.limit_local(value_param, sent);
            let object_param = ctx.add_local(
                DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Local,
                    "object",
                    &format!("slot-setter:{}:object", slot.definition.raw()),
                ),
                "object".into(),
                ValueKind::Object(class),
                None,
                slot.span,
            );
            let value = Expr::Local {
                local: value_param,
                result_kind: slot.kind,
                callable_signature: None,
                span: slot.span,
            };
            // The method writes only values in its own slot's range, so it
            // checks one the send did not already confine to it.
            let checked = match (slot.range, sent) {
                (Some(own), Some(sent)) if own.includes(sent) => None,
                (own, _) => own,
            };
            let value = self.limit_value(value, slot.kind, checked);
            let mut body = Block {
                statements: Vec::new(),
                result: Expr::SlotWrite {
                    setter: definition,
                    receiver: Box::new(Expr::Local {
                        local: object_param,
                        result_kind: ValueKind::Object(class),
                        callable_signature: None,
                        span: slot.span,
                    }),
                    value: Box::new(value),
                    class,
                    slot: slot.id,
                    result_kind: slot.kind,
                    span: slot.span,
                },
                span: slot.span,
            };
            // It returns the value it wrote, which a declared result range
            // of the generic may not hold.
            let written = slot.range.or(sent);
            let result_check = returned
                .filter(|returned| !written.is_some_and(|written| returned.includes(written)));
            let result_kinds = self.check_result_ranges(
                &mut ctx,
                &mut body,
                &[slot.kind],
                &[None],
                &[result_check],
            );
            self.methods.push(Method {
                id,
                definition,
                generic,
                specializer: class,
                specializers: vec![self.method_specializer_for_kind(slot.kind), Some(class)],
                param: object_param,
                params: vec![value_param, object_param],
                locals: ctx.locals,
                body,
                result_kinds,
                result_ranges: vec![returned.or(slot.range)],
                return_annotation: Some(vec![slot.kind]),
                span: slot.span,
                origin: MethodOrigin::SlotSetter {
                    class,
                    slot: slot.id,
                },
                strict: false,
            });
        }
    }

    fn check_ambiguous_generic_methods(&mut self) {
        let mut reported = Vec::new();
        for generic in &self.generics {
            let methods = self
                .methods
                .iter()
                .filter(|method| method.generic == generic.id)
                .collect::<Vec<_>>();
            for (left_index, left) in methods.iter().enumerate() {
                for right in methods.iter().skip(left_index + 1) {
                    // A method only a kernel collection method could be
                    // ambiguous with overrides it, which
                    // `check_collection_method_overrides` reports instead.
                    if [left, right]
                        .iter()
                        .any(|method| matches!(method.origin, MethodOrigin::Collection { .. }))
                    {
                        continue;
                    }
                    // A method specializing on a class that did not resolve
                    // was reported there; what it overlaps is unknown.
                    if [left, right].iter().any(|method| {
                        method
                            .specializers
                            .iter()
                            .any(|class| is_unresolved_specializer(*class))
                    }) {
                        continue;
                    }
                    let Some(actuals) = self.intersection_actuals(left, right) else {
                        continue;
                    };
                    let MethodSelection::Ambiguous = select_method_for_classes_in_methods(
                        &self.classes,
                        &self.methods,
                        generic.id,
                        &actuals,
                    ) else {
                        continue;
                    };
                    let key = format!(
                        "{}:{}",
                        generic.id.0,
                        actuals
                            .iter()
                            .map(|class| class.0.to_string())
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    if reported.iter().any(|seen| seen == &key) {
                        continue;
                    }
                    reported.push(key);
                    let candidates = self
                        .methods
                        .iter()
                        .filter(|method| {
                            method.generic == generic.id
                                && method_applies_to_classes_in_classes(
                                    &self.classes,
                                    method,
                                    &actuals,
                                )
                                && !self.methods.iter().any(|other| {
                                    other.generic == generic.id
                                        && other.id != method.id
                                        && method_applies_to_classes_in_classes(
                                            &self.classes,
                                            other,
                                            &actuals,
                                        )
                                        && method_more_specific_in_classes(
                                            &self.classes,
                                            other,
                                            method,
                                        )
                                })
                        })
                        .map(|method| format!("m{}", method.id.0))
                        .collect::<Vec<_>>()
                        .join(",");
                    self.diagnostics.push(
                        Diagnostic::error(
                            left.span,
                            DiagnosticCode::AmbiguousGenericMethodDispatch,
                        )
                        .with_arg(DiagnosticArg::text("generic", generic.name.clone()))
                        .with_arg(DiagnosticArg::text(
                            "actuals",
                            self.actual_tuple_label(&actuals),
                        ))
                        .with_arg(DiagnosticArg::text("candidates", candidates)),
                    );
                }
            }
        }
    }

    fn intersection_actuals(&self, left: &Method, right: &Method) -> Option<Vec<ClassId>> {
        if left.specializers.len() != right.specializers.len() {
            return None;
        }
        let mut actuals = Vec::new();
        for (left, right) in left.specializers.iter().zip(&right.specializers) {
            let actual = match (*left, *right) {
                (Some(left), Some(right)) if left == right => left,
                (Some(left), Some(right)) if self.is_same_or_subclass(left, right) => left,
                (Some(left), Some(right)) if self.is_same_or_subclass(right, left) => right,
                (Some(_), Some(_)) => return None,
                (Some(class), None) | (None, Some(class)) => class,
                (None, None) => self
                    .classes
                    .first()
                    .map_or(ClassId(u32::MAX), |class| class.id),
            };
            actuals.push(actual);
        }
        Some(actuals)
    }

    fn actual_tuple_label(&self, actuals: &[ClassId]) -> String {
        format!(
            "({})",
            actuals
                .iter()
                .map(|class| self.method_specializer_label(Some(*class)))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn collect_functions(&mut self, module: &ast::Module) {
        for (index, function) in module.functions.iter().enumerate() {
            // A default function is duplicated with its owner, already
            // reported.
            if !keywords::is_default_function(&function.name)
                && let Some(previous) = self
                    .functions
                    .iter()
                    .find(|symbol| symbol.name == function.name)
            {
                self.diagnostics.push(
                    Diagnostic::error(function.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "function"))
                        .with_arg(DiagnosticArg::text("name", function.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let symbol = self.function_symbol(
                FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                function,
            );
            self.functions.push(symbol);
        }
    }

    /// The symbol of `function`, whose id is `id`: its parameters' and
    /// declared results' types.
    fn function_symbol(&mut self, id: FunctionId, function: &ast::Function) -> FunctionSymbol {
        let mut params = function
            .params
            .iter()
            .map(|param| {
                self.resolve_function_type_detail(param.type_annotation.as_ref(), ValueKind::Any)
            })
            .collect::<Vec<_>>();
        if function.rest.is_some() {
            params.push(self.rest_param_type());
        }
        let param_callable_sigs = params
            .iter()
            .map(|param| param.callable_signature.clone())
            .collect::<Vec<_>>();
        let param_ranges = params.iter().map(|param| param.range).collect();
        let param_kinds = params
            .into_iter()
            .map(|param| param.kind)
            .collect::<Vec<_>>();
        let DeclaredResults {
            kinds: return_annotation,
            callable_sigs: return_callable_sigs,
            ranges: return_ranges,
        } = self.resolve_declared_results(function.return_type.as_ref());
        let return_kinds = return_annotation
            .clone()
            .unwrap_or_else(provisional_open_result_kinds);
        FunctionSymbol {
            name: function.name.clone(),
            id,
            param_kinds,
            rest: function.rest.is_some(),
            param_callable_sigs,
            param_ranges,
            return_kinds,
            return_callable_sigs,
            return_ranges,
            return_annotation,
            span: function.span,
        }
    }

    fn collect_constant_symbols(&mut self, module: &ast::Module) {
        for (index, constant) in module.constants.iter().enumerate() {
            if constant.name == keywords::UNSUPPLIED {
                self.diagnostics
                    .push(reserved_name(&constant.name, constant.span));
            }
            if let Some(previous) = self.resolve_constant(&constant.name) {
                self.diagnostics.push(
                    Diagnostic::error(constant.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "constant"))
                        .with_arg(DiagnosticArg::text("name", constant.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            if let Some(previous) = self.resolve_function(&constant.name) {
                self.diagnostics.push(
                    Diagnostic::error(constant.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "constant"))
                        .with_arg(DiagnosticArg::text("name", constant.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            if let Some(previous) = self.resolve_generic(&constant.name) {
                self.diagnostics.push(
                    Diagnostic::error(constant.span, DiagnosticCode::DuplicateDefinition)
                        .with_arg(DiagnosticArg::text("kind", "constant"))
                        .with_arg(DiagnosticArg::text("name", constant.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_start",
                            u64::try_from(previous.span.start).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "previous_end",
                            u64::try_from(previous.span.end).unwrap_or(u64::MAX),
                        )),
                );
            }
            let id = ConstantId(u32::try_from(index).unwrap_or(u32::MAX));
            let annotation = constant
                .type_annotation
                .as_ref()
                .map(|annotation| self.resolve_type_annotation_detail(annotation));
            self.constant_symbols.push(ConstantSymbol {
                name: constant.name.clone(),
                id,
                kind: annotation
                    .as_ref()
                    .map_or(ValueKind::Error, |annotation| annotation.kind),
                callable_signature: annotation
                    .as_ref()
                    .and_then(|annotation| annotation.callable_signature.clone()),
                range: annotation.as_ref().and_then(|annotation| annotation.range),
                annotation: annotation.map(|annotation| annotation.kind),
                span: constant.span,
            });
        }
    }

    /// Reports each cycle of constants whose initializers read one another,
    /// such as `$a = $b + 1` and `$b = $a + 1`: none of them could finish
    /// initializing. Only reads the initializers make themselves count,
    /// with those of the slot defaults their `make`s run; one made through
    /// a function call fails when it runs instead.
    fn report_constant_cycles(&mut self) {
        let reads = self
            .constants
            .iter()
            .map(|constant| self.constant_reads(&constant.value))
            .collect::<Vec<_>>();
        // Depth-first from each constant in turn: a constant met again while
        // its own reads are being followed closes a cycle.
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            New,
            Open,
            Done,
        }
        let mut marks = vec![Mark::New; self.constants.len()];
        let mut reported = Vec::new();
        for root in 0..self.constants.len() {
            if marks[root] != Mark::New {
                continue;
            }
            marks[root] = Mark::Open;
            let mut path = vec![(root, 0_usize)];
            while let Some(&mut (index, ref mut next)) = path.last_mut() {
                let Some(read) = reads[index].get(*next).copied() else {
                    marks[index] = Mark::Done;
                    path.pop();
                    continue;
                };
                *next += 1;
                match marks.get(read) {
                    Some(Mark::New) => {
                        marks[read] = Mark::Open;
                        path.push((read, 0));
                    }
                    Some(Mark::Open) if !reported.contains(&read) => {
                        let start = path.iter().position(|(open, _)| *open == read).unwrap_or(0);
                        let cycle = path[start..]
                            .iter()
                            .map(|(open, _)| *open)
                            .collect::<Vec<_>>();
                        reported.extend(cycle.iter().copied());
                        self.report_constant_cycle(&cycle);
                    }
                    _ => {}
                }
            }
        }
    }

    /// The constants `value` reads, by index, itself or in the code of a
    /// slot default it runs.
    fn constant_reads(&self, value: &Expr) -> Vec<usize> {
        let mut reads = Vec::new();
        let mut defaults: Vec<(ClassId, SlotId)> = Vec::new();
        let mut visit = |expr: &Expr, defaults: &mut Vec<(ClassId, SlotId)>| match expr {
            Expr::ConstantRef { constant, .. } if !reads.contains(&(constant.0 as usize)) => {
                reads.push(constant.0 as usize);
            }
            Expr::SlotDefault { class, slot, .. } if !defaults.contains(&(*class, *slot)) => {
                defaults.push((*class, *slot));
            }
            _ => {}
        };
        visit_evaluated(value, &mut |expr| visit(expr, &mut defaults));
        let mut next = 0;
        while let Some(&(class, slot)) = defaults.get(next) {
            next += 1;
            let Some(initializer) = self
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.direct_slots.get(slot.0 as usize))
                .and_then(|slot| slot.initializer.as_ref())
            else {
                continue;
            };
            let mut found = Vec::new();
            visit_evaluated(&initializer.value, &mut |expr| visit(expr, &mut found));
            for default in found {
                if !defaults.contains(&default) {
                    defaults.push(default);
                }
            }
        }
        reads
    }

    fn report_constant_cycle(&mut self, cycle: &[usize]) {
        let names = cycle
            .iter()
            .chain(cycle.first())
            .filter_map(|index| self.constants.get(*index))
            .map(|constant| format!("`{}`", constant.name))
            .collect::<Vec<_>>()
            .join(" -> ");
        let Some(first) = cycle.first().and_then(|index| self.constants.get(*index)) else {
            return;
        };
        self.diagnostics.push(
            Diagnostic::error(first.span, DiagnosticCode::ConstantInitializerCycle)
                .with_arg(DiagnosticArg::text("constant", first.name.clone()))
                .with_arg(DiagnosticArg::text("cycle", names)),
        );
    }

    fn lower_constants(&mut self, module: &ast::Module) {
        self.constants.clear();
        for (index, constant) in module.constants.iter().enumerate() {
            let id = ConstantId(u32::try_from(index).unwrap_or(u32::MAX));
            let mut ctx = FunctionContext::new();
            let expected_kind = self
                .constant_symbols
                .get(index)
                .and_then(|symbol| symbol.annotation);
            let expected_callable_signature = self
                .constant_symbols
                .get(index)
                .and_then(|symbol| symbol.callable_signature.clone());
            ctx.shared_names = ast::shared_local_names_in(&constant.value);
            let value = self
                .lower_method_against(
                    &mut ctx,
                    &constant.value,
                    expected_callable_signature.as_ref(),
                )
                .unwrap_or_else(|| {
                    self.lower_expr_expected(&mut ctx, &constant.value, expected_kind)
                });
            let value = match expected_kind {
                Some(expected) => {
                    self.narrow(value, expected, expected_callable_signature.as_ref())
                }
                None => value,
            };
            self.diagnostics.extend(ctx.diagnostics);
            let actual_kind = self.expr_kind(&value);
            let actual_callable_signature = self.expr_callable_signature(&value);
            if matches!(actual_kind, ValueKind::Error)
                || (matches!(actual_kind, ValueKind::KnownCallable(_))
                    && !matches!(expected_kind, Some(ValueKind::Callable)))
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        constant.value.span(),
                        DiagnosticCode::ConstantInitializerTypeMismatch,
                    )
                    .with_arg(DiagnosticArg::text("constant", constant.name.clone()))
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(actual_kind))),
                );
            }
            if let Some(expected) = expected_kind
                && !self.kinds_match(expected, actual_kind)
            {
                self.diagnostics.push(
                    Diagnostic::error(constant.value.span(), DiagnosticCode::ConstantTypeMismatch)
                        .with_arg(DiagnosticArg::text("constant", constant.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", self.kind_name(expected)))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual_kind))),
                );
            }
            if let Some(expected) = &expected_callable_signature
                && !self
                    .callable_signatures_match(Some(expected), actual_callable_signature.as_ref())
            {
                let diagnostic = self
                    .callable_variance_diagnostic(
                        constant.value.span(),
                        expected,
                        actual_callable_signature.as_ref(),
                    )
                    .unwrap_or_else(|| {
                        Diagnostic::error(
                            constant.value.span(),
                            DiagnosticCode::ConstantTypeMismatch,
                        )
                        .with_arg(DiagnosticArg::text("constant", constant.name.clone()))
                        .with_arg(DiagnosticArg::text(
                            "expected",
                            self.callable_signature_name(expected),
                        ))
                        .with_arg(DiagnosticArg::text(
                            "actual",
                            actual_callable_signature.as_ref().map_or_else(
                                || self.kind_name(actual_kind),
                                |signature| self.callable_signature_name(signature),
                            ),
                        ))
                    });
                self.diagnostics.push(diagnostic);
            }
            let result_kind = expected_kind.unwrap_or(actual_kind);
            let range = self
                .constant_symbols
                .get(index)
                .and_then(|symbol| symbol.range);
            let value = self.limit_value(value, result_kind, range);
            let callable_signature = expected_callable_signature.or(match actual_kind {
                ValueKind::Callable => actual_callable_signature,
                _ => None,
            });
            if let Some(symbol) = self.constant_symbols.get_mut(index) {
                symbol.kind = result_kind;
                symbol.callable_signature = callable_signature.clone();
            }
            self.constants.push(Constant {
                id,
                definition: constant.definition,
                name: constant.name.clone(),
                value,
                locals: ctx.locals,
                result_kind,
                callable_signature,
                strict: constant.strict,
                span: constant.span,
            });
        }
    }

    fn lower_module(&mut self, module: &ast::Module) -> Module {
        let diagnostic_len = self.diagnostics.len();
        let closure_len = self.closures.len();
        self.infer_open_result_signatures(module, diagnostic_len, closure_len);
        self.diagnostics.truncate(diagnostic_len);
        self.closures.truncate(closure_len);
        self.snapshot_dispatch_specializers();
        self.methods.clear();
        for method in &module.methods {
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let method = self.lower_method(id, method);
            self.methods.push(method);
        }
        self.collect_slot_setter_methods();
        self.collect_slot_getter_methods();
        self.collect_numeric_methods();
        self.collect_collection_methods();
        self.collect_built_in_methods();
        self.refresh_inferred_generic_results();
        self.check_ambiguous_generic_methods();
        // After the methods, so a default's or an initializer's sends find
        // them, and before the code whose `make`s take the defaults.
        self.lower_slot_defaults(module);
        self.lower_constants(module);
        self.report_constant_cycles();

        let mut functions = module
            .functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                self.lower_function(
                    FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                    function,
                )
            })
            .collect::<Vec<_>>();
        self.lower_library_functions();
        // The hidden kernel functions, the values built-ins named as values
        // are and the kernel library's, follow, in the order their ids were
        // given.
        let mut kernel = self
            .built_in_functions
            .iter()
            .map(|(_, function)| function.clone())
            .chain(self.library_bodies.iter().cloned())
            .collect::<Vec<_>>();
        kernel.sort_by_key(|function| function.id.0);
        functions.extend(kernel);

        let mut lowered = Module {
            source_file: module.source_file,
            classes: self.classes.clone(),
            generics: self.generics.clone(),
            methods: self.methods.clone(),
            functions,
            closures: self.closures.clone(),
            constants: self.constants.clone(),
            host_functions: self.host_functions.clone(),
            diagnostics: self.diagnostics.clone(),
        };
        check_collection_method_overrides(&mut lowered);
        check_slot_getter_overrides(&mut lowered);
        // A keyword's default function restates its owner's parameter types,
        // so a problem with one is found once per default: report it once.
        let mut seen = Vec::with_capacity(lowered.diagnostics.len());
        lowered.diagnostics.retain(|diagnostic| {
            let first = !seen.contains(diagnostic);
            if first {
                seen.push(diagnostic.clone());
            }
            first
        });
        // Numbered once the bodies are final, so the result-inference
        // fixpoint above cannot shift them.
        assign_check_sites(&mut lowered);
        lowered
    }

    fn infer_open_result_signatures(
        &mut self,
        module: &ast::Module,
        diagnostic_len: usize,
        closure_len: usize,
    ) {
        let max_passes = module
            .methods
            .len()
            .saturating_add(module.functions.len())
            .saturating_add(self.generics.len())
            // A constant read before it is declared takes its kind a pass
            // after the constant it reads.
            .saturating_add(module.constants.len())
            .saturating_add(2);
        for _ in 0..max_passes {
            let before = self.result_signature_snapshot();
            self.diagnostics.truncate(diagnostic_len);
            self.closures.truncate(closure_len);
            self.snapshot_dispatch_specializers();
            self.methods.clear();

            for method in &module.methods {
                let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
                let method = self.lower_method(id, method);
                self.methods.push(method);
            }
            self.collect_slot_setter_methods();
            self.collect_slot_getter_methods();
            self.collect_numeric_methods();
            self.collect_collection_methods();
            self.collect_built_in_methods();
            self.refresh_inferred_generic_results();
            self.lower_slot_defaults(module);
            self.lower_constants(module);
            for (index, function) in module.functions.iter().enumerate() {
                let _ = self.lower_function(
                    FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                    function,
                );
            }
            self.lower_library_functions();

            if self.result_signature_snapshot() == before {
                break;
            }
        }
    }

    fn result_signature_snapshot(&self) -> Vec<Vec<ValueKind>> {
        self.generic_symbols
            .iter()
            .map(|symbol| symbol.return_kinds.clone())
            .chain(
                self.functions
                    .iter()
                    .map(|symbol| symbol.return_kinds.clone()),
            )
            .chain(self.constant_symbols.iter().map(|symbol| vec![symbol.kind]))
            .collect()
    }

    fn refresh_inferred_generic_results(&mut self) {
        let mut updates = Vec::new();
        for generic in &self.generics {
            if generic.return_annotation.is_some() {
                continue;
            }
            let mut inferred: Option<Vec<ValueKind>> = None;
            for method in self
                .methods
                .iter()
                .filter(|method| method.generic == generic.id)
            {
                let Some(current) = &inferred else {
                    inferred = Some(method.result_kinds.clone());
                    continue;
                };
                let Some(joined) = self.join_result_kinds(current, &method.result_kinds) else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            method.body.result.span(),
                            DiagnosticCode::GenericMethodResultTypeMismatch,
                        )
                        .with_arg(DiagnosticArg::text("method", generic.name.clone()))
                        .with_arg(DiagnosticArg::text(
                            "actual",
                            self.result_kinds_name(&method.result_kinds),
                        ))
                        .with_arg(DiagnosticArg::text(
                            "expected",
                            self.result_kinds_name(current),
                        )),
                    );
                    break;
                };
                inferred = Some(joined);
            }
            if let Some(result_kinds) = inferred {
                updates.push((generic.id, result_kinds));
            }
        }

        for (id, result_kinds) in updates {
            if let Some(generic) = self.generics.get_mut(id.0 as usize) {
                generic.result_kinds = result_kinds.clone();
                generic.result_callable_sigs = vec![None; result_kinds.len()];
            }
            if let Some(symbol) = self.generic_symbols.get_mut(id.0 as usize) {
                symbol.return_kinds = result_kinds.clone();
                symbol.return_callable_sigs = vec![None; result_kinds.len()];
            }
        }
    }

    fn join_result_kinds(
        &self,
        current: &[ValueKind],
        next: &[ValueKind],
    ) -> Option<Vec<ValueKind>> {
        if current.len() != next.len() {
            return None;
        }
        current
            .iter()
            .copied()
            .zip(next.iter().copied())
            .map(|(current, next)| self.join_result_kind(current, next))
            .collect()
    }

    /// Joins one inferred result position across two methods: the method
    /// kind that holds the other's values, as an `if` joins its branches,
    /// else the top category the two share.
    fn join_result_kind(&self, current: ValueKind, next: ValueKind) -> Option<ValueKind> {
        if current == ValueKind::Error {
            return Some(next);
        }
        if next == ValueKind::Error {
            return Some(current);
        }
        if let Some(joined) = join_kind(&self.classes, current, next, |expected, actual| {
            self.kinds_match(expected, actual)
        }) {
            return Some(joined);
        }
        if self.is_callable_kind(current) && self.is_callable_kind(next) {
            return Some(ValueKind::Callable);
        }
        if !self.is_callable_kind(current) && !self.is_callable_kind(next) {
            return Some(ValueKind::Any);
        }
        None
    }

    fn is_callable_kind(&self, kind: ValueKind) -> bool {
        matches!(kind, ValueKind::Callable | ValueKind::KnownCallable(_))
    }

    fn lower_method(&mut self, id: MethodId, method: &ast::Method) -> Method {
        let mut ctx = FunctionContext::new();
        ctx.shared_names = ast::shared_local_names(&method.body);
        // A method may be the first to name a built-in collection generic,
        // or the generic a built-in function with several overloads is.
        let _ = self.collection_generic_named(&method.name);
        let _ = self.built_in_generic_named(&method.name);
        let _ = self.slot_getter_named(&method.name);
        // A kernel generic the compiler's own code made, as `c[k]` makes
        // `element`, is no method's generic where a function, constant or
        // host function of the program takes its name.
        let generic_symbol = self
            .resolve_generic(&method.name)
            .filter(|symbol| {
                !self.is_kernel_generic(symbol.id)
                    || (self.resolve_function(&method.name).is_none()
                        && self.resolve_host_function(&method.name).is_none()
                        && self.resolve_constant(&method.name).is_none())
            })
            .cloned();
        let generic = generic_symbol
            .as_ref()
            .map(|symbol| symbol.id)
            .unwrap_or_else(|| {
                self.diagnostics.push(
                    Diagnostic::error(method.span, DiagnosticCode::UnresolvedGeneric)
                        .with_arg(DiagnosticArg::text("generic", method.name.clone())),
                );
                GenericFunctionId(u32::MAX)
            });
        if let Some(symbol) = &generic_symbol
            && method.params.len() != symbol.arity()
        {
            ctx.diagnostics.push(
                Diagnostic::error(method.span, DiagnosticCode::GenericMethodArityMismatch)
                    .with_arg(DiagnosticArg::text("method", method.name.clone()))
                    .with_arg(DiagnosticArg::unsigned(
                        "expected_count",
                        u64::try_from(symbol.arity()).unwrap_or(u64::MAX),
                    ))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(method.params.len()).unwrap_or(u64::MAX),
                    )),
            );
        }

        let mut params = Vec::new();
        let mut specializers = Vec::new();
        for (index, param) in method.params.iter().enumerate() {
            if ctx.find_local(&param.name).is_some() {
                ctx.diagnostics.push(
                    Diagnostic::error(param.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("name", param.name.clone())),
                );
            }
            let generic_kind = generic_symbol
                .as_ref()
                .and_then(|symbol| symbol.param_kinds.get(index).copied().flatten());
            let generic_range = generic_symbol
                .as_ref()
                .and_then(|symbol| symbol.param_ranges.get(index).copied().flatten());
            // A keyword parameter has its generic's type, and dispatch does
            // not look at it.
            let keyword = generic_symbol
                .as_ref()
                .is_some_and(|symbol| index >= self.keyword_start(&symbol.name, symbol.arity()));
            let (specializer, kind) = if keyword {
                self.method_keyword_kind(&mut ctx, method, param, generic_kind, generic_range)
            } else {
                self.resolve_method_specializer(param, generic_kind, generic_range)
            };
            // A specializer that did not resolve has been reported already.
            if let Some(expected) = generic_kind
                && !keyword
                && kind != ValueKind::Error
                && !self.generic_specializer_matches(expected, specializer)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(
                        param.span,
                        DiagnosticCode::GenericMethodSpecializerTypeMismatch,
                    )
                    .with_arg(DiagnosticArg::text("method", method.name.clone()))
                    .with_arg(DiagnosticArg::unsigned(
                        "argument_index",
                        u64::try_from(index + 1).unwrap_or(u64::MAX),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "actual",
                        self.method_specializer_label(specializer),
                    ))
                    .with_arg(DiagnosticArg::text("expected", self.kind_name(expected))),
                );
            }
            let local = ctx.add_local(
                self.local_definition(method.definition, &param.name),
                param.name.clone(),
                kind,
                None,
                param.span,
            );
            // Every send checks the argument against the generic's range,
            // whether the method restates it, widens it or specializes on
            // `<integer>`, so the body may assume the generic's.
            if kind == ValueKind::I64 {
                ctx.limit_local(local, generic_range);
            }
            ctx.allow_assignment(local);
            params.push(local);
            specializers.push(specializer);
        }
        let param = generic_symbol
            .as_ref()
            .and_then(|symbol| params.get(symbol.dispatch_param))
            .copied()
            .or_else(|| params.first().copied())
            .unwrap_or(LocalId(u32::MAX));
        let specializer = generic_symbol
            .as_ref()
            .and_then(|symbol| specializers.get(symbol.dispatch_param))
            .copied()
            .flatten()
            .or_else(|| specializers.iter().copied().flatten().next())
            .unwrap_or(ClassId(u32::MAX));
        let resolved_results = method
            .return_type
            .as_ref()
            .map(|annotation| self.resolve_result_annotation_detail(annotation));
        let (return_annotation, annotated_callable_sigs, annotated_ranges) = match resolved_results
        {
            Some(resolved) => (
                Some(resolved.kinds),
                Some(resolved.callable_sigs),
                resolved.ranges,
            ),
            None => (None, None, Vec::new()),
        };
        let mut body = self.lower_result_block(
            &mut ctx,
            &method.body,
            return_annotation.as_deref(),
            annotated_callable_sigs.as_deref().unwrap_or_default(),
        );
        if return_annotation.as_deref() == Some(&[]) {
            discard_results(&mut body);
        } else if let Some((expected, sigs)) = match (&return_annotation, &generic_symbol) {
            (Some(kinds), _) => Some((
                kinds.as_slice(),
                annotated_callable_sigs.as_deref().unwrap_or_default(),
            )),
            (None, Some(symbol)) => symbol
                .return_annotation
                .as_deref()
                .map(|kinds| (kinds, symbol.return_callable_sigs.as_slice())),
            (None, None) => None,
        } {
            let result = core::mem::replace(&mut body.result, Expr::Error { span: body.span });
            body.result = self.narrow_results(&mut ctx, result, expected, sigs, "method-result");
        }
        let body_kinds = self.expr_kinds(&body.result);
        let body_callable_sigs = self.expr_result_callable_signatures(&body.result);
        let result_kinds = return_annotation
            .clone()
            .unwrap_or_else(|| body_kinds.clone());
        let result_callable_sigs =
            annotated_callable_sigs.unwrap_or_else(|| body_callable_sigs.clone());
        let mut results_match = true;
        if let Some(expected) = &return_annotation {
            let mismatch = |expected: String, actual: String| {
                Diagnostic::error(body.result.span(), DiagnosticCode::MethodReturnTypeMismatch)
                    .with_arg(DiagnosticArg::text("method", method.name.clone()))
                    .with_arg(DiagnosticArg::text("expected", expected))
                    .with_arg(DiagnosticArg::text("actual", actual))
            };
            if self.result_kinds_match(expected, &body_kinds) {
                let diagnostics = self.callable_results_diagnostics(
                    body.result.span(),
                    &result_callable_sigs,
                    &body_kinds,
                    &body_callable_sigs,
                    mismatch,
                );
                results_match = diagnostics.is_empty();
                ctx.diagnostics.extend(diagnostics);
            } else {
                ctx.diagnostics.push(mismatch(
                    self.result_kinds_name(expected),
                    self.result_kinds_name(&body_kinds),
                ));
            }
        }
        // Every send trusts the generic's declared results, so a method must
        // return what they promise, a typed callable included.
        if let Some(symbol) = &generic_symbol
            && let Some(expected) = &symbol.return_annotation
        {
            let mismatch = |expected: String, actual: String| {
                Diagnostic::error(
                    body.result.span(),
                    DiagnosticCode::GenericMethodResultTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("method", method.name.clone()))
                .with_arg(DiagnosticArg::text("actual", actual))
                .with_arg(DiagnosticArg::text("expected", expected))
            };
            if !self.result_kinds_match(expected, &result_kinds) {
                ctx.diagnostics.push(mismatch(
                    self.result_kinds_name(expected),
                    self.result_kinds_name(&result_kinds),
                ));
            } else if results_match {
                ctx.diagnostics.extend(self.callable_results_diagnostics(
                    body.result.span(),
                    &symbol.return_callable_sigs,
                    &result_kinds,
                    &result_callable_sigs,
                    mismatch,
                ));
                // A declared limited result, `<i32>` or `<u32>` must be
                // within the generic's range too.
                for (index, generic_range) in symbol.return_ranges.iter().enumerate() {
                    let declared = return_annotation
                        .as_ref()
                        .and_then(|kinds| kinds.get(index).copied());
                    let own = annotated_ranges.get(index).copied().flatten().or_else(|| {
                        declared
                            .and_then(NumericKind::of)
                            .filter(|kind| matches!(kind, NumericKind::I32 | NumericKind::U32))
                            .map(integer_range_of)
                    });
                    if let (Some(generic_range), Some(own)) = (generic_range, own)
                        && !generic_range.includes(own)
                    {
                        let own = match declared {
                            Some(kind @ (ValueKind::I32 | ValueKind::U32)) => self.kind_name(kind),
                            _ => limited_type_name(own),
                        };
                        ctx.diagnostics
                            .push(mismatch(limited_type_name(*generic_range), own));
                    }
                }
            }
        }
        // The method checks what it returns against its own limited result
        // types, or else its generic's, which every send trusts.
        let result_ranges = (0..result_kinds.len())
            .map(|index| {
                annotated_ranges.get(index).copied().flatten().or_else(|| {
                    generic_symbol
                        .as_ref()
                        .and_then(|symbol| symbol.return_ranges.get(index).copied().flatten())
                })
            })
            .collect::<Vec<_>>();
        let result_kinds = self.check_result_ranges(
            &mut ctx,
            &mut body,
            &result_kinds,
            &result_callable_sigs,
            &result_ranges,
        );
        self.diagnostics.extend(ctx.diagnostics);
        Method {
            id,
            definition: method.definition,
            generic,
            specializer,
            specializers,
            param,
            params,
            locals: ctx.locals,
            body,
            result_kinds,
            result_ranges,
            return_annotation,
            span: method.span,
            origin: MethodOrigin::Source,
            strict: method.strict,
        }
    }

    fn lower_function(&mut self, id: FunctionId, function: &ast::Function) -> Function {
        let mut ctx = FunctionContext::new();
        ctx.shared_names = ast::shared_local_names(&function.body);
        let mut params = Vec::new();
        let symbol = self.functions.get(id.0 as usize).cloned();

        // A `#rest` parameter is the last, a `<vector>` the call makes.
        for (index, param) in function
            .params
            .iter()
            .chain(function.rest.as_ref())
            .enumerate()
        {
            if ctx.find_local(&param.name).is_some() {
                ctx.diagnostics.push(
                    Diagnostic::error(param.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", param.name.clone())),
                );
            }
            let kind = symbol
                .as_ref()
                .and_then(|symbol| symbol.param_kinds.get(index))
                .copied()
                .unwrap_or(ValueKind::Error);
            let callable_signature = symbol
                .as_ref()
                .and_then(|symbol| symbol.param_callable_sigs.get(index))
                .cloned()
                .flatten();
            let local = ctx.add_local(
                self.local_definition(function.definition, &param.name),
                param.name.clone(),
                kind,
                callable_signature,
                param.span,
            );
            ctx.limit_local(
                local,
                symbol
                    .as_ref()
                    .and_then(|symbol| symbol.param_ranges.get(index).copied().flatten()),
            );
            ctx.allow_assignment(local);
            params.push(local);
        }

        let return_annotation = symbol
            .as_ref()
            .and_then(|symbol| symbol.return_annotation.clone());
        let annotated_return_callable_sigs = symbol
            .as_ref()
            .map_or_else(Vec::new, |symbol| symbol.return_callable_sigs.clone());
        let mut body = self.lower_result_block(
            &mut ctx,
            &function.body,
            return_annotation.as_deref(),
            &annotated_return_callable_sigs,
        );
        if return_annotation.as_deref() == Some(&[]) {
            discard_results(&mut body);
        } else if let Some(expected) = return_annotation.as_deref() {
            let result = core::mem::replace(&mut body.result, Expr::Error { span: body.span });
            body.result = self.narrow_results(
                &mut ctx,
                result,
                expected,
                &annotated_return_callable_sigs,
                "function-result",
            );
        }
        let body_kinds = self.expr_kinds(&body.result);
        let body_callable_sigs = self.expr_result_callable_signatures(&body.result);
        let result_kinds = return_annotation
            .clone()
            .unwrap_or_else(|| body_kinds.clone());
        let result_callable_sigs = if return_annotation.is_some() {
            annotated_return_callable_sigs
        } else {
            body_callable_sigs.clone()
        };
        if let Some(expected) = &return_annotation {
            let mismatch = |expected: String, actual: String| {
                Diagnostic::error(
                    body.result.span(),
                    DiagnosticCode::FunctionReturnTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("function", function.name.clone()))
                .with_arg(DiagnosticArg::text("expected", expected))
                .with_arg(DiagnosticArg::text("actual", actual))
            };
            if self.result_kinds_match(expected, &body_kinds) {
                ctx.diagnostics.extend(self.callable_results_diagnostics(
                    body.result.span(),
                    &result_callable_sigs,
                    &body_kinds,
                    &body_callable_sigs,
                    mismatch,
                ));
            } else {
                ctx.diagnostics.push(mismatch(
                    self.result_kinds_name(expected),
                    self.result_kinds_name(&body_kinds),
                ));
            }
        }
        if let Some(symbol) = self.functions.get_mut(id.0 as usize)
            && symbol.return_annotation.is_none()
        {
            symbol.return_kinds = result_kinds.clone();
            symbol.return_callable_sigs = result_callable_sigs.clone();
            symbol.return_ranges = vec![None; result_kinds.len()];
        }
        let result_ranges = match (&symbol, &return_annotation) {
            (Some(symbol), Some(_)) => symbol.return_ranges.clone(),
            _ => vec![None; result_kinds.len()],
        };
        let result_kinds = self.check_result_ranges(
            &mut ctx,
            &mut body,
            &result_kinds,
            &result_callable_sigs,
            &result_ranges,
        );
        self.diagnostics.extend(ctx.diagnostics);
        let (param_callable_sigs, param_ranges) = symbol.as_ref().map_or_else(
            || (Vec::new(), Vec::new()),
            |symbol| {
                (
                    symbol.param_callable_sigs.clone(),
                    symbol.param_ranges.clone(),
                )
            },
        );

        Function {
            id,
            definition: function.definition,
            name: function.name.clone(),
            params,
            param_kinds: symbol.map_or_else(Vec::new, |symbol| symbol.param_kinds),
            param_callable_sigs,
            param_ranges,
            locals: ctx.locals,
            body,
            result_kinds,
            result_callable_sigs,
            result_ranges,
            return_annotation,
            entry_checks: Vec::new(),
            keywords: self.keyword_params(self.keyword_functions.iter(), &function.name),
            rest: function.rest.is_some(),
            strict: function.strict,
            kernel: false,
            span: function.span,
        }
    }

    /// Records each source generic's keyword parameters, whose defaults are
    /// functions [`Self::collect_functions`] collected.
    fn collect_generic_keywords(&mut self) {
        for index in 0..self.generics.len() {
            let name = self.generics[index].name.clone();
            let keywords = self.keyword_params(self.keyword_generics.iter(), &name);
            self.generics[index].keywords = keywords;
        }
    }

    /// The keyword parameters `table` records for `owner`, with their
    /// default functions' ids.
    fn keyword_params<'a>(
        &self,
        mut table: impl Iterator<Item = &'a (String, KeywordSignature)>,
        owner: &str,
    ) -> Vec<KeywordParam> {
        table
            .find(|(name, _)| name == owner)
            .map(|(_, signature)| {
                signature
                    .keywords
                    .iter()
                    .map(|slot| KeywordParam {
                        name: slot.name.clone(),
                        default: match &slot.default {
                            KeywordDefault::Function(name) => {
                                self.resolve_function(name).map(|function| function.id)
                            }
                            KeywordDefault::Unsupplied | KeywordDefault::Missing => None,
                        },
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn lower_block(
        &mut self,
        ctx: &mut FunctionContext,
        block: &ast::Block,
        expected_result: Option<&[ValueKind]>,
    ) -> Block {
        self.lower_result_block(ctx, block, expected_result, &[])
    }

    /// Lowers a function's, method's or closure's body, whose results are
    /// `expected_result` with the callable signatures `expected_sigs`: an
    /// anonymous method it ends with takes its types from the one it
    /// returns as.
    fn lower_result_block(
        &mut self,
        ctx: &mut FunctionContext,
        block: &ast::Block,
        expected_result: Option<&[ValueKind]>,
        expected_sigs: &[Option<CallableSignature>],
    ) -> Block {
        let mut statements = Vec::new();
        for statement in &block.statements {
            let local = match statement {
                ast::Statement::Let(local) => local,
                ast::Statement::Local(methods) => {
                    statements.push(self.lower_local_methods(ctx, methods));
                    continue;
                }
                ast::Statement::Expr(expr) => {
                    // A statement's values are discarded, so the arms of a
                    // conditional or a handle there need not agree.
                    let value =
                        self.lower_expr_expected_results(ctx, expr, effect_expectation(expr));
                    statements.push(Statement::Expr(value));
                    continue;
                }
            };
            match &local.pattern {
                ast::LetPattern::Name(binding) => {
                    let expected_type = binding.type_annotation.as_ref().map(|annotation| {
                        self.resolve_type_annotation_detail_in_context(ctx, annotation)
                    });
                    let expected_kind = expected_type.as_ref().map(|resolved| resolved.kind);
                    let expected_signature = expected_type
                        .as_ref()
                        .and_then(|resolved| resolved.callable_signature.as_ref());
                    let value = self
                        .lower_method_against(ctx, &local.value, expected_signature)
                        .unwrap_or_else(|| match expected_kind {
                            Some(kind) => self.lower_expr_expected_results(
                                ctx,
                                &local.value,
                                Some(core::slice::from_ref(&kind)),
                            ),
                            None => self.lower_expr_expected_results(ctx, &local.value, None),
                        });
                    let value = self.take_values(ctx, value, 1);
                    let actual_kind = self.expr_kind(&value);
                    let actual_callable_signature = self.expr_callable_signature(&value);
                    let actual_kinds = self.expr_kinds(&value);
                    if actual_kinds.len() != 1 {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                local.value.span(),
                                DiagnosticCode::LocalTypeMismatch,
                            )
                            .with_arg(DiagnosticArg::text("local", binding.name.clone()))
                            .with_arg(DiagnosticArg::text("expected", "1 value"))
                            .with_arg(DiagnosticArg::text(
                                "actual",
                                self.result_kinds_name(&actual_kinds),
                            )),
                        );
                    }
                    let value = match expected_kind {
                        Some(expected) => self.narrow(
                            value,
                            expected,
                            expected_type
                                .as_ref()
                                .and_then(|resolved| resolved.callable_signature.as_ref()),
                        ),
                        None => value,
                    };
                    // A value that did not check has been reported already.
                    if let Some(expected) = expected_kind
                        && actual_kind != ValueKind::Error
                        && !self.kinds_match(expected, self.expr_kind(&value))
                    {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                local.value.span(),
                                DiagnosticCode::LocalTypeMismatch,
                            )
                            .with_arg(DiagnosticArg::text("local", binding.name.clone()))
                            .with_arg(DiagnosticArg::text("expected", self.kind_name(expected)))
                            .with_arg(DiagnosticArg::text("actual", self.kind_name(actual_kind))),
                        );
                    }
                    if let Some(expected) = expected_type
                        .as_ref()
                        .and_then(|resolved| resolved.callable_signature.as_ref())
                        && actual_kind != ValueKind::Error
                        && !self.callable_signatures_match(
                            Some(expected),
                            actual_callable_signature.as_ref(),
                        )
                    {
                        let diagnostic = self
                            .callable_variance_diagnostic(
                                local.value.span(),
                                expected,
                                actual_callable_signature.as_ref(),
                            )
                            .unwrap_or_else(|| {
                                Diagnostic::error(
                                    local.value.span(),
                                    DiagnosticCode::LocalTypeMismatch,
                                )
                                .with_arg(DiagnosticArg::text("local", binding.name.clone()))
                                .with_arg(DiagnosticArg::text(
                                    "expected",
                                    self.callable_signature_name(expected),
                                ))
                                .with_arg(DiagnosticArg::text(
                                    "actual",
                                    actual_callable_signature.as_ref().map_or_else(
                                        || self.kind_name(actual_kind),
                                        |signature| self.callable_signature_name(signature),
                                    ),
                                ))
                            });
                        ctx.diagnostics.push(diagnostic);
                    }
                    let local_kind = expected_kind.unwrap_or(actual_kind);
                    let range = expected_type.as_ref().and_then(|resolved| resolved.range);
                    let value = self.limit_value(value, local_kind, range);
                    let callable_signature = expected_type
                        .and_then(|resolved| resolved.callable_signature)
                        .or(match actual_kind {
                            ValueKind::Callable => actual_callable_signature,
                            _ => None,
                        });
                    if ctx.find_local(&binding.name).is_some() {
                        ctx.diagnostics.push(
                            Diagnostic::error(binding.span, DiagnosticCode::DuplicateLocal)
                                .with_arg(DiagnosticArg::text("local", binding.name.clone())),
                        );
                    }
                    let id = ctx.add_local(
                        self.local_definition(binding.definition, &binding.name),
                        binding.name.clone(),
                        local_kind,
                        callable_signature,
                        binding.span,
                    );
                    ctx.limit_local(id, range);
                    ctx.allow_assignment(id);
                    statements.push(Statement::Let(Let {
                        local: id,
                        locals: vec![id],
                        value,
                        span: local.span,
                    }));
                }
                ast::LetPattern::Tuple { bindings, .. } => {
                    let expected_kinds = bindings
                        .iter()
                        .map(|binding| {
                            binding.type_annotation.as_ref().map(|annotation| {
                                self.resolve_type_annotation_detail_in_context(ctx, annotation)
                            })
                        })
                        .collect::<Vec<_>>();
                    let expected_results = expected_kinds
                        .iter()
                        .map(|kind| kind.as_ref().map(|resolved| resolved.kind))
                        .collect::<Option<Vec<_>>>();
                    let value = self.lower_expr_expected_results(
                        ctx,
                        &local.value,
                        expected_results.as_deref(),
                    );
                    let actual_kinds = self.expr_kinds(&value);
                    let actual_callable_sigs = self.expr_result_callable_signatures(&value);
                    if bindings.len() != actual_kinds.len()
                        && !result_kinds_contain_error(&actual_kinds)
                    {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                local.value.span(),
                                DiagnosticCode::LocalTypeMismatch,
                            )
                            .with_arg(DiagnosticArg::text("local", local.name.clone()))
                            .with_arg(DiagnosticArg::text(
                                "expected",
                                format!("{} values", bindings.len()),
                            ))
                            .with_arg(DiagnosticArg::text(
                                "actual",
                                self.result_kinds_name(&actual_kinds),
                            )),
                        );
                    }
                    let mut local_ids = Vec::with_capacity(bindings.len());
                    for (index, binding) in bindings.iter().enumerate() {
                        if ctx.find_local(&binding.name).is_some() {
                            ctx.diagnostics.push(
                                Diagnostic::error(binding.span, DiagnosticCode::DuplicateLocal)
                                    .with_arg(DiagnosticArg::text("local", binding.name.clone())),
                            );
                        }
                        let expected_type = expected_kinds.get(index).cloned().flatten();
                        let expected_kind = expected_type.as_ref().map(|resolved| resolved.kind);
                        let actual_kind =
                            actual_kinds.get(index).copied().unwrap_or(ValueKind::Error);
                        if let Some(expected) = expected_kind
                            && !self.kinds_match(expected, actual_kind)
                            && self
                                .implicit_cast_class(
                                    expected,
                                    actual_kind,
                                    expected_type
                                        .as_ref()
                                        .and_then(|resolved| resolved.callable_signature.as_ref()),
                                )
                                .is_none()
                        {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    local.value.span(),
                                    DiagnosticCode::LocalTypeMismatch,
                                )
                                .with_arg(DiagnosticArg::text("local", binding.name.clone()))
                                .with_arg(DiagnosticArg::text("expected", self.kind_name(expected)))
                                .with_arg(DiagnosticArg::text(
                                    "actual",
                                    self.kind_name(actual_kind),
                                )),
                            );
                        }
                        let actual_callable_signature =
                            actual_callable_sigs.get(index).cloned().flatten();
                        if let Some(expected) = expected_type
                            .as_ref()
                            .and_then(|resolved| resolved.callable_signature.as_ref())
                            && !self.callable_signatures_match(
                                Some(expected),
                                actual_callable_signature.as_ref(),
                            )
                        {
                            let diagnostic = self
                                .callable_variance_diagnostic(
                                    local.value.span(),
                                    expected,
                                    actual_callable_signature.as_ref(),
                                )
                                .unwrap_or_else(|| {
                                    Diagnostic::error(
                                        local.value.span(),
                                        DiagnosticCode::LocalTypeMismatch,
                                    )
                                    .with_arg(DiagnosticArg::text("local", binding.name.clone()))
                                    .with_arg(DiagnosticArg::text(
                                        "expected",
                                        self.callable_signature_name(expected),
                                    ))
                                    .with_arg(
                                        DiagnosticArg::text(
                                            "actual",
                                            actual_callable_signature.as_ref().map_or_else(
                                                || self.kind_name(actual_kind),
                                                |signature| self.callable_signature_name(signature),
                                            ),
                                        ),
                                    )
                                });
                            ctx.diagnostics.push(diagnostic);
                        }
                        let kind = expected_kind.unwrap_or(actual_kind);
                        let range = expected_type.as_ref().and_then(|resolved| resolved.range);
                        let callable_signature = expected_type
                            .and_then(|resolved| resolved.callable_signature)
                            .or(match actual_kind {
                                ValueKind::Callable => actual_callable_signature,
                                _ => None,
                            });
                        let id = ctx.add_local(
                            self.local_definition(binding.definition, &binding.name),
                            binding.name.clone(),
                            kind,
                            callable_signature,
                            binding.span,
                        );
                        ctx.limit_local(id, range);
                        ctx.allow_assignment(id);
                        local_ids.push(id);
                    }
                    // Each binding checks its own value: one a declaration
                    // narrows is cast, and a limited one is range checked.
                    let checks = actual_kinds
                        .iter()
                        .zip(&expected_kinds)
                        .map(|(kind, resolved)| {
                            let range = resolved.as_ref().and_then(|resolved| resolved.range);
                            let narrow = resolved.as_ref().and_then(|resolved| {
                                (!self.kinds_match(resolved.kind, *kind))
                                    .then(|| {
                                        self.implicit_cast_class(
                                            resolved.kind,
                                            *kind,
                                            resolved.callable_signature.as_ref(),
                                        )
                                    })
                                    .flatten()
                            });
                            match (narrow, range) {
                                (Some(class), range) => CallableResultCheck::Narrow {
                                    from: *kind,
                                    class,
                                    range,
                                },
                                (None, Some(range))
                                    if NumericKind::of(*kind)
                                        .is_some_and(NumericKind::is_integer) =>
                                {
                                    CallableResultCheck::Range(range)
                                }
                                (None, _) => CallableResultCheck::Trusted(*kind),
                            }
                        })
                        .collect::<Vec<_>>();
                    let value = if bindings.len() == actual_kinds.len()
                        && checks
                            .iter()
                            .any(|check| !matches!(check, CallableResultCheck::Trusted(_)))
                        && !result_kinds_contain_error(&actual_kinds)
                    {
                        let kinds = checks
                            .iter()
                            .map(|check| match check {
                                CallableResultCheck::Trusted(kind) => *kind,
                                CallableResultCheck::Narrow { class, .. } => self
                                    .kind_for_builtin_class(*class)
                                    .unwrap_or(ValueKind::Object(*class)),
                                check => check.wire_kind(),
                            })
                            .collect::<Vec<_>>();
                        self.check_values(
                            ctx,
                            value,
                            &checks,
                            &kinds,
                            &actual_callable_sigs,
                            "checked-binding",
                            local.span,
                        )
                    } else {
                        value
                    };
                    let local_id = local_ids.first().copied().unwrap_or(LocalId(u32::MAX));
                    statements.push(Statement::Let(Let {
                        local: local_id,
                        locals: local_ids,
                        value,
                        span: local.span,
                    }));
                }
            }
        }

        Block {
            statements,
            result: match expected_sigs {
                [signature] => self.lower_method_against(ctx, &block.result, signature.as_ref()),
                _ => None,
            }
            .unwrap_or_else(|| {
                self.lower_expr_expected_results(ctx, &block.result, expected_result)
            }),
            span: block.span,
        }
    }

    /// Lowers `expr` where a callable of `signature` is expected, when it is
    /// an anonymous method: the method takes the types it leaves unwritten
    /// from the signature ([`ExpectedSignature`]). `None` for any other
    /// expression, or without a signature; the caller lowers it as usual.
    fn lower_method_against(
        &mut self,
        ctx: &mut FunctionContext,
        expr: &ast::Expr,
        signature: Option<&CallableSignature>,
    ) -> Option<Expr> {
        let signature = signature?;
        let ast::Expr::Function {
            params,
            rest,
            return_type,
            body,
            span,
            ..
        } = expr
        else {
            return None;
        };
        Some(self.lower_closure_expr(
            ctx,
            params,
            rest.as_deref(),
            return_type.as_ref(),
            body,
            *span,
            Some(signature),
        ))
    }

    /// Lowers call arguments, each where its parameter's callable signature
    /// in `signatures`, if any, is expected ([`Self::lower_method_against`]).
    fn lower_args_against(
        &mut self,
        ctx: &mut FunctionContext,
        args: &[ast::Expr],
        signatures: &[Option<CallableSignature>],
    ) -> Vec<Expr> {
        args.iter()
            .enumerate()
            .map(|(index, arg)| {
                let signature = signatures.get(index).and_then(Option::as_ref);
                self.lower_method_against(ctx, arg, signature)
                    .unwrap_or_else(|| self.lower_expr(ctx, arg))
            })
            .collect()
    }

    fn lower_expr(&mut self, ctx: &mut FunctionContext, expr: &ast::Expr) -> Expr {
        self.lower_scalar_expr(ctx, expr, None, "expression")
    }

    fn lower_expr_expected(
        &mut self,
        ctx: &mut FunctionContext,
        expr: &ast::Expr,
        expected_result: Option<ValueKind>,
    ) -> Expr {
        self.lower_scalar_expr(ctx, expr, expected_result, "expression")
    }

    fn lower_scalar_expr(
        &mut self,
        ctx: &mut FunctionContext,
        expr: &ast::Expr,
        expected_result: Option<ValueKind>,
        context: &'static str,
    ) -> Expr {
        let lowered = match expected_result {
            Some(kind) => {
                self.lower_expr_expected_results(ctx, expr, Some(core::slice::from_ref(&kind)))
            }
            None => self.lower_expr_expected_results(ctx, expr, None),
        };
        let lowered = self.take_values(ctx, lowered, 1);
        self.require_scalar_result(ctx, &lowered, expr.span(), context);
        lowered
    }

    /// The first `count` of the values `value` produces, as Dylan keeps
    /// the first where fewer are expected: `value` itself when it produces
    /// no more. The values are bound to temporaries no source name can
    /// reach, so every one is still computed, in order.
    fn take_values(&self, ctx: &mut FunctionContext, value: Expr, count: usize) -> Expr {
        // `values(x)` is `x` where one value is expected.
        let value = match value {
            Expr::Values { mut values, .. } if count == 1 && values.len() == 1 => {
                values.pop().unwrap_or(Expr::Error {
                    span: Span::default(),
                })
            }
            value => value,
        };
        let kinds = self.expr_kinds(&value);
        if kinds.len() <= count || count == 0 || result_kinds_contain_error(&kinds) {
            return value;
        }
        let span = value.span();
        let sigs = self.expr_result_callable_signatures(&value);
        let sig = |index: usize| sigs.get(index).cloned().flatten();
        let scope = ctx.scope_start();
        let temporaries = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                ctx.add_local(
                    DefinitionId::new(
                        self.source_file,
                        DefinitionKind::Local,
                        "$value",
                        &format!("first-values:{}:{}:{index}", span.start, span.end),
                    ),
                    format!("$value{index}"),
                    *kind,
                    sig(index),
                    span,
                )
            })
            .collect::<Vec<_>>();
        ctx.hide_locals_from(scope);
        let kept = temporaries
            .iter()
            .zip(&kinds)
            .take(count)
            .enumerate()
            .map(|(index, (local, kind))| Expr::Local {
                local: *local,
                result_kind: *kind,
                callable_signature: sig(index),
                span,
            })
            .collect::<Vec<_>>();
        let result = if count == 1 {
            kept.into_iter().next().unwrap_or(Expr::Error { span })
        } else {
            Expr::Values {
                values: kept,
                result_kinds: kinds[..count].to_vec(),
                span,
            }
        };
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: vec![Statement::Let(Let {
                    local: temporaries[0],
                    locals: temporaries,
                    value,
                    span,
                })],
                result,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    fn require_scalar_result(
        &self,
        ctx: &mut FunctionContext,
        expr: &Expr,
        span: Span,
        context: &'static str,
    ) {
        let actual_kinds = self.expr_kinds(expr);
        if actual_kinds.len() == 1 || result_kinds_contain_error(&actual_kinds) {
            return;
        }
        ctx.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::ScalarResultArityMismatch)
                .with_arg(DiagnosticArg::text("context", context))
                .with_arg(DiagnosticArg::text(
                    "actual",
                    self.result_kinds_name(&actual_kinds),
                )),
        );
    }

    fn lower_expr_expected_results(
        &mut self,
        ctx: &mut FunctionContext,
        expr: &ast::Expr,
        expected_results: Option<&[ValueKind]>,
    ) -> Expr {
        match expr {
            ast::Expr::Number { value, span, .. } => adapt_literal(
                Expr::ConstI64 {
                    value: *value,
                    span: *span,
                },
                expected_results,
            ),
            ast::Expr::Float { value, span, .. } => adapt_literal(
                Expr::ConstF64 {
                    value: *value,
                    span: *span,
                },
                expected_results,
            ),
            ast::Expr::Number32 { value, span, .. } => Expr::Const32 {
                value: *value,
                span: *span,
            },
            ast::Expr::Bool { value, span, .. } => Expr::ConstBool {
                value: *value,
                span: *span,
            },
            ast::Expr::Character { value, span, .. } => Expr::ConstChar {
                value: *value,
                span: *span,
            },
            ast::Expr::String { value, span, .. } => Expr::StringLiteral {
                value: value.clone(),
                class: self
                    .builtin_value_class(BuiltinValueClass::String)
                    .unwrap_or(ClassId(u32::MAX)),
                span: *span,
            },
            ast::Expr::Symbol { name, span, .. } => Expr::SymbolLiteral {
                name: name.clone(),
                class: self
                    .builtin_value_class(BuiltinValueClass::Symbol)
                    .unwrap_or(ClassId(u32::MAX)),
                span: *span,
            },
            ast::Expr::Vector { elements, span, .. } => Expr::VectorLiteral {
                elements: elements
                    .iter()
                    .map(|element| self.lower_expr(ctx, element))
                    .collect(),
                class: self
                    .builtin_value_class(BuiltinValueClass::Vector)
                    .unwrap_or(ClassId(u32::MAX)),
                span: *span,
            },
            ast::Expr::Function {
                params,
                rest,
                return_type,
                body,
                span,
                ..
            } => self.lower_closure_expr(
                ctx,
                params,
                rest.as_deref(),
                return_type.as_ref(),
                body,
                *span,
                None,
            ),
            // `$unsupplied` is reserved, so it always means the kernel's.
            ast::Expr::Name { name, span, .. } if name == keywords::UNSUPPLIED => {
                self.unsupplied(*span)
            }
            ast::Expr::Name { name, span, .. } => match ctx.find_local(name) {
                Some(local) => Expr::Local {
                    local,
                    result_kind: ctx.local_kind(local),
                    callable_signature: ctx.local_callable_signature(local),
                    span: *span,
                },
                None => match ctx.find_capture_or_add(name) {
                    Some(capture) => Expr::Capture {
                        capture,
                        result_kind: ctx.capture_kind(capture),
                        callable_signature: ctx.capture_callable_signature(capture),
                        span: *span,
                    },
                    None => {
                        // A name may be the first mention of a built-in
                        // collection generic, used as a value.
                        let _ = self.collection_generic_named(name);
                        self.lower_known_name(ctx, name, *span)
                    }
                },
            },
            ast::Expr::ClassName {
                class_name, span, ..
            } => match LimitedInteger::from_name(class_name) {
                Some(limited) => self.lower_type_object(TypeObject::named(limited), *span),
                None => self.lower_class_object(ctx, class_name, *span),
            },
            ast::Expr::TypeLiteral {
                id, target, span, ..
            } => {
                if self.binding_or_definition(ctx, "limited") {
                    return self.lower_shadowed_type_literal(
                        ctx,
                        *id,
                        target,
                        expected_results,
                        *span,
                    );
                }
                self.lower_type_literal(ctx, target, *span)
            }
            ast::Expr::Cast {
                target,
                value,
                span,
                ..
            } => self.lower_cast(ctx, value, target, *span),
            ast::Expr::Call {
                id,
                callee,
                args,
                keywords,
                span,
            } => self.lower_named_call(ctx, *id, callee, args, keywords, expected_results, *span),
            ast::Expr::ValueCall {
                id,
                callee,
                args,
                keywords,
                span,
            } => {
                let target = self.lower_expr(ctx, callee);
                let signatures = self
                    .callable_signature_of(&target)
                    .map(|signature| signature.param_sigs)
                    .unwrap_or_default();
                let args = self.lower_args_against(ctx, args, &signatures);
                self.lower_value_call(
                    ctx,
                    *id,
                    None,
                    target,
                    args,
                    keywords,
                    expected_results,
                    *span,
                )
            }
            ast::Expr::Signal {
                condition, span, ..
            } => self.lower_signal(ctx, condition, expected_results, *span),
            ast::Expr::MakeObject {
                class,
                fields,
                span,
                ..
            } => self.lower_make_object(ctx, class, fields, *span),
            // As in Dylan, `x.f` is `f(x)` and `x.f := v` is
            // `f-setter(v, x)`: a name no class's slot has is called.
            ast::Expr::SlotRead {
                id,
                receiver,
                slot,
                span,
            } if self.calls_for_slot_syntax(ctx, slot) => {
                let call = ast::Expr::Call {
                    id: *id,
                    callee: slot.clone(),
                    args: vec![receiver.as_ref().clone()],
                    keywords: Vec::new(),
                    span: *span,
                };
                self.lower_expr_expected_results(ctx, &call, expected_results)
            }
            ast::Expr::SlotAssign {
                id,
                receiver,
                slot,
                value,
                span,
            } if self.calls_for_slot_syntax(ctx, slot) => {
                let call = ast::Expr::Call {
                    id: *id,
                    callee: slot_setter_name(slot),
                    args: vec![value.as_ref().clone(), receiver.as_ref().clone()],
                    keywords: Vec::new(),
                    span: *span,
                };
                self.lower_expr_expected_results(ctx, &call, expected_results)
            }
            ast::Expr::SlotRead {
                id,
                receiver,
                slot,
                span,
            } => self.lower_slot_read(ctx, *id, receiver, slot, *span),
            ast::Expr::SlotAssign {
                receiver,
                slot,
                value,
                span,
                ..
            } => self.lower_slot_assign(ctx, receiver, slot, value, *span),
            ast::Expr::Binary {
                op, lhs, rhs, span, ..
            } => {
                let (lhs, rhs) = self.lower_numeric_operands(ctx, lhs, rhs);
                let op = BinaryOp::from(*op);
                if op.is_wrapping() {
                    return self.lower_wrapping(ctx, op, lhs, rhs, *span);
                }
                if Self::dispatches_numerically(&[self.expr_kind(&lhs), self.expr_kind(&rhs)]) {
                    return self.numeric_dispatch(
                        promotion::NumericOperator::Arithmetic(op),
                        vec![lhs, rhs],
                        *span,
                    );
                }
                let Some((lhs_kind, rhs_kind)) = self.numeric_operand_kinds(
                    ctx,
                    *span,
                    &lhs,
                    &rhs,
                    DiagnosticCode::ArithmeticOperandTypeMismatch,
                ) else {
                    return Expr::Error { span: *span };
                };
                let kind = match promotion::arithmetic(op, lhs_kind, rhs_kind) {
                    promotion::ArithmeticRule::Promote { kind } => kind,
                    promotion::ArithmeticRule::Undefined => {
                        if lhs_kind.is_integer() && rhs_kind.is_integer() {
                            ctx.diagnostics.push(Diagnostic::error(
                                *span,
                                DiagnosticCode::IntegerDivisionUnsupported,
                            ));
                        }
                        return Expr::Error { span: *span };
                    }
                };
                Expr::Binary {
                    site: None,
                    op,
                    kind,
                    lhs: Box::new(promote_operand(lhs, lhs_kind, kind)),
                    rhs: Box::new(promote_operand(rhs, rhs_kind, kind)),
                    span: *span,
                }
            }
            ast::Expr::UnaryMinus { expr, span, .. } => {
                let value = self.lower_expr_expected(ctx, expr, Some(ValueKind::I64));
                let value_kind = self.expr_kind(&value);
                if Self::dispatches_numerically(&[value_kind]) {
                    return self.numeric_dispatch(
                        promotion::NumericOperator::Negate,
                        vec![value],
                        *span,
                    );
                }
                let Some(kind) = NumericKind::of(value_kind) else {
                    if value_kind != ValueKind::Error {
                        let name = self.kind_name(value_kind);
                        ctx.diagnostics.push(
                            Diagnostic::error(*span, DiagnosticCode::ArithmeticOperandTypeMismatch)
                                .with_arg(DiagnosticArg::text("lhs", name.clone()))
                                .with_arg(DiagnosticArg::text("rhs", name)),
                        );
                    }
                    return Expr::Error { span: *span };
                };
                negate_number(value, kind, *span)
            }
            ast::Expr::Compare {
                op, lhs, rhs, span, ..
            } => {
                let (lhs, rhs) = if *op == ast::CompareOp::Eq {
                    (self.lower_expr(ctx, lhs), self.lower_expr(ctx, rhs))
                } else {
                    self.lower_numeric_operands(ctx, lhs, rhs)
                };
                self.lower_compare(ctx, *op, lhs, rhs, *span)
            }
            ast::Expr::Identical { lhs, rhs, span, .. } => {
                let lhs = self.lower_expr(ctx, lhs);
                let rhs = self.lower_expr(ctx, rhs);
                self.lower_identical(ctx, lhs, rhs, *span)
            }
            ast::Expr::If {
                arms,
                else_body,
                span,
                ..
            } => self.lower_conditional(
                ctx,
                Conditional::If,
                &arm_refs(arms),
                else_body.as_deref(),
                *span,
                expected_results,
                None,
            ),
            ast::Expr::Case {
                arms,
                otherwise,
                span,
                ..
            } => self.lower_conditional(
                ctx,
                Conditional::Case,
                &arm_refs(arms),
                otherwise.as_deref(),
                *span,
                expected_results,
                None,
            ),
            ast::Expr::Select {
                id,
                target,
                test,
                arms,
                otherwise,
                span,
            } => self.lower_select(
                ctx,
                *id,
                target,
                test.as_deref(),
                arms,
                otherwise.as_deref(),
                *span,
                expected_results,
            ),
            ast::Expr::Unless {
                condition,
                body,
                span,
                ..
            } => {
                let condition = self.lower_expr(ctx, condition);
                self.check_test_is_boolean(ctx, "unless", &condition);
                let body = self.lower_body_expr(ctx, body, None);
                Expr::If {
                    condition: Box::new(condition),
                    refinement: None,
                    then_branch: Box::new(no_values(Span::new(span.end, span.end))),
                    else_branch: Box::new(discard_values(self, body)),
                    span: *span,
                }
            }
            ast::Expr::Assign {
                name,
                name_span,
                value,
                span,
                ..
            } => self.lower_local_assign(ctx, name, *name_span, value, *span),
            ast::Expr::Loop {
                kind,
                test,
                body,
                span,
                ..
            } => self.lower_loop(ctx, *kind, test, body, *span),
            ast::Expr::For {
                clauses,
                body,
                span,
                ..
            } => self.lower_for(ctx, clauses, body, *span),
            ast::Expr::Handle {
                body,
                restart,
                span,
                ..
            } => self.lower_handle(ctx, body, restart, *span, expected_results),
            ast::Expr::Block {
                exit,
                body,
                cleanup,
                span,
                ..
            } => self.lower_block_expr(
                ctx,
                exit.as_ref(),
                body,
                cleanup.as_deref(),
                *span,
                expected_results,
            ),
            ast::Expr::Error { span, .. } => Expr::Error { span: *span },
        }
    }

    /// Lowers a call of the name `callee`. As in Dylan, the innermost
    /// binding of the name wins: a local or captured binding, then a
    /// definition of the program (a function, host function, constant,
    /// generic of its own or slot getter), and only then the kernel's
    /// built-in of that name. Code the compiler writes calls the kernel by
    /// names that start with `%`, which no source name can.
    #[allow(clippy::too_many_arguments)]
    fn lower_named_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        if let Some(target) = self.local_callable_target(ctx, callee, span) {
            // A binding that names a function or generic statically calls
            // it as a call of its name does.
            if let ValueKind::KnownCallable(callable) = self.expr_kind(&target)
                && let Some(call) =
                    self.lower_known_binding_call(ctx, id, callable, args, keywords, span)
            {
                return call;
            }
            let signatures = self
                .callable_signature_of(&target)
                .map(|signature| signature.param_sigs)
                .unwrap_or_default();
            let lowered_args = self.lower_args_against(ctx, args, &signatures);
            return self.lower_value_call(
                ctx,
                id,
                Some(callee),
                target,
                lowered_args,
                keywords,
                expected_results,
                span,
            );
        }
        if !self.program_defines(callee)
            && built_in::names_built_in(callee)
            && self.has_accessor(callee)
        {
            return self.lower_getter_or_built_in_call(
                ctx,
                id,
                callee,
                args,
                keywords,
                expected_results,
                span,
            );
        }
        if self.program_defines(callee) {
            return self.lower_definition_call(
                ctx,
                id,
                callee,
                args,
                keywords,
                expected_results,
                span,
            );
        }
        self.lower_built_in_call(ctx, id, callee, args, keywords, expected_results, span)
    }

    /// Whether a binding or definition of the program takes `name` from
    /// the kernel's built-in of that name; see [`Self::lower_named_call`].
    fn binding_or_definition(&self, ctx: &FunctionContext, name: &str) -> bool {
        ctx.binds_lexically(name) || self.program_defines(name)
    }

    /// Whether a definition of the program takes `name` from the kernel's
    /// built-in of that name: a function, host function, constant, generic
    /// of its own or slot getter. A method of a kernel collection generic
    /// joins it rather than replacing it, and so does a slot getter of the
    /// same name, which serves its own class.
    fn program_defines(&self, name: &str) -> bool {
        if self.kernel_scope {
            return false;
        }
        self.shadows_collection_generic(name)
            || self.resolve_constant(name).is_some()
            || (CollectionFunction::from_name(name).is_none()
                && !built_in::names_built_in(name)
                && self.has_accessor(name))
    }

    /// Lowers a call of `callee`, a built-in's name that a slot's getter
    /// has too and no other definition of the program takes. The getter
    /// serves a call of one receiver of a class with the slot, read
    /// directly; every other call is the built-in's, so a class with a
    /// slot `apply` or `make` leaves `apply(f, x)` and `make(<c>)` alone.
    #[allow(clippy::too_many_arguments)]
    fn lower_getter_or_built_in_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let ([receiver], []) = (args, keywords) else {
            return self.lower_built_in_call(
                ctx,
                id,
                callee,
                args,
                keywords,
                expected_results,
                span,
            );
        };
        let value = self.lower_expr(ctx, receiver);
        if self.declares_slot(self.expr_kind(&value), callee) {
            return self.lower_accessor_call(ctx, value, callee, span);
        }
        // The built-in reads the receiver, evaluated once, from a local
        // whose name source cannot spell.
        let scope = ctx.scope_start();
        let name = format!("{callee} argument@{}", span.start);
        let bound = self.bind_hidden_local(ctx, id, &name, value, receiver.span());
        let arg = ast::Expr::Name {
            id,
            name,
            span: receiver.span(),
        };
        let call =
            self.lower_built_in_call(ctx, id, callee, &[arg], keywords, expected_results, span);
        ctx.hide_locals_from(scope);
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: vec![Statement::Let(bound)],
                result: call,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Lowers a call of `callee`, a definition of the program.
    #[allow(clippy::too_many_arguments)]
    fn lower_definition_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        if let Some(signature) = self.keyword_signature(callee).cloned() {
            return self.lower_keyword_call(ctx, id, callee, &signature, args, keywords, span);
        }
        // A constant holds a value, which the call calls as any operand's.
        if let Some(constant) = self.resolve_constant(callee).cloned()
            && self.resolve_function(callee).is_none()
            && self.resolve_host_function(callee).is_none()
            && self.resolve_generic(callee).is_none()
        {
            let target = Expr::ConstantRef {
                constant: constant.id,
                result_kind: constant.kind,
                callable_signature: constant.callable_signature,
                span,
            };
            let signatures = self
                .callable_signature_of(&target)
                .map(|signature| signature.param_sigs)
                .unwrap_or_default();
            let lowered_args = self.lower_args_against(ctx, args, &signatures);
            return self.lower_value_call(
                ctx,
                id,
                None,
                target,
                lowered_args,
                keywords,
                expected_results,
                span,
            );
        }
        for keyword in keywords {
            ctx.diagnostics
                .push(Self::unexpected_keyword(callee, keyword));
        }
        // An anonymous method passed where a function takes a typed callable
        // takes its types from that signature.
        let signatures = self
            .resolve_function(callee)
            .map(|symbol| symbol.param_callable_sigs.clone())
            .unwrap_or_default();
        let mut lowered_args = self.lower_args_against(ctx, args, &signatures);
        if let Some(symbol) = self.resolve_function(callee).cloned() {
            return self.lower_function_call(ctx, &symbol, lowered_args, callee, span);
        }
        if let Some(function) = self.resolve_host_function(callee).cloned() {
            if function.param_kinds.len() == args.len() {
                adapt_literal_args(&mut lowered_args, function.param_kinds.iter().map(Some));
                self.narrow_args(&mut lowered_args, &function.param_kinds, &[]);
                for (index, (arg, expected)) in
                    lowered_args.iter().zip(&function.param_kinds).enumerate()
                {
                    let actual = self.expr_kind(arg);
                    if !self.kinds_match(*expected, actual) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                arg.span(),
                                DiagnosticCode::FunctionArgumentTypeMismatch,
                            )
                            .with_arg(DiagnosticArg::text("function", callee))
                            .with_arg(DiagnosticArg::unsigned(
                                "argument_index",
                                u64::try_from(index + 1).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::text("expected", self.kind_name(*expected)))
                            .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                        );
                    }
                }
                return Expr::HostCall {
                    function: function.id,
                    args: lowered_args,
                    result_kind: function.result_kind,
                    span,
                };
            }
            ctx.diagnostics.push(Self::arity_mismatch(
                callee,
                function.param_kinds.len(),
                args.len(),
                span,
            ));
            return Expr::Error { span };
        }
        // A slot's getter reads a receiver of the slot's class directly.
        if self.slot_getter_named(callee).is_some() && lowered_args.len() == 1 {
            let receiver = lowered_args.into_iter().next().expect("one argument");
            return self.lower_accessor_call(ctx, receiver, callee, span);
        }
        if let Some(symbol) = self.resolve_generic(callee).cloned() {
            return self.lower_generic_send(ctx, &symbol, lowered_args, callee, span);
        }
        // A slot getter whose name a definition of the program takes too,
        // the one definition left.
        if lowered_args.len() != 1 {
            ctx.diagnostics
                .push(Self::arity_mismatch(callee, 1, args.len(), span));
            return Expr::Error { span };
        }
        let receiver = lowered_args.into_iter().next().expect("arity was checked");
        self.lower_accessor_call(ctx, receiver, callee, span)
    }

    /// Lowers a call of `callee`, a built-in of the kernel's, since no
    /// binding or definition of the program takes its name.
    #[allow(clippy::too_many_arguments)]
    fn lower_built_in_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        if let Some(function) = CollectionFunction::from_name(callee)
            .or_else(|| CollectionFunction::from_kernel_name(callee))
        {
            return self.lower_collection_call(ctx, id, function, args, keywords, span);
        }
        match callee {
            "copy-sequence" | "starts-with?" => {
                return self.lower_keyword_text_call(ctx, id, callee, args, keywords, span);
            }
            "string-to-integer" => {
                return self.lower_string_to_integer(ctx, id, args, keywords, span);
            }
            "make" => return self.lower_make_call(ctx, args, keywords, span),
            "limited" => {
                // `limited(…)` that writes a type is a type literal; this
                // one does not.
                for arg in args {
                    let _ = self.lower_expr(ctx, arg);
                }
                for keyword in keywords {
                    let _ = self.lower_expr(ctx, &keyword.value);
                }
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    DiagnosticCode::AstInvalidTypeLiteral,
                ));
                return Expr::Error { span };
            }
            _ => {}
        }
        // No other built-in takes keyword arguments.
        for keyword in keywords {
            ctx.diagnostics
                .push(Self::unexpected_keyword(callee, keyword));
        }
        if let Some(rounding) = Rounding::from_name(callee) {
            return self.lower_integer_division(ctx, id, rounding, args, span, expected_results);
        }
        match callee {
            "apply" => return self.lower_apply(ctx, id, args, expected_results, span),
            "list" | ast::KERNEL_LIST => {
                let elements = args
                    .iter()
                    .map(|arg| self.lower_expr(ctx, arg))
                    .collect::<Vec<_>>();
                return self.lower_list(ctx, elements, span);
            }
            // The functions `\==`, `\~==` and `\~=` name, which no call
            // of a name reaches: their operators' tests of two objects.
            "==" | "~==" | "~=" if args.len() == 2 => {
                let (lhs, rhs) = (Box::new(args[0].clone()), Box::new(args[1].clone()));
                let test = if callee == "~=" {
                    ast::Expr::Compare {
                        id,
                        op: ast::CompareOp::Eq,
                        lhs,
                        rhs,
                        span,
                    }
                } else {
                    ast::Expr::Identical { id, lhs, rhs, span }
                };
                let test = if callee == "==" {
                    test
                } else {
                    ast::negation(id, test, span)
                };
                return self.lower_expr(ctx, &test);
            }
            "values" | KERNEL_VALUES => {
                let mut values = args
                    .iter()
                    .map(|arg| self.lower_expr(ctx, arg))
                    .collect::<Vec<_>>();
                if let Some(expected) = expected_results
                    && expected.len() == values.len()
                {
                    adapt_literal_args(&mut values, expected.iter().map(Some));
                }
                let result_kinds = values
                    .iter()
                    .map(|value| self.expr_kind(value))
                    .collect::<Vec<_>>();
                return Expr::Values {
                    values,
                    result_kinds,
                    span,
                };
            }
            "signal" => {
                let [condition] = args else {
                    ctx.diagnostics.push(
                        Diagnostic::error(span, DiagnosticCode::AstInvalidSignal)
                            .with_arg(DiagnosticArg::unsigned("expected_count", 1))
                            .with_arg(DiagnosticArg::unsigned(
                                "actual_count",
                                u64::try_from(args.len()).unwrap_or(u64::MAX),
                            )),
                    );
                    return Expr::Error { span };
                };
                return self.lower_signal(ctx, condition, expected_results, span);
            }
            "error" => return self.lower_error(ctx, id, args, expected_results, span),
            "as" => return self.lower_as_call(ctx, args, span),
            "instance?" => {
                if let [value, target] = args
                    && let Some(target) = self.written_type(ctx, target)
                {
                    return self.lower_instance_of(ctx, value, &target, span);
                }
                // A type the program computes, such as a `<type-error>`'s
                // expected type, is tested by the intrinsic below.
            }
            _ => {}
        }
        let lowered_args: Vec<_> = args.iter().map(|arg| self.lower_expr(ctx, arg)).collect();
        if let Some(symbol) = self.library_function(callee) {
            return self.lower_function_call(ctx, &symbol, lowered_args, callee, span);
        }
        if callee == "concatenate" {
            return self.lower_concatenate(ctx, lowered_args, span);
        }
        // A built-in with several overloads sends to its generic when no
        // overload takes the arguments' types: an `<object>` among them, or
        // a class a program's method joined the generic for.
        if self.sends_to_built_in_generic(callee, &lowered_args)
            && let Some(generic) = self.built_in_generic_named(callee)
            && let Some(symbol) = self
                .generic_symbols
                .iter()
                .find(|symbol| symbol.id == generic)
                .cloned()
        {
            return self.lower_generic_send(ctx, &symbol, lowered_args, callee, span);
        }
        if callee == "class-of" {
            if lowered_args.len() != 1 {
                ctx.diagnostics
                    .push(Self::arity_mismatch(callee, 1, args.len(), span));
                return Expr::Error { span };
            }
            let object = lowered_args.into_iter().next().expect("arity was checked");
            return self.lower_class_of(ctx, object, span);
        }
        if let Some(call) = self.lower_intrinsic_call(ctx, callee, &lowered_args, span) {
            return call;
        }
        // Rounding an integer leaves it as it is.
        if matches!(callee, "floor" | "ceiling" | "round" | "truncate")
            && let [value] = lowered_args.as_slice()
            && matches!(
                self.expr_kind(value),
                ValueKind::I64 | ValueKind::I32 | ValueKind::U32
            )
        {
            return lowered_args
                .into_iter()
                .next()
                .expect("one argument was matched");
        }
        if let Some(builtin) = FloatBuiltin::from_name(callee) {
            return self.lower_float_call(ctx, builtin, lowered_args, span);
        }
        if let Some(functions) = text_functions_for_name(callee) {
            return self.lower_text_call(ctx, callee, functions, lowered_args, span);
        }
        if callee == ast::POSITION_FROM_OFFSET {
            return self.lower_text_call(
                ctx,
                callee,
                &[TextFunction::PositionFromOffset],
                lowered_args,
                span,
            );
        }
        if let Some(op) = BinaryOp::bitwise_function(callee) {
            return self.lower_bitwise(ctx, op, lowered_args, span);
        }
        ctx.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::UnresolvedFunctionOrGeneric)
                .with_arg(DiagnosticArg::text("callee", callee)),
        );
        Expr::Error { span }
    }

    /// Lowers a call of the kernel's `make`: `make(<class>, slot: value,
    /// ...)`, whose one positional argument is the class.
    fn lower_make_call(
        &mut self,
        ctx: &mut FunctionContext,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let [class] = args else {
            for arg in args {
                let _ = self.lower_expr(ctx, arg);
            }
            for keyword in keywords {
                let _ = self.lower_expr(ctx, &keyword.value);
            }
            ctx.diagnostics
                .push(Self::arity_mismatch("make", 1, args.len(), span));
            return Expr::Error { span };
        };
        self.lower_make_object(ctx, class, keywords, span)
    }

    /// Lowers a call of the kernel's `as`: `as(type, value)`, whose type is
    /// written, or `as(<string-position>, s, n)`, the position at byte
    /// offset `n` of `s`.
    fn lower_as_call(&mut self, ctx: &mut FunctionContext, args: &[ast::Expr], span: Span) -> Expr {
        if let [target, string, offset] = args
            && matches!(target, ast::Expr::ClassName { class_name, .. } if class_name == "<string-position>")
        {
            let args = vec![self.lower_expr(ctx, string), self.lower_expr(ctx, offset)];
            return self.lower_text_call(
                ctx,
                ast::POSITION_FROM_OFFSET,
                &[TextFunction::PositionFromOffset],
                args,
                span,
            );
        }
        let [target, value] = args else {
            for arg in args {
                let _ = self.lower_expr(ctx, arg);
            }
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::AstInvalidCast)
                    .with_arg(DiagnosticArg::unsigned("expected_count", 2))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error { span };
        };
        let Some(target) = self.written_type(ctx, target) else {
            let _ = self.lower_expr(ctx, value);
            ctx.diagnostics
                .push(Diagnostic::error(span, DiagnosticCode::AstInvalidCast));
            return Expr::Error { span };
        };
        self.lower_cast(ctx, value, &target, span)
    }

    /// The type `arg`, an argument of the kernel's `as` or `instance?`,
    /// writes: a type name, or a `limited(...)` type literal while no
    /// binding or definition of the program takes `limited`. Any other
    /// argument computes a type object.
    fn written_type(&self, ctx: &FunctionContext, arg: &ast::Expr) -> Option<ast::TypeExpr> {
        match arg {
            ast::Expr::ClassName {
                class_name, span, ..
            } => Some(ast::TypeExpr::Named {
                name: class_name.clone(),
                span: *span,
            }),
            ast::Expr::TypeLiteral { target, .. }
                if !self.binding_or_definition(ctx, "limited") =>
            {
                Some(target.clone())
            }
            _ => None,
        }
    }

    /// Lowers the type `target` written as a value: a type object.
    fn lower_type_literal(
        &mut self,
        ctx: &mut FunctionContext,
        target: &ast::TypeExpr,
        span: Span,
    ) -> Expr {
        match self.resolve_type_annotation_detail_in_context(ctx, target) {
            ResolvedType {
                kind: ValueKind::I64,
                range,
                ..
            } => self.lower_type_object(
                TypeObject::limited(range.unwrap_or(IntRange::INTEGER)),
                span,
            ),
            _ => Expr::Error { span },
        }
    }

    /// Lowers `limited(...)`, written as a type, while a binding or
    /// definition of the program takes `limited`: a call of it with the
    /// base type and the bounds as written. A bound that is not an
    /// `<integer>` literal is reported, since only the type kept it.
    fn lower_shadowed_type_literal(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        target: &ast::TypeExpr,
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let ast::TypeExpr::Limited(limited) = target else {
            return Expr::Error { span };
        };
        let as_value = |target: &ast::TypeExpr| match target {
            ast::TypeExpr::Named { name, span } => ast::Expr::ClassName {
                id,
                class_name: name.clone(),
                span: *span,
            },
            target => ast::Expr::TypeLiteral {
                id,
                target: target.clone(),
                span: target.span(),
            },
        };
        let args = vec![as_value(&limited.base)];
        let mut keywords = Vec::new();
        for option in &limited.options {
            let Some(value) = option.value else {
                ctx.diagnostics.push(
                    Diagnostic::error(option.span, DiagnosticCode::LimitedTypeBound)
                        .with_arg(DiagnosticArg::text("option", option.keyword.clone()))
                        .with_arg(DiagnosticArg::text("bound", option.text.clone())),
                );
                return Expr::Error { span };
            };
            keywords.push(ast::KeywordArg {
                id,
                name: option.keyword.clone(),
                value: ast::Expr::Number {
                    id,
                    value,
                    span: option.span,
                },
                span: option.span,
            });
        }
        self.lower_named_call(ctx, id, "limited", &args, &keywords, expected_results, span)
    }

    /// Resolves a call's callee to a lexical `<callable>` local or capture.
    ///
    /// Lexical bindings shadow module definitions, so `exit(42)` calls a
    /// block's exit procedure and `f(x)` calls a local closure. Locals of any
    /// other kind leave the name to module resolution.
    /// The local or captured binding named `name`, read, when there is
    /// one: a call of `name` calls its value.
    fn local_callable_target(
        &self,
        ctx: &mut FunctionContext,
        name: &str,
        span: Span,
    ) -> Option<Expr> {
        if let Some(local) = ctx.find_local(name) {
            return Some(Expr::Local {
                local,
                result_kind: ctx.local_kind(local),
                callable_signature: ctx.local_callable_signature(local),
                span,
            });
        }
        if !ctx.binds_lexically(name) {
            return None;
        }
        let capture = ctx.find_capture_or_add(name)?;
        Some(Expr::Capture {
            capture,
            result_kind: ctx.capture_kind(capture),
            callable_signature: ctx.capture_callable_signature(capture),
            span,
        })
    }

    fn lower_known_name(&mut self, ctx: &mut FunctionContext, name: &str, span: Span) -> Expr {
        if let Some(symbol) = self.resolve_constant(name) {
            return Expr::ConstantRef {
                constant: symbol.id,
                result_kind: symbol.kind,
                callable_signature: symbol.callable_signature.clone(),
                span,
            };
        }
        if let Some(symbol) = self.resolve_function(name) {
            return Expr::KnownCallable {
                callable: KnownCallable::DirectFunction(symbol.id),
                span,
            };
        }
        if let Some(symbol) = self.resolve_generic(name) {
            return Expr::KnownCallable {
                callable: KnownCallable::GenericFunction(symbol.id),
                span,
            };
        }
        // A slot's getter is a generic, made the first time a value names
        // it.
        if let Some(generic) = self.slot_getter_named(name) {
            return Expr::KnownCallable {
                callable: KnownCallable::GenericFunction(generic),
                span,
            };
        }
        // `\+` names the built-in generic the operator sends to.
        if let Some(operator) = operator_generic(name) {
            return Expr::KnownCallable {
                callable: KnownCallable::GenericFunction(self.numeric_generic(operator)),
                span,
            };
        }
        if let Some(callable) = self.built_in_value(name) {
            return Expr::KnownCallable { callable, span };
        }
        let diagnostic = match self.spaced_hyphen_suggestion(ctx, name) {
            _ if built_in::FORMS.contains(&name) => {
                Diagnostic::error(span, DiagnosticCode::BuiltInFormValue)
                    .with_arg(DiagnosticArg::text("function", name))
            }
            Some(spaced) => Diagnostic::error(span, DiagnosticCode::UnresolvedHyphenatedLocal)
                .with_arg(DiagnosticArg::text("local", name))
                .with_arg(DiagnosticArg::text("spaced", spaced)),
            None => Diagnostic::error(span, DiagnosticCode::UnresolvedLocal)
                .with_arg(DiagnosticArg::text("local", name)),
        };
        ctx.diagnostics.push(diagnostic);
        Expr::Error { span }
    }

    /// Suggests the spaced form of an unresolved hyphenated name.
    ///
    /// A hyphen is a name character, so `x-1` is one name rather than a
    /// subtraction. When the name splits at a hyphen into a value that is in
    /// scope and either a number or another value in scope, the author most
    /// likely meant to subtract: `x-1` suggests `x - 1` and `a-b` suggests
    /// `a - b`. The first such split wins.
    fn spaced_hyphen_suggestion(&self, ctx: &FunctionContext, name: &str) -> Option<String> {
        let is_value = |text: &str| syntax::is_name(text) && self.names_value(ctx, text);
        name.match_indices('-').find_map(|(index, _)| {
            let (left, right) = (&name[..index], &name[index + 1..]);
            (is_value(left) && (syntax::is_number(right) || is_value(right)))
                .then(|| format!("{left} - {right}"))
        })
    }

    /// Whether `name` resolves as a value at this point, without recording
    /// a closure capture the way resolving it for real would.
    fn names_value(&self, ctx: &FunctionContext, name: &str) -> bool {
        ctx.binds(name)
            || self.resolve_constant(name).is_some()
            || self.resolve_function(name).is_some()
            || self.resolve_generic(name).is_some()
            || self.has_accessor(name)
    }

    /// Lowers `local method …, method …;`, binding each method to its name
    /// for the rest of the body: one `let` of the group's closures.
    ///
    /// Every name is bound before any method is lowered, so each may call
    /// itself and the others. A method that writes its result types is
    /// called through its signature there; one that writes none, through
    /// an untyped `<function>`, since its results are known only once its
    /// body is. Calls after the group see each method's own signature. When
    /// a method calls one of the group, the bindings live in shared cells
    /// that exist before the closures capturing them are made: a `let`
    /// whose closures capture the locals it binds is the IR's `letrec`,
    /// which lowering makes the cells for first.
    fn lower_local_methods(
        &mut self,
        ctx: &mut FunctionContext,
        methods: &[ast::LocalMethod],
    ) -> Statement {
        let recursive = ast::local_methods_recursive(methods);
        let mut locals = Vec::with_capacity(methods.len());
        for method in methods {
            if ctx.find_local(&method.name).is_some() {
                ctx.diagnostics.push(
                    Diagnostic::error(method.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", method.name.clone())),
                );
            }
            let signature = self.declared_method_signature(&method.function);
            let local = ctx.add_local(
                self.local_definition(method.definition, &method.name),
                method.name.clone(),
                ValueKind::Callable,
                signature,
                method.span,
            );
            if recursive && let Some(local) = ctx.locals.get_mut(local.0 as usize) {
                local.shared = true;
            }
            locals.push(local);
        }
        let values = methods
            .iter()
            .map(|method| self.lower_expr(ctx, &method.function))
            .collect::<Vec<_>>();
        // Calls after the group, and only those, call each method as it is.
        for (value, local) in values.iter().zip(&locals) {
            let signature = self.expr_callable_signature(value);
            if let Some(binding) = ctx.locals.get_mut(local.0 as usize) {
                binding.callable_signature = signature;
            }
        }
        let span = methods
            .iter()
            .map(|method| method.function.span())
            .reduce(Span::cover)
            .unwrap_or_default();
        let value = match <[Expr; 1]>::try_from(values) {
            Ok([value]) => value,
            Err(values) => Expr::Values {
                result_kinds: vec![ValueKind::Callable; values.len()],
                values,
                span,
            },
        };
        Statement::Let(Let {
            local: locals[0],
            locals,
            value,
            span,
        })
    }

    /// The signature a local method declares, before its body is lowered:
    /// its parameters' written types or `<object>`, a `#rest` parameter's
    /// `<vector>`, and its written results. `None` when it writes no
    /// results, which only its body gives.
    fn declared_method_signature(&mut self, function: &ast::Expr) -> Option<CallableSignature> {
        let ast::Expr::Function {
            params,
            rest,
            return_type,
            ..
        } = function
        else {
            return None;
        };
        // Lowering the method reports what its types get wrong.
        let reported = self.diagnostics.len();
        let results = self.resolve_result_annotation_detail(return_type.as_ref()?);
        let mut resolved = params
            .iter()
            .map(|param| {
                self.resolve_function_type_detail(param.type_annotation.as_ref(), ValueKind::Any)
            })
            .collect::<Vec<_>>();
        self.diagnostics.truncate(reported);
        if rest.is_some() {
            resolved.push(self.rest_param_type());
        }
        Some(CallableSignature {
            params: resolved.iter().map(|param| param.kind).collect(),
            param_sigs: resolved
                .iter()
                .map(|param| param.callable_signature.clone())
                .collect(),
            param_ranges: resolved.iter().map(|param| param.range).collect(),
            results: results.kinds,
            result_sigs: results.callable_sigs,
            result_ranges: results.ranges,
            keywords: Vec::new(),
            rest: rest.is_some(),
        })
    }

    /// The type of a `#rest` parameter: a `<vector>` of the arguments
    /// after the required ones, which the call makes.
    fn rest_param_type(&self) -> ResolvedType {
        ResolvedType {
            kind: ValueKind::Vector(
                self.builtin_value_class(BuiltinValueClass::Vector)
                    .unwrap_or(ClassId(u32::MAX)),
            ),
            callable_signature: None,
            range: None,
        }
    }

    /// Lowers the anonymous method `method (params) => return_type body
    /// end`, taking what it leaves unwritten from the `expected` signature
    /// of the position it is written in; see [`ExpectedSignature`].
    #[allow(clippy::too_many_arguments)]
    fn lower_closure_expr(
        &mut self,
        ctx: &mut FunctionContext,
        params: &[ast::FunctionParam],
        rest: Option<&ast::FunctionParam>,
        return_type: Option<&ast::ResultAnnotation>,
        body: &ast::Block,
        span: Span,
        expected: Option<&CallableSignature>,
    ) -> Expr {
        let id = ClosureId(u32::try_from(self.closures.len()).unwrap_or(u32::MAX));
        self.closures.push(Closure {
            id,
            params: Vec::new(),
            param_kinds: Vec::new(),
            param_callable_sigs: Vec::new(),
            param_ranges: Vec::new(),
            captures: Vec::new(),
            locals: Vec::new(),
            body: Block {
                statements: Vec::new(),
                result: Expr::Error { span },
                span,
            },
            result_kinds: vec![ValueKind::Error],
            result_callable_sigs: vec![None],
            result_ranges: vec![None],
            return_annotation: None,
            entry_checks: Vec::new(),
            rest: false,
            restart_guard: false,
            expected: None,
            span,
        });

        // A signature of the method's arity without keywords lends its
        // types to what the method leaves unwritten; a method taking
        // `#rest` takes none.
        let expected = expected.filter(|signature| {
            signature.params.len() == params.len()
                && signature.keywords.is_empty()
                && !signature.rest
                && rest.is_none()
        });
        let mut closure_ctx =
            FunctionContext::for_closure(ctx.visible_local_sources(), ctx.exit_definitions.clone());
        closure_ctx.shared_names.clone_from(&ctx.shared_names);
        closure_ctx.loop_variables.clone_from(&ctx.loop_variables);
        let mut param_ids = Vec::new();
        let mut param_kinds = Vec::new();
        let mut param_callable_sigs = Vec::new();
        let mut param_ranges = Vec::new();
        for (index, param) in params.iter().enumerate() {
            if closure_ctx.find_local(&param.name).is_some() {
                closure_ctx.diagnostics.push(
                    Diagnostic::error(param.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", param.name.clone())),
                );
            }
            // An unwritten type is the expected parameter's, or `<object>`.
            let resolved = match (param.type_annotation.as_ref(), expected) {
                (None, Some(expected)) => ResolvedType {
                    kind: expected.params[index],
                    callable_signature: expected.param_sig(index).cloned(),
                    range: expected.param_range(index),
                },
                (annotation, _) => self.resolve_function_type_detail(annotation, ValueKind::Any),
            };
            param_kinds.push(resolved.kind);
            param_callable_sigs.push(resolved.callable_signature.clone());
            param_ranges.push(resolved.range);
            let local = closure_ctx.add_local(
                DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Local,
                    &param.name,
                    &format!("closure:{}:param", id.0),
                ),
                param.name.clone(),
                resolved.kind,
                resolved.callable_signature,
                param.span,
            );
            closure_ctx.limit_local(local, resolved.range);
            closure_ctx.allow_assignment(local);
            param_ids.push(local);
        }
        // A `#rest` parameter is the last, a `<vector>` the call makes.
        if let Some(rest) = rest {
            if closure_ctx.find_local(&rest.name).is_some() {
                closure_ctx.diagnostics.push(
                    Diagnostic::error(rest.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", rest.name.clone())),
                );
            }
            let kind = self.rest_param_type().kind;
            param_kinds.push(kind);
            param_callable_sigs.push(None);
            param_ranges.push(None);
            let local = closure_ctx.add_local(
                DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Local,
                    &rest.name,
                    &format!("closure:{}:param", id.0),
                ),
                rest.name.clone(),
                kind,
                None,
                rest.span,
            );
            closure_ctx.allow_assignment(local);
            param_ids.push(local);
        }

        let (return_annotation, annotated_return_callable_sigs, annotated_ranges) =
            match (return_type, expected) {
                (Some(annotation), _) => {
                    let resolved = self.resolve_result_annotation_detail(annotation);
                    (
                        Some(resolved.kinds),
                        resolved.callable_sigs,
                        resolved.ranges,
                    )
                }
                // Unwritten results are the expected ones.
                (None, Some(expected)) => (
                    Some(expected.results.clone()),
                    expected.result_sigs.clone(),
                    expected.result_ranges.clone(),
                ),
                (None, None) => (None, Vec::new(), Vec::new()),
            };
        let mut lowered_body = self.lower_result_block(
            &mut closure_ctx,
            body,
            return_annotation.as_deref(),
            &annotated_return_callable_sigs,
        );
        if return_annotation.as_deref() == Some(&[]) {
            discard_results(&mut lowered_body);
        } else if let Some(expected) = return_annotation.as_deref() {
            let result = core::mem::replace(
                &mut lowered_body.result,
                Expr::Error {
                    span: lowered_body.span,
                },
            );
            lowered_body.result = self.narrow_results(
                &mut closure_ctx,
                result,
                expected,
                &annotated_return_callable_sigs,
                "closure-result",
            );
        }
        let body_kinds = self.expr_kinds(&lowered_body.result);
        let body_callable_sigs = self.expr_result_callable_signatures(&lowered_body.result);
        let result_kinds = return_annotation
            .clone()
            .unwrap_or_else(|| body_kinds.clone());
        let result_callable_sigs = if return_annotation.is_some() {
            annotated_return_callable_sigs
        } else {
            body_callable_sigs.clone()
        };
        if let Some(expected) = &return_annotation {
            let mismatch = |expected: String, actual: String| {
                Diagnostic::error(
                    body.result.span(),
                    DiagnosticCode::FunctionReturnTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("function", "<closure>"))
                .with_arg(DiagnosticArg::text("expected", expected))
                .with_arg(DiagnosticArg::text("actual", actual))
            };
            if self.result_kinds_match(expected, &body_kinds) {
                closure_ctx
                    .diagnostics
                    .extend(self.callable_results_diagnostics(
                        body.result.span(),
                        &result_callable_sigs,
                        &body_kinds,
                        &body_callable_sigs,
                        mismatch,
                    ));
            } else {
                closure_ctx.diagnostics.push(mismatch(
                    self.result_kinds_name(expected),
                    self.result_kinds_name(&body_kinds),
                ));
            }
        }

        let result_ranges = if return_annotation.is_some() {
            annotated_ranges
        } else {
            vec![None; result_kinds.len()]
        };
        let result_kinds = self.check_result_ranges(
            &mut closure_ctx,
            &mut lowered_body,
            &result_kinds,
            &result_callable_sigs,
            &result_ranges,
        );
        let captures = closure_ctx.captures.clone();
        self.diagnostics.extend(closure_ctx.diagnostics);
        // An assignment through a capture assigns the enclosing local.
        for local in &closure_ctx.assigned_captures {
            ctx.note_assignment(*local);
        }
        if let Some(slot) = self.closures.get_mut(id.0 as usize) {
            *slot = Closure {
                id,
                params: param_ids,
                param_kinds: param_kinds.clone(),
                param_callable_sigs: param_callable_sigs.clone(),
                param_ranges: param_ranges.clone(),
                captures: captures.clone(),
                locals: closure_ctx.locals,
                body: lowered_body,
                result_kinds: result_kinds.clone(),
                result_callable_sigs: result_callable_sigs.clone(),
                result_ranges: result_ranges.clone(),
                return_annotation,
                entry_checks: Vec::new(),
                rest: rest.is_some(),
                restart_guard: false,
                expected: expected
                    .map(|signature| ExpectedSignature {
                        signature: signature.clone(),
                        params: params
                            .iter()
                            .map(|param| param.type_annotation.is_none())
                            .collect(),
                        results: return_type.is_none(),
                    })
                    .filter(ExpectedSignature::takes_anything),
                span,
            };
        }

        let callable_signature = CallableSignature {
            params: param_kinds,
            param_sigs: param_callable_sigs,
            param_ranges,
            results: result_kinds,
            result_sigs: result_callable_sigs,
            result_ranges,
            keywords: Vec::new(),
            rest: rest.is_some(),
        };
        Expr::Closure {
            closure: id,
            captures,
            result_kind: ValueKind::Callable,
            callable_signature,
            span,
        }
    }

    /// Lowers Dylan's `apply(f, a, …, sequence)`: a call of `f` with `a,
    /// …` and then each element of `sequence`, which it spreads. `f` is
    /// evaluated first, then the arguments in order. The arguments travel
    /// in one `<vector>`, `sequence` itself when nothing precedes it and it
    /// is one, and lowering calls the value with its elements
    /// ([`CallArguments::Spread`]), checked by its entry as any boxed
    /// call's are. `apply` passes no keywords.
    fn lower_apply(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        args: &[ast::Expr],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let [target, rest @ ..] = args else {
            ctx.diagnostics
                .push(Diagnostic::error(span, DiagnosticCode::ApplyArityMismatch));
            return Expr::Error { span };
        };
        let Some((sequence, prefix)) = rest.split_last() else {
            ctx.diagnostics
                .push(Diagnostic::error(span, DiagnosticCode::ApplyArityMismatch));
            return Expr::Error { span };
        };
        let scope = ctx.scope_start();
        let mut statements = Vec::new();
        let target = self.lower_expr(ctx, target);
        let target = match self.expr_kind(&target) {
            ValueKind::Callable
            | ValueKind::KnownCallable(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::Error => target,
            ValueKind::Any => self.narrow(target, ValueKind::Callable, None),
            actual => {
                ctx.diagnostics.push(
                    Diagnostic::error(target.span(), DiagnosticCode::ApplyTargetTypeMismatch)
                        .with_arg(DiagnosticArg::text("expected", "<callable>"))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
                Expr::Error { span }
            }
        };
        // The function is evaluated before the arguments, which may assign
        // a local holding it; only a binding of a definition always holds
        // the same one.
        let target = match target {
            Expr::KnownCallable { .. } | Expr::Error { .. } => target,
            target if matches!(self.expr_kind(&target), ValueKind::KnownCallable(_)) => target,
            target => {
                let name = format!("{APPLY_FUNCTION}@{}", span.start);
                let bound = self.bind_hidden_local(ctx, id, &name, target, span);
                let read = Expr::Local {
                    local: bound.local,
                    result_kind: ctx.local_kind(bound.local),
                    callable_signature: ctx.local_callable_signature(bound.local),
                    span,
                };
                statements.push(Statement::Let(bound));
                read
            }
        };
        let prefix = prefix
            .iter()
            .map(|arg| self.lower_expr(ctx, arg))
            .collect::<Vec<_>>();
        let sequence_span = sequence.span();
        let sequence = self.lower_expr(ctx, sequence);
        let arguments =
            if prefix.is_empty() && matches!(self.expr_kind(&sequence), ValueKind::Vector(_)) {
                sequence
            } else {
                self.apply_arguments(
                    ctx,
                    id,
                    prefix,
                    sequence,
                    sequence_span,
                    &mut statements,
                    span,
                )
            };
        if self.expr_kind(&target) == ValueKind::Error
            || self.expr_kind(&arguments) == ValueKind::Error
        {
            ctx.hide_locals_from(scope);
            return Expr::Error { span };
        }
        let arguments_kind = self.expr_kind(&arguments);
        let signature = match self.expr_kind(&target) {
            ValueKind::GenericFunctionObject(generic) => {
                self.known_callable_signature(KnownCallable::GenericFunction(generic))
            }
            _ => self.callable_signature_of(&target),
        };
        let call = match signature {
            // A block's exit procedure never returns, so nothing relies on
            // its results.
            Some(signature) if ctx.is_block_exit(&target) => {
                let result_len = signature.results.len();
                Expr::CallableCall {
                    callable: Box::new(target),
                    args: vec![arguments],
                    arguments: CallArguments::Spread,
                    arg_kinds: vec![arguments_kind],
                    keywords: Vec::new(),
                    result_kinds: signature.results,
                    result_callable_sigs: vec![None; result_len],
                    span,
                }
            }
            Some(signature) => {
                let called = CallableSignature {
                    results: signature.results,
                    result_sigs: signature.result_sigs,
                    result_ranges: signature.result_ranges,
                    ..CallableSignature::flat(vec![arguments_kind], Vec::new())
                };
                self.checked_callable_call(
                    ctx,
                    target,
                    vec![arguments],
                    CallArguments::Spread,
                    called,
                    Self::signature_result_check,
                    span,
                )
                .unwrap_or_else(|diagnostic| {
                    ctx.diagnostics.push(diagnostic);
                    Expr::Error { span }
                })
            }
            None => match expected_results {
                Some(result_kinds) => self
                    .checked_callable_call(
                        ctx,
                        target,
                        vec![arguments],
                        CallArguments::Spread,
                        CallableSignature::flat(vec![arguments_kind], result_kinds.to_vec()),
                        Self::untyped_result_check,
                        span,
                    )
                    .unwrap_or_else(|diagnostic| {
                        ctx.diagnostics.push(diagnostic);
                        Expr::Error { span }
                    }),
                None => {
                    ctx.diagnostics.push(Diagnostic::error(
                        target.span(),
                        DiagnosticCode::UntypedCallableResult,
                    ));
                    Expr::Error { span }
                }
            },
        };
        ctx.hide_locals_from(scope);
        if statements.is_empty() {
            return call;
        }
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements,
                result: call,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// The `<vector>` of `apply`'s arguments, `prefix` and then each
    /// element of `sequence`, which is checked to be a `<sequence>`: a
    /// `<stretchy-vector>` the statements pushed to `statements` fill, in
    /// order. A `for` collects the elements, so any sequence the iteration
    /// protocol covers spreads.
    #[allow(clippy::too_many_arguments)]
    fn apply_arguments(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        prefix: Vec<Expr>,
        sequence: Expr,
        sequence_span: Span,
        statements: &mut Vec<Statement>,
        span: Span,
    ) -> Expr {
        let collected = format!("{APPLY_ARGUMENTS}@{}", span.start);
        let make = Expr::IntrinsicCall {
            intrinsic: Intrinsic::MakeStretchyVector,
            args: vec![
                Expr::ConstI64 { value: 0, span },
                Expr::ConstBool { value: false, span },
            ],
            result_kind: self.intrinsic_result_kind(Intrinsic::MakeStretchyVector),
            span,
        };
        let bound = self.bind_hidden_local(ctx, id, &collected, make, span);
        let read = Expr::Local {
            local: bound.local,
            result_kind: ctx.local_kind(bound.local),
            callable_signature: None,
            span,
        };
        statements.push(Statement::Let(bound));
        for arg in prefix {
            let arg_span = arg.span();
            statements.push(Statement::Expr(Expr::IntrinsicCall {
                intrinsic: Intrinsic::StretchyAdd,
                args: vec![read.clone(), arg],
                result_kind: self.intrinsic_result_kind(Intrinsic::StretchyAdd),
                span: arg_span,
            }));
        }
        let actual = self.expr_kind(&sequence);
        let expected = self
            .resolve_class("<sequence>")
            .map_or(ValueKind::Error, |class| ValueKind::Object(class.id));
        let sequence = self.narrow(sequence, expected, None);
        if actual != ValueKind::Error && !self.kinds_match(expected, self.expr_kind(&sequence)) {
            ctx.diagnostics.push(
                Diagnostic::error(sequence_span, DiagnosticCode::ApplySequenceTypeMismatch)
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
            );
            return Expr::Error { span };
        }
        let elements = format!("{APPLY_SEQUENCE}@{}", span.start);
        statements.push(Statement::Let(self.bind_hidden_local(
            ctx,
            id,
            &elements,
            sequence,
            sequence_span,
        )));
        let name = |name: &str| ast::Expr::Name {
            id,
            name: name.into(),
            span: sequence_span,
        };
        let element = format!("{APPLY_ELEMENT}@{}", span.start);
        let collect = ast::Expr::For {
            id,
            clauses: vec![ast::ForClause::Collection(ast::CollectionFor {
                binding: ast::LetBinding {
                    definition: DefinitionId::new(
                        self.source_file,
                        DefinitionKind::Local,
                        &element,
                        &id.raw().to_string(),
                    ),
                    name: element.clone(),
                    type_annotation: None,
                    span: sequence_span,
                },
                collection: Box::new(name(&elements)),
                span: sequence_span,
            })],
            body: Box::new(ast::Block {
                id,
                statements: Vec::new(),
                result: ast::Expr::Call {
                    id,
                    callee: CollectionFunction::Add.kernel_name().into(),
                    args: vec![name(&collected), name(&element)],
                    keywords: Vec::new(),
                    span: sequence_span,
                },
                span: sequence_span,
            }),
            span: sequence_span,
        };
        statements.push(Statement::Expr(self.lower_expr_expected_results(
            ctx,
            &collect,
            Some(&[]),
        )));
        read
    }

    /// The index of the first keyword parameter of the generic `name` of
    /// `arity` parameters: `arity` when it takes no keywords.
    fn keyword_start(&self, name: &str, arity: usize) -> usize {
        if let Some(function) = CollectionFunction::from_name(name)
            && self
                .collection_generics
                .iter()
                .any(|(existing, _)| *existing == function)
            && !self.shadows_collection_generic(name)
        {
            return function.arity();
        }
        self.keyword_generics
            .iter()
            .find(|(owner, _)| owner == name)
            .map_or(arity, |(_, signature)| signature.required)
    }

    /// The keyword names of `generic`, a source or kernel collection
    /// generic, in declaration order.
    fn generic_keyword_names(&self, generic: &GenericSymbol) -> Vec<String> {
        if let Some((function, _)) = self
            .collection_generics
            .iter()
            .find(|(_, id)| *id == generic.id)
        {
            return function
                .keywords()
                .iter()
                .map(|name| (*name).into())
                .collect();
        }
        self.keyword_generics
            .iter()
            .find(|(owner, _)| *owner == generic.name)
            .map(|(_, signature)| keyword_names(signature))
            .unwrap_or_default()
    }

    /// The keyword parameters of the function or generic a call of `name`
    /// calls, when it takes any. A built-in collection generic's are
    /// [`keywords::kernel_signature`].
    fn keyword_signature(&self, name: &str) -> Option<&KeywordSignature> {
        let table = if self.resolve_function(name).is_some() {
            &self.keyword_functions
        } else if self.resolve_generic(name).is_some() {
            &self.keyword_generics
        } else {
            return None;
        };
        table
            .iter()
            .find(|(owner, _)| owner == name)
            .map(|(_, signature)| signature)
    }

    /// `$unsupplied`: the one kernel object, which a keyword declared
    /// without a default takes when a call does not pass it.
    fn unsupplied(&self, span: Span) -> Expr {
        Expr::IntrinsicCall {
            intrinsic: Intrinsic::Unsupplied,
            args: Vec::new(),
            result_kind: ValueKind::Any,
            span,
        }
    }

    /// The marker a call through a callable value passes for a keyword it
    /// does not pass ([`Intrinsic::KeywordAbsent`]).
    fn keyword_absent(&self, span: Span) -> Expr {
        Expr::IntrinsicCall {
            intrinsic: Intrinsic::KeywordAbsent,
            args: Vec::new(),
            result_kind: ValueKind::Any,
            span,
        }
    }

    /// Lowers a call of the function or generic `callee`, which takes the
    /// keyword parameters `signature`: a direct call or a send passing
    /// every parameter, keywords in declaration order.
    #[allow(clippy::too_many_arguments)]
    fn lower_keyword_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        signature: &KeywordSignature,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let scope = ctx.scope_start();
        // A function's typed callable parameters' signatures are expected
        // of its arguments; a generic's parameters have none.
        let signatures = self
            .resolve_function(callee)
            .map(|symbol| symbol.param_callable_sigs.clone())
            .unwrap_or_default();
        let positional = self.lower_args_against(ctx, args, &signatures);
        let first_diagnostic = ctx.diagnostics.len();
        let call = if let Some(symbol) = self.resolve_function(callee).cloned() {
            let kinds = symbol
                .param_kinds
                .iter()
                .copied()
                .map(Some)
                .collect::<Vec<_>>();
            self.lower_keyword_args(
                ctx,
                id,
                callee,
                signature,
                &kinds,
                &signatures,
                positional,
                keywords,
                span,
            )
            .map(|(lets, args)| {
                (
                    lets,
                    self.lower_function_call(ctx, &symbol, args, callee, span),
                )
            })
        } else if let Some(symbol) = self.resolve_generic(callee).cloned() {
            self.lower_keyword_args(
                ctx,
                id,
                callee,
                signature,
                &symbol.param_kinds,
                &[],
                positional,
                keywords,
                span,
            )
            .map(|(lets, args)| {
                (
                    lets,
                    self.lower_generic_send(ctx, &symbol, args, callee, span),
                )
            })
        } else {
            None
        };
        ctx.hide_locals_from(scope);
        name_keyword_mismatches(&mut ctx.diagnostics[first_diagnostic..], signature);
        match call {
            None => Expr::Error { span },
            Some((lets, call)) => with_lets(lets, call, span),
        }
    }

    /// Lowers the arguments of a call of `callee`, which takes `signature`'s
    /// keyword parameters after its required ones, all of kinds
    /// `param_kinds` and, where they are typed callables, of signatures
    /// `param_sigs`, to one argument per parameter in parameter order.
    /// `positional` are the call's positional arguments, already lowered.
    ///
    /// The order the call evaluates its arguments in is the one [`keywords`]
    /// documents: positional arguments and the keyword arguments passed, in
    /// source order, then each missing keyword's default, in declaration
    /// order. When that is not parameter order, the values go through
    /// hidden locals, whose `let`s come back for the call to sit in.
    ///
    /// Returns `None`, having reported it, for a call with the wrong number
    /// of positional arguments or a keyword the callee does not take or
    /// passed twice.
    #[allow(clippy::too_many_arguments)]
    fn lower_keyword_args(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        signature: &KeywordSignature,
        param_kinds: &[Option<ValueKind>],
        param_sigs: &[Option<CallableSignature>],
        mut positional: Vec<Expr>,
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Option<(Vec<Let>, Vec<Expr>)> {
        let mut valid = true;
        if positional.len() != signature.required {
            ctx.diagnostics.push(Self::arity_mismatch(
                callee,
                signature.required,
                positional.len(),
                span,
            ));
            valid = false;
        }
        // The keyword argument passed for each keyword parameter, by index.
        let mut supplied = vec![None; signature.keywords.len()];
        for (index, keyword) in keywords.iter().enumerate() {
            match signature.position(&keyword.name) {
                None => {
                    ctx.diagnostics
                        .push(Self::unexpected_keyword(callee, keyword));
                    valid = false;
                }
                Some(position) if supplied[position].is_some() => {
                    ctx.diagnostics.push(
                        Diagnostic::error(keyword.span, DiagnosticCode::DuplicateKeywordArgument)
                            .with_arg(DiagnosticArg::text("keyword", keyword.name.clone())),
                    );
                    valid = false;
                }
                Some(position) => supplied[position] = Some(index),
            }
        }
        let mut passed = keywords
            .iter()
            .map(|keyword| {
                let expected = signature
                    .position(&keyword.name)
                    .and_then(|position| param_sigs.get(signature.required + position))
                    .and_then(Option::as_ref);
                Some(
                    self.lower_method_against(ctx, &keyword.value, expected)
                        .unwrap_or_else(|| self.lower_expr(ctx, &keyword.value)),
                )
            })
            .collect::<Vec<_>>();
        if !valid {
            return None;
        }
        adapt_literal_args(&mut positional, param_kinds.iter().map(Option::as_ref));
        // Each argument is checked where it meets its parameter's declared
        // type, before any keyword's default function sees it. A callable
        // parameter's signature is the call's to check.
        let narrow = |this: &Self, value: &mut Expr, kind: Option<&ValueKind>| {
            if let Some(kind) = kind.filter(|kind| **kind != ValueKind::Callable) {
                let taken = core::mem::replace(value, Expr::Error { span: value.span() });
                *value = this.narrow(taken, *kind, None);
            }
        };
        for (value, kind) in positional.iter_mut().zip(param_kinds) {
            narrow(self, value, kind.as_ref());
        }
        for (position, index) in supplied.iter().enumerate() {
            if let Some(index) = index
                && let Some(value) = passed[*index].as_mut()
            {
                let kind = param_kinds
                    .get(signature.required + position)
                    .and_then(Option::as_ref);
                adapt_literal_args(core::slice::from_mut(value), [kind]);
                narrow(self, value, kind);
            }
        }
        let runs_defaults = signature
            .keywords
            .iter()
            .zip(&supplied)
            .any(|(slot, supplied)| {
                supplied.is_none() && matches!(slot.default, KeywordDefault::Function(_))
            });
        let in_order = supplied
            .iter()
            .flatten()
            .zip(supplied.iter().flatten().skip(1))
            .all(|(earlier, later)| earlier < later);
        if !runs_defaults && in_order {
            let mut args = positional;
            for (slot, index) in signature.keywords.iter().zip(&supplied) {
                args.push(match index {
                    Some(index) => passed[*index].take()?,
                    None => self.keyword_default_value(slot, span),
                });
            }
            return Some((Vec::new(), args));
        }
        let mut lets = Vec::new();
        let required = positional
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let name = format!("{callee} #arg {index}@{}", span.start);
                self.hidden_read(ctx, &mut lets, id, &name, value, span)
            })
            .collect::<Vec<_>>();
        let mut values = vec![None; signature.keywords.len()];
        for (index, value) in passed.into_iter().enumerate() {
            let Some(position) = supplied.iter().position(|passed| *passed == Some(index)) else {
                continue;
            };
            let name = format!("{callee} #key {}@{}", keywords[index].name, span.start);
            values[position] = Some(self.hidden_read(ctx, &mut lets, id, &name, value?, span));
        }
        for (position, slot) in signature.keywords.iter().enumerate() {
            if values[position].is_some() {
                continue;
            }
            let value = match &slot.default {
                KeywordDefault::Unsupplied | KeywordDefault::Missing => {
                    self.keyword_default_value(slot, span)
                }
                KeywordDefault::Function(default) => {
                    let function = self.resolve_function(default)?.clone();
                    let mut args = required.clone();
                    args.extend(values[..position].iter().flatten().cloned());
                    let args = self.limit_args(args, &function.param_kinds, &function.param_ranges);
                    let call = Expr::DirectCall {
                        function: function.id,
                        args,
                        span,
                    };
                    let name = format!("{callee} #key {}@{}", slot.name, span.start);
                    self.hidden_read(ctx, &mut lets, id, &name, call, span)
                }
            };
            values[position] = Some(value);
        }
        let mut args = required;
        args.extend(values.into_iter().flatten());
        Some((lets, args))
    }

    /// The value a call passes for the keyword `slot` it does not pass, when
    /// `slot` has no default function.
    fn keyword_default_value(&self, slot: &keywords::KeywordSlot, span: Span) -> Expr {
        match slot.default {
            KeywordDefault::Missing => Expr::Error { span },
            _ => self.unsupplied(span),
        }
    }

    /// Binds `value` to the hidden local `name`, adding its `let` to `lets`,
    /// and returns a read of it.
    fn hidden_read(
        &mut self,
        ctx: &mut FunctionContext,
        lets: &mut Vec<Let>,
        id: AstNodeId,
        name: &str,
        value: Expr,
        span: Span,
    ) -> Expr {
        // A read has its value's span, so a check of the argument points
        // at what the call passed.
        let read_span = match &value {
            Expr::DirectCall { .. } => span,
            value => value.span(),
        };
        let binding = self.bind_hidden_local(ctx, id, name, value, span);
        let local = binding.local;
        lets.push(binding);
        Expr::Local {
            local,
            result_kind: ctx.local_kind(local),
            callable_signature: ctx.local_callable_signature(local),
            span: read_span,
        }
    }

    /// Lowers a call of the function `symbol` with `lowered_args`, keyword
    /// arguments already in their parameters' places.
    fn lower_function_call(
        &self,
        ctx: &mut FunctionContext,
        symbol: &FunctionSymbol,
        mut lowered_args: Vec<Expr>,
        callee: &str,
        span: Span,
    ) -> Expr {
        // A function taking `#rest` gets the arguments after its required
        // ones in a vector the call makes.
        if symbol.rest {
            if lowered_args.len() < symbol.required() {
                ctx.diagnostics.push(Self::too_few_arguments(
                    callee,
                    symbol.required(),
                    lowered_args.len(),
                    span,
                ));
                return Expr::Error { span };
            }
            lowered_args = self.pack_rest_args(lowered_args, symbol.required(), span);
        }
        if symbol.arity() == lowered_args.len() {
            adapt_literal_args(&mut lowered_args, symbol.param_kinds.iter().map(Some));
            self.narrow_args(
                &mut lowered_args,
                &symbol.param_kinds,
                &symbol.param_callable_sigs,
            );
            for (index, (arg, expected)) in lowered_args.iter().zip(&symbol.param_kinds).enumerate()
            {
                let actual = self.expr_kind(arg);
                if !self.kinds_match(*expected, actual) {
                    ctx.diagnostics.push(
                        Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                            .with_arg(DiagnosticArg::text("function", callee.to_string()))
                            .with_arg(DiagnosticArg::unsigned(
                                "argument_index",
                                u64::try_from(index + 1).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::text("expected", self.kind_name(*expected)))
                            .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                    );
                }
                if let Some(expected) = symbol
                    .param_callable_sigs
                    .get(index)
                    .and_then(|signature| signature.as_ref())
                    && let actual_signature = self.expr_callable_signature(arg)
                    && !self.callable_signatures_match(Some(expected), actual_signature.as_ref())
                {
                    let diagnostic = self
                        .callable_variance_diagnostic(
                            arg.span(),
                            expected,
                            actual_signature.as_ref(),
                        )
                        .unwrap_or_else(|| {
                            Diagnostic::error(
                                arg.span(),
                                DiagnosticCode::FunctionArgumentTypeMismatch,
                            )
                            .with_arg(DiagnosticArg::text("function", callee.to_string()))
                            .with_arg(DiagnosticArg::unsigned(
                                "argument_index",
                                u64::try_from(index + 1).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::text(
                                "expected",
                                self.callable_signature_name(expected),
                            ))
                            .with_arg(DiagnosticArg::text(
                                "actual",
                                actual_signature.as_ref().map_or_else(
                                    || self.kind_name(actual),
                                    |signature| self.callable_signature_name(signature),
                                ),
                            ))
                        });
                    ctx.diagnostics.push(diagnostic);
                }
            }
            // The caller checks each argument for a limited
            // parameter, where typing usually knows its range.
            let args = self.limit_args(lowered_args, &symbol.param_kinds, &symbol.param_ranges);
            return Expr::DirectCall {
                function: symbol.id,
                args,
                span,
            };
        }
        ctx.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                .with_arg(DiagnosticArg::text("callee", callee.to_string()))
                .with_arg(DiagnosticArg::unsigned(
                    "expected_count",
                    u64::try_from(symbol.arity()).unwrap_or(u64::MAX),
                ))
                .with_arg(DiagnosticArg::unsigned(
                    "actual_count",
                    u64::try_from(lowered_args.len()).unwrap_or(u64::MAX),
                )),
        );
        Expr::Error { span }
    }

    /// Lowers a send of the generic `symbol` names to `args`, checking them
    /// against its parameters.
    fn lower_generic_send(
        &self,
        ctx: &mut FunctionContext,
        symbol: &GenericSymbol,
        mut args: Vec<Expr>,
        callee: &str,
        span: Span,
    ) -> Expr {
        if symbol.arity() != args.len() {
            ctx.diagnostics.push(Self::arity_mismatch(
                callee,
                symbol.arity(),
                args.len(),
                span,
            ));
            return Expr::Error { span };
        }
        adapt_literal_args(&mut args, symbol.param_kinds.iter().map(Option::as_ref));
        self.narrow_generic_args(&mut args, &symbol.param_kinds);
        for (index, (arg, declared)) in args.iter().zip(&symbol.param_kinds).enumerate() {
            ctx.diagnostics
                .extend(self.generic_argument_diagnostic(arg, symbol.id, index, *declared));
        }
        // Every send checks its arguments for limited parameters, so methods
        // may assume the ranges.
        let kinds = symbol
            .param_kinds
            .iter()
            .map(|kind| kind.unwrap_or(ValueKind::Any))
            .collect::<Vec<_>>();
        let args = self.limit_args(args, &kinds, &symbol.param_ranges);
        Expr::GenericCall {
            site: SiteId::PENDING,
            generic: symbol.id,
            args,
            span,
        }
    }

    fn lower_intrinsic_call(
        &self,
        ctx: &mut FunctionContext,
        callee: &str,
        args: &[Expr],
        span: Span,
    ) -> Option<Expr> {
        let intrinsic = intrinsic_for_name(callee)?;
        let expected = intrinsic_arity(intrinsic);
        if args.len() != expected {
            ctx.diagnostics
                .push(Self::arity_mismatch(callee, expected, args.len(), span));
            return Some(Expr::Error { span });
        }
        let mut args = args.to_vec();
        // A list operation checks an `<object>` argument where it takes a
        // `<list>` or a `<pair>`, as a declaration would.
        if matches!(
            intrinsic,
            Intrinsic::ListHead
                | Intrinsic::ListTail
                | Intrinsic::ListHeadSetter
                | Intrinsic::ListTailSetter
        ) {
            let (index, class) = match intrinsic {
                Intrinsic::ListHead | Intrinsic::ListTail => (0, BuiltinValueClass::List),
                _ => (1, BuiltinValueClass::Pair),
            };
            let class = self.builtin_value_class(class)?;
            let arg = core::mem::replace(&mut args[index], Expr::Error { span });
            args[index] = self.narrow(arg, ValueKind::Object(class), None);
        }
        // `concatenate` takes characters too, each as a one-character
        // string.
        if intrinsic == Intrinsic::Concatenate {
            let string = self.builtin_value_class(BuiltinValueClass::String)?;
            for arg in &mut args {
                if self.expr_kind(arg) == ValueKind::Char {
                    let span = arg.span();
                    let character = core::mem::replace(arg, Expr::Error { span });
                    *arg = Expr::IntrinsicCall {
                        intrinsic: Intrinsic::Text(TextFunction::CharacterToString),
                        args: vec![character],
                        result_kind: ValueKind::String(string),
                        span,
                    };
                }
            }
        }
        Some(self.checked_intrinsic_call(ctx, intrinsic, callee, args, span))
    }

    /// Whether a module-level definition of `name` (a function, a host
    /// function, or a generic of its own) replaces the built-in collection
    /// generic of that name.
    fn shadows_collection_generic(&self, name: &str) -> bool {
        self.resolve_function(name).is_some()
            || self.resolve_host_function(name).is_some()
            || self.resolve_generic(name).is_some_and(|symbol| {
                !self.is_kernel_generic(symbol.id) && !self.is_slot_getter_generic(symbol.id)
            })
    }

    /// Whether `generic` is one the compiler made for slots' getters,
    /// rather than the program's own: it serves its slots' classes and
    /// takes no name from a built-in.
    fn is_slot_getter_generic(&self, generic: GenericFunctionId) -> bool {
        self.generics
            .get(generic.0 as usize)
            .is_some_and(|generic| {
                matches!(generic.origin, GenericFunctionOrigin::SlotGetter { .. })
            })
    }

    /// Whether `generic` is one of the kernel's open generics a program's
    /// methods join: a built-in collection generic, or the built-in generic
    /// a built-in function with several overloads is.
    fn is_kernel_generic(&self, generic: GenericFunctionId) -> bool {
        self.collection_generics
            .iter()
            .any(|(_, id)| *id == generic)
            || self.built_in_generics.iter().any(|(_, id)| *id == generic)
    }

    /// The value the built-in `name` is, creating it on first use: its
    /// hidden kernel function when it has one overload, its built-in
    /// generic when it has several (see the `built_in` module). `None`
    /// when `name` is no built-in with a value.
    fn built_in_value(&mut self, name: &str) -> Option<KnownCallable> {
        if self.program_defines(name) {
            return None;
        }
        if let Some(symbol) = self.library_function(name) {
            return Some(KnownCallable::DirectFunction(symbol.id));
        }
        let overloads = built_in::overloads(name)?;
        Some(match overloads.as_slice() {
            [params] => KnownCallable::DirectFunction(self.built_in_function(name, params)),
            _ => KnownCallable::GenericFunction(self.built_in_generic(name, &overloads)),
        })
    }

    /// The built-in generic named `name`, creating it on first use, when
    /// `name` is a built-in with several overloads that no definition of
    /// the program takes.
    fn built_in_generic_named(&mut self, name: &str) -> Option<GenericFunctionId> {
        if self.program_defines(name) {
            return None;
        }
        let overloads = built_in::overloads(name).filter(|overloads| overloads.len() > 1)?;
        Some(self.built_in_generic(name, &overloads))
    }

    /// Whether a call of the built-in `name` with `args` sends to its
    /// built-in generic: it has several overloads, none takes the
    /// arguments' types as typing sees them, and one of those types may
    /// have a method all the same, an `<object>` or a class of the
    /// program's own, whose methods may join the generic. A call of a
    /// built-in with one overload, or of kernel types none takes, lowers
    /// the built-in's call, inline or reported.
    fn sends_to_built_in_generic(&mut self, name: &str, args: &[Expr]) -> bool {
        let Some(overloads) = built_in::overloads(name).filter(|overloads| overloads.len() > 1)
        else {
            return false;
        };
        let actual = args
            .iter()
            .map(|arg| self.expr_kind(arg))
            .collect::<Vec<_>>();
        let may_join = actual.iter().any(|kind| match kind {
            ValueKind::Any => true,
            ValueKind::Object(class) => self.classes.get(class.0 as usize).is_some_and(|class| {
                matches!(
                    class.origin,
                    ClassOrigin::Native | ClassOrigin::HostBacked { .. }
                )
            }),
            _ => false,
        });
        may_join
            && !overloads.iter().any(|params| {
                params.len() == actual.len()
                    && params.iter().zip(&actual).all(|(param, actual)| {
                        let expected = self.resolve_type_name(param, false, Span::default());
                        *actual == ValueKind::Error || self.kinds_match(expected, *actual)
                    })
            })
    }

    /// The hidden kernel function the single-overload built-in `name`,
    /// taking `params`, is as a value, creating it on first use. Its body
    /// is the built-in's call of its parameters, lowered as any call of the
    /// built-in is, and its contract is closed: its results are the
    /// call's.
    fn built_in_function(&mut self, name: &str, params: &[&str]) -> FunctionId {
        if let Some((_, function)) = self
            .built_in_functions
            .iter()
            .find(|(existing, _)| existing == name)
        {
            return function.id;
        }
        let id = FunctionId(u32::try_from(self.functions.len()).unwrap_or(u32::MAX));
        let definition = DefinitionId::new(
            self.source_file,
            DefinitionKind::Function,
            name,
            BUILT_IN_DISCRIMINATOR,
        );
        let (ctx, locals, param_kinds, body) = self.built_in_body(name, definition, params);
        let result_kinds = self.expr_kinds(&body.result);
        let result_callable_sigs = self.expr_result_callable_signatures(&body.result);
        let arity = param_kinds.len();
        self.functions.push(FunctionSymbol {
            name: name.into(),
            id,
            param_kinds: param_kinds.clone(),
            rest: false,
            param_callable_sigs: vec![None; arity],
            param_ranges: vec![None; arity],
            return_kinds: result_kinds.clone(),
            return_callable_sigs: result_callable_sigs.clone(),
            return_ranges: vec![None; result_kinds.len()],
            return_annotation: Some(result_kinds.clone()),
            span: Span::default(),
        });
        self.built_in_functions.push((
            name.into(),
            Function {
                id,
                definition,
                name: name.into(),
                params: locals,
                param_kinds,
                param_callable_sigs: vec![None; arity],
                param_ranges: vec![None; arity],
                locals: ctx.locals,
                body,
                result_ranges: vec![None; result_kinds.len()],
                result_kinds: result_kinds.clone(),
                result_callable_sigs,
                return_annotation: Some(result_kinds),
                entry_checks: Vec::new(),
                keywords: Vec::new(),
                rest: false,
                strict: false,
                kernel: true,
                span: Span::default(),
            },
        ));
        id
    }

    /// The built-in generic the built-in `name` with several overloads,
    /// `overloads`, is, creating it on first use. Its parameters are
    /// unannotated, as a numeric generic's are, and each result is the
    /// overloads' own where they agree and an `<object>` where they do not.
    /// It persists across lowering passes; its kernel methods are
    /// regenerated with every pass ([`Self::collect_built_in_methods`]).
    fn built_in_generic(
        &mut self,
        name: &str,
        overloads: &[Vec<&'static str>],
    ) -> GenericFunctionId {
        if let Some((_, id)) = self
            .built_in_generics
            .iter()
            .find(|(existing, _)| existing == name)
        {
            return *id;
        }
        let id = GenericFunctionId(u32::try_from(self.generics.len()).unwrap_or(u32::MAX));
        let definition = DefinitionId::new(
            self.source_file,
            DefinitionKind::Generic,
            name,
            BUILT_IN_DISCRIMINATOR,
        );
        let mut result_kinds: Option<Vec<ValueKind>> = None;
        for params in overloads {
            let (_, _, _, body) = self.built_in_body(name, definition, params);
            let kinds = self.expr_kinds(&body.result);
            result_kinds = Some(match result_kinds {
                None => kinds,
                Some(joined) => joined
                    .into_iter()
                    .zip(kinds)
                    .map(|(joined, kind)| if joined == kind { kind } else { ValueKind::Any })
                    .collect(),
            });
        }
        let result_kinds = result_kinds.unwrap_or_default();
        let arity = overloads.first().map_or(0, Vec::len);
        let results = result_kinds.len();
        self.generic_symbols.push(GenericSymbol {
            name: name.into(),
            id,
            param_kinds: vec![None; arity],
            param_ranges: vec![None; arity],
            return_kinds: result_kinds.clone(),
            return_callable_sigs: vec![None; results],
            return_ranges: vec![None; results],
            return_annotation: Some(result_kinds.clone()),
            dispatch_param: 0,
            span: Span::default(),
        });
        self.generics.push(GenericFunction {
            id,
            definition,
            name: name.into(),
            params: BUILT_IN_PARAMS
                .iter()
                .take(arity)
                .map(|name| (*name).into())
                .collect(),
            param_kinds: vec![None; arity],
            param_ranges: vec![None; arity],
            arity,
            dispatch_param: 0,
            dispatch_params: (0..arity).collect(),
            result_kinds: result_kinds.clone(),
            result_callable_sigs: vec![None; results],
            result_ranges: vec![None; results],
            return_annotation: Some(result_kinds),
            span: Span::default(),
            origin: GenericFunctionOrigin::BuiltIn { generic: id },
            sealing: Sealing::Open,
            strict: false,
            keywords: Vec::new(),
        });
        self.built_in_generics.push((name.into(), id));
        id
    }

    /// Generates the kernel methods of every built-in generic created so
    /// far, one per overload of its built-in: the built-in's call of the
    /// method's parameters, each of its overload's type.
    fn collect_built_in_methods(&mut self) {
        for (name, generic) in self.built_in_generics.clone() {
            for params in built_in::overloads(&name).unwrap_or_default() {
                let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
                let definition = DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Method,
                    &name,
                    &format!("{BUILT_IN_DISCRIMINATOR}:{}", params.join(",")),
                );
                let (ctx, locals, kinds, body) = self.built_in_body(&name, definition, &params);
                let specializers = kinds
                    .iter()
                    .map(|kind| self.method_specializer_for_kind(*kind))
                    .collect::<Vec<_>>();
                let result_kinds = self.expr_kinds(&body.result);
                self.methods.push(Method {
                    id,
                    definition,
                    generic,
                    specializer: specializers
                        .first()
                        .copied()
                        .flatten()
                        .unwrap_or(OBJECT_CLASS_ID),
                    specializers,
                    param: locals.first().copied().unwrap_or(LocalId(u32::MAX)),
                    params: locals,
                    locals: ctx.locals,
                    body,
                    result_ranges: vec![None; result_kinds.len()],
                    result_kinds: result_kinds.clone(),
                    return_annotation: Some(result_kinds),
                    span: Span::default(),
                    origin: MethodOrigin::BuiltIn,
                    strict: false,
                });
            }
        }
    }

    /// A built-in's call of parameters of the types `params`, of the
    /// function or method `definition`: the context holding them, their
    /// locals and kinds, and the body, the call lowered as a call of the
    /// built-in always is.
    fn built_in_body(
        &mut self,
        name: &str,
        definition: DefinitionId,
        params: &[&str],
    ) -> (FunctionContext, Vec<LocalId>, Vec<ValueKind>, Block) {
        let span = Span::default();
        let mut ctx = FunctionContext::new();
        let mut locals = Vec::new();
        let mut kinds = Vec::new();
        let mut args = Vec::new();
        let id = AstNodeId::generated(self.source_file, BUILT_IN_TAG, name, 0, 0);
        debug_assert!(
            params.len() <= BUILT_IN_PARAMS.len(),
            "a built-in takes at most as many parameters as have names"
        );
        for (param, type_name) in BUILT_IN_PARAMS.iter().zip(params) {
            let kind = self.resolve_type_name(type_name, false, span);
            locals.push(ctx.add_local(
                self.local_definition(definition, param),
                (*param).into(),
                kind,
                None,
                span,
            ));
            kinds.push(kind);
            args.push(ast::Expr::Name {
                id,
                name: (*param).into(),
                span,
            });
        }
        let result = self.lower_built_in_call(&mut ctx, id, name, &args, &[], None, span);
        self.diagnostics.append(&mut ctx.diagnostics);
        let body = Block {
            statements: Vec::new(),
            result,
            span,
        };
        (ctx, locals, kinds, body)
    }

    /// The kernel library, parsed on first use.
    fn library(&mut self) -> &library::Library {
        self.library.get_or_insert_with(library::Library::parse)
    }

    /// The symbol of the kernel library's function `name`, creating it on
    /// first use: one the library exports, or in the library's own code
    /// any of its functions. Its body is lowered with the pass's others
    /// ([`Self::lower_library_functions`]).
    fn library_function(&mut self, name: &str) -> Option<FunctionSymbol> {
        if !self.kernel_scope && !library::exports(name) {
            return None;
        }
        self.kernel_library_function(name)
    }

    /// The symbol of the kernel library's function `name`, exported or
    /// not, for code the compiler writes; see [`Self::library_function`].
    fn kernel_library_function(&mut self, name: &str) -> Option<FunctionSymbol> {
        if let Some((_, id)) = self
            .library_functions
            .iter()
            .find(|(existing, _)| existing == name)
        {
            return self.functions.get(id.0 as usize).cloned();
        }
        let function = self.library().function(name)?.clone();
        let id = FunctionId(u32::try_from(self.functions.len()).unwrap_or(u32::MAX));
        let scope = core::mem::replace(&mut self.kernel_scope, true);
        let symbol = self.function_symbol(id, &function);
        self.kernel_scope = scope;
        self.functions.push(symbol.clone());
        self.library_functions.push((name.into(), id));
        Some(symbol)
    }

    /// Lowers the body of every kernel library function created so far,
    /// and of any those bodies name in turn, in the kernel's scope.
    fn lower_library_functions(&mut self) {
        let scope = core::mem::replace(&mut self.kernel_scope, true);
        let mut index = 0;
        while let Some((name, id)) = self.library_functions.get(index).cloned() {
            let Some(function) = self.library().function(&name).cloned() else {
                index += 1;
                continue;
            };
            let mut lowered = self.lower_function(id, &function);
            lowered.kernel = true;
            lowered.keywords = Vec::new();
            if let Some(body) = self.library_bodies.get_mut(index) {
                *body = lowered;
            } else {
                self.library_bodies.push(lowered);
            }
            index += 1;
        }
        self.kernel_scope = scope;
    }

    /// Generates the kernel library's methods of the collection generic
    /// `function`, `generic`, each lowered in the kernel's scope as a kernel
    /// method of the generic.
    fn collect_library_methods(
        &mut self,
        function: CollectionFunction,
        generic: GenericFunctionId,
    ) {
        let methods = self
            .library()
            .methods(function.name())
            .cloned()
            .collect::<Vec<_>>();
        let scope = core::mem::replace(&mut self.kernel_scope, true);
        for method in methods {
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let mut ctx = FunctionContext::new();
            ctx.shared_names = ast::shared_local_names(&method.body);
            let mut params = Vec::new();
            let mut specializers = Vec::new();
            for param in &method.params {
                let class = match &param.specializer {
                    Some(ast::TypeExpr::Named { name, .. }) => self
                        .resolve_class(name)
                        .map_or(OBJECT_CLASS_ID, |class| class.id),
                    _ => OBJECT_CLASS_ID,
                };
                let kind = if class == OBJECT_CLASS_ID {
                    ValueKind::Any
                } else {
                    self.kind_for_builtin_class(class)
                        .unwrap_or(ValueKind::Object(class))
                };
                let local = ctx.add_local(
                    self.local_definition(method.definition, &param.name),
                    param.name.clone(),
                    kind,
                    None,
                    Span::default(),
                );
                ctx.allow_assignment(local);
                params.push(local);
                specializers.push(Some(class));
            }
            // The generic's keywords follow its required parameters.
            for keyword in function.keywords() {
                let local = ctx.add_local(
                    self.local_definition(method.definition, keyword),
                    (*keyword).into(),
                    ValueKind::Any,
                    None,
                    Span::default(),
                );
                params.push(local);
                specializers.push(Some(OBJECT_CLASS_ID));
            }
            let (result_kinds, result_sigs) = self.collection_results(function);
            let mut body =
                self.lower_result_block(&mut ctx, &method.body, Some(&result_kinds), &result_sigs);
            let result = core::mem::replace(&mut body.result, Expr::Error { span: body.span });
            body.result = self.narrow_results(
                &mut ctx,
                result,
                &result_kinds,
                &result_sigs,
                "function-result",
            );
            self.diagnostics.append(&mut ctx.diagnostics);
            self.methods.push(Method {
                id,
                definition: method.definition,
                generic,
                specializer: specializers
                    .get(function.collection_index())
                    .copied()
                    .flatten()
                    .unwrap_or(OBJECT_CLASS_ID),
                specializers,
                param: params
                    .get(function.collection_index())
                    .copied()
                    .unwrap_or(LocalId(u32::MAX)),
                params,
                locals: ctx.locals,
                body,
                result_ranges: vec![None; result_kinds.len()],
                result_kinds: result_kinds.clone(),
                return_annotation: Some(result_kinds),
                span: Span::default(),
                origin: MethodOrigin::Collection { function },
                strict: false,
            });
        }
        self.kernel_scope = scope;
    }

    /// Returns the built-in collection generic named `name`, creating it on
    /// first use, unless the module defines `name` itself.
    fn collection_generic_named(&mut self, name: &str) -> Option<GenericFunctionId> {
        let function = CollectionFunction::from_name(name)?;
        if self.shadows_collection_generic(name) {
            return None;
        }
        Some(self.collection_generic(function))
    }

    /// Returns the built-in generic for `function`, creating it on first
    /// use. It persists across lowering passes; its kernel methods are
    /// regenerated with every pass ([`Self::collect_collection_methods`]).
    fn collection_generic(&mut self, function: CollectionFunction) -> GenericFunctionId {
        if let Some((_, id)) = self
            .collection_generics
            .iter()
            .find(|(existing, _)| *existing == function)
        {
            return *id;
        }
        let id = GenericFunctionId(u32::try_from(self.generics.len()).unwrap_or(u32::MAX));
        let name = function.name();
        let required = function.arity();
        let arity = required + function.keywords().len();
        // Keyword parameters are `<object>`s, after the required ones.
        let param_kinds = (0..arity)
            .map(|index| (index >= required).then_some(ValueKind::Any))
            .collect::<Vec<_>>();
        let (result_kinds, result_sigs) = self.collection_results(function);
        self.generic_symbols.push(GenericSymbol {
            name: name.into(),
            id,
            param_kinds: param_kinds.clone(),
            param_ranges: vec![None; arity],
            return_kinds: result_kinds.clone(),
            return_callable_sigs: result_sigs.clone(),
            return_ranges: vec![None; result_kinds.len()],
            return_annotation: Some(result_kinds.clone()),
            dispatch_param: function.collection_index(),
            span: Span::default(),
        });
        self.generics.push(GenericFunction {
            id,
            definition: DefinitionId::new(
                self.source_file,
                DefinitionKind::Generic,
                name,
                "collection",
            ),
            name: name.into(),
            params: function
                .params()
                .iter()
                .chain(function.keywords())
                .map(|name| (*name).into())
                .collect(),
            param_kinds,
            param_ranges: vec![None; arity],
            arity,
            dispatch_param: function.collection_index(),
            dispatch_params: (0..required).collect(),
            result_ranges: vec![None; result_kinds.len()],
            result_kinds: result_kinds.clone(),
            result_callable_sigs: result_sigs,
            return_annotation: Some(result_kinds),
            span: Span::default(),
            origin: GenericFunctionOrigin::Collection { function },
            sealing: Sealing::Open,
            strict: false,
            keywords: function
                .keywords()
                .iter()
                .map(|name| KeywordParam {
                    name: (*name).into(),
                    default: None,
                })
                .collect(),
        });
        self.collection_generics.push((function, id));
        id
    }

    /// The results a collection generic declares, with their callable
    /// signatures: `forward-iteration-protocol`'s seven, whose callables
    /// take and return `<object>`s (`finished-state?` a `<boolean>`).
    fn collection_results(
        &self,
        function: CollectionFunction,
    ) -> (Vec<ValueKind>, Vec<Option<CallableSignature>>) {
        let kind = match function {
            CollectionFunction::Size => ValueKind::I64,
            CollectionFunction::Element
            | CollectionFunction::ElementSetter
            | CollectionFunction::Add => ValueKind::Any,
            CollectionFunction::ForwardIterationProtocol => {
                let callable = |params: usize, result: ValueKind| {
                    Some(CallableSignature::flat(
                        vec![ValueKind::Any; params],
                        vec![result],
                    ))
                };
                let sigs = vec![
                    None,
                    None,
                    callable(2, ValueKind::Any),
                    callable(3, ValueKind::Bool),
                    callable(2, ValueKind::Any),
                    callable(2, ValueKind::Any),
                    None,
                ];
                let kinds = sigs
                    .iter()
                    .map(|sig| {
                        if sig.is_some() {
                            ValueKind::Callable
                        } else {
                            ValueKind::Any
                        }
                    })
                    .collect();
                return (kinds, sigs);
            }
        };
        (vec![kind], vec![None])
    }

    /// Generates the kernel methods of `forward-iteration-protocol`, one per
    /// class in [`iteration::ITERATION_CLASSES`], by lowering the syntax
    /// trees [`iteration::KernelTree`] writes for them.
    fn collect_iteration_methods(&mut self, generic: GenericFunctionId) {
        let function = CollectionFunction::ForwardIterationProtocol;
        for class_kind in iteration::ITERATION_CLASSES {
            let Some(class) = self.builtin_value_class(*class_kind) else {
                continue;
            };
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Method,
                function.name(),
                &format!("collection:{}", class_kind.name()),
            );
            let kind = self
                .kind_for_builtin_class(class)
                .unwrap_or(ValueKind::Object(class));
            let mut ctx = FunctionContext::new();
            let param = ctx.add_local(
                self.local_definition(definition, "c"),
                "c".into(),
                kind,
                None,
                Span::default(),
            );
            let tree = iteration::KernelTree::new(self.source_file, class_kind.name())
                .protocol_body(*class_kind, definition);
            let body = self.lower_block(&mut ctx, &tree, None);
            self.diagnostics.append(&mut ctx.diagnostics);
            let (result_kinds, _) = self.collection_results(function);
            self.methods.push(Method {
                id,
                definition,
                generic,
                specializer: class,
                specializers: vec![Some(class)],
                param,
                params: vec![param],
                locals: ctx.locals,
                body,
                result_ranges: vec![None; result_kinds.len()],
                result_kinds: result_kinds.clone(),
                return_annotation: Some(result_kinds),
                span: Span::default(),
                origin: MethodOrigin::Collection { function },
                strict: false,
            });
        }
    }

    /// Generates the kernel methods of every collection generic created so
    /// far, from [`BUILTIN_METHODS`]: each runs its row's operation on its
    /// parameters.
    fn collect_collection_methods(&mut self) {
        for (function, generic) in self.collection_generics.clone() {
            self.collect_library_methods(function, generic);
            if function == CollectionFunction::ForwardIterationProtocol {
                self.collect_iteration_methods(generic);
                continue;
            }
            for builtin in collection::builtin_methods(function) {
                let classes = function
                    .params()
                    .iter()
                    .chain(function.keywords())
                    .enumerate()
                    .map(|(index, _)| {
                        if index == function.collection_index() {
                            self.builtin_value_class(builtin.collection)
                        } else if index == function.collection_index() + 1 {
                            match builtin.key {
                                Some(collection::MethodKey::Class(key)) => {
                                    self.builtin_value_class(key)
                                }
                                Some(collection::MethodKey::Object) => Some(OBJECT_CLASS_ID),
                                None => None,
                            }
                        } else {
                            Some(OBJECT_CLASS_ID)
                        }
                    })
                    .collect::<Option<Vec<_>>>();
                let Some(classes) = classes else {
                    continue;
                };
                let collection = self.builtin_value_class(builtin.collection);
                let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
                let definition = DefinitionId::new(
                    self.source_file,
                    DefinitionKind::Method,
                    function.name(),
                    &format!("collection:{}", builtin.collection.name()),
                );
                let mut ctx = FunctionContext::new();
                let mut params = Vec::new();
                let mut args = Vec::new();
                for (name, class) in function
                    .params()
                    .iter()
                    .chain(function.keywords())
                    .zip(&classes)
                {
                    let kind = self
                        .kind_for_builtin_class(*class)
                        .unwrap_or(ValueKind::Any);
                    let local = ctx.add_local(
                        self.local_definition(definition, name),
                        (*name).into(),
                        kind,
                        None,
                        Span::default(),
                    );
                    params.push(local);
                    args.push(Expr::Local {
                        local,
                        result_kind: kind,
                        callable_signature: None,
                        span: Span::default(),
                    });
                }
                let result = self.kernel_method_body(&mut ctx, function, builtin, args);
                let result_kinds = vec![self.expr_kind(&result)];
                self.methods.push(Method {
                    id,
                    definition,
                    generic,
                    specializer: collection.unwrap_or(OBJECT_CLASS_ID),
                    specializers: classes.into_iter().map(Some).collect(),
                    param: params
                        .get(function.collection_index())
                        .copied()
                        .unwrap_or(LocalId(u32::MAX)),
                    params,
                    locals: ctx.locals,
                    body: Block {
                        statements: Vec::new(),
                        result,
                        span: Span::default(),
                    },
                    result_ranges: vec![None; result_kinds.len()],
                    result_kinds: result_kinds.clone(),
                    return_annotation: Some(result_kinds),
                    span: Span::default(),
                    origin: MethodOrigin::Collection { function },
                    strict: false,
                });
            }
        }
    }

    /// The body of `builtin`, a kernel method of `function`, over `args`,
    /// its parameters read: its operation on them. A generic's keywords
    /// follow its required parameters, and a kernel method that takes one,
    /// `element`'s `default`, runs its operation on the required ones when
    /// the call passed `$unsupplied` for it, and otherwise its defaulting
    /// operation, the default checked to be what that takes.
    fn kernel_method_body(
        &mut self,
        ctx: &mut FunctionContext,
        function: CollectionFunction,
        builtin: &BuiltinMethod,
        mut args: Vec<Expr>,
    ) -> Expr {
        let span = Span::default();
        let call = |this: &Self, intrinsic: Intrinsic, args: Vec<Expr>| Expr::IntrinsicCall {
            intrinsic,
            args,
            result_kind: this.intrinsic_result_kind(intrinsic),
            span,
        };
        let keyword_args = args.split_off(function.arity().min(args.len()));
        let (Some(default), Some(with_default)) = (keyword_args.first(), builtin.with_default)
        else {
            return call(self, builtin.operation, args);
        };
        // A string's `default:` may be any object, as Dylan's is: the
        // kernel method answers it at or past the end, and the character
        // at the position otherwise.
        if with_default == Intrinsic::Text(TextFunction::StringElementOr)
            && let [string, key] = args.as_slice()
        {
            let span = Span::default();
            let end = Expr::IntrinsicCall {
                intrinsic: Intrinsic::Text(TextFunction::EndPosition),
                args: vec![string.clone()],
                result_kind: ValueKind::Position,
                span,
            };
            let past_end = Expr::IntrinsicCall {
                intrinsic: Intrinsic::PositionCompare(CompareOp::Ge),
                args: vec![key.clone(), end],
                result_kind: ValueKind::Bool,
                span,
            };
            let character = || Expr::Cast {
                site: SiteId::PENDING,
                value: Box::new(call(self, builtin.operation, args.clone())),
                class: OBJECT_CLASS_ID,
                explicit: false,
                span,
            };
            let defaulted = Expr::If {
                condition: Box::new(past_end),
                refinement: None,
                then_branch: Box::new(default.clone()),
                else_branch: Box::new(character()),
                span,
            };
            return Expr::If {
                condition: Box::new(Expr::Identical {
                    lhs: Box::new(default.clone()),
                    rhs: Box::new(self.unsupplied(span)),
                    span,
                }),
                refinement: None,
                then_branch: Box::new(character()),
                else_branch: Box::new(defaulted),
                span,
            };
        }
        let index = args.len();
        let default_arg = if self.intrinsic_arg_matches(with_default, index, ValueKind::Any) {
            default.clone()
        } else {
            // A default of a kernel class, as a string's `<character>` is.
            let class = BuiltinValueClass::ALL
                .iter()
                .filter_map(|(kind, _)| self.builtin_value_class(*kind))
                .find(|class| {
                    self.kind_for_builtin_class(*class)
                        .is_some_and(|kind| self.intrinsic_arg_matches(with_default, index, kind))
                });
            match class {
                Some(class) => self.cast_to_class(ctx, default.clone(), class, span),
                None => return call(self, builtin.operation, args),
            }
        };
        let plain = call(self, builtin.operation, args.clone());
        let mut defaulted_args = args;
        defaulted_args.push(default_arg);
        let defaulted = call(self, with_default, defaulted_args);
        let unsupplied = self.unsupplied(span);
        Expr::If {
            condition: Box::new(Expr::Identical {
                lhs: Box::new(default.clone()),
                rhs: Box::new(unsupplied),
                span,
            }),
            refinement: None,
            then_branch: Box::new(plain),
            else_branch: Box::new(defaulted),
            span,
        }
    }

    /// Whether the class of a value of kind `kind` has a slot getter named
    /// `name`, itself or through a superclass.
    fn class_has_accessor(&self, kind: ValueKind, name: &str) -> bool {
        let ValueKind::Object(mut class) = kind else {
            return false;
        };
        loop {
            let Some(info) = self.classes.get(class.0 as usize) else {
                return false;
            };
            if info
                .direct_slots
                .iter()
                .any(|slot| slot.accessor.name == name)
            {
                return true;
            }
            let Some(superclass) = info.superclass else {
                return false;
            };
            class = superclass;
        }
    }

    /// Lowers a call of a built-in collection generic: a send passing every
    /// keyword, `$unsupplied` for each the call does not pass, or the kernel
    /// method's operation itself when typing's view of the receiver already
    /// fixes the method.
    fn lower_collection_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        function: CollectionFunction,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let name = function.name();
        let args = args
            .iter()
            .map(|arg| self.lower_expr(ctx, arg))
            .collect::<Vec<_>>();
        // A slot getter of the same name serves its own class.
        if let [receiver] = args.as_slice()
            && keywords.is_empty()
            && self.class_has_accessor(self.expr_kind(receiver), name)
        {
            let receiver = args.into_iter().next().expect("one argument was matched");
            return self.lower_accessor_call(ctx, receiver, name, span);
        }
        let generic = self.collection_generic(function);
        let Some(symbol) = self
            .generic_symbols
            .iter()
            .find(|symbol| symbol.id == generic)
            .cloned()
        else {
            return Expr::Error { span };
        };
        let signature = keywords::kernel_signature(function);
        let scope = ctx.scope_start();
        let lowered = self.lower_keyword_args(
            ctx,
            id,
            name,
            &signature,
            &symbol.param_kinds,
            &[],
            args,
            keywords,
            span,
        );
        ctx.hide_locals_from(scope);
        let Some((lets, mut args)) = lowered else {
            return Expr::Error { span };
        };
        // The keyword arguments, after the required ones: `default:`, when
        // the call passes it.
        let keyword_args = args.split_off(function.arity().min(args.len()));
        let default = keyword_args
            .first()
            .filter(|arg| !arg.is_unsupplied())
            .cloned();
        let first_diagnostic = ctx.diagnostics.len();
        if function == CollectionFunction::Element
            && let Some(default) = &default
            && let [string, key] = args.as_slice()
            && matches!(self.expr_kind(string), ValueKind::String(_))
            && self.expr_kind(key) == ValueKind::Position
            && !matches!(self.expr_kind(default), ValueKind::Char | ValueKind::Error)
        {
            let call = self.string_element_or_object(ctx, id, args, default.clone(), span);
            return with_lets(lets, call, span);
        }
        let call = match self.lower_kernel_collection_call(ctx, function, &args, default, span) {
            Some(call) => call,
            None => {
                args.extend(keyword_args);
                self.lower_generic_send(ctx, &symbol, args, name, span)
            }
        };
        name_keyword_mismatches(&mut ctx.diagnostics[first_diagnostic..], &signature);
        with_lets(lets, call, span)
    }

    /// `element(s, p, default: d)` of a string, for a default that is not a
    /// `<character>`, as Dylan's `default:` may be any object: `d` at or
    /// past the end of `s`, and otherwise the character at `p`, as an
    /// `<object>`. The string, the position and the default are each
    /// evaluated once, in order.
    fn string_element_or_object(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        args: Vec<Expr>,
        default: Expr,
        span: Span,
    ) -> Expr {
        let scope = ctx.scope_start();
        let mut lets = Vec::new();
        let mut reads = Vec::new();
        for (role, value) in ["string", "key", "default"]
            .into_iter()
            .zip(args.into_iter().chain([default]))
        {
            let name = format!("element {role}@{}", span.start);
            let bound = self.bind_hidden_local(ctx, id, &name, value, span);
            let kind = ctx.local_kind(bound.local);
            reads.push(Expr::Local {
                local: bound.local,
                result_kind: kind,
                callable_signature: None,
                span,
            });
            lets.push(bound);
        }
        ctx.hide_locals_from(scope);
        let [string, key, default] = <[Expr; 3]>::try_from(reads).expect("three values were bound");
        let end = Expr::IntrinsicCall {
            intrinsic: Intrinsic::Text(TextFunction::EndPosition),
            args: vec![string.clone()],
            result_kind: ValueKind::Position,
            span,
        };
        let past_end = Expr::IntrinsicCall {
            intrinsic: Intrinsic::PositionCompare(CompareOp::Ge),
            args: vec![key.clone(), end],
            result_kind: ValueKind::Bool,
            span,
        };
        let character = self.checked_intrinsic_call(
            ctx,
            Intrinsic::Text(TextFunction::StringElement),
            "element",
            vec![string, key],
            span,
        );
        let call = Expr::If {
            condition: Box::new(past_end),
            refinement: None,
            then_branch: Box::new(default),
            else_branch: Box::new(Expr::Cast {
                site: SiteId::PENDING,
                value: Box::new(character),
                class: OBJECT_CLASS_ID,
                explicit: false,
                span,
            }),
            span,
        };
        with_lets(lets, call, span)
    }

    /// The kernel method's operation of a call of `function` with the
    /// required `args`, passing `default:` when given, where the receiver's
    /// type fixes the method: a text receiver, whose class is closed and
    /// over which the generic is sealed, so that its precise result, a
    /// `<character>` for `element`, is kept; or a receiver of a kernel class
    /// with a defaulting operation, for a call passing `default:`. Returns
    /// `None` for a send.
    fn lower_kernel_collection_call(
        &self,
        ctx: &mut FunctionContext,
        function: CollectionFunction,
        args: &[Expr],
        default: Option<Expr>,
        span: Span,
    ) -> Option<Expr> {
        let name = function.name();
        if args.len() != function.arity() {
            return None;
        }
        // A generic whose rows also differ by key, as `add!`'s builder rows
        // do by element, takes the one row the key's class picks.
        let key_class = args
            .get(function.collection_index() + 1)
            .and_then(|key| self.value_kind_class(self.expr_kind(key)));
        let text_method = self
            .value_kind_class(self.expr_kind(&args[function.collection_index()]))
            .and_then(|class| {
                let mut rows = collection::builtin_methods(function).filter(|method| {
                    matches!(method.operation, Intrinsic::Text(_))
                        && self.builtin_value_class(method.collection) == Some(class)
                        && match method.key {
                            Some(collection::MethodKey::Class(key))
                                if function == CollectionFunction::Add =>
                            {
                                key_class.is_some_and(|actual| {
                                    self.builtin_value_class(key) == Some(actual)
                                })
                            }
                            _ => true,
                        }
                });
                let row = rows.next()?;
                rows.next().is_none().then_some(row)
            });
        // A key typing does not know is a position, an `<object>`,
        // dispatches instead.
        if let Some(method) = text_method {
            let mut direct = args.to_vec();
            let operation = match &default {
                Some(default) => {
                    direct.push(default.clone());
                    method.with_default.unwrap_or(method.operation)
                }
                None => method.operation,
            };
            // Only an `<object>` argument may still be of the method's type.
            let may_dispatch = direct.iter().enumerate().any(|(index, arg)| {
                let kind = self.expr_kind(arg);
                kind == ValueKind::Any && !self.intrinsic_arg_matches(operation, index, kind)
            });
            if !may_dispatch || default.is_some() {
                return Some(self.checked_intrinsic_call(ctx, operation, name, direct, span));
            }
        }
        // A receiver of a kernel class with a defaulting operation runs it
        // for a call passing `default:`; any other call sends, and the
        // kernel method tests for `$unsupplied` itself.
        let default = default?;
        let collection = self.expr_kind(&args[function.collection_index()]);
        let operation = self.value_kind_class(collection).and_then(|class| {
            collection::builtin_methods(function)
                .filter(|method| {
                    self.builtin_value_class(method.collection)
                        .is_some_and(|kernel| self.is_same_or_subclass(class, kernel))
                })
                .find_map(|method| method.with_default)
        })?;
        let mut args = args.to_vec();
        args.push(default);
        Some(self.checked_intrinsic_call(ctx, operation, name, args, span))
    }

    /// The classes text values have in this module.
    fn text_classes(&self) -> Option<TextClasses> {
        Some(TextClasses {
            string: self.builtin_value_class(BuiltinValueClass::String)?,
            builder: self.builtin_value_class(BuiltinValueClass::StringBuilder)?,
            symbol: self.builtin_value_class(BuiltinValueClass::Symbol)?,
        })
    }

    /// Lowers a call of the text function `callee` with `args` to the first
    /// of its overloads `functions` whose parameters the arguments match,
    /// reporting a mismatch against the first when none does.
    fn lower_text_call(
        &self,
        ctx: &mut FunctionContext,
        callee: &str,
        functions: &[TextFunction],
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let Some(first) = functions.first() else {
            return Expr::Error { span };
        };
        let arity = first.params().len();
        if args.len() != arity {
            ctx.diagnostics
                .push(Self::arity_mismatch(callee, arity, args.len(), span));
            return Expr::Error { span };
        }
        let function = functions
            .iter()
            .find(|function| {
                args.iter().enumerate().all(|(index, arg)| {
                    self.intrinsic_arg_matches(
                        Intrinsic::Text(**function),
                        index,
                        self.expr_kind(arg),
                    )
                })
            })
            .unwrap_or(first);
        self.checked_intrinsic_call(ctx, Intrinsic::Text(*function), callee, args, span)
    }

    /// Lowers `copy-sequence(s, start: p, end: q)` and
    /// `starts-with?(s, prefix, at: p)`, whose keyword arguments are
    /// optional. The string is bound to a hidden local, as a missing
    /// keyword's default reads it again: the start or the end of `s`.
    /// Arguments evaluate in source order.
    fn lower_keyword_text_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let (arity, accepted): (usize, &[&str]) = if callee == "copy-sequence" {
            (1, &["start", "end"])
        } else {
            (2, &["at"])
        };
        let mut valid = true;
        for (index, keyword) in keywords.iter().enumerate() {
            if !accepted.contains(&keyword.name.as_str()) {
                ctx.diagnostics
                    .push(Self::unexpected_keyword(callee, keyword));
                valid = false;
            } else if keywords[..index]
                .iter()
                .any(|earlier| earlier.name == keyword.name)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(keyword.span, DiagnosticCode::DuplicateKeywordArgument)
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone())),
                );
                valid = false;
            }
        }
        if args.len() != arity {
            ctx.diagnostics
                .push(Self::arity_mismatch(callee, arity, args.len(), span));
            valid = false;
        }
        if !valid {
            return Expr::Error { span };
        }
        let scope_start = ctx.scope_start();
        let string = self.lower_expr(ctx, &args[0]);
        let string_kind = self.expr_kind(&string);
        let name = format!("{TEXT_STRING_LOCAL}@{}", span.start);
        let bound = self.bind_hidden_local(ctx, id, &name, string, args[0].span());
        let read = || Expr::Local {
            local: bound.local,
            result_kind: string_kind,
            callable_signature: None,
            span: args[0].span(),
        };
        let keyword = |name: &str| keywords.iter().find(|keyword| keyword.name == name);
        let call = |this: &mut Self,
                    ctx: &mut FunctionContext,
                    function: TextFunction,
                    args: Vec<Expr>,
                    span: Span| {
            this.checked_intrinsic_call(ctx, Intrinsic::Text(function), callee, args, span)
        };
        let result = if callee == "copy-sequence" {
            let start = match keyword("start") {
                Some(start) => {
                    let position = self.lower_expr(ctx, &start.value);
                    call(
                        self,
                        ctx,
                        TextFunction::CheckPosition,
                        vec![read(), position],
                        start.span,
                    )
                }
                None => call(self, ctx, TextFunction::StartPosition, vec![read()], span),
            };
            let end = match keyword("end") {
                Some(end) => self.lower_expr(ctx, &end.value),
                None => call(self, ctx, TextFunction::EndPosition, vec![read()], span),
            };
            call(
                self,
                ctx,
                TextFunction::CopyRange,
                vec![read(), start, end],
                span,
            )
        } else {
            let prefix = self.lower_expr(ctx, &args[1]);
            let at = match keyword("at") {
                Some(at) => self.lower_expr(ctx, &at.value),
                None => call(self, ctx, TextFunction::StartPosition, vec![read()], span),
            };
            call(
                self,
                ctx,
                TextFunction::StartsWith,
                vec![read(), prefix, at],
                span,
            )
        };
        ctx.hide_locals_from(scope_start);
        if matches!(result, Expr::Error { .. }) {
            return result;
        }
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: vec![Statement::Let(bound)],
                result,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Lowers Dylan's `truncate/(a, b)`, `floor/`, `ceiling/` or `round/`:
    /// the quotient rounded as named and the remainder `a - q * b`, as two
    /// `<integer>` values. It is written as the Loko it abbreviates:
    ///
    /// ```text
    /// let dividend :: <integer> = a; let divisor :: <integer> = b;
    /// if (divisor = 0) signal(make(<division-by-zero-error>, dividend: dividend))
    /// elseif (divisor = -1) %values(- dividend, 0)
    /// else %values(%floor/-quotient(dividend, divisor),
    ///              %floor/-remainder(dividend, divisor)) end
    /// ```
    ///
    /// so a zero divisor signals a `<division-by-zero-error>` whose
    /// `use-value` supplies the results, and the only quotient outside the
    /// integers, `- i64::MIN`, signals `type-error.overflow` as negation
    /// does. The runtime host computes the other results.
    fn lower_integer_division(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        rounding: Rounding,
        args: &[ast::Expr],
        span: Span,
        expected_results: Option<&[ValueKind]>,
    ) -> Expr {
        let [dividend_arg, divisor_arg] = args else {
            ctx.diagnostics
                .push(Self::arity_mismatch(rounding.name(), 2, args.len(), span));
            return Expr::Error { span };
        };
        let dividend = format!("{DIVISION_DIVIDEND_LOCAL}@{}", span.start);
        let divisor = format!("{DIVISION_DIVISOR_LOCAL}@{}", span.start);
        let name = |name: &str| ast::Expr::Name {
            id,
            name: name.into(),
            span,
        };
        let int = |value: i64| ast::Expr::Number { id, value, span };
        let integer = || ast::TypeExpr::Named {
            name: "<integer>".into(),
            span,
        };
        let bind = |local: &str, value: &ast::Expr| {
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Local,
                local,
                &id.raw().to_string(),
            );
            ast::Statement::Let(Box::new(ast::Let {
                id,
                definition,
                name: local.into(),
                pattern: ast::LetPattern::Name(ast::LetBinding {
                    definition,
                    name: local.into(),
                    type_annotation: Some(integer()),
                    span,
                }),
                type_annotation: Some(integer()),
                value: value.clone(),
                span: value.span(),
            }))
        };
        let block = |result: ast::Expr| ast::Block {
            id,
            statements: Vec::new(),
            result,
            span,
        };
        let is = |value: i64| ast::Expr::Compare {
            id,
            op: ast::CompareOp::Eq,
            lhs: Box::new(name(&divisor)),
            rhs: Box::new(int(value)),
            span,
        };
        let values = |values: Vec<ast::Expr>| ast::Expr::Call {
            id,
            callee: KERNEL_VALUES.into(),
            args: values,
            keywords: Vec::new(),
            span,
        };
        let part = |function: IntegerDivision| ast::Expr::Call {
            id,
            callee: integer_division_name(function),
            args: vec![name(&dividend), name(&divisor)],
            keywords: Vec::new(),
            span,
        };
        let by_zero = ast::Expr::Signal {
            id,
            condition: Box::new(ast::Expr::MakeObject {
                id,
                class: Box::new(ast::Expr::ClassName {
                    id,
                    class_name: ConditionClass::DivisionByZeroError.name().into(),
                    span,
                }),
                fields: vec![ast::KeywordArg {
                    id,
                    name: "dividend".into(),
                    value: name(&dividend),
                    span,
                }],
                span,
            }),
            span,
        };
        let negated = values(vec![
            ast::Expr::UnaryMinus {
                id,
                expr: Box::new(name(&dividend)),
                span,
            },
            int(0),
        ]);
        let divided = values(vec![
            part(IntegerDivision::quotient(rounding)),
            part(IntegerDivision::remainder(rounding)),
        ]);
        let tree = ast::Expr::Block {
            id,
            exit: None,
            body: Box::new(ast::Block {
                id,
                statements: vec![bind(&dividend, dividend_arg), bind(&divisor, divisor_arg)],
                result: ast::Expr::If {
                    id,
                    arms: vec![
                        ast::CondArm {
                            test: is(0),
                            body: block(by_zero),
                            span,
                        },
                        ast::CondArm {
                            test: is(-1),
                            body: block(negated),
                            span,
                        },
                    ],
                    else_body: Some(Box::new(block(divided))),
                    span,
                },
                span,
            }),
            cleanup: None,
            span,
        };
        self.lower_expr_expected_results(ctx, &tree, expected_results)
    }

    /// The diagnostic for a keyword argument `callee` does not take.
    fn unexpected_keyword(callee: &str, keyword: &ast::KeywordArg) -> Diagnostic {
        Diagnostic::error(keyword.span, DiagnosticCode::UnexpectedKeywordArgument)
            .with_arg(DiagnosticArg::text("callee", callee.to_string()))
            .with_arg(DiagnosticArg::text("keyword", keyword.name.clone()))
    }

    /// The diagnostic for a call of `callee` with `actual` arguments where
    /// it takes `expected`.
    /// The error for a call passing `actual` arguments to `callee`, which
    /// takes `#rest` after `required` ones.
    fn too_few_arguments(callee: &str, required: usize, actual: usize, span: Span) -> Diagnostic {
        Diagnostic::error(span, DiagnosticCode::FunctionTooFewArguments)
            .with_arg(DiagnosticArg::text("callee", callee))
            .with_arg(DiagnosticArg::unsigned(
                "expected_count",
                u64::try_from(required).unwrap_or(u64::MAX),
            ))
            .with_arg(DiagnosticArg::unsigned(
                "actual_count",
                u64::try_from(actual).unwrap_or(u64::MAX),
            ))
    }

    /// `args` for a callee taking `required` arguments and then a `#rest`
    /// parameter: the required ones, then a `<vector>` of the others, which
    /// is constant as a vector literal is. Dylan's rest sequence is not
    /// for the callee to modify.
    fn pack_rest_args(&self, mut args: Vec<Expr>, required: usize, span: Span) -> Vec<Expr> {
        let rest = args.split_off(required.min(args.len()));
        let rest_span = match (rest.first(), rest.last()) {
            (Some(first), Some(last)) => first.span().cover(last.span()),
            _ => Span::new(span.end, span.end),
        };
        args.push(Expr::VectorLiteral {
            elements: rest,
            class: self
                .builtin_value_class(BuiltinValueClass::Vector)
                .unwrap_or(ClassId(u32::MAX)),
            span: rest_span,
        });
        args
    }

    fn arity_mismatch(callee: &str, expected: usize, actual: usize, span: Span) -> Diagnostic {
        Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
            .with_arg(DiagnosticArg::text("callee", callee.to_string()))
            .with_arg(DiagnosticArg::unsigned(
                "expected_count",
                u64::try_from(expected).unwrap_or(u64::MAX),
            ))
            .with_arg(DiagnosticArg::unsigned(
                "actual_count",
                u64::try_from(actual).unwrap_or(u64::MAX),
            ))
    }

    /// Checks `args` against what `intrinsic` takes, reporting mismatches as
    /// arguments of `callee`, and builds the call.
    fn checked_intrinsic_call(
        &self,
        ctx: &mut FunctionContext,
        intrinsic: Intrinsic,
        callee: &str,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let args = args
            .into_iter()
            .enumerate()
            .map(|(index, arg)| self.lower_intrinsic_arg(intrinsic, index, arg))
            .collect::<Vec<_>>();
        for (index, arg) in args.iter().enumerate() {
            let actual = self.expr_kind(arg);
            if !self.intrinsic_arg_matches(intrinsic, index, actual) {
                ctx.diagnostics.push(
                    Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                        .with_arg(DiagnosticArg::text("function", callee.to_string()))
                        .with_arg(DiagnosticArg::unsigned(
                            "argument_index",
                            u64::try_from(index + 1).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::text(
                            "expected",
                            intrinsic_expected_name(intrinsic, index),
                        ))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
            }
        }
        Expr::IntrinsicCall {
            intrinsic,
            args,
            result_kind: self.intrinsic_result_kind(intrinsic),
            span,
        }
    }

    fn lower_float_call(
        &self,
        ctx: &mut FunctionContext,
        builtin: FloatBuiltin,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        if args.len() != builtin.arity() {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                    .with_arg(DiagnosticArg::text("callee", builtin.name()))
                    .with_arg(DiagnosticArg::unsigned(
                        "expected_count",
                        u64::try_from(builtin.arity()).unwrap_or(u64::MAX),
                    ))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error { span };
        }
        // Literals adopt a single-float partner's format, as for arithmetic.
        let single = args.iter().any(|arg| self.expr_kind(arg) == ValueKind::F32);
        let mut args = args;
        if single {
            adapt_literal_args(&mut args, core::iter::repeat(Some(&ValueKind::F32)));
        }
        let mut valid = true;
        for (index, arg) in args.iter().enumerate() {
            let actual = self.expr_kind(arg);
            if !matches!(actual, ValueKind::F64 | ValueKind::F32 | ValueKind::Error) {
                valid = false;
                ctx.diagnostics.push(
                    Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                        .with_arg(DiagnosticArg::text("function", builtin.name()))
                        .with_arg(DiagnosticArg::unsigned(
                            "argument_index",
                            u64::try_from(index + 1).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::text("expected", "<float>"))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
            }
        }
        if !valid {
            return Expr::Error { span };
        }
        // Mixed formats widen to binary64.
        if args.iter().any(|arg| self.expr_kind(arg) == ValueKind::F64) {
            args = args
                .into_iter()
                .map(|arg| {
                    if self.expr_kind(&arg) == ValueKind::F32 {
                        promote_operand(arg, NumericKind::SingleFloat, NumericKind::Float)
                    } else {
                        arg
                    }
                })
                .collect();
        }
        Expr::FloatCall {
            builtin,
            args,
            span,
        }
    }

    fn lower_intrinsic_arg(&self, intrinsic: Intrinsic, index: usize, arg: Expr) -> Expr {
        if index == 0
            && matches!(
                intrinsic,
                Intrinsic::GenericFunctionName | Intrinsic::GenericFunctionMethods
            )
            && let Some(generic) = self.generic_metaobject_arg(&arg)
        {
            return Expr::GenericFunctionObject {
                generic,
                span: arg.span(),
            };
        }
        arg
    }

    fn generic_metaobject_arg(&self, arg: &Expr) -> Option<GenericFunctionId> {
        match self.expr_kind(arg) {
            ValueKind::KnownCallable(KnownCallable::GenericFunction(generic))
            | ValueKind::GenericFunctionObject(generic) => Some(generic),
            _ => None,
        }
    }

    fn intrinsic_result_kind(&self, intrinsic: Intrinsic) -> ValueKind {
        match intrinsic {
            Intrinsic::ClassName
            | Intrinsic::SlotName
            | Intrinsic::SlotType
            | Intrinsic::SlotInitKeyword
            | Intrinsic::SlotGetter
            | Intrinsic::GenericFunctionName
            | Intrinsic::FloatToString
            | Intrinsic::IntegerToString
            | Intrinsic::Concatenate
            | Intrinsic::TypeName => self
                .builtin_value_class(BuiltinValueClass::String)
                .map_or(ValueKind::Error, ValueKind::String),
            Intrinsic::ClassDirectSlots
            | Intrinsic::ClassEffectiveSlots
            | Intrinsic::GenericFunctionMethods
            | Intrinsic::MethodSpecializers => self
                .builtin_value_class(BuiltinValueClass::Vector)
                .map_or(ValueKind::Error, ValueKind::Vector),
            Intrinsic::SlotMutable
            | Intrinsic::InstanceOfType
            | Intrinsic::CharCompare(_)
            | Intrinsic::PositionCompare(_) => ValueKind::Bool,
            Intrinsic::CollectionSize
            | Intrinsic::CharToInteger
            | Intrinsic::PositionToInteger
            | Intrinsic::IntegerDivision(_) => ValueKind::I64,
            Intrinsic::Text(function) => self.text_classes().map_or(ValueKind::Error, |classes| {
                classes.result_kind(function.result())
            }),
            Intrinsic::StringStep => ValueKind::Char,
            Intrinsic::TableVersion | Intrinsic::TablePositions | Intrinsic::TableNextPosition => {
                ValueKind::I64
            }
            Intrinsic::TablePositionValue | Intrinsic::TablePositionKey => ValueKind::Any,
            Intrinsic::VectorConstant => ValueKind::Bool,
            Intrinsic::MakeVector => self
                .builtin_value_class(BuiltinValueClass::Vector)
                .map_or(ValueKind::Error, ValueKind::Vector),
            Intrinsic::MakeStretchyVector => self
                .builtin_value_class(BuiltinValueClass::StretchyVector)
                .map_or(ValueKind::Error, ValueKind::Object),
            Intrinsic::MakeTable => self
                .builtin_value_class(BuiltinValueClass::Table)
                .map_or(ValueKind::Error, ValueKind::Object),
            Intrinsic::TableKeyExists | Intrinsic::TableRemoveKey => ValueKind::Bool,
            Intrinsic::ListPair => self
                .builtin_value_class(BuiltinValueClass::Pair)
                .map_or(ValueKind::Error, ValueKind::Object),
            Intrinsic::EmptyList => self
                .builtin_value_class(BuiltinValueClass::EmptyList)
                .map_or(ValueKind::Error, ValueKind::Object),
            Intrinsic::ListHead
            | Intrinsic::ListTail
            | Intrinsic::ListHeadSetter
            | Intrinsic::ListTailSetter => ValueKind::Any,
            Intrinsic::StretchyAdd => self
                .builtin_value_class(BuiltinValueClass::StretchyVector)
                .map_or(ValueKind::Error, ValueKind::Object),
            Intrinsic::LimitedIntegerBase => self
                .builtin_value_class(BuiltinValueClass::Integer)
                .map_or(ValueKind::Error, ValueKind::ClassObject),
            Intrinsic::SubclassTypeClass => ValueKind::ClassObject(OBJECT_CLASS_ID),
            Intrinsic::ObjectClass
            | Intrinsic::ClassDirectSuperclass
            | Intrinsic::SlotSetter
            | Intrinsic::MethodGenericFunction
            | Intrinsic::VectorElement
            | Intrinsic::VectorElementOr
            | Intrinsic::VectorElementSetter
            | Intrinsic::TableElement
            | Intrinsic::TableElementOr
            | Intrinsic::TableElementSetter
            | Intrinsic::Unsupplied
            | Intrinsic::KeywordAbsent
            | Intrinsic::LimitedIntegerMin
            | Intrinsic::LimitedIntegerMax => ValueKind::Any,
        }
    }

    /// The collection a value of kind `actual` is, by its static class: a
    /// `<vector>` (a `<stretchy-vector>` too) or a `<table>`.
    fn collection_kind(&self, actual: ValueKind) -> Option<CollectionKind> {
        let class = match actual {
            ValueKind::String(_) => return Some(CollectionKind::String),
            ValueKind::Vector(class) | ValueKind::Object(class) => class,
            _ => return None,
        };
        let is = |kind| {
            self.builtin_value_class(kind)
                .is_some_and(|builtin| self.is_same_or_subclass(class, builtin))
        };
        if is(BuiltinValueClass::Vector) {
            Some(CollectionKind::Vector)
        } else if is(BuiltinValueClass::Table) {
            Some(CollectionKind::Table)
        } else {
            None
        }
    }

    /// Whether `actual` is a type object: a class object, or a value
    /// declared as `<type>` or one of its subclasses.
    fn is_type_object_kind(&self, actual: ValueKind) -> bool {
        matches!(actual, ValueKind::ClassObject(_) | ValueKind::Any)
            || self.is_builtin_subclass_instance(actual, BuiltinValueClass::Type)
    }

    /// Whether `actual` is the object kind of the built-in class `class` or
    /// a built-in subclass of it.
    fn is_builtin_subclass_instance(&self, actual: ValueKind, class: BuiltinValueClass) -> bool {
        matches!(actual, ValueKind::Object(id)
            if self.builtin_value_class(class).is_some_and(|class| self.is_same_or_subclass(id, class)))
    }

    /// Whether `actual` is the object kind of the built-in class `class`,
    /// as a value declared with that class's name has.
    fn is_builtin_instance(&self, actual: ValueKind, class: BuiltinValueClass) -> bool {
        matches!(actual, ValueKind::Object(id) if Some(id) == self.builtin_value_class(class))
    }

    fn intrinsic_arg_matches(&self, intrinsic: Intrinsic, index: usize, actual: ValueKind) -> bool {
        if actual == ValueKind::Error {
            return true;
        }
        match (intrinsic, index) {
            (Intrinsic::ObjectClass, 0) => matches!(
                actual,
                ValueKind::Any
                    | ValueKind::I64
                    | ValueKind::F64
                    | ValueKind::I32
                    | ValueKind::U32
                    | ValueKind::F32
                    | ValueKind::String(_)
                    | ValueKind::Symbol(_)
                    | ValueKind::Vector(_)
                    | ValueKind::Object(_)
                    | ValueKind::ClassObject(_)
                    | ValueKind::GenericFunctionObject(_)
                    | ValueKind::MethodObject(_)
                    | ValueKind::SlotDescriptorObject { .. }
                    | ValueKind::Callable
                    | ValueKind::KnownCallable(_)
            ),
            (
                Intrinsic::ClassName
                | Intrinsic::ClassDirectSuperclass
                | Intrinsic::ClassDirectSlots
                | Intrinsic::ClassEffectiveSlots,
                0,
            ) => {
                // A value declared `<class>` is a class object too.
                matches!(actual, ValueKind::ClassObject(_) | ValueKind::Any)
                    || self.is_builtin_instance(actual, BuiltinValueClass::Class)
            }
            (
                Intrinsic::SlotName
                | Intrinsic::SlotType
                | Intrinsic::SlotInitKeyword
                | Intrinsic::SlotMutable
                | Intrinsic::SlotGetter
                | Intrinsic::SlotSetter,
                0,
            ) => matches!(
                actual,
                ValueKind::SlotDescriptorObject { .. } | ValueKind::Any
            ),
            // A value declared `<generic-function>`, such as a
            // `<no-applicable-method>`'s generic, is the metaobject too.
            (Intrinsic::GenericFunctionName | Intrinsic::GenericFunctionMethods, 0) => {
                matches!(actual, ValueKind::GenericFunctionObject(_) | ValueKind::Any)
                    || self.is_builtin_instance(actual, BuiltinValueClass::GenericFunction)
            }
            (Intrinsic::MethodGenericFunction | Intrinsic::MethodSpecializers, 0) => {
                matches!(actual, ValueKind::MethodObject(_) | ValueKind::Any)
                    || self.is_builtin_instance(actual, BuiltinValueClass::Method)
            }
            // A string's size is its own text operation.
            (Intrinsic::CollectionSize, 0) => matches!(
                self.collection_kind(actual),
                Some(CollectionKind::Vector | CollectionKind::Table)
            ),
            (Intrinsic::VectorElement | Intrinsic::VectorElementOr, 0)
            | (Intrinsic::VectorElementSetter, 1) => {
                self.collection_kind(actual) == Some(CollectionKind::Vector)
            }
            (
                Intrinsic::TableVersion
                | Intrinsic::TablePositions
                | Intrinsic::TableNextPosition
                | Intrinsic::TablePositionValue
                | Intrinsic::TablePositionKey,
                0,
            ) => self.collection_kind(actual) == Some(CollectionKind::Table),
            (
                Intrinsic::TableNextPosition
                | Intrinsic::TablePositionValue
                | Intrinsic::TablePositionKey,
                _,
            ) => actual == ValueKind::I64,
            (Intrinsic::VectorConstant, 0) => {
                self.collection_kind(actual) == Some(CollectionKind::Vector)
            }
            (Intrinsic::StretchyAdd, 0) => {
                self.is_builtin_subclass_instance(actual, BuiltinValueClass::StretchyVector)
            }
            (
                Intrinsic::TableElement
                | Intrinsic::TableElementOr
                | Intrinsic::TableKeyExists
                | Intrinsic::TableRemoveKey,
                0,
            )
            | (Intrinsic::TableElementSetter, 1) => {
                self.collection_kind(actual) == Some(CollectionKind::Table)
            }
            // Any value boxes to the `<object>` an element, a key or a
            // default is.
            (Intrinsic::VectorElementOr | Intrinsic::TableElementOr, 2)
            | (Intrinsic::VectorElementSetter | Intrinsic::TableElementSetter, 0)
            | (
                Intrinsic::StretchyAdd
                | Intrinsic::TableElement
                | Intrinsic::TableElementOr
                | Intrinsic::TableKeyExists
                | Intrinsic::TableRemoveKey,
                1,
            )
            | (Intrinsic::TableElementSetter, 2)
            | (Intrinsic::MakeVector | Intrinsic::MakeStretchyVector, 1) => {
                !matches!(actual, ValueKind::Error)
            }
            (Intrinsic::VectorElement | Intrinsic::VectorElementOr, 1)
            | (Intrinsic::VectorElementSetter, 2)
            | (Intrinsic::MakeVector | Intrinsic::MakeStretchyVector, 0)
            | (Intrinsic::IntegerToString, 0) => {
                matches!(actual, ValueKind::I64 | ValueKind::I32 | ValueKind::U32)
            }
            (Intrinsic::FloatToString, 0) => matches!(actual, ValueKind::F64 | ValueKind::F32),
            (Intrinsic::Concatenate, 0 | 1) => matches!(actual, ValueKind::String(_)),
            (Intrinsic::InstanceOfType, 0) => true,
            (Intrinsic::InstanceOfType, 1) | (Intrinsic::TypeName, 0) => {
                self.is_type_object_kind(actual)
            }
            (
                Intrinsic::LimitedIntegerBase
                | Intrinsic::LimitedIntegerMin
                | Intrinsic::LimitedIntegerMax,
                0,
            ) => {
                actual == ValueKind::Any
                    || self
                        .is_builtin_subclass_instance(actual, BuiltinValueClass::LimitedIntegerType)
            }
            (Intrinsic::SubclassTypeClass, 0) => {
                actual == ValueKind::Any
                    || self.is_builtin_subclass_instance(actual, BuiltinValueClass::SubclassType)
            }
            (Intrinsic::Text(function), index) => {
                let classes = self.text_classes();
                function
                    .params()
                    .get(index)
                    .zip(classes)
                    .is_some_and(|(kind, classes)| classes.arg_matches(*kind, actual))
            }
            (Intrinsic::CharCompare(_), 0 | 1) | (Intrinsic::CharToInteger, 0) => {
                actual == ValueKind::Char
            }
            (Intrinsic::PositionCompare(_), 0 | 1) | (Intrinsic::PositionToInteger, 0) => {
                actual == ValueKind::Position
            }
            (Intrinsic::IntegerDivision(_), 0 | 1) => {
                matches!(actual, ValueKind::I64 | ValueKind::I32 | ValueKind::U32)
            }
            (Intrinsic::ListHead | Intrinsic::ListTail, 0) => {
                self.is_builtin_subclass_instance(actual, BuiltinValueClass::List)
            }
            (Intrinsic::ListHeadSetter | Intrinsic::ListTailSetter, 1) => {
                self.is_builtin_subclass_instance(actual, BuiltinValueClass::Pair)
            }
            // Any value boxes to the `<object>` a pair holds.
            (Intrinsic::ListPair, 0 | 1)
            | (Intrinsic::ListHeadSetter | Intrinsic::ListTailSetter, 0) => {
                !matches!(actual, ValueKind::Error)
            }
            _ => false,
        }
    }

    fn lower_applied_generic_callable_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        generic: GenericFunctionId,
        mut args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let Some(symbol) = self.generic_symbols.get(generic.0 as usize) else {
            return Expr::Error { span };
        };
        if symbol.arity() != args.len() {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                    .with_arg(DiagnosticArg::text("callee", symbol.name.clone()))
                    .with_arg(DiagnosticArg::unsigned(
                        "expected_count",
                        u64::try_from(symbol.arity()).unwrap_or(u64::MAX),
                    ))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error { span };
        }
        adapt_literal_args(&mut args, symbol.param_kinds.iter().map(Option::as_ref));
        self.narrow_generic_args(&mut args, &symbol.param_kinds);
        for (index, (arg, declared)) in args.iter().zip(&symbol.param_kinds).enumerate() {
            ctx.diagnostics
                .extend(self.generic_argument_diagnostic(arg, generic, index, *declared));
        }
        Expr::CallableCall {
            callable: Box::new(target),
            args,
            arguments: CallArguments::Each,
            arg_kinds: (0..symbol.arity())
                .map(|index| self.generic_callable_arg_kind(generic, index))
                .collect(),
            keywords: Vec::new(),
            result_kinds: symbol.return_kinds.clone(),
            result_callable_sigs: symbol.return_callable_sigs.clone(),
            span,
        }
    }

    /// Lowers a call of a binding whose value is statically `callable`, a
    /// function or generic, as a call of the definition itself: the same
    /// direct call or send, keywords and defaults included, that a call of
    /// its name makes. Reading a binding has no effect to keep, and a
    /// binding of that type always holds that definition. Returns `None`
    /// for a slot accessor, which is called through its value.
    fn lower_known_binding_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callable: KnownCallable,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Option<Expr> {
        let name = match callable {
            KnownCallable::DirectFunction(function) => {
                self.functions.get(function.0 as usize)?.name.clone()
            }
            KnownCallable::GenericFunction(generic) => {
                self.generic_symbols.get(generic.0 as usize)?.name.clone()
            }
        };
        // A built-in's value calls the built-in, inline where a call of its
        // name would be.
        let built_in = match callable {
            KnownCallable::DirectFunction(function) => {
                function.0 as usize >= self.program_functions
            }
            KnownCallable::GenericFunction(generic) => {
                self.built_in_generics.iter().any(|(_, id)| *id == generic)
            }
        };
        if built_in {
            return Some(self.lower_built_in_call(ctx, id, &name, args, keywords, None, span));
        }
        // A slot's getter reads a receiver of the slot's class directly.
        if let KnownCallable::GenericFunction(generic) = callable
            && self.slot_getter(&name) == Some(generic)
            && let [receiver] = args
            && keywords.is_empty()
        {
            let receiver = self.lower_expr(ctx, receiver);
            return Some(self.lower_accessor_call(ctx, receiver, &name, span));
        }
        if let Some(function) = self
            .collection_generics
            .iter()
            .find(|(_, id)| matches!(callable, KnownCallable::GenericFunction(generic) if generic == *id))
            .map(|(function, _)| *function)
        {
            return Some(self.lower_collection_call(ctx, id, function, args, keywords, span));
        }
        if let Some(signature) = self.keyword_signature(&name).cloned() {
            return Some(self.lower_keyword_call(ctx, id, &name, &signature, args, keywords, span));
        }
        for keyword in keywords {
            ctx.diagnostics
                .push(Self::unexpected_keyword(&name, keyword));
        }
        let signatures = match callable {
            KnownCallable::DirectFunction(function) => self
                .functions
                .get(function.0 as usize)
                .map(|symbol| symbol.param_callable_sigs.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let lowered = self.lower_args_against(ctx, args, &signatures);
        Some(match callable {
            KnownCallable::DirectFunction(function) => {
                let symbol = self.functions.get(function.0 as usize)?.clone();
                self.lower_function_call(ctx, &symbol, lowered, &name, span)
            }
            KnownCallable::GenericFunction(generic) => {
                let symbol = self.generic_symbols.get(generic.0 as usize)?.clone();
                self.lower_generic_send(ctx, &symbol, lowered, &name, span)
            }
        })
    }

    /// Lowers a call through the value `target` evaluates to, as Dylan
    /// calls any operand: a local or captured binding named `binding`,
    /// whatever module-level definition of the name it shadows, or any
    /// other operand (`binding` is `None`), such as `f(1)(2)`. A callable
    /// value, a function or generic it names statically, and a
    /// generic-function object are called through the value, typed when
    /// its signature is known. An `<object>` is checked to be a
    /// `<function>`, as a declaration would check it, and called untyped.
    /// A value of any other type cannot be called.
    #[allow(clippy::too_many_arguments)]
    fn lower_value_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        binding: Option<&str>,
        target: Expr,
        args: Vec<Expr>,
        keywords: &[ast::KeywordArg],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let callee = binding.unwrap_or(VALUE_CALLEE);
        match self.expr_kind(&target) {
            ValueKind::Callable | ValueKind::KnownCallable(_) => self.lower_callable_value_call(
                ctx,
                id,
                callee,
                target,
                args,
                keywords,
                expected_results,
                span,
            ),
            ValueKind::Any => {
                let target = self.narrow(target, ValueKind::Callable, None);
                self.lower_callable_value_call(
                    ctx,
                    id,
                    callee,
                    target,
                    args,
                    keywords,
                    expected_results,
                    span,
                )
            }
            ValueKind::GenericFunctionObject(generic) => {
                for keyword in keywords {
                    ctx.diagnostics
                        .push(Self::unexpected_keyword(callee, keyword));
                }
                self.lower_applied_generic_callable_call(ctx, target, generic, args, span)
            }
            ValueKind::Error => Expr::Error { span },
            actual => {
                let diagnostic = match binding {
                    Some(local) => Diagnostic::error(span, DiagnosticCode::LocalNotCallable)
                        .with_arg(DiagnosticArg::text("local", local.to_string())),
                    None => Diagnostic::error(target.span(), DiagnosticCode::ValueNotCallable),
                };
                ctx.diagnostics.push(
                    diagnostic.with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
                Expr::Error { span }
            }
        }
    }

    /// Lowers a call through the callable value `target` with the
    /// positional arguments `args`, already lowered, and the keyword
    /// arguments `keywords`, of the call `id` that names the value `callee`.
    #[allow(clippy::too_many_arguments)]
    fn lower_callable_value_call(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        callee: &str,
        target: Expr,
        args: Vec<Expr>,
        keywords: &[ast::KeywordArg],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        if keywords.is_empty() {
            return self.lower_applied_dynamic_callable_call(
                ctx,
                target,
                args,
                false,
                Vec::new(),
                expected_results,
                span,
            );
        }
        let mut valid = true;
        for (index, keyword) in keywords.iter().enumerate() {
            if keywords[..index]
                .iter()
                .any(|earlier| earlier.name == keyword.name)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(keyword.span, DiagnosticCode::DuplicateKeywordArgument)
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone())),
                );
                valid = false;
            }
        }
        let signature = self.callable_signature_of(&target);
        if let Some(signature) = &signature {
            for keyword in keywords {
                if !signature
                    .keywords
                    .iter()
                    .any(|known| known.name == keyword.name)
                {
                    ctx.diagnostics
                        .push(Self::unexpected_keyword(callee, keyword));
                    valid = false;
                }
            }
        }
        let scope = ctx.scope_start();
        let values = keywords
            .iter()
            .map(|keyword| self.lower_expr(ctx, &keyword.value))
            .collect::<Vec<_>>();
        if !valid {
            ctx.hide_locals_from(scope);
            return Expr::Error { span };
        }
        let Some(signature) = signature else {
            // An untyped callable takes the keywords as the call passes
            // them, in source order; its entry finds out which it takes.
            let mut args = args;
            args.extend(values);
            let names = keywords
                .iter()
                .map(|keyword| keyword.name.clone())
                .collect();
            ctx.hide_locals_from(scope);
            return self.lower_applied_dynamic_callable_call(
                ctx,
                target,
                args,
                false,
                names,
                expected_results,
                span,
            );
        };
        // A typed callable takes every keyword of its signature, in
        // declaration order: the call passes `$unsupplied` for those it
        // does not pass, and evaluates what it passes in source order.
        for (keyword, value) in keywords.iter().zip(&values) {
            let Some(known) = signature
                .keywords
                .iter()
                .find(|known| known.name == keyword.name)
            else {
                continue;
            };
            let actual = self.expr_kind(value);
            if !self.kinds_match(known.kind, actual) {
                ctx.diagnostics.push(
                    Diagnostic::error(value.span(), DiagnosticCode::KeywordArgumentTypeMismatch)
                        .with_arg(DiagnosticArg::text("callee", callee.to_string()))
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", self.kind_name(known.kind)))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
            }
        }
        let positions = keywords
            .iter()
            .filter_map(|keyword| {
                signature
                    .keywords
                    .iter()
                    .position(|known| known.name == keyword.name)
            })
            .collect::<Vec<_>>();
        let in_order = positions.windows(2).all(|pair| pair[0] < pair[1]);
        let mut lets = Vec::new();
        let (mut args, mut values) = (args, values);
        if !in_order {
            args = args
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    let name = format!("{callee} #arg {index}@{}", span.start);
                    self.hidden_read(ctx, &mut lets, id, &name, value, span)
                })
                .collect();
            values = values
                .into_iter()
                .zip(keywords)
                .map(|(value, keyword)| {
                    let name = format!("{callee} #key {}@{}", keyword.name, span.start);
                    self.hidden_read(ctx, &mut lets, id, &name, value, span)
                })
                .collect();
        }
        let mut slots = signature.keywords.iter().map(|_| None).collect::<Vec<_>>();
        for (position, value) in positions.into_iter().zip(values) {
            slots[position] = Some(value);
        }
        args.extend(
            slots
                .into_iter()
                .map(|slot| slot.unwrap_or_else(|| self.keyword_absent(span))),
        );
        ctx.hide_locals_from(scope);
        let call = self.lower_applied_dynamic_callable_call(
            ctx,
            target,
            args,
            true,
            Vec::new(),
            expected_results,
            span,
        );
        with_lets(lets, call, span)
    }

    /// The signature of the callable `target` evaluates to, when known:
    /// its own, or that of the function or generic it is statically.
    fn callable_signature_of(&self, target: &Expr) -> Option<CallableSignature> {
        self.expr_callable_signature(target)
            .or_else(|| match self.expr_kind(target) {
                ValueKind::KnownCallable(callable) => self.known_callable_signature(callable),
                _ => None,
            })
    }

    /// Lowers a call through `target` with `args`. For a typed callable,
    /// `with_keywords` says whether `args` end with one argument per
    /// keyword of its signature; otherwise the call passes none, and the
    /// value fills each default. For an untyped one, the last `untyped_keywords.len()` of
    /// `args` pass those keywords.
    #[allow(clippy::too_many_arguments)]
    fn lower_applied_dynamic_callable_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        mut args: Vec<Expr>,
        with_keywords: bool,
        untyped_keywords: Vec<String>,
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        if let Some(signature) = self.callable_signature_of(&target) {
            // Keyword arguments, `$unsupplied` for those the call does not
            // pass, follow the positional ones.
            let keyword_count = signature.keywords.len();
            let keyword_args = if with_keywords {
                args.split_off(args.len().saturating_sub(keyword_count))
            } else {
                (0..keyword_count)
                    .map(|_| self.keyword_absent(span))
                    .collect()
            };
            // A value taking `#rest` gets the arguments after its required
            // ones in a vector the call makes.
            let arguments = if signature.rest {
                if args.len() < signature.required() {
                    ctx.diagnostics.push(Self::too_few_arguments(
                        VALUE_CALLEE,
                        signature.required(),
                        args.len(),
                        span,
                    ));
                    return Expr::Error { span };
                }
                args = self.pack_rest_args(args, signature.required(), span);
                CallArguments::Rest
            } else {
                CallArguments::Each
            };
            if signature.params.len() != args.len() {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                        .with_arg(DiagnosticArg::text("callee", VALUE_CALLEE))
                        .with_arg(DiagnosticArg::unsigned(
                            "expected_count",
                            u64::try_from(signature.params.len()).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::unsigned(
                            "actual_count",
                            u64::try_from(args.len()).unwrap_or(u64::MAX),
                        )),
                );
                return Expr::Error { span };
            }
            adapt_literal_args(&mut args, signature.params.iter().map(Some));
            self.narrow_args(&mut args, &signature.params, &signature.param_sigs);
            for (index, (arg, expected)) in args.iter().zip(&signature.params).enumerate() {
                let actual = self.expr_kind(arg);
                let mismatch = |expected: String, actual: String| {
                    Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                        .with_arg(DiagnosticArg::text("function", VALUE_CALLEE))
                        .with_arg(DiagnosticArg::unsigned(
                            "argument_index",
                            u64::try_from(index + 1).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::text("expected", expected))
                        .with_arg(DiagnosticArg::text("actual", actual))
                };
                if !self.kinds_match(*expected, actual) {
                    ctx.diagnostics
                        .push(mismatch(self.kind_name(*expected), self.kind_name(actual)));
                } else if let Some(expected) = signature.param_sig(index) {
                    ctx.diagnostics.extend(self.callable_value_diagnostic(
                        arg.span(),
                        expected,
                        actual,
                        self.expr_callable_signature(arg).as_ref(),
                        mismatch,
                    ));
                }
            }
            args.extend(keyword_args);
            // A block's exit procedure never returns, so nothing relies on
            // its results.
            if ctx.is_block_exit(&target) {
                let result_len = signature.results.len();
                return Expr::CallableCall {
                    callable: Box::new(target),
                    args,
                    arguments: CallArguments::Each,
                    arg_kinds: signature.params,
                    keywords: Vec::new(),
                    result_kinds: signature.results,
                    result_callable_sigs: vec![None; result_len],
                    span,
                };
            }
            // Otherwise the call is a typed one: lowering calls the value's
            // typed entry with the signature's register types, which static
            // typing, and the adapter an untyped callable gets where it
            // enters typed code, make agree with the entry. Every class shares
            // the `object` register type, so class results are cast as for an
            // untyped call, and a limited result is checked against its range.
            // Arguments need nothing here: the callee's indirect entry checks
            // the classes and ranges it relies on.
            return self
                .checked_callable_call(
                    ctx,
                    target,
                    args,
                    arguments,
                    signature,
                    Self::signature_result_check,
                    span,
                )
                .unwrap_or_else(|diagnostic| {
                    ctx.diagnostics.push(diagnostic);
                    Expr::Error { span }
                });
        }
        let Some(result_kinds) = expected_results else {
            ctx.diagnostics.push(Diagnostic::error(
                target.span(),
                DiagnosticCode::UntypedCallableResult,
            ));
            return Expr::Error { span };
        };
        // An untyped callable's register kinds come from the call site, and
        // nothing about the callee backs them. Lowering calls the value's
        // typed entry when its signature has exactly this call's shape, and
        // otherwise its boxed entry, unboxing each result where it lands.
        // Either way the registers only carry `object` for classes, so a
        // `<square>` arrives where the context wants a `<circle>`: each
        // result crosses at the widest kind its register can carry and is
        // checked back to the context's kind, a limited integer by a range
        // conversion, a class by a cast. Both are ordinary check sites, so
        // soft typing sees (and may later prove) them. Arguments go at the
        // caller's kinds, limited integers widened; the callee's entry checks
        // what it relies on, since only it knows.
        let as_integer = |kind: ValueKind| match kind {
            ValueKind::I32 | ValueKind::U32 => ValueKind::I64,
            kind => kind,
        };
        let positional = args.len().saturating_sub(untyped_keywords.len());
        let arg_kinds = args[..positional]
            .iter()
            .map(|arg| as_integer(self.expr_kind(arg)))
            .collect::<Vec<_>>();
        let mut called = CallableSignature::flat(arg_kinds, result_kinds.to_vec());
        called.keywords = untyped_keywords
            .into_iter()
            .map(|name| CallableKeyword {
                name,
                kind: ValueKind::Any,
            })
            .collect();
        self.checked_callable_call(
            ctx,
            target,
            args,
            CallArguments::Each,
            called,
            Self::untyped_result_check,
            span,
        )
        .unwrap_or_else(|diagnostic| {
            ctx.diagnostics.push(diagnostic);
            Expr::Error { span }
        })
    }

    /// Builds a call through a `<callable>` value as `called`: arguments at
    /// its parameter kinds, and results the call site relies on as its result
    /// kinds, each checked as `check` says. A typed callable result is called
    /// as `called` declares; calls through it check what they rely on in
    /// turn.
    ///
    /// The call itself produces each result at its check's wire kind, and
    /// [`Self::check_values`] checks them. When no check can establish a
    /// result, this returns a [`DiagnosticCode::UncheckedCallableResult`]
    /// error.
    #[allow(clippy::too_many_arguments)]
    fn checked_callable_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        args: Vec<Expr>,
        arguments: CallArguments,
        called: CallableSignature,
        check: fn(&Self, ValueKind, Option<IntRange>) -> Option<CallableResultCheck>,
        span: Span,
    ) -> Result<Expr, Diagnostic> {
        let Some(checks) = called
            .results
            .iter()
            .enumerate()
            .map(|(index, kind)| check(self, *kind, called.result_range(index)))
            .collect::<Option<Vec<_>>>()
        else {
            return Err(
                Diagnostic::error(span, DiagnosticCode::UncheckedCallableResult).with_arg(
                    DiagnosticArg::text("expected", self.result_kinds_name(&called.results)),
                ),
            );
        };
        // Keyword arguments cross boxed: the value's entry checks each, and
        // fills the default of one passed as `$unsupplied`.
        let mut arg_kinds = called.params.clone();
        arg_kinds.extend(called.keywords.iter().map(|_| ValueKind::Any));
        let call = Expr::CallableCall {
            callable: Box::new(target),
            args,
            arguments,
            arg_kinds,
            keywords: called
                .keywords
                .iter()
                .map(|keyword| keyword.name.clone())
                .collect(),
            result_callable_sigs: called.result_sigs.clone(),
            result_kinds: checks.iter().map(CallableResultCheck::wire_kind).collect(),
            span,
        };
        Ok(self.check_values(
            ctx,
            call,
            &checks,
            &called.results,
            &called.result_sigs,
            "callable-result",
            span,
        ))
    }

    /// Checks each value `values` produces, at its check's wire kind, back
    /// to `kinds`: a single checked value is wrapped in its check, an
    /// ordinary site soft typing records and may later prove; several are
    /// bound to temporaries of a block that checks each. `sigs` are the
    /// values' callable signatures, and `tag` names the temporaries'
    /// definitions.
    #[expect(
        clippy::too_many_arguments,
        reason = "the values, their checks, and what they are checked back to"
    )]
    fn check_values(
        &self,
        ctx: &mut FunctionContext,
        values: Expr,
        checks: &[CallableResultCheck],
        kinds: &[ValueKind],
        sigs: &[Option<CallableSignature>],
        tag: &str,
        span: Span,
    ) -> Expr {
        if let [check] = checks {
            return check.apply(values, span);
        }
        if checks
            .iter()
            .all(|check| matches!(check, CallableResultCheck::Trusted(_)))
        {
            return values;
        }
        let sig = |index: usize| sigs.get(index).cloned().flatten();
        // Each value is checked on its own, so the values are bound to
        // temporaries no source name can reach.
        let scope = ctx.scope_start();
        let temporaries = checks
            .iter()
            .enumerate()
            .map(|(index, check)| {
                ctx.add_local(
                    DefinitionId::new(
                        self.source_file,
                        DefinitionKind::Local,
                        "$result",
                        &format!("{tag}:{}:{}:{index}", span.start, span.end),
                    ),
                    format!("$result{index}"),
                    check.wire_kind(),
                    sig(index),
                    span,
                )
            })
            .collect::<Vec<_>>();
        ctx.hide_locals_from(scope);
        let checked = temporaries
            .iter()
            .zip(checks)
            .enumerate()
            .map(|(index, (local, check))| {
                let value = Expr::Local {
                    local: *local,
                    result_kind: check.wire_kind(),
                    callable_signature: sig(index),
                    span,
                };
                check.apply(value, span)
            })
            .collect();
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: vec![Statement::Let(Let {
                    local: temporaries[0],
                    locals: temporaries,
                    value: values,
                    span,
                })],
                result: Expr::Values {
                    values: checked,
                    result_kinds: kinds.to_vec(),
                    span,
                },
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Wraps `value` in a check against `range` when it reaches a
    /// declaration of a `limited(<integer>, ...)` type of `kind`: an
    /// argument for a limited parameter, a `let` binding, a slot value, or a
    /// constant. A value that is not an integer has already been reported.
    fn limit_value(&self, value: Expr, kind: ValueKind, range: Option<IntRange>) -> Expr {
        let integer = NumericKind::of(self.expr_kind(&value)).is_some_and(NumericKind::is_integer);
        match range {
            Some(range) if kind == ValueKind::I64 && integer => {
                let span = value.span();
                Expr::CheckRange {
                    value: Box::new(value),
                    range,
                    site: SiteId::PENDING,
                    explicit: false,
                    span,
                }
            }
            _ => value,
        }
    }

    /// Checks each of `args` that reaches a limited parameter of `kinds`
    /// against its range; see [`Self::limit_value`].
    fn limit_args(
        &self,
        args: Vec<Expr>,
        kinds: &[ValueKind],
        ranges: &[Option<IntRange>],
    ) -> Vec<Expr> {
        args.into_iter()
            .enumerate()
            .map(|(index, arg)| {
                let kind = kinds.get(index).copied().unwrap_or(ValueKind::Error);
                self.limit_value(arg, kind, ranges.get(index).copied().flatten())
            })
            .collect()
    }

    /// Checks a definition's `body` result against the `ranges` that limit
    /// its `kinds`, so callers may assume them, and returns the result kinds
    /// it then has; see [`Self::check_values`].
    ///
    /// Any integer result is checked: an `<i32>` or `<u32>` whose own range
    /// is not within the limit widens to the `<integer>` the check yields,
    /// as a method's result may when its generic declares a limited one.
    /// Results whose kind is in error are left alone.
    fn check_result_ranges(
        &self,
        ctx: &mut FunctionContext,
        body: &mut Block,
        kinds: &[ValueKind],
        sigs: &[Option<CallableSignature>],
        ranges: &[Option<IntRange>],
    ) -> Vec<ValueKind> {
        if ranges.iter().all(Option::is_none) || result_kinds_contain_error(kinds) {
            return kinds.to_vec();
        }
        let checks = kinds
            .iter()
            .enumerate()
            .map(|(index, kind)| {
                let own = NumericKind::of(*kind)
                    .filter(|kind| kind.is_integer())
                    .map(integer_range_of);
                match (ranges.get(index).copied().flatten(), own) {
                    (Some(range), Some(own)) if !range.includes(own) => {
                        CallableResultCheck::Range(range)
                    }
                    _ => CallableResultCheck::Trusted(*kind),
                }
            })
            .collect::<Vec<_>>();
        if checks
            .iter()
            .all(|check| matches!(check, CallableResultCheck::Trusted(_)))
        {
            return kinds.to_vec();
        }
        let checked_kinds = checks
            .iter()
            .map(CallableResultCheck::wire_kind)
            .collect::<Vec<_>>();
        // A single limited result arrives as whatever integer the body
        // produced; several pass through temporaries of the body's kinds.
        let body_kinds = self.expr_kinds(&body.result);
        let checks = if let [check] = checks.as_slice() {
            vec![*check]
        } else {
            checks
                .iter()
                .zip(&body_kinds)
                .map(|(check, kind)| match check {
                    CallableResultCheck::Trusted(_) => CallableResultCheck::Trusted(*kind),
                    check => *check,
                })
                .collect()
        };
        let span = body.result.span();
        let result = core::mem::replace(&mut body.result, Expr::Error { span });
        body.result = self.check_values(
            ctx,
            result,
            &checks,
            &checked_kinds,
            sigs,
            "checked-result",
            span,
        );
        checked_kinds
    }

    /// Returns how one result of an untyped callable call reaches the call
    /// site's `kind`, or `None` when no runtime check can establish it.
    ///
    /// Unboxed numbers, booleans, `<object>` and callables are register
    /// types a typed entry returns at, and lowering unboxes them to that
    /// type from a boxed entry, checking the class. Every class shares the
    /// `object` register type, so a class result arrives as any object and
    /// is cast. Metaobject kinds name one class, generic, method or slot,
    /// which no cast can check.
    fn untyped_result_check(
        &self,
        kind: ValueKind,
        _range: Option<IntRange>,
    ) -> Option<CallableResultCheck> {
        Some(match kind {
            ValueKind::I32 | ValueKind::U32 => {
                CallableResultCheck::Convert(NumericKind::of(kind).unwrap_or(NumericKind::Integer))
            }
            ValueKind::Object(class)
            | ValueKind::String(class)
            | ValueKind::Symbol(class)
            | ValueKind::Vector(class)
                if class != OBJECT_CLASS_ID =>
            {
                CallableResultCheck::Cast(class)
            }
            ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. }
            | ValueKind::KnownCallable(_) => return None,
            kind => CallableResultCheck::Trusted(kind),
        })
    }

    /// Returns how one result of a call through a callable value of a
    /// declared signature reaches the signature's `kind`, or `None` when no
    /// runtime check can establish it.
    ///
    /// As [`Self::untyped_result_check`], except that a declared signature
    /// fixes a limited integer's register kind, which the typed entry
    /// returns at, and that a declared `limited(<integer>, ...)` result is
    /// checked against its range, as a class result is cast.
    fn signature_result_check(
        &self,
        kind: ValueKind,
        range: Option<IntRange>,
    ) -> Option<CallableResultCheck> {
        match (kind, range) {
            (ValueKind::I32 | ValueKind::U32, _) => Some(CallableResultCheck::Trusted(kind)),
            (ValueKind::I64, Some(range)) => Some(CallableResultCheck::Range(range)),
            (kind, _) => self.untyped_result_check(kind, None),
        }
    }

    fn generic_callable_arg_kind(&self, generic: GenericFunctionId, index: usize) -> ValueKind {
        let Some(generic) = self.generics.get(generic.0 as usize) else {
            return ValueKind::Error;
        };
        let boxed = self.generic_param_boxes_scalars(generic.id, index);
        match generic.param_kinds.get(index).copied().flatten() {
            Some(ValueKind::Any) if generic.dispatch_params.contains(&index) && !boxed => {
                ValueKind::Object(OBJECT_CLASS_ID)
            }
            Some(kind) => kind,
            None if generic.dispatch_params.contains(&index) && !boxed => {
                ValueKind::Object(OBJECT_CLASS_ID)
            }
            None => ValueKind::Any,
        }
    }

    /// Lowers `signal(condition)`.
    ///
    /// Handlers are found dynamically, so any statically known object may be
    /// signalled anywhere. A signal never returns: its result kinds are only a
    /// placeholder that lets it sit in any expression position. They come from
    /// the innermost lexical restart accepting the condition, then from the
    /// context, and are adopted from a sibling branch where one exists.
    fn lower_signal(
        &mut self,
        ctx: &mut FunctionContext,
        condition: &ast::Expr,
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let condition = self.lower_expr(ctx, condition);
        self.signal_lowered(ctx, condition, expected_results, span)
    }

    /// Signals `condition`, already lowered; see [`Self::lower_signal`].
    fn signal_lowered(
        &mut self,
        ctx: &mut FunctionContext,
        condition: Expr,
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let condition_class = match self.expr_kind(&condition) {
            ValueKind::Object(class) => class,
            ValueKind::Error => ClassId(u32::MAX),
            _ => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::SignalConditionTypeMismatch).with_arg(
                        DiagnosticArg::text("actual", self.kind_name(self.expr_kind(&condition))),
                    ),
                );
                ClassId(u32::MAX)
            }
        };
        let (result_kinds, result_callable_sigs) = ctx
            .matching_restart(condition_class, self)
            .map(|restart| (restart.result_kinds, restart.result_callable_sigs))
            .or_else(|| expected_results.map(|kinds| (kinds.to_vec(), vec![None; kinds.len()])))
            .unwrap_or_else(|| {
                let kinds = provisional_open_result_kinds();
                let sigs = vec![None; kinds.len()];
                (kinds, sigs)
            });
        Expr::Signal {
            condition: Box::new(condition),
            condition_class,
            result_kinds,
            result_callable_sigs,
            span,
        }
    }

    /// Lowers `string-to-integer(s)` and `string-to-integer(s, default:
    /// d)`, common Dylan's: the value of the optional sign and decimal
    /// digits `s` starts with, up to the first character that is not a
    /// digit. A string that starts with no integer answers `d`, or without
    /// one signals a `<simple-error>`. The string and the default are each
    /// evaluated once, in order; the result is an `<integer>` unless the
    /// default is not one, when it is an `<object>`.
    fn lower_string_to_integer(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        args: &[ast::Expr],
        keywords: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        const CALLEE: &str = "string-to-integer";
        let mut valid = true;
        for (index, keyword) in keywords.iter().enumerate() {
            if keyword.name != "default" {
                ctx.diagnostics
                    .push(Self::unexpected_keyword(CALLEE, keyword));
                valid = false;
            } else if keywords[..index]
                .iter()
                .any(|earlier| earlier.name == keyword.name)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(keyword.span, DiagnosticCode::DuplicateKeywordArgument)
                        .with_arg(DiagnosticArg::text("keyword", keyword.name.clone())),
                );
                valid = false;
            }
        }
        let [string] = args else {
            ctx.diagnostics
                .push(Self::arity_mismatch(CALLEE, 1, args.len(), span));
            return Expr::Error { span };
        };
        if !valid {
            return Expr::Error { span };
        }
        let scope = ctx.scope_start();
        let string_span = string.span();
        let string = self.lower_expr(ctx, string);
        let Some(string_class) = self.builtin_value_class(BuiltinValueClass::String) else {
            return Expr::Error { span };
        };
        let string = self.narrow(string, ValueKind::String(string_class), None);
        let name = format!("{CALLEE} string@{}", span.start);
        let bound = self.bind_hidden_local(ctx, id, &name, string, string_span);
        let read = Expr::Local {
            local: bound.local,
            result_kind: ctx.local_kind(bound.local),
            callable_signature: None,
            span: string_span,
        };
        let mut lets = vec![bound];
        let test = self.checked_intrinsic_call(
            ctx,
            Intrinsic::Text(TextFunction::IsIntegerText),
            CALLEE,
            vec![read.clone()],
            span,
        );
        let mut integer = self.checked_intrinsic_call(
            ctx,
            Intrinsic::Text(TextFunction::StringToInteger),
            CALLEE,
            vec![read],
            span,
        );
        let otherwise = match keywords.first() {
            Some(default) => {
                let value = self.lower_expr(ctx, &default.value);
                let default_name = format!("{CALLEE} default@{}", span.start);
                let default_span = default.value.span();
                let bound = self.bind_hidden_local(ctx, id, &default_name, value, default_span);
                let kind = ctx.local_kind(bound.local);
                lets.push(bound);
                let read = Expr::Local {
                    local: lets[1].local,
                    result_kind: kind,
                    callable_signature: None,
                    span: default_span,
                };
                if matches!(kind, ValueKind::I64 | ValueKind::I32 | ValueKind::U32) {
                    let from = NumericKind::of(kind).unwrap_or(NumericKind::Integer);
                    promote_operand(read, from, NumericKind::Integer)
                } else {
                    // An integer or the default, as an `<object>`.
                    integer = Expr::Cast {
                        site: SiteId::PENDING,
                        value: Box::new(integer),
                        class: OBJECT_CLASS_ID,
                        explicit: false,
                        span,
                    };
                    if kind == ValueKind::Any {
                        read
                    } else {
                        Expr::Cast {
                            site: SiteId::PENDING,
                            value: Box::new(read),
                            class: OBJECT_CLASS_ID,
                            explicit: false,
                            span: default_span,
                        }
                    }
                }
            }
            None => {
                let error_args = [
                    ast::Expr::String {
                        id,
                        value: "string-to-integer: %= does not start with an integer".into(),
                        span,
                    },
                    ast::Expr::Name {
                        id,
                        name: name.clone(),
                        span: string_span,
                    },
                ];
                self.lower_error(ctx, id, &error_args, Some(&[ValueKind::I64]), span)
            }
        };
        ctx.hide_locals_from(scope);
        let call = Expr::If {
            condition: Box::new(test),
            refinement: None,
            then_branch: Box::new(integer),
            else_branch: Box::new(otherwise),
            span,
        };
        with_lets(lets, call, span)
    }

    /// Lowers Dylan's `concatenate(sequence, more, …)`: a new collection of
    /// the first sequence's kind holding the elements of each in turn.
    /// Strings and characters typing knows join inline, two at a time;
    /// any other sequences join through the kernel library's
    /// `concatenate-two`. When the program's own methods join the built-in
    /// `concatenate` generic, a call of two arguments that may be of one of
    /// its classes, an `<object>` among them, sends to the generic, as it
    /// did before sequences joined.
    fn lower_concatenate(
        &mut self,
        ctx: &mut FunctionContext,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        const CALLEE: &str = "concatenate";
        if args.is_empty() {
            ctx.diagnostics
                .push(Self::arity_mismatch(CALLEE, 1, 0, span));
            return Expr::Error { span };
        }
        let text = |kind: ValueKind| {
            matches!(
                kind,
                ValueKind::String(_) | ValueKind::Char | ValueKind::Error
            )
        };
        if args.len() >= 2 && args.iter().all(|arg| text(self.expr_kind(arg))) {
            let mut args = args.into_iter();
            let mut joined = args.next().expect("there are arguments");
            for next in args {
                joined = self
                    .lower_intrinsic_call(ctx, CALLEE, &[joined, next], span)
                    .unwrap_or(Expr::Error { span });
            }
            return joined;
        }
        if args.len() == 2
            && !self.kernel_scope
            && self.program_method_names.iter().any(|name| name == CALLEE)
            && self.sends_to_built_in_generic(CALLEE, &args)
            && let Some(generic) = self.built_in_generic_named(CALLEE)
            && let Some(symbol) = self
                .generic_symbols
                .iter()
                .find(|symbol| symbol.id == generic)
                .cloned()
        {
            return self.lower_generic_send(ctx, &symbol, args, CALLEE, span);
        }
        let Some(two) = self.kernel_library_function("concatenate-two") else {
            return Expr::Error { span };
        };
        let mut args = args.into_iter();
        let first = args.next().expect("there are arguments");
        let mut rest = args.collect::<Vec<_>>();
        // One sequence is copied, as joining it to nothing.
        if rest.is_empty() {
            rest.push(Expr::IntrinsicCall {
                intrinsic: Intrinsic::EmptyList,
                args: Vec::new(),
                result_kind: self.intrinsic_result_kind(Intrinsic::EmptyList),
                span,
            });
        }
        let mut joined = first;
        for next in rest {
            joined = self.lower_function_call(ctx, &two, vec![joined, next], CALLEE, span);
        }
        joined
    }

    /// A new list of `elements`, evaluated in order: `#()` for none, and
    /// otherwise a `<pair>` of the first and a list of the rest. A list
    /// literal `#(a, b)` and `list(a, b)` both make one.
    fn lower_list(&self, ctx: &mut FunctionContext, elements: Vec<Expr>, span: Span) -> Expr {
        let mut list = Expr::IntrinsicCall {
            intrinsic: Intrinsic::EmptyList,
            args: Vec::new(),
            result_kind: self.intrinsic_result_kind(Intrinsic::EmptyList),
            span,
        };
        for element in elements.into_iter().rev() {
            list = self.checked_intrinsic_call(
                ctx,
                Intrinsic::ListPair,
                "list",
                vec![element, list],
                span,
            );
        }
        list
    }

    /// Lowers Dylan's `error`: `error(condition)` signals the condition,
    /// and `error(format-string, argument, …)` signals a `<simple-error>`
    /// holding the string and a vector of the arguments. Either way it
    /// does not return unless a handler supplies its values, so like
    /// `signal` it takes the result kinds its context expects.
    fn lower_error(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        args: &[ast::Expr],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let Some((first, arguments)) = args.split_first() else {
            ctx.diagnostics
                .push(Self::arity_mismatch("error", 1, 0, span));
            return Expr::Error { span };
        };
        let first_value = self.lower_expr(ctx, first);
        if self.expr_kind(&first_value) == ValueKind::Any {
            return self.lower_untyped_error(
                ctx,
                id,
                first,
                first_value,
                arguments,
                expected_results,
                span,
            );
        }
        if !matches!(self.expr_kind(&first_value), ValueKind::String(_)) {
            if !arguments.is_empty() {
                for argument in arguments {
                    let _ = self.lower_expr(ctx, argument);
                }
                ctx.diagnostics
                    .push(Self::arity_mismatch("error", 1, args.len(), span));
                return Expr::Error { span };
            }
            return self.signal_lowered(ctx, first_value, expected_results, span);
        }
        // The format string, evaluated first, is read back from a local
        // whose name source cannot spell.
        let scope = ctx.scope_start();
        let name = format!("error format-string@{}", span.start);
        let bound = self.bind_hidden_local(ctx, id, &name, first_value, first.span());
        let condition = ast::Expr::MakeObject {
            id,
            class: Box::new(ast::Expr::ClassName {
                id,
                class_name: ConditionClass::SimpleError.name().into(),
                span,
            }),
            fields: vec![
                ast::KeywordArg {
                    id,
                    name: "format-string".into(),
                    value: ast::Expr::Name {
                        id,
                        name,
                        span: first.span(),
                    },
                    span,
                },
                ast::KeywordArg {
                    id,
                    name: "format-arguments".into(),
                    value: ast::Expr::Vector {
                        id,
                        elements: arguments.to_vec(),
                        span,
                    },
                    span,
                },
            ],
            span,
        };
        let condition = self.lower_expr(ctx, &condition);
        let signal = self.signal_lowered(ctx, condition, expected_results, span);
        ctx.hide_locals_from(scope);
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: vec![Statement::Let(bound)],
                result: signal,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Lowers `error(x, argument, …)` for an `x` typing does not know: a
    /// string signals a `<simple-error>` of it and the arguments, and
    /// anything else is signalled as the condition it must be (checked to
    /// be a `<condition>`, as a declaration would check it), the arguments
    /// left unevaluated.
    #[allow(clippy::too_many_arguments)]
    fn lower_untyped_error(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        first: &ast::Expr,
        first_value: Expr,
        arguments: &[ast::Expr],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let scope = ctx.scope_start();
        let name = format!("error value@{}", span.start);
        let bound = self.bind_hidden_local(ctx, id, &name, first_value, first.span());
        let value = || ast::Expr::Name {
            id,
            name: name.clone(),
            span: first.span(),
        };
        let class = |class_name: &str| ast::Expr::ClassName {
            id,
            class_name: class_name.into(),
            span,
        };
        let cast = |class_name: &str| ast::Expr::Cast {
            id,
            target: ast::TypeExpr::Named {
                name: class_name.into(),
                span,
            },
            value: Box::new(value()),
            span,
        };
        let simple_error = ast::Expr::Signal {
            id,
            condition: Box::new(ast::Expr::MakeObject {
                id,
                class: Box::new(class(ConditionClass::SimpleError.name())),
                fields: vec![
                    ast::KeywordArg {
                        id,
                        name: "format-string".into(),
                        value: cast("<string>"),
                        span,
                    },
                    ast::KeywordArg {
                        id,
                        name: "format-arguments".into(),
                        value: ast::Expr::Vector {
                            id,
                            elements: arguments.to_vec(),
                            span,
                        },
                        span,
                    },
                ],
                span,
            }),
            span,
        };
        let condition = ast::Expr::Signal {
            id,
            condition: Box::new(cast(ConditionClass::Condition.name())),
            span,
        };
        // The test is the kernel's, whatever the program calls `instance?`.
        let Some(string) = self.builtin_value_class(BuiltinValueClass::String) else {
            return Expr::Error { span };
        };
        let test = Expr::InstanceOf {
            site: SiteId::PENDING,
            value: Box::new(Expr::Local {
                local: bound.local,
                result_kind: ValueKind::Any,
                callable_signature: None,
                span,
            }),
            class: string,
            conversion: false,
            span,
        };
        let refinement = self.true_branch_refinement(&test);
        let simple_error = self.lower_expr_expected_results(ctx, &simple_error, expected_results);
        let condition = self.lower_expr_expected_results(ctx, &condition, expected_results);
        let signal = Expr::If {
            condition: Box::new(test),
            refinement,
            then_branch: Box::new(simple_error),
            else_branch: Box::new(condition),
            span,
        };
        ctx.hide_locals_from(scope);
        with_lets(vec![bound], signal, span)
    }

    /// Lets a diverging `signal` take the result kinds of its sibling.
    ///
    /// A signal never produces a value, so this only removes a spurious
    /// mismatch between, say, an `if` branch that signals and one that
    /// returns a float.
    /// Widens `if` branches of different integer types to `<integer>`, the
    /// type that holds both, so neither branch is truncated or rejected.
    fn join_integer_branches(&self, lhs: &mut Expr, rhs: &mut Expr) {
        let (Some(lhs_kind), Some(rhs_kind)) = (
            NumericKind::of(self.expr_kind(lhs)),
            NumericKind::of(self.expr_kind(rhs)),
        ) else {
            return;
        };
        if lhs_kind == rhs_kind || !lhs_kind.is_integer() || !rhs_kind.is_integer() {
            return;
        }
        for (branch, kind) in [(lhs, lhs_kind), (rhs, rhs_kind)] {
            let expr = core::mem::replace(
                branch,
                Expr::Error {
                    span: branch.span(),
                },
            );
            *branch = promote_operand(expr, kind, NumericKind::Integer);
        }
    }

    /// Lowers the arms of an `if` or `case` to nested [`Expr::If`]s.
    ///
    /// Each arm's test guards its body, and the rest of the chain is the
    /// false branch, so a later test and body see everything the earlier
    /// tests ruled out, exactly as nested `if`s do. The chain's value is the
    /// join of its bodies. Without a default body (`else` or `otherwise`) the
    /// chain produces no values: each arm's values are discarded, and using
    /// the chain where a value is expected is a type error there. Where no
    /// values are expected, such as in a statement, each arm's are discarded
    /// too, so arms that would not join are no error.
    #[expect(
        clippy::too_many_arguments,
        reason = "a conditional's written parts, plus how a select matches its keys"
    )]
    fn lower_conditional(
        &mut self,
        ctx: &mut FunctionContext,
        form: Conditional,
        arms: &[ArmRef<'_>],
        default: Option<&ast::Block>,
        span: Span,
        expected_results: Option<&[ValueKind]>,
        matcher: Option<&SelectMatcher>,
    ) -> Expr {
        // Where no values are expected, each arm's are discarded, so the
        // arms need not agree, as in Dylan.
        let discarded = expected_results.is_some_and(<[ValueKind]>::is_empty);
        let Some((arm, rest)) = arms.split_first() else {
            return match default {
                Some(body) if discarded => {
                    let body = self.lower_body_expr(ctx, body, effect_expectation(&body.result));
                    discard_values(self, body)
                }
                Some(body) => self.lower_body_expr(ctx, body, expected_results),
                None => no_values(Span::new(span.end, span.end)),
            };
        };
        // An arm whose values are discarded is lowered as a statement is.
        let arm_expected = if discarded {
            effect_expectation(&arm.body.result)
        } else if default.is_some() {
            expected_results
        } else {
            None
        };
        let condition = match (arm.test, matcher) {
            (ArmTest::Keys(keys), Some(matcher)) => {
                self.lower_key_tests(ctx, matcher, keys, arm.span)
            }
            (ArmTest::Expr(test), _) => {
                let condition = self.lower_expr(ctx, test);
                self.check_test_is_boolean(ctx, form.test_keyword(), &condition);
                condition
            }
            (ArmTest::Keys(_), None) => Expr::Error { span: arm.span },
        };
        // A body that assigns the local may store any value of its declared
        // type, so the test no longer describes it there.
        let refinement = self
            .true_branch_refinement(&condition)
            .filter(|refinement| {
                // A shared local may change in any call that runs a closure,
                // so no test describes it for a whole arm.
                ctx.locals
                    .get(refinement.local.0 as usize)
                    .is_none_or(|local| !local.shared && !arm.body.assigns(&local.name))
            });
        let previous_kind =
            refinement.and_then(|refinement| ctx.refine_local(refinement.local, refinement.kind));
        let mut then_branch = self.lower_body_expr(ctx, arm.body, arm_expected);
        if let (Some(refinement), Some(previous_kind)) = (refinement, previous_kind) {
            let _ = ctx.refine_local(refinement.local, previous_kind);
        }
        let rest_start = rest.first().map_or(arm.span.end, |next| next.span.start);
        let mut else_branch = self.lower_conditional(
            ctx,
            form.rest(),
            rest,
            default,
            Span::new(rest_start, span.end),
            expected_results,
            matcher,
        );
        if default.is_none() || discarded {
            then_branch = discard_values(self, then_branch);
        }
        self.adopt_divergent_kinds(&mut then_branch, &else_branch);
        self.adopt_divergent_kinds(&mut else_branch, &then_branch);
        // Arms of different counts keep as many values as every arm has,
        // or as many as are expected, as a single-value position keeps the
        // first of several.
        let then_count = self.expr_kinds(&then_branch).len();
        let else_count = self.expr_kinds(&else_branch).len();
        if then_count != else_count && then_count > 0 && else_count > 0 {
            // A body whose values are discarded keeps them all until then.
            let count = expected_results
                .map(<[ValueKind]>::len)
                .filter(|expected| *expected > 0)
                .map_or_else(
                    || then_count.min(else_count),
                    |expected| expected.min(then_count).min(else_count),
                );
            then_branch = self.take_values(ctx, then_branch, count);
            else_branch = self.take_values(ctx, else_branch, count);
        }
        self.join_integer_branches(&mut then_branch, &mut else_branch);
        let then_kinds = self.expr_kinds(&then_branch);
        let else_kinds = self.expr_kinds(&else_branch);
        if self.join_arm_kinds(&then_kinds, &else_kinds).is_none()
            && !result_kinds_contain_error(&then_kinds)
            && !result_kinds_contain_error(&else_kinds)
        {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::IfBranchTypeMismatch)
                    .with_arg(DiagnosticArg::text("form", form.construct()))
                    .with_arg(DiagnosticArg::text(
                        "then",
                        self.result_kinds_name(&then_kinds),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "else",
                        self.result_kinds_name(&else_kinds),
                    )),
            );
        }
        Expr::If {
            condition: Box::new(condition),
            refinement,
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
            span,
        }
    }

    /// Lowers `select (target by test) key, key => body; … end` as a
    /// conditional whose arm tests match the target against each key in turn.
    ///
    /// The target is evaluated once, before any key. A local or a literal is
    /// read where it is, so each arm narrows a local as `if (x == key)`
    /// would; anything else is bound to a hidden local first, as is a `by`
    /// test that is not a name. A key matches when `target == key`, or
    /// `test(target, key)` with `by`; integer keys against an integer target
    /// compare inline. Without an `otherwise`, the default body signals a
    /// `<select-error>` holding the target, so the arms keep their values and
    /// a handler or restart can supply one.
    ///
    /// Reading a local target again for each key relies on locals being
    /// immutable until the arms run: only a key could change it.
    #[expect(
        clippy::too_many_arguments,
        reason = "the written parts of a select, threaded as the other conditionals are"
    )]
    fn lower_select(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        target: &ast::Expr,
        test: Option<&ast::Expr>,
        arms: &[ast::SelectArm],
        otherwise: Option<&ast::Block>,
        span: Span,
        expected_results: Option<&[ValueKind]>,
    ) -> Expr {
        let scope_start = ctx.scope_start();
        let mut lets = Vec::new();
        // Reading a literal again cannot be observed, nor a local that the
        // test and keys, which run between the reads, do not assign. A
        // shared local may change in any call that runs a closure.
        let shared = ctx.shared_names.clone();
        let reassigned = |name: &str| {
            shared.iter().any(|shared| shared == name)
                || test.is_some_and(|test| test.assigns(name))
                || arms
                    .iter()
                    .any(|arm| arm.keys.iter().any(|key| key.assigns(name)))
        };
        let target_ref = if matches!(target, ast::Expr::Name { name, .. } if !reassigned(name))
            || matches!(
                target,
                ast::Expr::Number { .. }
                    | ast::Expr::Float { .. }
                    | ast::Expr::Number32 { .. }
                    | ast::Expr::Bool { .. }
                    | ast::Expr::Symbol { .. }
                    | ast::Expr::ClassName { .. }
            ) {
            target.clone()
        } else {
            let value = self.lower_expr_expected_results(ctx, target, None);
            self.require_scalar_result(ctx, &value, target.span(), "select target");
            if self.expr_kinds(&value).len() != 1 {
                return Expr::Error { span };
            }
            let name = format!("{SELECT_TARGET_LOCAL}@{}", span.start);
            lets.push(self.bind_hidden_local(ctx, id, &name, value, target.span()));
            ast::Expr::Name {
                id,
                name,
                span: target.span(),
            }
        };
        let read = self.lower_expr(ctx, &target_ref);
        let target_kind = self.expr_kind(&read);
        if target_kind == ValueKind::Error {
            ctx.hide_locals_from(scope_start);
            return Expr::Error { span };
        }
        let callee = match test {
            None => None,
            Some(test) => {
                let value = self.lower_expr_expected_results(ctx, test, None);
                if !self.check_select_test(ctx, test.span(), &value) {
                    ctx.hide_locals_from(scope_start);
                    return Expr::Error { span };
                }
                match test {
                    // An operator's name, `\=`, is no name a call can
                    // write, so its value is bound as any other test's.
                    ast::Expr::Name { name, .. } if !reassigned(name) && syntax::is_name(name) => {
                        Some(name.clone())
                    }
                    test => {
                        let name = format!("{SELECT_TEST_LOCAL}@{}", span.start);
                        lets.push(self.bind_hidden_local(ctx, id, &name, value, test.span()));
                        Some(name)
                    }
                }
            }
        };
        let matcher = SelectMatcher {
            id,
            target: target_ref.clone(),
            callee,
        };
        let no_match;
        let default = match otherwise {
            Some(otherwise) => otherwise,
            None => {
                let end = Span::new(span.end, span.end);
                no_match = ast::Block {
                    id,
                    statements: Vec::new(),
                    result: ast::Expr::Signal {
                        id,
                        condition: Box::new(ast::Expr::MakeObject {
                            id,
                            class: Box::new(ast::Expr::ClassName {
                                id,
                                class_name: ConditionClass::SelectError.name().into(),
                                span: end,
                            }),
                            fields: vec![ast::KeywordArg {
                                id,
                                name: "value".into(),
                                value: target_ref,
                                span: end,
                            }],
                            span: end,
                        }),
                        span: end,
                    },
                    span: end,
                };
                &no_match
            }
        };
        let arms = arms
            .iter()
            .map(|arm| ArmRef {
                test: ArmTest::Keys(&arm.keys),
                body: &arm.body,
                span: arm.span,
            })
            .collect::<Vec<_>>();
        let chain = self.lower_conditional(
            ctx,
            Conditional::Select,
            &arms,
            Some(default),
            span,
            expected_results,
            Some(&matcher),
        );
        ctx.hide_locals_from(scope_start);
        if lets.is_empty() {
            return chain;
        }
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: lets.into_iter().map(Statement::Let).collect(),
                result: chain,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Reports a `select`'s `by` test, lowered to `test`, unless it is a
    /// function of the target and a key that answers a `<boolean>`, as far
    /// as its signature says. Returns whether the arms can call it.
    fn check_select_test(&self, ctx: &mut FunctionContext, span: Span, test: &Expr) -> bool {
        let kinds = self.expr_kinds(test);
        if result_kinds_contain_error(&kinds) {
            return false;
        }
        let actual = match kinds.as_slice() {
            [ValueKind::GenericFunctionObject(_)] => return true,
            [kind] if self.is_callable_kind(*kind) => match self.expr_callable_signature(test) {
                Some(signature)
                    if signature.params.len() != 2
                        || !matches!(signature.results.as_slice(), [ValueKind::Bool]) =>
                {
                    self.callable_signature_name(&signature)
                }
                _ => return true,
            },
            [kind] => self.kind_name(*kind),
            kinds => self.result_kinds_name(kinds),
        };
        ctx.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::SelectTestTypeMismatch)
                .with_arg(DiagnosticArg::text("actual", actual)),
        );
        false
    }

    /// Binds `value` to the hidden local `name`, which source cannot name.
    fn bind_hidden_local(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        name: &str,
        value: Expr,
        span: Span,
    ) -> Let {
        let definition = DefinitionId::new(
            self.source_file,
            DefinitionKind::Local,
            name,
            &id.raw().to_string(),
        );
        let local = ctx.add_local(
            definition,
            name.into(),
            self.expr_kind(&value),
            self.expr_callable_signature(&value),
            span,
        );
        Let {
            local,
            locals: vec![local],
            value,
            span,
        }
    }

    /// Lowers a `select` arm's keys to its test: whether any key matches the
    /// target, tried in order. Each key's own test must be a `<boolean>`.
    fn lower_key_tests(
        &mut self,
        ctx: &mut FunctionContext,
        matcher: &SelectMatcher,
        keys: &[ast::Expr],
        span: Span,
    ) -> Expr {
        let mut tests = Vec::with_capacity(keys.len());
        for key in keys {
            let test = match &matcher.callee {
                None => ast::Expr::Identical {
                    id: matcher.id,
                    lhs: Box::new(matcher.target.clone()),
                    rhs: Box::new(key.clone()),
                    span: key.span(),
                },
                Some(callee) => ast::Expr::Call {
                    id: matcher.id,
                    callee: callee.clone(),
                    args: vec![matcher.target.clone(), key.clone()],
                    keywords: Vec::new(),
                    span: key.span(),
                },
            };
            let test = self.lower_expr(ctx, &test);
            self.check_test_is_boolean(ctx, Conditional::Select.test_keyword(), &test);
            tests.push(test);
        }
        // `k1, k2 => body` runs its body when either key matches.
        let mut tests = tests.into_iter().rev();
        let Some(last) = tests.next() else {
            return Expr::Error { span };
        };
        tests.fold(last, |rest, test| Expr::If {
            span: Span::new(test.span().start, rest.span().end),
            refinement: None,
            then_branch: Box::new(Expr::ConstBool {
                value: true,
                span: test.span(),
            }),
            condition: Box::new(test),
            else_branch: Box::new(rest),
        })
    }

    /// Reports a conditional test, written after `keyword`, whose value is
    /// not a `<boolean>`.
    fn check_test_is_boolean(&self, ctx: &mut FunctionContext, keyword: &str, test: &Expr) {
        let kind = self.expr_kind(test);
        if !matches!(kind, ValueKind::Bool | ValueKind::Error) {
            ctx.diagnostics.push(
                Diagnostic::error(test.span(), DiagnosticCode::IfConditionTypeMismatch)
                    .with_arg(DiagnosticArg::text("form", keyword))
                    .with_arg(DiagnosticArg::text("expected", "<boolean>"))
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(kind))),
            );
        }
    }

    /// Lowers a conditional arm's body in its own scope: its `let`s end with
    /// it. A body without `let`s is just its result expression.
    fn lower_body_expr(
        &mut self,
        ctx: &mut FunctionContext,
        body: &ast::Block,
        expected_results: Option<&[ValueKind]>,
    ) -> Expr {
        let scope_start = ctx.scope_start();
        let block = self.lower_block(ctx, body, expected_results);
        ctx.hide_locals_from(scope_start);
        if block.statements.is_empty() {
            return block.result;
        }
        Expr::Block {
            exit: None,
            span: block.span,
            body: Box::new(block),
            cleanup: None,
        }
    }

    fn adopt_divergent_kinds(&self, expr: &mut Expr, sibling: &Expr) {
        if matches!(sibling, Expr::Signal { .. }) {
            return;
        }
        let kinds = self.expr_kinds(sibling);
        if result_kinds_contain_error(&kinds) {
            return;
        }
        let sigs = self.expr_result_callable_signatures(sibling);
        let mut expr = expr;
        // A body with `let`s ends in the expression that may diverge.
        while let Expr::Block {
            exit: None,
            cleanup: None,
            body,
            ..
        } = expr
        {
            expr = &mut body.result;
        }
        if let Expr::Signal {
            result_kinds,
            result_callable_sigs,
            ..
        } = expr
        {
            *result_kinds = kinds;
            *result_callable_sigs = sigs;
        }
    }

    fn lower_handle(
        &mut self,
        ctx: &mut FunctionContext,
        body: &ast::Expr,
        restart: &ast::Restart,
        span: Span,
        expected_results: Option<&[ValueKind]>,
    ) -> Expr {
        let specializer = self
            .resolve_class(&restart.class_name)
            .map(|class| class.id)
            .unwrap_or_else(|| {
                // Lowering to the AST reported a restart it found no class for.
                if !restart.class_name.is_empty() {
                    ctx.diagnostics.push(
                        Diagnostic::error(restart.span, DiagnosticCode::UnresolvedClass)
                            .with_arg(DiagnosticArg::text("class", restart.class_name.clone())),
                    );
                }
                ClassId(u32::MAX)
            });

        let guard = restart
            .guard
            .as_ref()
            .map(|guard| Box::new(self.lower_restart_guard(ctx, restart, guard)));
        let scope_start = ctx.scope_start();
        let condition_local = restart.condition_name.as_ref().map(|name| {
            if ctx.find_local(name).is_some() {
                ctx.diagnostics.push(
                    Diagnostic::error(restart.condition_span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", name.clone())),
                );
            }
            ctx.add_local(
                self.restart_condition_definition(restart, name),
                name.clone(),
                ValueKind::Object(specializer),
                None,
                restart.condition_span,
            )
        });
        // Where fewer values are expected than an arm has, it keeps the
        // first, as a single-value position does.
        let expected_count = expected_results.map_or(0, <[ValueKind]>::len);
        // Where no values are expected, each arm runs for its effects: it is
        // lowered as a statement would be and its values are discarded, so
        // the arms need not agree.
        let discarded = expected_results.is_some_and(<[ValueKind]>::is_empty);
        let restart_expected = if discarded { None } else { expected_results };
        let restart_body = self.lower_expr_expected_results(ctx, &restart.body, restart_expected);
        let restart_body = self.take_values(ctx, restart_body, expected_count);
        // A `signal` in the body takes the restart's own values, whether or
        // not the handle discards them.
        let signal_kinds = self.expr_kinds(&restart_body);
        let signal_callable_sigs = self.expr_result_callable_signatures(&restart_body);
        let restart_body = if discarded {
            discard_values(self, restart_body)
        } else {
            restart_body
        };
        let restart_kinds = self.expr_kinds(&restart_body);
        let restart_callable_sigs = self.expr_result_callable_signatures(&restart_body);
        ctx.hide_locals_from(scope_start);

        ctx.push_restart(ActiveRestart {
            specializer,
            result_kinds: signal_kinds,
            result_callable_sigs: signal_callable_sigs,
        });
        let mut body = if discarded {
            let body = self.lower_expr_expected_results(ctx, body, effect_expectation(body));
            discard_values(self, body)
        } else {
            let body = self.lower_expr_expected_results(ctx, body, expected_results);
            self.take_values(ctx, body, expected_count)
        };
        ctx.pop_restart();
        self.adopt_divergent_kinds(&mut body, &restart_body);

        let body_kinds = self.expr_kinds(&body);
        let joined = self.join_arm_kinds(&body_kinds, &restart_kinds);
        // Both sides already fixed the restart's result kinds, so integer
        // types must agree exactly here; nothing widens one to the other.
        if (joined.is_none() || integer_types_differ(&body_kinds, &restart_kinds))
            && !result_kinds_contain_error(&body_kinds)
            && !result_kinds_contain_error(&restart_kinds)
        {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::HandleRestartResultMismatch)
                    .with_arg(DiagnosticArg::text(
                        "body",
                        self.result_kinds_name(&body_kinds),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "restart",
                        self.result_kinds_name(&restart_kinds),
                    )),
            );
        }

        // The handle produces whichever arm runs, so its results are the
        // join; the handler's callable signatures describe them only while
        // the join is the handler's own kinds.
        let result_kinds = joined.unwrap_or_else(|| restart_kinds.clone());
        let result_callable_sigs = if result_kinds == restart_kinds {
            restart_callable_sigs
        } else {
            vec![None; result_kinds.len()]
        };
        Expr::Handle {
            body: Box::new(body),
            restart: Restart {
                specializer,
                condition_local,
                guard,
                body: Box::new(restart_body),
                result_kinds,
                result_callable_sigs,
                span: restart.span,
            },
            span,
        }
    }

    /// Lowers a `block` expression. Locals of the body and of the cleanup
    /// are scoped to their own bodies; the exit procedure is visible in the
    /// body only, because its binding has ended before the cleanup runs.
    ///
    /// The exit's parameter kinds are the block's result kinds. They come
    /// from the context, else the provisional open-result kinds, so a block
    /// producing other values needs a typed context, such as an annotated
    /// `let`.
    fn lower_block_expr(
        &mut self,
        ctx: &mut FunctionContext,
        exit: Option<&ast::BlockExit>,
        body: &ast::Block,
        cleanup: Option<&ast::Block>,
        span: Span,
        expected_results: Option<&[ValueKind]>,
    ) -> Expr {
        let scope_start = ctx.scope_start();
        let exit = exit.map(|exit| {
            let result_kinds =
                expected_results.map_or_else(provisional_open_result_kinds, <[ValueKind]>::to_vec);
            if ctx.find_local(&exit.name).is_some() {
                ctx.diagnostics.push(
                    Diagnostic::error(exit.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", exit.name.clone())),
                );
            }
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Local,
                &exit.name,
                &exit.id.raw().to_string(),
            );
            ctx.exit_definitions.push(definition);
            let local = ctx.add_local(
                definition,
                exit.name.clone(),
                ValueKind::Callable,
                Some(CallableSignature::flat(
                    result_kinds.clone(),
                    result_kinds.clone(),
                )),
                exit.span,
            );
            BlockExit {
                local,
                result_kinds,
            }
        });
        let body_expected = exit
            .as_ref()
            .map_or(expected_results, |exit| Some(exit.result_kinds.as_slice()));
        let mut body = self.lower_block(ctx, body, body_expected);
        ctx.hide_locals_from(scope_start);
        if let Some(exit) = &exit {
            // A limited integer body widens to an `<integer>` exit.
            if let ([ValueKind::I64], [narrow @ (ValueKind::I32 | ValueKind::U32)]) = (
                exit.result_kinds.as_slice(),
                self.expr_kinds(&body.result).as_slice(),
            ) && let Some(from) = NumericKind::of(*narrow)
            {
                let result = core::mem::replace(&mut body.result, Expr::Error { span });
                body.result = promote_operand(result, from, NumericKind::Integer);
            }
            // The block produces the body's values or the exit's, so the two
            // must join; the block's result kinds are that join.
            let body_kinds = self.expr_kinds(&body.result);
            if self
                .join_arm_kinds(&body_kinds, &exit.result_kinds)
                .is_none()
                && !result_kinds_contain_error(&body_kinds)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::BlockExitResultMismatch)
                        .with_arg(DiagnosticArg::text(
                            "body",
                            self.result_kinds_name(&body_kinds),
                        ))
                        .with_arg(DiagnosticArg::text(
                            "exit",
                            self.result_kinds_name(&exit.result_kinds),
                        )),
                );
            }
        }
        let cleanup = cleanup.map(|cleanup| {
            let scope_start = ctx.scope_start();
            let cleanup = self.lower_block(ctx, cleanup, None);
            ctx.hide_locals_from(scope_start);
            Box::new(cleanup)
        });
        Expr::Block {
            exit,
            body: Box::new(body),
            cleanup,
            span,
        }
    }

    /// Lowers a restart guard to a closure over the handle's locals taking
    /// the condition, typed by the clause's class, and returning `<boolean>`.
    fn lower_restart_guard(
        &mut self,
        ctx: &mut FunctionContext,
        restart: &ast::Restart,
        guard: &ast::Expr,
    ) -> Expr {
        let span = guard.span();
        let param = ast::FunctionParam {
            id: restart.id,
            name: restart
                .condition_name
                .clone()
                .unwrap_or_else(|| "$condition".into()),
            type_annotation: Some(ast::TypeExpr::Named {
                name: restart.class_name.clone(),
                span: restart.condition_span,
            }),
            span: restart.condition_span,
        };
        let body = ast::Block {
            id: guard.id(),
            statements: Vec::new(),
            result: guard.clone(),
            span,
        };
        let closure = self.lower_closure_expr(
            ctx,
            core::slice::from_ref(&param),
            None,
            None,
            &body,
            span,
            None,
        );
        if let Expr::Closure { closure, .. } = &closure
            && let Some(closure) = self.closures.get_mut(closure.0 as usize)
        {
            closure.restart_guard = true;
        }
        let results = match &closure {
            Expr::Closure {
                callable_signature, ..
            } => callable_signature.results.clone(),
            _ => Vec::new(),
        };
        if !matches!(results.as_slice(), [ValueKind::Bool] | [ValueKind::Error]) {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::RestartGuardTypeMismatch).with_arg(
                    DiagnosticArg::text("actual", self.result_kinds_name(&results)),
                ),
            );
        }
        closure
    }

    /// Lowers the operands of a numeric operator.
    ///
    /// The left operand keeps the legacy integer result hint used by
    /// open-result forms such as `apply` and `signal`; the right operand is
    /// hinted with the left operand's numeric kind, so `x * apply(f, v)`
    /// with a float `x` expects a float result.
    fn lower_numeric_operands(
        &mut self,
        ctx: &mut FunctionContext,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
    ) -> (Expr, Expr) {
        let lhs = self.lower_expr_expected(ctx, lhs, Some(ValueKind::I64));
        let hint =
            NumericKind::of(self.expr_kind(&lhs)).map_or(ValueKind::I64, NumericKind::value_kind);
        let rhs = self.lower_expr_expected(ctx, rhs, Some(hint));
        // A literal on the left adopts a 32-bit right operand's type, as one
        // on the right already did through the hint: `1 + x` like `x + 1`.
        let lhs = adapt_literal(lhs, Some(&[self.expr_kind(&rhs)]));
        (lhs, rhs)
    }

    /// Returns the numeric kinds of two operands.
    ///
    /// Reports `code` and returns `None` when either is not a number; callers
    /// then produce an error expression so the mismatch is reported once
    /// rather than cascading into result-type diagnostics. An erroneous
    /// operand takes the other operand's kind.
    fn numeric_operand_kinds(
        &self,
        ctx: &mut FunctionContext,
        span: Span,
        lhs: &Expr,
        rhs: &Expr,
        code: DiagnosticCode,
    ) -> Option<(NumericKind, NumericKind)> {
        let lhs_kind = self.expr_kind(lhs);
        let rhs_kind = self.expr_kind(rhs);
        match (NumericKind::of(lhs_kind), NumericKind::of(rhs_kind)) {
            (Some(lhs), Some(rhs)) => return Some((lhs, rhs)),
            (Some(kind), None) if rhs_kind == ValueKind::Error => return Some((kind, kind)),
            (None, Some(kind)) if lhs_kind == ValueKind::Error => return Some((kind, kind)),
            (None, None) if lhs_kind == ValueKind::Error && rhs_kind == ValueKind::Error => {
                return Some((NumericKind::Integer, NumericKind::Integer));
            }
            _ => {}
        }
        ctx.diagnostics.push(
            Diagnostic::error(span, code)
                .with_arg(DiagnosticArg::text("lhs", self.kind_name(lhs_kind)))
                .with_arg(DiagnosticArg::text("rhs", self.kind_name(rhs_kind))),
        );
        None
    }

    /// Lowers a call of `logand`, `logior`, `logxor`, `shift-left` or
    /// `shift-right`. Their operands must be statically known integers;
    /// the rules come from [`promotion::arithmetic`]. A constant shift
    /// count is checked here; any other is checked when it runs.
    fn lower_bitwise(
        &mut self,
        ctx: &mut FunctionContext,
        op: BinaryOp,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let [lhs, rhs] = match <[Expr; 2]>::try_from(args) {
            Ok(args) => args,
            Err(args) => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                        .with_arg(DiagnosticArg::text("callee", op.symbol()))
                        .with_arg(DiagnosticArg::unsigned("expected_count", 2))
                        .with_arg(DiagnosticArg::unsigned(
                            "actual_count",
                            u64::try_from(args.len()).unwrap_or(u64::MAX),
                        )),
                );
                return Expr::Error { span };
            }
        };
        // Literals adopt a 32-bit partner's type, as for arithmetic.
        let rhs = adapt_literal(rhs, Some(&[self.expr_kind(&lhs)]));
        let lhs = adapt_literal(lhs, Some(&[self.expr_kind(&rhs)]));
        let (lhs_kind, rhs_kind) = (self.expr_kind(&lhs), self.expr_kind(&rhs));
        if lhs_kind == ValueKind::Error || rhs_kind == ValueKind::Error {
            return Expr::Error { span };
        }
        let rule = match (NumericKind::of(lhs_kind), NumericKind::of(rhs_kind)) {
            (Some(lhs_numeric), Some(rhs_numeric)) => {
                match promotion::arithmetic(op, lhs_numeric, rhs_numeric) {
                    promotion::ArithmeticRule::Promote { kind } => {
                        Some((kind, lhs_numeric, rhs_numeric))
                    }
                    promotion::ArithmeticRule::Undefined => None,
                }
            }
            _ => None,
        };
        let Some((kind, lhs_numeric, rhs_numeric)) = rule else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::IntegerOperandTypeMismatch)
                    .with_arg(DiagnosticArg::text("function", op.symbol()))
                    .with_arg(DiagnosticArg::text("lhs", self.kind_name(lhs_kind)))
                    .with_arg(DiagnosticArg::text("rhs", self.kind_name(rhs_kind))),
            );
            return Expr::Error { span };
        };
        if op.is_shift() {
            let count = match &rhs {
                Expr::ConstI64 { value, .. } => Some(*value),
                Expr::Const32 {
                    value: Number32::I32(value),
                    ..
                } => Some(i64::from(*value)),
                Expr::Const32 {
                    value: Number32::U32(value),
                    ..
                } => Some(i64::from(*value)),
                _ => None,
            };
            if let Some(count) = count
                && !(0..32).contains(&count)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(rhs.span(), DiagnosticCode::ShiftCountOutOfRange)
                        .with_arg(DiagnosticArg::text("function", op.symbol()))
                        .with_arg(DiagnosticArg::text("count", format!("{count}"))),
                );
                return Expr::Error { span };
            }
        }
        Expr::Binary {
            site: None,
            op,
            kind,
            lhs: Box::new(promote_operand(lhs, lhs_numeric, kind)),
            rhs: Box::new(promote_operand(rhs, rhs_numeric, kind)),
            span,
        }
    }

    /// Lowers `+%`, `-%` or `*%`. Wrapping needs a modulus, so both
    /// operands must already share one fixed-width type; there is no
    /// promotion and no dispatch.
    fn lower_wrapping(
        &mut self,
        ctx: &mut FunctionContext,
        op: BinaryOp,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    ) -> Expr {
        let (lhs_kind, rhs_kind) = (self.expr_kind(&lhs), self.expr_kind(&rhs));
        if lhs_kind == ValueKind::Error || rhs_kind == ValueKind::Error {
            return Expr::Error { span };
        }
        match (NumericKind::of(lhs_kind), NumericKind::of(rhs_kind)) {
            (Some(lhs_kind), Some(rhs_kind))
                if matches!(
                    promotion::arithmetic(op, lhs_kind, rhs_kind),
                    promotion::ArithmeticRule::Promote { .. }
                ) =>
            {
                Expr::Binary {
                    site: None,
                    op,
                    kind: lhs_kind,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span,
                }
            }
            _ => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::WrappingOperandTypeMismatch)
                        .with_arg(DiagnosticArg::text("op", op.symbol()))
                        .with_arg(DiagnosticArg::text("lhs", self.kind_name(lhs_kind)))
                        .with_arg(DiagnosticArg::text("rhs", self.kind_name(rhs_kind))),
                );
                Expr::Error { span }
            }
        }
    }

    fn lower_compare(
        &mut self,
        ctx: &mut FunctionContext,
        op: ast::CompareOp,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    ) -> Expr {
        // `=` lowers its operands without a numeric hint, so a literal
        // adopts a 32-bit partner's type here, as it does for `<`.
        let rhs = adapt_literal(rhs, Some(&[self.expr_kind(&lhs)]));
        let lhs = adapt_literal(lhs, Some(&[self.expr_kind(&rhs)]));
        let lhs_kind = self.expr_kind(&lhs);
        let rhs_kind = self.expr_kind(&rhs);
        // Type objects compare as types: the same class, the same range of
        // integers whatever its name, or the same subclass bound.
        let type_object = |kind| kind != ValueKind::Any && self.is_type_object_kind(kind);
        if op == ast::CompareOp::Eq && type_object(lhs_kind) && type_object(rhs_kind) {
            return Expr::ValueEqual {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                kind: lhs_kind,
                span,
            };
        }
        // Positions compare by byte offset, which orders the places in one
        // string.
        if lhs_kind == ValueKind::Position && rhs_kind == ValueKind::Position {
            return Expr::IntrinsicCall {
                intrinsic: Intrinsic::PositionCompare(op.into()),
                args: vec![lhs, rhs],
                result_kind: ValueKind::Bool,
                span,
            };
        }
        // Strings order by scalar value, as UTF-8 byte order gives: the
        // host answers -1, 0 or 1, compared with 0. `=` compares contents
        // below.
        if op != ast::CompareOp::Eq
            && lhs_kind == rhs_kind
            && matches!(lhs_kind, ValueKind::String(_))
        {
            let order = Expr::IntrinsicCall {
                intrinsic: Intrinsic::Text(TextFunction::StringOrder),
                args: vec![lhs, rhs],
                result_kind: ValueKind::I64,
                span,
            };
            let zero = Expr::ConstI64 { value: 0, span };
            return compare_numbers(
                op.into(),
                (order, NumericKind::Integer),
                (zero, NumericKind::Integer),
                span,
            );
        }
        // Characters compare by scalar value, which orders them as UTF-8
        // orders their encodings.
        if lhs_kind == ValueKind::Char && rhs_kind == ValueKind::Char {
            return Expr::IntrinsicCall {
                intrinsic: Intrinsic::CharCompare(op.into()),
                args: vec![lhs, rhs],
                result_kind: ValueKind::Bool,
                span,
            };
        }
        if Self::dispatches_numerically(&[lhs_kind, rhs_kind]) {
            return self.numeric_dispatch(
                promotion::NumericOperator::Compare(op.into()),
                vec![lhs, rhs],
                span,
            );
        }
        let numeric = |kind: ValueKind| NumericKind::of(kind).is_some() || kind == ValueKind::Error;
        if op != ast::CompareOp::Eq || (numeric(lhs_kind) && numeric(rhs_kind)) {
            let Some((lhs_kind, rhs_kind)) = self.numeric_operand_kinds(
                ctx,
                span,
                &lhs,
                &rhs,
                DiagnosticCode::ComparisonOperandTypeMismatch,
            ) else {
                return Expr::Error { span };
            };
            return compare_numbers(op.into(), (lhs, lhs_kind), (rhs, rhs_kind), span);
        }

        if lhs_kind == rhs_kind && matches!(lhs_kind, ValueKind::String(_) | ValueKind::Symbol(_)) {
            return Expr::ValueEqual {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                kind: lhs_kind,
                span,
            };
        }
        // `=` is defined on any two objects, as in Dylan: an `<object>`
        // operand sends to its generic, whose default method is `==`.
        if lhs_kind == ValueKind::Any || rhs_kind == ValueKind::Any {
            return self.numeric_dispatch(
                promotion::NumericOperator::Compare(op.into()),
                vec![lhs, rhs],
                span,
            );
        }

        ctx.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::ComparisonOperandTypeMismatch)
                .with_arg(DiagnosticArg::text("lhs", self.kind_name(lhs_kind)))
                .with_arg(DiagnosticArg::text("rhs", self.kind_name(rhs_kind))),
        );
        Expr::Error { span }
    }

    /// Lowers `lhs == rhs`, Dylan's identity test: it never dispatches and
    /// is defined on every pair of objects.
    ///
    /// Heap objects are identical when they are the same object. Numbers and
    /// booleans have no identity apart from their value, so they are
    /// identical when they are the same class and the same value: integers
    /// by value, whatever `<integer>` or limited representation holds them,
    /// and floats by class and bit pattern. Boxing never changes the answer.
    fn lower_identical(
        &mut self,
        ctx: &mut FunctionContext,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    ) -> Expr {
        let lhs_kind = self.expr_kind(&lhs);
        let rhs_kind = self.expr_kind(&rhs);
        // Exact integers are identical exactly when they are equal, so two
        // unboxed integers compare as `=` compares them, without boxing.
        let integer = |kind| matches!(kind, ValueKind::I64 | ValueKind::I32 | ValueKind::U32);
        if integer(lhs_kind) && integer(rhs_kind) {
            return self.lower_compare(ctx, ast::CompareOp::Eq, lhs, rhs, span);
        }
        // So are characters, by scalar value, and positions, by offset.
        if (lhs_kind == ValueKind::Char && rhs_kind == ValueKind::Char)
            || (lhs_kind == ValueKind::Position && rhs_kind == ValueKind::Position)
        {
            return self.lower_compare(ctx, ast::CompareOp::Eq, lhs, rhs, span);
        }
        // Every value is an object, so only an operand that failed to
        // check stops the test.
        if lhs_kind == ValueKind::Error || rhs_kind == ValueKind::Error {
            return Expr::Error { span };
        }
        Expr::Identical {
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span,
        }
    }

    /// The type object `object`, of the built-in class `<limited-integer>`.
    fn lower_type_object(&self, object: TypeObject, span: Span) -> Expr {
        match self.builtin_value_class(BuiltinValueClass::LimitedIntegerType) {
            Some(class) => Expr::TypeObject {
                object,
                class,
                span,
            },
            None => Expr::Error { span },
        }
    }

    fn lower_class_object(&self, ctx: &mut FunctionContext, class_name: &str, span: Span) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return Expr::Error { span };
        };
        Expr::ClassObject {
            class: class.id,
            span,
        }
    }

    fn lower_class_of(&self, ctx: &mut FunctionContext, object: Expr, span: Span) -> Expr {
        let class = match self.value_kind_class(self.expr_kind(&object)) {
            Some(class) => class,
            None if self.expr_kind(&object) == ValueKind::Error => return Expr::Error { span },
            None => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::ClassOfTypeMismatch).with_arg(
                        DiagnosticArg::text("actual", self.kind_name(self.expr_kind(&object))),
                    ),
                );
                return Expr::Error { span };
            }
        };
        Expr::ClassOf {
            object: Box::new(object),
            class,
            span,
        }
    }

    fn lower_instance_of(
        &mut self,
        ctx: &mut FunctionContext,
        value: &ast::Expr,
        target: &ast::TypeExpr,
        span: Span,
    ) -> Expr {
        // A limited type's members are `<integer>`s in its range.
        let range = match target {
            ast::TypeExpr::Named { name, .. } => {
                LimitedInteger::from_name(name).map(IntRange::from)
            }
            target => match self.resolve_type_annotation_detail_in_context(ctx, target) {
                ResolvedType {
                    kind: ValueKind::I64,
                    range,
                    ..
                } => Some(range.unwrap_or(IntRange::INTEGER)),
                _ => {
                    let _ = self.lower_expr(ctx, value);
                    return Expr::Error { span };
                }
            },
        };
        let integer = ast::TypeExpr::Named {
            name: BuiltinValueClass::Integer.name().into(),
            span: target.span(),
        };
        if let Some(range) = range {
            return match self.lower_instance_of(ctx, value, &integer, span) {
                Expr::InstanceOf { value, .. } => Expr::InstanceOfLimited {
                    site: SiteId::PENDING,
                    value,
                    range,
                    span,
                },
                other => other,
            };
        }
        let ast::TypeExpr::Named {
            name: class_name, ..
        } = target
        else {
            return Expr::Error { span };
        };
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return Expr::Error { span };
        };
        let class_id = class.id;
        let value = self.lower_expr(ctx, value);
        if let Some(actual) = self.value_kind_class(self.expr_kind(&value))
            && !self.classes_may_overlap(actual, class_id)
        {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::InstanceOfImpossible)
                    .with_arg(DiagnosticArg::text(
                        "actual",
                        self.kind_name(ValueKind::Object(actual)),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "expected",
                        self.kind_name(ValueKind::Object(class_id)),
                    )),
            );
        }
        Expr::InstanceOf {
            site: SiteId::PENDING,
            value: Box::new(value),
            class: class_id,
            conversion: false,
            span,
        }
    }

    fn lower_cast(
        &mut self,
        ctx: &mut FunctionContext,
        value: &ast::Expr,
        target: &ast::TypeExpr,
        span: Span,
    ) -> Expr {
        let class_name = match target {
            ast::TypeExpr::Named { name, .. } => name.as_str(),
            target => {
                let resolved = self.resolve_type_annotation_detail_in_context(ctx, target);
                let value = self.lower_expr(ctx, value);
                if resolved.kind != ValueKind::I64 {
                    return Expr::Error { span };
                }
                return self.cast_boxed(ctx, value, AsTarget::Range(resolved.range), span);
            }
        };
        if let Some(limited) = LimitedInteger::from_name(class_name) {
            let value = self.lower_expr(ctx, value);
            return self.cast_boxed(ctx, value, AsTarget::Limited(limited), span);
        }
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return Expr::Error { span };
        };
        let class_id = class.id;
        let value = self.lower_expr(ctx, value);
        self.cast_boxed(ctx, value, AsTarget::Class(class_id), span)
    }

    /// Lowers `as(target, value)`. A conversion depends on the value's
    /// class, as Dylan's `as` does, so when `value` is a boxed `<object>`
    /// and `target` converts from other classes, `as` tests the value's
    /// class at run time and converts it from there by the rules it applies
    /// when the class is known: to a number type, an `<integer>`, a
    /// `<double-float>` and a `<single-float>` each convert; to `<string>`,
    /// a `<character>`, a `<symbol>` and a `<string-builder>`; to
    /// `<symbol>`, a `<string>`. Any other value is cast, which signals
    /// `type-error.cast`. The tests are ordinary `instance?` tests on a
    /// hidden local, so typing decides the ones it can, and lowering keeps
    /// only the conversion that can run.
    fn cast_boxed(
        &mut self,
        ctx: &mut FunctionContext,
        value: Expr,
        target: AsTarget,
        span: Span,
    ) -> Expr {
        let target_kind = match target {
            AsTarget::Class(class) => self.kind_for_builtin_class(class),
            AsTarget::Limited(_) | AsTarget::Range(_) => Some(ValueKind::I64),
        };
        let sources: &[BuiltinValueClass] = match target_kind {
            Some(kind) if NumericKind::of(kind).is_some() => &[
                BuiltinValueClass::SingleFloat,
                BuiltinValueClass::DoubleFloat,
                BuiltinValueClass::Integer,
            ],
            Some(ValueKind::String(_)) => &[
                BuiltinValueClass::StringBuilder,
                BuiltinValueClass::Symbol,
                BuiltinValueClass::Character,
            ],
            Some(ValueKind::Symbol(_)) => &[BuiltinValueClass::String],
            _ => &[],
        };
        if sources.is_empty() || self.expr_kind(&value) != ValueKind::Any {
            return self.cast_to(ctx, value, target, span);
        }
        let scope = ctx.scope_start();
        let local = ctx.add_local(
            DefinitionId::new(
                self.source_file,
                DefinitionKind::Local,
                AS_VALUE_LOCAL,
                &format!("as:{}:{}", span.start, span.end),
            ),
            AS_VALUE_LOCAL.into(),
            ValueKind::Any,
            None,
            span,
        );
        ctx.hide_locals_from(scope);
        let boxed = Expr::Local {
            local,
            result_kind: ValueKind::Any,
            callable_signature: None,
            span,
        };
        // Anything else is cast, and fails unless it is of the target.
        let mut chain = self.cast_to(ctx, boxed.clone(), target, span);
        for kind in sources.iter().copied() {
            let Some(class) = self.builtin_value_class(kind) else {
                continue;
            };
            let condition = Expr::InstanceOf {
                site: SiteId::PENDING,
                value: Box::new(boxed.clone()),
                class,
                conversion: true,
                span,
            };
            let Some(refinement) = self.true_branch_refinement(&condition) else {
                continue;
            };
            let number = Expr::Local {
                local,
                result_kind: refinement.kind,
                callable_signature: None,
                span,
            };
            let converted = self.cast_to(ctx, number, target, span);
            chain = Expr::If {
                condition: Box::new(condition),
                refinement: Some(refinement),
                then_branch: Box::new(converted),
                else_branch: Box::new(chain),
                span,
            };
        }
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements: vec![Statement::Let(Let {
                    local,
                    locals: vec![local],
                    value,
                    span,
                })],
                result: chain,
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Lowers `as(target, value)` by `value`'s static kind.
    fn cast_to(
        &mut self,
        ctx: &mut FunctionContext,
        value: Expr,
        target: AsTarget,
        span: Span,
    ) -> Expr {
        match target {
            AsTarget::Class(class) => self.cast_to_class(ctx, value, class, span),
            AsTarget::Limited(limited) => self.cast_to_limited(ctx, value, limited, span),
            AsTarget::Range(range) => self.cast_to_range(ctx, value, range, span),
        }
    }

    /// Lowers `as(<i32>, x)` or `as(<u32>, x)`: a checked conversion from
    /// any number (exact for floats, in range for integers), or a checked
    /// cast to `<integer>` followed by that conversion.
    fn cast_to_limited(
        &mut self,
        ctx: &mut FunctionContext,
        value: Expr,
        limited: LimitedInteger,
        span: Span,
    ) -> Expr {
        let to = NumericKind::of(limited_integer_kind(limited)).unwrap_or(NumericKind::Integer);
        let value = match NumericKind::of(self.expr_kind(&value)) {
            Some(from) if from == to => return value,
            Some(_) => value,
            None => {
                let Some(integer) = self.builtin_value_class(BuiltinValueClass::Integer) else {
                    return Expr::Error { span };
                };
                match self.cast_to_class(ctx, value, integer, span) {
                    error @ Expr::Error { .. } => return error,
                    integer => integer,
                }
            }
        };
        Expr::Convert {
            site: SiteId::PENDING,
            to,
            value: Box::new(value),
            explicit: true,
            span,
        }
    }

    /// Lowers `as(limited(<integer>, ...), x)`: `x` as an `<integer>`, as
    /// `as(<integer>, x)` would give it, checked to be in `range` (`None`
    /// for every `<integer>`).
    fn cast_to_range(
        &mut self,
        ctx: &mut FunctionContext,
        value: Expr,
        range: Option<IntRange>,
        span: Span,
    ) -> Expr {
        let value = match self.expr_kind(&value) {
            ValueKind::I64 | ValueKind::I32 | ValueKind::U32 | ValueKind::Error => value,
            _ => {
                let Some(integer) = self.builtin_value_class(BuiltinValueClass::Integer) else {
                    return Expr::Error { span };
                };
                match self.cast_to_class(ctx, value, integer, span) {
                    error @ Expr::Error { .. } => return error,
                    integer => integer,
                }
            }
        };
        match range {
            Some(range) => Expr::CheckRange {
                value: Box::new(value),
                range,
                site: SiteId::PENDING,
                explicit: true,
                span,
            },
            None => {
                let from = NumericKind::of(self.expr_kind(&value)).unwrap_or(NumericKind::Integer);
                promote_operand(value, from, NumericKind::Integer)
            }
        }
    }

    fn cast_to_class(
        &mut self,
        ctx: &mut FunctionContext,
        value: Expr,
        class_id: ClassId,
        span: Span,
    ) -> Expr {
        // A `<character>` and its scalar value convert both ways: to an
        // `<integer>` always, and back when the integer is a scalar value.
        let target = self.kind_for_builtin_class(class_id);
        match (self.expr_kind(&value), target) {
            (ValueKind::Char, Some(ValueKind::I64)) => {
                return Expr::IntrinsicCall {
                    intrinsic: Intrinsic::CharToInteger,
                    args: vec![value],
                    result_kind: ValueKind::I64,
                    span,
                };
            }
            (ValueKind::I64 | ValueKind::I32 | ValueKind::U32, Some(ValueKind::Char)) => {
                return Expr::IntrinsicCall {
                    intrinsic: Intrinsic::Text(TextFunction::CharacterFromInteger),
                    args: vec![value],
                    result_kind: ValueKind::Char,
                    span,
                };
            }
            // A character, a builder's text or a symbol's name becomes a
            // `<string>`.
            (actual, Some(ValueKind::String(class))) => {
                let builder = self.builtin_value_class(BuiltinValueClass::StringBuilder);
                let symbol = self.builtin_value_class(BuiltinValueClass::Symbol);
                let function = match actual {
                    ValueKind::Char => Some(TextFunction::CharacterToString),
                    ValueKind::Object(object) if Some(object) == builder => {
                        Some(TextFunction::BuilderToString)
                    }
                    ValueKind::Symbol(_) => Some(TextFunction::SymbolToString),
                    // A boxed symbol `instance?` narrowed.
                    ValueKind::Object(object) if Some(object) == symbol => {
                        Some(TextFunction::SymbolToString)
                    }
                    _ => None,
                };
                if let Some(function) = function {
                    return Expr::IntrinsicCall {
                        intrinsic: Intrinsic::Text(function),
                        args: vec![value],
                        result_kind: ValueKind::String(class),
                        span,
                    };
                }
            }
            // A string names the symbol it interns; a boxed one `instance?`
            // narrowed is an object of the string class.
            (ValueKind::String(_), Some(ValueKind::Symbol(class))) => {
                return Expr::IntrinsicCall {
                    intrinsic: Intrinsic::Text(TextFunction::StringToSymbol),
                    args: vec![value],
                    result_kind: ValueKind::Symbol(class),
                    span,
                };
            }
            (ValueKind::Object(object), Some(ValueKind::Symbol(class)))
                if Some(object) == self.builtin_value_class(BuiltinValueClass::String) =>
            {
                return Expr::IntrinsicCall {
                    intrinsic: Intrinsic::Text(TextFunction::StringToSymbol),
                    args: vec![value],
                    result_kind: ValueKind::Symbol(class),
                    span,
                };
            }
            _ => {}
        }
        if let Some(from) = NumericKind::of(self.expr_kind(&value))
            && let Some(to) = self
                .kind_for_builtin_class(class_id)
                .and_then(NumericKind::of)
            && from != to
        {
            return Expr::Convert {
                site: SiteId::PENDING,
                to,
                value: Box::new(value),
                explicit: true,
                span,
            };
        }
        if let Some(actual) = self.value_kind_class(self.expr_kind(&value))
            && !self.classes_may_overlap(actual, class_id)
        {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::CastImpossible)
                    .with_arg(DiagnosticArg::text(
                        "actual",
                        self.kind_name(ValueKind::Object(actual)),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "expected",
                        self.kind_name(ValueKind::Object(class_id)),
                    )),
            );
        }
        Expr::Cast {
            site: SiteId::PENDING,
            value: Box::new(value),
            class: class_id,
            explicit: true,
            span,
        }
    }

    /// `value` where it meets a declared `expected` kind, and callable
    /// `signature` when it has one, as Dylan has it:
    /// as it is when its kind already holds, or wrapped in an inserted
    /// check when it may hold at run time. A value that never can is left
    /// as it is, for the caller to report.
    ///
    /// The check is an implicit cast to the class `expected` names, which
    /// signals a `<type-error>` with a `use-value` restart as `as` does and
    /// unboxes a scalar on the way. It narrows only: a value whose class is
    /// a superclass of the declared one, an `<object>` among them, may be
    /// an instance of it, while one of a disjoint class never is. Soft
    /// typing records the check like any other, proves it away when it can,
    /// and holds a strict definition to proving it.
    fn narrow(
        &self,
        value: Expr,
        expected: ValueKind,
        signature: Option<&CallableSignature>,
    ) -> Expr {
        let actual = self.expr_kind(&value);
        if self.kinds_match(expected, actual) {
            return value;
        }
        let Some(class) = self.implicit_cast_class(expected, actual, signature) else {
            return value;
        };
        let span = value.span();
        Expr::Cast {
            value: Box::new(value),
            class,
            site: SiteId::PENDING,
            explicit: false,
            span,
        }
    }

    /// Narrows each of `args` to the parameter kind it meets; see
    /// [`Self::narrow`].
    fn narrow_args(
        &self,
        args: &mut [Expr],
        kinds: &[ValueKind],
        sigs: &[Option<CallableSignature>],
    ) {
        for (index, (arg, expected)) in args.iter_mut().zip(kinds).enumerate() {
            let value = core::mem::replace(arg, Expr::Error { span: arg.span() });
            let signature = sigs.get(index).and_then(Option::as_ref);
            *arg = self.narrow(value, *expected, signature);
        }
    }

    /// Narrows each of `args` to the type its generic parameter declares,
    /// if any; see [`Self::narrow`].
    fn narrow_generic_args(&self, args: &mut [Expr], declared: &[Option<ValueKind>]) {
        for (arg, declared) in args.iter_mut().zip(declared) {
            let Some(expected) = *declared else {
                continue;
            };
            if self.generic_argument_matches(expected, self.expr_kind(arg)) {
                continue;
            }
            let value = core::mem::replace(arg, Expr::Error { span: arg.span() });
            *arg = self.narrow(value, expected, None);
        }
    }

    /// Narrows the values `value` produces to the `expected` kinds they
    /// meet, each on its own; see [`Self::narrow`]. Values of another count
    /// are left for the caller to report.
    fn narrow_results(
        &self,
        ctx: &mut FunctionContext,
        value: Expr,
        expected: &[ValueKind],
        sigs: &[Option<CallableSignature>],
        tag: &str,
    ) -> Expr {
        // A result of more values than declared keeps the first.
        let value = self.take_values(ctx, value, expected.len());
        let actual = self.expr_kinds(&value);
        if actual.len() != expected.len() {
            return value;
        }
        let sig = |index: usize| sigs.get(index).and_then(Option::as_ref);
        if let [expected] = expected {
            return self.narrow(value, *expected, sig(0));
        }
        let checks = actual
            .iter()
            .zip(expected)
            .enumerate()
            .map(|(index, (actual, expected))| {
                match (!self.kinds_match(*expected, *actual))
                    .then(|| self.implicit_cast_class(*expected, *actual, sig(index)))
                    .flatten()
                {
                    Some(class) => CallableResultCheck::Narrow {
                        from: *actual,
                        class,
                        range: None,
                    },
                    None => CallableResultCheck::Trusted(*actual),
                }
            })
            .collect::<Vec<_>>();
        if checks
            .iter()
            .all(|check| matches!(check, CallableResultCheck::Trusted(_)))
        {
            return value;
        }
        let kinds = checks
            .iter()
            .map(|check| match check {
                CallableResultCheck::Narrow { class, .. } => self
                    .kind_for_builtin_class(*class)
                    .unwrap_or(ValueKind::Object(*class)),
                check => check.wire_kind(),
            })
            .collect::<Vec<_>>();
        let sigs = self.expr_result_callable_signatures(&value);
        let span = value.span();
        self.check_values(ctx, value, &checks, &kinds, &sigs, tag, span)
    }

    /// `receiver` of the slot accessor `name`, cast to the one class that
    /// declares an accessor of that name when the receiver's own type does
    /// not have it but may be an instance of that class, as a send of the
    /// accessor would find its only method; see [`Self::narrow`].
    fn narrow_receiver(&self, receiver: Expr, name: &str) -> Expr {
        let actual = self.expr_kind(&receiver);
        let known = self.value_kind_class(actual).and_then(|class| {
            self.classes.get(class.0 as usize).filter(|class| {
                class
                    .effective_slots
                    .iter()
                    .any(|slot| slot.accessor.name == name || slot.name == name)
            })
        });
        if known.is_some() {
            return receiver;
        }
        let mut owners = self.classes.iter().filter(|class| {
            class
                .direct_slots
                .iter()
                .any(|slot| slot.accessor.name == name || slot.name == name)
        });
        let (Some(owner), None) = (owners.next(), owners.next()) else {
            return receiver;
        };
        self.narrow(receiver, ValueKind::Object(owner.id), None)
    }

    /// The class an implicit cast from `actual` to `expected` checks, when
    /// one may succeed: `expected` names a class whose values have a single
    /// representation, and that class is a proper subclass of `actual`'s.
    /// A limited integer has its own conversions, and a callable `signature`
    /// is more than a class a run-time cast could check, so neither is one.
    fn implicit_cast_class(
        &self,
        expected: ValueKind,
        actual: ValueKind,
        signature: Option<&CallableSignature>,
    ) -> Option<ClassId> {
        if signature.is_some() {
            return None;
        }
        let target = match expected {
            ValueKind::Object(class)
            | ValueKind::String(class)
            | ValueKind::Symbol(class)
            | ValueKind::Vector(class) => class,
            ValueKind::I64
            | ValueKind::F64
            | ValueKind::F32
            | ValueKind::Bool
            | ValueKind::Char
            | ValueKind::Position
            | ValueKind::Callable => self.value_kind_class(expected)?,
            ValueKind::Any
            | ValueKind::Error
            | ValueKind::I32
            | ValueKind::U32
            | ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. }
            | ValueKind::KnownCallable(_) => return None,
        };
        let source = match actual {
            ValueKind::Any => OBJECT_CLASS_ID,
            ValueKind::Error => return None,
            actual => self.value_kind_class(actual)?,
        };
        (target != source && self.is_same_or_subclass(target, source)).then_some(target)
    }

    /// The operation `make` on the built-in collection class `kind` is.
    const fn collection_maker(kind: BuiltinValueClass) -> Option<Intrinsic> {
        match kind {
            BuiltinValueClass::Vector => Some(Intrinsic::MakeVector),
            BuiltinValueClass::StretchyVector => Some(Intrinsic::MakeStretchyVector),
            BuiltinValueClass::Table => Some(Intrinsic::MakeTable),
            _ => None,
        }
    }

    /// Lowers `make` of a built-in collection class: `make(<vector>, size:
    /// n, fill: x)` and `make(<stretchy-vector>, ...)`, whose `size:` is 0
    /// and `fill:` `#f` when absent, and `make(<table>)`, which takes no
    /// keywords. A size below 0 or past
    /// [`interloko_runtime::MAX_VECTOR_SIZE`] signals a
    /// `type-error.range`.
    fn lower_make_collection(
        &mut self,
        ctx: &mut FunctionContext,
        make: Intrinsic,
        class_name: &str,
        fields: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let keywords: &[&str] = match make {
            Intrinsic::MakeTable => &[],
            _ => &["size", "fill"],
        };
        let mut size = None;
        let mut fill = None;
        for field in fields {
            let slot = match field.name.as_str() {
                "size" if keywords.contains(&"size") => &mut size,
                "fill" if keywords.contains(&"fill") => &mut fill,
                _ => {
                    ctx.diagnostics.push(
                        Diagnostic::error(field.span, DiagnosticCode::UnknownInitKeyword)
                            .with_arg(DiagnosticArg::text("class", class_name))
                            .with_arg(DiagnosticArg::text("keyword", format!("{}:", field.name))),
                    );
                    continue;
                }
            };
            if slot.is_some() {
                ctx.diagnostics.push(
                    Diagnostic::error(field.span, DiagnosticCode::DuplicateInitKeyword)
                        .with_arg(DiagnosticArg::text("keyword", format!("{}:", field.name))),
                );
                continue;
            }
            *slot = Some(self.lower_expr_expected(ctx, &field.value, None));
        }
        if make == Intrinsic::MakeTable {
            return self.checked_intrinsic_call(ctx, make, "make", Vec::new(), span);
        }
        let size = size.unwrap_or(Expr::ConstI64 { value: 0, span });
        let size = self.limit_value(
            size,
            ValueKind::I64,
            Some(IntRange {
                min: Some(0),
                max: i64::try_from(interloko_runtime::MAX_VECTOR_SIZE).ok(),
            }),
        );
        let fill = fill.unwrap_or(Expr::ConstBool { value: false, span });
        self.checked_intrinsic_call(ctx, make, "make", vec![size, fill], span)
    }

    fn lower_make_object(
        &mut self,
        ctx: &mut FunctionContext,
        class_expr: &ast::Expr,
        fields: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let class_expr = self.lower_expr(ctx, class_expr);
        let class_id = match self.expr_kind(&class_expr) {
            ValueKind::ClassObject(class_id) => class_id,
            ValueKind::Error => return Expr::Error { span },
            _ => {
                ctx.diagnostics.push(
                    Diagnostic::error(
                        class_expr.span(),
                        DiagnosticCode::MakeClassArgumentTypeMismatch,
                    )
                    .with_arg(DiagnosticArg::text(
                        "actual",
                        self.kind_name(self.expr_kind(&class_expr)),
                    )),
                );
                return Expr::Error { span };
            }
        };
        let Some(class) = self.classes.get(class_id.0 as usize) else {
            return Expr::Error { span };
        };
        // The builder is the one kernel class `make` creates: it takes no
        // keywords and starts empty.
        if class.origin
            == (ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::StringBuilder,
            })
        {
            if let Some(field) = fields.first() {
                ctx.diagnostics
                    .push(Self::unexpected_keyword("make", field));
                return Expr::Error { span };
            }
            return self.checked_intrinsic_call(
                ctx,
                Intrinsic::Text(TextFunction::MakeBuilder),
                "make",
                Vec::new(),
                span,
            );
        }
        let class_name = class.name.clone();
        let class_origin = class.origin;
        let effective_slots = class.effective_slots.clone();
        let initialization_plan = class.initialization_plan.clone();
        if matches!(class_origin, ClassOrigin::HostBacked { .. }) {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::HostBackedMake)
                    .with_arg(DiagnosticArg::text("class", class_name.clone())),
            );
            return Expr::Error { span };
        }
        if let ClassOrigin::BuiltinValue { kind } = class_origin
            && let Some(make) = Self::collection_maker(kind)
            && matches!(class_expr, Expr::ClassObject { .. })
        {
            return self.lower_make_collection(ctx, make, &class_name, fields, span);
        }
        if matches!(class_origin, ClassOrigin::BuiltinValue { .. }) {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::BuiltinValueMake)
                    .with_arg(DiagnosticArg::text("class", class_name.clone())),
            );
            return Expr::Error { span };
        }

        let mut init_args = Vec::new();
        for field in fields {
            if init_args
                .iter()
                .any(|arg: &InitArg| arg.keyword == field.name)
            {
                ctx.diagnostics.push(
                    Diagnostic::error(field.span, DiagnosticCode::DuplicateInitKeyword)
                        .with_arg(DiagnosticArg::text("keyword", format!("{}:", field.name))),
                );
                continue;
            }
            let Some(slot) = effective_slots
                .iter()
                .find(|slot| slot.init_keyword == field.name)
            else {
                ctx.diagnostics.push(
                    Diagnostic::error(field.span, DiagnosticCode::UnknownInitKeyword)
                        .with_arg(DiagnosticArg::text("class", class_name.clone()))
                        .with_arg(DiagnosticArg::text("keyword", format!("{}:", field.name))),
                );
                continue;
            };
            let value = self
                .lower_method_against(ctx, &field.value, slot.callable_signature.as_ref())
                .unwrap_or_else(|| self.lower_expr(ctx, &field.value));
            let value = adapt_literal(value, Some(core::slice::from_ref(&slot.kind)));
            let value = self.narrow(value, slot.kind, slot.callable_signature.as_ref());
            let value_kind = self.expr_kind(&value);
            if !self.kinds_match(slot.kind, value_kind) {
                ctx.diagnostics.push(
                    Diagnostic::error(field.span, DiagnosticCode::SlotInitializerTypeMismatch)
                        .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", self.kind_name(slot.kind)))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(value_kind))),
                );
            } else if let Some(expected) = &slot.callable_signature {
                ctx.diagnostics.extend(self.callable_value_diagnostic(
                    field.span,
                    expected,
                    value_kind,
                    self.expr_callable_signature(&value).as_ref(),
                    |expected, actual| {
                        Diagnostic::error(field.span, DiagnosticCode::SlotInitializerTypeMismatch)
                            .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                            .with_arg(DiagnosticArg::text("expected", expected))
                            .with_arg(DiagnosticArg::text("actual", actual))
                    },
                ));
            }
            init_args.push(InitArg {
                keyword: field.name.clone(),
                slot: slot.id,
                value: self.limit_value(value, slot.kind, slot.range),
                span: field.span,
            });
        }

        let mut resolved = Vec::new();
        for step in &initialization_plan.steps {
            let slot = &effective_slots[step.slot.0 as usize];
            if let Some(arg) = init_args.iter().find(|arg| arg.slot == step.slot) {
                resolved.push(FieldInit {
                    slot: step.slot,
                    value: arg.value.clone(),
                    source: FieldInitSource::Explicit,
                    span: arg.span,
                });
            } else {
                if let Some(init_value) = &slot.init_value {
                    resolved.push(FieldInit {
                        slot: step.slot,
                        value: init_value.clone(),
                        source: FieldInitSource::Default,
                        span: init_value.span(),
                    });
                } else if step.required_init_keyword {
                    ctx.diagnostics.push(
                        Diagnostic::error(span, DiagnosticCode::MissingRequiredInitKeyword)
                            .with_arg(DiagnosticArg::text(
                                "keyword",
                                format!("{}:", slot.init_keyword),
                            ))
                            .with_arg(DiagnosticArg::text("slot", slot.name.clone())),
                    );
                } else {
                    ctx.diagnostics.push(
                        Diagnostic::error(span, DiagnosticCode::MissingSlotInitializer)
                            .with_arg(DiagnosticArg::text("slot", slot.name.clone())),
                    );
                }
            }
        }

        // Only a class literal names its class exactly; any other class
        // value (`class-of(x)`, a parameter, a local) may name a subclass of
        // its bound, which is the class `make` must allocate.
        if !matches!(class_expr, Expr::ClassObject { .. }) {
            return Expr::MakeRuntimeClass {
                class: Box::new(class_expr),
                bound: class_id,
                candidates: self.make_candidates(class_id, &init_args),
                init_args,
                span,
            };
        }
        if self
            .classes
            .get(class_id.0 as usize)
            .is_some_and(|class| class.is_abstract)
        {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::AbstractClassMake)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return Expr::Error { span };
        }
        Expr::MakeObject {
            class: Box::new(class_expr),
            class_id,
            init_args,
            fields: resolved,
            span,
        }
    }

    /// Returns every class a `make` on a class value bounded by `bound` may
    /// allocate, each with the defaults that complete `init_args` for it.
    ///
    /// Subclasses keep their superclass's effective slots, in order, as a
    /// prefix, so an init argument resolved against `bound` fills the same
    /// slot id in every candidate. Built-in value and host-backed classes
    /// cannot be made and are never candidates, and neither are abstract
    /// classes, which have no direct instances: a class value naming one
    /// traps as one outside the candidates does.
    fn make_candidates(&self, bound: ClassId, init_args: &[InitArg]) -> Vec<MakeCandidate> {
        self.classes
            .iter()
            .filter(|class| {
                matches!(class.origin, ClassOrigin::Native | ClassOrigin::ObjectRoot)
                    && !class.is_abstract
                    && self.is_same_or_subclass(class.id, bound)
            })
            .map(|class| {
                let mut defaults = Vec::new();
                let mut missing_keyword = None;
                for step in &class.initialization_plan.steps {
                    if init_args.iter().any(|arg| arg.slot == step.slot) {
                        continue;
                    }
                    let Some(slot) = class.effective_slots.get(step.slot.0 as usize) else {
                        continue;
                    };
                    match &slot.init_value {
                        Some(init_value) => defaults.push(FieldInit {
                            slot: step.slot,
                            value: init_value.clone(),
                            source: FieldInitSource::Default,
                            span: init_value.span(),
                        }),
                        None => {
                            missing_keyword
                                .get_or_insert_with(|| format!("{}:", slot.init_keyword));
                        }
                    }
                }
                MakeCandidate {
                    class: class.id,
                    defaults,
                    missing_keyword,
                }
            })
            .collect()
    }

    fn lower_slot_read(
        &mut self,
        ctx: &mut FunctionContext,
        id: AstNodeId,
        receiver: &ast::Expr,
        slot_name: &str,
        span: Span,
    ) -> Expr {
        let receiver_span = receiver.span();
        let receiver = self.lower_expr(ctx, receiver);
        // `x.f` is `f(x)`: a receiver typing knows to be of a class no
        // class with the slot can be, such as a `<pair>` read with `.head`
        // where a program's class has a `head` slot, calls the built-in.
        if built_in::names_built_in(slot_name)
            && !self.program_defines(slot_name)
            && let Some(class) = self.value_kind_class(self.expr_kind(&receiver))
            && !self.classes.iter().any(|candidate| {
                candidate
                    .direct_slots
                    .iter()
                    .any(|slot| slot.accessor.name == slot_name)
                    && self.classes_may_overlap(candidate.id, class)
            })
        {
            let scope = ctx.scope_start();
            let name = format!("{slot_name} receiver@{}", span.start);
            let bound = self.bind_hidden_local(ctx, id, &name, receiver, receiver_span);
            let arg = ast::Expr::Name {
                id,
                name,
                span: receiver_span,
            };
            let call = self.lower_built_in_call(ctx, id, slot_name, &[arg], &[], None, span);
            ctx.hide_locals_from(scope);
            return with_lets(vec![bound], call, span);
        }
        self.lower_accessor_call(ctx, receiver, slot_name, span)
    }

    /// Whether `x.name` calls `name` rather than reading a slot: no class
    /// has a slot `name`, and `name` is something a call can reach, a
    /// binding, a definition or a built-in such as `head`.
    fn calls_for_slot_syntax(&self, ctx: &FunctionContext, name: &str) -> bool {
        !self.has_accessor(name)
            && (ctx.binds(name)
                || CollectionFunction::from_name(name).is_some()
                || self.resolve_function(name).is_some()
                || self.resolve_generic(name).is_some()
                || self.resolve_constant(name).is_some()
                || self.resolve_host_function(name).is_some()
                || built_in::names_built_in(name))
    }

    /// Lowers `name := value` for a `let` local `name`.
    ///
    /// The value must have the local's declared category, which for a
    /// local declared without a type is its initializer's, and passes the
    /// local's range check when the local is limited.
    fn lower_local_assign(
        &mut self,
        ctx: &mut FunctionContext,
        name: &str,
        name_span: Span,
        value: &ast::Expr,
        span: Span,
    ) -> Expr {
        let Some(local) = ctx.find_local(name) else {
            if let Some(capture) = ctx.find_capture_or_add(name) {
                return self.lower_capture_assign(ctx, capture, name, name_span, value, span);
            }
            let _ = self.lower_expr(ctx, value);
            let kind = if self.resolve_constant(name).is_some() {
                "constant"
            } else if self.resolve_function(name).is_some() || self.resolve_generic(name).is_some()
            {
                "function"
            } else {
                ctx.diagnostics.push(
                    Diagnostic::error(name_span, DiagnosticCode::UnresolvedLocal)
                        .with_arg(DiagnosticArg::text("local", name)),
                );
                return Expr::Error { span };
            };
            ctx.diagnostics.push(
                Diagnostic::error(name_span, DiagnosticCode::AssignmentTargetNotAssignable)
                    .with_arg(DiagnosticArg::text("name", name))
                    .with_arg(DiagnosticArg::text("kind", kind)),
            );
            return Expr::Error { span };
        };
        if !ctx.assignable.contains(&local) {
            let _ = self.lower_expr(ctx, value);
            let definition = ctx
                .locals
                .get(local.0 as usize)
                .map(|local| local.definition);
            let exit =
                definition.is_some_and(|definition| ctx.exit_definitions.contains(&definition));
            let loop_variable =
                definition.is_some_and(|definition| ctx.loop_variables.contains(&definition));
            let kind = if loop_variable {
                "`for` loop variable"
            } else if exit {
                "block exit procedure"
            } else {
                "restart's condition"
            };
            ctx.diagnostics.push(
                Diagnostic::error(name_span, DiagnosticCode::AssignmentTargetNotAssignable)
                    .with_arg(DiagnosticArg::text("name", name))
                    .with_arg(DiagnosticArg::text("kind", kind)),
            );
            return Expr::Error { span };
        }
        let kind = ctx.local_kind(local);
        let signature = ctx.local_callable_signature(local);
        let range = ctx
            .locals
            .get(local.0 as usize)
            .and_then(|local| local.range);
        let value = self.lower_assigned_value(ctx, name, kind, signature.as_ref(), range, value);
        ctx.note_assignment(local);
        Expr::AssignLocal {
            local,
            result_kind: kind,
            callable_signature: signature,
            value: Box::new(value),
            span,
        }
    }

    /// Lowers `name := value` inside a closure, where `name` is the
    /// capture `capture` of an enclosing local. Only a shared local, which
    /// lives in a cell the closure holds, can be assigned this way: a `let`
    /// local is shared whenever a closure assigns it.
    fn lower_capture_assign(
        &mut self,
        ctx: &mut FunctionContext,
        capture: CaptureId,
        name: &str,
        name_span: Span,
        value: &ast::Expr,
        span: Span,
    ) -> Expr {
        let Some(captured) = ctx.captures.get(capture.0 as usize).cloned() else {
            return Expr::Error { span };
        };
        if !captured.shared {
            let _ = self.lower_expr(ctx, value);
            let source = ctx
                .capture_sources
                .iter()
                .find(|source| source.id == captured.source)
                .map(|source| source.definition);
            let exit = source.is_some_and(|source| ctx.exit_definitions.contains(&source));
            let loop_variable = source.is_some_and(|source| ctx.loop_variables.contains(&source));
            let kind = if exit {
                "block exit procedure"
            } else if loop_variable {
                "`for` loop variable"
            } else {
                "restart's condition"
            };
            ctx.diagnostics.push(
                Diagnostic::error(name_span, DiagnosticCode::AssignmentTargetNotAssignable)
                    .with_arg(DiagnosticArg::text("name", name))
                    .with_arg(DiagnosticArg::text("kind", kind)),
            );
            return Expr::Error { span };
        }
        let value = self.lower_assigned_value(
            ctx,
            name,
            captured.kind,
            captured.callable_signature.as_ref(),
            captured.range,
            value,
        );
        if !ctx.assigned_captures.contains(&captured.source) {
            ctx.assigned_captures.push(captured.source);
        }
        Expr::AssignCapture {
            capture,
            result_kind: captured.kind,
            callable_signature: captured.callable_signature,
            value: Box::new(value),
            span,
        }
    }

    /// Lowers the value `:=` stores in the local `name` declared `kind`,
    /// checked against its callable `signature` and limited `range`.
    fn lower_assigned_value(
        &mut self,
        ctx: &mut FunctionContext,
        name: &str,
        kind: ValueKind,
        signature: Option<&CallableSignature>,
        range: Option<IntRange>,
        value: &ast::Expr,
    ) -> Expr {
        let value_expr = self
            .lower_method_against(ctx, value, signature)
            .unwrap_or_else(|| {
                self.lower_expr_expected_results(ctx, value, Some(core::slice::from_ref(&kind)))
            });
        let value_expr = self.take_values(ctx, value_expr, 1);
        let actual_kinds = self.expr_kinds(&value_expr);
        let actual_kind = self.expr_kind(&value_expr);
        let mismatch = |expected: String, actual: String| {
            Diagnostic::error(value.span(), DiagnosticCode::LocalAssignmentTypeMismatch)
                .with_arg(DiagnosticArg::text("local", name))
                .with_arg(DiagnosticArg::text("expected", expected))
                .with_arg(DiagnosticArg::text("actual", actual))
        };
        let mut value_expr = value_expr;
        if actual_kinds.len() != 1 && !result_kinds_contain_error(&actual_kinds) {
            ctx.diagnostics.push(mismatch(
                "1 value".into(),
                self.result_kinds_name(&actual_kinds),
            ));
        } else if !self.kinds_match(kind, actual_kind) {
            value_expr = self.narrow(value_expr, kind, signature);
            if !self.kinds_match(kind, self.expr_kind(&value_expr)) {
                ctx.diagnostics
                    .push(mismatch(self.kind_name(kind), self.kind_name(actual_kind)));
            }
        } else if let Some(expected) = signature {
            let actual = self.expr_callable_signature(&value_expr);
            if !self.callable_signatures_match(Some(expected), actual.as_ref()) {
                let diagnostic = self
                    .callable_variance_diagnostic(value.span(), expected, actual.as_ref())
                    .unwrap_or_else(|| {
                        mismatch(
                            self.callable_signature_name(expected),
                            actual.as_ref().map_or_else(
                                || self.kind_name(actual_kind),
                                |signature| self.callable_signature_name(signature),
                            ),
                        )
                    });
                ctx.diagnostics.push(diagnostic);
            }
        }
        self.limit_value(value_expr, kind, range)
    }

    /// Lowers `while (test) body end` or `until (test) body end`. The body's
    /// locals are scoped to it, and its values are discarded.
    fn lower_loop(
        &mut self,
        ctx: &mut FunctionContext,
        kind: ast::LoopKind,
        test: &ast::Expr,
        body: &ast::Block,
        span: Span,
    ) -> Expr {
        let test = self.lower_expr(ctx, test);
        let keyword = match kind {
            ast::LoopKind::While => "while",
            ast::LoopKind::Until => "until",
        };
        self.check_test_is_boolean(ctx, keyword, &test);
        let scope = ctx.scope_start();
        let body = self.lower_block(ctx, body, None);
        ctx.hide_locals_from(scope);
        Expr::Loop {
            kind: kind.into(),
            test: Box::new(test),
            body: Box::new(body),
            span,
        }
    }

    /// Lowers a `for` loop to a `while` loop over state only the loop can
    /// name, one clause at a time ([`ForPlan`]): the loop runs while every
    /// clause has another value, testing them in order, and steps each
    /// after the body.
    ///
    /// Each clause's expressions are evaluated once, before the loop, in
    /// clause order. Each iteration binds every variable afresh, as `let
    /// name = value;` at the top of the body would, except that `:=` may
    /// not assign it, so a closure may always capture it, capturing that
    /// iteration's value. The loop produces no values.
    fn lower_for(
        &mut self,
        ctx: &mut FunctionContext,
        clauses: &[ast::ForClause],
        body: &ast::Block,
        span: Span,
    ) -> Expr {
        let scope = ctx.scope_start();
        let mut statements = Vec::new();
        let plans = clauses
            .iter()
            .map(|clause| match clause {
                ast::ForClause::Numeric(clause) => {
                    self.plan_numeric_for(ctx, clause, &mut statements)
                }
                ast::ForClause::Collection(clause) => {
                    self.plan_collection_for(ctx, clause, &mut statements)
                }
            })
            .collect::<Vec<_>>();
        // Every clause has to have a value for the body to run; the first
        // that has none ends the loop.
        let test = plans
            .iter()
            .rev()
            .map(|plan| plan.test.clone())
            .reduce(|rest, test| Expr::If {
                span: test.span(),
                condition: Box::new(test),
                refinement: None,
                then_branch: Box::new(rest),
                else_branch: Box::new(Expr::ConstBool { value: false, span }),
            })
            .unwrap_or(Expr::ConstBool { value: true, span });
        // The body is lowered as if it began with a `let` per variable, so a
        // variable's annotation and range check are a `let`'s; only its
        // assignability differs.
        let body_scope = ctx.scope_start();
        // Every local of one name shares a definition, so a variable is a
        // loop variable only while its body is lowered: a later `let` of
        // the same name is assignable.
        let loop_variables = ctx.loop_variables.len();
        let mut each = Vec::new();
        let mut bindings = Vec::new();
        for (clause, plan) in clauses.iter().zip(&plans) {
            let mut binding = clause.binding().clone();
            // An element is an `<object>`, which `as` checks against a class
            // or limited type; there is no `as` to a callable type.
            if plan.checked
                && let Some(annotation @ ast::TypeExpr::Callable { .. }) = &binding.type_annotation
            {
                ctx.diagnostics.push(
                    Diagnostic::error(annotation.span(), DiagnosticCode::ForClauseTypeMismatch)
                        .with_arg(DiagnosticArg::text("word", "in"))
                        .with_arg(DiagnosticArg::text(
                            "expected",
                            "a class or limited type for its elements",
                        ))
                        .with_arg(DiagnosticArg::text("actual", annotation.to_string())),
                );
                binding.type_annotation = None;
            }
            let binding = &binding;
            each.extend(plan.each.iter().cloned());
            ctx.loop_variables
                .push(self.local_definition(binding.definition, &binding.name));
            bindings.push(ast::Statement::Let(Box::new(ast::Let {
                id: body.id,
                definition: binding.definition,
                name: binding.name.clone(),
                type_annotation: binding.type_annotation.clone(),
                pattern: ast::LetPattern::Name(binding.clone()),
                value: plan.binding_value(body.id, binding),
                span: binding.span,
            })));
        }
        let mut iteration = body.clone();
        iteration.statements.splice(0..0, bindings);
        let mut lowered = self.lower_block(ctx, &iteration, None);
        ctx.hide_locals_from(body_scope);
        ctx.loop_variables.truncate(loop_variables);
        lowered.statements.splice(0..0, each);
        let result = core::mem::replace(&mut lowered.result, no_values(span));
        lowered.statements.push(Statement::Expr(result));
        for plan in plans {
            lowered.statements.push(Statement::Expr(plan.step));
            ctx.note_assignment(plan.state);
        }
        ctx.hide_locals_from(scope);
        Expr::Block {
            exit: None,
            body: Box::new(Block {
                statements,
                result: Expr::Loop {
                    kind: LoopKind::While,
                    test: Box::new(test),
                    body: Box::new(lowered),
                    span,
                },
                span,
            }),
            cleanup: None,
            span,
        }
    }

    /// Plans a numeric clause, `name from start (to|below|above bound)? (by
    /// step)?`: a counter, with the start, and a bound or step that is not
    /// a constant, in hidden locals evaluated once. The counter steps with
    /// checked `<integer>` arithmetic.
    fn plan_numeric_for(
        &mut self,
        ctx: &mut FunctionContext,
        clause: &ast::NumericFor,
        statements: &mut Vec<Statement>,
    ) -> ForPlan {
        let binding = &clause.binding;
        let start = self.lower_for_integer(ctx, "from", &clause.start);
        let counter =
            self.hidden_for_local(ctx, binding, "counter", ValueKind::I64, start, statements);
        let bound = clause.bound.as_ref().map(|(kind, bound)| {
            let value = self.lower_for_integer(ctx, kind.word(), bound);
            (
                *kind,
                self.hidden_for_operand(ctx, binding, "bound", value, statements),
            )
        });
        let step = match &clause.step {
            Some(step) => {
                let value = self.lower_for_integer(ctx, "by", step);
                self.hidden_for_operand(ctx, binding, "step", value, statements)
            }
            None => Expr::ConstI64 {
                value: 1,
                span: clause.span,
            },
        };
        let local = hidden_local(counter, ValueKind::I64, clause.span);
        let compare =
            |lowering: &mut Self, ctx: &mut FunctionContext, op: ast::CompareOp, bound: &Expr| {
                lowering.lower_compare(ctx, op, local.clone(), bound.clone(), clause.span)
            };
        let test = match &bound {
            None => Expr::ConstBool {
                value: true,
                span: clause.span,
            },
            Some((ast::ForBound::Below, bound)) => compare(self, ctx, ast::CompareOp::Lt, bound),
            Some((ast::ForBound::Above, bound)) => compare(self, ctx, ast::CompareOp::Gt, bound),
            Some((ast::ForBound::To, bound)) => match step {
                Expr::ConstI64 { value, .. } if value < 0 => {
                    compare(self, ctx, ast::CompareOp::Ge, bound)
                }
                Expr::ConstI64 { .. } => compare(self, ctx, ast::CompareOp::Le, bound),
                // A step known only at run time picks the direction then.
                _ => {
                    let zero = Expr::ConstI64 {
                        value: 0,
                        span: clause.span,
                    };
                    let down = self.lower_compare(
                        ctx,
                        ast::CompareOp::Lt,
                        step.clone(),
                        zero,
                        clause.span,
                    );
                    Expr::If {
                        condition: Box::new(down),
                        refinement: None,
                        then_branch: Box::new(compare(self, ctx, ast::CompareOp::Ge, bound)),
                        else_branch: Box::new(compare(self, ctx, ast::CompareOp::Le, bound)),
                        span: clause.span,
                    }
                }
            },
        };
        ForPlan {
            test,
            each: Vec::new(),
            value: hidden_for_name(binding, "counter"),
            checked: false,
            step: Expr::AssignLocal {
                local: counter,
                result_kind: ValueKind::I64,
                callable_signature: None,
                value: Box::new(Expr::Binary {
                    site: None,
                    op: BinaryOp::Add,
                    kind: NumericKind::Integer,
                    lhs: Box::new(local),
                    rhs: Box::new(step),
                    span: clause.span,
                }),
                span: clause.span,
            },
            state: counter,
        }
    }

    /// Plans a collection clause, `name in collection`, by the collection's
    /// static class: a vector, table or string the loop reads directly, and
    /// any other collection through the iteration protocol it answers to
    /// `forward-iteration-protocol`. A vector's or table's elements are
    /// `<object>`s, which a variable annotated with another type checks with
    /// `as`; a string's are `<character>`s, checked the same way only under
    /// an annotation a character may not satisfy. Reading these collections
    /// directly is sound because no program can override their kernel
    /// `forward-iteration-protocol` methods.
    fn plan_collection_for(
        &mut self,
        ctx: &mut FunctionContext,
        clause: &ast::CollectionFor,
        statements: &mut Vec<Statement>,
    ) -> ForPlan {
        let binding = &clause.binding;
        let span = clause.span;
        let collection = self.lower_expr(ctx, &clause.collection);
        let kind = self.expr_kind(&collection);
        let collection_local =
            self.hidden_for_local(ctx, binding, "collection", kind, collection, statements);
        let c = hidden_local(collection_local, kind, span);
        let int = |value| Expr::ConstI64 { value, span };
        let call = |lowering: &Self, intrinsic, args| Expr::IntrinsicCall {
            intrinsic,
            args,
            result_kind: lowering.intrinsic_result_kind(intrinsic),
            span,
        };
        let element = |lowering: &Self, ctx: &mut FunctionContext| {
            let name = hidden_for_name(binding, "element");
            ctx.add_local(
                lowering.local_definition(binding.definition, &name),
                name,
                ValueKind::Any,
                None,
                span,
            )
        };
        match self.collection_kind(kind) {
            Some(CollectionKind::Vector) => {
                let index = self.hidden_for_local(
                    ctx,
                    binding,
                    "index",
                    ValueKind::I64,
                    int(0),
                    statements,
                );
                let i = hidden_local(index, ValueKind::I64, span);
                let size = call(self, Intrinsic::CollectionSize, vec![c.clone()]);
                let test = self.lower_compare(ctx, ast::CompareOp::Lt, i.clone(), size, span);
                let read = call(self, Intrinsic::VectorElement, vec![c, i.clone()]);
                let local = element(self, ctx);
                ForPlan {
                    test,
                    each: vec![hidden_let(local, read, span)],
                    value: hidden_for_name(binding, "element"),
                    checked: true,
                    step: hidden_increment(index, i, int(1), span),
                    state: index,
                }
            }
            Some(CollectionKind::Table) => {
                let version_value = call(self, Intrinsic::TableVersion, vec![c.clone()]);
                let version = self.hidden_for_local(
                    ctx,
                    binding,
                    "version",
                    ValueKind::I64,
                    version_value,
                    statements,
                );
                let v = hidden_local(version, ValueKind::I64, span);
                let limit_value = call(self, Intrinsic::TablePositions, vec![c.clone()]);
                let limit = self.hidden_for_local(
                    ctx,
                    binding,
                    "limit",
                    ValueKind::I64,
                    limit_value,
                    statements,
                );
                let first = call(
                    self,
                    Intrinsic::TableNextPosition,
                    vec![c.clone(), int(0), v.clone()],
                );
                let position = self.hidden_for_local(
                    ctx,
                    binding,
                    "position",
                    ValueKind::I64,
                    first,
                    statements,
                );
                let p = hidden_local(position, ValueKind::I64, span);
                let test = self.lower_compare(
                    ctx,
                    ast::CompareOp::Lt,
                    p.clone(),
                    hidden_local(limit, ValueKind::I64, span),
                    span,
                );
                let read = call(
                    self,
                    Intrinsic::TablePositionValue,
                    vec![c.clone(), p.clone(), v.clone()],
                );
                let local = element(self, ctx);
                let after = Expr::Binary {
                    site: None,
                    op: BinaryOp::Add,
                    kind: NumericKind::Integer,
                    lhs: Box::new(p),
                    rhs: Box::new(int(1)),
                    span,
                };
                let next = call(self, Intrinsic::TableNextPosition, vec![c, after, v]);
                ForPlan {
                    test,
                    each: vec![hidden_let(local, read, span)],
                    value: hidden_for_name(binding, "element"),
                    checked: true,
                    step: Expr::AssignLocal {
                        local: position,
                        result_kind: ValueKind::I64,
                        callable_signature: None,
                        value: Box::new(next),
                        span,
                    },
                    state: position,
                }
            }
            Some(CollectionKind::String) => {
                // The state is a position the loop makes itself, from the
                // start to the end, so each step reads the character there
                // and the position after it with one decode, and nothing can
                // signal: a string's bytes never change.
                let start = call(
                    self,
                    Intrinsic::Text(TextFunction::StartPosition),
                    vec![c.clone()],
                );
                let position = self.hidden_for_local(
                    ctx,
                    binding,
                    "position",
                    ValueKind::Position,
                    start,
                    statements,
                );
                let end_value = call(
                    self,
                    Intrinsic::Text(TextFunction::EndPosition),
                    vec![c.clone()],
                );
                let end = self.hidden_for_local(
                    ctx,
                    binding,
                    "end",
                    ValueKind::Position,
                    end_value,
                    statements,
                );
                let p = hidden_local(position, ValueKind::Position, span);
                let test = call(
                    self,
                    Intrinsic::PositionCompare(CompareOp::Lt),
                    vec![p.clone(), hidden_local(end, ValueKind::Position, span)],
                );
                let read = call(self, Intrinsic::StringStep, vec![c, p]);
                // The variable is a `<character>`, unless an annotation a
                // character may not satisfy asks for a check: then it is the
                // `<object>` the protocol would give, cast with `as` as a
                // vector's element is, so a mismatch signals as it would.
                // The binding's own `let` reports an annotation that does not
                // resolve, so its diagnostics are not repeated here.
                let unchecked = binding.type_annotation.as_ref().is_none_or(|annotation| {
                    let resolved = self.resolve_type_expr(annotation, &mut Vec::new());
                    resolved.range.is_none()
                        && matches!(resolved.kind, ValueKind::Char | ValueKind::Any)
                });
                let name = hidden_for_name(binding, "element");
                let local = ctx.add_local(
                    self.local_definition(binding.definition, &name),
                    name,
                    if unchecked {
                        ValueKind::Char
                    } else {
                        ValueKind::Any
                    },
                    None,
                    span,
                );
                ForPlan {
                    test,
                    each: vec![hidden_let(local, read, span)],
                    value: hidden_for_name(binding, "element"),
                    checked: !unchecked,
                    step: no_values(span),
                    state: position,
                }
            }
            None => self.plan_protocol_for(ctx, clause, c, statements),
        }
    }

    /// Plans a collection clause over a collection whose class is not a
    /// `<vector>` or `<table>` statically: one `forward-iteration-protocol`
    /// send before the loop, then calls through the callables it answers:
    /// `finished-state?` before each iteration, `current-element` to bind
    /// the variable, and `next-state` after the body.
    fn plan_protocol_for(
        &mut self,
        ctx: &mut FunctionContext,
        clause: &ast::CollectionFor,
        collection: Expr,
        statements: &mut Vec<Statement>,
    ) -> ForPlan {
        let binding = &clause.binding;
        let span = clause.span;
        let id = clause.collection.id();
        let function = CollectionFunction::ForwardIterationProtocol;
        let generic = self.collection_generic(function);
        let Some(symbol) = self
            .generic_symbols
            .iter()
            .find(|symbol| symbol.id == generic)
            .cloned()
        else {
            return ForPlan::error(binding, span);
        };
        let send = self.lower_generic_send(ctx, &symbol, vec![collection], function.name(), span);
        let (kinds, sigs) = self.collection_results(function);
        let roles = [
            "state", "limit", "next", "finished", "key", "current", "setter",
        ];
        let locals = roles
            .iter()
            .zip(kinds.iter().zip(sigs))
            .map(|(role, (kind, sig))| {
                let name = hidden_for_name(binding, role);
                ctx.add_local(
                    self.local_definition(binding.definition, &name),
                    name,
                    *kind,
                    sig,
                    binding.span,
                )
            })
            .collect::<Vec<_>>();
        let state = locals[0];
        statements.push(Statement::Let(Let {
            local: state,
            locals,
            value: send,
            span,
        }));
        ctx.allow_assignment(state);
        let name = |role: &str| ast::Expr::Name {
            id,
            name: hidden_for_name(binding, role),
            span,
        };
        let call = |callee: &str, args: Vec<ast::Expr>| ast::Expr::Call {
            id,
            callee: hidden_for_name(binding, callee),
            args,
            keywords: Vec::new(),
            span,
        };
        let finished = call(
            "finished",
            vec![name("collection"), name("state"), name("limit")],
        );
        let finished = self.lower_expr(ctx, &finished);
        let test = Expr::If {
            condition: Box::new(finished),
            refinement: None,
            then_branch: Box::new(Expr::ConstBool { value: false, span }),
            else_branch: Box::new(Expr::ConstBool { value: true, span }),
            span,
        };
        let current = call("current", vec![name("collection"), name("state")]);
        let read = self.lower_expr(ctx, &current);
        let element_name = hidden_for_name(binding, "element");
        let element = ctx.add_local(
            self.local_definition(binding.definition, &element_name),
            element_name,
            ValueKind::Any,
            None,
            span,
        );
        let next = ast::Expr::Assign {
            id,
            name: hidden_for_name(binding, "state"),
            name_span: span,
            value: Box::new(call("next", vec![name("collection"), name("state")])),
            span,
        };
        let step = self.lower_expr(ctx, &next);
        ForPlan {
            test,
            each: vec![hidden_let(element, read, span)],
            value: hidden_for_name(binding, "element"),
            checked: true,
            step,
            state,
        }
    }

    /// Lowers a numeric `for` clause's `start`, `bound` or `step`, which
    /// must be an `<integer>` (an `<i32>` or `<u32>` widens).
    fn lower_for_integer(
        &mut self,
        ctx: &mut FunctionContext,
        word: &str,
        expr: &ast::Expr,
    ) -> Expr {
        let value = self.lower_expr_expected(ctx, expr, Some(ValueKind::I64));
        let value = self.narrow(value, ValueKind::I64, None);
        let kinds = self.expr_kinds(&value);
        let kind = self.expr_kind(&value);
        let single = kinds.len() == 1 || result_kinds_contain_error(&kinds);
        if !single || !self.kinds_match(ValueKind::I64, kind) {
            let actual = if single {
                self.kind_name(kind)
            } else {
                self.result_kinds_name(&kinds)
            };
            ctx.diagnostics.push(
                Diagnostic::error(expr.span(), DiagnosticCode::ForClauseTypeMismatch)
                    .with_arg(DiagnosticArg::text("word", word))
                    .with_arg(DiagnosticArg::text("expected", "<integer>"))
                    .with_arg(DiagnosticArg::text("actual", actual)),
            );
            return Expr::Error { span: expr.span() };
        }
        match NumericKind::of(kind) {
            Some(from) => promote_operand(value, from, NumericKind::Integer),
            None => value,
        }
    }

    /// Binds `value` to a new hidden local of kind `kind` for the `for`
    /// variable `binding`, evaluated once before the loop.
    fn hidden_for_local(
        &mut self,
        ctx: &mut FunctionContext,
        binding: &ast::LetBinding,
        role: &str,
        kind: ValueKind,
        value: Expr,
        statements: &mut Vec<Statement>,
    ) -> LocalId {
        let name = hidden_for_name(binding, role);
        let span = binding.span;
        let local = ctx.add_local(
            self.local_definition(binding.definition, &name),
            name,
            kind,
            None,
            span,
        );
        statements.push(hidden_let(local, value, span));
        local
    }

    /// A `for` loop's bound or step as the loop reads it: a constant as it
    /// is, anything else from a hidden local evaluated once.
    fn hidden_for_operand(
        &mut self,
        ctx: &mut FunctionContext,
        binding: &ast::LetBinding,
        role: &str,
        value: Expr,
        statements: &mut Vec<Statement>,
    ) -> Expr {
        if matches!(value, Expr::ConstI64 { .. } | Expr::Error { .. }) {
            return value;
        }
        let local = self.hidden_for_local(ctx, binding, role, ValueKind::I64, value, statements);
        hidden_local(local, ValueKind::I64, binding.span)
    }

    fn lower_slot_assign(
        &mut self,
        ctx: &mut FunctionContext,
        receiver: &ast::Expr,
        slot_name: &str,
        value: &ast::Expr,
        span: Span,
    ) -> Expr {
        let receiver = self.lower_expr(ctx, receiver);
        // A receiver of a class without the slot sends to the setter
        // generic, which dispatches on its class, as a getter's read does.
        let setter_name = slot_setter_name(slot_name);
        if !self.declares_slot(self.expr_kind(&receiver), slot_name)
            && self.slot_getter_named(slot_name).is_some()
            && let Some(setter) = self.resolve_generic(&setter_name).cloned()
        {
            let value = self.lower_expr(ctx, value);
            return self.lower_generic_send(
                ctx,
                &setter,
                vec![value, receiver],
                &setter_name,
                span,
            );
        }
        let receiver = self.narrow_receiver(receiver, slot_name);
        let receiver_kind = self.expr_kind(&receiver);
        // A typed callable slot's signature is expected of the value.
        let signature = match receiver_kind {
            ValueKind::Object(class) => self
                .classes
                .get(class.0 as usize)
                .and_then(|class| {
                    class
                        .effective_slots
                        .iter()
                        .find(|slot| slot.name == slot_name || slot.accessor.name == slot_name)
                })
                .and_then(|slot| slot.callable_signature.clone()),
            _ => None,
        };
        let value = self
            .lower_method_against(ctx, value, signature.as_ref())
            .unwrap_or_else(|| self.lower_expr(ctx, value));
        let ValueKind::Object(class_id) = receiver_kind else {
            // A receiver in error was reported where it went wrong.
            if receiver_kind == ValueKind::Error {
                return Expr::Error { span };
            }
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SlotReceiverTypeMismatch)
                    .with_arg(DiagnosticArg::text("accessor", slot_name))
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(receiver_kind))),
            );
            return Expr::Error { span };
        };
        let Some(class) = self.classes.get(class_id.0 as usize) else {
            return Expr::Error { span };
        };
        if matches!(class.origin, ClassOrigin::HostBacked { .. }) {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SlotAssignmentReadOnly)
                    .with_arg(DiagnosticArg::text("class", class.name.clone()))
                    .with_arg(DiagnosticArg::text("slot", slot_name))
                    .with_arg(DiagnosticArg::text("reason", "host-backed")),
            );
            return Expr::Error { span };
        }
        let Some(slot) = class
            .effective_slots
            .iter()
            .find(|slot| slot.name == slot_name || slot.accessor.name == slot_name)
        else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnknownSlot)
                    .with_arg(DiagnosticArg::text("class", class.name.clone()))
                    .with_arg(DiagnosticArg::text("slot", slot_name)),
            );
            return Expr::Error { span };
        };
        if !matches!(slot.origin, SlotOrigin::Native)
            || !matches!(slot.mutability, SlotMutability::Mutable)
        {
            let reason = if matches!(slot.origin, SlotOrigin::HostBacked { .. }) {
                "host-backed"
            } else {
                "constant"
            };
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SlotAssignmentReadOnly)
                    .with_arg(DiagnosticArg::text("class", class.name.clone()))
                    .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                    .with_arg(DiagnosticArg::text("reason", reason)),
            );
            return Expr::Error { span };
        }
        let value = adapt_literal(value, Some(core::slice::from_ref(&slot.kind)));
        let value = self.narrow(value, slot.kind, slot.callable_signature.as_ref());
        let value_kind = self.expr_kind(&value);
        if !self.kinds_match(slot.kind, value_kind) {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SlotAssignmentTypeMismatch)
                    .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                    .with_arg(DiagnosticArg::text("expected", self.kind_name(slot.kind)))
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(value_kind))),
            );
        } else if let Some(expected) = &slot.callable_signature {
            ctx.diagnostics.extend(self.callable_value_diagnostic(
                span,
                expected,
                value_kind,
                self.expr_callable_signature(&value).as_ref(),
                |expected, actual| {
                    Diagnostic::error(span, DiagnosticCode::SlotAssignmentTypeMismatch)
                        .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", expected))
                        .with_arg(DiagnosticArg::text("actual", actual))
                },
            ));
        }
        let setter_name = slot_setter_name(&slot.name);
        let Some(setter) = self.resolve_generic(&setter_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedGeneric)
                    .with_arg(DiagnosticArg::text("generic", setter_name)),
            );
            return Expr::Error { span };
        };
        // The send checks the new value against a limited slot's range.
        let value = self.limit_value(value, slot.kind, slot.range);
        Expr::GenericCall {
            site: SiteId::PENDING,
            generic: setter.id,
            args: vec![value, receiver],
            span,
        }
    }

    /// Lowers a read of the slot getter `accessor_name` of `receiver`,
    /// `receiver.name` or `name(receiver)`. A receiver of a class with the
    /// slot reads it directly, since a getter is sealed over its slot's
    /// classes; any other receiver sends to the getter generic, which
    /// dispatches on its class (and caches the method it finds). A getter
    /// whose name a definition of the program takes has no generic, so its
    /// receiver must be of a class with the slot, or narrow to the one
    /// class declaring it.
    fn lower_accessor_call(
        &mut self,
        ctx: &mut FunctionContext,
        receiver: Expr,
        accessor_name: &str,
        span: Span,
    ) -> Expr {
        if !self.declares_slot(self.expr_kind(&receiver), accessor_name)
            && let Some(generic) = self.slot_getter_named(accessor_name)
            && let Some(symbol) = self.generic_symbols.get(generic.0 as usize).cloned()
        {
            return self.lower_generic_send(ctx, &symbol, vec![receiver], accessor_name, span);
        }
        let receiver = self.narrow_receiver(receiver, accessor_name);
        let Some(class_id) = self.value_kind_class(self.expr_kind(&receiver)) else {
            // A receiver in error was reported where it went wrong.
            if self.expr_kind(&receiver) == ValueKind::Error {
                return Expr::Error { span };
            }
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SlotReceiverTypeMismatch)
                    .with_arg(DiagnosticArg::text("accessor", accessor_name))
                    .with_arg(DiagnosticArg::text(
                        "actual",
                        self.kind_name(self.expr_kind(&receiver)),
                    )),
            );
            return Expr::Error { span };
        };
        let Some(class) = self.classes.get(class_id.0 as usize) else {
            return Expr::Error { span };
        };
        let Some(slot) = class
            .effective_slots
            .iter()
            .find(|slot| slot.accessor.name == accessor_name)
        else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnknownSlot)
                    .with_arg(DiagnosticArg::text("class", class.name.clone()))
                    .with_arg(DiagnosticArg::text("slot", accessor_name)),
            );
            return Expr::Error { span };
        };
        Expr::AccessorCall {
            accessor: slot.accessor.definition,
            receiver: Box::new(receiver),
            class: class_id,
            slot: slot.id,
            result_kind: slot.accessor.result_kind,
            span,
        }
    }

    /// The narrower kind a local tested by `condition` has in the `if`'s
    /// true branch, if any.
    ///
    /// An object class narrows the local to that class. A concrete scalar
    /// class narrows a boxed `<object>` local to the scalar's unboxed kind:
    /// `<integer>` to `i64`, `<double-float>` to `f64`, `<single-float>` to
    /// `f32`. Every value that passes `instance?(x, <integer>)` is an `i64`,
    /// because the runtime has no other `<integer>` representation: a boxed
    /// `<i32>` or `<u32>` is the `<integer>` it denotes, and there are no
    /// big integers yet (overflow signals where they would begin). When they
    /// arrive, this narrowing must stop at a range that fits `i64`.
    ///
    /// A limited integer test, such as `instance?(x, <i32>)`, narrows to
    /// `i64` as well, not to the limited kind: its members are `<integer>`s,
    /// so `x + 1` must keep computing as an `<integer>` (as dispatch on the
    /// boxed value does) rather than trap at the limited type's bound. Flow
    /// typing knows the range and drops the overflow checks it rules out.
    /// Abstract numeric classes have no single representation and do not
    /// narrow.
    fn true_branch_refinement(&self, condition: &Expr) -> Option<Refinement> {
        let (value, class) = match condition {
            Expr::InstanceOf { value, class, .. } => (value, Some(*class)),
            Expr::InstanceOfLimited { value, .. } => (value, None),
            Expr::Identical { lhs, rhs, .. } => return identical_refinement(lhs, rhs),
            // `a or b`, as a `select` arm with several keys tests: it refines
            // where both sides refine alike.
            Expr::If {
                condition,
                then_branch,
                else_branch,
                ..
            } if matches!(then_branch.as_ref(), Expr::ConstBool { value: true, .. }) => {
                let refinement = self.true_branch_refinement(condition)?;
                return (self.true_branch_refinement(else_branch) == Some(refinement))
                    .then_some(refinement);
            }
            _ => return None,
        };
        let Expr::Local {
            local, result_kind, ..
        } = value.as_ref()
        else {
            return None;
        };
        // Only a boxed value unboxes; a local of any other kind either
        // already has the scalar's kind or can never pass the test.
        let unboxed = |kind: ValueKind| {
            (*result_kind == ValueKind::Any).then_some(Refinement {
                local: *local,
                kind,
            })
        };
        let class = match class {
            None => return unboxed(ValueKind::I64),
            Some(class) if is_unboxed_value_class_in(&self.classes, class) => {
                return self.kind_for_builtin_class(class).and_then(unboxed);
            }
            Some(class) => class,
        };
        let kind = if let ValueKind::Object(actual) = result_kind
            && self.is_same_or_subclass(*actual, class)
        {
            ValueKind::Object(*actual)
        } else {
            ValueKind::Object(class)
        };
        Some(Refinement {
            local: *local,
            kind,
        })
    }

    fn expr_kind(&self, expr: &Expr) -> ValueKind {
        match expr {
            Expr::ConstI64 { .. } | Expr::CheckRange { .. } => ValueKind::I64,
            Expr::Binary { kind, .. } | Expr::Negate { kind, .. } => kind.value_kind(),
            Expr::Convert { to, .. } => to.value_kind(),
            Expr::FloatCall { builtin, args, .. } => {
                float_call_kind(*builtin, args, |arg| self.expr_kind(arg))
            }
            Expr::ConstF64 { .. } => ValueKind::F64,
            Expr::ConstChar { .. } => ValueKind::Char,
            Expr::Const32 { value, .. } => number32_kind(*value),
            Expr::StringLiteral { class, .. } => ValueKind::String(*class),
            Expr::SymbolLiteral { class, .. } => ValueKind::Symbol(*class),
            Expr::VectorLiteral { class, .. } => ValueKind::Vector(*class),
            Expr::Values { result_kinds, .. } => {
                if result_kinds.len() == 1 {
                    result_kinds[0]
                } else {
                    ValueKind::Error
                }
            }
            Expr::GenericCall { generic, .. } => self
                .generic_symbols
                .get(generic.0 as usize)
                .map_or(ValueKind::Error, |symbol| {
                    scalar_result_kind(&symbol.return_kinds)
                }),
            Expr::CallableCall { result_kinds, .. } => scalar_result_kind(result_kinds),
            Expr::ClassObject { class, .. } | Expr::ClassOf { class, .. } => {
                ValueKind::ClassObject(*class)
            }
            Expr::TypeObject { class, .. } => ValueKind::Object(*class),
            Expr::KnownCallable { callable, .. } => ValueKind::KnownCallable(*callable),
            Expr::Closure { result_kind, .. } => *result_kind,
            Expr::GenericFunctionObject { generic, .. } => {
                ValueKind::GenericFunctionObject(*generic)
            }
            Expr::IntrinsicCall { result_kind, .. } => *result_kind,
            Expr::HostCall { result_kind, .. } => *result_kind,
            Expr::AccessorCall { result_kind, .. } => *result_kind,
            Expr::SlotWrite { result_kind, .. }
            | Expr::AssignLocal { result_kind, .. }
            | Expr::AssignCapture { result_kind, .. } => *result_kind,
            Expr::Loop { .. } => scalar_result_kind(&[]),
            Expr::ConstBool { .. }
            | Expr::Compare { .. }
            | Expr::MixedCompare { .. }
            | Expr::ValueEqual { .. }
            | Expr::Identical { .. }
            | Expr::InstanceOf { .. }
            | Expr::InstanceOfLimited { .. } => ValueKind::Bool,
            Expr::Local { result_kind, .. } => *result_kind,
            Expr::Capture { result_kind, .. } => *result_kind,
            Expr::ConstantRef { result_kind, .. } | Expr::SlotDefault { result_kind, .. } => {
                *result_kind
            }
            Expr::Cast { class, .. } if *class == OBJECT_CLASS_ID => ValueKind::Any,
            Expr::Cast { class, .. } => self
                .kind_for_builtin_class(*class)
                .unwrap_or(ValueKind::Object(*class)),
            Expr::Signal { result_kinds, .. } => scalar_result_kind(result_kinds),
            Expr::DirectCall { function, .. } => self
                .functions
                .get(function.0 as usize)
                .map_or(ValueKind::Error, |symbol| {
                    scalar_result_kind(&symbol.return_kinds)
                }),
            Expr::MakeObject { class_id, .. } => ValueKind::Object(*class_id),
            Expr::MakeRuntimeClass { bound, .. } => ValueKind::Object(*bound),
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => join_kind(
                &self.classes,
                self.expr_kind(then_branch),
                self.expr_kind(else_branch),
                |expected, actual| self.kinds_match(expected, actual),
            )
            .unwrap_or(ValueKind::Error),
            Expr::Handle { restart, .. } => scalar_result_kind(&restart.result_kinds),
            Expr::Block { .. } => scalar_result_kind(&self.expr_kinds(expr)),
            Expr::Discard { .. } | Expr::Error { .. } => ValueKind::Error,
        }
    }

    fn expr_kinds(&self, expr: &Expr) -> Vec<ValueKind> {
        match expr {
            Expr::Values { result_kinds, .. } => result_kinds.clone(),
            Expr::CallableCall { result_kinds, .. } => result_kinds.clone(),
            Expr::HostCall { result_kind, .. } => vec![*result_kind],
            Expr::GenericCall { generic, .. } => {
                self.generic_symbols.get(generic.0 as usize).map_or_else(
                    || vec![ValueKind::Error],
                    |symbol| symbol.return_kinds.clone(),
                )
            }
            Expr::DirectCall { function, .. } => {
                self.functions.get(function.0 as usize).map_or_else(
                    || vec![ValueKind::Error],
                    |symbol| symbol.return_kinds.clone(),
                )
            }
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => self
                .join_arm_kinds(&self.expr_kinds(then_branch), &self.expr_kinds(else_branch))
                .unwrap_or_else(|| vec![ValueKind::Error]),
            Expr::Signal { result_kinds, .. } => result_kinds.clone(),
            Expr::Handle { restart, .. } => restart.result_kinds.clone(),
            Expr::Discard { .. } => Vec::new(),
            // Values leaving through the exit join the body's own results.
            Expr::Block {
                exit: Some(exit),
                body,
                ..
            } => self
                .join_arm_kinds(&self.expr_kinds(&body.result), &exit.result_kinds)
                .unwrap_or_else(|| vec![ValueKind::Error]),
            Expr::Block { body, .. } => self.expr_kinds(&body.result),
            Expr::Loop { .. } => Vec::new(),
            _ => vec![self.expr_kind(expr)],
        }
    }

    fn expr_callable_signature(&self, expr: &Expr) -> Option<CallableSignature> {
        match expr {
            Expr::Local {
                callable_signature, ..
            }
            | Expr::Capture {
                callable_signature, ..
            }
            | Expr::ConstantRef {
                callable_signature, ..
            }
            | Expr::SlotDefault {
                callable_signature, ..
            }
            | Expr::AssignLocal {
                callable_signature, ..
            }
            | Expr::AssignCapture {
                callable_signature, ..
            } => callable_signature.clone(),
            Expr::Closure {
                callable_signature, ..
            } => Some(callable_signature.clone()),
            Expr::KnownCallable { callable, .. } => self.known_callable_signature(*callable),
            Expr::AccessorCall { class, slot, .. } => self
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .and_then(|slot| slot.callable_signature.clone()),
            Expr::CallableCall { .. }
            | Expr::Signal { .. }
            | Expr::DirectCall { .. }
            | Expr::GenericCall { .. }
            | Expr::If { .. }
            | Expr::Handle { .. }
            | Expr::Block { .. } => {
                match self.expr_result_callable_signatures(expr).as_mut_slice() {
                    [signature] => signature.take(),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn expr_result_callable_signatures(&self, expr: &Expr) -> Vec<Option<CallableSignature>> {
        match expr {
            Expr::Values { values, .. } => values
                .iter()
                .map(|value| self.expr_callable_signature(value))
                .collect(),
            Expr::CallableCall {
                result_callable_sigs,
                ..
            } => result_callable_sigs.clone(),
            Expr::Signal {
                result_callable_sigs,
                ..
            } => result_callable_sigs.clone(),
            Expr::DirectCall { function, .. } => self
                .functions
                .get(function.0 as usize)
                .map_or_else(|| vec![None], |symbol| symbol.return_callable_sigs.clone()),
            Expr::GenericCall { generic, .. } => self
                .generic_symbols
                .get(generic.0 as usize)
                .map_or_else(|| vec![None], |symbol| symbol.return_callable_sigs.clone()),
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => {
                let then_signatures = self.expr_result_callable_signatures(then_branch);
                if then_signatures == self.expr_result_callable_signatures(else_branch) {
                    then_signatures
                } else {
                    vec![None; self.expr_kinds(expr).len()]
                }
            }
            // A join keeps a callable's signature only when every way out
            // agrees on it: otherwise an untyped callable could reach a typed
            // binding through the other.
            Expr::Handle { body, restart, .. } => {
                let body_signatures = self.expr_result_callable_signatures(body);
                if body_signatures == restart.result_callable_sigs {
                    body_signatures
                } else {
                    vec![None; self.expr_kinds(expr).len()]
                }
            }
            // A block's exit procedure takes untyped callables, so a block
            // with one produces untyped callables.
            Expr::Block {
                exit: Some(exit), ..
            } => vec![None; exit.result_kinds.len()],
            Expr::Block { body, .. } => self.expr_result_callable_signatures(&body.result),
            Expr::Discard { .. } => Vec::new(),
            Expr::Loop { .. } => Vec::new(),
            _ => vec![self.expr_callable_signature(expr)],
        }
    }

    fn resolve_class(&self, name: &str) -> Option<&Class> {
        if BuiltinValueClass::from_name(name).is_some()
            && let Some(class) = self.classes.iter().find(|class| {
                class.name == name && matches!(class.origin, ClassOrigin::BuiltinValue { .. })
            })
        {
            return Some(class);
        }
        self.classes.iter().find(|class| class.name == name)
    }

    fn builtin_value_class(&self, kind: BuiltinValueClass) -> Option<ClassId> {
        self.classes
            .iter()
            .find(|class| class.origin == ClassOrigin::BuiltinValue { kind })
            .map(|class| class.id)
            .or_else(|| {
                let source_count = self.classes.len();
                let host_count = self.host_registry.classes.len();
                let index = source_count
                    .checked_add(host_count)?
                    .checked_add(kind.index())?;
                Some(ClassId(u32::try_from(index).ok()?))
            })
    }

    fn kind_for_builtin_class(&self, class: ClassId) -> Option<ValueKind> {
        match self.classes.get(class.0 as usize)?.origin {
            ClassOrigin::BuiltinValue { kind } => builtin_value_kind(kind, class),
            ClassOrigin::ObjectRoot
            | ClassOrigin::Native
            | ClassOrigin::Condition { .. }
            | ClassOrigin::HostBacked { .. } => None,
        }
    }

    fn value_kind_class(&self, kind: ValueKind) -> Option<ClassId> {
        match kind {
            ValueKind::String(class) | ValueKind::Symbol(class) | ValueKind::Vector(class) => {
                Some(class)
            }
            // `<i32>` and `<u32>` are limited `<integer>` types, not classes.
            ValueKind::I64 | ValueKind::I32 | ValueKind::U32 => {
                self.builtin_value_class(BuiltinValueClass::Integer)
            }
            ValueKind::F64 => self.builtin_value_class(BuiltinValueClass::DoubleFloat),
            ValueKind::F32 => self.builtin_value_class(BuiltinValueClass::SingleFloat),
            ValueKind::Object(class) => Some(class),
            ValueKind::ClassObject(_) => self.builtin_value_class(BuiltinValueClass::Class),
            ValueKind::GenericFunctionObject(_) => {
                self.builtin_value_class(BuiltinValueClass::GenericFunction)
            }
            ValueKind::MethodObject(_) => self.builtin_value_class(BuiltinValueClass::Method),
            ValueKind::SlotDescriptorObject { .. } => {
                self.builtin_value_class(BuiltinValueClass::SlotDescriptor)
            }
            ValueKind::Bool => self.builtin_value_class(BuiltinValueClass::Boolean),
            ValueKind::Char => self.builtin_value_class(BuiltinValueClass::Character),
            ValueKind::Position => self.builtin_value_class(BuiltinValueClass::StringPosition),
            ValueKind::Callable | ValueKind::KnownCallable(_) => {
                self.builtin_value_class(BuiltinValueClass::Function)
            }
            ValueKind::Any | ValueKind::Error => None,
        }
    }

    fn has_accessor(&self, name: &str) -> bool {
        if self.kernel_scope {
            return false;
        }
        self.classes.iter().any(|class| {
            class
                .direct_slots
                .iter()
                .any(|slot| slot.accessor.name == name)
        })
    }

    fn resolve_function_type_detail(
        &mut self,
        annotation: Option<&ast::TypeExpr>,
        default: ValueKind,
    ) -> ResolvedType {
        annotation.map_or(
            ResolvedType {
                kind: default,
                callable_signature: None,
                range: None,
            },
            |annotation| self.resolve_type_annotation_detail(annotation),
        )
    }

    fn resolve_type_annotation_detail(&mut self, annotation: &ast::TypeExpr) -> ResolvedType {
        let mut diagnostics = Vec::new();
        let resolved = self.resolve_type_expr(annotation, &mut diagnostics);
        self.diagnostics.extend(diagnostics);
        resolved
    }

    fn resolve_result_annotation_detail(
        &mut self,
        annotation: &ast::ResultAnnotation,
    ) -> ResolvedResults {
        let mut diagnostics = Vec::new();
        let resolved = self.resolve_result_types(&annotation.values, &mut diagnostics);
        self.diagnostics.extend(diagnostics);
        resolved
    }

    /// Resolves a definition's result annotation: its declared kinds
    /// (`None` for an open result, which the body's kinds fill in), and the
    /// callable signatures and limited ranges beside them.
    fn resolve_declared_results(
        &mut self,
        annotation: Option<&ast::ResultAnnotation>,
    ) -> DeclaredResults {
        match annotation {
            Some(annotation) => {
                let resolved = self.resolve_result_annotation_detail(annotation);
                DeclaredResults {
                    kinds: Some(resolved.kinds),
                    callable_sigs: resolved.callable_sigs,
                    ranges: resolved.ranges,
                }
            }
            None => {
                let open = provisional_open_result_kinds().len();
                DeclaredResults {
                    kinds: None,
                    callable_sigs: vec![None; open],
                    ranges: vec![None; open],
                }
            }
        }
    }

    fn resolve_type_annotation_detail_in_context(
        &self,
        ctx: &mut FunctionContext,
        annotation: &ast::TypeExpr,
    ) -> ResolvedType {
        self.resolve_type_expr(annotation, &mut ctx.diagnostics)
    }

    /// Resolves a source type to the value kind sem gives it, the signature
    /// of a callable type, and the range of a limited integer type,
    /// reporting what does not resolve into `diagnostics`.
    fn resolve_type_expr(
        &self,
        annotation: &ast::TypeExpr,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> ResolvedType {
        let plain = |kind| ResolvedType {
            kind,
            callable_signature: None,
            range: None,
        };
        match annotation {
            ast::TypeExpr::Named { name, span } => {
                plain(self.resolve_type_name_with_sink(name, false, *span, diagnostics))
            }
            ast::TypeExpr::ClassObject { name, span } => {
                plain(self.resolve_type_name_with_sink(name, true, *span, diagnostics))
            }
            ast::TypeExpr::Callable { signature, .. } => {
                let params = self.resolve_result_types(&signature.params, diagnostics);
                let results = self.resolve_result_types(&signature.results.values, diagnostics);
                for (index, (name, ty)) in signature.keywords.iter().enumerate() {
                    if signature.keywords[..index]
                        .iter()
                        .any(|(earlier, _)| earlier == name)
                    {
                        diagnostics.push(
                            Diagnostic::error(ty.span(), DiagnosticCode::DuplicateKeywordArgument)
                                .with_arg(DiagnosticArg::text("keyword", name.clone())),
                        );
                    }
                }
                let keyword_types = signature
                    .keywords
                    .iter()
                    .map(|(_, ty)| ty.clone())
                    .collect::<Vec<_>>();
                let keyword_kinds = self.resolve_result_types(&keyword_types, diagnostics);
                let keywords = signature
                    .keywords
                    .iter()
                    .zip(keyword_kinds.kinds)
                    .map(|((name, _), kind)| CallableKeyword {
                        name: name.clone(),
                        kind,
                    })
                    .collect();
                ResolvedType {
                    kind: ValueKind::Callable,
                    callable_signature: Some(CallableSignature {
                        params: params.kinds,
                        param_sigs: params.callable_sigs,
                        param_ranges: params.ranges,
                        results: results.kinds,
                        result_sigs: results.callable_sigs,
                        result_ranges: results.ranges,
                        keywords,
                        rest: false,
                    }),
                    range: None,
                }
            }
            ast::TypeExpr::Limited(limited) => match self.resolve_limited(limited, diagnostics) {
                Some(range) => ResolvedType {
                    kind: ValueKind::I64,
                    callable_signature: None,
                    range: (range != IntRange::INTEGER).then_some(range),
                },
                None => plain(ValueKind::Error),
            },
        }
    }

    fn resolve_result_types(
        &self,
        types: &[ast::TypeExpr],
        diagnostics: &mut Vec<Diagnostic>,
    ) -> ResolvedResults {
        let mut resolved = ResolvedResults::default();
        for ty in types {
            let ty = self.resolve_type_expr(ty, diagnostics);
            resolved.kinds.push(ty.kind);
            resolved.callable_sigs.push(ty.callable_signature);
            resolved.ranges.push(ty.range);
        }
        resolved
    }

    /// Resolves `limited(<integer>, min: m, max: n)` to its range, or reports
    /// why it names no limited integer type and returns `None`.
    ///
    /// Only `<integer>` can be limited. `min:` and `max:` may each appear
    /// once, with an `<integer>` literal, and the range must not be empty.
    fn resolve_limited(
        &self,
        limited: &ast::LimitedTypeExpr,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<IntRange> {
        let start = diagnostics.len();
        if limited.base.name() != Some(BuiltinValueClass::Integer.name()) {
            diagnostics.push(
                Diagnostic::error(limited.base.span(), DiagnosticCode::LimitedTypeBase)
                    .with_arg(DiagnosticArg::text("base", limited.base.to_string())),
            );
        }
        let mut range = IntRange::INTEGER;
        for (index, option) in limited.options.iter().enumerate() {
            let repeated = limited.options[..index]
                .iter()
                .any(|earlier| earlier.keyword == option.keyword);
            let bound = match option.keyword.as_str() {
                "min" if !repeated => &mut range.min,
                "max" if !repeated => &mut range.max,
                _ => {
                    diagnostics.push(
                        Diagnostic::error(option.span, DiagnosticCode::LimitedTypeOption)
                            .with_arg(DiagnosticArg::text("option", option.keyword.clone())),
                    );
                    continue;
                }
            };
            match option.value {
                Some(value) => *bound = Some(value),
                None => diagnostics.push(
                    Diagnostic::error(option.span, DiagnosticCode::LimitedTypeBound)
                        .with_arg(DiagnosticArg::text("option", option.keyword.clone()))
                        .with_arg(DiagnosticArg::text("bound", option.text.clone())),
                ),
            }
        }
        if range.is_empty() {
            diagnostics.push(
                Diagnostic::error(limited.span, DiagnosticCode::EmptyLimitedType)
                    .with_arg(DiagnosticArg::text("type", limited_type_name(range))),
            );
        }
        (diagnostics.len() == start).then_some(range)
    }

    /// Resolves a method parameter's specializer and the value kind the
    /// method body sees for it.
    ///
    /// An abstract numeric specializer such as `<real>` covers integers and
    /// floats, which have different unboxed representations. The parameter
    /// keeps the generic's scalar kind when the generic fixes one (a `<real>`
    /// method on a `<double-float>` generic receives an f64); otherwise it
    /// receives the boxed `<object>` value that dispatch already classified.
    ///
    /// A limited type such as `<u32>` or `limited(<integer>, min: 0)` is not
    /// a class, so it can only restate the generic's own parameter type
    /// (or, for `limited(<integer>, ...)`, a wider range); it then
    /// specializes on `<integer>`. Unlike Dylan, methods do not dispatch on
    /// ranges.
    /// Resolves the type of a method's keyword parameter `param`, reporting
    /// one that is not its generic's, of kind `generic_kind` and range
    /// `generic_range`. Its specializer is `<object>`: dispatch does not
    /// look at keywords.
    fn method_keyword_kind(
        &mut self,
        ctx: &mut FunctionContext,
        method: &ast::Method,
        param: &ast::MethodParam,
        generic_kind: Option<ValueKind>,
        generic_range: Option<IntRange>,
    ) -> (MethodSpecializer, ValueKind) {
        let Some(written) = &param.specializer else {
            return (Some(OBJECT_CLASS_ID), ValueKind::Error);
        };
        let resolved = self.resolve_type_annotation_detail(written);
        let expected = generic_kind.unwrap_or(ValueKind::Any);
        if resolved.kind != ValueKind::Error
            && generic_kind != Some(ValueKind::Error)
            && (resolved.kind != expected || resolved.range != generic_range)
        {
            ctx.diagnostics.push(
                Diagnostic::error(written.span(), DiagnosticCode::MethodKeywordTypeMismatch)
                    .with_arg(DiagnosticArg::text("keyword", param.name.clone()))
                    .with_arg(DiagnosticArg::text("generic", method.name.clone()))
                    .with_arg(DiagnosticArg::text(
                        "expected",
                        generic_range.map_or_else(|| self.kind_name(expected), limited_type_name),
                    ))
                    .with_arg(DiagnosticArg::text("actual", written.to_string())),
            );
            return (Some(OBJECT_CLASS_ID), ValueKind::Error);
        }
        (Some(OBJECT_CLASS_ID), resolved.kind)
    }

    fn resolve_method_specializer(
        &mut self,
        param: &ast::MethodParam,
        generic_kind: Option<ValueKind>,
        generic_range: Option<IntRange>,
    ) -> (MethodSpecializer, ValueKind) {
        // Lowering to the AST reported a parameter it found no class for.
        let Some(specializer) = &param.specializer else {
            return (Some(ClassId(u32::MAX)), ValueKind::Error);
        };
        let class_name = match specializer {
            ast::TypeExpr::Named { name, .. } => name.as_str(),
            limited => {
                let resolved = self.resolve_type_annotation_detail(limited);
                if resolved.kind == ValueKind::Error {
                    return (Some(ClassId(u32::MAX)), ValueKind::Error);
                }
                // A method may accept more than its generic's range, as a
                // parameter may widen, but never less: sends check only the
                // generic's range, and dispatch does not select on ranges.
                let widens = match (resolved.range, generic_range) {
                    (None, _) => true,
                    (Some(own), Some(sent)) => own.includes(sent),
                    (Some(_), None) => false,
                };
                if generic_kind != Some(resolved.kind) || !widens {
                    self.diagnostics.push(
                        Diagnostic::error(param.span, DiagnosticCode::LimitedTypeSpecializer)
                            .with_arg(DiagnosticArg::text("type", limited.to_string())),
                    );
                    return (Some(ClassId(u32::MAX)), ValueKind::Error);
                }
                return (
                    self.builtin_value_class(BuiltinValueClass::Integer),
                    resolved.kind,
                );
            }
        };
        if class_name == "<object>" {
            return (Some(OBJECT_CLASS_ID), ValueKind::Any);
        }
        if let Some(limited) = LimitedInteger::from_name(class_name) {
            let kind = limited_integer_kind(limited);
            if generic_kind != Some(kind) {
                self.diagnostics.push(
                    Diagnostic::error(param.span, DiagnosticCode::LimitedTypeSpecializer)
                        .with_arg(DiagnosticArg::text("type", limited.name())),
                );
                return (Some(ClassId(u32::MAX)), ValueKind::Error);
            }
            return (self.builtin_value_class(BuiltinValueClass::Integer), kind);
        }
        let Some(class) = self.resolve_class(class_name) else {
            self.diagnostics.push(
                Diagnostic::error(specializer.span(), DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return (Some(ClassId(u32::MAX)), ValueKind::Error);
        };
        let (class_id, origin) = (class.id, class.origin);
        // A class specializer on a limited-integer parameter would hand the
        // body a `<u32>` it declared as, say, `<integer>`, changing what its
        // arithmetic means; methods restate the limited type instead.
        if let Some(limited @ (ValueKind::I32 | ValueKind::U32)) = generic_kind
            && class_id != OBJECT_CLASS_ID
        {
            self.diagnostics.push(
                Diagnostic::error(param.span, DiagnosticCode::LimitedParameterSpecializer)
                    .with_arg(DiagnosticArg::text("type", self.kind_name(limited))),
            );
            return (Some(class_id), ValueKind::Error);
        }
        // A numeric specializer on a generic that fixes a scalar kind sees
        // that kind: a `<real>` method on a `<double-float>` generic gets the
        // f64.
        if let ClassOrigin::BuiltinValue { kind } = origin
            && kind.is_numeric()
            && let Some(scalar) = generic_kind.filter(|kind| kind.is_number())
        {
            return (Some(class_id), scalar);
        }
        if let ClassOrigin::BuiltinValue { kind } = origin
            && kind.is_numeric()
            && self.kind_for_builtin_class(class_id).is_none()
        {
            return (Some(class_id), ValueKind::Any);
        }
        let kind = self
            .kind_for_builtin_class(class_id)
            .unwrap_or(ValueKind::Object(class_id));
        (Some(class_id), kind)
    }

    fn resolve_type_name(&mut self, type_name: &str, class_object: bool, span: Span) -> ValueKind {
        let mut diagnostics = Vec::new();
        let kind =
            self.resolve_type_name_with_sink(type_name, class_object, span, &mut diagnostics);
        self.diagnostics.extend(diagnostics);
        kind
    }

    fn resolve_type_name_with_sink(
        &self,
        type_name: &str,
        class_object: bool,
        span: Span,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> ValueKind {
        let unresolved = |diagnostics: &mut Vec<Diagnostic>| {
            diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", type_name)),
            );
            ValueKind::Error
        };
        // A kernel class resolves by its kind, not by looking for its class
        // among those defined so far: annotations resolved before the kernel
        // classes are added, such as slot types, still find it.
        let builtin = BuiltinValueClass::from_name(type_name)
            .and_then(|kind| Some((kind, self.builtin_value_class(kind)?)));
        if class_object {
            return builtin
                .map(|(_, class)| class)
                .or_else(|| self.condition_class(ConditionClass::from_name(type_name)?))
                .or_else(|| self.resolve_class(type_name).map(|class| class.id))
                .map_or_else(|| unresolved(diagnostics), ValueKind::ClassObject);
        }
        if let Some((kind, class)) = builtin {
            return builtin_value_kind(kind, class).unwrap_or_else(|| {
                diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::AbstractNumericTypeUnsupported)
                        .with_arg(DiagnosticArg::text("class", type_name)),
                );
                ValueKind::Error
            });
        }
        match type_name {
            "<object>" => ValueKind::Any,
            "<i32>" => ValueKind::I32,
            "<u32>" => ValueKind::U32,
            // `<callable>` names a callable value without a signature, as
            // `<function>`, the class of every one, does.
            "<callable>" => ValueKind::Callable,
            name => self
                .resolve_class(name)
                .map(|class| class.id)
                .or_else(|| self.condition_class(ConditionClass::from_name(name)?))
                .map_or_else(|| unresolved(diagnostics), ValueKind::Object),
        }
    }

    fn kinds_match(&self, expected: ValueKind, actual: ValueKind) -> bool {
        kind_holds(
            expected,
            actual,
            |class, ancestor| self.is_same_or_subclass(class, ancestor),
            |kind| self.value_kind_class(kind),
        )
    }

    /// Joins the result kinds of two control-flow arms; see [`join_arm_kinds`].
    fn join_arm_kinds(&self, lhs: &[ValueKind], rhs: &[ValueKind]) -> Option<Vec<ValueKind>> {
        join_arm_kinds(&self.classes, lhs, rhs, |expected, actual| {
            self.kinds_match(expected, actual)
        })
    }

    fn generic_specializer_matches(
        &self,
        expected: ValueKind,
        specializer: MethodSpecializer,
    ) -> bool {
        let Some(specializer) = specializer else {
            return matches!(expected, ValueKind::Any | ValueKind::Error);
        };
        match expected {
            ValueKind::Any | ValueKind::Error => true,
            ValueKind::Object(class) => self.is_same_or_subclass(specializer, class),
            ValueKind::String(class) | ValueKind::Symbol(class) | ValueKind::Vector(class) => {
                self.is_same_or_subclass(specializer, class)
            }
            // A scalar argument reaches every method specialized on its
            // class or a superclass, such as `<float>` or `<real>` for an
            // f64.
            ValueKind::I64
            | ValueKind::F64
            | ValueKind::I32
            | ValueKind::U32
            | ValueKind::F32
            | ValueKind::Bool
            | ValueKind::Char
            | ValueKind::Position => self
                .value_kind_class(expected)
                .is_some_and(|class| self.is_same_or_subclass(class, specializer)),
            // A callable reaches methods on `<function>` and `<object>`.
            ValueKind::Callable | ValueKind::KnownCallable(_) => self
                .value_kind_class(expected)
                .is_some_and(|class| self.is_same_or_subclass(class, specializer)),
            ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. } => false,
        }
    }

    fn generic_argument_matches(&self, expected: ValueKind, actual: ValueKind) -> bool {
        match expected {
            // A boxed `<object>` is an `<object>` too. A parameter whose
            // methods all specialize on object classes takes runtime
            // objects, so a send unboxes it, and finds no method when it
            // holds a number. A number or callable itself is rejected here:
            // no method can ever take it.
            ValueKind::Any => matches!(
                actual,
                ValueKind::Any
                    | ValueKind::Object(_)
                    | ValueKind::String(_)
                    | ValueKind::Symbol(_)
                    | ValueKind::Vector(_)
                    | ValueKind::ClassObject(_)
                    | ValueKind::GenericFunctionObject(_)
                    | ValueKind::MethodObject(_)
                    | ValueKind::SlotDescriptorObject { .. }
                    | ValueKind::Error
            ),
            ValueKind::Object(expected) => {
                self.value_kind_class(actual)
                    .is_some_and(|actual| self.is_same_or_subclass(actual, expected))
                    || matches!(actual, ValueKind::Error)
            }
            ValueKind::String(expected)
            | ValueKind::Symbol(expected)
            | ValueKind::Vector(expected) => {
                self.value_kind_class(actual)
                    .is_some_and(|actual| self.is_same_or_subclass(actual, expected))
                    || matches!(actual, ValueKind::Error)
            }
            ValueKind::Error => true,
            ValueKind::Callable => matches!(
                actual,
                ValueKind::Callable | ValueKind::KnownCallable(_) | ValueKind::Error
            ),
            ValueKind::I64 => matches!(actual, ValueKind::I64 | ValueKind::I32 | ValueKind::U32),
            ValueKind::F64
            | ValueKind::I32
            | ValueKind::U32
            | ValueKind::F32
            | ValueKind::Bool
            | ValueKind::Char
            | ValueKind::Position
            | ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. }
            | ValueKind::KnownCallable(_) => expected == actual,
        }
    }

    fn is_same_or_subclass(&self, actual: ClassId, expected: ClassId) -> bool {
        is_same_or_subclass_in_classes(&self.classes, actual, expected)
    }

    fn classes_may_overlap(&self, left: ClassId, right: ClassId) -> bool {
        self.is_same_or_subclass(left, right) || self.is_same_or_subclass(right, left)
    }

    fn kind_name(&self, kind: ValueKind) -> String {
        match kind {
            ValueKind::Any => "<object>".into(),
            ValueKind::I64 => "<integer>".into(),
            ValueKind::F64 => "<double-float>".into(),
            ValueKind::I32 => "<i32>".into(),
            ValueKind::U32 => "<u32>".into(),
            ValueKind::F32 => "<single-float>".into(),
            ValueKind::Bool => "<boolean>".into(),
            ValueKind::Char => "<character>".into(),
            ValueKind::Position => "<string-position>".into(),
            ValueKind::Callable => "<callable>".into(),
            ValueKind::String(_) => "<string>".into(),
            ValueKind::Symbol(_) => "<symbol>".into(),
            ValueKind::Vector(_) => "<vector>".into(),
            ValueKind::GenericFunctionObject(generic) => {
                self.generic_symbols.get(generic.0 as usize).map_or_else(
                    || format!("generic-function-object#{}", generic.0),
                    |symbol| format!("generic-function object `{}`", symbol.name),
                )
            }
            ValueKind::MethodObject(method) => self.methods.get(method.0 as usize).map_or_else(
                || format!("method-object#{}", method.0),
                |method| format!("method object m{}", method.id.0),
            ),
            ValueKind::SlotDescriptorObject { class, slot } => self
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or_else(
                    || format!("slot-descriptor#{}.{}", class.0, slot.0),
                    |slot| format!("slot descriptor `{}`", slot.name),
                ),
            ValueKind::Object(class) => self
                .classes
                .get(class.0 as usize)
                .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone()),
            ValueKind::ClassObject(class) => self.classes.get(class.0 as usize).map_or_else(
                || format!("class-object#{}", class.0),
                |class| format!("class object {}", class.name),
            ),
            ValueKind::KnownCallable(callable) => self.callable_name(callable),
            ValueKind::Error => "<error>".into(),
        }
    }

    fn known_callable_signature(&self, callable: KnownCallable) -> Option<CallableSignature> {
        match callable {
            KnownCallable::DirectFunction(function) => {
                self.functions.get(function.0 as usize).map(|function| {
                    let names = self
                        .keyword_functions
                        .iter()
                        .find(|(owner, _)| *owner == function.name)
                        .map(|(_, signature)| keyword_names(signature))
                        .unwrap_or_default();
                    split_keywords(
                        CallableSignature {
                            params: function.param_kinds.clone(),
                            param_sigs: function.param_callable_sigs.clone(),
                            param_ranges: function.param_ranges.clone(),
                            results: function.return_kinds.clone(),
                            result_sigs: function.return_callable_sigs.clone(),
                            result_ranges: function.return_ranges.clone(),
                            keywords: Vec::new(),
                            rest: function.rest,
                        },
                        names,
                    )
                })
            }
            KnownCallable::GenericFunction(generic) => {
                self.generic_symbols.get(generic.0 as usize).map(|generic| {
                    let params = (0..generic.arity())
                        .map(|index| self.generic_callable_arg_kind(generic.id, index))
                        .collect::<Vec<_>>();
                    split_keywords(
                        CallableSignature {
                            param_sigs: vec![None; params.len()],
                            param_ranges: generic.param_ranges.clone(),
                            params,
                            results: generic.return_kinds.clone(),
                            result_sigs: generic.return_callable_sigs.clone(),
                            result_ranges: generic.return_ranges.clone(),
                            keywords: Vec::new(),
                            rest: false,
                        },
                        self.generic_keyword_names(generic),
                    )
                })
            }
        }
    }

    fn callable_signatures_match(
        &self,
        expected: Option<&CallableSignature>,
        actual: Option<&CallableSignature>,
    ) -> bool {
        let Some(expected) = expected else {
            return true;
        };
        let Some(actual) = actual else {
            return false;
        };
        self.callable_signature_mismatch(expected, actual).is_none()
    }

    /// Returns why a callable of signature `actual` cannot be called as
    /// `expected`, or `None` when it can.
    ///
    /// Callable subtyping is [`Contract::check_replaces`]'s rule: each
    /// parameter must accept at least every value callers may pass
    /// (contravariance) and each result must be at most what callers rely
    /// on (covariance). A parameter or result that is itself a typed
    /// callable compares its own signature by the same rule, flipped for a
    /// parameter; an untyped `<callable>` stands in only where no signature
    /// is relied on. On top of that, a callable's ABI fixes its register
    /// kinds and nothing adapts one signature to another, so each pair must
    /// also share a register type: `<i32>` does not stand in for
    /// `<integer>`, nor a class for `<object>`, though every class shares
    /// one.
    fn callable_signature_mismatch(
        &self,
        expected: &CallableSignature,
        actual: &CallableSignature,
    ) -> Option<SignatureMismatch> {
        // A value's typed entry takes its keywords, boxed, after its
        // required parameters, so callers must agree on them exactly.
        // A caller makes a `#rest` parameter's vector itself, so a value
        // taking one never stands in for one that does not, nor the other
        // way round, though their register types agree.
        if expected.params.len() != actual.params.len()
            || expected.results.len() != actual.results.len()
            || expected.keywords != actual.keywords
            || expected.rest != actual.rest
        {
            return Some(SignatureMismatch::Incompatible);
        }
        // Callers pass the expected parameter into the callable's own.
        let params = (0..expected.params.len()).map(|index| {
            let (expected, actual) = (expected.param_value(index), actual.param_value(index));
            (
                ContractPosition::Parameter(index),
                expected,
                actual,
                expected,
                actual,
            )
        });
        // The callable's result flows to callers relying on the expected one.
        let results = (0..expected.results.len()).map(|index| {
            let (expected, actual) = (expected.result_value(index), actual.result_value(index));
            (
                ContractPosition::Result(index),
                expected,
                actual,
                actual,
                expected,
            )
        });
        params
            .chain(results)
            .find_map(|(position, expected, actual, from, into)| {
                // A generic whose methods all specialize a parameter on
                // object classes takes only objects there, never the
                // number or boolean a caller passing any `<object>` may:
                // its parameter is narrower, which source has no name for.
                if matches!(position, ContractPosition::Parameter(_))
                    && expected.kind == ValueKind::Any
                    && expected.signature.is_none()
                    && actual.kind == ValueKind::Object(OBJECT_CLASS_ID)
                {
                    return Some(SignatureMismatch::Variance {
                        position,
                        expected: self.kind_name(ValueKind::Any),
                        actual: "an object that is not a number or boolean".into(),
                    });
                }
                if !shares_register_type(expected.kind, actual.kind) {
                    return Some(SignatureMismatch::Incompatible);
                }
                let variance = || SignatureMismatch::Variance {
                    position,
                    expected: self.signature_value_name(expected),
                    actual: self.signature_value_name(actual),
                };
                if !self.kinds_match(into.kind, from.kind) {
                    return Some(if self.kinds_match(from.kind, into.kind) {
                        variance()
                    } else {
                        SignatureMismatch::Incompatible
                    });
                }
                // A limited integer flows only into a range holding its own.
                let holds = match (into.range, from.range) {
                    (None, _) => true,
                    (Some(into), Some(from)) => into.includes(from),
                    (Some(_), None) => false,
                };
                if into.kind == ValueKind::I64 && !holds {
                    return Some(variance());
                }
                // A value whose callers call it with a signature must have
                // one it can be called as.
                let into_signature = self.signature_value_signature(into)?;
                let Some(from_signature) = self.signature_value_signature(from) else {
                    return Some(variance());
                };
                match self.callable_signature_mismatch(&into_signature, &from_signature)? {
                    SignatureMismatch::Variance { .. } => Some(variance()),
                    SignatureMismatch::Incompatible => Some(SignatureMismatch::Incompatible),
                }
            })
    }

    /// The signature callers of a callable `value` may rely on: its declared
    /// one, or a known callable's own.
    fn signature_value_signature(&self, value: SignatureValue<'_>) -> Option<CallableSignature> {
        match (value.signature, value.kind) {
            (Some(signature), _) => Some(signature.clone()),
            (None, ValueKind::KnownCallable(callable)) => self.known_callable_signature(callable),
            (None, _) => None,
        }
    }

    fn signature_value_name(&self, value: SignatureValue<'_>) -> String {
        match (value.signature, value.range) {
            (Some(signature), _) => self.callable_signature_name(signature),
            (None, Some(range)) if value.kind == ValueKind::I64 => limited_type_name(range),
            (None, _) => self.kind_name(value.kind),
        }
    }

    /// Returns the diagnostic for a callable of signature `actual` that
    /// cannot be called as `expected` because one of its parameters or
    /// results is the wrong way round, or `None` when the mismatch is of
    /// another kind, which the caller reports in its own terms.
    fn callable_variance_diagnostic(
        &self,
        span: Span,
        expected: &CallableSignature,
        actual: Option<&CallableSignature>,
    ) -> Option<Diagnostic> {
        let Some(SignatureMismatch::Variance {
            position,
            expected: expected_name,
            actual: actual_name,
        }) = self.callable_signature_mismatch(expected, actual?)
        else {
            return None;
        };
        let (code, index, names) = match position {
            ContractPosition::Parameter(index) => (
                DiagnosticCode::CallableParameterVariance,
                index,
                [("accepted", actual_name), ("passed", expected_name)],
            ),
            ContractPosition::Result(index) => (
                DiagnosticCode::CallableResultVariance,
                index,
                [("returned", actual_name), ("relied", expected_name)],
            ),
        };
        let mut diagnostic = Diagnostic::error(span, code)
            .with_arg(DiagnosticArg::text(
                "expected",
                self.callable_signature_name(expected),
            ))
            .with_arg(DiagnosticArg::text(
                "actual",
                self.callable_signature_name(actual?),
            ))
            .with_arg(DiagnosticArg::unsigned(
                "index",
                u64::try_from(index + 1).unwrap_or(u64::MAX),
            ));
        for (name, value) in names {
            diagnostic = diagnostic.with_arg(DiagnosticArg::text(name, value));
        }
        Some(diagnostic)
    }

    /// Returns the diagnostic for a value of `actual_kind`, a callable of
    /// signature `actual` when one is known, held where callers call it as
    /// `expected`, or `None` when it can be called that way: a variance
    /// diagnostic when one of its parameters or results is the wrong way
    /// round, otherwise what `mismatch` builds from the expected and actual
    /// types' names.
    fn callable_value_diagnostic(
        &self,
        span: Span,
        expected: &CallableSignature,
        actual_kind: ValueKind,
        actual: Option<&CallableSignature>,
        mismatch: impl FnOnce(String, String) -> Diagnostic,
    ) -> Option<Diagnostic> {
        // A known callable is called with its own signature.
        let known = match (actual, actual_kind) {
            (None, ValueKind::KnownCallable(callable)) => self.known_callable_signature(callable),
            _ => None,
        };
        let actual = actual.or(known.as_ref());
        if matches!(actual_kind, ValueKind::Error)
            || self.callable_signatures_match(Some(expected), actual)
        {
            return None;
        }
        Some(
            self.callable_variance_diagnostic(span, expected, actual)
                .unwrap_or_else(|| {
                    mismatch(
                        self.callable_signature_name(expected),
                        actual.map_or_else(
                            || self.kind_name(actual_kind),
                            |signature| self.callable_signature_name(signature),
                        ),
                    )
                }),
        )
    }

    /// Returns a diagnostic for each typed callable result in `expected`
    /// that the result of `actual_kinds` and `actual_sigs` at the same
    /// position cannot be called as; see [`Self::callable_value_diagnostic`].
    fn callable_results_diagnostics(
        &self,
        span: Span,
        expected: &[Option<CallableSignature>],
        actual_kinds: &[ValueKind],
        actual_sigs: &[Option<CallableSignature>],
        mismatch: impl Fn(String, String) -> Diagnostic,
    ) -> Vec<Diagnostic> {
        expected
            .iter()
            .enumerate()
            .filter_map(|(index, expected)| {
                self.callable_value_diagnostic(
                    span,
                    expected.as_ref()?,
                    actual_kinds.get(index).copied().unwrap_or(ValueKind::Error),
                    actual_sigs.get(index).and_then(Option::as_ref),
                    &mismatch,
                )
            })
            .collect()
    }

    fn callable_signature_name(&self, signature: &CallableSignature) -> String {
        let mut params = (0..signature.params.len())
            .map(|index| {
                let name = self.signature_value_name(signature.param_value(index));
                if signature.rest && index == signature.required() {
                    format!("#rest {name}")
                } else {
                    name
                }
            })
            .collect::<Vec<_>>();
        for (index, keyword) in signature.keywords.iter().enumerate() {
            let prefix = if index == 0 { "#key " } else { "" };
            params.push(format!(
                "{prefix}{} :: {}",
                keyword.name,
                self.kind_name(keyword.kind)
            ));
        }
        let params = params.join(", ");
        let results = (0..signature.results.len())
            .map(|index| self.signature_value_name(signature.result_value(index)))
            .collect::<Vec<_>>();
        let results = match results.as_slice() {
            [] => "0 values".into(),
            [result] => result.clone(),
            _ => format!("{} values ({})", results.len(), results.join(", ")),
        };
        format!("callable({params}) => {results}")
    }

    fn result_kinds_name(&self, kinds: &[ValueKind]) -> String {
        match kinds {
            [] => "0 values".into(),
            [kind] => self.kind_name(*kind),
            _ => {
                let names = kinds
                    .iter()
                    .map(|kind| self.kind_name(*kind))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} values ({names})", kinds.len())
            }
        }
    }

    fn result_kinds_match(&self, expected: &[ValueKind], actual: &[ValueKind]) -> bool {
        expected.len() == actual.len()
            && expected
                .iter()
                .copied()
                .zip(actual.iter().copied())
                .all(|(expected, actual)| self.kinds_match(expected, actual))
    }

    fn method_specializer_label(&self, specializer: MethodSpecializer) -> String {
        specializer.map_or_else(
            || "<object>".into(),
            |class| {
                self.classes
                    .get(class.0 as usize)
                    .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone())
            },
        )
    }

    fn callable_name(&self, callable: KnownCallable) -> String {
        match callable {
            KnownCallable::DirectFunction(function) => {
                self.functions.get(function.0 as usize).map_or_else(
                    || format!("function#{}", function.0),
                    |symbol| format!("function `{}`", symbol.name),
                )
            }
            KnownCallable::GenericFunction(generic) => {
                self.generic_symbols.get(generic.0 as usize).map_or_else(
                    || format!("generic#{}", generic.0),
                    |symbol| format!("generic `{}`", symbol.name),
                )
            }
        }
    }

    /// The program's function named `name`. A built-in's hidden kernel
    /// function has its name too, but no call of the name finds it.
    fn resolve_function(&self, name: &str) -> Option<&FunctionSymbol> {
        if self.kernel_scope {
            return None;
        }
        self.functions[..self.program_functions.min(self.functions.len())]
            .iter()
            .find(|symbol| symbol.name == name)
    }

    fn resolve_host_function(&self, name: &str) -> Option<&ResolvedHostFunction> {
        if self.kernel_scope {
            return None;
        }
        self.host_functions
            .iter()
            .find(|function| function.name == name)
    }

    fn resolve_constant(&self, name: &str) -> Option<&ConstantSymbol> {
        if self.kernel_scope {
            return None;
        }
        self.constant_symbols
            .iter()
            .find(|symbol| symbol.name == name)
    }

    fn generic_param_boxes_scalars(&self, generic: GenericFunctionId, index: usize) -> bool {
        self.generics
            .get(generic.0 as usize)
            .is_some_and(|generic| {
                let methods = self
                    .dispatch_specializers
                    .iter()
                    .map(|(generic, specializers)| (*generic, specializers.as_slice()));
                generic_param_boxes_scalars_in(&self.classes, methods, generic, index)
            })
    }

    /// Returns the diagnostic for `arg`, passed as argument `index` of
    /// `generic` whose parameter is `declared`, when the generic cannot
    /// take it.
    ///
    /// A parameter whose methods all specialize on object classes carries
    /// runtime objects, so a number, boolean or callable, which only a
    /// boxed `<object>` parameter can carry, has no method to reach: the
    /// diagnostic says so and names the classes the methods take.
    fn generic_argument_diagnostic(
        &self,
        arg: &Expr,
        generic: GenericFunctionId,
        index: usize,
        declared: Option<ValueKind>,
    ) -> Option<Diagnostic> {
        let actual = self.expr_kind(arg);
        let unboxed = actual.is_number()
            || matches!(
                actual,
                ValueKind::Bool | ValueKind::Callable | ValueKind::KnownCallable(_)
            );
        if unboxed && self.generic_param_boxes_scalars(generic, index) {
            return None;
        }
        let expected = declared.or_else(|| unboxed.then_some(ValueKind::Any))?;
        if self.generic_argument_matches(expected, actual) {
            return None;
        }
        // A method specializing the argument on a class that did not
        // resolve, reported there, might have taken it.
        if self
            .dispatch_specializers
            .iter()
            .filter(|(method_generic, _)| *method_generic == generic)
            .any(|(_, specializers)| {
                specializers
                    .get(index)
                    .is_some_and(|class| is_unresolved_specializer(*class))
            })
        {
            return None;
        }
        if expected == ValueKind::Any && unboxed {
            return Some(self.no_applicable_method_diagnostic(arg.span(), generic, index, actual));
        }
        let name = self.generic_symbols.get(generic.0 as usize).map_or_else(
            || format!("generic#{}", generic.0),
            |symbol| symbol.name.clone(),
        );
        Some(
            Diagnostic::error(arg.span(), DiagnosticCode::GenericArgumentTypeMismatch)
                .with_arg(DiagnosticArg::text("generic", name))
                .with_arg(DiagnosticArg::unsigned(
                    "argument_index",
                    u64::try_from(index + 1).unwrap_or(u64::MAX),
                ))
                .with_arg(DiagnosticArg::text("expected", self.kind_name(expected)))
                .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
        )
    }

    /// Returns the diagnostic for a number or boolean passed as argument
    /// `index` of `generic` where no method can accept one: every method
    /// specializes that argument on a class it is not an instance of, or
    /// there is no method at all. It names those classes and the class a
    /// method would take the argument on, so the fix (pass an instance of
    /// one, or define such a method) reads off the message.
    fn no_applicable_method_diagnostic(
        &self,
        span: Span,
        generic: GenericFunctionId,
        index: usize,
        actual: ValueKind,
    ) -> Diagnostic {
        let name = self.generic_symbols.get(generic.0 as usize).map_or_else(
            || format!("generic#{}", generic.0),
            |symbol| symbol.name.clone(),
        );
        let mut specializers = Vec::<String>::new();
        for (_, method_specializers) in self
            .dispatch_specializers
            .iter()
            .filter(|(method_generic, _)| *method_generic == generic)
        {
            let label =
                self.method_specializer_label(method_specializers.get(index).copied().flatten());
            if !specializers.contains(&label) {
                specializers.push(label);
            }
        }
        // The class a method would specialize on to take the argument: a
        // number's or boolean's own, else `<object>`.
        let class = self.value_kind_class(actual).or(Some(OBJECT_CLASS_ID));
        let code = if specializers.is_empty() {
            DiagnosticCode::GenericWithoutMethods
        } else {
            DiagnosticCode::NoApplicableMethod
        };
        let diagnostic = Diagnostic::error(span, code)
            .with_arg(DiagnosticArg::text("generic", name))
            .with_arg(DiagnosticArg::unsigned(
                "argument_index",
                u64::try_from(index + 1).unwrap_or(u64::MAX),
            ))
            .with_arg(DiagnosticArg::text("actual", self.kind_name(actual)))
            .with_arg(DiagnosticArg::text(
                "class",
                self.method_specializer_label(class),
            ));
        if specializers.is_empty() {
            diagnostic
        } else {
            diagnostic.with_arg(DiagnosticArg::text("specializers", specializers.join(", ")))
        }
    }

    fn snapshot_dispatch_specializers(&mut self) {
        if !self.methods.is_empty() {
            self.dispatch_specializers = self
                .methods
                .iter()
                .map(|method| (method.generic, method.specializers.clone()))
                .collect();
        }
    }

    /// Resolves a source-visible generic by name. Built-in numeric generics
    /// are reached only through operators, never by name.
    fn resolve_generic(&self, name: &str) -> Option<&GenericSymbol> {
        self.generic_symbols.iter().find(|symbol| {
            symbol.name == name
                && !self.numeric_generics.iter().any(|(_, id)| *id == symbol.id)
                && (!self.kernel_scope || self.is_kernel_generic(symbol.id))
        })
    }

    fn local_definition(&self, owner: DefinitionId, name: &str) -> DefinitionId {
        DefinitionId::new(
            self.source_file,
            DefinitionKind::Local,
            name,
            &owner.raw().to_string(),
        )
    }

    fn restart_condition_definition(&self, restart: &ast::Restart, name: &str) -> DefinitionId {
        DefinitionId::new(
            self.source_file,
            DefinitionKind::Local,
            name,
            &restart.id.raw().to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast, grammar_frontend};

    fn lower_source(source: &str) -> Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let ast = ast::lower(&tree);
        lower(&ast)
    }

    /// Check-site ids in `tree`. Every site field follows its operands, so
    /// `Debug` lists them in evaluation order.
    fn sites(tree: &impl core::fmt::Debug) -> Vec<u32> {
        let text = format!("{tree:?}");
        text.split("SiteId(")
            .skip(1)
            .map(|rest| rest[..rest.find(')').expect("id")].parse().expect("id"))
            .collect()
    }

    /// Closures created directly or transitively in `tree`, in order.
    fn closure_ids(tree: &impl core::fmt::Debug) -> Vec<usize> {
        let text = format!("{tree:?}");
        text.split("closure: ClosureId(")
            .skip(1)
            .map(|rest| rest[..rest.find(')').expect("id")].parse().expect("id"))
            .collect()
    }

    const SITES: &str = "define class <shape> (<object>)\nend;\n\
        define generic area(x);\n\
        define method area(s :: <shape>) => <integer>\n  1\nend;\n\
        define function first(s :: <shape>, n :: <integer>) => <integer>\n  \
        let a = area(s) + n;\n  \
        let f :: <callable> = method () => <integer>\n    area(as(<shape>, s)) + 1\n  end;\n  \
        a - area(s)\nend;\n";

    fn function<'a>(module: &'a Module, name: &str) -> &'a Function {
        module
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("function")
    }

    /// Each annotation position, with `{C}` for the class it names, and
    /// whether it names the class itself, as an abstract class may be named,
    /// rather than the type of a value.
    const ANNOTATION_POSITIONS: &[(&str, &str, bool)] = &[
        (
            "parameter",
            "define function f(x :: {C}) => <integer>\n  0\nend;\n",
            false,
        ),
        (
            "result",
            "define function f(x :: <object>) => {C}\n  as({C}, x)\nend;\n",
            false,
        ),
        (
            "let",
            "define function f(x :: <object>) => <integer>\n  \
             let y :: {C} = as({C}, x);\n  0\nend;\n",
            false,
        ),
        (
            "slot",
            "define class <holder> (<object>)\n  slot s :: {C};\nend;\n",
            false,
        ),
        ("generic", "define generic f(x :: {C}) => {C};\n", false),
        (
            "as",
            "define function f(x :: <object>) => <object>\n  as({C}, x)\nend;\n",
            false,
        ),
        (
            "instance?",
            "define function f(x :: <object>) => <boolean>\n  instance?(x, {C})\nend;\n",
            true,
        ),
        (
            "specializer",
            "define generic f(x) => <object>;\n\
             define method f(x :: {C}) => <object>\n  x\nend;\n",
            true,
        ),
        (
            "class parameter",
            "define function f(x :: class {C}) => <integer>\n  0\nend;\n",
            true,
        ),
        (
            "class slot",
            "define class <holder> (<object>)\n  slot s :: class {C};\nend;\n",
            true,
        ),
    ];

    #[test]
    fn every_kernel_class_resolves_in_every_annotation_position() {
        // Every kernel class, built-in or condition, is a type a program can
        // name wherever it names one, except that the abstract numeric
        // classes type no value yet.
        let builtins = BuiltinValueClass::ALL
            .iter()
            .map(|(kind, name)| (*name, kind.is_abstract() && kind.is_numeric()));
        let conditions = ConditionClass::ALL
            .iter()
            .map(|condition| (condition.name(), false));
        let mut failures = Vec::new();
        for (class, abstract_numeric) in builtins.chain(conditions) {
            for (position, template, names_class) in ANNOTATION_POSITIONS {
                if abstract_numeric && !names_class {
                    continue;
                }
                let source = format!(
                    "{}define function main() => <integer>\n  0\nend;\n",
                    template.replace("{C}", class)
                );
                let module = lower_source(&source);
                if !module.diagnostics.is_empty() {
                    failures.push(format!("{class} as {position}: {:?}", module.diagnostics));
                    continue;
                }
                // A slot type resolves before the kernel classes exist, so
                // check it found the class it names, not merely a class.
                let Some(holder) = module.classes.iter().find(|item| item.name == "<holder>")
                else {
                    continue;
                };
                let found = match holder.direct_slots[0].kind {
                    ValueKind::ClassObject(class) => Some(class),
                    kind => module.value_kind_class(kind),
                };
                let found = found.and_then(|id| module.classes.get(id.0 as usize));
                if found.is_none_or(|found| found.name != class) {
                    failures.push(format!(
                        "{class} as {position} resolved to {:?}",
                        found.map(|found| &found.name)
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn check_sites_are_dense_per_owner_and_shared_with_closures() {
        let module = lower_source(&format!(
            "{SITES}define function second(s :: <shape>) => <integer>\n  area(s)\nend;"
        ));
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let first = function(&module, "first");
        // `area(s)` and `+`, then the closure's cast, `area` and `+`, then
        // `area(s)` and `-`.
        assert_eq!(sites(&first.body), [0, 1, 5, 6]);
        let closures = closure_ids(&first.body);
        assert_eq!(closures.len(), 1);
        assert_eq!(sites(&module.closures[closures[0]].body), [2, 3, 4]);
        assert_eq!(sites(&function(&module, "second").body), [0]);
        assert!(sites(&module.methods[0].body).is_empty());
        let Expr::Binary { site, .. } = &first.body.result else {
            panic!("binary result");
        };
        assert_eq!(*site, Some(SiteId(6)));
        assert_eq!(SiteId(6).to_string(), "s6");
    }

    #[test]
    fn indirect_entries_check_parameters_after_the_body_sites() {
        let module = lower_source(&format!(
            "{SITES}define function named(c :: class <shape>, s :: <shape>, n :: <integer>) => <integer>\n  area(s)\nend;\n\
             define function apply-named(c :: class <shape>) => <integer>\n  \
             let f :: <callable> = named;\n  \
             let g :: <callable> = method (k :: class <shape>, o :: <object>) => <integer> 1 end;\n  \
             0\nend;"
        ));
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let shape = class_id(&module, "<shape>");
        // `first` is never a value, so only direct calls enter it.
        assert!(function(&module, "first").entry_checks.is_empty());
        assert!(function(&module, "apply-named").entry_checks.is_empty());
        // `named` is: its parameters are checked after `area(s)`, s0, the
        // `<integer>` too, since it arrives boxed.
        let named = function(&module, "named");
        assert_eq!(
            named.entry_checks,
            [
                EntryCheck {
                    param: named.params[0],
                    target: EntryTarget::ClassObject(shape),
                    site: SiteId(1),
                },
                EntryCheck {
                    param: named.params[1],
                    target: EntryTarget::Instance(shape),
                    site: SiteId(2),
                },
                EntryCheck {
                    param: named.params[2],
                    target: EntryTarget::Unbox(ValueKind::I64),
                    site: SiteId(3),
                },
            ]
        );
        // A closure is only entered indirectly; its checks are sites of the
        // body creating it, and an `<object>` parameter needs none.
        let closures = closure_ids(&function(&module, "apply-named").body);
        let closure = &module.closures[closures[0]];
        assert_eq!(
            closure.entry_checks,
            [EntryCheck {
                param: closure.params[0],
                target: EntryTarget::ClassObject(shape),
                site: SiteId(0),
            }]
        );
    }

    #[test]
    fn check_sites_survive_edits_to_other_definitions() {
        let numbering = |module: &Module| {
            let first = function(module, "first");
            let closures = closure_ids(&first.body)
                .into_iter()
                .map(|closure| sites(&module.closures[closure].body))
                .collect::<Vec<_>>();
            (sites(&first.body), closures)
        };
        let before = lower_source(&format!(
            "{SITES}define function second(s :: <shape>) => <integer>\n  area(s)\nend;"
        ));
        // A definition with checks and a closure is added before `first`,
        // and `second` gains checks.
        let after = lower_source(&format!(
            "define function zeroth(n :: <integer>) => <integer>\n  \
             let g :: <callable> = method () => <integer>\n    n * 2\n  end;\n  \
             -n\nend;\n\
             {SITES}define function second(s :: <shape>) => <integer>\n  \
             area(s) + area(s) * 3\nend;"
        ));
        assert!(after.diagnostics.is_empty(), "{:?}", after.diagnostics);
        assert_eq!(numbering(&before), numbering(&after));
        assert_eq!(sites(&function(&after, "zeroth").body), [1]);
        assert_eq!(sites(&function(&after, "second").body), [0, 1, 2, 3]);
    }

    #[test]
    fn check_sites_are_numbered_after_result_inference() {
        // `helper` has an open result, so its callers are lowered again
        // once its result is inferred.
        let source = "define function helper(n :: <integer>)\n  n + 1\nend;\n\
             define function main() => <integer>\n  helper(2) + helper(3)\nend;";
        let module = lower_source(source);
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        assert_eq!(sites(&function(&module, "helper").body), [0]);
        assert_eq!(sites(&function(&module, "main").body), [0]);
        assert_eq!(module, lower_source(source));
        // Slot initializers and constants are numbered as their own roots.
        let module = lower_source(
            "define constant $k :: <integer> = 1 + 2 * 3;\n\
             define class <box> (<object>)\n  slot size :: <integer>, init-value: 4 - 5;\nend;\n\
             define function main() => <integer>\n  $k + make(<box>).size\nend;",
        );
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        assert_eq!(sites(&module.constants[0].value), [0, 1]);
        let class = module
            .classes
            .iter()
            .find(|class| class.name == "<box>")
            .expect("box");
        assert_eq!(sites(&class.direct_slots[0].init_value), [0]);
        assert_eq!(sites(&class.effective_slots[0].init_value), [0]);
        // The default copied into `make` is numbered with `main`.
        assert_eq!(sites(&function(&module, "main").body), [0, 1]);
    }

    #[test]
    fn kind_type_maps_every_value_kind_to_its_values() {
        use interloko_model::types::{Atom, ClassHierarchy, IntRange, Metaobject, Type};

        let module = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  1\nend;",
        );
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let class = |name: &str| {
            module
                .classes
                .iter()
                .find(|class| class.name == name)
                .map(|class| class.id)
                .expect("class")
        };
        let builtin = |kind| module.builtin_value_class(kind).expect("builtin class");
        let (shape, circle) = (class("<shape>"), class("<circle>"));
        let generic = GenericFunctionId(0);
        let method = module.methods[0].id;
        let slot = SlotId(0);
        let of = |atom| Type::of(atom, &module);

        let kinds = [
            ValueKind::Any,
            ValueKind::I64,
            ValueKind::F64,
            ValueKind::I32,
            ValueKind::U32,
            ValueKind::F32,
            ValueKind::Bool,
            ValueKind::Char,
            ValueKind::Position,
            ValueKind::String(builtin(BuiltinValueClass::String)),
            ValueKind::Symbol(builtin(BuiltinValueClass::Symbol)),
            ValueKind::Vector(builtin(BuiltinValueClass::Vector)),
            ValueKind::Object(shape),
            ValueKind::ClassObject(circle),
            ValueKind::GenericFunctionObject(generic),
            ValueKind::MethodObject(method),
            ValueKind::SlotDescriptorObject {
                class: circle,
                slot,
            },
            ValueKind::Callable,
            ValueKind::KnownCallable(KnownCallable::GenericFunction(generic)),
            ValueKind::Error,
        ];
        for kind in kinds {
            // Exhaustive, so a new kind must be added to the list above.
            let expected = match kind {
                ValueKind::Any | ValueKind::Error => Type::top(),
                ValueKind::I64 => of(Atom::Integer(IntRange::INTEGER)),
                ValueKind::I32 => of(Atom::Integer(IntRange::I32)),
                ValueKind::U32 => of(Atom::Integer(IntRange::U32)),
                ValueKind::F64 => of(Atom::Exact(builtin(BuiltinValueClass::DoubleFloat))),
                ValueKind::F32 => of(Atom::Exact(builtin(BuiltinValueClass::SingleFloat))),
                ValueKind::Bool => {
                    Type::union([Atom::Boolean(true), Atom::Boolean(false)], &module)
                }
                ValueKind::Char => of(Atom::Exact(builtin(BuiltinValueClass::Character))),
                ValueKind::Position => of(Atom::Exact(builtin(BuiltinValueClass::StringPosition))),
                // Built-in value classes are closed, except `<vector>`,
                // which `<stretchy-vector>` extends.
                ValueKind::String(class) | ValueKind::Symbol(class) => of(Atom::Exact(class)),
                ValueKind::Vector(class) => of(Atom::Cone(class)),
                ValueKind::Object(class) => of(Atom::Cone(class)),
                ValueKind::ClassObject(_) => of(Atom::Exact(builtin(BuiltinValueClass::Class))),
                ValueKind::GenericFunctionObject(generic) => {
                    of(Atom::Singleton(Metaobject::Generic(generic)))
                }
                ValueKind::MethodObject(method) => of(Atom::Singleton(Metaobject::Method(method))),
                ValueKind::SlotDescriptorObject { class, slot } => {
                    of(Atom::Singleton(Metaobject::SlotDescriptor { class, slot }))
                }
                ValueKind::Callable | ValueKind::KnownCallable(_) => of(Atom::Callable),
            };
            let ty = module.kind_type(kind);
            assert_eq!(ty, expected, "{kind:?}");
            // Every class-shaped kind is bounded by its sem class.
            if let Some(class) = module.value_kind_class(kind) {
                assert_eq!(ty.class_bound(&module), Some(class), "{kind:?}");
            }
        }
        assert_eq!(
            module.kind_type(ValueKind::Object(OBJECT_CLASS_ID)),
            Type::top()
        );
        assert_eq!(
            module.kind_type(ValueKind::Object(builtin(BuiltinValueClass::Integer))),
            module.kind_type(ValueKind::I64)
        );
        assert!(module.is_closed(builtin(BuiltinValueClass::Integer)));
        assert!(!module.is_closed(builtin(BuiltinValueClass::Real)));
        assert!(!module.is_closed(circle));
        assert_eq!(module.superclass(circle), Some(shape));
        assert_eq!(
            format!(
                "{}",
                module
                    .kind_type(ValueKind::SlotDescriptorObject {
                        class: circle,
                        slot
                    })
                    .display(&module)
            ),
            "singleton(<circle>.radius)"
        );
        assert_eq!(
            format!(
                "{}",
                module
                    .kind_type(ValueKind::MethodObject(method))
                    .display(&module)
            ),
            "singleton(area(<circle>))"
        );
    }

    #[test]
    fn module_reports_recorded_expression_kinds() {
        let module = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\nend;\n\
             define function widen() => <shape>\n  as(<shape>, make(<circle>))\nend;\n\
             define function main()\n  if (1 < 2) 3 else 4 end\nend;",
        );
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let shape = module
            .classes
            .iter()
            .find(|class| class.name == "<shape>")
            .map(|class| class.id)
            .expect("shape class");
        let integer = module
            .builtin_value_class(BuiltinValueClass::Integer)
            .expect("integer class");

        assert_eq!(
            module.expr_kind(&module.functions[0].body.result),
            ValueKind::Object(shape)
        );
        assert_eq!(
            module.expr_kind(&module.functions[1].body.result),
            ValueKind::I64
        );
        assert_eq!(module.value_kind_class(ValueKind::I64), Some(integer));
        assert_eq!(module.builtin_value_kind(integer), Some(ValueKind::I64));
        assert_eq!(module.value_kind_class(ValueKind::Any), None);
        assert_eq!(module.builtin_value_kind(shape), None);
        let double_float = module
            .builtin_value_class(BuiltinValueClass::DoubleFloat)
            .expect("double-float class");
        assert_eq!(module.value_kind_class(ValueKind::F64), Some(double_float));
        assert_eq!(
            module.builtin_value_kind(double_float),
            Some(ValueKind::F64)
        );
    }

    fn lower_source_with_host_registry(source: &str, registry: &HostMetaRegistry) -> Module {
        let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
        let ast = ast::lower(&tree);
        lower_with_host_registry(&ast, registry)
    }

    fn rust_circle_registry() -> HostMetaRegistry {
        let mut registry = HostMetaRegistry::new();
        registry.add_class(HostClass::new("<rust-circle>").with_slot(HostSlot::new(
            "radius",
            ValueKind::I64,
            "<integer>",
        )));
        registry
    }

    fn has_diagnostic(module: &Module, code: DiagnosticCode, args: &[(&str, &str)]) -> bool {
        module.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == code
                && args.iter().all(|(name, value)| {
                    diagnostic
                        .args
                        .iter()
                        .any(|arg| arg.name == *name && arg.value.render() == *value)
                })
        })
    }

    fn class_id(module: &Module, name: &str) -> ClassId {
        class(module, name).id
    }

    fn class<'a>(module: &'a Module, name: &str) -> &'a Class {
        module
            .classes
            .iter()
            .find(|class| class.name == name)
            .unwrap_or_else(|| panic!("missing class {name}"))
    }

    #[test]
    fn object_is_a_real_root_class() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic score(x :: <object>) => <integer>;\n\
             define method score(x :: <object>)\n  1\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].name, "<object>");
        assert_eq!(module.classes[0].superclass, None);
        assert_eq!(module.classes[1].name, "<circle>");
        assert_eq!(module.classes[1].superclass, Some(ClassId(0)));
        assert_eq!(module.methods[0].specializers, [Some(ClassId(0))]);
    }

    #[test]
    fn resolves_locals_and_direct_calls() {
        let module = lower_source(
            "define function add(a, b)\n  a + b\nend;\n\
             define function main()\n  let x = 40;\n  add(x, 2)\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions.len(), 2);
        assert_eq!(module.functions[0].params, [LocalId(0), LocalId(1)]);
        assert_eq!(
            module.functions[1].body.lets().next().expect("a let").local,
            LocalId(0)
        );
        let Expr::DirectCall { function, args, .. } = &module.functions[1].body.result else {
            panic!("expected direct call");
        };
        assert_eq!(*function, FunctionId(0));
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn resolves_top_level_constants_and_reports_bad_initializers() {
        let module = lower_source(
            "define constant $answer :: <integer> = 40 + 2;\n\
             define constant $alias :: <integer> = $answer;\n\
             define function main()\n  $alias\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.constants.len(), 2);
        assert_eq!(module.constants[0].name, "$answer");
        assert_eq!(module.constants[0].result_kind, ValueKind::I64);
        assert!(matches!(
            module.functions[0].body.result,
            Expr::ConstantRef {
                constant: ConstantId(1),
                result_kind: ValueKind::I64,
                ..
            }
        ));

        let duplicate = lower_source(
            "define constant $answer = 1;\n\
             define constant $answer = 2;\n\
             define function main()\n  $answer\nend;",
        );
        assert!(has_diagnostic(
            &duplicate,
            DiagnosticCode::DuplicateDefinition,
            &[("kind", "constant"), ("name", "$answer")]
        ));

        let unresolved = lower_source("define function main()\n  $missing\nend;");
        assert!(has_diagnostic(
            &unresolved,
            DiagnosticCode::UnresolvedLocal,
            &[("local", "$missing")]
        ));

        let invalid = lower_source(
            "define function helper()\n  1\nend;\n\
             define constant $helper = helper;\n\
             define function main()\n  0\nend;",
        );
        assert!(has_diagnostic(
            &invalid,
            DiagnosticCode::ConstantInitializerTypeMismatch,
            &[("constant", "$helper")]
        ));

        let mismatch = lower_source(
            "define constant $answer :: <boolean> = 42;\n\
             define function main()\n  0\nend;",
        );
        assert!(has_diagnostic(
            &mismatch,
            DiagnosticCode::ConstantTypeMismatch,
            &[
                ("constant", "$answer"),
                ("expected", "<boolean>"),
                ("actual", "<integer>")
            ]
        ));
    }

    #[test]
    fn a_string_loop_assigns_its_position_where_it_steps() {
        // The step advances the hidden position in place, so analyses that
        // ask what a loop assigns, such as typing's loop-head widening, see
        // it as assigned.
        let module = lower_source(
            "define function count(s :: <string>) => <integer>\n  \
             let n = 0;\n  for (c in s) n := n + 1 end;\n  n\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let function = &module.functions[0];
        let position = function
            .locals
            .iter()
            .find(|local| local.name == "c#for-position")
            .expect("the loop's position local")
            .id;
        assert!(
            function.body.assigned_locals().contains(&position),
            "{:?}",
            function.body.assigned_locals()
        );
    }

    #[test]
    fn checks_local_type_annotations() {
        let module =
            lower_source("define function main()\n  let answer :: <integer> = 42;\n  answer\nend;");
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions[0].locals[0].kind, ValueKind::I64);

        let mismatch =
            lower_source("define function main()\n  let answer :: <boolean> = 42;\n  answer\nend;");
        assert!(has_diagnostic(
            &mismatch,
            DiagnosticCode::LocalTypeMismatch,
            &[
                ("local", "answer"),
                ("expected", "<boolean>"),
                ("actual", "<integer>")
            ]
        ));
    }

    #[test]
    fn resolves_known_callable_values_and_their_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function inc(x :: <integer>)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  let g = area;\n  (f)(2) + (g)(make(<circle>, radius: 3))\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(
            module.functions[1].locals[0].kind,
            ValueKind::KnownCallable(KnownCallable::DirectFunction(FunctionId(0)))
        );
        assert_eq!(
            module.functions[1].locals[1].kind,
            ValueKind::KnownCallable(KnownCallable::GenericFunction(GenericFunctionId(0)))
        );
        let Expr::Binary { lhs, rhs, .. } = &module.functions[1].body.result else {
            panic!("expected binary result");
        };
        let Expr::CallableCall { callable, .. } = lhs.as_ref() else {
            panic!("expected direct apply to remain a callable call");
        };
        assert!(matches!(
            callable.as_ref(),
            Expr::Local {
                local: LocalId(0),
                result_kind: ValueKind::KnownCallable(KnownCallable::DirectFunction(FunctionId(0))),
                ..
            }
        ));
        let Expr::CallableCall { callable, .. } = rhs.as_ref() else {
            panic!("expected generic apply to remain a callable call");
        };
        assert!(matches!(
            callable.as_ref(),
            Expr::Local {
                local: LocalId(1),
                result_kind: ValueKind::KnownCallable(KnownCallable::GenericFunction(
                    GenericFunctionId(0)
                )),
                ..
            }
        ));
    }

    #[test]
    fn limited_types_written_as_values_are_type_objects() {
        let module = lower_source(
            "define function main() => <boolean>\n  \
             let t = limited(<integer>, min: 0, max: 9);\n  \
             let member = instance?(3, t);\n  \
             t = <u32>\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let limited_integer = class_id(&module, "<limited-integer>");
        let body = &module.functions[0].body;
        assert_eq!(
            body.lets().next().expect("a let").value,
            Expr::TypeObject {
                object: TypeObject::limited(IntRange {
                    min: Some(0),
                    max: Some(9)
                }),
                class: limited_integer,
                span: body.lets().next().expect("a let").value.span(),
            }
        );
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::Object(limited_integer)
        );
        assert!(matches!(
            body.lets().nth(1).expect("a let").value,
            Expr::IntrinsicCall {
                intrinsic: Intrinsic::InstanceOfType,
                result_kind: ValueKind::Bool,
                ..
            }
        ));
        let Expr::ValueEqual { rhs, .. } = &body.result else {
            panic!("expected type objects compared, got {:?}", body.result);
        };
        assert!(matches!(
            rhs.as_ref(),
            Expr::TypeObject {
                object: TypeObject::LimitedInteger {
                    named: Some(LimitedInteger::U32),
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn resolves_generic_callable_metaobject_intrinsic_args() {
        let module = lower_source(
            "define generic area(x);\n\
             define function main() => <string>\n  generic-function-name(area)\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::IntrinsicCall {
            intrinsic, args, ..
        } = &module.functions[0].body.result
        else {
            panic!("expected intrinsic call");
        };
        assert_eq!(*intrinsic, Intrinsic::GenericFunctionName);
        assert!(matches!(
            args.as_slice(),
            [Expr::GenericFunctionObject {
                generic: GenericFunctionId(0),
                ..
            }]
        ));
    }

    #[test]
    fn slot_getters_are_generics_whose_methods_read_the_slots() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <wheel> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let getter = radius;\n  (getter)(make(<circle>, radius: 3))\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let getter = module
            .generics
            .iter()
            .find(|generic| generic.name == "radius")
            .expect("the getter is a generic");
        assert_eq!(
            getter.origin,
            GenericFunctionOrigin::SlotGetter { generic: getter.id }
        );
        assert_eq!(getter.result_kinds, [ValueKind::I64]);
        let readers = module
            .methods
            .iter()
            .filter(|method| method.generic == getter.id)
            .map(|method| method.specializers.clone())
            .collect::<Vec<_>>();
        let (circle, wheel) = (class_id(&module, "<circle>"), class_id(&module, "<wheel>"));
        assert_eq!(readers, [vec![Some(circle)], vec![Some(wheel)]]);
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::KnownCallable(KnownCallable::GenericFunction(getter.id))
        );
        // Sealed over the slots' classes: a method for one would override a
        // getter, while one for another class may join.
        assert_eq!(
            module.slot_getter_overridden(getter.id, &[Some(circle)]),
            Some(circle)
        );
        assert_eq!(
            module.slot_getter_overridden(getter.id, &[Some(OBJECT_CLASS_ID)]),
            None
        );
    }

    #[test]
    fn slot_reads_of_a_known_class_skip_the_getter_generic() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <wheel> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function known(c :: <circle>) => <integer>\n  c.radius + radius(c)\nend;\n\
             define function any(o :: <object>) => <integer>\n  o.radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::Binary { lhs, rhs, .. } = &module.functions[0].body.result else {
            panic!("expected the sum of two reads");
        };
        assert!(matches!(lhs.as_ref(), Expr::AccessorCall { .. }));
        assert!(matches!(rhs.as_ref(), Expr::AccessorCall { .. }));
        assert!(matches!(
            &module.functions[1].body.result,
            Expr::GenericCall { generic, .. } if module.generics[generic.0 as usize].name == "radius"
        ));
    }

    #[test]
    fn resolves_runtime_callable_parameters() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function callit(f :: <callable>) => <integer>\n  f(41)\nend;\n\
             define function main()\n  callit(inc)\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions[1].param_kinds, [ValueKind::Callable]);
        let Expr::CallableCall {
            callable,
            arg_kinds,
            result_kinds,
            ..
        } = &module.functions[1].body.result
        else {
            panic!("expected runtime callable apply");
        };
        assert_eq!(arg_kinds, &[ValueKind::I64]);
        assert_eq!(result_kinds, &[ValueKind::I64]);
        assert!(matches!(
            callable.as_ref(),
            Expr::Local {
                local: LocalId(0),
                result_kind: ValueKind::Callable,
                ..
            }
        ));
    }

    #[test]
    fn a_rest_call_makes_its_vector_and_apply_spreads_its_last_argument() {
        let module = lower_source(
            "define function count(a, #rest xs) => <integer>\n  size(xs)\nend;\n\
             define function direct() => <integer>\n  count(1, 2, 3)\nend;\n\
             define function spread() => <integer>\n  apply(count, 1, #[2, 3])\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        assert!(module.functions[0].rest);
        let Expr::DirectCall { args, .. } = &module.functions[1].body.result else {
            panic!("expected a direct call");
        };
        assert_eq!(args.len(), 2);
        assert!(matches!(&args[1], Expr::VectorLiteral { elements, .. } if elements.len() == 2));
        // `apply` collects its arguments into one vector, then calls through
        // the function's value with it.
        let mut spread = false;
        visit_evaluated(&module.functions[2].body.result, &mut |expr| {
            spread |= matches!(
                expr,
                Expr::CallableCall {
                    arguments: CallArguments::Spread,
                    args,
                    ..
                } if args.len() == 1
            );
        });
        assert!(spread);
    }

    #[test]
    fn reports_invalid_apply_targets() {
        let non_callable = lower_source("define function main()\n  apply(1, #[2])\nend;");
        assert!(has_diagnostic(
            &non_callable,
            DiagnosticCode::ApplyTargetTypeMismatch,
            &[("actual", "<integer>")]
        ));
        let untyped = lower_source(
            "define function run(f :: <callable>)\n  let r = apply(f, #[4]);\n  r\nend;",
        );
        assert!(has_diagnostic(
            &untyped,
            DiagnosticCode::UntypedCallableResult,
            &[]
        ));
    }

    #[test]
    fn an_unresolved_class_does_not_cascade() {
        let source = "define generic evaluate(x, env);\n\
             define method evaluate(x :: <lisp-pair>, env :: <object>) 1 end;\n\
             define method evaluate(x :: <lisp-symbol>, env :: <object>) 2 end;\n\
             define function main() => <integer>\n\
               let p :: <lisp-pair> = 3;\n  p.size + evaluate(1, 2)\nend;";
        let module = lower_source(source);
        let codes = module
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert_eq!(codes, vec![DiagnosticCode::UnresolvedClass; 3]);
        // Each names its specializer's own span, not the parameter's.
        let start = source.find("<lisp-pair>").unwrap_or(0);
        assert!(module.diagnostics.iter().any(|diagnostic| {
            (diagnostic.span.start, diagnostic.span.end) == (start, start + "<lisp-pair>".len())
        }));
    }

    #[test]
    fn resolves_host_backed_classes_and_methods() {
        let registry = rust_circle_registry();
        let module = lower_source_with_host_registry(
            "define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <rust-circle>) => <integer>\n  c.radius * c.radius\nend;\n\
             define function radius(c :: <rust-circle>) => <integer>\n  c.radius\nend;",
            &registry,
        );

        assert_eq!(module.diagnostics, Vec::new());
        let rust_circle = class(&module, "<rust-circle>");
        assert_eq!(
            rust_circle.origin,
            ClassOrigin::HostBacked {
                host_class: HostClassId(0),
            }
        );
        assert_eq!(
            rust_circle.direct_slots[0].origin,
            SlotOrigin::HostBacked {
                host_class: HostClassId(0),
                host_slot: HostSlotId(0),
            }
        );
        assert_eq!(module.methods[0].specializer, rust_circle.id);
        assert!(matches!(module.methods[0].body.result, Expr::Binary { .. }));
    }

    #[test]
    fn rejects_make_for_host_backed_classes() {
        let registry = rust_circle_registry();
        let module = lower_source_with_host_registry(
            "define function main()\n  make(<rust-circle>, radius: 10)\nend;",
            &registry,
        );

        assert!(has_diagnostic(
            &module,
            DiagnosticCode::HostBackedMake,
            &[("class", "<rust-circle>")]
        ));
    }

    #[test]
    fn resolves_classes_methods_slots_and_generic_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let circle = class(&module, "<circle>");
        assert_eq!(circle.direct_slots[0].id, SlotId(0));
        assert_eq!(circle.direct_slots[0].kind, ValueKind::I64);
        assert_eq!(
            circle.direct_slots[0].accessor.definition,
            circle.direct_slots[0].definition
        );
        assert_eq!(module.generics[0].id, GenericFunctionId(0));
        assert_eq!(module.methods[0].id, MethodId(0));
        assert_eq!(module.methods[0].generic, GenericFunctionId(0));
        assert_eq!(module.methods[0].specializer, circle.id);
        let Expr::GenericCall { generic, args, .. } = &module.functions[0].body.result else {
            panic!("expected generic call");
        };
        assert_eq!(*generic, GenericFunctionId(0));
        assert!(matches!(
            args[0],
            Expr::MakeObject {
                class_id,
                ..
            } if class_id == circle.id
        ));
        let Expr::Binary { lhs, .. } = &module.methods[0].body.result else {
            panic!("expected method body binary expression");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::AccessorCall {
                class,
                slot: SlotId(0),
                ..
            } if *class == circle.id
        ));
    }

    #[test]
    fn finalizes_inherited_slots_before_direct_slots() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot x :: <integer>, required-init-keyword: x:;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, required-init-keyword: y:;\nend;\n\
             define function main()\n  make(<point>, x: 2, y: 3).x\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let shape_id = class_id(&module, "<shape>");
        let point = class(&module, "<point>");
        assert_eq!(point.superclass, Some(shape_id));
        assert!(point.finalized);
        assert_eq!(point.direct_slots.len(), 1);
        assert_eq!(point.effective_slots.len(), 2);
        assert_eq!(point.effective_slots[0].name, "x");
        assert_eq!(point.effective_slots[0].source_class, shape_id);
        assert_eq!(point.effective_slots[0].source_slot, SlotId(0));
        assert_eq!(point.effective_slots[1].name, "y");
        assert_eq!(point.effective_slots[1].source_class, point.id);
        let plan = &point.initialization_plan;
        assert_eq!(plan.class, point.id);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].slot, SlotId(0));
        assert_eq!(plan.steps[0].init_keyword, "x");
        assert!(plan.steps[0].inherited);
        assert!(plan.steps[0].required_init_keyword);
        assert_eq!(plan.steps[1].slot, SlotId(1));
        assert_eq!(plan.steps[1].init_keyword, "y");
        assert!(!plan.steps[1].inherited);
        assert!(plan.steps[1].required_init_keyword);

        let Expr::AccessorCall { class, slot, .. } = &module.functions[0].body.result else {
            panic!("expected inherited slot read");
        };
        assert_eq!(*class, point.id);
        assert_eq!(*slot, SlotId(0));
    }

    #[test]
    fn defines_the_kernel_condition_classes_in_every_module() {
        let module = lower_source(
            "define class <bad-input> (<error>)\n  slot cause :: <type-error>;\nend;\n\
             define function main()\n  make(<type-error>, value: 1, expected-type: <string>, \
             site: \"here\", code: \"type-error.cast\").type-error-site\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let condition = class_id(&module, "<condition>");
        let error = class_id(&module, "<error>");
        let type_error = class(&module, "<type-error>");
        assert_eq!(class(&module, "<error>").superclass, Some(condition));
        assert_eq!(type_error.superclass, Some(error));
        assert_eq!(
            type_error.origin,
            ClassOrigin::Condition {
                class: ConditionClass::TypeError
            }
        );
        let names = type_error
            .effective_slots
            .iter()
            .map(|slot| slot.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "type-error-value",
                "type-error-expected-type",
                "type-error-site",
                "type-error-code"
            ]
        );
        assert!(
            type_error
                .direct_slots
                .iter()
                .all(|slot| slot.mutability == SlotMutability::Constant)
        );
        // A source slot names a kernel class before the kernel classes are
        // added after the built-in value classes.
        let bad_input = class(&module, "<bad-input>");
        assert_eq!(bad_input.superclass, Some(error));
        assert_eq!(
            bad_input.direct_slots[0].kind,
            ValueKind::Object(type_error.id)
        );
        assert!(
            module
                .generics
                .iter()
                .all(|generic| !generic.name.starts_with("type-error-"))
        );
        // Kernel classes are appended, so no other class id moves.
        let no_method = class(&module, "<no-applicable-method>");
        assert_eq!(no_method.superclass, Some(error));
        assert_eq!(no_method.id.0, type_error.id.0 + 1);
        let select_error = class(&module, "<select-error>");
        assert_eq!(select_error.superclass, Some(error));
        assert_eq!(select_error.id.0, no_method.id.0 + 1);
        let not_found = class(&module, "<not-found-error>");
        assert_eq!(not_found.superclass, Some(error));
        assert_eq!(not_found.id.0, select_error.id.0 + 1);
        let immutable = class(&module, "<immutable-collection-error>");
        assert_eq!(immutable.superclass, Some(error));
        assert_eq!(immutable.id.0, not_found.id.0 + 1);
        let modified = class(&module, "<collection-modified-error>");
        assert_eq!(modified.superclass, Some(error));
        assert_eq!(modified.id.0, immutable.id.0 + 1);
        assert_eq!(
            not_found
                .effective_slots
                .iter()
                .map(|slot| slot.name.as_str())
                .collect::<Vec<_>>(),
            [
                "not-found-error-collection",
                "not-found-error-key",
                "not-found-error-code"
            ]
        );
        assert_eq!(
            select_error
                .effective_slots
                .iter()
                .map(|slot| (slot.name.as_str(), slot.kind))
                .collect::<Vec<_>>(),
            [("select-error-value", ValueKind::Any)]
        );
        let cycle = class(&module, "<constant-cycle-error>");
        assert_eq!(cycle.superclass, Some(error));
        assert_eq!(cycle.id.0, modified.id.0 + 1);
        let by_zero = class(&module, "<division-by-zero-error>");
        assert_eq!(by_zero.superclass, Some(error));
        assert_eq!(by_zero.id.0, cycle.id.0 + 1);
        let unknown_keyword = class(&module, "<unknown-keyword-error>");
        assert_eq!(unknown_keyword.superclass, Some(error));
        assert_eq!(unknown_keyword.id.0, by_zero.id.0 + 1);
        let simple = class(&module, "<simple-error>");
        assert_eq!(simple.superclass, Some(error));
        assert_eq!(simple.id.0, unknown_keyword.id.0 + 1);
        assert_eq!(
            simple
                .effective_slots
                .iter()
                .map(|slot| slot.name.as_str())
                .collect::<Vec<_>>(),
            ["condition-format-string", "condition-format-arguments"]
        );
        let argument_count = class(&module, "<argument-count-error>");
        assert_eq!(argument_count.superclass, Some(error));
        assert_eq!(argument_count.id.0, simple.id.0 + 1);
        assert_eq!(argument_count.id.0 as usize, module.classes.len() - 1);
        assert_eq!(
            argument_count
                .effective_slots
                .iter()
                .map(|slot| (slot.name.as_str(), slot.kind))
                .collect::<Vec<_>>(),
            [
                ("argument-count-error-expected", ValueKind::I64),
                ("argument-count-error-actual", ValueKind::I64)
            ]
        );
        assert_eq!(
            unknown_keyword
                .effective_slots
                .iter()
                .map(|slot| (slot.name.as_str(), slot.kind))
                .collect::<Vec<_>>(),
            [(
                "unknown-keyword-error-keyword",
                ValueKind::Symbol(class_id(&module, "<symbol>"))
            )]
        );
        assert_eq!(
            by_zero
                .effective_slots
                .iter()
                .map(|slot| (slot.name.as_str(), slot.kind))
                .collect::<Vec<_>>(),
            [("division-by-zero-error-dividend", ValueKind::I64)]
        );
        let string = ValueKind::String(class_id(&module, "<string>"));
        assert_eq!(
            cycle
                .effective_slots
                .iter()
                .map(|slot| (slot.name.as_str(), slot.kind))
                .collect::<Vec<_>>(),
            [
                ("constant-cycle-error-constant", string),
                ("constant-cycle-error-reader", string),
                ("constant-cycle-error-code", string),
            ]
        );
        let kinds = no_method
            .effective_slots
            .iter()
            .map(|slot| (slot.name.as_str(), slot.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                (
                    "no-applicable-method-generic",
                    ValueKind::Object(class_id(&module, "<generic-function>"))
                ),
                (
                    "no-applicable-method-arguments",
                    ValueKind::Vector(class_id(&module, "<vector>"))
                ),
                (
                    "no-applicable-method-code",
                    ValueKind::String(class_id(&module, "<string>"))
                ),
            ]
        );
    }

    #[test]
    fn treats_single_inheritance_as_nominal_subtyping() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 5;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 7;\nend;\n\
             define class <holder> (<object>)\n  slot item :: <shape>;\nend;\n\
             define function hold(s :: <shape>) => <shape>\n  s\nend;\n\
             define function build() => <shape>\n  make(<circle>)\nend;\n\
             define generic describe(x :: <shape>) => <integer>;\n\
             define method describe(c :: <circle>) => <integer>\n  c.radius\nend;\n\
             define function main()\n  hold(make(<circle>)).size + build().size + make(<holder>, item: make(<circle>)).item.size + describe(make(<circle>))\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let shape_id = class_id(&module, "<shape>");
        let circle_id = class_id(&module, "<circle>");
        let holder = class(&module, "<holder>");
        assert_eq!(holder.direct_slots[0].kind, ValueKind::Object(shape_id));
        assert_eq!(
            module.functions[0].result_kinds,
            [ValueKind::Object(shape_id)]
        );
        assert_eq!(
            module.functions[1].result_kinds,
            [ValueKind::Object(shape_id)]
        );
        assert_eq!(module.methods[0].specializer, circle_id);
    }

    #[test]
    fn resolves_parent_instance_tests_casts_and_restart_conditions() {
        let module = lower_source(
            "define class <problem> (<object>)\n  slot fallback :: <integer>, init-value: 1;\nend;\n\
             define class <invalid-radius> (<problem>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  handle if (instance?(make(<invalid-radius>), <problem>)) as(<problem>, make(<invalid-radius>)).fallback else 0 end with restart e :: <problem> e.fallback end\nend;\n\
             define function recover()\n  handle signal(make(<invalid-radius>)) with restart e :: <problem> e.fallback end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let problem_id = class_id(&module, "<problem>");
        let invalid_radius_id = class_id(&module, "<invalid-radius>");
        let Expr::Handle { body, .. } = &module.functions[0].body.result else {
            panic!("expected handle");
        };
        let Expr::If {
            condition,
            then_branch,
            ..
        } = body.as_ref()
        else {
            panic!("expected if");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::InstanceOf {
                class,
                ..
            } if *class == problem_id
        ));
        let Expr::AccessorCall {
            receiver, class, ..
        } = then_branch.as_ref()
        else {
            panic!("expected parent slot read");
        };
        assert_eq!(*class, problem_id);
        assert!(matches!(
            receiver.as_ref(),
            Expr::Cast {
                class,
                ..
            } if *class == problem_id
        ));

        let Expr::Handle { body, restart, .. } = &module.functions[1].body.result else {
            panic!("expected restart handle");
        };
        assert_eq!(restart.specializer, problem_id);
        assert!(matches!(
            body.as_ref(),
            Expr::Signal {
                condition_class,
                ..
            } if *condition_class == invalid_radius_id
        ));
    }

    #[test]
    fn keeps_sibling_instance_tests_and_casts_impossible() {
        let instance = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\nend;\n\
             define class <square> (<shape>)\nend;\n\
             define function main()\n  instance?(make(<square>), <circle>)\nend;",
        );
        assert!(has_diagnostic(
            &instance,
            DiagnosticCode::InstanceOfImpossible,
            &[("actual", "<square>"), ("expected", "<circle>")]
        ));

        let cast = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 1;\nend;\n\
             define class <square> (<shape>)\nend;\n\
             define function main()\n  as(<circle>, make(<square>)).radius\nend;",
        );
        assert!(has_diagnostic(
            &cast,
            DiagnosticCode::CastImpossible,
            &[("actual", "<square>"), ("expected", "<circle>")]
        ));
    }

    #[test]
    fn reports_bad_superclasses_and_effective_slot_conflicts() {
        let unknown = lower_source(
            "define class <child> (<missing>)\nend;\n\
             define function main()\n  0\nend;",
        );
        assert!(has_diagnostic(
            &unknown,
            DiagnosticCode::UnresolvedSuperclass,
            &[("superclass", "<missing>"), ("class", "<child>")]
        ));

        let primitive = lower_source(
            "define class <bad> (<boolean>)\nend;\n\
             define function main()\n  0\nend;",
        );
        assert!(has_diagnostic(
            &primitive,
            DiagnosticCode::InvalidPrimitiveSuperclass,
            &[("class", "<bad>"), ("superclass", "<boolean>")]
        ));

        let duplicate = lower_source(
            "define class <shape> (<object>)\n  slot radius :: <integer>, init-value: 1;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  0\nend;",
        );
        assert!(has_diagnostic(
            &duplicate,
            DiagnosticCode::DuplicateEffectiveSlot,
            &[("class", "<circle>"), ("slot", "radius")]
        ));
        assert!(has_diagnostic(
            &duplicate,
            DiagnosticCode::DuplicateEffectiveAccessor,
            &[("class", "<circle>"), ("accessor", "radius")]
        ));
    }

    #[test]
    fn resolves_typed_generic_contracts_and_method_returns() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <integer>\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.generics[0].params, ["x"]);
        assert_eq!(module.generics[0].param_kinds, [Some(ValueKind::Any)]);
        assert_eq!(module.generics[0].result_kinds, [ValueKind::I64]);
        assert_eq!(
            module.generics[0].return_annotation,
            Some(vec![ValueKind::I64])
        );
        assert_eq!(
            module.methods[0].return_annotation,
            Some(vec![ValueKind::I64])
        );
        assert_eq!(module.methods[0].result_kinds, [ValueKind::I64]);
        let Expr::GenericCall { generic, .. } = &module.functions[0].body.result else {
            panic!("expected generic call");
        };
        assert_eq!(*generic, GenericFunctionId(0));
    }

    #[test]
    fn limited_types_resolve_to_integers_with_ranges() {
        let module = lower_source(
            "define generic send(n :: limited(<integer>, min: 0));\n\
             define method send(n :: limited(<integer>, min: 0))\n  n\nend;\n\
             define function f(n :: limited(<integer>, min: -1, max: 255)) => limited(<integer>, max: 9)\n  \
             let m :: limited(<integer>) = n;\n  0\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let function = &module.functions[0];
        let byte = IntRange {
            min: Some(-1),
            max: Some(255),
        };
        assert_eq!(function.param_kinds, [ValueKind::I64]);
        assert_eq!(function.param_ranges, [Some(byte)]);
        assert_eq!(function.locals[0].range, Some(byte));
        // `limited(<integer>)` is every `<integer>`.
        assert_eq!(function.locals[1].range, None);
        assert_eq!(
            function.result_ranges,
            [Some(IntRange {
                min: None,
                max: Some(9)
            })]
        );
        let at_least_zero = IntRange {
            min: Some(0),
            max: None,
        };
        assert_eq!(module.generics[0].param_ranges, [Some(at_least_zero)]);
        assert_eq!(module.methods[0].locals[0].range, Some(at_least_zero));
        let contract = module.function_contract(function);
        let names = contract
            .params
            .iter()
            .chain(&contract.results)
            .map(|value| value.display(&module).to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "limited(<integer>, min: -1, max: 255)",
                "limited(<integer>, max: 9)"
            ]
        );
    }

    #[test]
    fn reports_limited_types_that_name_no_range() {
        type Case<'a> = (&'a str, DiagnosticCode, &'a [(&'a str, &'a str)]);
        let cases: [Case<'_>; 6] = [
            (
                "limited(<string>, max: 1)",
                DiagnosticCode::LimitedTypeBase,
                &[("base", "<string>")],
            ),
            (
                "limited(<integer>, size: 1)",
                DiagnosticCode::LimitedTypeOption,
                &[("option", "size")],
            ),
            (
                "limited(<integer>, min: 1, min: 2)",
                DiagnosticCode::LimitedTypeOption,
                &[("option", "min")],
            ),
            (
                "limited(<integer>, max: 1.5)",
                DiagnosticCode::LimitedTypeBound,
                &[("option", "max"), ("bound", "1.5")],
            ),
            (
                "limited(<integer>, max: 99999999999999999999)",
                DiagnosticCode::LimitedTypeBound,
                &[("option", "max")],
            ),
            (
                "limited(<integer>, min: 5, max: 4)",
                DiagnosticCode::EmptyLimitedType,
                &[("type", "limited(<integer>, min: 5, max: 4)")],
            ),
        ];
        for (ty, code, args) in cases {
            let module = lower_source(&format!("define function f(n :: {ty})\n  0\nend;"));
            assert!(
                has_diagnostic(&module, code, args),
                "{ty}: {:?}",
                module.diagnostics
            );
        }
        // A method may accept more than its generic's range, but not less.
        let wider = lower_source(
            "define generic send(n :: limited(<integer>, min: 0, max: 9));\n\
             define method send(n :: limited(<integer>, min: -5))\n  n\nend;",
        );
        assert_eq!(wider.diagnostics, Vec::new());
        // Sends check the generic's range, which the body may assume.
        assert_eq!(
            wider.methods[0].locals[0].range,
            Some(IntRange {
                min: Some(0),
                max: Some(9)
            })
        );
        let module = lower_source(
            "define generic send(n :: limited(<integer>, min: 0));\n\
             define method send(n :: limited(<integer>, min: 1))\n  n\nend;",
        );
        assert!(has_diagnostic(
            &module,
            DiagnosticCode::LimitedTypeSpecializer,
            &[("type", "limited(<integer>, min: 1)")]
        ));
        // A callable signature compares ranges as a contract does.
        let module = lower_source(
            "define function digit(n :: limited(<integer>, min: 0, max: 9)) => <integer>\n  n\nend;\n\
             define function main() => <integer>\n  \
             let f :: callable(<integer>) => <integer> = digit;\n  0\nend;",
        );
        assert!(has_diagnostic(
            &module,
            DiagnosticCode::CallableParameterVariance,
            &[("accepted", "limited(<integer>, min: 0, max: 9)")]
        ));
    }

    #[test]
    fn reports_typed_generic_contract_mismatches() {
        let bad_call = lower_source(
            "define generic area(x :: <object>) => <integer>;\n\
             define function main()\n  area(1)\nend;",
        );
        assert!(has_diagnostic(
            &bad_call,
            DiagnosticCode::GenericWithoutMethods,
            &[
                ("generic", "area"),
                ("argument_index", "1"),
                ("actual", "<integer>"),
                ("class", "<integer>")
            ]
        ));

        // Methods on object classes only: the diagnostic names them, and
        // the class a method would need to take the argument.
        let shapes = "define class <circle> (<object>)\nend;\n\
             define class <square> (<object>)\nend;\n\
             define generic area(x) => <integer>;\n\
             define method area(c :: <circle>) => <integer>\n  1\nend;\n\
             define method area(s :: <square>) => <integer>\n  2\nend;\n";
        for (call, actual, class) in [
            ("area(5)", "<integer>", "<integer>"),
            ("area(2.5)", "<double-float>", "<double-float>"),
            ("area(#t)", "<boolean>", "<boolean>"),
            ("area(area)", "generic `area`", "<function>"),
        ] {
            let module = lower_source(&format!("{shapes}define function main()\n  {call}\nend;"));
            assert!(
                has_diagnostic(
                    &module,
                    DiagnosticCode::NoApplicableMethod,
                    &[
                        ("generic", "area"),
                        ("argument_index", "1"),
                        ("actual", actual),
                        ("specializers", "<circle>, <square>"),
                        ("class", class)
                    ]
                ),
                "{call}: {:?}",
                module.diagnostics
            );
        }

        let bad_method_return = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <boolean>\n  #t\nend;",
        );
        assert!(has_diagnostic(
            &bad_method_return,
            DiagnosticCode::GenericMethodResultTypeMismatch,
            &[
                ("method", "area"),
                ("actual", "<boolean>"),
                ("expected", "<integer>")
            ]
        ));

        let bad_body = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <integer>\n  #t\nend;",
        );
        assert!(has_diagnostic(
            &bad_body,
            DiagnosticCode::MethodReturnTypeMismatch,
            &[
                ("method", "area"),
                ("expected", "<integer>"),
                ("actual", "<boolean>")
            ]
        ));

        let bad_specializer = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define generic area(x :: <circle>) => <integer>;\n\
             define method area(s :: <square>) => <integer>\n  s.side * s.side\nend;",
        );
        assert!(has_diagnostic(
            &bad_specializer,
            DiagnosticCode::GenericMethodSpecializerTypeMismatch,
            &[
                ("method", "area"),
                ("actual", "<square>"),
                ("expected", "<circle>")
            ]
        ));
    }

    #[test]
    fn resolves_explicit_slot_accessor_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = make(<circle>, radius: 7);\n  radius(c)\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle = class(&module, "<circle>");
        let Expr::AccessorCall {
            accessor,
            class,
            slot,
            result_kind,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected accessor call");
        };
        assert_eq!(*accessor, circle.direct_slots[0].definition);
        assert_eq!(*class, circle.id);
        assert_eq!(*slot, SlotId(0));
        assert_eq!(*result_kind, ValueKind::I64);
    }

    #[test]
    fn resolves_class_objects_for_make_and_class_of() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = <circle>;\n  make(c, radius: 7).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle_id = class_id(&module, "<circle>");
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::ClassObject(circle_id)
        );
        let Expr::ClassObject { class, .. } =
            &module.functions[0].body.lets().next().expect("a let").value
        else {
            panic!("expected class object let value");
        };
        assert_eq!(*class, circle_id);

        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = make(<circle>, radius: 7);\n  let k = class-of(c);\n  make(k, radius: 8).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle_id = class_id(&module, "<circle>");
        assert_eq!(
            module.functions[0].locals[1].kind,
            ValueKind::ClassObject(circle_id)
        );
        let Expr::ClassOf { class, .. } =
            &module.functions[0].body.lets().nth(1).expect("a let").value
        else {
            panic!("expected class-of expression");
        };
        assert_eq!(*class, circle_id);
    }

    #[test]
    fn resolves_typed_direct_function_signatures() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function keep?(flag :: <boolean>) => <boolean>\n  flag\nend;\n\
             define function build(klass :: class <circle>, radius :: <integer>) => <circle>\n  make(klass, radius: radius)\nend;\n\
             define function main()\n  if (keep?(#t)) build(<circle>, 4).radius else 0 end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle_id = class_id(&module, "<circle>");
        assert_eq!(module.functions[0].param_kinds, [ValueKind::Bool]);
        assert_eq!(
            module.functions[0].return_annotation,
            Some(vec![ValueKind::Bool])
        );
        assert_eq!(
            module.functions[1].param_kinds,
            [ValueKind::ClassObject(circle_id), ValueKind::I64]
        );
        assert_eq!(
            module.functions[1].return_annotation,
            Some(vec![ValueKind::Object(circle_id)])
        );
        assert_eq!(module.functions[2].result_kinds, [ValueKind::I64]);
    }

    #[test]
    fn reports_typed_direct_function_mismatches() {
        let argument = lower_source(
            "define function keep?(flag :: <boolean>) => <boolean>\n  flag\nend;\n\
             define function main()\n  keep?(1)\nend;",
        );
        assert!(has_diagnostic(
            &argument,
            DiagnosticCode::FunctionArgumentTypeMismatch,
            &[
                ("function", "keep?"),
                ("argument_index", "1"),
                ("expected", "<boolean>"),
                ("actual", "<integer>")
            ]
        ));

        let result = lower_source("define function bad() => <boolean>\n  1\nend;");
        assert!(has_diagnostic(
            &result,
            DiagnosticCode::FunctionReturnTypeMismatch,
            &[
                ("function", "bad"),
                ("expected", "<boolean>"),
                ("actual", "<integer>")
            ]
        ));
    }

    #[test]
    fn a_branch_that_assigns_the_tested_local_is_not_refined() {
        // The branch may store any `<object>` in `x`, so the test does not
        // describe it there; one that only reads `x` is refined.
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function reset(given :: <object>) => <integer>\n  \
             let x = given;\n  \
             if (instance?(x, <circle>)) x := 0; 1 else 0 end\nend;\n\
             define function read(given :: <object>) => <integer>\n  \
             let x = given;\n  \
             if (instance?(x, <circle>)) x.radius else 0 end\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let refinement = |index: usize| {
            let Expr::If { refinement, .. } = &module.functions[index].body.result else {
                panic!("expected if");
            };
            *refinement
        };
        assert_eq!(refinement(0), None);
        assert!(module.functions[0].locals[1].assigned);
        assert!(refinement(1).is_some());
        assert!(!module.functions[1].locals[1].assigned);
    }

    #[test]
    fn assignments_check_the_local_and_its_captures() {
        let module = lower_source(
            "define function main(p :: <integer>) => <integer>\n  \
             let n :: <integer> = 0;\n  \
             n := \"text\";\n  \
             p := 1;\n  \
             let k = 1;\n  \
             let f = method () => <integer> k end;\n  \
             let g = method () => <integer> p := 2 end;\n  \
             k := 2;\n  \
             while (k) k := 3 end;\n  \
             block (out) out := 4; 0 end;\n  \
             block (exit) let h = method () => () exit := 5 end; 0 end;\n  \
             n\nend;",
        );
        let codes = module
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        // A parameter is assignable, a block exit is not; the closure's
        // diagnostic is reported first, as its body is lowered.
        assert_eq!(
            codes,
            [
                DiagnosticCode::AssignmentTargetNotAssignable,
                DiagnosticCode::LocalAssignmentTypeMismatch,
                DiagnosticCode::IfConditionTypeMismatch,
                DiagnosticCode::AssignmentTargetNotAssignable,
            ]
        );
        // `k` and the parameter `p` are assigned and captured, so each lives
        // in a cell the closures share; `n` is assigned but never captured.
        let main = &module.functions[0];
        let local = |name: &str| {
            main.locals
                .iter()
                .find(|local| local.name == name)
                .expect("local")
        };
        assert!(local("k").shared);
        assert!(local("p").shared);
        assert!(!local("n").shared);
        assert!(module.closures[0].captures[0].shared);
        assert!(module.closures[1].captures[0].shared);
    }

    #[test]
    fn resolves_instance_tests_casts_and_true_branch_refinement() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function radius-or-zero(x :: <object>) => <integer>\n  if (instance?(x, <circle>)) x.radius else 0 end\nend;\n\
             define function radius(x :: <object>) => <integer>\n  as(<circle>, x).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle_id = class_id(&module, "<circle>");
        let Expr::If {
            condition,
            refinement,
            then_branch,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected if");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::InstanceOf {
                class,
                ..
            } if *class == circle_id
        ));
        assert_eq!(
            *refinement,
            Some(Refinement {
                local: LocalId(0),
                kind: ValueKind::Object(circle_id),
            })
        );
        let Expr::AccessorCall { receiver, .. } = then_branch.as_ref() else {
            panic!("expected refined slot accessor");
        };
        assert!(matches!(
            receiver.as_ref(),
            Expr::Local {
                local: LocalId(0),
                result_kind,
                ..
            } if *result_kind == ValueKind::Object(circle_id)
        ));
        let Expr::AccessorCall { receiver, .. } = &module.functions[1].body.result else {
            panic!("expected cast slot accessor");
        };
        assert!(matches!(
            receiver.as_ref(),
            Expr::Cast {
                class,
                ..
            } if *class == circle_id
        ));
    }

    #[test]
    fn scalar_instance_tests_refine_a_boxed_local_to_its_unboxed_kind() {
        let module = lower_source(
            "define function f(x :: <object>) => <integer>\n  if (instance?(x, <integer>)) x + 1 else 0 end\nend;\n\
             define function g(x :: <object>) => <double-float>\n  if (instance?(x, <double-float>)) x else 0.0 end\nend;\n\
             define function h(x :: <object>) => <single-float>\n  if (instance?(x, <single-float>)) x else 0.0f32 end\nend;\n\
             define function i(x :: <object>) => <integer>\n  if (instance?(x, <i32>)) x else 0 end\nend;\n\
             define function j(x :: <object>) => <object>\n  if (instance?(x, <real>)) x else 0 end\nend;\n\
             define function k(x :: <i32>) => <integer>\n  if (instance?(x, <integer>)) x else 0 end\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        let refinement = |index: usize| {
            let Expr::If { refinement, .. } = &module.functions[index].body.result else {
                panic!("expected if");
            };
            refinement.map(|refinement| refinement.kind)
        };
        assert_eq!(refinement(0), Some(ValueKind::I64));
        assert_eq!(refinement(1), Some(ValueKind::F64));
        assert_eq!(refinement(2), Some(ValueKind::F32));
        // A limited type's members are `<integer>`s: the local is an `i64`.
        assert_eq!(refinement(3), Some(ValueKind::I64));
        // `<real>` has no single representation.
        assert_eq!(refinement(4), None);
        // An unboxed local already has a scalar kind.
        assert_eq!(refinement(5), None);
        // The then branch adds `i64`s directly instead of sending `+`.
        let Expr::If { then_branch, .. } = &module.functions[0].body.result else {
            panic!("expected if");
        };
        assert!(matches!(
            then_branch.as_ref(),
            Expr::Binary {
                kind: NumericKind::Integer,
                ..
            }
        ));
    }

    #[test]
    fn reports_impossible_instance_tests_and_casts() {
        let instance = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define function main()\n  if (instance?(make(<square>, side: 2), <circle>)) 1 else 0 end\nend;",
        );
        assert!(has_diagnostic(
            &instance,
            DiagnosticCode::InstanceOfImpossible,
            &[("actual", "<square>"), ("expected", "<circle>")]
        ));

        let cast = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define function main()\n  as(<circle>, make(<square>, side: 2)).radius\nend;",
        );
        assert!(has_diagnostic(
            &cast,
            DiagnosticCode::CastImpossible,
            &[("actual", "<square>"), ("expected", "<circle>")]
        ));

        let primitive = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  instance?(1, <circle>)\nend;",
        );
        assert!(has_diagnostic(
            &primitive,
            DiagnosticCode::InstanceOfImpossible,
            &[("actual", "<integer>"), ("expected", "<circle>")]
        ));

        let integer = lower_source("define function main()\n  instance?(1, <integer>)\nend;");
        assert_eq!(integer.diagnostics, Vec::new());
    }

    #[test]
    fn resolves_typed_slots_and_checks_initializers() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <box> (<object>)\n  slot item :: <circle>;\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if (make(<box>, item: make(<circle>, radius: 7), valid?: #t).valid?) make(<box>, item: make(<circle>, radius: 7), valid?: #t).item.radius else 0 end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle_id = class_id(&module, "<circle>");
        let circle = class(&module, "<circle>");
        let box_class = class(&module, "<box>");
        assert_eq!(circle.direct_slots[0].kind, ValueKind::I64);
        assert_eq!(box_class.direct_slots[0].kind, ValueKind::Object(circle_id));
        assert_eq!(box_class.direct_slots[1].kind, ValueKind::Bool);
        assert_eq!(module.functions[0].result_kinds, [ValueKind::I64]);
    }

    #[test]
    fn untyped_slots_default_to_object_top() {
        let module = lower_source(
            "define class <box> (<object>)\n  slot value;\n  slot flag;\nend;\n\
             define function main()\n  make(<box>, value: 7, flag: #t).value\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let box_class = class(&module, "<box>");
        assert_eq!(box_class.direct_slots[0].type_name, "<object>");
        assert_eq!(box_class.direct_slots[0].kind, ValueKind::Any);
        assert_eq!(box_class.direct_slots[0].init_keyword, "value");
        assert!(!box_class.direct_slots[0].required_init_keyword);
        assert_eq!(box_class.direct_slots[1].kind, ValueKind::Any);
        assert_eq!(module.functions[0].result_kinds, [ValueKind::Any]);
    }

    #[test]
    fn resolves_slot_init_keywords_and_required_keywords() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: r:;\n  slot cached, init-keyword: payload:;\nend;\n\
             define function main()\n  make(<circle>, r: 10, payload: #f).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle = class(&module, "<circle>");
        assert_eq!(circle.direct_slots[0].init_keyword, "r");
        assert!(circle.direct_slots[0].required_init_keyword);
        assert_eq!(circle.direct_slots[1].init_keyword, "payload");
        assert!(!circle.direct_slots[1].required_init_keyword);
        assert_eq!(circle.initialization_plan.steps.len(), 2);
        assert_eq!(circle.initialization_plan.steps[0].init_keyword, "r");
        assert_eq!(circle.initialization_plan.steps[1].init_keyword, "payload");
        let Expr::AccessorCall { receiver, .. } = &module.functions[0].body.result else {
            panic!("expected accessor call");
        };
        let Expr::MakeObject {
            init_args, fields, ..
        } = receiver.as_ref()
        else {
            panic!("expected make");
        };
        assert_eq!(init_args.len(), 2);
        assert_eq!(init_args[0].keyword, "r");
        assert_eq!(init_args[0].slot, SlotId(0));
        assert_eq!(init_args[1].keyword, "payload");
        assert_eq!(init_args[1].slot, SlotId(1));
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].slot, SlotId(0));
        assert_eq!(fields[0].source, FieldInitSource::Explicit);
        assert_eq!(fields[1].slot, SlotId(1));
        assert_eq!(fields[1].source, FieldInitSource::Explicit);
        assert_eq!(module.functions[0].result_kinds, [ValueKind::I64]);
    }

    #[test]
    fn resolves_slot_init_values_and_constant_metadata() {
        let module = lower_source(
            "define class <circle> (<object>)\n  constant slot radius :: <integer>, required-init-keyword: r:, init-value: 10;\n  slot cached, init-value: #t;\nend;\n\
             define function main()\n  make(<circle>).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle = class(&module, "<circle>");
        let radius = &circle.direct_slots[0];
        assert_eq!(radius.init_keyword, "r");
        assert!(radius.required_init_keyword);
        assert_eq!(radius.allocation, SlotAllocation::Instance);
        assert_eq!(radius.mutability, SlotMutability::Constant);
        assert!(matches!(
            radius.init_value,
            Some(Expr::ConstI64 { value: 10, .. })
        ));
        let cached = &circle.direct_slots[1];
        assert_eq!(cached.kind, ValueKind::Any);
        assert_eq!(cached.mutability, SlotMutability::Mutable);
        assert!(matches!(
            cached.init_value,
            Some(Expr::ConstBool { value: true, .. })
        ));
        let plan = &circle.initialization_plan;
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.steps[0].required_init_keyword);
        assert!(plan.steps[0].has_default);
        assert!(!plan.steps[1].required_init_keyword);
        assert!(plan.steps[1].has_default);

        let Expr::AccessorCall { receiver, .. } = &module.functions[0].body.result else {
            panic!("expected accessor call");
        };
        let Expr::MakeObject {
            init_args, fields, ..
        } = receiver.as_ref()
        else {
            panic!("expected defaulted make");
        };
        assert!(init_args.is_empty());
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].source, FieldInitSource::Default);
        assert!(matches!(fields[0].value, Expr::ConstI64 { value: 10, .. }));
        assert_eq!(fields[1].source, FieldInitSource::Default);
        assert!(matches!(
            fields[1].value,
            Expr::ConstBool { value: true, .. }
        ));
    }

    #[test]
    fn make_records_init_args_and_explicit_overrides() {
        let module = lower_source(
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>, x: 7).x\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::AccessorCall { receiver, .. } = &module.functions[0].body.result else {
            panic!("expected accessor call");
        };
        let Expr::MakeObject {
            init_args, fields, ..
        } = receiver.as_ref()
        else {
            panic!("expected make");
        };
        assert_eq!(init_args.len(), 1);
        assert_eq!(init_args[0].keyword, "x");
        assert_eq!(init_args[0].slot, SlotId(0));
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].slot, SlotId(0));
        assert_eq!(fields[0].source, FieldInitSource::Explicit);
        assert!(matches!(fields[0].value, Expr::ConstI64 { value: 7, .. }));
        assert_eq!(fields[1].slot, SlotId(1));
        assert_eq!(fields[1].source, FieldInitSource::Default);
        assert!(matches!(fields[1].value, Expr::ConstI64 { value: 2, .. }));
    }

    #[test]
    fn reports_slot_type_errors() {
        let mismatch = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  make(<flag>, valid?: 1)\nend;",
        );
        assert!(has_diagnostic(
            &mismatch,
            DiagnosticCode::SlotInitializerTypeMismatch,
            &[
                ("slot", "valid?"),
                ("expected", "<boolean>"),
                ("actual", "<integer>")
            ]
        ));

        let unresolved = lower_source(
            "define class <bad> (<object>)\n  slot cause :: <missing>;\nend;\n\
             define function main()\n  1\nend;",
        );
        assert!(has_diagnostic(
            &unresolved,
            DiagnosticCode::UnresolvedClass,
            &[("class", "<missing>")]
        ));

        let bad_default = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>, init-value: 1;\nend;\n\
             define function main()\n  make(<flag>)\nend;",
        );
        assert!(has_diagnostic(
            &bad_default,
            DiagnosticCode::SlotDefaultTypeMismatch,
            &[
                ("slot", "valid?"),
                ("expected", "<boolean>"),
                ("actual", "<integer>")
            ]
        ));
    }

    #[test]
    fn reports_slot_keyword_construction_errors() {
        let missing = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: r:;\nend;\n\
             define function main()\n  make(<circle>)\nend;",
        );
        assert!(has_diagnostic(
            &missing,
            DiagnosticCode::MissingRequiredInitKeyword,
            &[("keyword", "r:"), ("slot", "radius")]
        ));

        let inherited_missing = lower_source(
            "define class <shape> (<object>)\n  slot x :: <integer>, required-init-keyword: x:;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>)\nend;",
        );
        assert!(has_diagnostic(
            &inherited_missing,
            DiagnosticCode::MissingRequiredInitKeyword,
            &[("keyword", "x:"), ("slot", "x")]
        ));

        let unknown = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, init-keyword: r:;\nend;\n\
             define function main()\n  make(<circle>, radius: 10)\nend;",
        );
        assert!(has_diagnostic(
            &unknown,
            DiagnosticCode::UnknownInitKeyword,
            &[("class", "<circle>"), ("keyword", "radius:")]
        ));

        let duplicate = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  make(<circle>, radius: 10, radius: 11)\nend;",
        );
        assert!(has_diagnostic(
            &duplicate,
            DiagnosticCode::DuplicateInitKeyword,
            &[("keyword", "radius:")]
        ));
    }

    #[test]
    fn checks_booleans_comparisons_and_if_kinds() {
        let module = lower_source(
            "define function nonnegative(x :: <integer>)\n  x >= 0\nend;\n\
             define function main(x :: <integer>)\n  if (nonnegative(x)) x else 0 - x end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions[0].result_kinds, [ValueKind::Bool]);
        assert_eq!(module.functions[1].result_kinds, [ValueKind::I64]);
        let Expr::If { condition, .. } = &module.functions[1].body.result else {
            panic!("expected if expression");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::DirectCall {
                function: FunctionId(0),
                ..
            }
        ));
    }

    #[test]
    fn reports_invalid_if_condition_and_branch_mismatch() {
        let invalid_condition = lower_source("define function main()\n  if (1) 2 else 3 end\nend;");
        assert!(has_diagnostic(
            &invalid_condition,
            DiagnosticCode::IfConditionTypeMismatch,
            &[("expected", "<boolean>"), ("actual", "<integer>")]
        ));

        let mismatched_branches =
            lower_source("define function main(x)\n  if (x = 0) #t else 3 end\nend;");
        assert!(has_diagnostic(
            &mismatched_branches,
            DiagnosticCode::IfBranchTypeMismatch,
            &[("then", "<boolean>"), ("else", "<integer>")]
        ));
    }

    #[test]
    fn resolves_signal_handle_and_restart() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let bad_id = class_id(&module, "<bad>");
        assert_eq!(module.functions[0].result_kinds, [ValueKind::I64]);
        let Expr::Handle { body, restart, .. } = &module.functions[0].body.result else {
            panic!("expected handle expression");
        };
        assert_eq!(restart.specializer, bad_id);
        assert_eq!(restart.result_kinds, [ValueKind::I64]);
        let condition_local = restart
            .condition_local
            .expect("restart should bind condition object");
        assert_eq!(
            module.functions[0].locals[condition_local.0 as usize].kind,
            ValueKind::Object(bad_id)
        );
        assert!(matches!(
            restart.body.as_ref(),
            Expr::AccessorCall {
                receiver,
                class,
                slot: SlotId(0),
                ..
            } if *class == bad_id
                && matches!(receiver.as_ref(), Expr::Local { local, .. } if *local == condition_local)
        ));
        assert!(matches!(
            body.as_ref(),
            Expr::Signal {
                condition_class,
                result_kinds,
                ..
            } if *condition_class == bad_id && result_kinds == &[ValueKind::I64]
        ));
    }

    #[test]
    fn restart_condition_binding_is_handler_scoped() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  let recovered = handle signal(make(<bad>, fallback: 0)) with restart e :: <bad> e.fallback end;\n  e.fallback\nend;",
        );

        assert!(has_diagnostic(
            &module,
            DiagnosticCode::UnresolvedLocal,
            &[("local", "e")]
        ));
    }

    #[test]
    fn reports_invalid_signal_and_restart_uses() {
        let dynamic = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  signal(make(<bad>, fallback: 0))\nend;",
        );
        assert!(
            dynamic.diagnostics.is_empty(),
            "a signal outside any lexical handler is searched for at run time"
        );

        let float_branch = lower_source(
            "define class <bad> (<object>)\nend;\n\
             define function pick(x :: <double-float>)\n  if (x < 0.0) signal(make(<bad>)) else x end\nend;",
        );
        assert!(
            float_branch.diagnostics.is_empty(),
            "a diverging signal adopts its sibling branch's kinds: {:?}",
            float_branch.diagnostics
        );

        let mismatched_restart = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle 1 with restart <bad> #f end\nend;",
        );
        assert!(has_diagnostic(
            &mismatched_restart,
            DiagnosticCode::HandleRestartResultMismatch,
            &[("body", "<integer>"), ("restart", "<boolean>")]
        ));
    }

    #[test]
    fn reports_unresolved_names() {
        let module = lower_source("define function main()\n  missing(x)\nend;");
        assert_eq!(module.diagnostics.len(), 2);
        assert!(has_diagnostic(
            &module,
            DiagnosticCode::UnresolvedFunctionOrGeneric,
            &[("callee", "missing")]
        ));
        assert!(has_diagnostic(
            &module,
            DiagnosticCode::UnresolvedLocal,
            &[("local", "x")]
        ));
    }

    #[test]
    fn unresolved_hyphenated_names_suggest_the_spaced_form() {
        let hint = |body: &str| {
            let module = lower_source(&format!(
                "define constant $limit = 3;\n\
                 define function main()\n  let x = 10;\n  let a = 7;\n  {body}\nend;"
            ));
            assert_eq!(module.diagnostics.len(), 1, "{:?}", module.diagnostics);
            let diagnostic = &module.diagnostics[0];
            let spaced = diagnostic
                .args
                .iter()
                .find(|arg| arg.name == "spaced")
                .map(|arg| arg.value.render());
            (diagnostic.code, spaced)
        };
        let spaced = |text: &str| {
            (
                DiagnosticCode::UnresolvedHyphenatedLocal,
                Some(text.to_string()),
            )
        };

        assert_eq!(hint("x-1"), spaced("x - 1"));
        assert_eq!(hint("a-x"), spaced("a - x"));
        assert_eq!(hint("$limit-1"), spaced("$limit - 1"));
        assert_eq!(hint("x-2i32"), spaced("x - 2i32"));
        // A closure sees `x` in its enclosing scope.
        assert_eq!(hint("method() x-1 end"), spaced("x - 1"));
        // Without a bound name on the left and an operand on the right, it
        // is an ordinary unresolved local.
        assert_eq!(hint("y-1"), (DiagnosticCode::UnresolvedLocal, None));
        assert_eq!(hint("x-y"), (DiagnosticCode::UnresolvedLocal, None));
        assert_eq!(hint("x--1"), (DiagnosticCode::UnresolvedLocal, None));
        assert_eq!(hint("x-"), (DiagnosticCode::UnresolvedLocal, None));
    }

    #[test]
    fn method_origin_session_distinguishes_mutation_sequence() {
        let first = MethodOrigin::Session { sequence: 0 };
        let same = MethodOrigin::Session { sequence: 0 };
        let later = MethodOrigin::Session { sequence: 7 };

        assert_eq!(first, same);
        assert_ne!(first, later);
        assert_ne!(first, MethodOrigin::Source);
        assert_ne!(
            first,
            MethodOrigin::SlotSetter {
                class: ClassId(1),
                slot: SlotId(0),
            }
        );

        let copied = first;
        assert_eq!(copied, first);
    }

    const SHAPES: &str = "define class <shape> (<object>)\nend;\n\
         define class <circle> (<shape>)\n  slot radius :: <integer>;\nend;\n\
         define function radius-of(c :: <circle>) => <integer>\n  c.radius\nend;\n\
         define function size-of(s :: <shape>) => <integer>\n  1\nend;\n\
         define function circle-of(x :: <integer>) => <circle>\n  make(<circle>, radius: x)\nend;\n\
         define function shape-of(x :: <integer>) => <shape>\n  make(<circle>, radius: x)\nend;\n";

    fn lower_shapes(rest: &str) -> Module {
        lower_source(&format!("{SHAPES}{rest}"))
    }

    #[test]
    fn callable_parameters_are_contravariant_everywhere_signatures_meet() {
        let parameter = [
            ("expected", "callable(<shape>) => <integer>"),
            ("actual", "callable(<circle>) => <integer>"),
            ("index", "1"),
            ("accepted", "<circle>"),
            ("passed", "<shape>"),
        ];
        for site in [
            "define function f(g :: callable(<shape>) => <integer>)\n  1\nend;\n\
             define function main()\n  f(radius-of)\nend;",
            "define function f(g :: callable(<shape>) => <integer>)\n  1\nend;\n\
             define function main()\n  (f)(radius-of)\nend;",
            "define function main()\n  let g :: callable(<shape>) => <integer> = radius-of;\n  1\nend;",
            "define function two()\n  values(radius-of, 1)\nend;\n\
             define function main()\n  let (g :: callable(<shape>) => <integer>, n :: <integer>) = two();\n  n\nend;",
            "define constant k :: callable(<shape>) => <integer> = radius-of;\n\
             define function main()\n  1\nend;",
        ] {
            let module = lower_shapes(site);
            assert_eq!(
                module.diagnostics.len(),
                1,
                "{site}: {:?}",
                module.diagnostics
            );
            assert!(
                has_diagnostic(
                    &module,
                    DiagnosticCode::CallableParameterVariance,
                    &parameter
                ),
                "{site}: {:?}",
                module.diagnostics
            );
        }

        let widened = lower_shapes(
            "define function f(g :: callable(<circle>) => <integer>)\n  1\nend;\n\
             define function main()\n  let g :: callable(<circle>) => <integer> = size-of;\n  f(size-of)\nend;",
        );
        assert!(widened.diagnostics.is_empty(), "{:?}", widened.diagnostics);
    }

    #[test]
    fn callable_results_are_covariant() {
        let wider = lower_shapes(
            "define function main()\n  let g :: callable(<integer>) => <circle> = shape-of;\n  1\nend;",
        );
        assert!(has_diagnostic(
            &wider,
            DiagnosticCode::CallableResultVariance,
            &[
                ("expected", "callable(<integer>) => <circle>"),
                ("actual", "callable(<integer>) => <shape>"),
                ("index", "1"),
                ("returned", "<shape>"),
                ("relied", "<circle>"),
            ]
        ));

        let narrower = lower_shapes(
            "define function main()\n  let g :: callable(<integer>) => <shape> = circle-of;\n  1\nend;",
        );
        assert!(
            narrower.diagnostics.is_empty(),
            "{:?}",
            narrower.diagnostics
        );
    }

    #[test]
    fn callable_signatures_keep_register_types() {
        // `<object>` is boxed and a class is not, so no signature names one
        // for the other, though the subtyping alone would allow it.
        let boxed = lower_shapes(
            "define function f(g :: callable(<integer>) => <object>)\n  1\nend;\n\
             define function main()\n  f(circle-of)\nend;",
        );
        assert!(has_diagnostic(
            &boxed,
            DiagnosticCode::FunctionArgumentTypeMismatch,
            &[
                ("expected", "callable(<integer>) => <object>"),
                ("actual", "callable(<integer>) => <circle>"),
            ]
        ));
    }

    #[test]
    fn calls_through_callable_values_cast_class_results() {
        let module = lower_shapes(
            "define function through(g :: callable(<integer>) => <circle>) => <integer>\n  \
               let picked :: <circle> = block (return)\n    return(circle-of(2))\n  end;\n  \
               g(1).radius + picked.radius\nend;",
        );
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let circle = module
            .classes
            .iter()
            .find(|class| class.name == "<circle>")
            .expect("circle class")
            .id;
        let through = module
            .functions
            .iter()
            .find(|function| function.name == "through")
            .expect("through function");
        let rendered = format!("{:?}", through.body);
        // `g` returns whatever it was handed, so its result is cast; the exit
        // procedure never returns, so its call is not.
        assert_eq!(rendered.matches("Cast {").count(), 1, "{rendered}");
        let Expr::Binary { lhs, .. } = &through.body.result else {
            panic!("expected an addition: {rendered}");
        };
        let Expr::AccessorCall { receiver, .. } = lhs.as_ref() else {
            panic!("expected a slot read: {rendered}");
        };
        assert!(matches!(
            receiver.as_ref(),
            Expr::Cast { class, value, .. }
                if *class == circle
                    && matches!(
                        value.as_ref(),
                        Expr::CallableCall { result_kinds, .. }
                            if result_kinds == &[ValueKind::Object(OBJECT_CLASS_ID)]
                    )
        ));
    }

    #[test]
    fn callable_annotations_reach_contracts_slots_and_nested_signatures() {
        let module = lower_shapes(
            "define class <holder> (<object>)\n  \
             slot measure :: callable(<shape>) => <integer>, init-keyword: measure:;\nend;\n\
             define generic chooser(s :: <shape>) => callable(<shape>) => <integer>;\n\
             define method chooser(s :: <shape>) => callable(<shape>) => <integer>\n  size-of\nend;\n\
             define function apply-to(f :: callable(callable(<shape>) => <integer>) => <integer>) => <integer>\n  \
             f(size-of)\nend;",
        );
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
        let holder = class(&module, "<holder>");
        let measure = CallableSignature::flat(
            vec![ValueKind::Object(class_id(&module, "<shape>"))],
            vec![ValueKind::I64],
        );
        assert_eq!(
            holder.direct_slots[0].callable_signature.as_ref(),
            Some(&measure)
        );
        assert_eq!(
            holder.direct_slots[0].type_name,
            "callable(<shape>) => <integer>"
        );
        let shown = |definition| {
            let contract = module.contract(definition).expect("contract");
            contract
                .params
                .iter()
                .chain(&contract.results)
                .map(|value| value.display(&module).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            shown(
                module
                    .generics
                    .iter()
                    .find(|g| g.name == "chooser")
                    .expect("generic")
                    .definition
            ),
            ["<shape>", "callable(<shape>) => <integer>"]
        );
        assert_eq!(
            shown(function(&module, "apply-to").definition),
            [
                "callable(callable(<shape>) => <integer>) => <integer>",
                "<integer>"
            ]
        );
    }

    #[test]
    fn nested_callable_signatures_compare_by_variance() {
        // Callers of `g` may hand it a callable of `<circle>`, but
        // `apply-to` calls its argument with any `<shape>`.
        let module = lower_shapes(
            "define function apply-to(f :: callable(<shape>) => <integer>) => <integer>\n  \
             f(make(<circle>, radius: 1))\nend;\n\
             define function main()\n  \
             let g :: callable(callable(<circle>) => <integer>) => <integer> = apply-to;\n  0\nend;",
        );
        assert!(has_diagnostic(
            &module,
            DiagnosticCode::CallableParameterVariance,
            &[
                ("accepted", "callable(<shape>) => <integer>"),
                ("passed", "callable(<circle>) => <integer>")
            ]
        ));
        // The wider nested parameter is accepted.
        let module = lower_shapes(
            "define function apply-to(f :: callable(<circle>) => <integer>) => <integer>\n  \
             f(make(<circle>, radius: 1))\nend;\n\
             define function main()\n  \
             let g :: callable(callable(<shape>) => <integer>) => <integer> = apply-to;\n  g(size-of)\nend;",
        );
        assert!(module.diagnostics.is_empty(), "{:?}", module.diagnostics);
    }
}
