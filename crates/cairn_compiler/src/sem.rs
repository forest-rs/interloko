//! Semantic IR for the first generic-dispatch Cairn slice.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::ast;
use crate::diagnostic::Diagnostic;
use crate::identity::{DefinitionId, DefinitionKind, SourceFileId};
use crate::syntax::Span;

/// Durable class identifier within a semantic module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ClassId(
    /// Zero-based class index.
    pub u32,
);

/// Durable slot identifier within a semantic class.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SlotId(
    /// Zero-based slot index within the class.
    pub u32,
);

/// Durable generic function identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct GenericFunctionId(
    /// Zero-based generic index.
    pub u32,
);

/// Durable method identifier.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct MethodId(
    /// Zero-based method index.
    pub u32,
);

/// Durable direct function identifier within a semantic module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FunctionId(
    /// Zero-based function index.
    pub u32,
);

/// Durable local identifier within a semantic function or method.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalId(
    /// Zero-based local index.
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
    /// Top Cairn value category corresponding to source `<object>`.
    Any,
    /// Signed integer value.
    I64,
    /// Boolean value.
    Bool,
    /// Runtime object with statically known nominal class.
    Object(ClassId),
    /// Class object naming a statically known nominal class.
    ClassObject(ClassId),
    /// Compile-time-known callable object.
    KnownCallable(KnownCallable),
    /// Invalid or unknown value.
    Error,
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
    /// Cairn-visible class name, such as `<rust-circle>`.
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
    /// Cairn-visible slot/accessor name.
    pub name: String,
    /// Declared Cairn value category.
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
    /// Cairn-visible function name.
    pub name: String,
    /// Declared parameter value categories.
    pub param_kinds: Vec<ValueKind>,
    /// Declared result value category.
    pub result_kind: ValueKind,
}

impl HostFunction {
    /// Creates a host-backed function declaration.
    #[must_use]
    pub fn new(name: &str, param_kinds: Vec<ValueKind>, result_kind: ValueKind) -> Self {
        Self {
            name: name.into(),
            param_kinds,
            result_kind,
        }
    }
}

/// Origin of a semantic class metaobject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClassOrigin {
    /// Class defined in Cairn source.
    Native,
    /// Class declared by the host-backed metaobject registry.
    HostBacked {
        /// Registry-local host-backed class id.
        host_class: HostClassId,
    },
}

/// Origin of a semantic slot metaobject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotOrigin {
    /// Slot defined in Cairn source.
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
    /// Semantic diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

impl Module {
    /// Returns true when `actual` is the same class as `expected` or inherits
    /// from it through Cairn's single nominal superclass chain.
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
    /// Result value category.
    pub result_kind: ValueKind,
    /// Explicit declared result category, when present.
    pub return_annotation: Option<ValueKind>,
    /// Source span.
    pub span: Span,
}

/// Method definition specialized on one nominal class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Method {
    /// Method identifier.
    pub id: MethodId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Implemented generic function.
    pub generic: GenericFunctionId,
    /// Specialized class for the first argument.
    pub specializer: ClassId,
    /// Parameter local.
    pub param: LocalId,
    /// Local table.
    pub locals: Vec<Local>,
    /// Method body.
    pub body: Block,
    /// Result value category.
    pub result_kind: ValueKind,
    /// Explicit declared result category, when present.
    pub return_annotation: Option<ValueKind>,
    /// Source span.
    pub span: Span,
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
    /// Local table containing parameters followed by `let` bindings.
    pub locals: Vec<Local>,
    /// Function body.
    pub body: Block,
    /// Result value category.
    pub result_kind: ValueKind,
    /// Explicit declared result category, when present.
    pub return_annotation: Option<ValueKind>,
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
}

impl From<ast::BinaryOp> for BinaryOp {
    fn from(value: ast::BinaryOp) -> Self {
        match value {
            ast::BinaryOp::Add => Self::Add,
            ast::BinaryOp::Sub => Self::Sub,
            ast::BinaryOp::Mul => Self::Mul,
        }
    }
}

