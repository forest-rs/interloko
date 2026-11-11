use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use interloko_grammar::ExpectedFragment;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::SourceFileId;
use crate::syntax::{self, CstElement, CstKind, CstNode, Span, SyntaxTree, Token, TokenKind};

use super::interpreter::{
    CompiledGrammar, InterpretedParseError, InterpretedRuleNode, interpret_rule_error,
};

pub(super) struct BaseLokoParseOutput {
    pub(super) syntax: SyntaxTree,
    pub(super) parse_tree: Option<interloko_grammar::OwnedRuleNode>,
    pub(super) metrics: interloko_grammar::ParseMetrics,
}

pub(super) fn parse_file(
    grammar: &CompiledGrammar,
    source_file: SourceFileId,
    source: &str,
) -> BaseLokoParseOutput {
    let report = interloko_grammar::interpret_region_recovering(grammar, "source-file", source, 0);
    match report.root {
        Some(root) => {
            let tokens = syntax::lex(source);
            let projector = BaseLokoCstProjector {
                source_file,
                source,
                tokens,
            };
            let parse_tree = root.to_owned_tree();
            let mut syntax = projector.project(&root);
            syntax.diagnostics.extend(
                report
                    .errors
                    .into_iter()
                    .map(|error| base_loko_parse_diagnostic(grammar, source, error)),
            );
            BaseLokoParseOutput {
                parse_tree: Some(parse_tree),
                syntax,
                metrics: report.metrics,
            }
        }
        None => {
            let Some(error) = report.errors.into_iter().next() else {
                // The kernel always returns either a recovered root or
                // at least one fallback parse error. Anything else is
                // a kernel bug, not a recoverable frontend state.
                unreachable!("recovering parse returned neither a root nor an error");
            };
            let diagnostic = base_loko_parse_diagnostic(grammar, source, error);
            BaseLokoParseOutput {
                syntax: syntax_tree_with_diagnostic(source_file, source, diagnostic),
                parse_tree: None,
                metrics: report.metrics,
            }
        }
    }
}

fn syntax_tree_with_diagnostic(
    source_file: SourceFileId,
    source: &str,
    diagnostic: Diagnostic,
) -> SyntaxTree {
    SyntaxTree {
        source_file,
        source: source.to_string(),
        tokens: syntax::lex(source),
        root: CstNode::from_children(
            source_file,
            CstKind::Root,
            Vec::new(),
            &[],
            source,
            Span::new(0, source.len()),
        ),
        diagnostics: vec![diagnostic],
    }
}

fn base_loko_parse_diagnostic(
    grammar: &super::interpreter::CompiledGrammar,
    source: &str,
    error: InterpretedParseError,
) -> Diagnostic {
    grammar_parse_diagnostic(base_loko_parse_error(grammar, source, error))
}

fn base_loko_parse_error(
    grammar: &super::interpreter::CompiledGrammar,
    source: &str,
    mut error: InterpretedParseError,
) -> InterpretedParseError {
    if error
        .expected
        .iter()
        .any(|expected| matches!(expected, ExpectedFragment::EndOfRegion))
    {
        error = interpret_rule_error(grammar, "definition", source, error.span.start, 0);
    }
    error
}

fn grammar_parse_diagnostic(error: InterpretedParseError) -> Diagnostic {
    let expected = if error.expected.is_empty() {
        "base Loko grammar".to_string()
    } else {
        error
            .expected
            .iter()
            .map(|expected| alloc::format!("{expected}"))
            .collect::<Vec<_>>()
            .join(" or ")
    };
    Diagnostic::error(error.span.into(), DiagnosticCode::SyntaxExpectedToken)
        .with_arg(DiagnosticArg::text("expected", expected))
}

struct BaseLokoCstProjector<'source> {
    source_file: SourceFileId,
    source: &'source str,
    tokens: Vec<Token>,
}

