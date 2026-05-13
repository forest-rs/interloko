//! Lexer, lossless-enough token stream, and milestone-0 CST parser.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::{SourceFileId, StableIdBuilder, SyntaxNodeId};

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
    /// `constant`.
    Constant,
    /// `generic`.
    Generic,
    /// `method`.
    Method,
    /// `let`.
    Let,
    /// `if`.
    If,
    /// `then`.
    Then,
    /// `else`.
    Else,
    /// `handle`.
    Handle,
    /// `with`.
    With,
    /// `restart`.
    Restart,
    /// `end`.
    End,
    /// User identifier.
    Ident,
    /// Base-10 integer literal.
    Number,
    /// Boolean literal, currently `#t` or `#f`.
    Bool,
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
    /// `=>`.
    Arrow,
    /// `<`.
    Less,
    /// `<=`.
    LessEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterEqual,
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
    /// Slot option such as `init-keyword: radius:`.
    SlotOption,
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
    /// Boolean literal expression.
    BoolExpr,
    /// Conditional expression.
    IfExpr,
    /// Recoverable condition handler expression.
    HandleExpr,
    /// Restart clause in a handler expression.
    RestartClause,
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
    /// Stable-ish node identity.
    pub id: SyntaxNodeId,
    /// Node kind.
    pub kind: CstKind,
    /// Source span covered by this node.
    pub span: Span,
    /// Child nodes and token references.
    pub children: Vec<CstElement>,
}

