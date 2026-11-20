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
///
/// Each variant's discriminant is its stable tag, mixed into syntax node
/// ids; `repr(u8)` fixes its type. Tags never change once
/// given and are never reused, so a new variant takes one past the highest
/// tag. Tag 28 (`then`) is retired. The compiler rejects two variants with the same tag.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum TokenKind {
    /// `define`.
    Define = 1,
    /// `function`.
    Function = 2,
    /// `class`.
    Class = 3,
    /// `slot`.
    Slot = 4,
    /// `constant`.
    Constant = 38,
    /// `generic`.
    Generic = 5,
    /// `method`.
    Method = 6,
    /// `let`.
    Let = 7,
    /// `if`.
    If = 27,
    /// `elseif`.
    Elseif = 54,
    /// `else`.
    Else = 29,
    /// `unless`.
    Unless = 55,
    /// `case`.
    Case = 56,
    /// `otherwise`.
    Otherwise = 57,
    /// `select`.
    Select = 61,
    /// `handle`.
    Handle = 35,
    /// `with`.
    With = 36,
    /// `restart`.
    Restart = 37,
    /// `when`.
    When = 50,
    /// `block`.
    Block = 48,
    /// `begin`.
    Begin = 62,
    /// `cleanup`.
    Cleanup = 49,
    /// `exception`.
    Exception = 71,
    /// `while`.
    While = 60,
    /// `until`.
    Until = 59,
    /// `for`.
    For = 64,
    /// `end`.
    End = 8,
    /// User identifier.
    Ident = 9,
    /// Base-10 numeric literal: an integer such as `42`, or a float with a
    /// fraction and/or exponent such as `1.5`, `2e10`, or `6.02e-23`,
    /// optionally followed by a 32-bit type suffix: `7i32`, `7u32`,
    /// `1.5f32`.
    Number = 10,
    /// Boolean literal, currently `#t` or `#f`.
    Bool = 30,
    /// String literal.
    String = 39,
    /// Character literal such as `'a'` or `'\n'`.
    Character = 63,
    /// Symbol literal such as `#name`.
    Symbol = 40,
    /// Legacy keyword-style symbol literal such as `#:name`.
    Keyword = 41,
    /// Vector literal opener `#[`.
    VectorStart = 44,
    /// List literal opener `#(`.
    ListStart = 72,
    /// Nominal type/class name such as `<circle>`.
    TypeName = 11,
    /// `(`.
    LParen = 12,
    /// `)`.
    RParen = 13,
    /// `[`.
    LBracket = 42,
    /// `]`.
    RBracket = 43,
    /// `,`.
    Comma = 14,
    /// `+`.
    Plus = 15,
    /// `-`.
    Minus = 16,
    /// `*`.
    Star = 17,
    /// `/` (a lone slash; `//` starts a comment).
    Slash = 47,
    /// `&`, short-circuit conjunction.
    Ampersand = 65,
    /// `|`, short-circuit disjunction.
    Bar = 66,
    /// `^`, exponentiation.
    Caret = 67,
    /// `~`, logical negation.
    Tilde = 68,
    /// `~=`, numeric or `=` inequality.
    TildeEqual = 69,
    /// `~==`, non-identity.
    TildeEqualEqual = 70,
    /// `+%`, wrapping addition.
    PlusPercent = 51,
    /// `-%`, wrapping subtraction.
    MinusPercent = 52,
    /// `*%`, wrapping multiplication.
    StarPercent = 53,
    /// `=`.
    Equal = 18,
    /// `==`, identity.
    EqualEqual = 58,
    /// `=>`.
    Arrow = 19,
    /// `<`.
    Less = 31,
    /// `<=`.
    LessEqual = 32,
    /// `>`.
    Greater = 33,
    /// `>=`.
    GreaterEqual = 34,
    /// `:`.
    Colon = 20,
    /// `:=`.
    Assign = 45,
    /// `::`.
    DoubleColon = 21,
    /// `.`.
    Dot = 22,
    /// `;`.
    Semicolon = 23,
    /// Whitespace trivia.
    Whitespace = 24,
    /// Line comment trivia.
    Comment = 25,
    /// Documentation line comment trivia, written with `///`.
    DocComment = 46,
    /// Unknown byte or character.
    Error = 26,
}

