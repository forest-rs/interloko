//! Surface AST lowered from the milestone-0 CST.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::identity::{AstNodeId, DefinitionId, DefinitionKind, SourceFileId};
use crate::syntax::{CstElement, CstKind, CstNode, Span, SyntaxTree, TokenKind};

/// AST module containing function definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Source file identity.
    pub source_file: SourceFileId,
    /// Class definitions in source order.
    pub classes: Vec<Class>,
    /// Generic function declarations in source order.
    pub generics: Vec<Generic>,
    /// Method definitions in source order.
    pub methods: Vec<Method>,
    /// Function definitions in source order.
    pub functions: Vec<Function>,
    /// AST lowering diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Function definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Function name.
    pub name: String,
    /// Parameter names in source order.
    pub params: Vec<String>,
    /// Function body.
    pub body: Block,
    /// Source span.
    pub span: Span,
}

/// Class definition with nominal slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Class {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Class name, including Dylan-style angle brackets.
    pub name: String,
    /// Slot declarations in source order.
    pub slots: Vec<Slot>,
    /// Source span.
    pub span: Span,
}

/// Slot declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slot {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Slot name.
    pub name: String,
    /// Optional declared slot type name.
    ///
    /// Missing types default to Dylan's top `<object>` value category during
    /// semantic lowering.
    pub type_name: Option<String>,
    /// Source span for the optional slot type, or the slot name span when the
    /// declaration is untyped.
    pub type_span: Span,
    /// Optional external construction keyword without the trailing colon.
    ///
    /// Missing keywords default to the slot name.
    pub init_keyword: Option<String>,
    /// Source span for the optional construction keyword, or the slot name span
    /// when the declaration uses the default keyword.
    pub init_keyword_span: Span,
    /// Whether the construction keyword was declared with
    /// `required-init-keyword:`.
    pub required_init_keyword: bool,
    /// Source span.
    pub span: Span,
}

/// Generic function declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Generic {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Generic function name.
    pub name: String,
    /// Parameter names.
    pub params: Vec<String>,
    /// Source span.
    pub span: Span,
}

/// Method definition specialized on a nominal class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Method {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity.
    pub definition: DefinitionId,
    /// Generic function name this method implements.
    pub name: String,
    /// Single specialized parameter for this slice.
    pub param: MethodParam,
    /// Method body.
    pub body: Block,
    /// Source span.
    pub span: Span,
}

/// Method parameter with a nominal specializer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodParam {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Parameter name.
    pub name: String,
    /// Nominal class specializer.
    pub class_name: String,
    /// Source span.
    pub span: Span,
}

/// Function body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Local `let` bindings.
    pub lets: Vec<Let>,
    /// Final result expression.
    pub result: Expr,
    /// Source span.
    pub span: Span,
}

/// Local binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Let {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Stable definition identity for the local binding.
    pub definition: DefinitionId,
    /// Local name.
    pub name: String,
    /// Bound value expression.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Binary operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
}

/// Integer comparison operator.
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