impl CstNode {
    fn new(
        source_file: SourceFileId,
        kind: CstKind,
        children: Vec<CstElement>,
        tokens: &[Token],
        source: &str,
        fallback: Span,
    ) -> Self {
        let span = children
            .iter()
            .map(|child| child.span(tokens))
            .reduce(Span::cover)
            .unwrap_or(fallback);
        let id = syntax_node_id(source_file, kind, &children, tokens, source);
        Self {
            id,
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

fn syntax_node_id(
    source_file: SourceFileId,
    kind: CstKind,
    children: &[CstElement],
    tokens: &[Token],
    source: &str,
) -> SyntaxNodeId {
    let mut builder = SyntaxNodeId::builder(source_file, cst_kind_tag(kind));

    match kind {
        CstKind::Class => {
            mix_token_text(&mut builder, children, tokens, source, TokenKind::TypeName);
        }
        CstKind::Function => {
            mix_definition_name(&mut builder, children, tokens, source, TokenKind::Function);
        }
        CstKind::Generic => {
            mix_definition_name(&mut builder, children, tokens, source, TokenKind::Generic);
        }
        CstKind::Method => {
            mix_definition_name(&mut builder, children, tokens, source, TokenKind::Method);
            mix_token_text(&mut builder, children, tokens, source, TokenKind::TypeName);
        }
        CstKind::SlotDecl
        | CstKind::SlotOption
        | CstKind::LetStmt
        | CstKind::KeywordArg
        | CstKind::NameExpr
        | CstKind::NumberExpr
        | CstKind::BoolExpr
        | CstKind::RestartClause
        | CstKind::TypeExpr => {
            mix_first_named_token(&mut builder, children, tokens, source);
        }
        _ => {
            for child in children {
                match child {
                    CstElement::Node(node) => builder.mix_u64(node.id.raw()),
                    CstElement::Token(index) => {
                        let token = tokens[*index];
                        if token.kind.is_trivia() {
                            continue;
                        }
                        builder.mix_u64(token_kind_tag(token.kind));
                        builder.mix_str(token.text(source));
                    }
                }
            }
        }
    }

    SyntaxNodeId::from_raw(builder.finish())
}

fn mix_definition_name(
    builder: &mut StableIdBuilder,
    children: &[CstElement],
    tokens: &[Token],
    source: &str,
    keyword: TokenKind,
) {
    let mut saw_keyword = false;
    for child in children {
        let CstElement::Token(index) = child else {
            continue;
        };
        let token = tokens[*index];
        if token.kind == keyword {
            saw_keyword = true;
        } else if saw_keyword && matches!(token.kind, TokenKind::Ident | TokenKind::TypeName) {
            builder.mix_str(token.text(source));
            return;
        }
    }
}

fn mix_token_text(
    builder: &mut StableIdBuilder,
    children: &[CstElement],
    tokens: &[Token],
    source: &str,
    kind: TokenKind,
) {
    for child in children {
        if let CstElement::Token(index) = child {
            let token = tokens[*index];
            if token.kind == kind {
                builder.mix_str(token.text(source));
                return;
            }
        }
    }
}

fn mix_first_named_token(
    builder: &mut StableIdBuilder,
    children: &[CstElement],
    tokens: &[Token],
    source: &str,
) {
    for child in children {
        if let CstElement::Token(index) = child {
            let token = tokens[*index];
            if matches!(
                token.kind,
                TokenKind::Ident | TokenKind::Number | TokenKind::TypeName | TokenKind::Bool
            ) {
                builder.mix_str(token.text(source));
                return;
            }
        }
    }
}

const fn cst_kind_tag(kind: CstKind) -> u64 {
    match kind {
        CstKind::Root => 1,
        CstKind::Function => 2,
        CstKind::Class => 3,
        CstKind::SlotDecl => 4,
        CstKind::Generic => 5,
        CstKind::Method => 6,
        CstKind::ParameterList => 7,
        CstKind::Body => 8,
        CstKind::LetStmt => 9,
        CstKind::BinaryExpr => 10,
        CstKind::CallExpr => 11,
        CstKind::SlotExpr => 12,
        CstKind::ArgList => 13,
        CstKind::KeywordArg => 14,
        CstKind::NameExpr => 15,
        CstKind::NumberExpr => 16,
        CstKind::TypeExpr => 17,
        CstKind::Error => 18,
        CstKind::BoolExpr => 19,
        CstKind::IfExpr => 20,
        CstKind::HandleExpr => 21,
        CstKind::RestartClause => 22,
        CstKind::SlotOption => 23,
    }
}

const fn token_kind_tag(kind: TokenKind) -> u64 {
    match kind {
        TokenKind::Define => 1,
        TokenKind::Function => 2,
        TokenKind::Class => 3,
        TokenKind::Slot => 4,
        TokenKind::Generic => 5,
        TokenKind::Method => 6,
        TokenKind::Let => 7,
        TokenKind::End => 8,
        TokenKind::Ident => 9,
        TokenKind::Number => 10,
        TokenKind::TypeName => 11,
        TokenKind::LParen => 12,
        TokenKind::RParen => 13,
        TokenKind::Comma => 14,
        TokenKind::Plus => 15,
        TokenKind::Minus => 16,
        TokenKind::Star => 17,
        TokenKind::Equal => 18,
        TokenKind::Arrow => 19,
        TokenKind::Colon => 20,
        TokenKind::DoubleColon => 21,
        TokenKind::Dot => 22,
        TokenKind::Semicolon => 23,
        TokenKind::Whitespace => 24,
        TokenKind::Comment => 25,
        TokenKind::Error => 26,
        TokenKind::If => 27,
        TokenKind::Then => 28,
        TokenKind::Else => 29,
        TokenKind::Bool => 30,
        TokenKind::Less => 31,
        TokenKind::LessEqual => 32,
        TokenKind::Greater => 33,
        TokenKind::GreaterEqual => 34,
        TokenKind::Handle => 35,
        TokenKind::With => 36,
        TokenKind::Restart => 37,
        TokenKind::Constant => 38,
    }
}

/// Parsed source, retaining the original source and all tokens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxTree {
    /// Source file identity.
    pub source_file: SourceFileId,
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
            b'=' if bytes.get(pos + 1) == Some(&b'>') => {
                pos += 2;
                TokenKind::Arrow
            }
            b'=' => {
                pos += 1;
                TokenKind::Equal
            }
            b'<' if bytes.get(pos + 1) == Some(&b'=') => {
                pos += 2;
                TokenKind::LessEqual
            }
            b'<' if lex_type_name_end(bytes, pos).is_some() => {
                pos = lex_type_name_end(bytes, pos).expect("type name end was just checked");
                TokenKind::TypeName
            }
            b'<' => {
                pos += 1;
                TokenKind::Less
            }
            b'>' if bytes.get(pos + 1) == Some(&b'=') => {
                pos += 2;
                TokenKind::GreaterEqual
            }
            b'>' => {
                pos += 1;
                TokenKind::Greater
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
            b'#' if matches!(bytes.get(pos + 1), Some(b't' | b'f')) => {
                pos += 2;
                TokenKind::Bool
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
                    "constant" => TokenKind::Constant,
                    "generic" => TokenKind::Generic,
                    "method" => TokenKind::Method,
                    "let" => TokenKind::Let,
                    "if" => TokenKind::If,
                    "then" => TokenKind::Then,
                    "else" => TokenKind::Else,
                    "handle" => TokenKind::Handle,
                    "with" => TokenKind::With,
                    "restart" => TokenKind::Restart,
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

fn lex_type_name_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start + 1;
    let first = *bytes.get(pos)?;
    if !is_ident_start(first) {
        return None;
    }
    pos += 1;
    while pos < bytes.len()
        && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' || bytes[pos] == b'-')
    {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'>') {
        Some(pos + 1)
    } else {
        None
    }
}

fn is_ident_continue(bytes: &[u8], pos: usize) -> bool {
    let byte = bytes[pos];
    byte.is_ascii_alphanumeric()
        || byte == b'_'
        || byte == b'?'
        || (byte == b'-' && bytes.get(pos + 1).is_some_and(|next| is_ident_start(*next)))
}

/// Parses source into a [`SyntaxTree`].
#[must_use]
pub fn parse(source: &str) -> SyntaxTree {
    parse_file(SourceFileId::ANONYMOUS, source)
}

/// Parses source from `source_file` into a [`SyntaxTree`].
#[must_use]
pub fn parse_file(source_file: SourceFileId, source: &str) -> SyntaxTree {
    let tokens = lex(source);
    Parser::new(source_file, source, tokens).parse()
}

struct Parser<'source> {
    source_file: SourceFileId,
    source: &'source str,
    tokens: Vec<Token>,
    cursor: usize,
    diagnostics: Vec<Diagnostic>,
}

impl<'source> Parser<'source> {
    fn new(source_file: SourceFileId, source: &'source str, tokens: Vec<Token>) -> Self {
        Self {
            source_file,
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
                    self.error_at(index, DiagnosticCode::SyntaxExpectedToken, "definition");
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
                    self.error_at(
                        index,
                        DiagnosticCode::SyntaxExpectedToken,
                        "definition kind after `define`",
                    );
                    children.push(CstElement::Token(index));
                }
                _ => {}
            }
        }

        let fallback = Span::new(0, self.source.len());
        let root = self.node(CstKind::Root, children, fallback);
        SyntaxTree {
            source_file: self.source_file,
            source: self.source.to_string(),
            tokens: self.tokens,
            root,
            diagnostics: self.diagnostics,
        }
    }

    fn node(&self, kind: CstKind, children: Vec<CstElement>, fallback: Span) -> CstNode {
        CstNode::new(
            self.source_file,
            kind,
            children,
            &self.tokens,
            self.source,
            fallback,
        )
    }

    fn parse_class(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "`define`");
        self.expect(TokenKind::Class, &mut children, "`class`");
        self.expect(TokenKind::TypeName, &mut children, "class name");
        if self.at(TokenKind::LParen) {
            self.expect(TokenKind::LParen, &mut children, "`(`");
            self.expect(TokenKind::TypeName, &mut children, "superclass name");
            self.expect(TokenKind::RParen, &mut children, "`)`");
        }
        while !self.at(TokenKind::End) && !self.at_end() {
            if self.at(TokenKind::Slot) || self.at(TokenKind::Constant) {
                children.push(CstElement::Node(self.parse_slot_decl()));
            } else if let Some(index) = self.bump() {
                self.error_at(
                    index,
                    DiagnosticCode::SyntaxExpectedToken,
                    "`slot`, `constant slot`, or `end`",
                );
                children.push(CstElement::Token(index));
            }
        }
        self.expect(TokenKind::End, &mut children, "`end` after class body");
        if self.at(TokenKind::Semicolon) {
            self.expect(TokenKind::Semicolon, &mut children, "`;`");
        }
        self.node(CstKind::Class, children, start)
    }

