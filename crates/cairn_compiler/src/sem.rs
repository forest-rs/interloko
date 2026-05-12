//! Minimal semantic IR for milestone-0 Cairn.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ast;
use crate::diagnostic::Diagnostic;
use crate::syntax::Span;

/// Durable function identifier within a semantic module.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FunctionId(
    /// Zero-based function index.
    pub u32,
);

/// Durable local identifier within a semantic function.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LocalId(
    /// Zero-based local index.
    pub u32,
);

/// Semantic module with resolved direct-call references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Semantic functions in source order.
    pub functions: Vec<Function>,
    /// Semantic diagnostics.
    pub diagnostics: Vec<Diagnostic>,
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
    /// Source span of the binding name.
    pub span: Span,
}

/// Semantic function body.
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

    fn add_local(&mut self, name: String, span: Span) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        self.locals.push(Local { id, name, span });
        id
    }

    fn find_local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .find(|local| local.name == name)
            .map(|local| local.id)
    }
}

struct Lowering {
    symbols: Vec<FunctionSymbol>,
    diagnostics: Vec<Diagnostic>,
}

impl Lowering {
    fn new(module: &ast::Module) -> Self {
        let mut diagnostics = module.diagnostics.clone();
        let mut symbols = Vec::new();

        for (index, function) in module.functions.iter().enumerate() {
            if let Some(previous) = symbols
                .iter()
                .find(|symbol: &&FunctionSymbol| symbol.name == function.name)
            {
                diagnostics.push(Diagnostic::error(
                    function.span,
                    format!(
                        "function `{}` is already defined at bytes {}..{}",
                        function.name, previous.span.start, previous.span.end
                    ),
                ));
            }
            symbols.push(FunctionSymbol {
                name: function.name.clone(),
                id: FunctionId(u32::try_from(index).unwrap_or(u32::MAX)),
                arity: function.params.len(),
                span: function.span,
            });
        }

        Self {
            symbols,
            diagnostics,
        }
    }

    fn lower_module(&mut self, module: &ast::Module) -> Module {
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
            functions,
            diagnostics: self.diagnostics.clone(),
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
            params.push(ctx.add_local(param.clone(), function.span));
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
            let id = ctx.add_local(local.name.clone(), local.span);
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
                let lowered_args = args.iter().map(|arg| self.lower_expr(ctx, arg)).collect();
                match self.resolve_function(callee) {
                    Some(symbol) if symbol.arity == args.len() => Expr::DirectCall {
                        function: symbol.id,
                        args: lowered_args,
                        span: *span,
                    },
                    Some(symbol) => {
                        ctx.diagnostics.push(Diagnostic::error(
                            *span,
                            format!(
                                "function `{callee}` expects {} arguments but got {}",
                                symbol.arity,
                                args.len()
                            ),
                        ));
                        Expr::Error { span: *span }
                    }
                    None => {
                        ctx.diagnostics.push(Diagnostic::error(
                            *span,
                            format!("unresolved function `{callee}`"),
                        ));
                        Expr::Error { span: *span }
                    }
                }
            }
            ast::Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
                op: (*op).into(),
                lhs: Box::new(self.lower_expr(ctx, lhs)),
                rhs: Box::new(self.lower_expr(ctx, rhs)),
                span: *span,
            },
            ast::Expr::Error { span } => Expr::Error { span: *span },
        }
    }

    fn resolve_function(&self, name: &str) -> Option<&FunctionSymbol> {
        self.symbols.iter().find(|symbol| symbol.name == name)
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