impl TokenKind {
    /// Returns this kind's stable tag, its discriminant.
    #[must_use]
    pub(crate) const fn tag(self) -> u64 {
        self as u64
    }

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
            | Self::Ampersand
            | Self::Bar
            | Self::Caret
            | Self::TildeEqual
            | Self::TildeEqualEqual
            | Self::PlusPercent
            | Self::MinusPercent
            | Self::StarPercent
            | Self::Equal
            | Self::EqualEqual
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
            | Self::Elseif
            | Self::Else
            | Self::Unless
            | Self::Case
            | Self::Otherwise
            | Self::Select
            | Self::Handle
            | Self::With
            | Self::Restart
            | Self::When
            | Self::Block
            | Self::Begin
            | Self::Cleanup
            | Self::Exception
            | Self::While
            | Self::Until
            | Self::For
            | Self::End
            | Self::Ident
            | Self::Number
            | Self::Bool
            | Self::String
            | Self::Character
            | Self::Symbol
            | Self::Keyword
            | Self::VectorStart
            | Self::ListStart
            | Self::TypeName
            | Self::LParen
            | Self::RParen
            | Self::LBracket
            | Self::RBracket
            | Self::Comma
            | Self::Tilde
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

    /// Returns the span of the name this token carries, if any.
    ///
    /// Identifiers and reserved words are names in their entirety. A symbol
    /// `#name`, a keyword symbol `#:name`, and a type name `<name>` carry a
    /// name inside their punctuation. Other tokens carry no name.
    pub(crate) const fn name_span(self) -> Option<Span> {
        let Span { start, end } = self.span;
        match self.kind {
            TokenKind::Ident
            | TokenKind::Define
            | TokenKind::Function
            | TokenKind::Class
            | TokenKind::Slot
            | TokenKind::Constant
            | TokenKind::Generic
            | TokenKind::Method
            | TokenKind::Let
            | TokenKind::If
            | TokenKind::Elseif
            | TokenKind::Else
            | TokenKind::Unless
            | TokenKind::Case
            | TokenKind::Otherwise
            | TokenKind::Select
            | TokenKind::Handle
            | TokenKind::With
            | TokenKind::Restart
            | TokenKind::When
            | TokenKind::Block
            | TokenKind::Begin
            | TokenKind::Cleanup
            | TokenKind::Exception
            | TokenKind::While
            | TokenKind::Until
            | TokenKind::For
            | TokenKind::End => Some(self.span),
            TokenKind::Symbol => Some(Span::new(start + 1, end)),
            TokenKind::Keyword => Some(Span::new(start + 2, end)),
            TokenKind::TypeName => Some(Span::new(start + 1, end - 1)),
            _ => None,
        }
    }
}