    fn parse_slot_decl(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        if self.at(TokenKind::Constant) {
            self.expect(TokenKind::Constant, &mut children, "`constant`");
        }
        self.expect(TokenKind::Slot, &mut children, "`slot`");
        self.expect(TokenKind::Ident, &mut children, "slot name");
        if self.at(TokenKind::DoubleColon) {
            self.expect(
                TokenKind::DoubleColon,
                &mut children,
                "`::` after slot name",
            );
            self.expect(TokenKind::TypeName, &mut children, "slot type");
        }
        while self.at(TokenKind::Comma) {
            self.expect(TokenKind::Comma, &mut children, "`,`");
            children.push(CstElement::Node(self.parse_slot_option()));
        }
        self.expect(
            TokenKind::Semicolon,
            &mut children,
            "`;` after slot declaration",
        );
        self.node(CstKind::SlotDecl, children, start)
    }

    fn parse_slot_option(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        let option = self.peek().map(|(_, token)| token);
        self.expect(TokenKind::Ident, &mut children, "slot option name");
        self.expect(
            TokenKind::Colon,
            &mut children,
            "`:` after slot option name",
        );
        if option.is_some_and(|token| token.text(self.source) == "init-value") {
            children.push(CstElement::Node(self.parse_expr()));
            return self.node(CstKind::SlotOption, children, start);
        }
        self.expect(TokenKind::Ident, &mut children, "slot keyword name");
        self.expect(
            TokenKind::Colon,
            &mut children,
            "`:` after slot keyword name",
        );
        self.node(CstKind::SlotOption, children, start)
    }

