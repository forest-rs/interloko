//! Lexer, lossless-enough token stream, and milestone-0 CST parser.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;

/// A byte span in a source file.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Span {
    /// Inclusive byte offset where the span starts.
    pub start: usize,
    /// Exclusive byte offset where the span ends.
    pub end: usize,
}

impl Span {
    /// Creates a span from byte offsets.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Returns a span covering both inputs.
    #[must_use]
    pub fn cover(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Returns true when this span is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Lexical token kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// `define`.
    Define,
    /// `function`.
    Function,
    /// `class`.
    Class,
    /// `slot`.
    Slot,
    /// `generic`.
    Generic,
    /// `method`.
    Method,
    /// `let`.
    Let,
    /// `end`.
    End,
    /// User identifier.
    Ident,
    /// Base-10 integer literal.
    Number,
    /// Nominal type/class name such as `<circle>`.
    TypeName,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `,`.
    Comma,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `=`.
    Equal,
    /// `:`.
    Colon,
    /// `::`.
    DoubleColon,
    /// `.`.
    Dot,
    /// `;`.
    Semicolon,
    /// Whitespace trivia.
    Whitespace,
    /// Line comment trivia.
    Comment,
    /// Unknown byte or character.
    Error,
}

impl TokenKind {
    fn is_trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment)
    }
}

/// A lexical token with its source span.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Token {
    /// Token kind.
    pub kind: TokenKind,
    /// Source span.
    pub span: Span,
}

impl Token {
    /// Returns the token text from the original source.
    #[must_use]
    pub fn text(self, source: &str) -> &str {
        &source[self.span.start..self.span.end]
    }
}

/// CST node kind.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CstKind {
    /// Root module node.
    Root,
    /// Function definition.
    Function,
    /// Class definition.
    Class,
    /// Slot declaration.
    SlotDecl,
    /// Generic function declaration.
    Generic,
    /// Method definition.
    Method,
    /// Function parameter list.
    ParameterList,
    /// Function body.
    Body,
    /// Local binding statement.
    LetStmt,
    /// Binary expression.
    BinaryExpr,
    /// Direct function call expression.
    CallExpr,
    /// Object slot read expression.
    SlotExpr,
    /// Call argument list.
    ArgList,
    /// Keyword argument.
    KeywordArg,
    /// Name reference expression.
    NameExpr,
    /// Integer literal expression.
    NumberExpr,
    /// Type/class name expression.
    TypeExpr,
    /// Malformed syntax placeholder.
    Error,
}

/// Child element of a CST node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CstElement {
    /// Nested CST node.
    Node(CstNode),
    /// Index into [`SyntaxTree::tokens`].
    Token(usize),
}

impl CstElement {
    fn span(&self, tokens: &[Token]) -> Span {
        match self {
            Self::Node(node) => node.span,
            Self::Token(index) => tokens[*index].span,
        }
    }
}

/// A concrete syntax tree node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CstNode {
    /// Node kind.
    pub kind: CstKind,
    /// Source span covered by this node.
    pub span: Span,
    /// Child nodes and token references.
    pub children: Vec<CstElement>,
}

impl CstNode {
    fn new(kind: CstKind, children: Vec<CstElement>, tokens: &[Token], fallback: Span) -> Self {
        let span = children
            .iter()
            .map(|child| child.span(tokens))
            .reduce(Span::cover)
            .unwrap_or(fallback);
        Self {
            kind,
            span,
            children,
        }
    }

    /// Returns child nodes with `kind`.
    pub fn child_nodes(&self, kind: CstKind) -> impl Iterator<Item = &CstNode> {
        self.children.iter().filter_map(move |child| match child {
            CstElement::Node(node) if node.kind == kind => Some(node),
            _ => None,
        })
    }
}

/// Parsed source, retaining the original source and all tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxTree {
    /// Original source text.
    pub source: String,
    /// All lexed tokens, including trivia.
    pub tokens: Vec<Token>,
    /// Root CST node.
    pub root: CstNode,
    /// Syntax diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lexes `source` into a token stream that includes trivia.
