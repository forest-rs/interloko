//! Lexer, lossless-enough token stream, and CST storage for grammar parsing.

use alloc::string::String;
use alloc::vec::Vec;

use crate::diagnostic::Diagnostic;
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
    /// `when`.
    When,
    /// `block`.
    Block,
    /// `cleanup`.
    Cleanup,
    /// `end`.
    End,
    /// User identifier.
    Ident,
    /// Base-10 numeric literal: an integer such as `42`, or a float with a
    /// fraction and/or exponent such as `1.5`, `2e10`, or `6.02e-23`.
    Number,
    /// Boolean literal, currently `#t` or `#f`.
    Bool,
    /// String literal.
    String,
    /// Symbol literal such as `#name`.
    Symbol,
    /// Legacy keyword-style symbol literal such as `#:name`.
    Keyword,
    /// Vector literal opener `#[`.
    VectorStart,
    /// Nominal type/class name such as `<circle>`.
    TypeName,
    /// `(`.
    LParen,
    /// `)`.
    RParen,
    /// `[`.
    LBracket,
    /// `]`.
    RBracket,
    /// `,`.
    Comma,
    /// `+`.
    Plus,
    /// `-`.
    Minus,
    /// `*`.
    Star,
    /// `/` (a lone slash; `//` starts a comment).
    Slash,
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
    /// `:=`.
    Assign,
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
    /// Documentation line comment trivia, written with `///`.
    DocComment,
    /// Unknown byte or character.
    Error,
}

impl TokenKind {
    pub(crate) fn is_trivia(self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment | Self::DocComment)
    }

    /// Returns true when this token can appear as a binary infix operator
    /// between two operands, such as `+` or `<=`.
    ///
    /// `-` is included even though it is also unary negation; callers
    /// asking this question are looking between two operands. The match is
    /// deliberately exhaustive so a new token kind must decide whether it
    /// is an infix operator.
    pub(crate) const fn is_infix_operator(self) -> bool {
        match self {
            Self::Plus
            | Self::Minus
            | Self::Star
            | Self::Slash
            | Self::Equal
            | Self::Less
            | Self::LessEqual
            | Self::Greater
            | Self::GreaterEqual => true,
            Self::Define
            | Self::Function
            | Self::Class
            | Self::Slot
            | Self::Constant
            | Self::Generic
            | Self::Method
            | Self::Let
            | Self::If
            | Self::Then
            | Self::Else
            | Self::Handle
            | Self::With
            | Self::Restart
            | Self::When
            | Self::Block
            | Self::Cleanup
            | Self::End
            | Self::Ident
            | Self::Number
            | Self::Bool
            | Self::String
            | Self::Symbol
            | Self::Keyword
            | Self::VectorStart
            | Self::TypeName
            | Self::LParen
            | Self::RParen
            | Self::LBracket
            | Self::RBracket
            | Self::Comma
            | Self::Arrow
            | Self::Colon
            | Self::Assign
            | Self::DoubleColon
            | Self::Dot
            | Self::Semicolon
            | Self::Whitespace
            | Self::Comment
            | Self::DocComment
            | Self::Error => false,
        }
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
    /// Constant definition.
    Constant,
    /// Class definition.
    Class,
    /// Slot declaration.
    SlotDecl,
    /// Slot option such as `init-keyword: radius:`.
    SlotOption,
    /// Class/type definition name, such as `<point>` in `define class`.
    TypeDefinitionName,
    /// Value definition name, such as `area` in `define function area`.
    ValueDefinitionName,
    /// Declared object slot name.
    SlotName,
    /// Keyword value used by a slot option, such as `radius:`.
    SlotKeyword,
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
    /// Function, method, or generic parameter name.
    ParameterName,
    /// Local binding name.
    LetName,
    /// Restart condition binding name.
    RestartName,
    /// Binary expression.
    BinaryExpr,
    /// Prefix unary expression.
    UnaryExpr,
    /// Direct function call expression.
    CallExpr,
    /// Direct call callee name.
    CallName,
    /// Object slot read expression.
    SlotExpr,
    /// Member name used after `.` in a slot read.
    MemberName,
    /// Object slot assignment expression.
    SlotAssignExpr,
    /// Call argument list.
    ArgList,
    /// Keyword argument.
    KeywordArg,
    /// Keyword introducer in an argument list, such as `radius:`.
    ArgumentKeyword,
    /// Name reference expression.
    NameExpr,
    /// Integer literal expression.
    NumberExpr,
    /// Boolean literal expression.
    BoolExpr,
    /// String literal expression.
    StringExpr,
    /// Symbol literal expression.
    SymbolExpr,
    /// Keyword symbol literal expression.
    KeywordExpr,
    /// Vector literal expression.
    VectorExpr,
    /// Conditional expression.
    IfExpr,
    /// Recoverable condition handler expression.
    HandleExpr,
    /// Anonymous function expression.
    FunctionExpr,
    /// Restart clause in a handler expression.
    RestartClause,
    /// Dylan-style `block` expression.
    BlockExpr,
    /// `cleanup` clause of a `block` expression.
    CleanupClause,
    /// Exit procedure name bound by a `block` expression.
    BlockExitName,
    /// `when … =>` guard of a restart clause.
    RestartGuard,
    /// Superclass type name in a class definition.
    SuperclassType,
    /// Nominal type name in an annotation.
    AnnotationType,
    /// Class-object type name in an annotation.
    ClassObjectAnnotationType,
    /// Bare condition type name in a restart clause.
    RestartConditionType,
    /// Type/class name expression.
    TypeExpr,
    /// Type name used as an expression.
    ExpressionTypeName,
    /// Malformed syntax placeholder.
    Error,
}