impl BaseLokoCstProjector<'_> {
    fn project(self, root: &InterpretedRuleNode<'_>) -> SyntaxTree {
        let root = self.build_root(root);
        SyntaxTree {
            source_file: self.source_file,
            source: self.source.to_string(),
            tokens: self.tokens,
            root,
            diagnostics: Vec::new(),
        }
    }

    fn build_root(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let children = node
            .children
            .iter()
            .filter(|child| child.is_rule("definition"))
            .filter_map(|definition| self.build_definition(definition))
            .map(CstElement::Node)
            .collect();
        self.node(CstKind::Root, children, node.span)
    }

    fn build_definition(&self, node: &InterpretedRuleNode<'_>) -> Option<CstNode> {
        node.children
            .iter()
            .find_map(|child| match child.name().unwrap_or("") {
                "class-definition" => Some(self.build_class(child)),
                "generic-definition" => Some(self.build_generic(child)),
                "method-definition" => Some(self.build_callable(child, CstKind::Method)),
                "function-definition" => Some(self.build_callable(child, CstKind::Function)),
                "constant-definition" => Some(self.build_constant(child)),
                _ => None,
            })
    }

    fn build_constant(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let value = node
            .children
            .iter()
            .find(|child| child.is_rule("expression"))
            .map(|expr| self.build_expr(expr));
        let mut roles = Vec::new();
        self.collect_role_nodes(
            node,
            "value-definition-name",
            CstKind::ValueDefinitionName,
            &mut roles,
        );
        self.collect_type_roles_from_rule_children(node, &["type-annotation"], &mut roles);
        let excluded = value
            .as_ref()
            .map_or_else(Vec::new, |value| vec![value.span]);
        let mut children = self.direct_tokens(node.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        if let Some(value) = value {
            children.push(CstElement::Node(value));
        }
        self.node(CstKind::Constant, self.sorted(children), node.span)
    }

    fn build_class(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let mut roles = Vec::new();
        self.collect_role_nodes(
            node,
            "type-definition-name",
            CstKind::TypeDefinitionName,
            &mut roles,
        );
        self.collect_role_nodes(node, "superclass-type", CstKind::SuperclassType, &mut roles);
        let slots = node
            .children
            .iter()
            .filter(|child| child.is_rule("slot-declaration"))
            .map(|slot| self.build_slot(slot))
            .collect::<Vec<_>>();
        let excluded = slots.iter().map(|slot| slot.span).collect::<Vec<_>>();
        let mut children = self.direct_tokens(node.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        children.extend(slots.into_iter().map(CstElement::Node));
        self.node(CstKind::Class, self.sorted(children), node.span)
    }

    fn build_slot(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let inner = node
            .children
            .iter()
            .find(|child| {
                matches!(
                    child.name().unwrap_or(""),
                    "constant-slot-declaration" | "mutable-slot-declaration"
                )
            })
            .unwrap_or(node);
        let options = inner
            .children
            .iter()
            .filter(|child| child.is_rule("slot-option-tail"))
            .filter_map(|tail| {
                tail.children
                    .iter()
                    .find(|child| child.is_rule("slot-option"))
            })
            .map(|option| self.build_slot_option(option))
            .collect::<Vec<_>>();
        let mut roles = Vec::new();
        self.collect_role_nodes(inner, "slot-name", CstKind::SlotName, &mut roles);
        self.collect_type_roles_from_rule_children(inner, &["type-annotation"], &mut roles);
        let excluded = options.iter().map(|option| option.span).collect::<Vec<_>>();
        let mut children = self.direct_tokens(inner.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        children.extend(options.into_iter().map(CstElement::Node));
        self.node(CstKind::SlotDecl, self.sorted(children), inner.span)
    }

    fn build_slot_option(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let inner = node
            .children
            .iter()
            .find(|child| {
                matches!(
                    child.name().unwrap_or(""),
                    "init-value-option"
                        | "init-keyword-option"
                        | "required-init-keyword-option"
                        | "extension-slot-option"
                )
            })
            .unwrap_or(node);
        let expr = inner
            .children
            .iter()
            .find(|child| child.is_rule("expression"))
            .map(|expr| self.build_expr(expr));
        let slot_keyword = inner
            .children
            .iter()
            .find(|child| child.is_rule("slot-keyword"))
            .map(|keyword| self.leaf(CstKind::SlotKeyword, keyword));
        let mut excluded = Vec::new();
        if let Some(expr) = &expr {
            excluded.push(expr.span);
        }
        if let Some(slot_keyword) = &slot_keyword {
            excluded.push(slot_keyword.span);
        }
        let mut children = self.direct_tokens(inner.span, &excluded);
        if let Some(slot_keyword) = slot_keyword {
            children.push(CstElement::Node(slot_keyword));
        }
        if let Some(expr) = expr {
            children.push(CstElement::Node(expr));
        }
        self.node(CstKind::SlotOption, self.sorted(children), inner.span)
    }

    fn build_generic(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let params = node
            .children
            .iter()
            .find(|child| child.is_rule("parameter-list"))
            .map(|params| self.build_parameter_list(params));
        let mut roles = Vec::new();
        self.collect_role_nodes(
            node,
            "value-definition-name",
            CstKind::ValueDefinitionName,
            &mut roles,
        );
        self.collect_type_roles_from_rule_children(node, &["result-annotation"], &mut roles);
        let excluded = params
            .as_ref()
            .map_or_else(Vec::new, |params| vec![params.span]);
        let mut children = self.direct_tokens(node.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        if let Some(params) = params {
            children.push(CstElement::Node(params));
        }
        self.node(CstKind::Generic, self.sorted(children), node.span)
    }

    fn build_callable(&self, node: &InterpretedRuleNode<'_>, kind: CstKind) -> CstNode {
        let params = node
            .children
            .iter()
            .find(|child| child.is_rule("parameter-list"))
            .map(|params| self.build_parameter_list(params));
        let body = node
            .children
            .iter()
            .find(|child| child.is_rule("body"))
            .map(|body| self.build_body(body));
        let mut roles = Vec::new();
        self.collect_role_nodes(
            node,
            "value-definition-name",
            CstKind::ValueDefinitionName,
            &mut roles,
        );
        self.collect_type_roles_from_rule_children(node, &["result-annotation"], &mut roles);
        let mut excluded = Vec::new();
        if let Some(params) = &params {
            excluded.push(params.span);
        }
        if let Some(body) = &body {
            excluded.push(body.span);
        }
        let mut children = self.direct_tokens(node.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        if let Some(params) = params {
            children.push(CstElement::Node(params));
        }
        if let Some(body) = body {
            children.push(CstElement::Node(body));
        }
        self.node(kind, self.sorted(children), node.span)
    }

    fn build_parameter_list(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let mut roles = Vec::new();
        self.collect_role_nodes(node, "parameter-name", CstKind::ParameterName, &mut roles);
        self.collect_annotation_type_roles(node, &mut roles);
        let mut children = self.direct_tokens(node.span, &[]);
        children.extend(roles.into_iter().map(CstElement::Node));
        self.node(CstKind::ParameterList, self.sorted(children), node.span)
    }

    fn build_body(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let lets = node
            .children
            .iter()
            .filter(|child| child.is_rule("let-statement"))
            .map(|let_stmt| self.build_let(let_stmt))
            .collect::<Vec<_>>();
        let expr = node
            .children
            .iter()
            .find(|child| {
                child.is_rule("expression")
                    && (!child.span.is_empty() || !child.children.is_empty())
            })
            .map(|expr| self.build_expr(expr));
        let mut children = lets.into_iter().map(CstElement::Node).collect::<Vec<_>>();
        if let Some(expr) = expr {
            children.push(CstElement::Node(expr));
        }
        self.node(CstKind::Body, self.sorted(children), node.span)
    }

    fn build_let(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let expr = node
            .children
            .iter()
            .find(|child| child.is_rule("expression"))
            .map(|expr| self.build_expr(expr));
        let mut roles = Vec::new();
        if let Some(binding) = node
            .children
            .iter()
            .find(|child| child.is_rule("let-binding"))
        {
            self.collect_role_nodes(binding, "let-name", CstKind::LetName, &mut roles);
            self.collect_annotation_type_roles(binding, &mut roles);
        }
        let excluded = expr.as_ref().map_or_else(Vec::new, |expr| vec![expr.span]);
        let mut children = self.direct_tokens(node.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        if let Some(expr) = expr {
            children.push(CstElement::Node(expr));
        }
        self.node(CstKind::LetStmt, self.sorted(children), node.span)
    }

    fn build_expr(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        match node.name().unwrap_or("") {
            "expression" => self.unwrap_first_expr(node),
            "handle-expression" => self.build_handle(node),
            "block-expression" => self.build_block(node),
            "restart-clause" => self.build_restart(node),
            "if-expression" => self.build_if(node),
            "assignment-expression" => self.build_assignment(node),
            "assignment-target" => self.unwrap_first_expr(node),
            "comparison-expression" => self.build_comparison(node),
            "additive-expression" => self.build_additive(node),
            "multiplicative-expression" => self.build_multiplicative(node),
            "unary-expression" => self.unwrap_first_expr(node),
            "negative-expression" => self.build_unary(node),
            "postfix-expression" => self.build_postfix(node),
            "primary-expression" => self.unwrap_first_expr(node),
            "parenthesized-expression" => node
                .children
                .iter()
                .find(|child| child.is_rule("expression"))
                .map_or_else(|| self.error_node(node.span), |expr| self.build_expr(expr)),
            "function-expression" => self.build_callable(node, CstKind::FunctionExpr),
            "vector-expression" => self.build_vector(node),
            "call-expression" => self.build_call(node),
            "argument-list" => self.build_argument_list(node),
            "keyword-argument" => self.build_keyword_arg(node),
            "name-expression" => self.leaf(CstKind::NameExpr, node),
            "type-expression" => self.build_type_expr(node),
            "number-literal" => self.leaf(CstKind::NumberExpr, node),
            "bool-literal" => self.leaf(CstKind::BoolExpr, node),
            "string-literal" => self.leaf(CstKind::StringExpr, node),
            "symbol-literal" => self.leaf(CstKind::SymbolExpr, node),
            "keyword-symbol" => self.build_keyword_symbol(node),
            _ => self.unwrap_first_expr(node),
        }
    }

    fn unwrap_first_expr(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        node.children
            .iter()
            .find(|child| child.name().is_some_and(|name| self.is_expr_rule(name)))
            .map_or_else(
                || self.error_node(node.span),
                |child| self.build_expr(child),
            )
    }

    fn build_keyword_symbol(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        self.leaf(CstKind::KeywordExpr, node)
    }

    fn build_type_expr(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let mut roles = Vec::new();
        self.collect_role_nodes(
            node,
            "expression-type-name",
            CstKind::ExpressionTypeName,
            &mut roles,
        );
        let mut children = self.direct_tokens(node.span, &[]);
        children.extend(roles.into_iter().map(CstElement::Node));
        self.node(CstKind::TypeExpr, self.sorted(children), node.span)
    }

    fn build_handle(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let nested = node
            .children
            .iter()
            .filter(|child| child.is_rule("expression") || child.is_rule("restart-clause"))
            .map(|child| self.build_expr(child))
            .collect::<Vec<_>>();
        self.node_with_nested(CstKind::HandleExpr, node, nested)
    }

    fn build_block(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let mut nested = Vec::new();
        for child in &node.children {
            if child.is_rule("body") {
                nested.push(self.build_body(child));
            } else if child.is_rule("cleanup-clause") {
                let body = child
                    .children
                    .iter()
                    .find(|body| body.is_rule("body"))
                    .map(|body| vec![self.build_body(body)])
                    .unwrap_or_default();
                nested.push(self.node_with_nested(CstKind::CleanupClause, child, body));
            }
        }
        self.node_with_nested(CstKind::BlockExpr, node, nested)
    }

    fn build_restart(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let nested = node
            .children
            .iter()
            .find(|child| child.is_rule("expression"))
            .map(|expr| vec![self.build_expr(expr)])
            .unwrap_or_default();
        let mut roles = Vec::new();
        if let Some(condition) = node
            .children
            .iter()
            .find(|child| child.is_rule("restart-condition"))
        {
            self.collect_role_nodes(condition, "restart-name", CstKind::RestartName, &mut roles);
            self.collect_role_nodes(
                condition,
                "restart-condition-type",
                CstKind::RestartConditionType,
                &mut roles,
            );
            self.collect_annotation_type_roles(condition, &mut roles);
        }
        self.node_with_nested_and_roles(CstKind::RestartClause, node, nested, roles)
    }

    fn build_if(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let nested = node
            .children
            .iter()
            .filter(|child| child.is_rule("expression"))
            .map(|expr| self.build_expr(expr))
            .collect::<Vec<_>>();
        self.node_with_nested(CstKind::IfExpr, node, nested)
    }

    fn build_assignment(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        if let Some(target_rule) = node
            .children
            .iter()
            .find(|child| child.is_rule("assignment-target"))
        {
            let target = self.build_expr(target_rule);
            let value = node
                .children
                .iter()
                .find(|child| child.is_rule("expression"))
                .map_or_else(|| self.error_node(node.span), |expr| self.build_expr(expr));
            let children = vec![
                CstElement::Node(target),
                CstElement::Token(self.first_token_index(node.span, TokenKind::Assign)),
                CstElement::Node(value),
            ];
            return self.node(CstKind::SlotAssignExpr, children, node.span);
        }

        let target = node
            .children
            .iter()
            .find(|child| child.is_rule("comparison-expression"))
            .map_or_else(|| self.error_node(node.span), |expr| self.build_expr(expr));
        let Some(tail) = node
            .children
            .iter()
            .find(|child| child.is_rule("assignment-tail"))
        else {
            return target;
        };
        let value = tail
            .children
            .iter()
            .find(|child| child.is_rule("expression"))
            .map_or_else(|| self.error_node(tail.span), |expr| self.build_expr(expr));
        let children = vec![
            CstElement::Node(target),
            CstElement::Token(self.first_token_index(tail.span, TokenKind::Assign)),
            CstElement::Node(value),
        ];
        self.node(CstKind::SlotAssignExpr, children, node.span)
    }

    fn build_comparison(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let lhs = node
            .children
            .iter()
            .find(|child| child.is_rule("additive-expression"))
            .map_or_else(|| self.error_node(node.span), |expr| self.build_expr(expr));
        let Some(tail) = node
            .children
            .iter()
            .find(|child| child.is_rule("comparison-tail"))
        else {
            return lhs;
        };
        let rhs = tail
            .children
            .iter()
            .find(|child| child.is_rule("additive-expression"))
            .map_or_else(|| self.error_node(tail.span), |expr| self.build_expr(expr));
        let op = self.first_operator_token(tail.span);
        self.node(
            CstKind::BinaryExpr,
            vec![
                CstElement::Node(lhs),
                CstElement::Token(op),
                CstElement::Node(rhs),
            ],
            node.span,
        )
    }

    fn build_additive(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let operands = node
            .children
            .iter()
            .filter(|child| child.is_rule("multiplicative-expression"))
            .collect::<Vec<_>>();
        let Some(first) = operands.first() else {
            return self.error_node(node.span);
        };
        if operands.len() == 1 {
            return self.build_expr(first);
        }
        let mut folded = self.build_expr(first);
        for rhs_node in operands.into_iter().skip(1) {
            let rhs = self.build_expr(rhs_node);
            let op = self.first_operator_token(Span::new(folded.span.end, rhs.span.start));
            let span = folded.span.cover(rhs.span);
            folded = self.node(
                CstKind::BinaryExpr,
                vec![
                    CstElement::Node(folded),
                    CstElement::Token(op),
                    CstElement::Node(rhs),
                ],
                span,
            );
        }
        folded
    }

    fn build_multiplicative(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let operands = node
            .children
            .iter()
            .filter(|child| child.is_rule("unary-expression"))
            .collect::<Vec<_>>();
        let Some(first) = operands.first() else {
            return self.error_node(node.span);
        };
        if operands.len() == 1 {
            return self.build_expr(first);
        }
        let mut folded = self.build_expr(first);
        for rhs_node in operands.into_iter().skip(1) {
            let rhs = self.build_expr(rhs_node);
            let op = self.first_operator_token(Span::new(folded.span.end, rhs.span.start));
            let span = folded.span.cover(rhs.span);
            folded = self.node(
                CstKind::BinaryExpr,
                vec![
                    CstElement::Node(folded),
                    CstElement::Token(op),
                    CstElement::Node(rhs),
                ],
                span,
            );
        }
        folded
    }

    fn build_unary(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let expr = node
            .children
            .iter()
            .find(|child| child.is_rule("unary-expression"))
            .map_or_else(|| self.error_node(node.span), |expr| self.build_expr(expr));
        self.node(
            CstKind::UnaryExpr,
            vec![
                CstElement::Token(self.first_token_index(node.span, TokenKind::Minus)),
                CstElement::Node(expr),
            ],
            node.span,
        )
    }

    fn build_postfix(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let Some(primary) = node
            .children
            .iter()
            .find(|child| child.is_rule("primary-expression"))
        else {
            return self.error_node(node.span);
        };
        let mut folded = self.build_expr(primary);
        for name_node in node
            .children
            .iter()
            .filter(|child| child.is_rule("member-name"))
        {
            let dot = self.first_token_index(
                Span::new(folded.span.end, name_node.span.start),
                TokenKind::Dot,
            );
            let name = self.first_token_index(Span::from(name_node.span), TokenKind::Ident);
            let member_name = self.leaf(CstKind::MemberName, name_node);
            let span = folded.span.cover(self.tokens[name].span);
            folded = self.node(
                CstKind::SlotExpr,
                vec![
                    CstElement::Node(folded),
                    CstElement::Token(dot),
                    CstElement::Token(name),
                    CstElement::Node(member_name),
                ],
                span,
            );
        }
        folded
    }

    fn build_vector(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        // `vector-items` invokes `trailing-list-of<expression, `,`>`; the
        // kernel substitutes the parametric body inline, so children are
        // flat `expression` nodes with no `list-of` wrapper to descend.
        let nested = node
            .children
            .iter()
            .find(|child| child.is_rule("vector-items"))
            .map(|items| {
                items
                    .children
                    .iter()
                    .filter(|child| child.is_rule("expression"))
                    .map(|expr| self.build_expr(expr))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.node_with_nested(CstKind::VectorExpr, node, nested)
    }

    fn build_call(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let arg_list = node
            .children
            .iter()
            .find(|child| child.is_rule("argument-list"))
            .map(|args| self.build_argument_list(args));
        let mut roles = Vec::new();
        self.collect_role_nodes(node, "call-name", CstKind::CallName, &mut roles);
        let excluded = arg_list
            .as_ref()
            .map_or_else(Vec::new, |args| vec![args.span]);
        let mut children = self.direct_tokens(node.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        if let Some(arg_list) = arg_list {
            children.push(CstElement::Node(arg_list));
        }
        self.node(CstKind::CallExpr, self.sorted(children), node.span)
    }

    fn build_argument_list(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        // `argument-list` invokes `trailing-list-of<argument, `,`>`; the
        // kernel substitutes the parametric body inline, so children are
        // flat `argument` nodes with no `list-of` wrapper to descend.
        let mut nested = Vec::new();
        for arg in node
            .children
            .iter()
            .filter(|child| child.is_rule("argument"))
        {
            if let Some(child) = arg
                .children
                .iter()
                .find(|child| child.is_rule("keyword-argument"))
                .map(|keyword| self.build_keyword_arg(keyword))
                .or_else(|| {
                    arg.children
                        .iter()
                        .find(|child| child.is_rule("expression"))
                        .map(|expr| self.build_expr(expr))
                })
            {
                nested.push(child);
            }
        }
        self.node_with_nested(CstKind::ArgList, node, nested)
    }

    fn build_keyword_arg(&self, node: &InterpretedRuleNode<'_>) -> CstNode {
        let argument_keyword = node
            .children
            .iter()
            .find(|child| child.is_rule("argument-keyword"))
            .map(|keyword| self.leaf(CstKind::ArgumentKeyword, keyword));
        let expr = node
            .children
            .iter()
            .find(|child| child.is_rule("expression"))
            .map(|expr| self.build_expr(expr));
        let mut excluded = Vec::new();
        if let Some(argument_keyword) = &argument_keyword {
            excluded.push(argument_keyword.span);
        }
        if let Some(expr) = &expr {
            excluded.push(expr.span);
        }
        let mut children = self.direct_tokens(node.span, &excluded);
        if let Some(argument_keyword) = argument_keyword {
            children.push(CstElement::Node(argument_keyword));
        }
        if let Some(expr) = expr {
            children.push(CstElement::Node(expr));
        }
        self.node(CstKind::KeywordArg, self.sorted(children), node.span)
    }

    fn collect_role_nodes(
        &self,
        node: &InterpretedRuleNode<'_>,
        rule_name: &str,
        kind: CstKind,
        out: &mut Vec<CstNode>,
    ) {
        if node.is_rule(rule_name) {
            out.push(self.leaf(kind, node));
            return;
        }
        for child in &node.children {
            self.collect_role_nodes(child, rule_name, kind, out);
        }
    }

    fn collect_type_roles_from_rule_children(
        &self,
        node: &InterpretedRuleNode<'_>,
        rule_names: &[&str],
        out: &mut Vec<CstNode>,
    ) {
        for child in &node.children {
            if child.name().is_some_and(|name| rule_names.contains(&name)) {
                self.collect_annotation_type_roles(child, out);
            }
        }
    }

    fn collect_annotation_type_roles(
        &self,
        node: &InterpretedRuleNode<'_>,
        out: &mut Vec<CstNode>,
    ) {
        match node.name().unwrap_or("") {
            "class-object-annotation-type" => {
                out.push(self.leaf(CstKind::ClassObjectAnnotationType, node));
                return;
            }
            "annotation-type" => {
                out.push(self.leaf(CstKind::AnnotationType, node));
                return;
            }
            _ => {}
        }
        for child in &node.children {
            self.collect_annotation_type_roles(child, out);
        }
    }

    fn node_with_nested(
        &self,
        kind: CstKind,
        source: &InterpretedRuleNode<'_>,
        nested: Vec<CstNode>,
    ) -> CstNode {
        self.node_with_nested_and_roles(kind, source, nested, Vec::new())
    }

    fn node_with_nested_and_roles(
        &self,
        kind: CstKind,
        source: &InterpretedRuleNode<'_>,
        nested: Vec<CstNode>,
        roles: Vec<CstNode>,
    ) -> CstNode {
        let excluded = nested.iter().map(|node| node.span).collect::<Vec<_>>();
        let mut children = self.direct_tokens(source.span, &excluded);
        children.extend(roles.into_iter().map(CstElement::Node));
        children.extend(nested.into_iter().map(CstElement::Node));
        self.node(kind, self.sorted(children), source.span)
    }

    fn leaf(&self, kind: CstKind, node: &InterpretedRuleNode<'_>) -> CstNode {
        self.node(kind, self.direct_tokens(node.span, &[]), node.span)
    }

    fn error_node(&self, span: impl Into<Span>) -> CstNode {
        let span = span.into();
        self.node(CstKind::Error, self.direct_tokens(span, &[]), span)
    }

    fn node(&self, kind: CstKind, children: Vec<CstElement>, fallback: impl Into<Span>) -> CstNode {
        CstNode::from_children(
            self.source_file,
            kind,
            children,
            &self.tokens,
            self.source,
            fallback.into(),
        )
    }

    /// Returns the tokens contained in `span`, with their indices.
    ///
    /// Tokens are in source order, so only the run that starts inside
    /// `span` can be contained in it. Finding that run by binary search
    /// keeps each lookup proportional to the span rather than to the
    /// file; scanning every token made projection quadratic.
    fn tokens_in(&self, span: Span) -> impl Iterator<Item = (usize, &Token)> {
        let first = self
            .tokens
            .partition_point(|token| token.span.start < span.start);
        (first..)
            .zip(&self.tokens[first..])
            .take_while(move |(_, token)| token.span.start <= span.end)
            .filter(move |(_, token)| span_contains(span, token.span))
    }

    fn direct_tokens(&self, span: impl Into<Span>, excluded: &[Span]) -> Vec<CstElement> {
        let span = span.into();
        self.tokens_in(span)
            .filter(|(_, token)| !token.kind.is_trivia())
            .filter(|(_, token)| {
                !excluded
                    .iter()
                    .any(|excluded| span_contains(*excluded, token.span))
            })
            .map(|(index, _)| CstElement::Token(index))
            .collect()
    }

    fn first_token_index(&self, span: impl Into<Span>, kind: TokenKind) -> usize {
        self.tokens_in(span.into())
            .find(|(_, token)| token.kind == kind)
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn first_operator_token(&self, span: impl Into<Span>) -> usize {
        self.tokens_in(span.into())
            .find(|(_, token)| token.kind.is_infix_operator())
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn sorted(&self, mut children: Vec<CstElement>) -> Vec<CstElement> {
        children.sort_by_key(|child| self.element_span(child).start);
        children
    }

    fn element_span(&self, child: &CstElement) -> Span {
        match child {
            CstElement::Node(node) => node.span,
            CstElement::Token(index) => self.tokens[*index].span,
        }
    }

    fn is_expr_rule(&self, rule: &str) -> bool {
        matches!(
            rule,
            "handle-expression"
                | "block-expression"
                | "if-expression"
                | "assignment-expression"
                | "assignment-target"
                | "comparison-expression"
                | "additive-expression"
                | "multiplicative-expression"
                | "unary-expression"
                | "negative-expression"
                | "postfix-expression"
                | "primary-expression"
                | "function-expression"
                | "parenthesized-expression"
                | "vector-expression"
                | "call-expression"
                | "name-expression"
                | "type-expression"
                | "number-literal"
                | "bool-literal"
                | "string-literal"
                | "symbol-literal"
                | "keyword-symbol"
        )
    }
}

fn span_contains(outer: impl Into<Span>, inner: impl Into<Span>) -> bool {
    let outer = outer.into();
    let inner = inner.into();
    outer.start <= inner.start
        && (inner.end <= outer.end || inner.start == inner.end && inner.start <= outer.end)
}