/// Surface expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    /// Integer literal.
    Number {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Parsed value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Boolean literal.
    Bool {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Parsed value.
        value: bool,
        /// Source span.
        span: Span,
    },
    /// Name reference.
    Name {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Referenced name.
        name: String,
        /// Source span.
        span: Span,
    },
    /// Direct call syntax.
    Call {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Callee name.
        callee: String,
        /// Argument expressions.
        args: Vec<Expr>,
        /// Source span.
        span: Span,
    },
    /// Recoverable condition signal.
    Signal {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Condition object expression.
        condition: Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Object construction through `make(<class>, slot: value)`.
    MakeObject {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Class name.
        class_name: String,
        /// Keyword slot initializers.
        fields: Vec<KeywordArg>,
        /// Source span.
        span: Span,
    },
    /// Object slot read.
    SlotRead {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Slot name.
        slot: String,
        /// Source span.
        span: Span,
    },
    /// Binary expression.
    Binary {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        lhs: alloc::boxed::Box<Expr>,
        /// Right operand.
        rhs: alloc::boxed::Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Integer comparison expression.
    Compare {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Comparison operator.
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
        /// Stable-ish AST node identity.
        id: AstNodeId,
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
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Handled body expression.
        body: Box<Expr>,
        /// Restart clause.
        restart: Restart,
        /// Source span.
        span: Span,
    },
    /// Placeholder for malformed syntax.
    Error {
        /// Stable-ish AST node identity.
        id: AstNodeId,
        /// Source span.
        span: Span,
    },
}

impl Expr {
    /// Returns this expression's stable-ish AST node id.
    #[must_use]
    pub const fn id(&self) -> AstNodeId {
        match self {
            Self::Number { id, .. }
            | Self::Bool { id, .. }
            | Self::Name { id, .. }
            | Self::Call { id, .. }
            | Self::Signal { id, .. }
            | Self::MakeObject { id, .. }
            | Self::SlotRead { id, .. }
            | Self::Binary { id, .. }
            | Self::Compare { id, .. }
            | Self::If { id, .. }
            | Self::Handle { id, .. }
            | Self::Error { id, .. } => *id,
        }
    }

    /// Returns this expression's source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Number { span, .. }
            | Self::Bool { span, .. }
            | Self::Name { span, .. }
            | Self::Call { span, .. }
            | Self::Signal { span, .. }
            | Self::MakeObject { span, .. }
            | Self::SlotRead { span, .. }
            | Self::Binary { span, .. }
            | Self::Compare { span, .. }
            | Self::If { span, .. }
            | Self::Handle { span, .. }
            | Self::Error { span, .. } => *span,
        }
    }
}

/// Restart clause for a lexical condition handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Restart {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Optional handler-local name bound to the signaled condition object.
    pub condition_name: Option<String>,
    /// Source span for the optional condition binding name.
    pub condition_span: Span,
    /// Nominal condition class handled by this restart.
    pub class_name: String,
    /// Handler expression evaluated when a matching condition is signaled.
    pub body: Box<Expr>,
    /// Source span.
    pub span: Span,
}

/// Keyword argument used by object construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeywordArg {
    /// Stable-ish AST node identity.
    pub id: AstNodeId,
    /// Keyword name.
    pub name: String,
    /// Argument value.
    pub value: Expr,
    /// Source span.
    pub span: Span,
}

/// Lowers a syntax tree to a surface AST.
#[must_use]
pub fn lower(tree: &SyntaxTree) -> Module {
    let mut lowering = Lowering {
        tree,
        diagnostics: tree.diagnostics.clone(),
    };
    lowering.lower_module()
}

struct Lowering<'tree> {
    tree: &'tree SyntaxTree,
    diagnostics: Vec<Diagnostic>,
}

impl<'tree> Lowering<'tree> {
    fn lower_module(&mut self) -> Module {
        let classes = self
            .tree
            .root
            .child_nodes(CstKind::Class)
            .map(|node| self.lower_class(node))
            .collect();
        let generics = self
            .tree
            .root
            .child_nodes(CstKind::Generic)
            .map(|node| self.lower_generic(node))
            .collect();
        let methods = self
            .tree
            .root
            .child_nodes(CstKind::Method)
            .map(|node| self.lower_method(node))
            .collect();
        let functions = self
            .tree
            .root
            .child_nodes(CstKind::Function)
            .map(|node| self.lower_function(node))
            .collect();
        Module {
            source_file: self.tree.source_file,
            classes,
            generics,
            methods,
            functions,
            diagnostics: self.diagnostics.clone(),
        }
    }

