//! Semantic IR for the first generic-dispatch Cairn slice.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
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
    /// Invalid or unknown value.
    Error,
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
    /// Semantic diagnostics.
    pub diagnostics: Vec<Diagnostic>,
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
    /// Declared slots.
    pub slots: Vec<Slot>,
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
    /// Getter accessor defined by this slot.
    pub accessor: Accessor,
    /// External construction keyword without the trailing colon.
    pub init_keyword: String,
    /// Whether the slot declared `required-init-keyword:`.
    pub required_init_keyword: bool,
    /// Source span.
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
    /// Arity.
    pub arity: usize,
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
            | Self::DirectCall { span, .. }
            | Self::GenericCall { span, .. }
            | Self::Signal { span, .. }
            | Self::ClassOf { span, .. }
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
    let mut lowering = Lowering::new(module);
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
    arity: usize,
    span: Span,
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
    classes: Vec<Class>,
    generics: Vec<GenericFunction>,
    methods: Vec<Method>,
    functions: Vec<FunctionSymbol>,
    generic_symbols: Vec<GenericSymbol>,
    diagnostics: Vec<Diagnostic>,
}

impl Lowering {
    fn new(module: &ast::Module) -> Self {
        let mut lowering = Self {
            source_file: module.source_file,
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
                slots: Vec::new(),
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
                        span: slot.span,
                    }
                })
                .collect::<Vec<_>>();
            self.check_duplicate_init_keywords(class, &slots);
            if let Some(lowered_class) = self.classes.get_mut(class_index) {
                lowered_class.slots = slots;
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
            self.generic_symbols.push(GenericSymbol {
                name: generic.name.clone(),
                id,
                arity: generic.params.len(),
                span: generic.span,
            });
            self.generics.push(GenericFunction {
                id,
                definition: generic.definition,
                name: generic.name.clone(),
                arity: generic.params.len(),
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
        let generic = self
            .resolve_generic(&method.name)
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
        let param = ctx.add_local(
            self.local_definition(method.definition, &method.param.name),
            method.param.name.clone(),
            ValueKind::Object(specializer),
            method.param.span,
        );
        let body = self.lower_block(&mut ctx, &method.body);
        let result_kind = self.expr_kind(&ctx, &body.result);
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
        let body_kind = self.expr_kind(&ctx, &body.result);
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
                self.expr_kind(ctx, &value),
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
                Some(local) => Expr::Local { local, span: *span },
                None => {
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        format!("unresolved local `{name}`"),
                    ));
                    Expr::Error { span: *span }
                }
            },
            ast::Expr::ClassName {
                class_name, span, ..
            } => self.lower_class_object(ctx, class_name, *span),
            ast::Expr::Call {
                callee, args, span, ..
            } => {
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
                            let actual = self.expr_kind(ctx, arg);
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
                    if symbol.arity == args.len() {
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
                            symbol.arity,
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
                let then_branch = self.lower_expr(ctx, then_branch);
                let else_branch = self.lower_expr(ctx, else_branch);
                let condition_kind = self.expr_kind(ctx, &condition);
                if !matches!(condition_kind, ValueKind::Bool | ValueKind::Error) {
                    ctx.diagnostics
                        .push(Diagnostic::error(*span, "if condition must be bool"));
                }
                let then_kind = self.expr_kind(ctx, &then_branch);
                let else_kind = self.expr_kind(ctx, &else_branch);
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

    fn lower_signal(&self, ctx: &mut FunctionContext, condition: &ast::Expr, span: Span) -> Expr {
        let condition = self.lower_expr(ctx, condition);
        let condition_class = match self.expr_kind(ctx, &condition) {
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
        if condition_class != ClassId(u32::MAX) && condition_class != active.specializer {
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
        let restart_kind = self.expr_kind(ctx, &restart_body);
        ctx.hide_locals_from(scope_start);

        ctx.push_restart(ActiveRestart {
            specializer,
            result_kind: restart_kind,
        });
        let body = self.lower_expr(ctx, body);
        ctx.pop_restart();

        let body_kind = self.expr_kind(ctx, &body);
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
        let lhs_kind = self.expr_kind(ctx, lhs);
        let rhs_kind = self.expr_kind(ctx, rhs);
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
        let class = match self.expr_kind(ctx, &object) {
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

    fn lower_make_object(
        &self,
        ctx: &mut FunctionContext,
        class_expr: &ast::Expr,
        fields: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let class_expr = self.lower_expr(ctx, class_expr);
        let class_id = match self.expr_kind(ctx, &class_expr) {
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

        let mut resolved = Vec::new();
        for field in fields {
            if resolved.iter().any(|resolved: &FieldInit| {
                class.slots[resolved.slot.0 as usize].init_keyword == field.name
            }) {
                ctx.diagnostics.push(Diagnostic::error(
                    field.span,
                    format!("duplicate initializer for keyword `{}:`", field.name),
                ));
                continue;
            }
            let Some(slot) = class
                .slots
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
            let value_kind = self.expr_kind(ctx, &value);
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

        for slot in &class.slots {
            if !resolved.iter().any(|field| field.slot == slot.id) {
                if slot.required_init_keyword {
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
        let ValueKind::Object(class_id) = self.expr_kind(ctx, &receiver) else {
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
            .slots
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

    fn expr_kind(&self, ctx: &FunctionContext, expr: &Expr) -> ValueKind {
        match expr {
            Expr::ConstI64 { .. } | Expr::GenericCall { .. } | Expr::Binary { .. } => {
                ValueKind::I64
            }
            Expr::ClassObject { class, .. } | Expr::ClassOf { class, .. } => {
                ValueKind::ClassObject(*class)
            }
            Expr::AccessorCall { result_kind, .. } => *result_kind,
            Expr::ConstBool { .. } | Expr::Compare { .. } => ValueKind::Bool,
            Expr::Local { local, .. } => ctx.local_kind(*local),
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
                let then_kind = self.expr_kind(ctx, then_branch);
                let else_kind = self.expr_kind(ctx, else_branch);
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
        self.classes
            .iter()
            .any(|class| class.slots.iter().any(|slot| slot.accessor.name == name))
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
        matches!(expected, ValueKind::Any)
            || expected == actual
            || matches!(expected, ValueKind::Error)
            || matches!(actual, ValueKind::Error)
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
            ValueKind::Error => "<error>".into(),
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
    fn resolves_classes_methods_slots_and_generic_calls() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].id, ClassId(0));
        assert_eq!(module.classes[0].slots[0].id, SlotId(0));
        assert_eq!(module.classes[0].slots[0].kind, ValueKind::I64);
        assert_eq!(
            module.classes[0].slots[0].accessor.definition,
            module.classes[0].slots[0].definition
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
        assert_eq!(*accessor, module.classes[0].slots[0].definition);
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
    fn resolves_typed_slots_and_checks_initializers() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define class <box> (<object>)\n  slot item :: <circle>;\n  slot valid? :: <boolean>;\nend;\n\
             define function main()\n  if make(<box>, item: make(<circle>, radius: 7), valid?: #t).valid? then make(<box>, item: make(<circle>, radius: 7), valid?: #t).item.radius else 0 end\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].slots[0].kind, ValueKind::I64);
        assert_eq!(
            module.classes[1].slots[0].kind,
            ValueKind::Object(ClassId(0))
        );
        assert_eq!(module.classes[1].slots[1].kind, ValueKind::Bool);
        assert_eq!(module.functions[0].result_kind, ValueKind::I64);
    }

    #[test]
    fn untyped_slots_default_to_object_top() {
        let module = lower_source(
            "define class <box> (<object>)\n  slot value;\n  slot flag;\nend;\n\
             define function main()\n  make(<box>, value: 7, flag: #t).value\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].slots[0].type_name, "<object>");
        assert_eq!(module.classes[0].slots[0].kind, ValueKind::Any);
        assert_eq!(module.classes[0].slots[0].init_keyword, "value");
        assert!(!module.classes[0].slots[0].required_init_keyword);
        assert_eq!(module.classes[0].slots[1].kind, ValueKind::Any);
        assert_eq!(module.functions[0].result_kind, ValueKind::Any);
    }

    #[test]
    fn resolves_slot_init_keywords_and_required_keywords() {
        let module = lower_source(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: r:;\n  slot cached, init-keyword: payload:;\nend;\n\
             define function main()\n  make(<circle>, r: 10, payload: #f).radius\nend;",
        );

        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].slots[0].init_keyword, "r");
        assert!(module.classes[0].slots[0].required_init_keyword);
        assert_eq!(module.classes[0].slots[1].init_keyword, "payload");
        assert!(!module.classes[0].slots[1].required_init_keyword);
        assert_eq!(module.functions[0].result_kind, ValueKind::I64);
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
