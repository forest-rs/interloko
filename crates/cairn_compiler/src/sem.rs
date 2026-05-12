//! Semantic IR for the first generic-dispatch Cairn slice.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast;
use crate::diagnostic::Diagnostic;
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
    /// Signed integer value.
    I64,
    /// Runtime object with statically known nominal class.
    Object(ClassId),
    /// Invalid or unknown value.
    Error,
}

/// Semantic module with resolved nominal IDs and calls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
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
    /// Slot name.
    pub name: String,
    /// Source span.
    pub span: Span,
}

/// Generic function declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenericFunction {
    /// Generic function identifier.
    pub id: GenericFunctionId,
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
    /// Source span.
    pub span: Span,
}

/// Semantic function body and local table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Function identifier.
    pub id: FunctionId,
    /// Function name.
    pub name: String,
    /// Parameter local IDs in source order.
    pub params: Vec<LocalId>,
    /// Local table containing parameters followed by `let` bindings.
    pub locals: Vec<Local>,
    /// Function body.
    pub body: Block,
    /// Source span.
    pub span: Span,
}

/// Semantic local binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Local {
    /// Local identifier.
    pub id: LocalId,
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
    /// Resolved local reference.
    Local {
        /// Local identifier.
        local: LocalId,
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
    /// Runtime object construction.
    MakeObject {
        /// Nominal class.
        class: ClassId,
        /// Field initializers.
        fields: Vec<FieldInit>,
        /// Source span.
        span: Span,
    },
    /// Runtime object slot read.
    SlotRead {
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Receiver class.
        class: ClassId,
        /// Slot being read.
        slot: SlotId,
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
            | Self::Local { span, .. }
            | Self::DirectCall { span, .. }
            | Self::GenericCall { span, .. }
            | Self::MakeObject { span, .. }
            | Self::SlotRead { span, .. }
            | Self::Binary { span, .. }
            | Self::Error { span } => *span,
        }
    }
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
    arity: usize,
    span: Span,
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
    diagnostics: Vec<Diagnostic>,
}

impl FunctionContext {
    fn new() -> Self {
        Self {
            locals: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn add_local(&mut self, name: String, kind: ValueKind, span: Span) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        self.locals.push(Local {
            id,
            name,
            kind,
            span,
        });
        id
    }

    fn find_local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .find(|local| local.name == name)
            .map(|local| local.id)
    }

    fn local_kind(&self, id: LocalId) -> ValueKind {
        self.locals
            .get(id.0 as usize)
            .map_or(ValueKind::Error, |local| local.kind)
    }
}

struct Lowering {
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
            let slots = class
                .slots
                .iter()
                .enumerate()
                .map(|(slot_index, slot)| Slot {
                    id: SlotId(u32::try_from(slot_index).unwrap_or(u32::MAX)),
                    name: slot.name.clone(),
                    span: slot.span,
                })
                .collect();
            self.classes.push(Class {
                id: ClassId(u32::try_from(index).unwrap_or(u32::MAX)),
                name: class.name.clone(),
                slots,
                span: class.span,
            });
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
            self.functions.push(FunctionSymbol {
                name: function.name.clone(),
                id: FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                arity: function.params.len(),
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
            method.param.name.clone(),
            ValueKind::Object(specializer),
            method.param.span,
        );
        let body = self.lower_block(&mut ctx, &method.body);
        self.diagnostics.extend(ctx.diagnostics);
        Method {
            id,
            generic,
            specializer,
            param,
            locals: ctx.locals,
            body,
            span: method.span,
        }
    }