/// Semantic integer comparison operator.
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
    /// Boolean constant.
    ConstBool {
        /// Constant value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// Resolved local reference.
    Local {
        /// Local identifier.
        local: LocalId,
        /// Value category at this use site, including local refinements.
        result_kind: ValueKind,
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
    /// Recoverable condition signal.
    Signal {
        /// Condition object expression.
        condition: Box<Expr>,
        /// Statically known condition class.
        condition_class: ClassId,
        /// Result category supplied by the active restart.
        result_kind: ValueKind,
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
        /// Field initializers.
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
    /// Binary expression.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Integer comparison.
    Compare {
        /// Operator.
        op: CompareOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
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
            | Self::ConstBool { span, .. }
            | Self::Local { span, .. }
            | Self::ClassObject { span, .. }
            | Self::KnownCallable { span, .. }
            | Self::DirectCall { span, .. }
            | Self::GenericCall { span, .. }
            | Self::Signal { span, .. }
            | Self::ClassOf { span, .. }
            | Self::InstanceOf { span, .. }
            | Self::Cast { span, .. }
            | Self::MakeObject { span, .. }
            | Self::AccessorCall { span, .. }
            | Self::Binary { span, .. }
            | Self::Compare { span, .. }
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
    /// Handler result category.
    pub result_kind: ValueKind,
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
    return_kind: ValueKind,
    return_annotation: Option<ValueKind>,
    span: Span,
}

impl FunctionSymbol {
    fn arity(&self) -> usize {
        self.param_kinds.len()
    }
}

#[derive(Clone, Debug)]
struct GenericSymbol {
    name: String,
    id: GenericFunctionId,
    param_kinds: Vec<GenericParamKind>,
    return_kind: ValueKind,
    return_annotation: Option<ValueKind>,
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
    active_restarts: Vec<ActiveRestart>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ActiveRestart {
    specializer: ClassId,
    result_kind: ValueKind,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AccessorSyntax {
    Slot,
    Call,
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
            active_restarts: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn add_local(
        &mut self,
        definition: DefinitionId,
        name: String,
        kind: ValueKind,
        span: Span,
    ) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        self.locals.push(Local {
            id,
            definition,
            name,
            kind,
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

    fn local_kind(&self, id: LocalId) -> ValueKind {
        self.locals
            .get(id.0 as usize)
            .map_or(ValueKind::Error, |local| local.kind)
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
        self.active_restarts.last().copied()
    }
}

struct Lowering {
    source_file: SourceFileId,
    host_registry: HostMetaRegistry,
    classes: Vec<Class>,
    generics: Vec<GenericFunction>,
    methods: Vec<Method>,
    functions: Vec<FunctionSymbol>,
    generic_symbols: Vec<GenericSymbol>,
    diagnostics: Vec<Diagnostic>,
}

impl Lowering {
    fn new(module: &ast::Module, registry: &HostMetaRegistry) -> Self {
        let mut lowering = Self {
            source_file: module.source_file,
            host_registry: registry.clone(),
            classes: Vec::new(),
            generics: Vec::new(),
            methods: Vec::new(),
            functions: Vec::new(),
            generic_symbols: Vec::new(),
            diagnostics: module.diagnostics.clone(),
        };
        lowering.collect_classes(module);
        lowering.collect_generics(module);
        lowering.collect_functions(module);
        lowering.collect_slot_init_values(module);
        lowering.finalize_classes();
        lowering
    }

    fn collect_classes(&mut self, module: &ast::Module) {
        for (index, class) in module.classes.iter().enumerate() {
            if let Some(previous) = self.classes.iter().find(|item| item.name == class.name) {
                self.diagnostics.push(Diagnostic::error(
                    class.span,
                    format!(
                        "class `{}` is already defined at bytes {}..{}",
                        class.name, previous.span.start, previous.span.end
                    ),
                ));
            }
            self.classes.push(Class {
                id: ClassId(u32::try_from(index).unwrap_or(u32::MAX)),
                definition: class.definition,
                name: class.name.clone(),
                origin: ClassOrigin::Native,
                superclass: None,
                finalized: false,
                direct_slots: Vec::new(),
                effective_slots: Vec::new(),
                span: class.span,
            });
        }

        for (class_index, class) in module.classes.iter().enumerate() {
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
                            class: ClassId(u32::try_from(class_index).unwrap_or(u32::MAX)),
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
            if let Some(lowered_class) = self.classes.get_mut(class_index) {
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
                self.diagnostics.push(Diagnostic::error(
                    Span::default(),
                    format!(
                        "host-backed class `{}` conflicts with class defined at bytes {}..{}",
                        host_class.name, previous.span.start, previous.span.end
                    ),
                ));
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
                superclass: None,
                finalized: false,
                direct_slots: slots,
                effective_slots: Vec::new(),
                span: Span::default(),
            });
        }

        self.resolve_superclasses(module);
    }

    fn resolve_superclasses(&mut self, module: &ast::Module) {
        for (class_index, class) in module.classes.iter().enumerate() {
            let Some(superclass) = class.superclass.as_deref() else {
                continue;
            };
            if superclass == "<object>" {
                continue;
            }
            if matches!(superclass, "<integer>" | "<boolean>") {
                self.diagnostics.push(Diagnostic::error(
                    class.superclass_span,
                    format!(
                        "class `{}` cannot inherit from primitive `{superclass}`",
                        class.name
                    ),
                ));
                continue;
            }
            let Some((parent_id, parent_origin)) = self
                .classes
                .iter()
                .find(|candidate| candidate.name == superclass)
                .map(|parent| (parent.id, parent.origin))
            else {
                self.diagnostics.push(Diagnostic::error(
                    class.superclass_span,
                    format!(
                        "unresolved superclass `{superclass}` for class `{}`",
                        class.name
                    ),
                ));
                continue;
            };
            if matches!(parent_origin, ClassOrigin::HostBacked { .. }) {
                self.diagnostics.push(Diagnostic::error(
                    class.superclass_span,
                    format!(
                        "class `{}` cannot inherit from host-backed class `{superclass}`",
                        class.name
                    ),
                ));
                continue;
            }
            if let Some(lowered_class) = self.classes.get_mut(class_index) {
                lowered_class.superclass = Some(parent_id);
            }
        }
    }

    fn collect_slot_init_values(&mut self, module: &ast::Module) {
        let mut lowered_values = Vec::new();
        for (class_index, class) in module.classes.iter().enumerate() {
            for (slot_index, slot) in class.slots.iter().enumerate() {
                let Some(init_value) = &slot.init_value else {
                    continue;
                };
                let mut ctx = FunctionContext::new();
                let value = self.lower_expr(&mut ctx, init_value);
                self.diagnostics.extend(ctx.diagnostics);
                let slot_kind = self
                    .classes
                    .get(class_index)
                    .and_then(|class| class.direct_slots.get(slot_index))
                    .map_or(ValueKind::Error, |slot| slot.kind);
                let value_kind = self.expr_kind(&value);
                if !self.kinds_match(slot_kind, value_kind) {
                    self.diagnostics.push(Diagnostic::error(
                        init_value.span(),
                        format!(
                            "slot `{}` expects {} but init-value has {}",
                            slot.name,
                            self.kind_name(slot_kind),
                            self.kind_name(value_kind)
                        ),
                    ));
                }
                lowered_values.push((class_index, slot_index, value));
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
                self.diagnostics.push(Diagnostic::error(
                    class.span,
                    format!("class inheritance cycle involving `{}`", class.name),
                ));
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
        if let Some(class) = self.classes.get_mut(class_index) {
            class.effective_slots = effective_slots;
            class.finalized = true;
        }
        states[class_index] = FinalizeState::Done;
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
                self.diagnostics.push(Diagnostic::error(
                    slot.span,
                    format!(
                        "class `{class_name}` has duplicate effective slot `{}`",
                        slot.name
                    ),
                ));
            }
            if slots[..index]
                .iter()
                .any(|previous| previous.init_keyword == slot.init_keyword)
            {
                self.diagnostics.push(Diagnostic::error(
                    slot.span,
                    format!(
                        "class `{class_name}` has duplicate effective init keyword `{}:`",
                        slot.init_keyword
                    ),
                ));
            }
            if slots[..index]
                .iter()
                .any(|previous| previous.accessor.name == slot.accessor.name)
            {
                self.diagnostics.push(Diagnostic::error(
                    slot.span,
                    format!(
                        "class `{class_name}` has duplicate effective accessor `{}`",
                        slot.accessor.name
                    ),
                ));
            }
        }
    }

    fn check_duplicate_init_keywords(&mut self, class: &ast::Class, slots: &[Slot]) {
        for (index, slot) in slots.iter().enumerate() {
            if slots[..index]
                .iter()
                .any(|previous| previous.init_keyword == slot.init_keyword)
            {
                self.diagnostics.push(Diagnostic::error(
                    class.span,
                    format!(
                        "class `{}` has duplicate init keyword `{}:`",
                        class.name, slot.init_keyword
                    ),
                ));
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
                self.diagnostics.push(Diagnostic::error(
                    generic.span,
                    format!(
                        "generic `{}` is already defined at bytes {}..{}",
                        generic.name, previous.span.start, previous.span.end
                    ),
                ));
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
                        ValueKind::Any | ValueKind::Object(_) | ValueKind::Error
                    )
                {
                    self.diagnostics.push(Diagnostic::error(
                        generic
                            .params
                            .get(param_index)
                            .map_or(generic.span, |param| param.span),
                        format!(
                            "generic `{}` parameter {} contract must be <object> or a nominal class in this slice",
                            generic.name,
                            param_index + 1
                        ),
                    ));
                }
            }
            let return_annotation = generic
                .return_type
                .as_ref()
                .map(|annotation| self.resolve_type_annotation(annotation));
            self.generic_symbols.push(GenericSymbol {
                name: generic.name.clone(),
                id,
                param_kinds: param_kinds.clone(),
                return_kind: return_annotation.unwrap_or(ValueKind::I64),
                return_annotation,
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
                result_kind: return_annotation.unwrap_or(ValueKind::I64),
                return_annotation,
                span: generic.span,
            });
        }
    }

    fn collect_functions(&mut self, module: &ast::Module) {
        for (index, function) in module.functions.iter().enumerate() {
            if let Some(previous) = self
                .functions
                .iter()
                .find(|symbol| symbol.name == function.name)
            {
                self.diagnostics.push(Diagnostic::error(
                    function.span,
                    format!(
                        "function `{}` is already defined at bytes {}..{}",
                        function.name, previous.span.start, previous.span.end
                    ),
                ));
            }
            let param_kinds = function
                .params
                .iter()
                .map(|param| {
                    self.resolve_function_type(param.type_annotation.as_ref(), ValueKind::I64)
                })
                .collect::<Vec<_>>();
            let return_annotation = function
                .return_type
                .as_ref()
                .map(|annotation| self.resolve_type_annotation(annotation));
            self.functions.push(FunctionSymbol {
                name: function.name.clone(),
                id: FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                param_kinds,
                return_kind: return_annotation.unwrap_or(ValueKind::I64),
                return_annotation,
                span: function.span,
            });
        }
    }

    fn lower_module(&mut self, module: &ast::Module) -> Module {
        self.methods = module
            .methods
            .iter()
            .enumerate()
            .map(|(index, method)| {
                self.lower_method(MethodId(u32::try_from(index).unwrap_or(u32::MAX)), method)
            })
            .collect();

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
            diagnostics: self.diagnostics.clone(),
        }
    }

    fn lower_method(&mut self, id: MethodId, method: &ast::Method) -> Method {
        let mut ctx = FunctionContext::new();
        let generic_symbol = self.resolve_generic(&method.name).cloned();
        let generic = generic_symbol
            .as_ref()
            .map(|symbol| symbol.id)
            .unwrap_or_else(|| {
                self.diagnostics.push(Diagnostic::error(
                    method.span,
                    format!("unresolved generic `{}`", method.name),
                ));
                GenericFunctionId(u32::MAX)
            });
        let specializer = self
            .resolve_class(&method.param.class_name)
            .map(|class| class.id)
            .unwrap_or_else(|| {
                self.diagnostics.push(Diagnostic::error(
                    method.param.span,
                    format!("unresolved class `{}`", method.param.class_name),
                ));
                ClassId(u32::MAX)
            });
        if let Some(symbol) = &generic_symbol
            && let Some(expected) = symbol.param_kinds.first().copied().flatten()
            && !self.generic_specializer_matches(expected, specializer)
        {
            ctx.diagnostics.push(Diagnostic::error(
                method.param.span,
                format!(
                    "method `{}` specializer {} does not satisfy generic parameter 1 contract {}",
                    method.name,
                    self.kind_name(ValueKind::Object(specializer)),
                    self.kind_name(expected)
                ),
            ));
        }
        let param = ctx.add_local(
            self.local_definition(method.definition, &method.param.name),
            method.param.name.clone(),
            ValueKind::Object(specializer),
            method.param.span,
        );
        let body = self.lower_block(&mut ctx, &method.body);
        let body_kind = self.expr_kind(&body.result);
        let return_annotation = method
            .return_type
            .as_ref()
            .map(|annotation| self.resolve_type_annotation(annotation));
        let result_kind = return_annotation.unwrap_or(body_kind);
        if let Some(expected) = return_annotation
            && !self.kinds_match(expected, body_kind)
        {
            ctx.diagnostics.push(Diagnostic::error(
                body.result.span(),
                format!(
                    "method `{}` returns {} but body has {}",
                    method.name,
                    self.kind_name(expected),
                    self.kind_name(body_kind)
                ),
            ));
        }
        if let Some(symbol) = &generic_symbol
            && let Some(expected) = symbol.return_annotation
            && !self.kinds_match(expected, result_kind)
        {
            ctx.diagnostics.push(Diagnostic::error(
                body.result.span(),
                format!(
                    "method `{}` result {} does not satisfy generic return {}",
                    method.name,
                    self.kind_name(result_kind),
                    self.kind_name(expected)
                ),
            ));
        }
        self.diagnostics.extend(ctx.diagnostics);
        Method {
            id,
            definition: method.definition,
            generic,
            specializer,
            param,
            locals: ctx.locals,
            body,
            result_kind,
            return_annotation,
            span: method.span,
        }
    }

    fn lower_function(&mut self, id: FunctionId, function: &ast::Function) -> Function {
        let mut ctx = FunctionContext::new();
        let mut params = Vec::new();
        let symbol = self.functions.get(id.0 as usize).cloned();

        for (index, param) in function.params.iter().enumerate() {
            if ctx.find_local(&param.name).is_some() {
                ctx.diagnostics.push(Diagnostic::error(
                    param.span,
                    format!("parameter `{}` is already defined", param.name),
                ));
            }
            let kind = symbol
                .as_ref()
                .and_then(|symbol| symbol.param_kinds.get(index))
                .copied()
                .unwrap_or(ValueKind::Error);
            params.push(ctx.add_local(
                self.local_definition(function.definition, &param.name),
                param.name.clone(),
                kind,
                param.span,
            ));
        }

        let body = self.lower_block(&mut ctx, &function.body);
        let body_kind = self.expr_kind(&body.result);
        let return_annotation = symbol.as_ref().and_then(|symbol| symbol.return_annotation);
        let result_kind = return_annotation.unwrap_or(body_kind);
        if let Some(expected) = return_annotation
            && !self.kinds_match(expected, body_kind)
        {
            ctx.diagnostics.push(Diagnostic::error(
                body.result.span(),
                format!(
                    "function `{}` returns {} but body has {}",
                    function.name,
                    self.kind_name(expected),
                    self.kind_name(body_kind)
                ),
            ));
        }
        if let Some(symbol) = self.functions.get_mut(id.0 as usize)
            && symbol.return_annotation.is_none()
        {
            symbol.return_kind = result_kind;
        }
        self.diagnostics.extend(ctx.diagnostics);

        Function {
            id,
            definition: function.definition,
            name: function.name.clone(),
            params,
            param_kinds: symbol.map_or_else(Vec::new, |symbol| symbol.param_kinds),
            locals: ctx.locals,
            body,
            result_kind,
            return_annotation,
            span: function.span,
        }
    }

    fn lower_block(&self, ctx: &mut FunctionContext, block: &ast::Block) -> Block {
        let mut lets = Vec::new();
        for local in &block.lets {
            let value = self.lower_expr(ctx, &local.value);
            if ctx.find_local(&local.name).is_some() {
                ctx.diagnostics.push(Diagnostic::error(
                    local.span,
                    format!("local `{}` is already defined", local.name),
                ));
            }
            let id = ctx.add_local(
                self.local_definition(local.definition, &local.name),
                local.name.clone(),
                self.expr_kind(&value),
                local.span,
            );
            lets.push(Let {
                local: id,
                value,
                span: local.span,
            });
        }

        Block {
            lets,
            result: self.lower_expr(ctx, &block.result),
            span: block.span,
        }
    }

    fn lower_expr(&self, ctx: &mut FunctionContext, expr: &ast::Expr) -> Expr {
        match expr {
            ast::Expr::Number { value, span, .. } => Expr::ConstI64 {
                value: *value,
                span: *span,
            },
            ast::Expr::Bool { value, span, .. } => Expr::ConstBool {
                value: *value,
                span: *span,
            },
            ast::Expr::Name { name, span, .. } => match ctx.find_local(name) {
                Some(local) => Expr::Local {
                    local,
                    result_kind: ctx.local_kind(local),
                    span: *span,
                },
                None => self.lower_known_name(ctx, name, *span),
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
                    return self.lower_apply(ctx, args, *span);
                }
                let lowered_args: Vec<_> =
                    args.iter().map(|arg| self.lower_expr(ctx, arg)).collect();
                if callee == "class-of" {
                    if args.len() == 1 {
                        let object = lowered_args.into_iter().next().expect("arity was checked");
                        return self.lower_class_of(ctx, object, *span);
                    }
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        format!("class-of expects 1 argument but got {}", args.len()),
                    ));
                    return Expr::Error { span: *span };
                }
                if let Some(symbol) = self.resolve_function(callee) {
                    if symbol.arity() == args.len() {
                        for (index, (arg, expected)) in
                            lowered_args.iter().zip(&symbol.param_kinds).enumerate()
                        {
                            let actual = self.expr_kind(arg);
                            if !self.kinds_match(*expected, actual) {
                                ctx.diagnostics.push(Diagnostic::error(
                                    arg.span(),
                                    format!(
                                        "function `{callee}` argument {} expects {} but got {}",
                                        index + 1,
                                        self.kind_name(*expected),
                                        self.kind_name(actual)
                                    ),
                                ));
                            }
                        }
                        return Expr::DirectCall {
                            function: symbol.id,
                            args: lowered_args,
                            span: *span,
                        };
                    }
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        format!(
                            "function `{callee}` expects {} arguments but got {}",
                            symbol.arity(),
                            args.len()
                        ),
                    ));
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
                                ctx.diagnostics.push(Diagnostic::error(
                                    arg.span(),
                                    format!(
                                        "generic `{callee}` argument {} expects {} but got {}",
                                        index + 1,
                                        self.kind_name(expected),
                                        self.kind_name(actual)
                                    ),
                                ));
                            }
                        }
                        return Expr::GenericCall {
                            generic: symbol.id,
                            args: lowered_args,
                            span: *span,
                        };
                    }
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        format!(
                            "generic `{callee}` expects {} arguments but got {}",
                            symbol.arity(),
                            args.len()
                        ),
                    ));
                    return Expr::Error { span: *span };
                }
                if self.has_accessor(callee) {
                    if args.len() == 1 {
                        let receiver = lowered_args.into_iter().next().expect("arity was checked");
                        return self.lower_accessor_call(
                            ctx,
                            receiver,
                            callee,
                            *span,
                            AccessorSyntax::Call,
                        );
                    }
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        format!(
                            "accessor `{callee}` expects 1 argument but got {}",
                            args.len()
                        ),
                    ));
                    return Expr::Error { span: *span };
                }
                ctx.diagnostics.push(Diagnostic::error(
                    *span,
                    format!("unresolved function or generic `{callee}`"),
                ));
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
            ast::Expr::Binary {
                op, lhs, rhs, span, ..
            } => {
                let lhs = self.lower_expr(ctx, lhs);
                let rhs = self.lower_expr(ctx, rhs);
                self.expect_i64_operands(ctx, *span, &lhs, &rhs, "arithmetic operands must be i64");
                Expr::Binary {
                    op: (*op).into(),
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span: *span,
                }
            }
            ast::Expr::Compare {
                op, lhs, rhs, span, ..
            } => {
                let lhs = self.lower_expr(ctx, lhs);
                let rhs = self.lower_expr(ctx, rhs);
                self.expect_i64_operands(ctx, *span, &lhs, &rhs, "comparison operands must be i64");
                Expr::Compare {
                    op: (*op).into(),
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    span: *span,
                }
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
                let then_branch = self.lower_expr(ctx, then_branch);
                if let (Some(refinement), Some(previous_kind)) = (refinement, previous_kind) {
                    let _ = ctx.refine_local(refinement.local, previous_kind);
                }
                let else_branch = self.lower_expr(ctx, else_branch);
                let condition_kind = self.expr_kind(&condition);
                if !matches!(condition_kind, ValueKind::Bool | ValueKind::Error) {
                    ctx.diagnostics
                        .push(Diagnostic::error(*span, "if condition must be bool"));
                }
                let then_kind = self.expr_kind(&then_branch);
                let else_kind = self.expr_kind(&else_branch);
                if then_kind != else_kind
                    && !matches!(then_kind, ValueKind::Error)
                    && !matches!(else_kind, ValueKind::Error)
                {
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        "if branches must have matching result kinds",
                    ));
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
            } => self.lower_handle(ctx, body, restart, *span),
            ast::Expr::Error { span, .. } => Expr::Error { span: *span },
        }
    }

    fn lower_known_name(&self, ctx: &mut FunctionContext, name: &str, span: Span) -> Expr {
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
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    format!("accessor `{name}` is ambiguous as a value"),
                ));
                Expr::Error { span }
            }
            AccessorResolution::None => {
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    format!("unresolved local `{name}`"),
                ));
                Expr::Error { span }
            }
        }
    }

    fn lower_apply(&self, ctx: &mut FunctionContext, args: &[ast::Expr], span: Span) -> Expr {
        let Some((target, args)) = args.split_first() else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                "apply expects a callable and arguments",
            ));
            return Expr::Error { span };
        };
        let target = self.lower_expr(ctx, target);
        let lowered_args = args
            .iter()
            .map(|arg| self.lower_expr(ctx, arg))
            .collect::<Vec<_>>();
        match self.expr_kind(&target) {
            ValueKind::KnownCallable(KnownCallable::DirectFunction(function)) => {
                self.lower_applied_direct_call(ctx, function, lowered_args, span)
            }
            ValueKind::KnownCallable(KnownCallable::GenericFunction(generic)) => {
                self.lower_applied_generic_call(ctx, generic, lowered_args, span)
            }
            ValueKind::KnownCallable(KnownCallable::SlotAccessor { .. }) => {
                ctx.diagnostics.push(Diagnostic::error(
                    target.span(),
                    "apply does not support slot accessors yet",
                ));
                Expr::Error { span }
            }
            ValueKind::Error => Expr::Error { span },
            actual => {
                ctx.diagnostics.push(Diagnostic::error(
                    target.span(),
                    format!(
                        "apply target must be a known direct or generic function, got {}",
                        self.kind_name(actual)
                    ),
                ));
                Expr::Error { span }
            }
        }
    }

    fn lower_applied_direct_call(
        &self,
        ctx: &mut FunctionContext,
        function: FunctionId,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let Some(symbol) = self.functions.get(function.0 as usize) else {
            return Expr::Error { span };
        };
        if symbol.arity() != args.len() {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!(
                    "function `{}` expects {} arguments but apply got {}",
                    symbol.name,
                    symbol.arity(),
                    args.len()
                ),
            ));
            return Expr::Error { span };
        }
        for (index, (arg, expected)) in args.iter().zip(&symbol.param_kinds).enumerate() {
            let actual = self.expr_kind(arg);
            if !self.kinds_match(*expected, actual) {
                ctx.diagnostics.push(Diagnostic::error(
                    arg.span(),
                    format!(
                        "function `{}` argument {} expects {} but got {}",
                        symbol.name,
                        index + 1,
                        self.kind_name(*expected),
                        self.kind_name(actual)
                    ),
                ));
            }
        }
        Expr::DirectCall {
            function,
            args,
            span,
        }
    }

    fn lower_applied_generic_call(
        &self,
        ctx: &mut FunctionContext,
        generic: GenericFunctionId,
        args: Vec<Expr>,
        span: Span,
    ) -> Expr {
        let Some(symbol) = self.generic_symbols.get(generic.0 as usize) else {
            return Expr::Error { span };
        };
        if symbol.arity() != args.len() {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!(
                    "generic `{}` expects {} arguments but apply got {}",
                    symbol.name,
                    symbol.arity(),
                    args.len()
                ),
            ));
            return Expr::Error { span };
        }
        for (index, (arg, expected)) in args.iter().zip(&symbol.param_kinds).enumerate() {
            let Some(expected) = *expected else {
                continue;
            };
            let actual = self.expr_kind(arg);
            if !self.generic_argument_matches(expected, actual) {
                ctx.diagnostics.push(Diagnostic::error(
                    arg.span(),
                    format!(
                        "generic `{}` argument {} expects {} but got {}",
                        symbol.name,
                        index + 1,
                        self.kind_name(expected),
                        self.kind_name(actual)
                    ),
                ));
            }
        }
        Expr::GenericCall {
            generic,
            args,
            span,
        }
    }

    fn lower_signal(&self, ctx: &mut FunctionContext, condition: &ast::Expr, span: Span) -> Expr {
        let condition = self.lower_expr(ctx, condition);
        let condition_class = match self.expr_kind(&condition) {
            ValueKind::Object(class) => class,
            ValueKind::Error => ClassId(u32::MAX),
            _ => {
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    "signal condition must be a known object",
                ));
                ClassId(u32::MAX)
            }
        };
        let Some(active) = ctx.active_restart() else {
            ctx.diagnostics
                .push(Diagnostic::error(span, "signal requires an active restart"));
            return Expr::Signal {
                condition: Box::new(condition),
                condition_class,
                result_kind: ValueKind::Error,
                span,
            };
        };
        if condition_class != ClassId(u32::MAX)
            && !self.is_same_or_subclass(condition_class, active.specializer)
        {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                "signal condition is not handled by the active restart",
            ));
            return Expr::Signal {
                condition: Box::new(condition),
                condition_class,
                result_kind: ValueKind::Error,
                span,
            };
        }
        Expr::Signal {
            condition: Box::new(condition),
            condition_class,
            result_kind: active.result_kind,
            span,
        }
    }

    fn lower_handle(
        &self,
        ctx: &mut FunctionContext,
        body: &ast::Expr,
        restart: &ast::Restart,
        span: Span,
    ) -> Expr {
        let specializer = self
            .resolve_class(&restart.class_name)
            .map(|class| class.id)
            .unwrap_or_else(|| {
                ctx.diagnostics.push(Diagnostic::error(
                    restart.span,
                    format!("unresolved class `{}`", restart.class_name),
                ));
                ClassId(u32::MAX)
            });

        let scope_start = ctx.scope_start();
        let condition_local = restart.condition_name.as_ref().map(|name| {
            if ctx.find_local(name).is_some() {
                ctx.diagnostics.push(Diagnostic::error(
                    restart.condition_span,
                    format!("restart condition `{name}` is already defined"),
                ));
            }
            ctx.add_local(
                self.restart_condition_definition(restart, name),
                name.clone(),
                ValueKind::Object(specializer),
                restart.condition_span,
            )
        });
        let restart_body = self.lower_expr(ctx, &restart.body);
        let restart_kind = self.expr_kind(&restart_body);
        ctx.hide_locals_from(scope_start);

        ctx.push_restart(ActiveRestart {
            specializer,
            result_kind: restart_kind,
        });
        let body = self.lower_expr(ctx, body);
        ctx.pop_restart();

        let body_kind = self.expr_kind(&body);
        if body_kind != restart_kind
            && !matches!(body_kind, ValueKind::Error)
            && !matches!(restart_kind, ValueKind::Error)
        {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                "handle body and restart must have matching result kinds",
            ));
        }

        Expr::Handle {
            body: Box::new(body),
            restart: Restart {
                specializer,
                condition_local,
                body: Box::new(restart_body),
                result_kind: restart_kind,
                span: restart.span,
            },
            span,
        }
    }

    fn expect_i64_operands(
        &self,
        ctx: &mut FunctionContext,
        span: Span,
        lhs: &Expr,
        rhs: &Expr,
        message: &str,
    ) {
        let lhs_kind = self.expr_kind(lhs);
        let rhs_kind = self.expr_kind(rhs);
        if matches!(lhs_kind, ValueKind::I64 | ValueKind::Error)
            && matches!(rhs_kind, ValueKind::I64 | ValueKind::Error)
        {
            return;
        }
        ctx.diagnostics.push(Diagnostic::error(span, message));
    }

    fn lower_class_object(&self, ctx: &mut FunctionContext, class_name: &str, span: Span) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!("unresolved class `{class_name}`"),
            ));
            return Expr::Error { span };
        };
        Expr::ClassObject {
            class: class.id,
            span,
        }
    }

    fn lower_class_of(&self, ctx: &mut FunctionContext, object: Expr, span: Span) -> Expr {
        let class = match self.expr_kind(&object) {
            ValueKind::Object(class) => class,
            ValueKind::Error => return Expr::Error { span },
            _ => {
                ctx.diagnostics
                    .push(Diagnostic::error(span, "class-of requires a known object"));
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
        &self,
        ctx: &mut FunctionContext,
        value: &ast::Expr,
        class_name: &str,
        span: Span,
    ) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!("unresolved class `{class_name}`"),
            ));
            return Expr::Error { span };
        };
        let value = self.lower_expr(ctx, value);
        match self.expr_kind(&value) {
            ValueKind::Any | ValueKind::Object(_) | ValueKind::Error => {}
            actual => {
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    format!(
                        "instance? expects <object> or a known object, got {}",
                        self.kind_name(actual)
                    ),
                ));
            }
        }
        if let ValueKind::Object(actual) = self.expr_kind(&value)
            && !self.classes_may_overlap(actual, class.id)
        {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!(
                    "instance? test is impossible: {} is not {}",
                    self.kind_name(ValueKind::Object(actual)),
                    self.kind_name(ValueKind::Object(class.id))
                ),
            ));
        }
        Expr::InstanceOf {
            value: Box::new(value),
            class: class.id,
            span,
        }
    }

    fn lower_cast(
        &self,
        ctx: &mut FunctionContext,
        value: &ast::Expr,
        class_name: &str,
        span: Span,
    ) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!("unresolved class `{class_name}`"),
            ));
            return Expr::Error { span };
        };
        let value = self.lower_expr(ctx, value);
        match self.expr_kind(&value) {
            ValueKind::Any | ValueKind::Object(_) | ValueKind::Error => {}
            actual => {
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    format!(
                        "as expects <object> or a known object, got {}",
                        self.kind_name(actual)
                    ),
                ));
            }
        }
        if let ValueKind::Object(actual) = self.expr_kind(&value)
            && !self.classes_may_overlap(actual, class.id)
        {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!(
                    "cast is impossible: {} is not {}",
                    self.kind_name(ValueKind::Object(actual)),
                    self.kind_name(ValueKind::Object(class.id))
                ),
            ));
        }
        Expr::Cast {
            value: Box::new(value),
            class: class.id,
            span,
        }
    }

    fn lower_make_object(
        &self,
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
                ctx.diagnostics.push(Diagnostic::error(
                    class_expr.span(),
                    "make class argument must be a known class object",
                ));
                return Expr::Error { span };
            }
        };
        let Some(class) = self.classes.get(class_id.0 as usize) else {
            return Expr::Error { span };
        };
        if matches!(class.origin, ClassOrigin::HostBacked { .. }) {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!(
                    "host-backed class `{}` cannot be constructed with make",
                    class.name
                ),
            ));
            return Expr::Error { span };
        }

        let mut resolved = Vec::new();
        for field in fields {
            if resolved.iter().any(|resolved: &FieldInit| {
                class.effective_slots[resolved.slot.0 as usize].init_keyword == field.name
            }) {
                ctx.diagnostics.push(Diagnostic::error(
                    field.span,
                    format!("duplicate initializer for keyword `{}:`", field.name),
                ));
                continue;
            }
            let Some(slot) = class
                .effective_slots
                .iter()
                .find(|slot| slot.init_keyword == field.name)
            else {
                ctx.diagnostics.push(Diagnostic::error(
                    field.span,
                    format!(
                        "class `{}` has no init keyword `{}:`",
                        class.name, field.name
                    ),
                ));
                continue;
            };
            let value = self.lower_expr(ctx, &field.value);
            let value_kind = self.expr_kind(&value);
            if !self.kinds_match(slot.kind, value_kind) {
                ctx.diagnostics.push(Diagnostic::error(
                    field.span,
                    format!(
                        "slot `{}` expects {} but initializer has {}",
                        slot.name,
                        self.kind_name(slot.kind),
                        self.kind_name(value_kind)
                    ),
                ));
            }
            resolved.push(FieldInit {
                slot: slot.id,
                value,
                span: field.span,
            });
        }

        for slot in &class.effective_slots {
            if !resolved.iter().any(|field| field.slot == slot.id) {
                if let Some(init_value) = &slot.init_value {
                    resolved.push(FieldInit {
                        slot: slot.id,
                        value: init_value.clone(),
                        span: init_value.span(),
                    });
                } else if slot.required_init_keyword {
                    ctx.diagnostics.push(Diagnostic::error(
                        span,
                        format!(
                            "missing required init keyword `{}:` for slot `{}`",
                            slot.init_keyword, slot.name
                        ),
                    ));
                } else {
                    ctx.diagnostics.push(Diagnostic::error(
                        span,
                        format!("missing initializer for slot `{}`", slot.name),
                    ));
                }
            }
        }

        Expr::MakeObject {
            class: Box::new(class_expr),
            class_id,
            fields: resolved,
            span,
        }
    }

    fn lower_slot_read(
        &self,
        ctx: &mut FunctionContext,
        receiver: &ast::Expr,
        slot_name: &str,
        span: Span,
    ) -> Expr {
        let receiver = self.lower_expr(ctx, receiver);
        self.lower_accessor_call(ctx, receiver, slot_name, span, AccessorSyntax::Slot)
    }

    fn lower_accessor_call(
        &self,
        ctx: &mut FunctionContext,
        receiver: Expr,
        accessor_name: &str,
        span: Span,
        syntax: AccessorSyntax,
    ) -> Expr {
        let ValueKind::Object(class_id) = self.expr_kind(&receiver) else {
            let message = match syntax {
                AccessorSyntax::Slot => {
                    "slot read receiver does not have a known object class".to_string()
                }
                AccessorSyntax::Call => {
                    format!(
                        "accessor `{accessor_name}` receiver does not have a known object class"
                    )
                }
            };
            ctx.diagnostics.push(Diagnostic::error(span, message));
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
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!("class `{}` has no slot `{accessor_name}`", class.name),
            ));
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
            Expr::ConstI64 { .. } | Expr::Binary { .. } => ValueKind::I64,
            Expr::GenericCall { generic, .. } => self
                .generic_symbols
                .get(generic.0 as usize)
                .map_or(ValueKind::Error, |symbol| symbol.return_kind),
            Expr::ClassObject { class, .. } | Expr::ClassOf { class, .. } => {
                ValueKind::ClassObject(*class)
            }
            Expr::KnownCallable { callable, .. } => ValueKind::KnownCallable(*callable),
            Expr::AccessorCall { result_kind, .. } => *result_kind,
            Expr::ConstBool { .. } | Expr::Compare { .. } | Expr::InstanceOf { .. } => {
                ValueKind::Bool
            }
            Expr::Local { result_kind, .. } => *result_kind,
            Expr::Cast { class, .. } => ValueKind::Object(*class),
            Expr::Signal { result_kind, .. } => *result_kind,
            Expr::DirectCall { function, .. } => self
                .functions
                .get(function.0 as usize)
                .map_or(ValueKind::Error, |symbol| symbol.return_kind),
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
            Expr::Handle { restart, .. } => restart.result_kind,
            Expr::Error { .. } => ValueKind::Error,
        }
    }

    fn resolve_class(&self, name: &str) -> Option<&Class> {
        self.classes.iter().find(|class| class.name == name)
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

    fn resolve_function_type(
        &mut self,
        annotation: Option<&ast::TypeAnnotation>,
        default: ValueKind,
    ) -> ValueKind {
        annotation.map_or(default, |annotation| {
            self.resolve_type_annotation(annotation)
        })
    }

    fn resolve_type_annotation(&mut self, annotation: &ast::TypeAnnotation) -> ValueKind {
        self.resolve_type_name(&annotation.name, annotation.class_object, annotation.span)
    }

    fn resolve_type_name(&mut self, type_name: &str, class_object: bool, span: Span) -> ValueKind {
        if class_object {
            return self
                .resolve_class(type_name)
                .map(|class| ValueKind::ClassObject(class.id))
                .unwrap_or_else(|| {
                    self.diagnostics.push(Diagnostic::error(
                        span,
                        format!("unresolved class-object type `{type_name}`"),
                    ));
                    ValueKind::Error
                });
        }
        match type_name {
            "<object>" => ValueKind::Any,
            "<integer>" => ValueKind::I64,
            "<boolean>" => ValueKind::Bool,
            name => self
                .resolve_class(name)
                .map(|class| ValueKind::Object(class.id))
                .unwrap_or_else(|| {
                    self.diagnostics.push(Diagnostic::error(
                        span,
                        format!("unresolved slot type `{name}`"),
                    ));
                    ValueKind::Error
                }),
        }
    }

    fn kinds_match(&self, expected: ValueKind, actual: ValueKind) -> bool {
        (matches!(expected, ValueKind::Any) && !matches!(actual, ValueKind::KnownCallable(_)))
            || matches!((expected, actual), (ValueKind::Object(expected), ValueKind::Object(actual)) if self.is_same_or_subclass(actual, expected))
            || expected == actual
            || matches!(expected, ValueKind::Error)
            || matches!(actual, ValueKind::Error)
    }

    fn generic_specializer_matches(&self, expected: ValueKind, specializer: ClassId) -> bool {
        match expected {
            ValueKind::Any | ValueKind::Error => true,
            ValueKind::Object(class) => self.is_same_or_subclass(specializer, class),
            ValueKind::I64
            | ValueKind::Bool
            | ValueKind::ClassObject(_)
            | ValueKind::KnownCallable(_) => false,
        }
    }

    fn generic_argument_matches(&self, expected: ValueKind, actual: ValueKind) -> bool {
        match expected {
            ValueKind::Any => matches!(actual, ValueKind::Object(_) | ValueKind::Error),
            ValueKind::Object(expected) => {
                matches!(actual, ValueKind::Object(actual) if self.is_same_or_subclass(actual, expected))
                    || matches!(actual, ValueKind::Error)
            }
            ValueKind::Error => true,
            ValueKind::I64
            | ValueKind::Bool
            | ValueKind::ClassObject(_)
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
            ValueKind::Bool => "<boolean>".into(),
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
    use crate::{ast, syntax};

    fn lower_source(source: &str) -> Module {
        let tree = syntax::parse(source);
        let ast = ast::lower(&tree);
        lower(&ast)
    }

    fn lower_source_with_host_registry(source: &str, registry: &HostMetaRegistry) -> Module {
        let tree = syntax::parse(source);
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
        assert!(matches!(
            lhs.as_ref(),
            Expr::DirectCall {
                function: FunctionId(0),
                ..
            }
        ));
        assert!(matches!(
            rhs.as_ref(),
            Expr::GenericCall {
                generic: GenericFunctionId(0),
                ..
            }
        ));
    }

    #[test]
    fn resolves_slot_accessor_values_without_applying_them() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let getter = radius;\n  1\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::KnownCallable(KnownCallable::SlotAccessor {
                accessor: module.classes[0].direct_slots[0].accessor.definition,
                class: ClassId(0),
                slot: SlotId(0),
            })
        );
    }

    #[test]
    fn reports_invalid_apply_targets() {
        let non_callable = lower_source("define function main()\n  apply(1, 2)\nend;");
        assert!(non_callable.diagnostics.iter().any(|diagnostic| {
            diagnostic.message
                == "apply target must be a known direct or generic function, got <integer>"
        }));

        let accessor = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  apply(radius, make(<circle>, radius: 3))\nend;",
        );
        assert!(accessor.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "apply does not support slot accessors yet"
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
        assert_eq!(module.classes.len(), 1);
        assert_eq!(
            module.classes[0].origin,
            ClassOrigin::HostBacked {
                host_class: HostClassId(0),
            }
        );
        assert_eq!(
            module.classes[0].direct_slots[0].origin,
            SlotOrigin::HostBacked {
                host_class: HostClassId(0),
                host_slot: HostSlotId(0),
            }
        );
        assert_eq!(module.methods[0].specializer, ClassId(0));
        assert!(matches!(module.methods[0].body.result, Expr::Binary { .. }));
    }

    #[test]
    fn rejects_make_for_host_backed_classes() {
        let registry = rust_circle_registry();
        let module = lower_source_with_host_registry(
            "define function main()\n  make(<rust-circle>, radius: 10)\nend;",
            &registry,
        );

        assert!(module.diagnostics.iter().any(|diagnostic| {
            diagnostic.message
                == "host-backed class `<rust-circle>` cannot be constructed with make"
        }));
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
        assert_eq!(module.classes[0].id, ClassId(0));
        assert_eq!(module.classes[0].direct_slots[0].id, SlotId(0));
        assert_eq!(module.classes[0].direct_slots[0].kind, ValueKind::I64);
        assert_eq!(
            module.classes[0].direct_slots[0].accessor.definition,
            module.classes[0].direct_slots[0].definition
        );
        assert_eq!(module.generics[0].id, GenericFunctionId(0));
        assert_eq!(module.methods[0].id, MethodId(0));
        assert_eq!(module.methods[0].generic, GenericFunctionId(0));
        assert_eq!(module.methods[0].specializer, ClassId(0));
        let Expr::GenericCall { generic, args, .. } = &module.functions[0].body.result else {
            panic!("expected generic call");
        };
        assert_eq!(*generic, GenericFunctionId(0));
        assert!(matches!(
            args[0],
            Expr::MakeObject {
                class_id: ClassId(0),
                ..
            }
        ));
        let Expr::Binary { lhs, .. } = &module.methods[0].body.result else {
            panic!("expected method body binary expression");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::AccessorCall {
                class: ClassId(0),
                slot: SlotId(0),
                ..
            }
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
        assert_eq!(module.classes[1].superclass, Some(ClassId(0)));
        assert!(module.classes[1].finalized);
        assert_eq!(module.classes[1].direct_slots.len(), 1);
        assert_eq!(module.classes[1].effective_slots.len(), 2);
        assert_eq!(module.classes[1].effective_slots[0].name, "x");
        assert_eq!(
            module.classes[1].effective_slots[0].source_class,
            ClassId(0)
        );
        assert_eq!(module.classes[1].effective_slots[0].source_slot, SlotId(0));
        assert_eq!(module.classes[1].effective_slots[1].name, "y");
        assert_eq!(
            module.classes[1].effective_slots[1].source_class,
            ClassId(1)
        );

        let Expr::AccessorCall { class, slot, .. } = &module.functions[0].body.result else {
            panic!("expected inherited slot read");
        };
        assert_eq!(*class, ClassId(1));
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
        assert_eq!(
            module.classes[2].direct_slots[0].kind,
            ValueKind::Object(ClassId(0))
        );
        assert_eq!(
            module.functions[0].result_kind,
            ValueKind::Object(ClassId(0))
        );
        assert_eq!(
            module.functions[1].result_kind,
            ValueKind::Object(ClassId(0))
        );
        assert_eq!(module.methods[0].specializer, ClassId(1));
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
                class: ClassId(0),
                ..
            }
        ));
        let Expr::AccessorCall {
            receiver, class, ..
        } = then_branch.as_ref()
        else {
            panic!("expected parent slot read");
        };
        assert_eq!(*class, ClassId(0));
        assert!(matches!(
            receiver.as_ref(),
            Expr::Cast {
                class: ClassId(0),
                ..
            }
        ));

        let Expr::Handle { body, restart, .. } = &module.functions[1].body.result else {
            panic!("expected restart handle");
        };
        assert_eq!(restart.specializer, ClassId(0));
        assert!(matches!(
            body.as_ref(),
            Expr::Signal {
                condition_class: ClassId(1),
                ..
            }
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
        assert!(instance.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "instance? test is impossible: <square> is not <circle>"
        }));

        let cast = lower_source(
            "define class <shape> (<object>)\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 1;\nend;\n\
             define class <square> (<shape>)\nend;\n\
             define function main()\n  as(<circle>, make(<square>)).radius\nend;",
        );
        assert!(cast.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "cast is impossible: <square> is not <circle>"
        }));
    }

    #[test]
    fn reports_bad_superclasses_and_effective_slot_conflicts() {
        let unknown = lower_source(
            "define class <child> (<missing>)\nend;\n\
             define function main()\n  0\nend;",
        );
        assert!(unknown.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "unresolved superclass `<missing>` for class `<child>`"
        }));

        let primitive = lower_source(
            "define class <bad> (<boolean>)\nend;\n\
             define function main()\n  0\nend;",
        );
        assert!(primitive.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "class `<bad>` cannot inherit from primitive `<boolean>`"
        }));

        let duplicate = lower_source(
            "define class <shape> (<object>)\n  slot radius :: <integer>, init-value: 1;\nend;\n\
             define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  0\nend;",
        );
        assert!(duplicate.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "class `<circle>` has duplicate effective slot `radius`"
        }));
        assert!(duplicate.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "class `<circle>` has duplicate effective accessor `radius`"
        }));
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
        assert_eq!(module.generics[0].result_kind, ValueKind::I64);
        assert_eq!(module.generics[0].return_annotation, Some(ValueKind::I64));
        assert_eq!(module.methods[0].return_annotation, Some(ValueKind::I64));
        assert_eq!(module.methods[0].result_kind, ValueKind::I64);
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
        assert!(bad_call.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "generic `area` argument 1 expects <object> but got <integer>"
        }));

        let bad_method_return = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <boolean>\n  #t\nend;",
        );
        assert!(bad_method_return.diagnostics.iter().any(|diagnostic| {
            diagnostic.message
                == "method `area` result <boolean> does not satisfy generic return <integer>"
        }));

        let bad_body = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <integer>\n  #t\nend;",
        );
        assert!(bad_body.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "method `area` returns <integer> but body has <boolean>"
        }));

        let bad_specializer = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define generic area(x :: <circle>) => <integer>;\n\
             define method area(s :: <square>) => <integer>\n  s.side * s.side\nend;",
        );
        assert!(bad_specializer.diagnostics.iter().any(|diagnostic| {
            diagnostic.message
                == "method `area` specializer <square> does not satisfy generic parameter 1 contract <circle>"
        }));
    }

    #[test]
    fn resolves_explicit_slot_accessor_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = make(<circle>, radius: 7);\n  radius(c)\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
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
        assert_eq!(*accessor, module.classes[0].direct_slots[0].definition);
        assert_eq!(*class, ClassId(0));
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
        assert_eq!(
            module.functions[0].locals[0].kind,
            ValueKind::ClassObject(ClassId(0))
        );
        let Expr::ClassObject { class, .. } = &module.functions[0].body.lets[0].value else {
            panic!("expected class object let value");
        };
        assert_eq!(*class, ClassId(0));

        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  let c = make(<circle>, radius: 7);\n  let k = class-of(c);\n  make(k, radius: 8).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(
            module.functions[0].locals[1].kind,
            ValueKind::ClassObject(ClassId(0))
        );
        let Expr::ClassOf { class, .. } = &module.functions[0].body.lets[1].value else {
            panic!("expected class-of expression");
        };
        assert_eq!(*class, ClassId(0));
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
        assert_eq!(module.functions[0].param_kinds, [ValueKind::Bool]);
        assert_eq!(module.functions[0].return_annotation, Some(ValueKind::Bool));
        assert_eq!(
            module.functions[1].param_kinds,
            [ValueKind::ClassObject(ClassId(0)), ValueKind::I64]
        );
        assert_eq!(
            module.functions[1].return_annotation,
            Some(ValueKind::Object(ClassId(0)))
        );
        assert_eq!(module.functions[2].result_kind, ValueKind::I64);
    }

    #[test]
    fn reports_typed_direct_function_mismatches() {
        let argument = lower_source(
            "define function keep?(flag :: <boolean>) => <boolean>\n  flag\nend;\n\
             define function main()\n  keep?(1)\nend;",
        );
        assert!(argument.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "function `keep?` argument 1 expects <boolean> but got <integer>"
        }));

        let result = lower_source("define function bad() => <boolean>\n  1\nend;");
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "function `bad` returns <boolean> but body has <integer>"
        }));
    }

    #[test]
    fn resolves_instance_tests_casts_and_true_branch_refinement() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function radius-or-zero(x :: <object>) => <integer>\n  if instance?(x, <circle>) then x.radius else 0 end\nend;\n\
             define function radius(x :: <object>) => <integer>\n  as(<circle>, x).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
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
                class: ClassId(0),
                ..
            }
        ));
        assert_eq!(
            *refinement,
            Some(Refinement {
                local: LocalId(0),
                kind: ValueKind::Object(ClassId(0)),
            })
        );
        let Expr::AccessorCall { receiver, .. } = then_branch.as_ref() else {
            panic!("expected refined slot accessor");
        };
        assert!(matches!(
            receiver.as_ref(),
            Expr::Local {
                local: LocalId(0),
                result_kind: ValueKind::Object(ClassId(0)),
                ..
            }
        ));
        let Expr::AccessorCall { receiver, .. } = &module.functions[1].body.result else {
            panic!("expected cast slot accessor");
        };
        assert!(matches!(
            receiver.as_ref(),
            Expr::Cast {
                class: ClassId(0),
                ..
            }
        ));
    }

    #[test]
    fn reports_impossible_instance_tests_and_casts() {
        let instance = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define function main()\n  if instance?(make(<square>, side: 2), <circle>) then 1 else 0 end\nend;",
        );
        assert!(instance.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "instance? test is impossible: <square> is not <circle>"
        }));

        let cast = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
             define function main()\n  as(<circle>, make(<square>, side: 2)).radius\nend;",
        );
        assert!(cast.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "cast is impossible: <square> is not <circle>"
        }));

        let primitive = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  instance?(1, <circle>)\nend;",
        );
        assert!(primitive.diagnostics.iter().any(|diagnostic| {
            diagnostic.message == "instance? expects <object> or a known object, got <integer>"
        }));
    }

    #[test]
    fn resolves_typed_slots_and_checks_initializers() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <box> (<object>)\n  slot item :: <circle>;\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<box>, item: make(<circle>, radius: 7), valid?: #t).valid? then make(<box>, item: make(<circle>, radius: 7), valid?: #t).item.radius else 0 end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].direct_slots[0].kind, ValueKind::I64);
        assert_eq!(
            module.classes[1].direct_slots[0].kind,
            ValueKind::Object(ClassId(0))
        );
        assert_eq!(module.classes[1].direct_slots[1].kind, ValueKind::Bool);
        assert_eq!(module.functions[0].result_kind, ValueKind::I64);
    }

    #[test]
    fn untyped_slots_default_to_object_top() {
        let module = lower_source(
            "define class <box> (<object>)\n  slot value;\n  slot flag;\nend;\n\
             define function main()\n  make(<box>, value: 7, flag: #t).value\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].direct_slots[0].type_name, "<object>");
        assert_eq!(module.classes[0].direct_slots[0].kind, ValueKind::Any);
        assert_eq!(module.classes[0].direct_slots[0].init_keyword, "value");
        assert!(!module.classes[0].direct_slots[0].required_init_keyword);
        assert_eq!(module.classes[0].direct_slots[1].kind, ValueKind::Any);
        assert_eq!(module.functions[0].result_kind, ValueKind::Any);
    }

    #[test]
    fn resolves_slot_init_keywords_and_required_keywords() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: r:;\n  slot cached, init-keyword: payload:;\nend;\n\
             define function main()\n  make(<circle>, r: 10, payload: #f).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].direct_slots[0].init_keyword, "r");
        assert!(module.classes[0].direct_slots[0].required_init_keyword);
        assert_eq!(module.classes[0].direct_slots[1].init_keyword, "payload");
        assert!(!module.classes[0].direct_slots[1].required_init_keyword);
        assert_eq!(module.functions[0].result_kind, ValueKind::I64);
    }

    #[test]
    fn resolves_slot_init_values_and_constant_metadata() {
        let module = lower_source(
            "define class <circle> (<object>)\n  constant slot radius :: <integer>, required-init-keyword: r:, init-value: 10;\n  slot cached, init-value: #t;\nend;\n\
             define function main()\n  make(<circle>).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        let radius = &module.classes[0].direct_slots[0];
        assert_eq!(radius.init_keyword, "r");
        assert!(radius.required_init_keyword);
        assert_eq!(radius.allocation, SlotAllocation::Instance);
        assert_eq!(radius.mutability, SlotMutability::Constant);
        assert!(matches!(
            radius.init_value,
            Some(Expr::ConstI64 { value: 10, .. })
        ));
        let cached = &module.classes[0].direct_slots[1];
        assert_eq!(cached.kind, ValueKind::Any);
        assert_eq!(cached.mutability, SlotMutability::Mutable);
        assert!(matches!(
            cached.init_value,
            Some(Expr::ConstBool { value: true, .. })
        ));

        let Expr::AccessorCall { receiver, .. } = &module.functions[0].body.result else {
            panic!("expected accessor call");
        };
        let Expr::MakeObject { fields, .. } = receiver.as_ref() else {
            panic!("expected defaulted make");
        };
        assert_eq!(fields.len(), 2);
        assert!(matches!(fields[0].value, Expr::ConstI64 { value: 10, .. }));
        assert!(matches!(
            fields[1].value,
            Expr::ConstBool { value: true, .. }
        ));
    }

    #[test]
    fn reports_slot_type_errors() {
        let mismatch = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  make(<flag>, valid?: 1)\nend;",
        );
        assert!(
            mismatch
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "slot `valid?` expects <boolean> but initializer has <integer>")
        );

        let unresolved = lower_source(
            "define class <bad> (<object>)\n  slot cause :: <missing>;\nend;\n\
             define function main()\n  1\nend;",
        );
        assert!(
            unresolved
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "unresolved slot type `<missing>`")
        );

        let bad_default = lower_source(
            "define class <flag> (<object>)\n  slot valid? :: <boolean>, init-value: 1;\nend;\n\
             define function main()\n  make(<flag>)\nend;",
        );
        assert!(
            bad_default
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "slot `valid?` expects <boolean> but init-value has <integer>")
        );
    }

    #[test]
    fn reports_slot_keyword_construction_errors() {
        let missing = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: r:;\nend;\n\
             define function main()\n  make(<circle>)\nend;",
        );
        assert!(
            missing
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "missing required init keyword `r:` for slot `radius`")
        );

        let unknown = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, init-keyword: r:;\nend;\n\
             define function main()\n  make(<circle>, radius: 10)\nend;",
        );
        assert!(unknown.diagnostics.iter().any(
            |diagnostic| diagnostic.message == "class `<circle>` has no init keyword `radius:`"
        ));

        let duplicate = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define function main()\n  make(<circle>, radius: 10, radius: 11)\nend;",
        );
        assert!(
            duplicate
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "duplicate initializer for keyword `radius:`")
        );
    }

    #[test]
    fn checks_booleans_comparisons_and_if_kinds() {
        let module = lower_source(
            "define function nonnegative(x)\n  x >= 0\nend;\n\
             define function main(x)\n  if nonnegative(x) then x else 0 - x end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions[0].result_kind, ValueKind::Bool);
        assert_eq!(module.functions[1].result_kind, ValueKind::I64);
        let Expr::If {
            condition,
            then_branch: _,
            else_branch: _,
            ..
        } = &module.functions[1].body.result
        else {
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
        assert!(
            invalid_condition
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "if condition must be bool")
        );

        let mismatched_branches =
            lower_source("define function main(x)\n  if x = 0 then #t else 3 end\nend;");
        assert!(
            mismatched_branches
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "if branches must have matching result kinds")
        );
    }

    #[test]
    fn resolves_signal_handle_and_restart() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle signal(make(<bad>, fallback: 42)) with restart e :: <bad> e.fallback end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions[0].result_kind, ValueKind::I64);
        let Expr::Handle { body, restart, .. } = &module.functions[0].body.result else {
            panic!("expected handle expression");
        };
        assert_eq!(restart.specializer, ClassId(0));
        assert_eq!(restart.result_kind, ValueKind::I64);
        let condition_local = restart
            .condition_local
            .expect("restart should bind condition object");
        assert_eq!(
            module.functions[0].locals[condition_local.0 as usize].kind,
            ValueKind::Object(ClassId(0))
        );
        assert!(matches!(
            restart.body.as_ref(),
            Expr::AccessorCall {
                receiver,
                class: ClassId(0),
                slot: SlotId(0),
                ..
            } if matches!(receiver.as_ref(), Expr::Local { local, .. } if *local == condition_local)
        ));
        assert!(matches!(
            body.as_ref(),
            Expr::Signal {
                condition_class: ClassId(0),
                result_kind: ValueKind::I64,
                ..
            }
        ));
    }

    #[test]
    fn restart_condition_binding_is_handler_scoped() {
        let module = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  let recovered = handle signal(make(<bad>, fallback: 0)) with restart e :: <bad> e.fallback end;\n  e.fallback\nend;",
        );

        assert!(
            module
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "unresolved local `e`")
        );
    }

    #[test]
    fn reports_invalid_signal_and_restart_uses() {
        let uncaught = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  signal(make(<bad>, fallback: 0))\nend;",
        );
        assert!(
            uncaught
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "signal requires an active restart")
        );

        let mismatched_restart = lower_source(
            "define class <bad> (<object>)\n  slot fallback :: <integer>;\nend;\n\
             define function main()\n  handle 1 with restart <bad> #f end\nend;",
        );
        assert!(
            mismatched_restart
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message
                    == "handle body and restart must have matching result kinds")
        );
    }

    #[test]
    fn reports_unresolved_names() {
        let module = lower_source("define function main()\n  missing(x)\nend;");
        assert_eq!(module.diagnostics.len(), 2);
        assert!(
            module
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved function"))
        );
        assert!(
            module
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("unresolved local"))
        );
    }
}