/// CST node kind.
///
/// Each variant's discriminant is its stable tag, mixed into syntax node
/// ids; `repr(u8)` fixes its type. Tags never change once
/// given and are never reused, so a new variant takes one past the highest
/// tag. The compiler rejects two variants with the same tag.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CstKind {
    /// Root module node.
    Root = 1,
    /// Function definition.
    Function = 2,
    /// Constant definition.
    Constant = 30,
    /// Class definition.
    Class = 3,
    /// Slot declaration.
    SlotDecl = 4,
    /// Slot option such as `init-keyword: radius:`.
    SlotOption = 23,
    /// Class/type definition name, such as `<point>` in `define class`.
    TypeDefinitionName = 34,
    /// Value definition name, such as `area` in `define function area`.
    ValueDefinitionName = 35,
    /// Declared object slot name.
    SlotName = 36,
    /// Keyword value used by a slot option, such as `radius:`.
    SlotKeyword = 32,
    /// Generic function declaration.
    Generic = 5,
    /// Method definition.
    Method = 6,
    /// Function parameter list.
    ParameterList = 7,
    /// Function body.
    Body = 8,
    /// Local binding statement.
    LetStmt = 9,
    /// Expression evaluated for effect in a body: `expression ;`.
    ExprStmt = 57,
    /// Function, method, or generic parameter name.
    ParameterName = 37,
    /// `#key` parameter: its name, optional type and default.
    KeywordParameter = 69,
    /// Keyword parameter name, which is also its keyword.
    KeywordParameterName = 70,
    /// Local binding name.
    LetName = 38,
    /// Restart condition binding name.
    RestartName = 39,
    /// Binary expression.
    BinaryExpr = 10,
    /// Prefix unary expression.
    UnaryExpr = 28,
    /// Direct function call expression.
    CallExpr = 11,
    /// Direct call callee name.
    CallName = 40,
    /// Object slot read expression.
    SlotExpr = 12,
    /// Member name used after `.` in a slot read.
    MemberName = 41,
    /// Element access expression `collection[key]`, Dylan's sugar for
    /// `element(collection, key)`.
    IndexExpr = 63,
    /// Call of the value an operand evaluates to, such as `f(1)(2)` or
    /// `node.operator.apply(x)`. A call of a name is a [`Self::CallExpr`].
    ValueCallExpr = 71,
    /// Assignment expression: `object.slot := value`, `collection[key] :=
    /// value` or `local := value`.
    SlotAssignExpr = 29,
    /// Call argument list.
    ArgList = 13,
    /// Keyword argument.
    KeywordArg = 14,
    /// Keyword introducer in an argument list, such as `radius:`.
    ArgumentKeyword = 33,
    /// Name reference expression.
    NameExpr = 15,
    /// Integer literal expression.
    NumberExpr = 16,
    /// Boolean literal expression.
    BoolExpr = 19,
    /// String literal expression.
    StringExpr = 24,
    /// Character literal expression.
    CharacterExpr = 65,
    /// Symbol literal expression.
    SymbolExpr = 25,
    /// Keyword symbol literal expression.
    KeywordExpr = 26,
    /// Vector literal expression.
    VectorExpr = 27,
    /// List literal expression, `#(1, 2)`.
    ListExpr = 72,
    /// `if (test) body … end` conditional expression.
    IfExpr = 20,
    /// `elseif (test) body` clause of an `if` expression.
    ElseifClause = 51,
    /// `else body` clause of an `if` expression.
    ElseClause = 52,
    /// `unless (test) body end` conditional expression.
    UnlessExpr = 53,
    /// `case test => body; … end` conditional expression.
    CaseExpr = 54,
    /// `test => body` clause of a `case` expression.
    CaseClause = 55,
    /// `otherwise => body` clause of a `case` or `select` expression.
    OtherwiseClause = 56,
    /// `select (target by test) keys => body; … end` expression.
    SelectExpr = 60,
    /// `by test` part of a `select` expression.
    SelectTest = 61,
    /// `key, key => body` clause of a `select` expression.
    SelectClause = 62,
    /// Recoverable condition handler expression.
    HandleExpr = 21,
    /// Anonymous `method (params) => results body end` expression.
    MethodExpr = 31,
    /// Restart clause in a handler expression.
    RestartClause = 22,
    /// Dylan-style `block` expression.
    BlockExpr = 47,
    /// `begin body end`: a body in its own scope, as an expression.
    BeginExpr = 64,
    /// `cleanup` clause of a `block` expression.
    CleanupClause = 48,
    /// `while (test) body end` loop.
    WhileExpr = 58,
    /// `until (test) body end` loop.
    UntilExpr = 59,
    /// `for (clause) body end` loop.
    ForExpr = 66,
    /// Numeric clause of a `for` loop: `i from a to b by s`.
    NumericForClause = 67,
    /// Collection clause of a `for` loop: `x in c`.
    CollectionForClause = 68,
    /// Exit procedure name bound by a `block` expression.
    BlockExitName = 49,
    /// `when … =>` guard of a restart clause.
    RestartGuard = 50,
    /// Superclass type name in a class definition.
    SuperclassType = 42,
    /// Nominal type name in an annotation.
    AnnotationType = 43,
    /// Class-object type name in an annotation.
    ClassObjectAnnotationType = 44,
    /// Bare condition type name in a restart clause.
    RestartConditionType = 45,
    /// Type/class name expression.
    TypeExpr = 17,
    /// Type name used as an expression.
    ExpressionTypeName = 46,
    /// Malformed syntax placeholder.
    Error = 18,
}

impl CstKind {
    /// Returns this kind's stable tag, its discriminant.
    #[must_use]
    pub(crate) const fn tag(self) -> u64 {
        self as u64
    }

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
                | Self::KeywordParameterName
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
    let mut builder = SyntaxNodeId::builder(source_file, kind.tag());

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
        | CstKind::KeywordParameterName
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
        | CstKind::CharacterExpr
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
                        builder.mix_u64(token.kind.tag());
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
                    | TokenKind::Character
                    | TokenKind::Symbol
                    | TokenKind::Keyword
            ) {
                builder.mix_str(token.text(source));
                return;
            }
        }
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