    fn lower_function(&mut self, id: FunctionId, function: &ast::Function) -> Function {
        let mut ctx = FunctionContext::new();
        let mut params = Vec::new();

        for param in &function.params {
            if ctx.find_local(param).is_some() {
                ctx.diagnostics.push(Diagnostic::error(
                    function.span,
                    format!("parameter `{param}` is already defined"),
                ));
            }
            params.push(ctx.add_local(param.clone(), ValueKind::I64, function.span));
        }

        let body = self.lower_block(&mut ctx, &function.body);
        self.diagnostics.extend(ctx.diagnostics);

        Function {
            id,
            name: function.name.clone(),
            params,
            locals: ctx.locals,
            body,
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
            let id = ctx.add_local(local.name.clone(), self.expr_kind(ctx, &value), local.span);
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
            ast::Expr::Number { value, span } => Expr::ConstI64 {
                value: *value,
                span: *span,
            },
            ast::Expr::Name { name, span } => match ctx.find_local(name) {
                Some(local) => Expr::Local { local, span: *span },
                None => {
                    ctx.diagnostics.push(Diagnostic::error(
                        *span,
                        format!("unresolved local `{name}`"),
                    ));
                    Expr::Error { span: *span }
                }
            },
            ast::Expr::Call { callee, args, span } => {
                let lowered_args: Vec<_> =
                    args.iter().map(|arg| self.lower_expr(ctx, arg)).collect();
                if let Some(symbol) = self.resolve_function(callee) {
                    if symbol.arity == args.len() {
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
                            symbol.arity,
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
                ctx.diagnostics.push(Diagnostic::error(
                    *span,
                    format!("unresolved function or generic `{callee}`"),
                ));
                Expr::Error { span: *span }
            }
            ast::Expr::MakeObject {
                class_name,
                fields,
                span,
            } => self.lower_make_object(ctx, class_name, fields, *span),
            ast::Expr::SlotRead {
                receiver,
                slot,
                span,
            } => self.lower_slot_read(ctx, receiver, slot, *span),
            ast::Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
                op: (*op).into(),
                lhs: Box::new(self.lower_expr(ctx, lhs)),
                rhs: Box::new(self.lower_expr(ctx, rhs)),
                span: *span,
            },
            ast::Expr::Error { span } => Expr::Error { span: *span },
        }
    }

    fn lower_make_object(
        &self,
        ctx: &mut FunctionContext,
        class_name: &str,
        fields: &[ast::KeywordArg],
        span: Span,
    ) -> Expr {
        let Some(class) = self.resolve_class(class_name) else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!("unresolved class `{class_name}`"),
            ));
            return Expr::Error { span };
        };

        let mut resolved = Vec::new();
        for field in fields {
            let Some(slot) = class.slots.iter().find(|slot| slot.name == field.name) else {
                ctx.diagnostics.push(Diagnostic::error(
                    field.span,
                    format!("class `{}` has no slot `{}`", class.name, field.name),
                ));
                continue;
            };
            resolved.push(FieldInit {
                slot: slot.id,
                value: self.lower_expr(ctx, &field.value),
                span: field.span,
            });
        }

        for slot in &class.slots {
            if !resolved.iter().any(|field| field.slot == slot.id) {
                ctx.diagnostics.push(Diagnostic::error(
                    span,
                    format!("missing initializer for slot `{}`", slot.name),
                ));
            }
        }

        Expr::MakeObject {
            class: class.id,
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
        let ValueKind::Object(class_id) = self.expr_kind(ctx, &receiver) else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                "slot read receiver does not have a known object class",
            ));
            return Expr::Error { span };
        };
        let Some(class) = self.classes.get(class_id.0 as usize) else {
            return Expr::Error { span };
        };
        let Some(slot) = class.slots.iter().find(|slot| slot.name == slot_name) else {
            ctx.diagnostics.push(Diagnostic::error(
                span,
                format!("class `{}` has no slot `{slot_name}`", class.name),
            ));
            return Expr::Error { span };
        };
        Expr::SlotRead {
            receiver: Box::new(receiver),
            class: class_id,
            slot: slot.id,
            span,
        }
    }

    fn expr_kind(&self, ctx: &FunctionContext, expr: &Expr) -> ValueKind {
        match expr {
            Expr::ConstI64 { .. }
            | Expr::DirectCall { .. }
            | Expr::GenericCall { .. }
            | Expr::SlotRead { .. }
            | Expr::Binary { .. } => ValueKind::I64,
            Expr::Local { local, .. } => ctx.local_kind(*local),
            Expr::MakeObject { class, .. } => ValueKind::Object(*class),
            Expr::Error { .. } => ValueKind::Error,
        }
    }

    fn resolve_class(&self, name: &str) -> Option<&Class> {
        self.classes.iter().find(|class| class.name == name)
    }

    fn resolve_function(&self, name: &str) -> Option<&FunctionSymbol> {
        self.functions.iter().find(|symbol| symbol.name == name)
    }

    fn resolve_generic(&self, name: &str) -> Option<&GenericSymbol> {
        self.generic_symbols
            .iter()
            .find(|symbol| symbol.name == name)
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
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].id, ClassId(0));
        assert_eq!(module.classes[0].slots[0].id, SlotId(0));
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
                class: ClassId(0),
                ..
            }
        ));
        let Expr::Binary { lhs, .. } = &module.methods[0].body.result else {
            panic!("expected method body binary expression");
        };
        assert!(matches!(
            lhs.as_ref(),
            Expr::SlotRead {
                class: ClassId(0),
                slot: SlotId(0),
                ..
            }
        ));
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