#[must_use]
pub fn lex(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut pos = 0;

    while pos < bytes.len() {
        let start = pos;
        let b = bytes[pos];
        let kind = match b {
            b'(' => {
                pos += 1;
                TokenKind::LParen
            }
            b')' => {
                pos += 1;
                TokenKind::RParen
            }
            b',' => {
                pos += 1;
                TokenKind::Comma
            }
            b'+' => {
                pos += 1;
                TokenKind::Plus
            }
            b'-' => {
                pos += 1;
                TokenKind::Minus
            }
            b'*' => {
                pos += 1;
                TokenKind::Star
            }
            b'=' => {
                pos += 1;
                TokenKind::Equal
            }
            b':' if bytes.get(pos + 1) == Some(&b':') => {
                pos += 2;
                TokenKind::DoubleColon
            }
            b':' => {
                pos += 1;
                TokenKind::Colon
            }
            b'.' => {
                pos += 1;
                TokenKind::Dot
            }
            b';' => {
                pos += 1;
                TokenKind::Semicolon
            }
            b'<' => {
                pos += 1;
                while pos < bytes.len()
                    && (bytes[pos].is_ascii_alphanumeric()
                        || bytes[pos] == b'_'
                        || bytes[pos] == b'-')
                {
                    pos += 1;
                }
                if bytes.get(pos) == Some(&b'>') {
                    pos += 1;
                    TokenKind::TypeName
                } else {
                    TokenKind::Error
                }
            }
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                TokenKind::Comment
            }
            b if b.is_ascii_whitespace() => {
                pos += 1;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                TokenKind::Whitespace
            }
            b if b.is_ascii_digit() => {
                pos += 1;
                while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                    pos += 1;
                }
                TokenKind::Number
            }
            b if is_ident_start(b) => {
                pos += 1;
                while pos < bytes.len() && is_ident_continue(bytes, pos) {
                    pos += 1;
                }
                match &source[start..pos] {
                    "define" => TokenKind::Define,
                    "function" => TokenKind::Function,
                    "class" => TokenKind::Class,
                    "slot" => TokenKind::Slot,
                    "generic" => TokenKind::Generic,
                    "method" => TokenKind::Method,
                    "let" => TokenKind::Let,
                    "end" => TokenKind::End,
                    _ => TokenKind::Ident,
                }
            }
            _ => {
                pos += 1;
                TokenKind::Error
            }
        };

        tokens.push(Token {
            kind,
            span: Span::new(start, pos),
        });
    }

    tokens
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_ident_continue(bytes: &[u8], pos: usize) -> bool {
    let byte = bytes[pos];
    byte.is_ascii_alphanumeric()
        || byte == b'_'
        || (byte == b'-' && bytes.get(pos + 1).is_some_and(|next| is_ident_start(*next)))
}

/// Parses source into a [`SyntaxTree`].
#[must_use]
pub fn parse(source: &str) -> SyntaxTree {
    let tokens = lex(source);
    Parser::new(source, tokens).parse()
}

