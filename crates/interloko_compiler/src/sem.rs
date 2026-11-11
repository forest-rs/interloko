//! Semantic IR for the first generic-dispatch Interloko slice.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::ast;
use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::{DefinitionId, DefinitionKind, SourceFileId};
use crate::syntax::Span;

pub use interloko_model::{
    BuiltinValueClass, ClassId, FloatBits, GenericFunctionId, MethodId, OBJECT_CLASS_ID, SlotId,
};
pub use interloko_runtime::FloatFunction;

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
    /// A generic function dispatcher.
    GenericFunction(GenericFunctionId),
    /// A getter accessor introduced by a slot declaration.
    SlotAccessor {
        /// Stable accessor identity.
        accessor: DefinitionId,
        /// Owning class.
        class: ClassId,
        /// Slot read by this accessor.
        slot: SlotId,
    },
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
    /// Boolean value.
    Bool,
    /// Runtime string object.
    String(ClassId),
    /// Runtime interned symbol object.
    Symbol(ClassId),
    /// Runtime immutable vector object.
    Vector(ClassId),
    /// Runtime object with statically known nominal class.
    Object(ClassId),
    /// Class metaobject naming a statically known nominal class.
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

/// Callable argument and result categories carried by a typed callable value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallableSignature {
    /// Parameter value categories in call order.
    pub params: Vec<ValueKind>,
    /// Result value categories in return order.
    pub results: Vec<ValueKind>,
}

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
        "vector-size" => Intrinsic::VectorSize,
        "vector-ref" => Intrinsic::VectorRef,
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
        | Intrinsic::VectorSize => 1,
        Intrinsic::VectorRef => 2,
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
        (Intrinsic::VectorSize | Intrinsic::VectorRef, 0) => "<vector>",
        (Intrinsic::VectorRef, 1) => "<integer>",
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
    /// Declared source type name, with missing declarations normalized to
    /// `<object>`.
    pub type_name: String,
    /// Declared slot value category.
    pub kind: ValueKind,
    /// Native or host-backed slot origin.
    pub origin: SlotOrigin,
    /// Getter accessor defined by this slot.
    pub accessor: Accessor,
    /// External construction keyword without the trailing colon.
    pub init_keyword: String,
    /// Whether the slot declared `required-init-keyword:`.
    pub required_init_keyword: bool,
    /// Optional default initializer used when construction omits this slot.
    pub init_value: Option<Expr>,
    /// Allocation policy.
    pub allocation: SlotAllocation,
    /// Mutability metadata.
    pub mutability: SlotMutability,
    /// Source span.
    pub span: Span,
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
    /// Declared source type name, with missing declarations normalized to
    /// `<object>`.
    pub type_name: String,
    /// Slot value category.
    pub kind: ValueKind,
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
    /// Arity.
    pub arity: usize,
    /// Zero-based parameter index used for nominal method dispatch.
    pub dispatch_param: usize,
    /// Zero-based parameter indexes considered by generic dispatch.
    pub dispatch_params: Vec<usize>,
    /// Result value categories in return order.
    pub result_kinds: Vec<ValueKind>,
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Source span.
    pub span: Span,
    /// Source or synthesized origin.
    pub origin: GenericFunctionOrigin,
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
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Source span.
    pub span: Span,
    /// Source or synthesized origin.
    pub origin: MethodOrigin,
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
    /// Local table containing parameters followed by `let` bindings.
    pub locals: Vec<Local>,
    /// Function body.
    pub body: Block,
    /// Result value categories in return order.
    pub result_kinds: Vec<ValueKind>,
    /// Typed callable signatures for callable results, when declared.
    pub result_callable_sigs: Vec<Option<CallableSignature>>,
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Source span.
    pub span: Span,
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
    /// Explicit declared result categories, when present. `None` means the
    /// source declaration has an open result contract and the concrete result
    /// categories are inferred for lowering.
    pub return_annotation: Option<Vec<ValueKind>>,
    /// Source span.
    pub span: Span,
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
    /// Result value category.
    pub result_kind: ValueKind,
    /// Typed callable signature when the constant is a typed callable value.
    pub callable_signature: Option<CallableSignature>,
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
    /// Source span of the binding name.
    pub span: Span,
}

/// Semantic function or method body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// Local `let` bindings.
    pub lets: Vec<Let>,
    /// Final result expression.
    pub result: Expr,
    /// Source span.
    pub span: Span,
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
    /// Narrowed local kind in the true branch.
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
}

impl From<ast::BinaryOp> for BinaryOp {
    fn from(value: ast::BinaryOp) -> Self {
        match value {
            ast::BinaryOp::Add => Self::Add,
            ast::BinaryOp::Sub => Self::Sub,
            ast::BinaryOp::Mul => Self::Mul,
            ast::BinaryOp::Div => Self::Div,
        }
    }
}

/// Unboxed numeric category of a primitive arithmetic or ordering operation.
///
/// Loko operators are statically typed primitives, not generic functions:
/// both operands must have the same numeric kind, and there is no implicit
/// integer-to-float contagion. Mixed operands are a diagnostic; convert with
/// `as(<float>, n)` instead.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NumericKind {
    /// Signed 64-bit integers, source `<integer>`.
    Integer,
    /// IEEE 754 binary64 floats, source `<float>`.
    Float,
}

impl NumericKind {
    /// Returns the semantic value category of this numeric kind.
    #[must_use]
    pub const fn value_kind(self) -> ValueKind {
        match self {
            Self::Integer => ValueKind::I64,
            Self::Float => ValueKind::F64,
        }
    }

    /// Returns the numeric kind of `kind`, when it is numeric.
    #[must_use]
    pub const fn of(kind: ValueKind) -> Option<Self> {
        match kind {
            ValueKind::I64 => Some(Self::Integer),
            ValueKind::F64 => Some(Self::Float),
            _ => None,
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
/// converted implicitly. Operations with a tape instruction lower inline;
/// [`FloatBuiltin::Library`] functions call the runtime host.
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
    /// A runtime library function such as `sqrt` or `sin`.
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
        Self::Library(FloatFunction::Floor),
        Self::Library(FloatFunction::Ceiling),
        Self::Library(FloatFunction::Round),
        Self::Library(FloatFunction::Truncate),
        Self::Library(FloatFunction::Sqrt),
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
            Self::Library(function) => match function {
                FloatFunction::Floor => "floor",
                FloatFunction::Ceiling => "ceiling",
                FloatFunction::Round => "round",
                FloatFunction::Truncate => "truncate",
                FloatFunction::Sqrt => "sqrt",
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
            Self::Abs | Self::IsNan | Self::IsFinite => 1,
            Self::Library(function) => function.arity(),
        }
    }

    /// Returns the result category: `<boolean>` for predicates, otherwise
    /// `<float>`.
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
    /// Returns an immutable vector's element count.
    VectorSize,
    /// Returns one immutable vector element as `<object>`.
    VectorRef,
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
    /// Boolean constant.
    ConstBool {
        /// Constant value.
        value: bool,
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
    /// Resolved class object value.
    ClassObject {
        /// Nominal class represented by this value.
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
        /// Source span.
        span: Span,
    },
    /// Call through a first-class callable value.
    CallableCall {
        /// Callable expression.
        callable: Box<Expr>,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Expected argument categories for this indirect call.
        arg_kinds: Vec<ValueKind>,
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
        /// Source span.
        span: Span,
    },
    /// Runtime checked object cast.
    Cast {
        /// Cast value.
        value: Box<Expr>,
        /// Target class.
        class: ClassId,
        /// Source span.
        span: Span,
    },
    /// Runtime object construction.
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
    /// Conversion between `<integer>` and `<float>`, from source
    /// `as(<float>, n)` or `as(<integer>, x)`.
    ///
    /// Integer to float rounds to nearest, ties to even. Float to integer is
    /// exact: it traps unless the operand is an integral value in the `i64`
    /// range, so rounding is always an explicit `round`/`floor`/... choice.
    Convert {
        /// Numeric kind of the result; the operand has the other kind.
        to: NumericKind,
        /// Converted operand.
        value: Box<Expr>,
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
    /// Recoverable condition handler expression.
    Handle {
        /// Handled body expression.
        body: Box<Expr>,
        /// Restart clause for the handled body.
        restart: Restart,
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
    /// Returns this expression's source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::ConstI64 { span, .. }
            | Self::ConstF64 { span, .. }
            | Self::ConstBool { span, .. }
            | Self::StringLiteral { span, .. }
            | Self::SymbolLiteral { span, .. }
            | Self::VectorLiteral { span, .. }
            | Self::Values { span, .. }
            | Self::Local { span, .. }
            | Self::Capture { span, .. }
            | Self::ConstantRef { span, .. }
            | Self::ClassObject { span, .. }
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
            | Self::Cast { span, .. }
            | Self::MakeObject { span, .. }
            | Self::AccessorCall { span, .. }
            | Self::SlotWrite { span, .. }
            | Self::Binary { span, .. }
            | Self::Negate { span, .. }
            | Self::Convert { span, .. }
            | Self::FloatCall { span, .. }
            | Self::Compare { span, .. }
            | Self::ValueEqual { span, .. }
            | Self::If { span, .. }
            | Self::Handle { span, .. }
            | Self::Error { span } => *span,
        }
    }
}

/// Semantic restart clause for a lexical handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Restart {
    /// Nominal condition class handled by this restart.
    pub specializer: ClassId,
    /// Optional handler-local bound to the signaled condition object.
    pub condition_local: Option<LocalId>,
    /// Handler body evaluated when a matching condition is signaled.
    pub body: Box<Expr>,
    /// Handler result categories.
    pub result_kinds: Vec<ValueKind>,
    /// Typed callable signatures for handler results, when known.
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
    let mut lowering = Lowering::new(module, registry);
    lowering.lower_module(module)
}

#[derive(Clone, Debug)]
struct FunctionSymbol {
    name: String,
    id: FunctionId,
    param_kinds: Vec<ValueKind>,
    param_callable_sigs: Vec<Option<CallableSignature>>,
    return_kinds: Vec<ValueKind>,
    return_callable_sigs: Vec<Option<CallableSignature>>,
    return_annotation: Option<Vec<ValueKind>>,
    span: Span,
}

impl FunctionSymbol {
    fn arity(&self) -> usize {
        self.param_kinds.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedType {
    kind: ValueKind,
    callable_signature: Option<CallableSignature>,
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
    span: Span,
}

#[derive(Clone, Debug)]
struct GenericSymbol {
    name: String,
    id: GenericFunctionId,
    param_kinds: Vec<GenericParamKind>,
    return_kinds: Vec<ValueKind>,
    return_callable_sigs: Vec<Option<CallableSignature>>,
    return_annotation: Option<Vec<ValueKind>>,
    dispatch_param: usize,
    span: Span,
}

impl GenericSymbol {
    fn arity(&self) -> usize {
        self.param_kinds.len()
    }
}

struct FunctionContext {
    locals: Vec<Local>,
    visible_locals: Vec<bool>,
    capture_sources: Vec<Local>,
    captures: Vec<Capture>,
    active_restarts: Vec<ActiveRestart>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ActiveRestart {
    specializer: ClassId,
    result_kinds: Vec<ValueKind>,
    result_callable_sigs: Vec<Option<CallableSignature>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AccessorResolution {
    None,
    One(KnownCallable),
    Ambiguous,
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
            diagnostics: Vec::new(),
        }
    }

    fn for_closure(capture_sources: Vec<Local>) -> Self {
        Self {
            locals: Vec::new(),
            visible_locals: Vec::new(),
            capture_sources,
            captures: Vec::new(),
            active_restarts: Vec::new(),
            diagnostics: Vec::new(),
        }
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
        self.locals.push(Local {
            id,
            definition,
            name,
            kind,
            callable_signature,
            span,
        });
        self.visible_locals.push(true);
        id
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

    fn visible_local_sources(&self) -> Vec<Local> {
        self.locals
            .iter()
            .enumerate()
            .filter(|(index, _)| self.visible_locals.get(*index).copied().unwrap_or(false))
            .map(|(_, local)| local.clone())
            .collect()
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

    fn active_restart(&self) -> Option<ActiveRestart> {
        self.active_restarts.last().cloned()
    }
}

struct Lowering {
    source_file: SourceFileId,
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
    diagnostics: Vec<Diagnostic>,
}

impl Lowering {
    fn new(module: &ast::Module, registry: &HostMetaRegistry) -> Self {
        let mut lowering = Self {
            source_file: module.source_file,
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
            diagnostics: module.diagnostics.clone(),
        };
        lowering.collect_classes(module);
        lowering.collect_host_functions();
        lowering.collect_generics(module);
        lowering.collect_functions(module);
        lowering.collect_constant_symbols(module);
        lowering.collect_slot_init_values(module);
        lowering.finalize_classes();
        lowering.collect_slot_setter_generics();
        lowering.check_host_function_conflicts();
        lowering.lower_constants(module);
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
                    let kind = self.resolve_slot_kind(slot.type_name.as_deref(), slot.type_span);
                    Slot {
                        id,
                        definition: slot.definition,
                        name: slot.name.clone(),
                        type_name: slot.type_name.clone().unwrap_or_else(|| "<object>".into()),
                        kind,
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
                finalized: false,
                direct_slots: slots,
                effective_slots: Vec::new(),
                initialization_plan: InitializationPlan::default(),
                span: Span::default(),
            });
        }

        self.collect_builtin_value_classes();
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
                finalized: false,
                direct_slots: Vec::new(),
                effective_slots: Vec::new(),
                initialization_plan: InitializationPlan::default(),
                span: Span::default(),
            });
        }
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
            if matches!(superclass, "<integer>" | "<boolean>")
                || BuiltinValueClass::from_name(superclass).is_some()
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