/// Returns the end of the character literal starting at `start`: the
/// closing quote after one character or one escape, which runs to the next
/// quote. An unclosed literal ends at the line's end, and the literal's
/// decoding reports it.
fn lex_character_end(bytes: &[u8], start: usize) -> usize {
    let mut pos = start + 1;
    if bytes.get(pos) == Some(&b'\\') {
        pos += 2;
    } else if pos < bytes.len() && bytes[pos] != b'\n' {
        pos += 1;
    }
    while pos < bytes.len() && !matches!(bytes[pos], b'\'' | b'\n') {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'\'') {
        pos += 1;
    }
    pos.min(bytes.len())
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
            b'+' if bytes.get(pos + 1) == Some(&b'%') => {
                pos += 2;
                TokenKind::PlusPercent
            }
            b'+' => {
                pos += 1;
                TokenKind::Plus
            }
            b'-' if bytes.get(pos + 1) == Some(&b'%') => {
                pos += 2;
                TokenKind::MinusPercent
            }
            b'-' => {
                pos += 1;
                TokenKind::Minus
            }
            b'*' if bytes.get(pos + 1) == Some(&b'%') => {
                pos += 2;
                TokenKind::StarPercent
            }
            b'*' => {
                pos += 1;
                TokenKind::Star
            }
            b'/' if bytes.get(pos + 1) != Some(&b'/') => {
                pos += 1;
                TokenKind::Slash
            }
            b'&' => {
                pos += 1;
                TokenKind::Ampersand
            }
            b'|' => {
                pos += 1;
                TokenKind::Bar
            }
            b'^' => {
                pos += 1;
                TokenKind::Caret
            }
            b'~' if bytes.get(pos + 1) == Some(&b'=') && bytes.get(pos + 2) == Some(&b'=') => {
                pos += 3;
                TokenKind::TildeEqualEqual
            }
            b'~' if bytes.get(pos + 1) == Some(&b'=') => {
                pos += 2;
                TokenKind::TildeEqual
            }
            b'~' => {
                pos += 1;
                TokenKind::Tilde
            }
            b'=' if bytes.get(pos + 1) == Some(&b'>') => {
                pos += 2;
                TokenKind::Arrow
            }
            b'=' if bytes.get(pos + 1) == Some(&b'=') => {
                pos += 2;
                TokenKind::EqualEqual
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
                pos = lex_string_end(bytes, pos);
                TokenKind::String
            }
            b'\'' => {
                pos = lex_character_end(bytes, pos);
                TokenKind::Character
            }
            b'#' if bytes.get(pos + 1) == Some(&b'[') => {
                pos += 2;
                TokenKind::VectorStart
            }
            b'#' if bytes.get(pos + 1) == Some(&b'(') => {
                pos += 2;
                TokenKind::ListStart
            }
            b'#' if matches!(bytes.get(pos + 1), Some(b't' | b'f'))
                && bytes
                    .get(pos + 2)
                    .is_none_or(|next| !is_name_continue(*next)) =>
            {
                pos += 2;
                TokenKind::Bool
            }
            b'#' if bytes.get(pos + 1) == Some(&b':')
                && bytes.get(pos + 2).is_some_and(|next| is_name_start(*next)) =>
            {
                pos = name_end(bytes, pos + 2);
                TokenKind::Keyword
            }
            // Dylan's general symbol literal, for any name: `#"set!"`.
            b'#' if bytes.get(pos + 1) == Some(&b'"') => {
                pos = lex_string_end(bytes, pos + 1);
                TokenKind::Symbol
            }
            b'#' if bytes.get(pos + 1).is_some_and(|next| is_name_start(*next)) => {
                pos = name_end(bytes, pos + 1);
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
            b if is_name_start(b) => {
                pos = name_end(bytes, pos);
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
                    "elseif" => TokenKind::Elseif,
                    "else" => TokenKind::Else,
                    "unless" => TokenKind::Unless,
                    "case" => TokenKind::Case,
                    "otherwise" => TokenKind::Otherwise,
                    "select" => TokenKind::Select,
                    "handle" => TokenKind::Handle,
                    "with" => TokenKind::With,
                    "restart" => TokenKind::Restart,
                    "when" => TokenKind::When,
                    "block" => TokenKind::Block,
                    "begin" => TokenKind::Begin,
                    "cleanup" => TokenKind::Cleanup,
                    "exception" => TokenKind::Exception,
                    "while" => TokenKind::While,
                    "until" => TokenKind::Until,
                    "for" => TokenKind::For,
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

/// Returns the end of the string literal whose opening quote is at
/// `start`: past its closing quote, or the end of the source when it has
/// none. A backslash escapes the byte after it.
fn lex_string_end(bytes: &[u8], start: usize) -> usize {
    let mut pos = start + 1;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => {
                pos += 1;
                if pos < bytes.len() {
                    pos += 1;
                }
            }
            b'"' => return pos + 1,
            _ => pos += 1,
        }
    }
    pos
}

/// Suffixes that give a numeric literal a 32-bit type.
pub const NUMBER_SUFFIXES: [&str; 3] = ["i32", "u32", "f32"];

/// Returns the end of the numeric literal starting at `start`.
///
/// Mirrors the base grammar's `number` rule: digits, then an optional
/// fraction that must have digits on both sides of the `.` (so `x.radius`
/// style postfix access after a number never becomes part of the literal),
/// then an optional exponent that must contain at least one digit, then an
/// optional type suffix from [`NUMBER_SUFFIXES`]. Like the grammar, the
/// suffix is taken whatever follows it: `7i32-1` is `7i32 - 1`, because a
/// name cannot start inside a number.
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
    for suffix in NUMBER_SUFFIXES {
        let end = pos + suffix.len();
        if bytes.get(pos..end) == Some(suffix.as_bytes()) {
            return end;
        }
    }
    pos
}