struct Parser<'source> {
    source: &'source str,
    tokens: Vec<Token>,
    cursor: usize,
    diagnostics: Vec<Diagnostic>,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str, tokens: Vec<Token>) -> Self {
        Self {
            source,
            tokens,
            cursor: 0,
            diagnostics: Vec::new(),
        }
    }

    fn parse(mut self) -> SyntaxTree {
        let mut children = Vec::new();
        while !self.at_end() {
            if !self.at(TokenKind::Define) {
                if let Some(index) = self.bump() {
                    self.error_at(index, "expected definition");
                    children.push(CstElement::Token(index));
                }
                continue;
            }

            match self.peek_non_trivia(1).map(|(_, token)| token.kind) {
                Some(TokenKind::Function) => children.push(CstElement::Node(self.parse_function())),
                Some(TokenKind::Class) => children.push(CstElement::Node(self.parse_class())),
                Some(TokenKind::Generic) => children.push(CstElement::Node(self.parse_generic())),
                Some(TokenKind::Method) => children.push(CstElement::Node(self.parse_method())),
                _ if self.at(TokenKind::Define) => {
                    let index = self.bump().expect("define token must exist");
                    self.error_at(index, "expected definition kind after `define`");
                    children.push(CstElement::Token(index));
                }
                _ => {}
            }
        }

        let fallback = Span::new(0, self.source.len());
        let root = CstNode::new(CstKind::Root, children, &self.tokens, fallback);
        SyntaxTree {
            source: self.source.to_string(),
            tokens: self.tokens,
            root,
            diagnostics: self.diagnostics,
        }
    }

    fn parse_class(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "expected `define`");
        self.expect(TokenKind::Class, &mut children, "expected `class`");
        self.expect(TokenKind::TypeName, &mut children, "expected class name");
        if self.at(TokenKind::LParen) {
            self.expect(TokenKind::LParen, &mut children, "expected `(`");
            self.expect(
                TokenKind::TypeName,
                &mut children,
                "expected superclass name",
            );
            self.expect(TokenKind::RParen, &mut children, "expected `)`");
        }
        while !self.at(TokenKind::End) && !self.at_end() {
            if self.at(TokenKind::Slot) {
                children.push(CstElement::Node(self.parse_slot_decl()));
            } else if let Some(index) = self.bump() {
                self.error_at(index, "expected `slot` or `end`");
                children.push(CstElement::Token(index));
            }
        }
        self.expect(
            TokenKind::End,
            &mut children,
            "expected `end` after class body",
        );
        if self.at(TokenKind::Semicolon) {
            self.expect(TokenKind::Semicolon, &mut children, "expected `;`");
        }
        CstNode::new(CstKind::Class, children, &self.tokens, start)
    }

    fn parse_slot_decl(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Slot, &mut children, "expected `slot`");
        self.expect(TokenKind::Ident, &mut children, "expected slot name");
        self.expect(
            TokenKind::Semicolon,
            &mut children,
            "expected `;` after slot declaration",
        );
        CstNode::new(CstKind::SlotDecl, children, &self.tokens, start)
    }

    fn parse_generic(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "expected `define`");
        self.expect(TokenKind::Generic, &mut children, "expected `generic`");
        self.expect(TokenKind::Ident, &mut children, "expected generic name");
        self.expect(
            TokenKind::LParen,
            &mut children,
            "expected `(` after generic name",
        );
        children.push(CstElement::Node(self.parse_parameter_list()));
        self.expect(
            TokenKind::RParen,
            &mut children,
            "expected `)` after parameters",
        );
        self.expect(
            TokenKind::Semicolon,
            &mut children,
            "expected `;` after generic declaration",
        );
        CstNode::new(CstKind::Generic, children, &self.tokens, start)
    }

    fn parse_method(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "expected `define`");
        self.expect(TokenKind::Method, &mut children, "expected `method`");
        self.expect(TokenKind::Ident, &mut children, "expected method name");
        self.expect(
            TokenKind::LParen,
            &mut children,
            "expected `(` after method name",
        );
        children.push(CstElement::Node(self.parse_parameter_list()));
        self.expect(
            TokenKind::RParen,
            &mut children,
            "expected `)` after parameters",
        );
        children.push(CstElement::Node(self.parse_body()));
        self.expect(
            TokenKind::End,
            &mut children,
            "expected `end` after method body",
        );
        if self.at(TokenKind::Semicolon) {
            self.expect(TokenKind::Semicolon, &mut children, "expected `;`");
        }
        CstNode::new(CstKind::Method, children, &self.tokens, start)
    }

    fn parse_function(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "expected `define`");
        self.expect(TokenKind::Function, &mut children, "expected `function`");
        self.expect(TokenKind::Ident, &mut children, "expected function name");
        self.expect(
            TokenKind::LParen,
            &mut children,
            "expected `(` after function name",
        );
        children.push(CstElement::Node(self.parse_parameter_list()));
        self.expect(
            TokenKind::RParen,
            &mut children,
            "expected `)` after parameters",
        );
        children.push(CstElement::Node(self.parse_body()));
        self.expect(
            TokenKind::End,
            &mut children,
            "expected `end` after function body",
        );
        if self.at(TokenKind::Semicolon) {
            self.expect(TokenKind::Semicolon, &mut children, "expected `;`");
        }
        CstNode::new(CstKind::Function, children, &self.tokens, start)
    }

    fn parse_parameter_list(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        if self.at(TokenKind::RParen) {
            return CstNode::new(CstKind::ParameterList, children, &self.tokens, start);
        }

        loop {
            self.expect(TokenKind::Ident, &mut children, "expected parameter name");
            if self.at(TokenKind::DoubleColon) {
                self.expect(TokenKind::DoubleColon, &mut children, "expected `::`");
                self.expect(
                    TokenKind::TypeName,
                    &mut children,
                    "expected parameter type",
                );
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.expect(TokenKind::Comma, &mut children, "expected `,`");
        }

        CstNode::new(CstKind::ParameterList, children, &self.tokens, start)
    }

    fn parse_body(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        while self.at(TokenKind::Let) {
            children.push(CstElement::Node(self.parse_let()));
        }

        if !self.at(TokenKind::End) && !self.at_end() {
            children.push(CstElement::Node(self.parse_expr()));
        } else {
            let span = self.current_span();
            self.diagnostics
                .push(Diagnostic::error(span, "expected expression"));
        }

        CstNode::new(CstKind::Body, children, &self.tokens, start)
    }

    fn parse_let(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Let, &mut children, "expected `let`");
        self.expect(TokenKind::Ident, &mut children, "expected local name");
        self.expect(
            TokenKind::Equal,
            &mut children,
            "expected `=` after local name",
        );
        children.push(CstElement::Node(self.parse_expr()));
        self.expect(
            TokenKind::Semicolon,
            &mut children,
            "expected `;` after local binding",
        );
        CstNode::new(CstKind::LetStmt, children, &self.tokens, start)
    }

    fn parse_expr(&mut self) -> CstNode {
        self.parse_add()
    }

    fn parse_add(&mut self) -> CstNode {
        let mut node = self.parse_mul();
        while self.at(TokenKind::Plus) || self.at(TokenKind::Minus) {
            let mut children = vec![CstElement::Node(node)];
            if let Some(op) = self.bump() {
                children.push(CstElement::Token(op));
            }
            children.push(CstElement::Node(self.parse_mul()));
            node = CstNode::new(CstKind::BinaryExpr, children, &self.tokens, Span::default());
        }
        node
    }

    fn parse_mul(&mut self) -> CstNode {
        let mut node = self.parse_postfix();
        while self.at(TokenKind::Star) {
            let mut children = vec![CstElement::Node(node)];
            if let Some(op) = self.bump() {
                children.push(CstElement::Token(op));
            }
            children.push(CstElement::Node(self.parse_postfix()));
            node = CstNode::new(CstKind::BinaryExpr, children, &self.tokens, Span::default());
        }
        node
    }

    fn parse_postfix(&mut self) -> CstNode {
        let mut node = self.parse_primary();
        while self.at(TokenKind::Dot) {
            let start = node.span;
            let mut children = vec![CstElement::Node(node)];
            self.expect(TokenKind::Dot, &mut children, "expected `.`");
            self.expect(TokenKind::Ident, &mut children, "expected slot name");
            node = CstNode::new(CstKind::SlotExpr, children, &self.tokens, start);
        }
        node
    }

    fn parse_primary(&mut self) -> CstNode {
        let start = self.current_span();
        if self.at(TokenKind::Number) {
            let mut children = Vec::new();
            self.expect(TokenKind::Number, &mut children, "expected number");
            return CstNode::new(CstKind::NumberExpr, children, &self.tokens, start);
        }

        if self.at(TokenKind::TypeName) {
            let mut children = Vec::new();
            self.expect(TokenKind::TypeName, &mut children, "expected type name");
            return CstNode::new(CstKind::TypeExpr, children, &self.tokens, start);
        }

        if self.at(TokenKind::Ident) {
            let Some(name) = self.bump() else {
                return CstNode::new(CstKind::Error, Vec::new(), &self.tokens, start);
            };
            if self.at(TokenKind::LParen) {
                let mut children = vec![CstElement::Token(name)];
                self.expect(TokenKind::LParen, &mut children, "expected `(`");
                children.push(CstElement::Node(self.parse_arg_list()));
                self.expect(
                    TokenKind::RParen,
                    &mut children,
                    "expected `)` after arguments",
                );
                return CstNode::new(CstKind::CallExpr, children, &self.tokens, start);
            }
            return CstNode::new(
                CstKind::NameExpr,
                vec![CstElement::Token(name)],
                &self.tokens,
                start,
            );
        }

        if self.at(TokenKind::LParen) {
            let _ = self.bump();
            let node = self.parse_expr();
            let mut ignored = Vec::new();
            self.expect(TokenKind::RParen, &mut ignored, "expected `)`");
            return node;
        }

        if let Some(index) = self.bump() {
            self.error_at(index, "expected expression");
            return CstNode::new(
                CstKind::Error,
                vec![CstElement::Token(index)],
                &self.tokens,
                start,
            );
        }

        self.diagnostics
            .push(Diagnostic::error(start, "expected expression"));
        CstNode::new(CstKind::Error, Vec::new(), &self.tokens, start)
    }

    fn parse_arg_list(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        if self.at(TokenKind::RParen) {
            return CstNode::new(CstKind::ArgList, children, &self.tokens, start);
        }

        loop {
            if self.at(TokenKind::Ident)
                && self
                    .peek_non_trivia(1)
                    .is_some_and(|(_, token)| token.kind == TokenKind::Colon)
            {
                children.push(CstElement::Node(self.parse_keyword_arg()));
            } else {
                children.push(CstElement::Node(self.parse_expr()));
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.expect(TokenKind::Comma, &mut children, "expected `,`");
        }

        CstNode::new(CstKind::ArgList, children, &self.tokens, start)
    }

    fn parse_keyword_arg(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Ident, &mut children, "expected keyword name");
        self.expect(
            TokenKind::Colon,
            &mut children,
            "expected `:` after keyword name",
        );
        children.push(CstElement::Node(self.parse_expr()));
        CstNode::new(CstKind::KeywordArg, children, &self.tokens, start)
    }

    fn expect(&mut self, kind: TokenKind, children: &mut Vec<CstElement>, message: &str) {
        if self.at(kind) {
            if let Some(index) = self.bump() {
                children.push(CstElement::Token(index));
            }
            return;
        }

        let span = self.current_span();
        self.diagnostics.push(Diagnostic::error(span, message));
    }

    fn at(&mut self, kind: TokenKind) -> bool {
        self.peek().is_some_and(|(_, token)| token.kind == kind)
    }

    fn at_end(&mut self) -> bool {
        self.peek().is_none()
    }

    fn bump(&mut self) -> Option<usize> {
        self.skip_trivia();
        let index = self.cursor;
        if index < self.tokens.len() {
            self.cursor += 1;
            Some(index)
        } else {
            None
        }
    }

    fn peek(&mut self) -> Option<(usize, Token)> {
        self.skip_trivia();
        self.tokens
            .get(self.cursor)
            .copied()
            .map(|token| (self.cursor, token))
    }

    fn peek_non_trivia(&self, ordinal: usize) -> Option<(usize, Token)> {
        let mut seen = 0;
        for (index, token) in self.tokens.iter().copied().enumerate().skip(self.cursor) {
            if token.kind.is_trivia() {
                continue;
            }
            if seen == ordinal {
                return Some((index, token));
            }
            seen += 1;
        }
        None
    }

    fn skip_trivia(&mut self) {
        while self
            .tokens
            .get(self.cursor)
            .is_some_and(|token| token.kind.is_trivia())
        {
            self.cursor += 1;
        }
    }

    fn current_span(&mut self) -> Span {
        self.peek().map_or_else(
            || Span::new(self.source.len(), self.source.len()),
            |(_, t)| t.span,
        )
    }

    fn error_at(&mut self, index: usize, message: &str) {
        self.diagnostics
            .push(Diagnostic::error(self.tokens[index].span, message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lex_preserves_trivia_and_keywords() {
        let tokens = lex("define function main() // ok\n  42\nend;");
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Whitespace)
        );
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Comment));
        assert_eq!(tokens[0].kind, TokenKind::Define);
        assert_eq!(tokens[2].kind, TokenKind::Function);
    }

    #[test]
    fn parse_function_with_let_and_call() {
        let tree = parse("define function main()\n  let x = 40;\n  add(x, 2)\nend;");
        assert_eq!(tree.diagnostics, Vec::new());
        let functions: Vec<_> = tree.root.child_nodes(CstKind::Function).collect();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].child_nodes(CstKind::Body).count(), 1);
        assert!(tree.tokens.iter().any(|token| token.kind == TokenKind::Let));
    }

    #[test]
    fn parse_class_generic_method_and_object_syntax() {
        let tree = parse(
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
        );
        assert_eq!(tree.diagnostics, Vec::new());
        assert_eq!(tree.root.child_nodes(CstKind::Class).count(), 1);
        assert_eq!(tree.root.child_nodes(CstKind::Generic).count(), 1);
        assert_eq!(tree.root.child_nodes(CstKind::Method).count(), 1);
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::TypeName)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::DoubleColon)
        );
        assert!(tree.tokens.iter().any(|token| token.kind == TokenKind::Dot));
    }
}