    fn collect_slot_init_values(&mut self, module: &ast::Module) {
        let mut lowered_values = Vec::new();
        for (class_index, class) in module.classes.iter().enumerate() {
            let lowered_class_index = class_index + 1;
            for (slot_index, slot) in class.slots.iter().enumerate() {
                let Some(init_value) = &slot.init_value else {
                    continue;
                };
                let mut ctx = FunctionContext::new();
                let value = self.lower_expr(&mut ctx, init_value);
                self.diagnostics.extend(ctx.diagnostics);
                let slot_kind = self
                    .classes
                    .get(lowered_class_index)
                    .and_then(|class| class.direct_slots.get(slot_index))
                    .map_or(ValueKind::Error, |slot| slot.kind);
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
                }
                lowered_values.push((lowered_class_index, slot_index, value));
            }
        }

        for (class_index, slot_index, value) in lowered_values {
            if let Some(slot) = self
                .classes
                .get_mut(class_index)
                .and_then(|class| class.direct_slots.get_mut(slot_index))
            {
                slot.init_value = Some(value);
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
            let param_kinds = generic
                .params
                .iter()
                .map(|param| {
                    param
                        .type_annotation
                        .as_ref()
                        .map(|annotation| self.resolve_type_annotation(annotation))
                })
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
                            | ValueKind::Vector(_)
                            | ValueKind::Error
                    )
                {
                    self.diagnostics.push(
                        Diagnostic::error(
                            generic
                                .params
                                .get(param_index)
                                .map_or(generic.span, |param| param.span),
                            DiagnosticCode::GenericArgumentTypeMismatch,
                        )
                        .with_arg(DiagnosticArg::text("generic", generic.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "argument_index",
                            u64::try_from(param_index + 1).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::text("expected", "<object>"))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(*kind))),
                    );
                }
            }
            let (return_annotation, return_callable_sigs) =
                generic.return_type.as_ref().map_or_else(
                    || (None, vec![None; provisional_open_result_kinds().len()]),
                    |annotation| {
                        let (kinds, callable_sigs) =
                            self.resolve_result_annotation_detail(annotation);
                        (Some(kinds), callable_sigs)
                    },
                );
            let return_kinds = return_annotation
                .clone()
                .unwrap_or_else(provisional_open_result_kinds);
            self.generic_symbols.push(GenericSymbol {
                name: generic.name.clone(),
                id,
                param_kinds: param_kinds.clone(),
                return_kinds: return_kinds.clone(),
                return_callable_sigs: return_callable_sigs.clone(),
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
                dispatch_params: (0..generic.params.len()).collect(),
                result_kinds: return_kinds,
                return_annotation,
                span: generic.span,
                origin: GenericFunctionOrigin::Source,
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
                        slot.kind,
                        slot.span,
                    ))
                })
            })
            .collect::<Vec<_>>();

        for (class, slot, slot_definition, slot_name, slot_kind, slot_span) in slots {
            let setter_name = slot_setter_name(&slot_name);
            if self
                .generic_symbols
                .iter()
                .any(|symbol| symbol.name == setter_name)
            {
                continue;
            }
            let id = GenericFunctionId(u32::try_from(self.generics.len()).unwrap_or(u32::MAX));
            let definition = DefinitionId::new(
                self.source_file,
                DefinitionKind::Generic,
                &setter_name,
                &format!("slot-setter:{}", slot_definition.raw()),
            );
            let param_kinds = vec![Some(slot_kind), Some(ValueKind::Any)];
            let origin = GenericFunctionOrigin::SlotSetter { class, slot };
            self.generic_symbols.push(GenericSymbol {
                name: setter_name.clone(),
                id,
                param_kinds: param_kinds.clone(),
                return_kinds: vec![slot_kind],
                return_callable_sigs: vec![None],
                return_annotation: Some(vec![slot_kind]),
                dispatch_param: 1,
                span: slot_span,
            });
            self.generics.push(GenericFunction {
                id,
                definition,
                name: setter_name,
                params: vec!["new-value".into(), "object".into()],
                param_kinds,
                arity: 2,
                dispatch_param: 1,
                dispatch_params: vec![0, 1],
                result_kinds: vec![slot_kind],
                return_annotation: Some(vec![slot_kind]),
                span: slot_span,
                origin,
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
            let Some(generic) = self.resolve_generic(&setter_name).map(|symbol| symbol.id) else {
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
            let body = Block {
                lets: Vec::new(),
                result: Expr::SlotWrite {
                    setter: definition,
                    receiver: Box::new(Expr::Local {
                        local: object_param,
                        result_kind: ValueKind::Object(class),
                        callable_signature: None,
                        span: slot.span,
                    }),
                    value: Box::new(Expr::Local {
                        local: value_param,
                        result_kind: slot.kind,
                        callable_signature: None,
                        span: slot.span,
                    }),
                    class,
                    slot: slot.id,
                    result_kind: slot.kind,
                    span: slot.span,
                },
                span: slot.span,
            };
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
                result_kinds: vec![slot.kind],
                return_annotation: Some(vec![slot.kind]),
                span: slot.span,
                origin: MethodOrigin::SlotSetter {
                    class,
                    slot: slot.id,
                },
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
            if let Some(previous) = self
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
            let param_kinds = function
                .params
                .iter()
                .map(|param| {
                    self.resolve_function_type_detail(
                        param.type_annotation.as_ref(),
                        ValueKind::I64,
                    )
                })
                .collect::<Vec<_>>();
            let param_callable_sigs = param_kinds
                .iter()
                .map(|param| param.callable_signature.clone())
                .collect::<Vec<_>>();
            let param_kinds = param_kinds
                .into_iter()
                .map(|param| param.kind)
                .collect::<Vec<_>>();
            let (return_annotation, return_callable_sigs) =
                function.return_type.as_ref().map_or_else(
                    || (None, vec![None; provisional_open_result_kinds().len()]),
                    |annotation| {
                        let (kinds, callable_sigs) =
                            self.resolve_result_annotation_detail(annotation);
                        (Some(kinds), callable_sigs)
                    },
                );
            let return_kinds = return_annotation
                .clone()
                .unwrap_or_else(provisional_open_result_kinds);
            self.functions.push(FunctionSymbol {
                name: function.name.clone(),
                id: FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                param_kinds,
                param_callable_sigs,
                return_kinds,
                return_callable_sigs,
                return_annotation,
                span: function.span,
            });
        }
    }

    fn collect_constant_symbols(&mut self, module: &ast::Module) {
        for (index, constant) in module.constants.iter().enumerate() {
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
                annotation: annotation.map(|annotation| annotation.kind),
                span: constant.span,
            });
        }
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
            let value = self.lower_expr_expected(&mut ctx, &constant.value, expected_kind);
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
                self.diagnostics.push(
                    Diagnostic::error(constant.value.span(), DiagnosticCode::ConstantTypeMismatch)
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
                        )),
                );
            }
            let result_kind = expected_kind.unwrap_or(actual_kind);
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
                result_kind,
                callable_signature,
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
        self.methods.clear();
        for method in &module.methods {
            let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
            let method = self.lower_method(id, method);
            self.methods.push(method);
        }
        self.collect_slot_setter_methods();
        self.refresh_inferred_generic_results();
        self.check_ambiguous_generic_methods();

        let functions = module
            .functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                self.lower_function(
                    FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                    function,
                )
            })
            .collect();

        Module {
            source_file: module.source_file,
            classes: self.classes.clone(),
            generics: self.generics.clone(),
            methods: self.methods.clone(),
            functions,
            closures: self.closures.clone(),
            constants: self.constants.clone(),
            host_functions: self.host_functions.clone(),
            diagnostics: self.diagnostics.clone(),
        }
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
            .saturating_add(2);
        for _ in 0..max_passes {
            let before = self.result_signature_snapshot();
            self.diagnostics.truncate(diagnostic_len);
            self.closures.truncate(closure_len);
            self.methods.clear();

            for method in &module.methods {
                let id = MethodId(u32::try_from(self.methods.len()).unwrap_or(u32::MAX));
                let method = self.lower_method(id, method);
                self.methods.push(method);
            }
            self.collect_slot_setter_methods();
            self.refresh_inferred_generic_results();
            for (index, function) in module.functions.iter().enumerate() {
                let _ = self.lower_function(
                    FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                    function,
                );
            }

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

    fn join_result_kind(&self, current: ValueKind, next: ValueKind) -> Option<ValueKind> {
        if current == next {
            return Some(current);
        }
        if current == ValueKind::Error {
            return Some(next);
        }
        if next == ValueKind::Error {
            return Some(current);
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
        let generic_symbol = self.resolve_generic(&method.name).cloned();
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
            let (specializer, kind) = self.resolve_method_specializer(param);
            if let Some(symbol) = &generic_symbol
                && let Some(expected) = symbol.param_kinds.get(index).copied().flatten()
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
        let return_annotation = method
            .return_type
            .as_ref()
            .map(|annotation| self.resolve_result_annotation(annotation));
        let body = self.lower_block(&mut ctx, &method.body, return_annotation.as_deref());
        let body_kinds = self.expr_kinds(&body.result);
        let result_kinds = return_annotation
            .clone()
            .unwrap_or_else(|| body_kinds.clone());
        if let Some(expected) = &return_annotation
            && !self.result_kinds_match(expected, &body_kinds)
        {
            ctx.diagnostics.push(
                Diagnostic::error(body.result.span(), DiagnosticCode::MethodReturnTypeMismatch)
                    .with_arg(DiagnosticArg::text("method", method.name.clone()))
                    .with_arg(DiagnosticArg::text(
                        "expected",
                        self.result_kinds_name(expected),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "actual",
                        self.result_kinds_name(&body_kinds),
                    )),
            );
        }
        if let Some(symbol) = &generic_symbol
            && let Some(expected) = &symbol.return_annotation
            && !self.result_kinds_match(expected, &result_kinds)
        {
            ctx.diagnostics.push(
                Diagnostic::error(
                    body.result.span(),
                    DiagnosticCode::GenericMethodResultTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("method", method.name.clone()))
                .with_arg(DiagnosticArg::text(
                    "actual",
                    self.result_kinds_name(&result_kinds),
                ))
                .with_arg(DiagnosticArg::text(
                    "expected",
                    self.result_kinds_name(expected),
                )),
            );
        }
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
            return_annotation,
            span: method.span,
            origin: MethodOrigin::Source,
        }
    }

    fn lower_function(&mut self, id: FunctionId, function: &ast::Function) -> Function {
        let mut ctx = FunctionContext::new();
        let mut params = Vec::new();
        let symbol = self.functions.get(id.0 as usize).cloned();

        for (index, param) in function.params.iter().enumerate() {
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
            params.push(ctx.add_local(
                self.local_definition(function.definition, &param.name),
                param.name.clone(),
                kind,
                callable_signature,
                param.span,
            ));
        }

        let return_annotation = symbol
            .as_ref()
            .and_then(|symbol| symbol.return_annotation.clone());
        let annotated_return_callable_sigs = symbol
            .as_ref()
            .map_or_else(Vec::new, |symbol| symbol.return_callable_sigs.clone());
        let body = self.lower_block(&mut ctx, &function.body, return_annotation.as_deref());
        let body_kinds = self.expr_kinds(&body.result);
        let body_callable_sigs = self.expr_result_callable_signatures(&body.result);
        let result_kinds = return_annotation
            .clone()
            .unwrap_or_else(|| body_kinds.clone());
        let result_callable_sigs = if return_annotation.is_some() {
            annotated_return_callable_sigs
        } else {
            body_callable_sigs
        };
        if let Some(expected) = &return_annotation
            && !self.result_kinds_match(expected, &body_kinds)
        {
            ctx.diagnostics.push(
                Diagnostic::error(
                    body.result.span(),
                    DiagnosticCode::FunctionReturnTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("function", function.name.clone()))
                .with_arg(DiagnosticArg::text(
                    "expected",
                    self.result_kinds_name(expected),
                ))
                .with_arg(DiagnosticArg::text(
                    "actual",
                    self.result_kinds_name(&body_kinds),
                )),
            );
        }
        if let Some(symbol) = self.functions.get_mut(id.0 as usize)
            && symbol.return_annotation.is_none()
        {
            symbol.return_kinds = result_kinds.clone();
            symbol.return_callable_sigs = result_callable_sigs.clone();
        }
        self.diagnostics.extend(ctx.diagnostics);
        let param_callable_sigs = symbol
            .as_ref()
            .map_or_else(Vec::new, |symbol| symbol.param_callable_sigs.clone());

        Function {
            id,
            definition: function.definition,
            name: function.name.clone(),
            params,
            param_kinds: symbol.map_or_else(Vec::new, |symbol| symbol.param_kinds),
            param_callable_sigs,
            locals: ctx.locals,
            body,
            result_kinds,
            result_callable_sigs,
            return_annotation,
            span: function.span,
        }
    }

    fn lower_block(
        &mut self,
        ctx: &mut FunctionContext,
        block: &ast::Block,
        expected_result: Option<&[ValueKind]>,
    ) -> Block {
        let mut lets = Vec::new();
        for local in &block.lets {
            match &local.pattern {
                ast::LetPattern::Name(binding) => {
                    let expected_type = binding.type_annotation.as_ref().map(|annotation| {
                        self.resolve_type_annotation_detail_in_context(ctx, annotation)
                    });
                    let expected_kind = expected_type.as_ref().map(|resolved| resolved.kind);
                    let value = match expected_kind {
                        Some(kind) => self.lower_expr_expected_results(
                            ctx,
                            &local.value,
                            Some(core::slice::from_ref(&kind)),
                        ),
                        None => self.lower_expr_expected_results(ctx, &local.value, None),
                    };
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
                    if let Some(expected) = expected_kind
                        && !self.kinds_match(expected, actual_kind)
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
                        && !self.callable_signatures_match(
                            Some(expected),
                            actual_callable_signature.as_ref(),
                        )
                    {
                        ctx.diagnostics.push(
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
                            )),
                        );
                    }
                    let local_kind = expected_kind.unwrap_or(actual_kind);
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
                    lets.push(Let {
                        local: id,
                        locals: vec![id],
                        value: value.clone(),
                        span: local.span,
                    });
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
                    if bindings.len() != actual_kinds.len() {
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
                            ctx.diagnostics.push(
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
                                )),
                            );
                        }
                        let kind = expected_kind.unwrap_or(actual_kind);
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
                        local_ids.push(id);
                    }
                    let local_id = local_ids.first().copied().unwrap_or(LocalId(u32::MAX));
                    lets.push(Let {
                        local: local_id,
                        locals: local_ids,
                        value: value.clone(),
                        span: local.span,
                    });
                }
            }
        }

        Block {
            lets,
            result: self.lower_expr_expected_results(ctx, &block.result, expected_result),
            span: block.span,
        }
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
        self.require_scalar_result(ctx, &lowered, expr.span(), context);
        lowered
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
            ast::Expr::Number { value, span, .. } => Expr::ConstI64 {
                value: *value,
                span: *span,
            },
            ast::Expr::Float { value, span, .. } => Expr::ConstF64 {
                value: *value,
                span: *span,
            },
            ast::Expr::Bool { value, span, .. } => Expr::ConstBool {
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
                return_type,
                body,
                span,
                ..
            } => self.lower_closure_expr(ctx, params, return_type.as_ref(), body, *span),
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
                    None => self.lower_known_name(ctx, name, *span),
                },
            },
            ast::Expr::ClassName {
                class_name, span, ..
            } => self.lower_class_object(ctx, class_name, *span),
            ast::Expr::InstanceOf {
                value,
                class_name,
                span,
                ..
            } => self.lower_instance_of(ctx, value, class_name, *span),
            ast::Expr::Cast {
                class_name,
                value,
                span,
                ..
            } => self.lower_cast(ctx, value, class_name, *span),
            ast::Expr::Call {
                callee, args, span, ..
            } => {
                if callee == "apply" {
                    return self.lower_apply(ctx, args, expected_results, *span);
                }
                if callee == "values" {
                    let values = args
                        .iter()
                        .map(|arg| self.lower_expr(ctx, arg))
                        .collect::<Vec<_>>();
                    let result_kinds = values
                        .iter()
                        .map(|value| self.expr_kind(value))
                        .collect::<Vec<_>>();
                    return Expr::Values {
                        values,
                        result_kinds,
                        span: *span,
                    };
                }
                let lowered_args: Vec<_> =
                    args.iter().map(|arg| self.lower_expr(ctx, arg)).collect();
                if callee == "class-of" {
                    if args.len() == 1 {
                        let object = lowered_args.into_iter().next().expect("arity was checked");
                        return self.lower_class_of(ctx, object, *span);
                    }
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::FunctionArityMismatch)
                            .with_arg(DiagnosticArg::text("callee", "class-of"))
                            .with_arg(DiagnosticArg::unsigned("expected_count", 1))
                            .with_arg(DiagnosticArg::unsigned(
                                "actual_count",
                                u64::try_from(args.len()).unwrap_or(u64::MAX),
                            )),
                    );
                    return Expr::Error { span: *span };
                }
                if let Some(call) = self.lower_intrinsic_call(ctx, callee, &lowered_args, *span) {
                    return call;
                }
                if let Some(symbol) = self.resolve_function(callee) {
                    if symbol.arity() == args.len() {
                        for (index, (arg, expected)) in
                            lowered_args.iter().zip(&symbol.param_kinds).enumerate()
                        {
                            let actual = self.expr_kind(arg);
                            if !self.kinds_match(*expected, actual) {
                                ctx.diagnostics.push(
                                    Diagnostic::error(
                                        arg.span(),
                                        DiagnosticCode::FunctionArgumentTypeMismatch,
                                    )
                                    .with_arg(DiagnosticArg::text("function", callee.clone()))
                                    .with_arg(DiagnosticArg::unsigned(
                                        "argument_index",
                                        u64::try_from(index + 1).unwrap_or(u64::MAX),
                                    ))
                                    .with_arg(DiagnosticArg::text(
                                        "expected",
                                        self.kind_name(*expected),
                                    ))
                                    .with_arg(
                                        DiagnosticArg::text("actual", self.kind_name(actual)),
                                    ),
                                );
                            }
                            if let Some(expected) = symbol
                                .param_callable_sigs
                                .get(index)
                                .and_then(|signature| signature.as_ref())
                                && !self.callable_signatures_match(
                                    Some(expected),
                                    self.expr_callable_signature(arg).as_ref(),
                                )
                            {
                                ctx.diagnostics.push(
                                    Diagnostic::error(
                                        arg.span(),
                                        DiagnosticCode::FunctionArgumentTypeMismatch,
                                    )
                                    .with_arg(DiagnosticArg::text("function", callee.clone()))
                                    .with_arg(DiagnosticArg::unsigned(
                                        "argument_index",
                                        u64::try_from(index + 1).unwrap_or(u64::MAX),
                                    ))
                                    .with_arg(DiagnosticArg::text(
                                        "expected",
                                        self.callable_signature_name(expected),
                                    ))
                                    .with_arg(
                                        DiagnosticArg::text(
                                            "actual",
                                            self.expr_callable_signature(arg).as_ref().map_or_else(
                                                || self.kind_name(actual),
                                                |signature| self.callable_signature_name(signature),
                                            ),
                                        ),
                                    ),
                                );
                            }
                        }
                        return Expr::DirectCall {
                            function: symbol.id,
                            args: lowered_args,
                            span: *span,
                        };
                    }
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::FunctionArityMismatch)
                            .with_arg(DiagnosticArg::text("callee", callee.clone()))
                            .with_arg(DiagnosticArg::unsigned(
                                "expected_count",
                                u64::try_from(symbol.arity()).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::unsigned(
                                "actual_count",
                                u64::try_from(args.len()).unwrap_or(u64::MAX),
                            )),
                    );
                    return Expr::Error { span: *span };
                }
                if let Some(function) = self.resolve_host_function(callee).cloned() {
                    if function.param_kinds.len() == args.len() {
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
                                    .with_arg(DiagnosticArg::text("function", callee.clone()))
                                    .with_arg(DiagnosticArg::unsigned(
                                        "argument_index",
                                        u64::try_from(index + 1).unwrap_or(u64::MAX),
                                    ))
                                    .with_arg(DiagnosticArg::text(
                                        "expected",
                                        self.kind_name(*expected),
                                    ))
                                    .with_arg(
                                        DiagnosticArg::text("actual", self.kind_name(actual)),
                                    ),
                                );
                            }
                        }
                        return Expr::HostCall {
                            function: function.id,
                            args: lowered_args,
                            result_kind: function.result_kind,
                            span: *span,
                        };
                    }
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::FunctionArityMismatch)
                            .with_arg(DiagnosticArg::text("callee", callee.clone()))
                            .with_arg(DiagnosticArg::unsigned(
                                "expected_count",
                                u64::try_from(function.param_kinds.len()).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::unsigned(
                                "actual_count",
                                u64::try_from(args.len()).unwrap_or(u64::MAX),
                            )),
                    );
                    return Expr::Error { span: *span };
                }
                if let Some(symbol) = self.resolve_generic(callee) {
                    if symbol.arity() == args.len() {
                        for (index, (arg, expected)) in
                            lowered_args.iter().zip(&symbol.param_kinds).enumerate()
                        {
                            let Some(expected) = *expected else {
                                continue;
                            };
                            let actual = self.expr_kind(arg);
                            if !self.generic_argument_matches(expected, actual) {
                                ctx.diagnostics.push(
                                    Diagnostic::error(
                                        arg.span(),
                                        DiagnosticCode::GenericArgumentTypeMismatch,
                                    )
                                    .with_arg(DiagnosticArg::text("generic", callee.clone()))
                                    .with_arg(DiagnosticArg::unsigned(
                                        "argument_index",
                                        u64::try_from(index + 1).unwrap_or(u64::MAX),
                                    ))
                                    .with_arg(DiagnosticArg::text(
                                        "expected",
                                        self.kind_name(expected),
                                    ))
                                    .with_arg(
                                        DiagnosticArg::text("actual", self.kind_name(actual)),
                                    ),
                                );
                            }
                        }
                        return Expr::GenericCall {
                            generic: symbol.id,
                            args: lowered_args,
                            span: *span,
                        };
                    }
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::FunctionArityMismatch)
                            .with_arg(DiagnosticArg::text("callee", callee.clone()))
                            .with_arg(DiagnosticArg::unsigned(
                                "expected_count",
                                u64::try_from(symbol.arity()).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::unsigned(
                                "actual_count",
                                u64::try_from(args.len()).unwrap_or(u64::MAX),
                            )),
                    );
                    return Expr::Error { span: *span };
                }
                if self.has_accessor(callee) {
                    if args.len() == 1 {
                        let receiver = lowered_args.into_iter().next().expect("arity was checked");
                        return self.lower_accessor_call(ctx, receiver, callee, *span);
                    }
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::FunctionArityMismatch)
                            .with_arg(DiagnosticArg::text("callee", callee.clone()))
                            .with_arg(DiagnosticArg::unsigned("expected_count", 1))
                            .with_arg(DiagnosticArg::unsigned(
                                "actual_count",
                                u64::try_from(args.len()).unwrap_or(u64::MAX),
                            )),
                    );
                    return Expr::Error { span: *span };
                }
                if let Some(builtin) = FloatBuiltin::from_name(callee) {
                    return self.lower_float_call(ctx, builtin, lowered_args, *span);
                }
                ctx.diagnostics.push(
                    Diagnostic::error(*span, DiagnosticCode::UnresolvedFunctionOrGeneric)
                        .with_arg(DiagnosticArg::text("callee", callee.clone())),
                );
                Expr::Error { span: *span }
            }
            ast::Expr::Signal {
                condition, span, ..
            } => self.lower_signal(ctx, condition, *span),
            ast::Expr::MakeObject {
                class,
                fields,
                span,
                ..
            } => self.lower_make_object(ctx, class, fields, *span),
            ast::Expr::SlotRead {
                receiver,
                slot,
                span,
                ..
            } => self.lower_slot_read(ctx, receiver, slot, *span),
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
                let Some(kind) = self.numeric_operand_kind(
                    ctx,
                    *span,
                    &lhs,
                    &rhs,
                    DiagnosticCode::ArithmeticOperandTypeMismatch,
                ) else {
                    return Expr::Error { span: *span };
                };
                if op == BinaryOp::Div
                    && kind == NumericKind::Integer
                    && self.expr_kind(&lhs) == ValueKind::I64
                    && self.expr_kind(&rhs) == ValueKind::I64
                {
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        DiagnosticCode::IntegerDivisionUnsupported,
                    ));
                }
                Expr::Binary {
                    op,
                    kind,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span: *span,
                }
            }
            ast::Expr::UnaryMinus { expr, span, .. } => {
                let value = self.lower_expr_expected(ctx, expr, Some(ValueKind::I64));
                let value_kind = self.expr_kind(&value);
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
                Expr::Negate {
                    kind,
                    value: Box::new(value),
                    span: *span,
                }
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
            ast::Expr::If {
                condition,
                then_branch,
                else_branch,
                span,
                ..
            } => {
                let condition = self.lower_expr(ctx, condition);
                let refinement = self.true_branch_refinement(&condition);
                let previous_kind = refinement
                    .and_then(|refinement| ctx.refine_local(refinement.local, refinement.kind));
                let then_branch =
                    self.lower_expr_expected_results(ctx, then_branch, expected_results);
                if let (Some(refinement), Some(previous_kind)) = (refinement, previous_kind) {
                    let _ = ctx.refine_local(refinement.local, previous_kind);
                }
                let else_branch =
                    self.lower_expr_expected_results(ctx, else_branch, expected_results);
                let condition_kind = self.expr_kind(&condition);
                if !matches!(condition_kind, ValueKind::Bool | ValueKind::Error) {
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::IfConditionTypeMismatch)
                            .with_arg(DiagnosticArg::text("expected", "<boolean>"))
                            .with_arg(DiagnosticArg::text(
                                "actual",
                                self.kind_name(condition_kind),
                            )),
                    );
                }
                let then_kinds = self.expr_kinds(&then_branch);
                let else_kinds = self.expr_kinds(&else_branch);
                if !self.result_kinds_match(&then_kinds, &else_kinds)
                    && !result_kinds_contain_error(&then_kinds)
                    && !result_kinds_contain_error(&else_kinds)
                {
                    ctx.diagnostics.push(
                        Diagnostic::error(*span, DiagnosticCode::IfBranchTypeMismatch)
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
                    span: *span,
                }
            }
            ast::Expr::Handle {
                body,
                restart,
                span,
                ..
            } => self.lower_handle(ctx, body, restart, *span, expected_results),
            ast::Expr::Error { span, .. } => Expr::Error { span: *span },
        }
    }

    fn lower_known_name(&self, ctx: &mut FunctionContext, name: &str, span: Span) -> Expr {
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
        match self.resolve_accessor_callable(name) {
            AccessorResolution::One(callable) => Expr::KnownCallable { callable, span },
            AccessorResolution::Ambiguous => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::AmbiguousCallable)
                        .with_arg(DiagnosticArg::text("accessor", name)),
                );
                Expr::Error { span }
            }
            AccessorResolution::None => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::UnresolvedLocal)
                        .with_arg(DiagnosticArg::text("local", name)),
                );
                Expr::Error { span }
            }
        }
    }

    fn lower_closure_expr(
        &mut self,
        ctx: &mut FunctionContext,
        params: &[ast::FunctionParam],
        return_type: Option<&ast::ResultAnnotation>,
        body: &ast::Block,
        span: Span,
    ) -> Expr {
        let id = ClosureId(u32::try_from(self.closures.len()).unwrap_or(u32::MAX));
        self.closures.push(Closure {
            id,
            params: Vec::new(),
            param_kinds: Vec::new(),
            param_callable_sigs: Vec::new(),
            captures: Vec::new(),
            locals: Vec::new(),
            body: Block {
                lets: Vec::new(),
                result: Expr::Error { span },
                span,
            },
            result_kinds: vec![ValueKind::Error],
            result_callable_sigs: vec![None],
            return_annotation: None,
            span,
        });

        let mut closure_ctx = FunctionContext::for_closure(ctx.visible_local_sources());
        let mut param_ids = Vec::new();
        let mut param_kinds = Vec::new();
        let mut param_callable_sigs = Vec::new();
        for param in params {
            if closure_ctx.find_local(&param.name).is_some() {
                closure_ctx.diagnostics.push(
                    Diagnostic::error(param.span, DiagnosticCode::DuplicateLocal)
                        .with_arg(DiagnosticArg::text("local", param.name.clone())),
                );
            }
            let resolved =
                self.resolve_function_type_detail(param.type_annotation.as_ref(), ValueKind::I64);
            param_kinds.push(resolved.kind);
            param_callable_sigs.push(resolved.callable_signature.clone());
            param_ids.push(closure_ctx.add_local(
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
            ));
        }

        let (return_annotation, annotated_return_callable_sigs) = return_type.map_or_else(
            || (None, Vec::new()),
            |annotation| {
                let (kinds, callable_sigs) = self.resolve_result_annotation_detail(annotation);
                (Some(kinds), callable_sigs)
            },
        );
        let lowered_body = self.lower_block(&mut closure_ctx, body, return_annotation.as_deref());
        let body_kinds = self.expr_kinds(&lowered_body.result);
        let body_callable_sigs = self.expr_result_callable_signatures(&lowered_body.result);
        let result_kinds = return_annotation
            .clone()
            .unwrap_or_else(|| body_kinds.clone());
        let result_callable_sigs = if return_annotation.is_some() {
            annotated_return_callable_sigs
        } else {
            body_callable_sigs
        };
        if let Some(expected) = &return_annotation
            && !self.result_kinds_match(expected, &body_kinds)
        {
            closure_ctx.diagnostics.push(
                Diagnostic::error(
                    body.result.span(),
                    DiagnosticCode::FunctionReturnTypeMismatch,
                )
                .with_arg(DiagnosticArg::text("function", "<closure>"))
                .with_arg(DiagnosticArg::text(
                    "expected",
                    self.result_kinds_name(expected),
                ))
                .with_arg(DiagnosticArg::text(
                    "actual",
                    self.result_kinds_name(&body_kinds),
                )),
            );
        }

        let captures = closure_ctx.captures.clone();
        self.diagnostics.extend(closure_ctx.diagnostics);
        if let Some(slot) = self.closures.get_mut(id.0 as usize) {
            *slot = Closure {
                id,
                params: param_ids,
                param_kinds: param_kinds.clone(),
                param_callable_sigs: param_callable_sigs.clone(),
                captures: captures.clone(),
                locals: closure_ctx.locals,
                body: lowered_body,
                result_kinds: result_kinds.clone(),
                result_callable_sigs: result_callable_sigs.clone(),
                return_annotation,
                span,
            };
        }

        let callable_signature = CallableSignature {
            params: param_kinds,
            results: result_kinds,
        };
        Expr::Closure {
            closure: id,
            captures,
            result_kind: ValueKind::Callable,
            callable_signature,
            span,
        }
    }

    fn lower_apply(
        &mut self,
        ctx: &mut FunctionContext,
        args: &[ast::Expr],
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        let Some((target, args)) = args.split_first() else {
            ctx.diagnostics
                .push(Diagnostic::error(span, DiagnosticCode::ApplyArityMismatch));
            return Expr::Error { span };
        };
        let target = self.lower_expr(ctx, target);
        let lowered_args = args
            .iter()
            .map(|arg| self.lower_expr(ctx, arg))
            .collect::<Vec<_>>();
        match self.expr_kind(&target) {
            ValueKind::KnownCallable(KnownCallable::DirectFunction(function)) => {
                self.lower_applied_direct_call(ctx, target, function, lowered_args, span)
            }
            ValueKind::KnownCallable(KnownCallable::GenericFunction(generic))
            | ValueKind::GenericFunctionObject(generic) => {
                self.lower_applied_generic_callable_call(ctx, target, generic, lowered_args, span)
            }
            ValueKind::KnownCallable(KnownCallable::SlotAccessor { class, slot, .. }) => {
                self.lower_applied_accessor_call(ctx, target, class, slot, lowered_args, span)
            }
            ValueKind::Callable => self.lower_applied_dynamic_callable_call(
                ctx,
                target,
                lowered_args,
                expected_results,
                span,
            ),
            ValueKind::Error => Expr::Error { span },
            actual => {
                ctx.diagnostics.push(
                    Diagnostic::error(target.span(), DiagnosticCode::ApplyTargetTypeMismatch)
                        .with_arg(DiagnosticArg::text(
                            "expected",
                            "known callable, generic function, or <callable>",
                        ))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
                Expr::Error { span }
            }
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
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                    .with_arg(DiagnosticArg::text("callee", callee.to_string()))
                    .with_arg(DiagnosticArg::unsigned(
                        "expected_count",
                        u64::try_from(expected).unwrap_or(u64::MAX),
                    ))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Some(Expr::Error { span });
        }
        let args = args
            .iter()
            .cloned()
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
        Some(Expr::IntrinsicCall {
            intrinsic,
            args,
            result_kind: self.intrinsic_result_kind(intrinsic),
            span,
        })
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
        let mut valid = true;
        for (index, arg) in args.iter().enumerate() {
            let actual = self.expr_kind(arg);
            if !matches!(actual, ValueKind::F64 | ValueKind::Error) {
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
            | Intrinsic::GenericFunctionName => self
                .builtin_value_class(BuiltinValueClass::String)
                .map_or(ValueKind::Error, ValueKind::String),
            Intrinsic::ClassDirectSlots
            | Intrinsic::ClassEffectiveSlots
            | Intrinsic::GenericFunctionMethods
            | Intrinsic::MethodSpecializers => self
                .builtin_value_class(BuiltinValueClass::Vector)
                .map_or(ValueKind::Error, ValueKind::Vector),
            Intrinsic::SlotMutable => ValueKind::Bool,
            Intrinsic::VectorSize => ValueKind::I64,
            Intrinsic::ObjectClass
            | Intrinsic::ClassDirectSuperclass
            | Intrinsic::SlotSetter
            | Intrinsic::MethodGenericFunction
            | Intrinsic::VectorRef => ValueKind::Any,
        }
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
                    | ValueKind::String(_)
                    | ValueKind::Symbol(_)
                    | ValueKind::Vector(_)
                    | ValueKind::Object(_)
                    | ValueKind::ClassObject(_)
                    | ValueKind::GenericFunctionObject(_)
                    | ValueKind::MethodObject(_)
                    | ValueKind::SlotDescriptorObject { .. }
            ),
            (
                Intrinsic::ClassName
                | Intrinsic::ClassDirectSuperclass
                | Intrinsic::ClassDirectSlots
                | Intrinsic::ClassEffectiveSlots,
                0,
            ) => matches!(actual, ValueKind::ClassObject(_) | ValueKind::Any),
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
            (Intrinsic::GenericFunctionName | Intrinsic::GenericFunctionMethods, 0) => {
                matches!(actual, ValueKind::GenericFunctionObject(_) | ValueKind::Any)
            }
            (Intrinsic::MethodGenericFunction | Intrinsic::MethodSpecializers, 0) => {
                matches!(actual, ValueKind::MethodObject(_) | ValueKind::Any)
            }
            (Intrinsic::VectorSize | Intrinsic::VectorRef, 0) => {
                matches!(actual, ValueKind::Vector(_) | ValueKind::Any)
            }
            (Intrinsic::VectorRef, 1) => actual == ValueKind::I64,
            _ => false,
        }
    }

    fn lower_applied_direct_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        function: FunctionId,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let Some(symbol) = self.functions.get(function.0 as usize) else {
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
        for (index, (arg, expected)) in args.iter().zip(&symbol.param_kinds).enumerate() {
            let actual = self.expr_kind(arg);
            if !self.kinds_match(*expected, actual) {
                ctx.diagnostics.push(
                    Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                        .with_arg(DiagnosticArg::text("function", symbol.name.clone()))
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
            {
                let actual_signature = self.expr_callable_signature(arg);
                if !self.callable_signatures_match(Some(expected), actual_signature.as_ref()) {
                    ctx.diagnostics.push(
                        Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                            .with_arg(DiagnosticArg::text("function", symbol.name.clone()))
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
                            )),
                    );
                }
            }
        }
        Expr::CallableCall {
            callable: Box::new(target),
            args,
            arg_kinds: symbol.param_kinds.clone(),
            result_kinds: symbol.return_kinds.clone(),
            result_callable_sigs: symbol.return_callable_sigs.clone(),
            span,
        }
    }

    fn lower_applied_accessor_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        class: ClassId,
        slot: SlotId,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let Some(class_info) = self.classes.get(class.0 as usize) else {
            return Expr::Error { span };
        };
        let Some(slot_info) = class_info.effective_slots.get(slot.0 as usize) else {
            return Expr::Error { span };
        };
        let expected_arity = 1;
        if args.len() != expected_arity {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                    .with_arg(DiagnosticArg::text(
                        "callee",
                        slot_info.accessor.name.clone(),
                    ))
                    .with_arg(DiagnosticArg::unsigned(
                        "expected_count",
                        u64::try_from(expected_arity).unwrap_or(u64::MAX),
                    ))
                    .with_arg(DiagnosticArg::unsigned(
                        "actual_count",
                        u64::try_from(args.len()).unwrap_or(u64::MAX),
                    )),
            );
            return Expr::Error { span };
        }
        let expected = ValueKind::Object(class);
        let actual = self.expr_kind(&args[0]);
        if !self.kinds_match(expected, actual) {
            ctx.diagnostics.push(
                Diagnostic::error(args[0].span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                    .with_arg(DiagnosticArg::text(
                        "function",
                        slot_info.accessor.name.clone(),
                    ))
                    .with_arg(DiagnosticArg::unsigned("argument_index", 1))
                    .with_arg(DiagnosticArg::text("expected", self.kind_name(expected)))
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
            );
        }
        Expr::CallableCall {
            callable: Box::new(target),
            args,
            arg_kinds: vec![ValueKind::Object(class)],
            result_kinds: vec![slot_info.kind],
            result_callable_sigs: vec![None],
            span,
        }
    }

    fn lower_applied_generic_callable_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        generic: GenericFunctionId,
        args: Vec<Expr>,
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
        for (index, (arg, expected)) in args.iter().zip(&symbol.param_kinds).enumerate() {
            let Some(expected) = *expected else {
                continue;
            };
            let actual = self.expr_kind(arg);
            if !self.generic_argument_matches(expected, actual) {
                ctx.diagnostics.push(
                    Diagnostic::error(arg.span(), DiagnosticCode::GenericArgumentTypeMismatch)
                        .with_arg(DiagnosticArg::text("generic", symbol.name.clone()))
                        .with_arg(DiagnosticArg::unsigned(
                            "argument_index",
                            u64::try_from(index + 1).unwrap_or(u64::MAX),
                        ))
                        .with_arg(DiagnosticArg::text("expected", self.kind_name(expected)))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
            }
        }
        Expr::CallableCall {
            callable: Box::new(target),
            args,
            arg_kinds: (0..symbol.arity())
                .map(|index| self.generic_callable_arg_kind(generic, index))
                .collect(),
            result_kinds: symbol.return_kinds.clone(),
            result_callable_sigs: symbol.return_callable_sigs.clone(),
            span,
        }
    }

    fn lower_applied_dynamic_callable_call(
        &self,
        ctx: &mut FunctionContext,
        target: Expr,
        args: Vec<Expr>,
        expected_results: Option<&[ValueKind]>,
        span: Span,
    ) -> Expr {
        if let Some(signature) = self.expr_callable_signature(&target) {
            if signature.params.len() != args.len() {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::FunctionArityMismatch)
                        .with_arg(DiagnosticArg::text("callee", "apply target"))
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
            for (index, (arg, expected)) in args.iter().zip(&signature.params).enumerate() {
                let actual = self.expr_kind(arg);
                if !self.kinds_match(*expected, actual) {
                    ctx.diagnostics.push(
                        Diagnostic::error(arg.span(), DiagnosticCode::FunctionArgumentTypeMismatch)
                            .with_arg(DiagnosticArg::text("function", "apply target"))
                            .with_arg(DiagnosticArg::unsigned(
                                "argument_index",
                                u64::try_from(index + 1).unwrap_or(u64::MAX),
                            ))
                            .with_arg(DiagnosticArg::text("expected", self.kind_name(*expected)))
                            .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                    );
                }
            }
            let result_len = signature.results.len();
            return Expr::CallableCall {
                callable: Box::new(target),
                args,
                arg_kinds: signature.params,
                result_kinds: signature.results,
                result_callable_sigs: vec![None; result_len],
                span,
            };
        }
        let Some(result_kinds) = expected_results else {
            ctx.diagnostics.push(
                Diagnostic::error(target.span(), DiagnosticCode::ApplyTargetTypeMismatch)
                    .with_arg(DiagnosticArg::text(
                        "expected",
                        "<callable> with known result context",
                    ))
                    .with_arg(DiagnosticArg::text("actual", "<callable>")),
            );
            return Expr::Error { span };
        };
        let arg_kinds = args
            .iter()
            .map(|arg| self.expr_kind(arg))
            .collect::<Vec<_>>();
        Expr::CallableCall {
            callable: Box::new(target),
            args,
            arg_kinds,
            result_kinds: result_kinds.to_vec(),
            result_callable_sigs: vec![None; result_kinds.len()],
            span,
        }
    }

    fn generic_callable_arg_kind(&self, generic: GenericFunctionId, index: usize) -> ValueKind {
        let Some(generic) = self.generics.get(generic.0 as usize) else {
            return ValueKind::Error;
        };
        match generic.param_kinds.get(index).copied().flatten() {
            Some(ValueKind::Any) if generic.dispatch_params.contains(&index) => {
                ValueKind::Object(OBJECT_CLASS_ID)
            }
            Some(kind) => kind,
            None if generic.dispatch_params.contains(&index) => ValueKind::Object(OBJECT_CLASS_ID),
            None => ValueKind::Any,
        }
    }

    fn lower_signal(
        &mut self,
        ctx: &mut FunctionContext,
        condition: &ast::Expr,
        span: Span,
    ) -> Expr {
        let condition = self.lower_expr(ctx, condition);
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
        let Some(active) = ctx.active_restart() else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                DiagnosticCode::SignalRequiresRestart,
            ));
            return Expr::Signal {
                condition: Box::new(condition),
                condition_class,
                result_kinds: vec![ValueKind::Error],
                result_callable_sigs: vec![None],
                span,
            };
        };
        if condition_class != ClassId(u32::MAX)
            && !self.is_same_or_subclass(condition_class, active.specializer)
        {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SignalRestartMismatch)
                    .with_arg(DiagnosticArg::text(
                        "condition_class",
                        self.kind_name(ValueKind::Object(condition_class)),
                    ))
                    .with_arg(DiagnosticArg::text(
                        "restart_class",
                        self.kind_name(ValueKind::Object(active.specializer)),
                    )),
            );
            return Expr::Signal {
                condition: Box::new(condition),
                condition_class,
                result_kinds: vec![ValueKind::Error],
                result_callable_sigs: vec![None],
                span,
            };
        }
        Expr::Signal {
            condition: Box::new(condition),
            condition_class,
            result_kinds: active.result_kinds,
            result_callable_sigs: active.result_callable_sigs,
            span,
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
                ctx.diagnostics.push(
                    Diagnostic::error(restart.span, DiagnosticCode::UnresolvedClass)
                        .with_arg(DiagnosticArg::text("class", restart.class_name.clone())),
                );
                ClassId(u32::MAX)
            });

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
        let restart_body = self.lower_expr_expected_results(ctx, &restart.body, expected_results);
        let restart_kinds = self.expr_kinds(&restart_body);
        let restart_callable_sigs = self.expr_result_callable_signatures(&restart_body);
        ctx.hide_locals_from(scope_start);

        ctx.push_restart(ActiveRestart {
            specializer,
            result_kinds: restart_kinds.clone(),
            result_callable_sigs: restart_callable_sigs.clone(),
        });
        let body = self.lower_expr_expected_results(ctx, body, expected_results);
        ctx.pop_restart();

        let body_kinds = self.expr_kinds(&body);
        if !self.result_kinds_match(&body_kinds, &restart_kinds)
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

        Expr::Handle {
            body: Box::new(body),
            restart: Restart {
                specializer,
                condition_local,
                body: Box::new(restart_body),
                result_kinds: restart_kinds,
                result_callable_sigs: restart_callable_sigs,
                span: restart.span,
            },
            span,
        }
    }

    /// Lowers the operands of a numeric operator.
    ///
    /// The left operand keeps the legacy integer result hint used by
    /// open-result forms such as `apply` and `signal`; the right operand is
    /// hinted with the left operand's numeric kind, so `x * apply(f)` with a
    /// float `x` expects a float result.
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
        (lhs, rhs)
    }

    /// Returns the shared numeric kind of two operands.
    ///
    /// Reports `code` and returns `None` when they are not both integers or
    /// both floats; callers then produce an error expression so the mismatch
    /// is reported once rather than cascading into result-type diagnostics.
    fn numeric_operand_kind(
        &self,
        ctx: &mut FunctionContext,
        span: Span,
        lhs: &Expr,
        rhs: &Expr,
        code: DiagnosticCode,
    ) -> Option<NumericKind> {
        let lhs_kind = self.expr_kind(lhs);
        let rhs_kind = self.expr_kind(rhs);
        match (NumericKind::of(lhs_kind), NumericKind::of(rhs_kind)) {
            (Some(lhs), Some(rhs)) if lhs == rhs => return Some(lhs),
            (Some(kind), None) if rhs_kind == ValueKind::Error => return Some(kind),
            (None, Some(kind)) if lhs_kind == ValueKind::Error => return Some(kind),
            (None, None) if lhs_kind == ValueKind::Error && rhs_kind == ValueKind::Error => {
                return Some(NumericKind::Integer);
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

    fn lower_compare(
        &self,
        ctx: &mut FunctionContext,
        op: ast::CompareOp,
        lhs: Expr,
        rhs: Expr,
        span: Span,
    ) -> Expr {
        let lhs_kind = self.expr_kind(&lhs);
        let rhs_kind = self.expr_kind(&rhs);
        let numeric = |kind: ValueKind| NumericKind::of(kind).is_some() || kind == ValueKind::Error;
        if op != ast::CompareOp::Eq || (numeric(lhs_kind) && numeric(rhs_kind)) {
            let Some(kind) = self.numeric_operand_kind(
                ctx,
                span,
                &lhs,
                &rhs,
                DiagnosticCode::ComparisonOperandTypeMismatch,
            ) else {
                return Expr::Error { span };
            };
            return Expr::Compare {
                op: op.into(),
                kind,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }

        if lhs_kind == rhs_kind && matches!(lhs_kind, ValueKind::String(_) | ValueKind::Symbol(_)) {
            return Expr::ValueEqual {
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                kind: lhs_kind,
                span,
            };
        }

        ctx.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::ComparisonOperandTypeMismatch)
                .with_arg(DiagnosticArg::text("lhs", self.kind_name(lhs_kind)))
                .with_arg(DiagnosticArg::text("rhs", self.kind_name(rhs_kind))),
        );
        Expr::Error { span }
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
        class_name: &str,
        span: Span,
    ) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return Expr::Error { span };
        };
        let class_id = class.id;
        let value = self.lower_expr(ctx, value);
        match self.expr_kind(&value) {
            ValueKind::Any
            | ValueKind::Object(_)
            | ValueKind::String(_)
            | ValueKind::Symbol(_)
            | ValueKind::Vector(_)
            | ValueKind::I64
            | ValueKind::F64
            | ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. }
            | ValueKind::Error => {}
            actual => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::InstanceOfTypeMismatch)
                        .with_arg(DiagnosticArg::text("expected", "<object>"))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
            }
        }
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
            value: Box::new(value),
            class: class_id,
            span,
        }
    }

    fn lower_cast(
        &mut self,
        ctx: &mut FunctionContext,
        value: &ast::Expr,
        class_name: &str,
        span: Span,
    ) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", class_name)),
            );
            return Expr::Error { span };
        };
        let class_id = class.id;
        let value = self.lower_expr(ctx, value);
        if let Some(from) = NumericKind::of(self.expr_kind(&value))
            && let Some(to) = self
                .kind_for_builtin_class(class_id)
                .and_then(NumericKind::of)
            && from != to
        {
            return Expr::Convert {
                to,
                value: Box::new(value),
                span,
            };
        }
        match self.expr_kind(&value) {
            ValueKind::Any
            | ValueKind::Object(_)
            | ValueKind::String(_)
            | ValueKind::Symbol(_)
            | ValueKind::Vector(_)
            | ValueKind::I64
            | ValueKind::F64
            | ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. }
            | ValueKind::Error => {}
            actual => {
                ctx.diagnostics.push(
                    Diagnostic::error(span, DiagnosticCode::CastTypeMismatch)
                        .with_arg(DiagnosticArg::text("expected", "<object>"))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(actual))),
                );
            }
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
            value: Box::new(value),
            class: class_id,
            span,
        }
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
            let value = self.lower_expr(ctx, &field.value);
            let value_kind = self.expr_kind(&value);
            if !self.kinds_match(slot.kind, value_kind) {
                ctx.diagnostics.push(
                    Diagnostic::error(field.span, DiagnosticCode::SlotInitializerTypeMismatch)
                        .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                        .with_arg(DiagnosticArg::text("expected", self.kind_name(slot.kind)))
                        .with_arg(DiagnosticArg::text("actual", self.kind_name(value_kind))),
                );
            }
            init_args.push(InitArg {
                keyword: field.name.clone(),
                slot: slot.id,
                value,
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

        Expr::MakeObject {
            class: Box::new(class_expr),
            class_id,
            init_args,
            fields: resolved,
            span,
        }
    }

    fn lower_slot_read(
        &mut self,
        ctx: &mut FunctionContext,
        receiver: &ast::Expr,
        slot_name: &str,
        span: Span,
    ) -> Expr {
        let receiver = self.lower_expr(ctx, receiver);
        self.lower_accessor_call(ctx, receiver, slot_name, span)
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
        let value = self.lower_expr(ctx, value);
        let receiver_kind = self.expr_kind(&receiver);
        let ValueKind::Object(class_id) = receiver_kind else {
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
        let value_kind = self.expr_kind(&value);
        if !self.kinds_match(slot.kind, value_kind) {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::SlotAssignmentTypeMismatch)
                    .with_arg(DiagnosticArg::text("slot", slot.name.clone()))
                    .with_arg(DiagnosticArg::text("expected", self.kind_name(slot.kind)))
                    .with_arg(DiagnosticArg::text("actual", self.kind_name(value_kind))),
            );
        }
        let setter_name = slot_setter_name(&slot.name);
        let Some(setter) = self.resolve_generic(&setter_name) else {
            ctx.diagnostics.push(
                Diagnostic::error(span, DiagnosticCode::UnresolvedGeneric)
                    .with_arg(DiagnosticArg::text("generic", setter_name)),
            );
            return Expr::Error { span };
        };
        Expr::GenericCall {
            generic: setter.id,
            args: vec![value, receiver],
            span,
        }
    }

    fn lower_accessor_call(
        &self,
        ctx: &mut FunctionContext,
        receiver: Expr,
        accessor_name: &str,
        span: Span,
    ) -> Expr {
        let Some(class_id) = self.value_kind_class(self.expr_kind(&receiver)) else {
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

    fn true_branch_refinement(&self, condition: &Expr) -> Option<Refinement> {
        let Expr::InstanceOf { value, class, .. } = condition else {
            return None;
        };
        let Expr::Local {
            local, result_kind, ..
        } = value.as_ref()
        else {
            return None;
        };
        let kind = if let ValueKind::Object(actual) = result_kind
            && self.is_same_or_subclass(*actual, *class)
        {
            ValueKind::Object(*actual)
        } else {
            ValueKind::Object(*class)
        };
        Some(Refinement {
            local: *local,
            kind,
        })
    }

    fn expr_kind(&self, expr: &Expr) -> ValueKind {
        match expr {
            Expr::ConstI64 { .. } => ValueKind::I64,
            Expr::Binary { kind, .. } | Expr::Negate { kind, .. } => kind.value_kind(),
            Expr::Convert { to, .. } => to.value_kind(),
            Expr::FloatCall { builtin, .. } => builtin.result_kind(),
            Expr::ConstF64 { .. } => ValueKind::F64,
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
            Expr::KnownCallable { callable, .. } => ValueKind::KnownCallable(*callable),
            Expr::Closure { result_kind, .. } => *result_kind,
            Expr::GenericFunctionObject { generic, .. } => {
                ValueKind::GenericFunctionObject(*generic)
            }
            Expr::IntrinsicCall { result_kind, .. } => *result_kind,
            Expr::HostCall { result_kind, .. } => *result_kind,
            Expr::AccessorCall { result_kind, .. } => *result_kind,
            Expr::SlotWrite { result_kind, .. } => *result_kind,
            Expr::ConstBool { .. }
            | Expr::Compare { .. }
            | Expr::ValueEqual { .. }
            | Expr::InstanceOf { .. } => ValueKind::Bool,
            Expr::Local { result_kind, .. } => *result_kind,
            Expr::Capture { result_kind, .. } => *result_kind,
            Expr::ConstantRef { result_kind, .. } => *result_kind,
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
            Expr::If {
                then_branch,
                else_branch,
                ..
            } => {
                let then_kind = self.expr_kind(then_branch);
                let else_kind = self.expr_kind(else_branch);
                if then_kind == else_kind {
                    then_kind
                } else {
                    ValueKind::Error
                }
            }
            Expr::Handle { restart, .. } => scalar_result_kind(&restart.result_kinds),
            Expr::Error { .. } => ValueKind::Error,
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
            } => {
                let then_kinds = self.expr_kinds(then_branch);
                if then_kinds == self.expr_kinds(else_branch) {
                    then_kinds
                } else {
                    vec![ValueKind::Error]
                }
            }
            Expr::Signal { result_kinds, .. } => result_kinds.clone(),
            Expr::Handle { restart, .. } => restart.result_kinds.clone(),
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
            } => callable_signature.clone(),
            Expr::Closure {
                callable_signature, ..
            } => Some(callable_signature.clone()),
            Expr::KnownCallable { callable, .. } => self.known_callable_signature(*callable),
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
            Expr::Handle { restart, .. } => restart.result_callable_sigs.clone(),
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
        match self
            .classes
            .get(class.0 as usize)
            .map(|class| class.origin)?
        {
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
                kind: BuiltinValueClass::Float,
            } => Some(ValueKind::F64),
            ClassOrigin::BuiltinValue {
                kind: BuiltinValueClass::Vector,
            } => Some(ValueKind::Vector(class)),
            ClassOrigin::BuiltinValue {
                kind:
                    BuiltinValueClass::Class
                    | BuiltinValueClass::GenericFunction
                    | BuiltinValueClass::Method
                    | BuiltinValueClass::SlotDescriptor,
            } => Some(ValueKind::Object(class)),
            ClassOrigin::ObjectRoot | ClassOrigin::Native | ClassOrigin::HostBacked { .. } => None,
        }
    }

    fn value_kind_class(&self, kind: ValueKind) -> Option<ClassId> {
        match kind {
            ValueKind::String(class) | ValueKind::Symbol(class) | ValueKind::Vector(class) => {
                Some(class)
            }
            ValueKind::I64 => self.builtin_value_class(BuiltinValueClass::Integer),
            ValueKind::F64 => self.builtin_value_class(BuiltinValueClass::Float),
            ValueKind::Object(class) => Some(class),
            ValueKind::ClassObject(_) => self.builtin_value_class(BuiltinValueClass::Class),
            ValueKind::GenericFunctionObject(_) => {
                self.builtin_value_class(BuiltinValueClass::GenericFunction)
            }
            ValueKind::MethodObject(_) => self.builtin_value_class(BuiltinValueClass::Method),
            ValueKind::SlotDescriptorObject { .. } => {
                self.builtin_value_class(BuiltinValueClass::SlotDescriptor)
            }
            ValueKind::Any
            | ValueKind::Bool
            | ValueKind::Callable
            | ValueKind::KnownCallable(_)
            | ValueKind::Error => None,
        }
    }

    fn has_accessor(&self, name: &str) -> bool {
        self.classes.iter().any(|class| {
            class
                .direct_slots
                .iter()
                .any(|slot| slot.accessor.name == name)
        })
    }

    fn resolve_accessor_callable(&self, name: &str) -> AccessorResolution {
        let mut found = None;
        for class in &self.classes {
            for slot in &class.direct_slots {
                if slot.accessor.name != name {
                    continue;
                }
                let effective_slot = class
                    .effective_slots
                    .iter()
                    .find(|effective| {
                        effective.source_class == class.id && effective.source_slot == slot.id
                    })
                    .map_or(slot.id, |effective| effective.id);
                let callable = KnownCallable::SlotAccessor {
                    accessor: slot.accessor.definition,
                    class: class.id,
                    slot: effective_slot,
                };
                if found.replace(callable).is_some() {
                    return AccessorResolution::Ambiguous;
                }
            }
        }
        found.map_or(AccessorResolution::None, AccessorResolution::One)
    }

    fn resolve_slot_kind(&mut self, type_name: Option<&str>, span: Span) -> ValueKind {
        let Some(type_name) = type_name else {
            return ValueKind::Any;
        };
        self.resolve_type_name(type_name, false, span)
    }

    fn resolve_function_type_detail(
        &mut self,
        annotation: Option<&ast::TypeAnnotation>,
        default: ValueKind,
    ) -> ResolvedType {
        annotation.map_or(
            ResolvedType {
                kind: default,
                callable_signature: None,
            },
            |annotation| self.resolve_type_annotation_detail(annotation),
        )
    }

    fn resolve_type_annotation(&mut self, annotation: &ast::TypeAnnotation) -> ValueKind {
        self.resolve_type_annotation_detail(annotation).kind
    }

    fn resolve_type_annotation_detail(&mut self, annotation: &ast::TypeAnnotation) -> ResolvedType {
        if let Some(callable) = &annotation.callable {
            let signature = self.resolve_callable_type_annotation(callable);
            return ResolvedType {
                kind: ValueKind::Callable,
                callable_signature: Some(signature),
            };
        }
        ResolvedType {
            kind: self.resolve_type_name(
                &annotation.name,
                annotation.class_object,
                annotation.span,
            ),
            callable_signature: None,
        }
    }

    fn resolve_result_annotation(&mut self, annotation: &ast::ResultAnnotation) -> Vec<ValueKind> {
        self.resolve_result_annotation_detail(annotation).0
    }

    fn resolve_result_annotation_detail(
        &mut self,
        annotation: &ast::ResultAnnotation,
    ) -> (Vec<ValueKind>, Vec<Option<CallableSignature>>) {
        annotation
            .values
            .iter()
            .map(|value| self.resolve_type_annotation_detail(value))
            .map(|resolved| (resolved.kind, resolved.callable_signature))
            .unzip()
    }

    fn resolve_callable_type_annotation(
        &mut self,
        annotation: &ast::CallableTypeAnnotation,
    ) -> CallableSignature {
        let params = annotation
            .params
            .iter()
            .map(|param| self.resolve_type_annotation(param))
            .collect();
        let results = self.resolve_result_annotation(&annotation.results);
        CallableSignature { params, results }
    }

    fn resolve_type_annotation_in_context(
        &self,
        ctx: &mut FunctionContext,
        annotation: &ast::TypeAnnotation,
    ) -> ValueKind {
        self.resolve_type_annotation_detail_in_context(ctx, annotation)
            .kind
    }

    fn resolve_type_annotation_detail_in_context(
        &self,
        ctx: &mut FunctionContext,
        annotation: &ast::TypeAnnotation,
    ) -> ResolvedType {
        if let Some(callable) = &annotation.callable {
            let signature = self.resolve_callable_type_annotation_in_context(ctx, callable);
            return ResolvedType {
                kind: ValueKind::Callable,
                callable_signature: Some(signature),
            };
        }
        ResolvedType {
            kind: self.resolve_type_name_with_sink(
                &annotation.name,
                annotation.class_object,
                annotation.span,
                &mut ctx.diagnostics,
            ),
            callable_signature: None,
        }
    }

    fn resolve_callable_type_annotation_in_context(
        &self,
        ctx: &mut FunctionContext,
        annotation: &ast::CallableTypeAnnotation,
    ) -> CallableSignature {
        let params = annotation
            .params
            .iter()
            .map(|param| self.resolve_type_annotation_in_context(ctx, param))
            .collect();
        let results = annotation
            .results
            .values
            .iter()
            .map(|result| self.resolve_type_annotation_in_context(ctx, result))
            .collect();
        CallableSignature { params, results }
    }

    fn resolve_method_specializer(
        &mut self,
        param: &ast::MethodParam,
    ) -> (MethodSpecializer, ValueKind) {
        if param.class_name == "<object>" {
            return (Some(OBJECT_CLASS_ID), ValueKind::Any);
        }
        let Some(class) = self.resolve_class(&param.class_name) else {
            self.diagnostics.push(
                Diagnostic::error(param.span, DiagnosticCode::UnresolvedClass)
                    .with_arg(DiagnosticArg::text("class", param.class_name.clone())),
            );
            return (Some(ClassId(u32::MAX)), ValueKind::Error);
        };
        let kind = self
            .kind_for_builtin_class(class.id)
            .unwrap_or(ValueKind::Object(class.id));
        (Some(class.id), kind)
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
        if class_object {
            return self
                .resolve_class(type_name)
                .map(|class| ValueKind::ClassObject(class.id))
                .unwrap_or_else(|| {
                    diagnostics.push(
                        Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                            .with_arg(DiagnosticArg::text("class", type_name)),
                    );
                    ValueKind::Error
                });
        }
        match type_name {
            "<object>" => ValueKind::Any,
            "<boolean>" => ValueKind::Bool,
            "<string>" => self
                .builtin_value_class(BuiltinValueClass::String)
                .map_or(ValueKind::Error, ValueKind::String),
            "<symbol>" => self
                .builtin_value_class(BuiltinValueClass::Symbol)
                .map_or(ValueKind::Error, ValueKind::Symbol),
            "<integer>" => ValueKind::I64,
            "<float>" => ValueKind::F64,
            "<callable>" => ValueKind::Callable,
            "<vector>" => self
                .builtin_value_class(BuiltinValueClass::Vector)
                .map_or(ValueKind::Error, ValueKind::Vector),
            "<class>" => self
                .builtin_value_class(BuiltinValueClass::Class)
                .map_or(ValueKind::Error, ValueKind::Object),
            "<generic-function>" => self
                .builtin_value_class(BuiltinValueClass::GenericFunction)
                .map_or(ValueKind::Error, ValueKind::Object),
            "<method>" => self
                .builtin_value_class(BuiltinValueClass::Method)
                .map_or(ValueKind::Error, ValueKind::Object),
            "<slot-descriptor>" => self
                .builtin_value_class(BuiltinValueClass::SlotDescriptor)
                .map_or(ValueKind::Error, ValueKind::Object),
            name => self
                .resolve_class(name)
                .map(|class| ValueKind::Object(class.id))
                .unwrap_or_else(|| {
                    diagnostics.push(
                        Diagnostic::error(span, DiagnosticCode::UnresolvedClass)
                            .with_arg(DiagnosticArg::text("class", name)),
                    );
                    ValueKind::Error
                }),
        }
    }

    fn kinds_match(&self, expected: ValueKind, actual: ValueKind) -> bool {
        (matches!(expected, ValueKind::Any)
            && !matches!(
                actual,
                ValueKind::Callable | ValueKind::KnownCallable(_)
            ))
            || (matches!(expected, ValueKind::Callable)
                && matches!(actual, ValueKind::Callable | ValueKind::KnownCallable(_)))
            || matches!((expected, actual), (ValueKind::Object(expected), ValueKind::Object(actual)) if self.is_same_or_subclass(actual, expected))
            || self
                .value_kind_class(actual)
                .is_some_and(|actual| matches!(expected, ValueKind::Object(expected) if self.is_same_or_subclass(actual, expected)))
            || expected == actual
            || matches!(expected, ValueKind::Error)
            || matches!(actual, ValueKind::Error)
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
            ValueKind::I64 | ValueKind::F64 => self
                .kind_for_builtin_class(specializer)
                .is_some_and(|kind| kind == expected),
            ValueKind::Bool
            | ValueKind::Callable
            | ValueKind::ClassObject(_)
            | ValueKind::GenericFunctionObject(_)
            | ValueKind::MethodObject(_)
            | ValueKind::SlotDescriptorObject { .. }
            | ValueKind::KnownCallable(_) => false,
        }
    }

    fn generic_argument_matches(&self, expected: ValueKind, actual: ValueKind) -> bool {
        match expected {
            ValueKind::Any => matches!(
                actual,
                ValueKind::Object(_)
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
            ValueKind::I64
            | ValueKind::F64
            | ValueKind::Bool
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
            ValueKind::F64 => "<float>".into(),
            ValueKind::Bool => "<boolean>".into(),
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
                self.functions
                    .get(function.0 as usize)
                    .map(|function| CallableSignature {
                        params: function.param_kinds.clone(),
                        results: function.return_kinds.clone(),
                    })
            }
            KnownCallable::GenericFunction(generic) => self
                .generic_symbols
                .get(generic.0 as usize)
                .map(|generic| CallableSignature {
                    params: (0..generic.arity())
                        .map(|index| self.generic_callable_arg_kind(generic.id, index))
                        .collect(),
                    results: generic.return_kinds.clone(),
                }),
            KnownCallable::SlotAccessor { class, slot, .. } => {
                let class = self.classes.get(class.0 as usize)?;
                let slot = class.effective_slots.get(slot.0 as usize)?;
                Some(CallableSignature {
                    params: vec![ValueKind::Object(class.id)],
                    results: vec![slot.kind],
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
        expected.params.len() == actual.params.len()
            && expected.results.len() == actual.results.len()
            && expected
                .params
                .iter()
                .copied()
                .zip(actual.params.iter().copied())
                .all(|(expected, actual)| self.kinds_match(expected, actual))
            && self.result_kinds_match(&expected.results, &actual.results)
    }

    fn callable_signature_name(&self, signature: &CallableSignature) -> String {
        let params = signature
            .params
            .iter()
            .map(|kind| self.kind_name(*kind))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "callable({params}) => {}",
            self.result_kinds_name(&signature.results)
        )
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
            KnownCallable::SlotAccessor { class, slot, .. } => self
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or_else(
                    || format!("accessor#{}.{}", class.0, slot.0),
                    |slot| format!("accessor `{}`", slot.accessor.name),
                ),
        }
    }

    fn resolve_function(&self, name: &str) -> Option<&FunctionSymbol> {
        self.functions.iter().find(|symbol| symbol.name == name)
    }

    fn resolve_host_function(&self, name: &str) -> Option<&ResolvedHostFunction> {
        self.host_functions
            .iter()
            .find(|function| function.name == name)
    }

    fn resolve_constant(&self, name: &str) -> Option<&ConstantSymbol> {
        self.constant_symbols
            .iter()
            .find(|symbol| symbol.name == name)
    }

    fn resolve_generic(&self, name: &str) -> Option<&GenericSymbol> {
        self.generic_symbols
            .iter()
            .find(|symbol| symbol.name == name)
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
        assert_eq!(module.functions[1].body.lets[0].local, LocalId(0));
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
    fn resolves_known_callable_values_and_apply() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function inc(x)\n  x + 1\nend;\n\
             define function main()\n  let f = inc;\n  let g = area;\n  apply(f, 2) + apply(g, make(<circle>, radius: 3))\nend;",
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
    fn resolves_slot_accessor_values_without_applying_them() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let getter = radius;\n  1\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle = class(&module, "<circle>");
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::KnownCallable(KnownCallable::SlotAccessor {
                accessor: circle.direct_slots[0].accessor.definition,
                class: circle.id,
                slot: SlotId(0),
            })
        );
    }

    #[test]
    fn resolves_slot_accessor_values_as_callable_apply_targets() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let getter = radius;\n  apply(getter, make(<circle>, radius: 3))\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let circle = class(&module, "<circle>");
        let expected = KnownCallable::SlotAccessor {
            accessor: circle.direct_slots[0].accessor.definition,
            class: circle.id,
            slot: SlotId(0),
        };
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::KnownCallable(expected)
        );
        let Expr::CallableCall {
            callable,
            result_kinds,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected accessor apply to remain a callable call");
        };
        assert_eq!(result_kinds, &[ValueKind::I64]);
        assert!(matches!(
            callable.as_ref(),
            Expr::Local {
                local: LocalId(0),
                result_kind: ValueKind::KnownCallable(callable),
                ..
            } if *callable == expected
        ));
    }

    #[test]
    fn resolves_runtime_callable_parameters() {
        let module = lower_source(
            "define function inc(x)\n  x + 1\nend;\n\
             define function callit(f :: <callable>) => <integer>\n  apply(f, 41)\nend;\n\
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
    fn reports_invalid_apply_targets() {
        let non_callable = lower_source("define function main()\n  apply(1, 2)\nend;");
        assert!(has_diagnostic(
            &non_callable,
            DiagnosticCode::ApplyTargetTypeMismatch,
            &[("actual", "<integer>")]
        ));
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
             define function main()\n  handle if instance?(make(<invalid-radius>), <problem>) then as(<problem>, make(<invalid-radius>)).fallback else 0 end with restart e :: <problem> e.fallback end\nend;\n\
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
    fn reports_typed_generic_contract_mismatches() {
        let bad_call = lower_source(
            "define generic area(x :: <object>) => <integer>;\n\
             define function main()\n  area(1)\nend;",
        );
        assert!(has_diagnostic(
            &bad_call,
            DiagnosticCode::GenericArgumentTypeMismatch,
            &[
                ("generic", "area"),
                ("argument_index", "1"),
                ("expected", "<object>"),
                ("actual", "<integer>")
            ]
        ));

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
        let Expr::ClassObject { class, .. } = &module.functions[0].body.lets[0].value else {
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
        let Expr::ClassOf { class, .. } = &module.functions[0].body.lets[1].value else {
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
             define function main()\n  if keep?(#t) then build(<circle>, 4).radius else 0 end\nend;",
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
    fn resolves_instance_tests_casts_and_true_branch_refinement() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function radius-or-zero(x :: <object>) => <integer>\n  if instance?(x, <circle>) then x.radius else 0 end\nend;\n\
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
    fn reports_impossible_instance_tests_and_casts() {
        let instance = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define function main()\n  if instance?(make(<square>, side: 2), <circle>) then 1 else 0 end\nend;",
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
        assert!(!has_diagnostic(
            &integer,
            DiagnosticCode::InstanceOfTypeMismatch,
            &[]
        ));
    }

    #[test]
    fn resolves_typed_slots_and_checks_initializers() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <box> (<object>)\n  slot item :: <circle>;\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<box>, item: make(<circle>, radius: 7), valid?: #t).valid? then make(<box>, item: make(<circle>, radius: 7), valid?: #t).item.radius else 0 end\nend;",
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
            "define function nonnegative(x)\n  x >= 0\nend;\n\
             define function main(x)\n  if nonnegative(x) then x else 0 - x end\nend;",
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
        let invalid_condition =
            lower_source("define function main()\n  if 1 then 2 else 3 end\nend;");
        assert!(has_diagnostic(
            &invalid_condition,
            DiagnosticCode::IfConditionTypeMismatch,
            &[("expected", "<boolean>"), ("actual", "<integer>")]
        ));

        let mismatched_branches =
            lower_source("define function main(x)\n  if x = 0 then #t else 3 end\nend;");
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
        let uncaught = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  signal(make(<bad>, fallback: 0))\nend;",
        );
        assert!(has_diagnostic(
            &uncaught,
            DiagnosticCode::SignalRequiresRestart,
            &[]
        ));

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
}