/// Returns true when `byte` can start a name.
///
/// Mirrors the first character class of the base grammar's `name` rule,
/// `[$A-Za-z_]`.
pub(crate) const fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

/// Returns true when `byte` can continue a name.
///
/// Mirrors the base grammar's `[A-Za-z0-9_?!-]`. As in Dylan, a hyphen is
/// always a name character: `x-1`, `a-b`, and `x-` are single names, so
/// subtraction is written with spaces, `x - 1`. A name may end in `?`, as a
/// predicate's does, or `!`, as a destructive operation's does (`add!`).
pub(crate) const fn is_name_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'?' | b'!' | b'-')
}

/// Returns the end of the name whose first byte is at `start`.
///
/// A name continues through [`is_name_continue`] bytes and, as in Dylan,
/// through a `/` that does not start a `//` comment, so `truncate/` is one
/// name and division is written with spaces, `a / b`.
///
/// The caller has checked that `bytes[start]` is a name start. Every name
/// boundary in the lexer goes through here, so the lexer cannot split a name
/// anywhere the grammar would not.
fn name_end(bytes: &[u8], start: usize) -> usize {
    debug_assert!(is_name_start(bytes[start]));
    let mut pos = start + 1;
    while pos < bytes.len()
        && (is_name_continue(bytes[pos])
            || (bytes[pos] == b'/' && bytes.get(pos + 1) != Some(&b'/')))
    {
        pos += 1;
    }
    pos
}

/// Returns true when `text` is exactly one name under the base grammar's
/// `name` rule.
pub(crate) fn is_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first().is_some_and(|first| is_name_start(*first)) && name_end(bytes, 0) == bytes.len()
}

/// Returns true when `text` is exactly one numeric literal under the base
/// grammar's `number` rule, such as `1`, `2.5e3`, or `7i32`.
pub(crate) fn is_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_digit) && lex_number_end(bytes, 0) == bytes.len()
}

