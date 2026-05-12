//! Surface AST lowered from the milestone-0 CST.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
use crate::syntax::{CstElement, CstKind, CstNode, Span, SyntaxTree, TokenKind};

/// AST module containing function definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Function definitions in source order.
    pub functions: Vec<Function>,
    /// AST lowering diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Function definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Function name.
    pub name: String,
    /// Parameter names in source order.
    pub params: Vec<String>,
    /// Function body.
    pub body: Block,
    /// Source span.
    pub span: Span,
}

/// Function body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
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

/// Surface expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    /// Integer literal.
    Number {
        /// Parsed value.
        value: i64,
        /// Source span.
        span: Span,
    },
    /// Name reference.
    Name {
        /// Referenced name.
        name: String,
        /// Source span.
        span: Span,
    },
    /// Direct call syntax.
    Call {
        /// Callee name.
        callee: String,
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
        lhs: alloc::boxed::Box<Expr>,
        /// Right operand.
        rhs: alloc::boxed::Box<Expr>,
        /// Source span.
        span: Span,
    },
    /// Placeholder for malformed syntax.
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
            Self::Number { span, .. }
            | Self::Name { span, .. }
            | Self::Call { span, .. }
            | Self::Binary { span, .. }
            | Self::Error { span } => *span,
        }
    }
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
        let functions = self
            .tree
            .root
            .child_nodes(CstKind::Function)
            .map(|node| self.lower_function(node))
            .collect();
        Module {
            functions,
            diagnostics: self.diagnostics.clone(),
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
            lets: Vec::new(),
            result: Expr::Error { span: node.span },
            span: node.span,
        });

        Function {
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
            lets,
            result: result.unwrap_or(Expr::Error { span: node.span }),
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
            name,
            value: value.unwrap_or(Expr::Error { span: node.span }),
            span: node.span,
        }
    }

    fn lower_expr(&mut self, node: &CstNode) -> Expr {
        match node.kind {
            CstKind::NumberExpr => self.lower_number(node),
            CstKind::NameExpr => self.lower_name(node),
            CstKind::CallExpr => self.lower_call(node),
            CstKind::BinaryExpr => self.lower_binary(node),
            _ => Expr::Error { span: node.span },
        }
    }

    fn lower_number(&mut self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Number) else {
            return Expr::Error { span: node.span };
        };
        match text.parse::<i64>() {
            Ok(value) => Expr::Number { value, span },
            Err(_) => {
                self.diagnostics
                    .push(Diagnostic::error(span, "integer literal is out of range"));
                Expr::Error { span }
            }
        }
    }

    fn lower_name(&self, node: &CstNode) -> Expr {
        let Some((span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Expr::Error { span: node.span };
        };
        Expr::Name {
            name: text.to_string(),
            span,
        }
    }

    fn lower_call(&mut self, node: &CstNode) -> Expr {
        let Some((name_span, text)) = self.first_token_text(node, TokenKind::Ident) else {
            return Expr::Error { span: node.span };
        };
        let mut args = Vec::new();
        for arg_list in node.child_nodes(CstKind::ArgList) {
            for child in &arg_list.children {
                if let CstElement::Node(expr) = child {
                    args.push(self.lower_expr(expr));
                }
            }
        }
        Expr::Call {
            callee: text.to_string(),
            args,
            span: node.span.cover(name_span),
        }
    }

    fn lower_binary(&mut self, node: &CstNode) -> Expr {
        let mut exprs = node.children.iter().filter_map(|child| match child {
            CstElement::Node(expr) => Some(self.lower_expr(expr)),
            _ => None,
        });
        let lhs = exprs.next().unwrap_or(Expr::Error { span: node.span });
        let rhs = exprs.next().unwrap_or(Expr::Error { span: node.span });
        let op = node.children.iter().find_map(|child| match child {
            CstElement::Token(index) => match self.tree.tokens[*index].kind {
                TokenKind::Plus => Some(BinaryOp::Add),
                TokenKind::Minus => Some(BinaryOp::Sub),
                TokenKind::Star => Some(BinaryOp::Mul),
                _ => None,
            },
            _ => None,
        });

        Expr::Binary {
            op: op.unwrap_or(BinaryOp::Add),
            lhs: alloc::boxed::Box::new(lhs),
            rhs: alloc::boxed::Box::new(rhs),
            span: node.span,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