impl CstKind {
    /// Returns true when this kind is role metadata layered over syntax tokens.
    #[must_use]
    pub const fn is_role(self) -> bool {
        matches!(
            self,
            Self::TypeDefinitionName
                | Self::ValueDefinitionName
                | Self::SlotName
                | Self::SlotKeyword
                | Self::ParameterName
                | Self::LetName
                | Self::RestartName
                | Self::BlockExitName
                | Self::CallName
                | Self::MemberName
                | Self::ArgumentKeyword
                | Self::SuperclassType
                | Self::AnnotationType
                | Self::ClassObjectAnnotationType
                | Self::RestartConditionType
                | Self::ExpressionTypeName
        )
    }
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
    /// Creates a CST node from explicit children.
    ///
    /// This is used by parser frontends that construct the same CST contract
    /// as the built-in Rust parser from another recognition strategy.
    #[must_use]
    pub(crate) fn from_children(
        source_file: SourceFileId,
        kind: CstKind,
        children: Vec<CstElement>,
        tokens: &[Token],
        source: &str,
        fallback: Span,
    ) -> Self {
        Self::new(source_file, kind, children, tokens, source, fallback)
    }

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
        CstKind::Constant => {
            mix_definition_name(&mut builder, children, tokens, source, TokenKind::Constant);
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
        | CstKind::TypeDefinitionName
        | CstKind::ValueDefinitionName
        | CstKind::SlotName
        | CstKind::SlotKeyword
        | CstKind::LetStmt
        | CstKind::ParameterName
        | CstKind::LetName
        | CstKind::RestartName
        | CstKind::BlockExitName
        | CstKind::CallName
        | CstKind::MemberName
        | CstKind::KeywordArg
        | CstKind::ArgumentKeyword
        | CstKind::NameExpr
        | CstKind::NumberExpr
        | CstKind::BoolExpr
        | CstKind::StringExpr
        | CstKind::SymbolExpr
        | CstKind::KeywordExpr
        | CstKind::RestartClause
        | CstKind::SuperclassType
        | CstKind::AnnotationType
        | CstKind::ClassObjectAnnotationType
        | CstKind::RestartConditionType
        | CstKind::TypeExpr
        | CstKind::ExpressionTypeName => {
            mix_first_named_token(&mut builder, children, tokens, source);
        }
        _ => {
            for child in children {
                match child {
                    CstElement::Node(node) if !node.kind.is_role() => {
                        builder.mix_u64(node.id.raw());
                    }
                    CstElement::Node(_) => {}
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
                TokenKind::Ident
                    | TokenKind::Number
                    | TokenKind::TypeName
                    | TokenKind::Bool
                    | TokenKind::String
                    | TokenKind::Symbol
                    | TokenKind::Keyword
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
        CstKind::SlotAssignExpr => 29,
        CstKind::Constant => 30,
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
        CstKind::StringExpr => 24,
        CstKind::SymbolExpr => 25,
        CstKind::KeywordExpr => 26,
        CstKind::VectorExpr => 27,
        CstKind::UnaryExpr => 28,
        CstKind::FunctionExpr => 31,
        CstKind::SlotKeyword => 32,
        CstKind::ArgumentKeyword => 33,
        CstKind::TypeDefinitionName => 34,
        CstKind::ValueDefinitionName => 35,
        CstKind::SlotName => 36,
        CstKind::ParameterName => 37,
        CstKind::LetName => 38,
        CstKind::RestartName => 39,
        CstKind::CallName => 40,
        CstKind::MemberName => 41,
        CstKind::SuperclassType => 42,
        CstKind::AnnotationType => 43,
        CstKind::ClassObjectAnnotationType => 44,
        CstKind::RestartConditionType => 45,
        CstKind::ExpressionTypeName => 46,
        CstKind::BlockExpr => 47,
        CstKind::CleanupClause => 48,
        CstKind::BlockExitName => 49,
        CstKind::RestartGuard => 50,
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
        TokenKind::Assign => 45,
        TokenKind::DoubleColon => 21,
        TokenKind::Dot => 22,
        TokenKind::Semicolon => 23,
        TokenKind::Whitespace => 24,
        TokenKind::Comment => 25,
        TokenKind::DocComment => 46,
        TokenKind::Slash => 47,
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
        TokenKind::String => 39,
        TokenKind::Symbol => 40,
        TokenKind::Keyword => 41,
        TokenKind::LBracket => 42,
        TokenKind::RBracket => 43,
        TokenKind::VectorStart => 44,
        TokenKind::Block => 48,
        TokenKind::Cleanup => 49,
        TokenKind::When => 50,
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
            b'[' => {
                pos += 1;
                TokenKind::LBracket
            }
            b']' => {
                pos += 1;
                TokenKind::RBracket
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
            b'/' if bytes.get(pos + 1) != Some(&b'/') => {
                pos += 1;
                TokenKind::Slash
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
            b':' if bytes.get(pos + 1) == Some(&b'=') => {
                pos += 2;
                TokenKind::Assign
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
            b'"' => {
                pos += 1;
                while pos < bytes.len() {
                    match bytes[pos] {
                        b'\\' => {
                            pos += 1;
                            if pos < bytes.len() {
                                pos += 1;
                            }
                        }
                        b'"' => {
                            pos += 1;
                            break;
                        }
                        _ => pos += 1,
                    }
                }
                TokenKind::String
            }
            b'#' if bytes.get(pos + 1) == Some(&b'[') => {
                pos += 2;
                TokenKind::VectorStart
            }
            b'#' if matches!(bytes.get(pos + 1), Some(b't' | b'f'))
                && bytes
                    .get(pos + 2)
                    .is_none_or(|_| !is_ident_continue(bytes, pos + 2)) =>
            {
                pos += 2;
                TokenKind::Bool
            }
            b'#' if bytes.get(pos + 1) == Some(&b':')
                && bytes.get(pos + 2).is_some_and(|next| is_ident_start(*next)) =>
            {
                pos += 3;
                while pos < bytes.len() && is_ident_continue(bytes, pos) {
                    pos += 1;
                }
                TokenKind::Keyword
            }
            b'#' if bytes.get(pos + 1).is_some_and(|next| is_ident_start(*next)) => {
                pos += 2;
                while pos < bytes.len() && is_ident_continue(bytes, pos) {
                    pos += 1;
                }
                TokenKind::Symbol
            }
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                let kind = if bytes.get(pos + 2) == Some(&b'/') {
                    TokenKind::DocComment
                } else {
                    TokenKind::Comment
                };
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                kind
            }
            b if b.is_ascii_whitespace() => {
                pos += 1;
                while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                TokenKind::Whitespace
            }
            b if b.is_ascii_digit() => {
                pos = lex_number_end(bytes, pos);
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
                    "when" => TokenKind::When,
                    "block" => TokenKind::Block,
                    "cleanup" => TokenKind::Cleanup,
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

/// Returns the end of the numeric literal starting at `start`.
///
/// Mirrors the base grammar's `number` rule: digits, then an optional
/// fraction that must have digits on both sides of the `.` (so `x.radius`
/// style postfix access after a number never becomes part of the literal),
/// then an optional exponent that must contain at least one digit.
fn lex_number_end(bytes: &[u8], start: usize) -> usize {
    let digits_end = |mut pos: usize| {
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        pos
    };
    let mut pos = digits_end(start);
    if bytes.get(pos) == Some(&b'.') && bytes.get(pos + 1).is_some_and(u8::is_ascii_digit) {
        pos = digits_end(pos + 1);
    }
    if matches!(bytes.get(pos), Some(b'e' | b'E')) {
        let mut exponent = pos + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            pos = digits_end(exponent);
        }
    }
    pos
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
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

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn lex_preserves_trivia_and_keywords() {
        let tokens = lex("define function main() // ok\n  42\nend;");
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Whitespace)
        );
        assert!(tokens.iter().any(|token| token.kind == TokenKind::Comment));
        assert!(
            lex("/// docs\n")
                .iter()
                .any(|token| token.kind == TokenKind::DocComment)
        );
        assert_eq!(tokens[0].kind, TokenKind::Define);
        assert_eq!(tokens[2].kind, TokenKind::Function);
    }

    #[test]
    fn lex_numbers_take_fractions_and_exponents_only_when_complete() {
        let numbers = |source: &str| {
            lex(source)
                .into_iter()
                .filter(|token| token.kind == TokenKind::Number)
                .map(|token| token.text(source).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            numbers("42 1.5 2e10 6.02E-23 7e+2"),
            ["42", "1.5", "2e10", "6.02E-23", "7e+2"]
        );
        assert_eq!(numbers("1.x"), ["1"]);
        assert_eq!(numbers("3.e4"), ["3"]);
        assert_eq!(numbers("4e"), ["4"]);
        assert_eq!(numbers("5e-x"), ["5"]);
    }
}