fn lex_type_name_end(bytes: &[u8], start: usize) -> Option<usize> {
    let name_start = start + 1;
    if !bytes
        .get(name_start)
        .is_some_and(|first| is_name_start(*first))
    {
        return None;
    }
    let pos = name_end(bytes, name_start);
    (bytes.get(pos) == Some(&b'>')).then_some(pos + 1)
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
    fn lex_general_symbol_literals_as_one_symbol_token() {
        let source = r#"#"set!" #"a \" b" #plain"#;
        let symbols = lex(source)
            .into_iter()
            .filter(|token| token.kind == TokenKind::Symbol)
            .map(|token| token.text(source).to_string())
            .collect::<Vec<_>>();
        assert_eq!(symbols, [r#"#"set!""#, r#"#"a \" b""#, "#plain"]);
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

    #[test]
    fn lex_wrapping_operators_as_single_tokens() {
        let kinds = |source: &str| {
            lex(source)
                .into_iter()
                .map(|token| token.kind)
                .filter(|kind| *kind != TokenKind::Whitespace)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds("a +% b -% c *% d"),
            [
                TokenKind::Ident,
                TokenKind::PlusPercent,
                TokenKind::Ident,
                TokenKind::MinusPercent,
                TokenKind::Ident,
                TokenKind::StarPercent,
                TokenKind::Ident,
            ]
        );
        assert_eq!(
            kinds("a + b"),
            [TokenKind::Ident, TokenKind::Plus, TokenKind::Ident]
        );
    }

    #[test]
    fn lex_numbers_take_a_whole_32_bit_suffix() {
        let numbers = |source: &str| {
            lex(source)
                .into_iter()
                .filter(|token| token.kind == TokenKind::Number)
                .map(|token| token.text(source).to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            numbers("7i32 7u32 1.5f32 2e3f32 5f32"),
            ["7i32", "7u32", "1.5f32", "2e3f32", "5f32"]
        );
        // As in the grammar, a suffix ends the literal whatever follows it,
        // so `7i32-1` subtracts; only a whole suffix counts.
        assert_eq!(numbers("7i32-1 7i64"), ["7i32", "1", "7"]);
    }

    fn texts(source: &str) -> Vec<(TokenKind, &str)> {
        lex(source)
            .into_iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| (token.kind, token.text(source)))
            .collect()
    }

    #[test]
    fn lex_hyphens_as_name_characters() {
        use TokenKind::{Ident, Keyword, Minus, MinusPercent, Number, Symbol, TypeName};
        assert_eq!(texts("x-1"), [(Ident, "x-1")]);
        assert_eq!(
            texts("a-b x- y--z"),
            [(Ident, "a-b"), (Ident, "x-"), (Ident, "y--z")]
        );
        assert_eq!(texts("x - 1"), [(Ident, "x"), (Minus, "-"), (Number, "1")]);
        assert_eq!(texts("x -1"), [(Ident, "x"), (Minus, "-"), (Number, "1")]);
        assert_eq!(
            texts("a -% b"),
            [(Ident, "a"), (MinusPercent, "-%"), (Ident, "b")]
        );
        assert_eq!(
            texts("a-%b"),
            [(Ident, "a-"), (TokenKind::Error, "%"), (Ident, "b")]
        );
        assert_eq!(
            texts("#foo-1 #:foo-1 #t-1 <foo-1?>"),
            [
                (Symbol, "#foo-1"),
                (Keyword, "#:foo-1"),
                (Symbol, "#t-1"),
                (TypeName, "<foo-1?>"),
            ]
        );
        assert_eq!(texts("end-1"), [(Ident, "end-1")]);
    }

    #[test]
    fn lex_identity_apart_from_equality_and_arrow() {
        use TokenKind::{Arrow, Equal, EqualEqual, Ident};
        assert_eq!(
            texts("a == b = c => d"),
            [
                (Ident, "a"),
                (EqualEqual, "=="),
                (Ident, "b"),
                (Equal, "="),
                (Ident, "c"),
                (Arrow, "=>"),
                (Ident, "d"),
            ]
        );
    }

    #[test]
    fn name_spans_strip_token_punctuation() {
        fn name(source: &str) -> Option<&str> {
            let token = lex(source)[0];
            token.name_span().map(|span| &source[span.start..span.end])
        }
        assert_eq!(name("x-1"), Some("x-1"));
        assert_eq!(name("end"), Some("end"));
        assert_eq!(name("#foo-1"), Some("foo-1"));
        assert_eq!(name("#:foo-1"), Some("foo-1"));
        assert_eq!(name("<foo-1>"), Some("foo-1"));
        assert_eq!(name("12"), None);
    }

    #[test]
    fn is_name_matches_the_grammar_name_rule() {
        for name in ["x", "x-1", "x-", "$answer", "_tmp", "nan?", "a--b"] {
            assert!(is_name(name), "{name}");
        }
        for text in ["", "1x", "-x", "x y", "x.y", "x%", "?x"] {
            assert!(!is_name(text), "{text}");
        }
    }
}