    fn lower_class(&mut self, node: &CstNode) -> Class {
        let name = self
            .first_token_text(node, TokenKind::TypeName)
            .map_or_else(String::new, |(_, text)| text.to_string());
        let mut slots = Vec::new();
        for slot in node.child_nodes(CstKind::SlotDecl) {
            slots.push(self.lower_slot(&name, slot));
        }
        Class {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Class, &name, ""),
            name,
            slots,
            span: node.span,
        }
    }

    fn lower_slot(&mut self, class_name: &str, node: &CstNode) -> Slot {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Slot {
                id: self.ast_id(node),
                definition: self.definition(DefinitionKind::Slot, "", class_name),
                name: String::new(),
                type_name: None,
                type_span: node.span,
                init_keyword: None,
                init_keyword_span: node.span,
                required_init_keyword: false,
                span: node.span,
            };
        };
        let name = text.to_string();
        let slot_type = self.first_token_text(node, TokenKind::TypeName);
        let mut init_keyword = None;
        let mut init_keyword_span = span;
        let mut required_init_keyword = false;
        for option in node.child_nodes(CstKind::SlotOption) {
            let idents = self.ident_tokens(option);
            let Some((option_span, option_name)) = idents.first() else {
                continue;
            };
            let Some((keyword_span, keyword_name)) = idents.get(1) else {
                self.diagnostics.push(Diagnostic::error(
                    *option_span,
                    format!("slot option `{option_name}:` requires a keyword value"),
                ));
                continue;
            };
            match *option_name {
                "init-keyword" => {
                    init_keyword = Some((*keyword_name).to_string());
                    init_keyword_span = *keyword_span;
                }
                "required-init-keyword" => {
                    init_keyword = Some((*keyword_name).to_string());
                    init_keyword_span = *keyword_span;
                    required_init_keyword = true;
                }
                _ => self.diagnostics.push(Diagnostic::error(
                    *option_span,
                    format!("unsupported slot option `{option_name}:`"),
                )),
            }
        }
        Slot {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Slot, &name, class_name),
            name,
            type_name: slot_type.map(|(_, text)| text.to_string()),
            type_span: slot_type.map_or(span, |(span, _)| span),
            init_keyword,
            init_keyword_span,
            required_init_keyword,
            span,
        }
    }

    fn ident_tokens<'a>(&'a self, node: &'a CstNode) -> Vec<(Span, &'a str)> {
        node.children
            .iter()
            .filter_map(|child| match child {
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Ident => {
                    let token = self.tree.tokens[*index];
                    Some((token.span, token.text(&self.tree.source)))
                }
                _ => None,
            })
            .collect()
    }

    fn lower_generic(&mut self, node: &CstNode) -> Generic {
        let name = self.definition_name_after(node, TokenKind::Generic);
        Generic {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Generic, &name, ""),
            name,
            params: node
                .child_nodes(CstKind::ParameterList)
                .next()
                .map_or_else(Vec::new, |params| self.lower_params(params)),
            span: node.span,
        }
    }

    fn lower_method(&mut self, node: &CstNode) -> Method {
        let params = node
            .child_nodes(CstKind::ParameterList)
            .next()
            .map_or_else(Vec::new, |params| self.lower_method_params(params));
        if params.len() != 1 {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                "milestone generic dispatch methods require exactly one specialized parameter",
            ));
        }
        let body = node
            .child_nodes(CstKind::Body)
            .next()
            .map(|body| self.lower_block(body))
            .unwrap_or_else(|| Block {
                id: self.ast_id(node),
                lets: Vec::new(),
                result: Expr::Error {
                    id: self.ast_id(node),
                    span: node.span,
                },
                span: node.span,
            });
        let name = self.definition_name_after(node, TokenKind::Method);
        let param = params.into_iter().next().unwrap_or(MethodParam {
            id: AstNodeId::child(node.id, 1, ""),
            name: String::new(),
            class_name: String::new(),
            span: node.span,
        });
        let definition = self.definition(DefinitionKind::Method, &name, &param.class_name);
        Method {
            id: self.ast_id(node),
            definition,
            name,
            param,
            body,
            span: node.span,
        }
    }

    fn lower_function(&mut self, node: &CstNode) -> Function {
        let mut name = String::new();
        let mut saw_function_keyword = false;
        let mut params = Vec::new();
        let mut body = None;

        for child in &node.children {
            match child {
                CstElement::Token(index)
                    if self.tree.tokens[*index].kind == TokenKind::Function =>
                {
                    saw_function_keyword = true;
                }
                CstElement::Token(index)
                    if saw_function_keyword
                        && self.tree.tokens[*index].kind == TokenKind::Ident
                        && name.is_empty() =>
                {
                    name = self.tree.tokens[*index].text(&self.tree.source).to_string();
                }
                CstElement::Token(_) => {}
                CstElement::Node(child_node) if child_node.kind == CstKind::ParameterList => {
                    params = self.lower_params(child_node);
                }
                CstElement::Node(child_node) if child_node.kind == CstKind::Body => {
                    body = Some(self.lower_block(child_node));
                }
                _ => {}
            }
        }

        let body = body.unwrap_or_else(|| Block {
            id: self.ast_id(node),
            lets: Vec::new(),
            result: Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
            span: node.span,
        });

        Function {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Function, &name, ""),
            name,
            params,
            body,
            span: node.span,
        }
    }

    fn lower_params(&self, node: &CstNode) -> Vec<String> {
        node.children
            .iter()
            .filter_map(|child| match child {
                CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Ident => {
                    Some(self.tree.tokens[*index].text(&self.tree.source).to_string())
                }
                _ => None,
            })
            .collect()
    }

    fn lower_method_params(&self, node: &CstNode) -> Vec<MethodParam> {
        let mut params = Vec::new();
        let mut index = 0;
        while index < node.children.len() {
            let CstElement::Token(name_index) = node.children[index] else {
                index += 1;
                continue;
            };
            if self.tree.tokens[name_index].kind != TokenKind::Ident {
                index += 1;
                continue;
            }
            let name = self.tree.tokens[name_index]
                .text(&self.tree.source)
                .to_string();
            let span = self.tree.tokens[name_index].span;
            let class_name = node.children[index + 1..]
                .iter()
                .find_map(|child| match child {
                    CstElement::Token(type_index)
                        if self.tree.tokens[*type_index].kind == TokenKind::TypeName =>
                    {
                        Some(
                            self.tree.tokens[*type_index]
                                .text(&self.tree.source)
                                .to_string(),
                        )
                    }
                    CstElement::Token(comma_index)
                        if self.tree.tokens[*comma_index].kind == TokenKind::Comma =>
                    {
                        None
                    }
                    _ => None,
                })
                .unwrap_or_default();
            params.push(MethodParam {
                id: AstNodeId::child(node.id, 2, &name),
                name,
                class_name,
                span,
            });
            index += 1;
        }
        params
    }

    fn lower_block(&mut self, node: &CstNode) -> Block {
        let mut lets = Vec::new();
        let mut result = None;

        for child in &node.children {
            let CstElement::Node(child_node) = child else {
                continue;
            };
            if child_node.kind == CstKind::LetStmt {
                lets.push(self.lower_let(child_node));
            } else {
                result = Some(self.lower_expr(child_node));
            }
        }

        Block {
            id: self.ast_id(node),
            lets,
            result: result.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            }),
            span: node.span,
        }
    }

    fn lower_let(&mut self, node: &CstNode) -> Let {
        let mut name = String::new();
        let mut value = None;

        for child in &node.children {
            match child {
                CstElement::Token(index)
                    if self.tree.tokens[*index].kind == TokenKind::Ident && name.is_empty() =>
                {
                    name = self.tree.tokens[*index].text(&self.tree.source).to_string();
                }
                CstElement::Node(child_node) => value = Some(self.lower_expr(child_node)),
                _ => {}
            }
        }

        Let {
            id: self.ast_id(node),
            definition: self.definition(DefinitionKind::Local, &name, ""),
            name,
            value: value.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            }),
            span: node.span,
        }
    }

    fn lower_expr(&mut self, node: &CstNode) -> Expr {
        match node.kind {
            CstKind::NumberExpr => self.lower_number(node),
            CstKind::BoolExpr => self.lower_bool(node),
            CstKind::NameExpr => self.lower_name(node),
            CstKind::CallExpr => self.lower_call(node),
            CstKind::SlotExpr => self.lower_slot_read(node),
            CstKind::BinaryExpr => self.lower_binary(node),
            CstKind::IfExpr => self.lower_if(node),
            CstKind::HandleExpr => self.lower_handle(node),
            _ => Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
        }
    }

    fn lower_bool(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Bool) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Bool {
            id: self.ast_id(node),
            value: text == "#t",
            span,
        }
    }

    fn lower_number(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Number) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        match text.parse::<i64>() {
            Ok(value) => Expr::Number {
                id: self.ast_id(node),
                value,
                span,
            },
            Err(_) => {
                self.diagnostics
                    .push(Diagnostic::error(span, "integer literal is out of range"));
                Expr::Error {
                    id: self.ast_id(node),
                    span,
                }
            }
        }
    }

    fn lower_name(&self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        Expr::Name {
            id: self.ast_id(node),
            name: text.to_string(),
            span,
        }
    }

    fn lower_call(&mut self, node: &CstNode) -> Expr {
        let Some((name_span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        if text == "make" {
            return self.lower_make_object(node);
        }
        if text == "signal" {
            return self.lower_signal(node);
        }
        let mut args = Vec::new();
        for arg_list in node.child_nodes(CstKind::ArgList) {
            for child in &arg_list.children {
                if let CstElement::Node(expr) = child {
                    if matches!(expr.kind, CstKind::KeywordArg | CstKind::TypeExpr) {
                        continue;
                    }
                    args.push(self.lower_expr(expr));
                }
            }
        }
        Expr::Call {
            id: self.ast_id(node),
            callee: text.to_string(),
            args,
            span: node.span.cover(name_span),
        }
    }

    fn lower_signal(&mut self, node: &CstNode) -> Expr {
        let args = self.lower_regular_args(node);
        if args.len() != 1 {
            self.diagnostics.push(Diagnostic::error(
                node.span,
                "signal expects exactly one condition",
            ));
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        }
        Expr::Signal {
            id: self.ast_id(node),
            condition: Box::new(args.into_iter().next().expect("arity was checked")),
            span: node.span,
        }
    }

    fn lower_regular_args(&mut self, node: &CstNode) -> Vec<Expr> {
        let mut args = Vec::new();
        for arg_list in node.child_nodes(CstKind::ArgList) {
            for child in &arg_list.children {
                if let CstElement::Node(expr) = child {
                    if matches!(expr.kind, CstKind::KeywordArg | CstKind::TypeExpr) {
                        continue;
                    }
                    args.push(self.lower_expr(expr));
                }
            }
        }
        args
    }

    fn lower_make_object(&mut self, node: &CstNode) -> Expr {
        let Some(arg_list) = node.child_nodes(CstKind::ArgList).next() else {
            return Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            };
        };
        let class_name = arg_list
            .child_nodes(CstKind::TypeExpr)
            .next()
            .and_then(|type_expr| self.first_token_text(type_expr, TokenKind::TypeName))
            .map_or_else(String::new, |(_, text)| text.to_string());
        let fields = arg_list
            .child_nodes(CstKind::KeywordArg)
            .map(|keyword| self.lower_keyword_arg(keyword))
            .collect();
        Expr::MakeObject {
            id: self.ast_id(node),
            class_name,
            fields,
            span: node.span,
        }
    }

    fn lower_keyword_arg(&mut self, node: &CstNode) -> KeywordArg {
        let Some((name_span, name)) = self.first_token_text(node, TokenKind::Ident) else {
            return KeywordArg {
                id: self.ast_id(node),
                name: String::new(),
                value: Expr::Error {
                    id: self.ast_id(node),
                    span: node.span,
                },
                span: node.span,
            };
        };
        let value = node
            .children
            .iter()
            .find_map(|child| match child {
                CstElement::Node(expr) => Some(self.lower_expr(expr)),
                _ => None,
            })
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        KeywordArg {
            id: self.ast_id(node),
            name: name.to_string(),
            value,
            span: node.span.cover(name_span),
        }
    }

    fn lower_slot_read(&mut self, node: &CstNode) -> Expr {
        let receiver = node.children.iter().find_map(|child| match child {
            CstElement::Node(expr) => Some(self.lower_expr(expr)),
            _ => None,
        });
        let slot = node.children.iter().rev().find_map(|child| match child {
            CstElement::Token(index) if self.tree.tokens[*index].kind == TokenKind::Ident => {
                Some(self.tree.tokens[*index].text(&self.tree.source).to_string())
            }
            _ => None,
        });
        Expr::SlotRead {
            id: self.ast_id(node),
            receiver: Box::new(receiver.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            })),
            slot: slot.unwrap_or_default(),
            span: node.span,
        }
    }

    fn lower_binary(&mut self, node: &CstNode) -> Expr {
        let mut exprs = Vec::new();
        for child in &node.children {
            if let CstElement::Node(expr) = child {
                exprs.push(self.lower_expr(expr));
            }
        }
        let mut exprs = exprs.into_iter();
        let fallback = || Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        };
        let lhs = exprs.next().unwrap_or_else(fallback);
        let rhs = exprs.next().unwrap_or_else(fallback);
        let op = node.children.iter().find_map(|child| match child {
            CstElement::Token(index) => Some(self.tree.tokens[*index].kind),
            _ => None,
        });

        match op {
            Some(TokenKind::Plus) => Expr::Binary {
                id: self.ast_id(node),
                op: BinaryOp::Add,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Minus) => Expr::Binary {
                id: self.ast_id(node),
                op: BinaryOp::Sub,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Star) => Expr::Binary {
                id: self.ast_id(node),
                op: BinaryOp::Mul,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Equal) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Eq,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Less) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Lt,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::LessEqual) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Le,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::Greater) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Gt,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            Some(TokenKind::GreaterEqual) => Expr::Compare {
                id: self.ast_id(node),
                op: CompareOp::Ge,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span: node.span,
            },
            _ => Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            },
        }
    }

    fn lower_if(&mut self, node: &CstNode) -> Expr {
        let mut exprs = Vec::new();
        for child in &node.children {
            if let CstElement::Node(expr) = child {
                exprs.push(self.lower_expr(expr));
            }
        }
        let mut exprs = exprs.into_iter();
        let condition = exprs.next().unwrap_or_else(|| Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        });
        let then_branch = exprs.next().unwrap_or_else(|| Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        });
        let else_branch = exprs.next().unwrap_or_else(|| Expr::Error {
            id: self.ast_id(node),
            span: node.span,
        });
        Expr::If {
            id: self.ast_id(node),
            condition: Box::new(condition),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
            span: node.span,
        }
    }

    fn lower_handle(&mut self, node: &CstNode) -> Expr {
        let mut body = None;
        let mut restart = None;
        for child in &node.children {
            let CstElement::Node(child_node) = child else {
                continue;
            };
            if child_node.kind == CstKind::RestartClause {
                restart = Some(self.lower_restart(child_node));
            } else {
                body = Some(self.lower_expr(child_node));
            }
        }
        Expr::Handle {
            id: self.ast_id(node),
            body: Box::new(body.unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            })),
            restart: restart.unwrap_or_else(|| Restart {
                id: self.ast_id(node),
                condition_name: None,
                condition_span: node.span,
                class_name: String::new(),
                body: Box::new(Expr::Error {
                    id: self.ast_id(node),
                    span: node.span,
                }),
                span: node.span,
            }),
            span: node.span,
        }
    }

    fn lower_restart(&mut self, node: &CstNode) -> Restart {
        let condition = self.first_restart_condition_name(node);
        let class_name = self
            .first_token_text(node, TokenKind::TypeName)
            .map_or_else(String::new, |(_, text)| text.to_string());
        let body = node
            .children
            .iter()
            .find_map(|child| match child {
                CstElement::Node(expr) => Some(self.lower_expr(expr)),
                _ => None,
            })
            .unwrap_or(Expr::Error {
                id: self.ast_id(node),
                span: node.span,
            });
        Restart {
            id: self.ast_id(node),
            condition_name: condition.map(|(_, text)| text.to_string()),
            condition_span: condition.map_or(node.span, |(span, _)| span),
            class_name,
            body: Box::new(body),
            span: node.span,
        }
    }

    fn ast_id(&self, node: &CstNode) -> AstNodeId {
        AstNodeId::from_syntax(node.id)
    }

    fn definition(&self, kind: DefinitionKind, name: &str, discriminator: &str) -> DefinitionId {
        DefinitionId::new(self.tree.source_file, kind, name, discriminator)
    }

    fn first_token_text(&self, node: &CstNode, kind: TokenKind) -> Option<(Span, &'tree str)> {
        node.children.iter().find_map(|child| match child {
            CstElement::Token(index) if self.tree.tokens[*index].kind == kind => {
                let token = self.tree.tokens[*index];
                Some((token.span, token.text(&self.tree.source)))
            }
            _ => None,
        })
    }

    fn first_restart_condition_name(&self, node: &CstNode) -> Option<(Span, &'tree str)> {
        let mut tokens = node.children.iter().filter_map(|child| match child {
            CstElement::Token(index) => Some(*index),
            CstElement::Node(_) => None,
        });
        while let Some(index) = tokens.next() {
            let token = self.tree.tokens[index];
            if token.kind != TokenKind::Restart {
                continue;
            }
            let name_index = tokens.next()?;
            let name = self.tree.tokens[name_index];
            if name.kind != TokenKind::Ident {
                return None;
            }
            let double_colon_index = tokens.next()?;
            if self.tree.tokens[double_colon_index].kind != TokenKind::DoubleColon {
                return None;
            }
            return Some((name.span, name.text(&self.tree.source)));
        }
        None
    }

    fn definition_name_after(&self, node: &CstNode, keyword: TokenKind) -> String {
        let mut saw_keyword = false;
        for child in &node.children {
            let CstElement::Token(index) = child else {
                continue;
            };
            let token = self.tree.tokens[*index];
            if token.kind == keyword {
                saw_keyword = true;
            } else if saw_keyword && token.kind == TokenKind::Ident {
                return token.text(&self.tree.source).to_string();
            }
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SourceFileId;
    use crate::syntax;

    #[test]
    fn lowers_function_params_lets_and_binary_result() {
        let tree = syntax::parse("define function add-one(x)\n  let one = 1;\n  x + one\nend;");
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions.len(), 1);
        assert_eq!(module.functions[0].name, "add-one");
        assert_eq!(module.functions[0].params, ["x"]);
        assert_eq!(module.functions[0].body.lets[0].name, "one");
        assert!(matches!(
            module.functions[0].body.result,
            Expr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
    }

    #[test]
    fn lowers_call_arguments() {
        let tree = syntax::parse("define function main()\n  add(40, 2)\nend;");
        let module = lower(&tree);
        let Expr::Call { callee, args, .. } = &module.functions[0].body.result else {
            panic!("expected call");
        };
        assert_eq!(callee, "add");
        assert_eq!(args.len(), 2);
    }

    #[test]
    fn lowers_classes_generics_methods_make_and_slot_reads() {
        let tree = syntax::parse(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: radius:;\n  slot valid? :: <boolean>, init-keyword: enabled?:;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10, enabled?: #t))\nend;",
        );
        let module = lower(&tree);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes[0].name, "<circle>");
        assert_eq!(module.classes[0].slots[0].name, "radius");
        assert_eq!(
            module.classes[0].slots[0].type_name.as_deref(),
            Some("<integer>")
        );
        assert_eq!(
            module.classes[0].slots[0].init_keyword.as_deref(),
            Some("radius")
        );
        assert!(module.classes[0].slots[0].required_init_keyword);
        assert_eq!(module.classes[0].slots[1].name, "valid?");
        assert_eq!(
            module.classes[0].slots[1].type_name.as_deref(),
            Some("<boolean>")
        );
        assert_eq!(
            module.classes[0].slots[1].init_keyword.as_deref(),
            Some("enabled?")
        );
        assert!(!module.classes[0].slots[1].required_init_keyword);
        assert_eq!(module.generics[0].name, "area");
        assert_eq!(module.methods[0].name, "area");
        assert_eq!(module.methods[0].param.class_name, "<circle>");
        assert!(matches!(
            module.methods[0].body.result,
            Expr::Binary { lhs: _, rhs: _, .. }
        ));
        let Expr::Call { args, .. } = &module.functions[0].body.result else {
            panic!("expected area call");
        };
        assert!(matches!(args[0], Expr::MakeObject { .. }));
    }

    #[test]
    fn lowers_booleans_comparisons_and_if() {
        let tree =
            syntax::parse("define function main(x)\n  if x < 0 then #f else x >= 10 end\nend;");
        let module = lower(&tree);

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected if expression");
        };
        assert!(matches!(
            condition.as_ref(),
            Expr::Compare {
                op: CompareOp::Lt,
                ..
            }
        ));
        assert!(matches!(
            then_branch.as_ref(),
            Expr::Bool { value: false, .. }
        ));
        assert!(matches!(
            else_branch.as_ref(),
            Expr::Compare {
                op: CompareOp::Ge,
                ..
            }
        ));
    }

    #[test]
    fn lowers_signal_handle_and_restart() {
        let tree = syntax::parse(
            "define function main()\n  handle signal(make(<bad>, fallback: 0)) with restart e :: <bad> e.fallback end\nend;",
        );
        let module = lower(&tree);

        assert_eq!(module.diagnostics, Vec::new());
        let Expr::Handle {
            body,
            restart,
            span: _,
            ..
        } = &module.functions[0].body.result
        else {
            panic!("expected handle expression");
        };
        assert_eq!(restart.class_name, "<bad>");
        assert_eq!(restart.condition_name.as_deref(), Some("e"));
        assert!(matches!(
            restart.body.as_ref(),
            Expr::SlotRead { receiver, slot, .. }
                if slot == "fallback" && matches!(receiver.as_ref(), Expr::Name { name, .. } if name == "e")
        ));
        assert!(matches!(
            body.as_ref(),
            Expr::Signal { condition, .. }
                if matches!(condition.as_ref(), Expr::MakeObject { class_name, .. } if class_name == "<bad>")
        ));
    }

    #[test]
    fn syntax_and_ast_definition_ids_survive_whitespace_edits() {
        let source_file = SourceFileId::from_path("examples/main.cairn");
        let before_tree = syntax::parse_file(source_file, "define function main()\n  40 + 2\nend;");
        let edited_tree = syntax::parse_file(
            source_file,
            "\n// comment before the same definition\n\
             define   function   main(  )\n  40   +   2\nend;",
        );
        let before_syntax = before_tree
            .root
            .child_nodes(syntax::CstKind::Function)
            .next()
            .expect("function syntax should exist")
            .id;
        let edited_syntax = edited_tree
            .root
            .child_nodes(syntax::CstKind::Function)
            .next()
            .expect("edited function syntax should exist")
            .id;
        assert_eq!(before_syntax, edited_syntax);

        let before = lower(&before_tree);
        let edited = lower(&edited_tree);
        assert_eq!(before.source_file, source_file);
        assert_eq!(before.functions[0].id, edited.functions[0].id);
        assert_eq!(
            before.functions[0].definition,
            edited.functions[0].definition
        );
    }
}