    fn parse_generic(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "`define`");
        self.expect(TokenKind::Generic, &mut children, "`generic`");
        self.expect(TokenKind::Ident, &mut children, "generic name");
        self.expect(TokenKind::LParen, &mut children, "`(` after generic name");
        children.push(CstElement::Node(self.parse_parameter_list()));
        self.expect(TokenKind::RParen, &mut children, "`)` after parameters");
        if self.at(TokenKind::Arrow) {
            self.expect(TokenKind::Arrow, &mut children, "`=>`");
            self.parse_type_annotation(&mut children, "generic return type");
        }
        self.expect(
            TokenKind::Semicolon,
            &mut children,
            "`;` after generic declaration",
        );
        self.node(CstKind::Generic, children, start)
    }

    fn parse_method(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "`define`");
        self.expect(TokenKind::Method, &mut children, "`method`");
        self.expect(TokenKind::Ident, &mut children, "method name");
        self.expect(TokenKind::LParen, &mut children, "`(` after method name");
        children.push(CstElement::Node(self.parse_parameter_list()));
        self.expect(TokenKind::RParen, &mut children, "`)` after parameters");
        if self.at(TokenKind::Arrow) {
            self.expect(TokenKind::Arrow, &mut children, "`=>`");
            self.parse_type_annotation(&mut children, "method return type");
        }
        children.push(CstElement::Node(self.parse_body()));
        self.expect(TokenKind::End, &mut children, "`end` after method body");
        if self.at(TokenKind::Semicolon) {
            self.expect(TokenKind::Semicolon, &mut children, "`;`");
        }
        self.node(CstKind::Method, children, start)
    }

    fn parse_function(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Define, &mut children, "`define`");
        self.expect(TokenKind::Function, &mut children, "`function`");
        self.expect(TokenKind::Ident, &mut children, "function name");
        self.expect(TokenKind::LParen, &mut children, "`(` after function name");
        children.push(CstElement::Node(self.parse_parameter_list()));
        self.expect(TokenKind::RParen, &mut children, "`)` after parameters");
        if self.at(TokenKind::Arrow) {
            self.expect(TokenKind::Arrow, &mut children, "`=>`");
            self.parse_type_annotation(&mut children, "function return type");
        }
        children.push(CstElement::Node(self.parse_body()));
        self.expect(TokenKind::End, &mut children, "`end` after function body");
        if self.at(TokenKind::Semicolon) {
            self.expect(TokenKind::Semicolon, &mut children, "`;`");
        }
        self.node(CstKind::Function, children, start)
    }

    fn parse_parameter_list(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        if self.at(TokenKind::RParen) {
            return self.node(CstKind::ParameterList, children, start);
        }

        loop {
            self.expect(TokenKind::Ident, &mut children, "parameter name");
            if self.at(TokenKind::DoubleColon) {
                self.expect(TokenKind::DoubleColon, &mut children, "`::`");
                self.parse_type_annotation(&mut children, "parameter type");
            }
            if !self.at(TokenKind::Comma) {
                break;
            }
            self.expect(TokenKind::Comma, &mut children, "`,`");
        }

        self.node(CstKind::ParameterList, children, start)
    }

    fn parse_type_annotation(&mut self, children: &mut Vec<CstElement>, message: &str) {
        if self.at(TokenKind::Class) {
            self.expect(TokenKind::Class, children, "`class`");
        }
        self.expect(TokenKind::TypeName, children, message);
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
            self.diagnostics.push(Diagnostic::error(
                span,
                DiagnosticCode::SyntaxExpectedExpression,
            ));
        }

        self.node(CstKind::Body, children, start)
    }

    fn parse_let(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Let, &mut children, "`let`");
        self.expect(TokenKind::Ident, &mut children, "local name");
        self.expect(TokenKind::Equal, &mut children, "`=` after local name");
        children.push(CstElement::Node(self.parse_expr()));
        self.expect(
            TokenKind::Semicolon,
            &mut children,
            "`;` after local binding",
        );
        self.node(CstKind::LetStmt, children, start)
    }

    fn parse_expr(&mut self) -> CstNode {
        self.parse_handle()
    }

    fn parse_handle(&mut self) -> CstNode {
        let start = self.current_span();
        if !self.at(TokenKind::Handle) {
            return self.parse_if();
        }

        let mut children = Vec::new();
        self.expect(TokenKind::Handle, &mut children, "`handle`");
        children.push(CstElement::Node(self.parse_expr()));
        self.expect(TokenKind::With, &mut children, "`with`");
        children.push(CstElement::Node(self.parse_restart_clause()));
        self.expect(TokenKind::End, &mut children, "`end` after `handle`");
        self.node(CstKind::HandleExpr, children, start)
    }

    fn parse_restart_clause(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Restart, &mut children, "`restart`");
        if self.at(TokenKind::Ident)
            && self
                .peek_non_trivia(1)
                .is_some_and(|(_, token)| token.kind == TokenKind::DoubleColon)
        {
            self.expect(TokenKind::Ident, &mut children, "restart condition name");
            self.expect(
                TokenKind::DoubleColon,
                &mut children,
                "`::` after restart condition name",
            );
        }
        self.expect(
            TokenKind::TypeName,
            &mut children,
            "restart condition class",
        );
        children.push(CstElement::Node(self.parse_expr()));
        self.node(CstKind::RestartClause, children, start)
    }

    fn parse_if(&mut self) -> CstNode {
        let start = self.current_span();
        if !self.at(TokenKind::If) {
            return self.parse_compare();
        }

        let mut children = Vec::new();
        self.expect(TokenKind::If, &mut children, "`if`");
        children.push(CstElement::Node(self.parse_expr()));
        self.expect(TokenKind::Then, &mut children, "`then`");
        children.push(CstElement::Node(self.parse_expr()));
        self.expect(TokenKind::Else, &mut children, "`else`");
        children.push(CstElement::Node(self.parse_expr()));
        self.expect(TokenKind::End, &mut children, "`end` after `if`");
        self.node(CstKind::IfExpr, children, start)
    }

    fn parse_compare(&mut self) -> CstNode {
        let mut node = self.parse_add();
        while matches!(
            self.peek().map(|(_, token)| token.kind),
            Some(
                TokenKind::Equal
                    | TokenKind::Less
                    | TokenKind::LessEqual
                    | TokenKind::Greater
                    | TokenKind::GreaterEqual
            )
        ) {
            let mut children = vec![CstElement::Node(node)];
            if let Some(op) = self.bump() {
                children.push(CstElement::Token(op));
            }
            children.push(CstElement::Node(self.parse_add()));
            node = self.node(CstKind::BinaryExpr, children, Span::default());
        }
        node
    }

    fn parse_add(&mut self) -> CstNode {
        let mut node = self.parse_mul();
        while self.at(TokenKind::Plus) || self.at(TokenKind::Minus) {
            let mut children = vec![CstElement::Node(node)];
            if let Some(op) = self.bump() {
                children.push(CstElement::Token(op));
            }
            children.push(CstElement::Node(self.parse_mul()));
            node = self.node(CstKind::BinaryExpr, children, Span::default());
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
            node = self.node(CstKind::BinaryExpr, children, Span::default());
        }
        node
    }

    fn parse_postfix(&mut self) -> CstNode {
        let mut node = self.parse_primary();
        while self.at(TokenKind::Dot) {
            let start = node.span;
            let mut children = vec![CstElement::Node(node)];
            self.expect(TokenKind::Dot, &mut children, "`.`");
            self.expect(TokenKind::Ident, &mut children, "slot name");
            node = self.node(CstKind::SlotExpr, children, start);
        }
        node
    }

    fn parse_primary(&mut self) -> CstNode {
        let start = self.current_span();
        if self.at(TokenKind::Number) {
            let mut children = Vec::new();
            self.expect(TokenKind::Number, &mut children, "number");
            return self.node(CstKind::NumberExpr, children, start);
        }

        if self.at(TokenKind::Bool) {
            let mut children = Vec::new();
            self.expect(TokenKind::Bool, &mut children, "boolean literal");
            return self.node(CstKind::BoolExpr, children, start);
        }

        if self.at(TokenKind::TypeName) {
            let mut children = Vec::new();
            self.expect(TokenKind::TypeName, &mut children, "type name");
            return self.node(CstKind::TypeExpr, children, start);
        }

        if self.at(TokenKind::Ident) {
            let Some(name) = self.bump() else {
                return self.node(CstKind::Error, Vec::new(), start);
            };
            if self.at(TokenKind::LParen) {
                let mut children = vec![CstElement::Token(name)];
                self.expect(TokenKind::LParen, &mut children, "`(`");
                children.push(CstElement::Node(self.parse_arg_list()));
                self.expect(TokenKind::RParen, &mut children, "`)` after arguments");
                return self.node(CstKind::CallExpr, children, start);
            }
            return self.node(CstKind::NameExpr, vec![CstElement::Token(name)], start);
        }

        if self.at(TokenKind::LParen) {
            let _ = self.bump();
            let node = self.parse_expr();
            let mut ignored = Vec::new();
            self.expect(TokenKind::RParen, &mut ignored, "`)`");
            return node;
        }

        if let Some(index) = self.bump() {
            self.error_at(
                index,
                DiagnosticCode::SyntaxExpectedExpression,
                "expression",
            );
            return self.node(CstKind::Error, vec![CstElement::Token(index)], start);
        }

        self.diagnostics.push(Diagnostic::error(
            start,
            DiagnosticCode::SyntaxExpectedExpression,
        ));
        self.node(CstKind::Error, Vec::new(), start)
    }

    fn parse_arg_list(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        if self.at(TokenKind::RParen) {
            return self.node(CstKind::ArgList, children, start);
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
            self.expect(TokenKind::Comma, &mut children, "`,`");
        }

        self.node(CstKind::ArgList, children, start)
    }

    fn parse_keyword_arg(&mut self) -> CstNode {
        let start = self.current_span();
        let mut children = Vec::new();
        self.expect(TokenKind::Ident, &mut children, "keyword name");
        self.expect(TokenKind::Colon, &mut children, "`:` after keyword name");
        children.push(CstElement::Node(self.parse_expr()));
        self.node(CstKind::KeywordArg, children, start)
    }

    fn expect(&mut self, kind: TokenKind, children: &mut Vec<CstElement>, message: &str) {
        if self.at(kind) {
            if let Some(index) = self.bump() {
                children.push(CstElement::Token(index));
            }
            return;
        }

        let span = self.current_span();
        self.diagnostics.push(
            Diagnostic::error(span, DiagnosticCode::SyntaxExpectedToken)
                .with_arg(DiagnosticArg::text("expected", message)),
        );
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

    fn error_at(&mut self, index: usize, code: DiagnosticCode, message: &str) {
        let mut diagnostic = Diagnostic::error(self.tokens[index].span, code);
        if code == DiagnosticCode::SyntaxExpectedToken {
            diagnostic = diagnostic.with_arg(DiagnosticArg::text("expected", message));
        }
        self.diagnostics.push(diagnostic);
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
        let tree = parse(
            "define function main(klass :: class <circle>, x :: <integer>) => <integer>\n  let y = 40;\n  add(y, x)\nend;",
        );
        assert_eq!(tree.diagnostics, Vec::new());
        let functions: Vec<_> = tree.root.child_nodes(CstKind::Function).collect();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].child_nodes(CstKind::Body).count(), 1);
        assert!(tree.tokens.iter().any(|token| token.kind == TokenKind::Let));
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Arrow)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Class)
        );
    }

    #[test]
    fn parse_class_generic_method_and_object_syntax() {
        let tree = parse(
            "define class <circle> (<object>)\n  slot radius :: <integer>, required-init-keyword: radius:;\n  slot valid? :: <boolean>, init-keyword: enabled?:;\n  constant slot cached :: <integer>, init-value: 5;\nend;\n\
             define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <circle>) => <integer>\n  c.radius * c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10, enabled?: #t))\nend;",
        );
        assert_eq!(tree.diagnostics, Vec::new());
        assert_eq!(tree.root.child_nodes(CstKind::Class).count(), 1);
        assert_eq!(
            tree.root
                .child_nodes(CstKind::Class)
                .next()
                .expect("class should parse")
                .child_nodes(CstKind::SlotDecl)
                .flat_map(|slot| slot.child_nodes(CstKind::SlotOption))
                .count(),
            3
        );
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
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.text(&tree.source) == "valid?")
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Constant)
        );
        assert!(tree.tokens.iter().any(|token| token.kind == TokenKind::Dot));
    }

    #[test]
    fn parse_booleans_comparisons_and_if_expression() {
        let tree = parse("define function main(x)\n  if x <= 10 then #t else x > 20 end\nend;");
        assert_eq!(tree.diagnostics, Vec::new());
        let body = tree
            .root
            .child_nodes(CstKind::Function)
            .next()
            .and_then(|function| function.child_nodes(CstKind::Body).next())
            .expect("function body should parse");

        assert_eq!(body.child_nodes(CstKind::IfExpr).count(), 1);
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Bool)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::LessEqual)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Greater)
        );
    }

    #[test]
    fn parse_handle_restart_expression() {
        let tree = parse(
            "define function main()\n  handle signal(make(<bad>, fallback: 0)) with restart e :: <bad> e.fallback end\nend;",
        );
        assert_eq!(tree.diagnostics, Vec::new());
        let body = tree
            .root
            .child_nodes(CstKind::Function)
            .next()
            .and_then(|function| function.child_nodes(CstKind::Body).next())
            .expect("function body should parse");

        let handle = body
            .child_nodes(CstKind::HandleExpr)
            .next()
            .expect("handle expression should parse");
        assert_eq!(handle.child_nodes(CstKind::RestartClause).count(), 1);
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Handle)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::With)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::Restart)
        );
        assert!(
            tree.tokens
                .iter()
                .any(|token| token.kind == TokenKind::DoubleColon)
        );
    }
}
