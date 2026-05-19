#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// A byte span in parsed source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl Span {
    /// Creates a byte span.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Returns true when this span has no byte width.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// Stable source of a projected grammar fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionOrigin {
    /// Projected from compiler-owned facts.
    CompilerProjected,
    /// Projected from a compiler-shipped prototype.
    CompilerProjectedPrototype,
    /// Projected from a trusted host fixture or adapter.
    TrustedHostFixture,
    /// Projected from source-authored grammar data.
    SourceAuthored,
}

impl ProjectionOrigin {
    /// Returns the stable origin token used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompilerProjected => "compiler-projected",
            Self::CompilerProjectedPrototype => "compiler-projected-prototype",
            Self::TrustedHostFixture => "trusted-host-fixture",
            Self::SourceAuthored => "source-authored",
        }
    }
}

impl fmt::Display for ProjectionOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured grammar expression used by grammar rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleExpression {
    /// A named grammar rule reference.
    Rule(String),
    /// A literal token or delimiter.
    Literal(String),
    /// A character class over a single character.
    CharacterClass(String),
    /// Consume source until the next literal delimiter, without consuming it.
    UntilLiteral(String),
    /// A sequence of expressions.
    Sequence(Vec<RuleExpression>),
    /// An ordered choice between alternatives.
    Choice(Vec<RuleExpression>),
    /// An optional expression.
    Optional(Box<RuleExpression>),
    /// One or more repetitions.
    OneOrMore(Box<RuleExpression>),
    /// Zero or more repetitions.
    ZeroOrMore(Box<RuleExpression>),
    /// A structural input expression plus an inert output description.
    Transform {
        /// Matched input expression.
        input: Box<RuleExpression>,
        /// Inert output description.
        output: TransformOutput,
    },
}

/// Inert output attached to a transformed rule expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransformOutput {
    /// Deterministic action descriptor.
    Action(GrammarAction),
    /// Literal string data.
    Literal(String),
    /// Symbolic output token.
    Symbol(String),
}

/// A deterministic structural action attached to a grammar rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarAction {
    /// Action name, such as `emit-class`.
    pub name: String,
    /// Named action arguments.
    pub args: Vec<GrammarActionArg>,
}

/// One named grammar action argument.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarActionArg {
    /// Argument name.
    pub name: String,
    /// Argument value expression.
    pub value: GrammarActionTerm,
}

/// A small grammar action value expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GrammarActionTerm {
    /// Capture the first child rule with this name.
    Capture {
        /// Captured rule name.
        rule: String,
    },
    /// Capture all child rules with this name.
    CaptureAll {
        /// Captured rule name.
        rule: String,
    },
    /// Literal string/symbol data.
    Literal(String),
    /// Literal boolean data.
    Bool(bool),
}

impl GrammarAction {
    /// Creates a grammar action.
    #[must_use]
    pub fn new(name: impl Into<String>, args: Vec<GrammarActionArg>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }
}

impl GrammarActionArg {
    /// Creates a named grammar action argument.
    #[must_use]
    pub fn new(name: impl Into<String>, value: GrammarActionTerm) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

impl RuleExpression {
    /// Creates a rule reference.
    #[must_use]
    pub fn rule(name: impl Into<String>) -> Self {
        Self::Rule(name.into())
    }

    /// Creates a literal token/delimiter expression.
    #[must_use]
    pub fn literal(value: impl Into<String>) -> Self {
        Self::Literal(value.into())
    }

    /// Creates a character class expression.
    #[must_use]
    pub fn character_class(value: impl Into<String>) -> Self {
        Self::CharacterClass(value.into())
    }

    /// Creates an opaque-region expression that consumes until `value`.
    #[must_use]
    pub fn until_literal(value: impl Into<String>) -> Self {
        Self::UntilLiteral(value.into())
    }

    /// Creates a sequence, collapsing a single item to that item.
    #[must_use]
    pub fn sequence(items: Vec<Self>) -> Self {
        collapse_expression(Self::Sequence(items))
    }

    /// Creates an ordered choice, collapsing a single item to that item.
    #[must_use]
    pub fn choice(items: Vec<Self>) -> Self {
        collapse_expression(Self::Choice(items))
    }

    /// Creates an optional expression.
    #[must_use]
    pub fn optional(item: Self) -> Self {
        Self::Optional(Box::new(item))
    }

    /// Creates a one-or-more repetition expression.
    #[must_use]
    pub fn one_or_more(item: Self) -> Self {
        Self::OneOrMore(Box::new(item))
    }

    /// Creates a zero-or-more repetition expression.
    #[must_use]
    pub fn zero_or_more(item: Self) -> Self {
        Self::ZeroOrMore(Box::new(item))
    }

    /// Creates a transform expression.
    #[must_use]
    pub fn transform(input: Self, output: TransformOutput) -> Self {
        Self::Transform {
            input: Box::new(input),
            output,
        }
    }
}

impl fmt::Display for RuleExpression {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rule(name) => formatter.write_str(name),
            Self::Literal(value) => write!(formatter, "`{value}`"),
            Self::CharacterClass(value) => write!(formatter, "[{value}]"),
            Self::UntilLiteral(value) => write!(formatter, "until(`{value}`)"),
            Self::Sequence(items) => {
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(" ")?;
                    }
                    write!(formatter, "{}", GroupedRuleExpression(item))?;
                }
                Ok(())
            }
            Self::Choice(items) => {
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(" | ")?;
                    }
                    write!(formatter, "{item}")?;
                }
                Ok(())
            }
            Self::Optional(item) => write!(formatter, "{}?", GroupedRuleExpression(item)),
            Self::OneOrMore(item) => write!(formatter, "{}+", GroupedRuleExpression(item)),
            Self::ZeroOrMore(item) => write!(formatter, "{}*", GroupedRuleExpression(item)),
            Self::Transform { input, output } => write!(formatter, "{input} => {output}"),
        }
    }
}

impl fmt::Display for TransformOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Action(action) => write!(formatter, "{action}"),
            Self::Literal(value) => write!(formatter, "`{value}`"),
            Self::Symbol(value) => formatter.write_str(value),
        }
    }
}

impl fmt::Display for GrammarAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}(", self.name)?;
        for (index, arg) in self.args.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{}: {}", arg.name, arg.value)?;
        }
        formatter.write_str(")")
    }
}

impl fmt::Display for GrammarActionTerm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capture { rule } => formatter.write_str(rule),
            Self::CaptureAll { rule } => write!(formatter, "{rule}*"),
            Self::Literal(value) => write!(formatter, "{value:?}"),
            Self::Bool(value) => write!(formatter, "{value}"),
        }
    }
}

fn collapse_expression(expression: RuleExpression) -> RuleExpression {
    match expression {
        RuleExpression::Sequence(mut items) | RuleExpression::Choice(mut items)
            if items.len() == 1 =>
        {
            items.remove(0)
        }
        expression => expression,
    }
}

struct GroupedRuleExpression<'a>(&'a RuleExpression);

impl fmt::Display for GroupedRuleExpression<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RuleExpression::Choice(_) | RuleExpression::Transform { .. } => {
                write!(formatter, "({})", self.0)
            }
            expression => write!(formatter, "{expression}"),
        }
    }
}

/// A rule parsed from grammar source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAuthoredRule {
    /// Rule name.
    pub name: String,
    /// Rule expression.
    pub expression: RuleExpression,
    /// Optional deterministic action descriptor.
    pub action: Option<GrammarAction>,
    /// Complete matched text values rejected after the expression matches.
    ///
    /// This is a neutral escape hatch for identifier-like rules that need a
    /// caller-owned reserved-word policy without baking any one language into
    /// the grammar kernel.
    pub excluded_text: Vec<String>,
    /// Source span in the grammar source.
    pub source_span: Span,
}

impl SourceAuthoredRule {
    /// Returns this rule with complete matched text exclusions attached.
    #[must_use]
    pub fn with_excluded_text(mut self, excluded_text: Vec<String>) -> Self {
        self.excluded_text = excluded_text;
        self
    }
}

/// Parse tree node produced by interpreted matching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleNode {
    /// Matched rule name.
    pub rule_name: String,
    /// Matched source span.
    pub span: Span,
    /// Matched source text.
    pub text: String,
    /// Child rule/literal/class nodes.
    pub children: Vec<RuleNode>,
}

/// Interpreted parse result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parse {
    /// Root parse node.
    pub root: RuleNode,
}

/// Interpreted parse failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    /// Error span.
    pub span: Span,
    /// Expected fragments at the error span.
    pub expected: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpressionMatch {
    end: usize,
    children: Vec<RuleNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParseFailure {
    position: usize,
    expected: Vec<String>,
}

/// Parses the small line-oriented grammar source format.
#[must_use]
pub fn parse_grammar_source(source: &str) -> Vec<SourceAuthoredRule> {
    let mut rules = Vec::new();
    let mut offset = 0_usize;
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("grammar ")
            || trimmed == "end"
        {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("rule ") else {
            continue;
        };
        let Some((name, expression)) = rest.split_once('=') else {
            continue;
        };
        let action = expression
            .split_once("=>")
            .and_then(|(_, output)| parse_grammar_action(output.trim()));
        let leading = line.find(trimmed).unwrap_or(0);
        let trimmed_start = line_start + leading;
        rules.push(SourceAuthoredRule {
            name: name.trim().to_string(),
            expression: parse_rule_expression(expression),
            action,
            excluded_text: Vec::new(),
            source_span: Span::new(trimmed_start, line_end.saturating_sub(1)),
        });
    }
    rules
}

/// Parses one grammar rule expression.
#[must_use]
pub fn parse_rule_expression(expression: &str) -> RuleExpression {
    let trimmed = expression.trim();
    if let Some((input, output)) = split_transform_top_level(trimmed) {
        return RuleExpression::transform(
            parse_rule_expression(input),
            parse_transform_output(output.trim()),
        );
    }
    let choices = split_top_level(trimmed, '|');
    if choices.len() > 1 {
        return RuleExpression::choice(
            choices
                .into_iter()
                .map(parse_rule_expression)
                .collect::<Vec<_>>(),
        );
    }
    let terms = split_whitespace_top_level(trimmed);
    RuleExpression::sequence(terms.into_iter().map(parse_rule_term).collect::<Vec<_>>())
}

fn split_transform_top_level(input: &str) -> Option<(&str, &str)> {
    let mut depth = 0_i32;
    let mut in_literal = false;
    let mut chars = input.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '`' => in_literal = !in_literal,
            '(' if !in_literal => depth += 1,
            ')' if !in_literal => depth -= 1,
            '=' if !in_literal && depth == 0 && matches!(chars.peek(), Some((_, '>'))) => {
                let output_start = index + "=>".len();
                return Some((input[..index].trim(), input[output_start..].trim()));
            }
            _ => {}
        }
    }
    None
}

fn parse_transform_output(output: &str) -> TransformOutput {
    if let Some(action) = parse_grammar_action(output) {
        return TransformOutput::Action(action);
    }
    output
        .strip_prefix('`')
        .and_then(|inner| inner.strip_suffix('`'))
        .map_or_else(
            || TransformOutput::Symbol(output.to_string()),
            |value| TransformOutput::Literal(value.to_string()),
        )
}

/// Parses an action descriptor.
#[must_use]
pub fn parse_grammar_action(output: &str) -> Option<GrammarAction> {
    let trimmed = output.trim();
    let (name, args) = trimmed.split_once('(')?;
    let args = args.strip_suffix(')')?;
    Some(GrammarAction::new(
        name.trim(),
        split_action_args(args)
            .into_iter()
            .filter_map(parse_grammar_action_arg)
            .collect(),
    ))
}

fn parse_grammar_action_arg(arg: &str) -> Option<GrammarActionArg> {
    let (name, value) = arg.split_once(':')?;
    Some(GrammarActionArg::new(
        name.trim(),
        parse_grammar_action_term(value.trim()),
    ))
}

fn parse_grammar_action_term(value: &str) -> GrammarActionTerm {
    let trimmed = value.trim();
    if trimmed == "true" {
        return GrammarActionTerm::Bool(true);
    }
    if trimmed == "false" {
        return GrammarActionTerm::Bool(false);
    }
    if let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('`')
                .and_then(|inner| inner.strip_suffix('`'))
        })
    {
        return GrammarActionTerm::Literal(inner.to_string());
    }
    if let Some(rule) = trimmed.strip_suffix('*') {
        return GrammarActionTerm::CaptureAll {
            rule: rule.trim().to_string(),
        };
    }
    GrammarActionTerm::Capture {
        rule: trimmed.to_string(),
    }
}

fn split_action_args(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut in_literal = false;
    for (index, ch) in input.char_indices() {
        match ch {
            '"' if !in_literal => in_string = !in_string,
            '`' if !in_string => in_literal = !in_literal,
            ',' if !in_string && !in_literal => {
                parts.push(input[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(input[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn parse_rule_term(term: &str) -> RuleExpression {
    let trimmed = term.trim();
    if let Some(inner) = trimmed.strip_suffix('*') {
        return RuleExpression::zero_or_more(parse_rule_expression(inner));
    }
    if let Some(inner) = trimmed.strip_suffix('+') {
        return RuleExpression::one_or_more(parse_rule_expression(inner));
    }
    if let Some(inner) = trimmed.strip_suffix('?') {
        return RuleExpression::optional(parse_rule_expression(inner));
    }
    if let Some(inner) = trimmed
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
    {
        return parse_rule_expression(inner);
    }
    if let Some(inner) = trimmed
        .strip_prefix("until(")
        .and_then(|inner| inner.strip_suffix(')'))
        .and_then(|inner| {
            inner
                .trim()
                .strip_prefix('`')
                .and_then(|inner| inner.strip_suffix('`'))
        })
    {
        return RuleExpression::until_literal(inner);
    }
    if let Some(inner) = trimmed
        .strip_prefix('`')
        .and_then(|inner| inner.strip_suffix('`'))
    {
        return RuleExpression::literal(inner);
    }
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        return RuleExpression::character_class(inner);
    }
    RuleExpression::rule(trimmed)
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut in_literal = false;
    for (index, ch) in input.char_indices() {
        match ch {
            '`' => in_literal = !in_literal,
            '(' if !in_literal => depth += 1,
            ')' if !in_literal => depth -= 1,
            _ if ch == delimiter && !in_literal && depth == 0 => {
                parts.push(input[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(input[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

fn split_whitespace_top_level(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = None;
    let mut depth = 0_i32;
    let mut in_literal = false;
    let mut in_class = false;
    for (index, ch) in input.char_indices() {
        if start.is_none() && (!ch.is_whitespace() || in_literal || in_class || depth > 0) {
            start = Some(index);
        }
        match ch {
            '`' if !in_class => in_literal = !in_literal,
            '[' if !in_literal => in_class = true,
            ']' if !in_literal => in_class = false,
            '(' if !in_literal && !in_class => depth += 1,
            ')' if !in_literal && !in_class => depth -= 1,
            _ if ch.is_whitespace() && !in_literal && !in_class && depth == 0 => {
                if let Some(start_index) = start.take() {
                    parts.push(input[start_index..index].trim());
                }
            }
            _ => {}
        }
    }
    if let Some(start_index) = start {
        parts.push(input[start_index..].trim());
    }
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

/// Interprets `root_rule` over a complete source region.
pub fn interpret_region(
    rules: &[SourceAuthoredRule],
    root_rule: &str,
    source: &str,
    source_offset: usize,
) -> Result<Parse, ParseError> {
    let root = interpret_rule(rules, root_rule, source, 0, source_offset)
        .map_err(|failure| failure.into_error(source_offset))?;
    let trailing = skip_trivia(source, root.span.end.saturating_sub(source_offset));
    if trailing == source.len() {
        Ok(Parse { root })
    } else {
        Err(ParseFailure::expected(trailing, "end of region").into_error(source_offset))
    }
}

/// Interprets one rule and returns the failure that rule would produce.
#[must_use]
pub fn interpret_rule_error(
    rules: &[SourceAuthoredRule],
    rule_name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> ParseError {
    interpret_rule(rules, rule_name, source, position, source_offset)
        .err()
        .unwrap_or_else(|| ParseFailure::expected(position, rule_name))
        .into_error(source_offset)
}

fn interpret_rule(
    rules: &[SourceAuthoredRule],
    name: &str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Result<RuleNode, ParseFailure> {
    if let Some(rule) = rules.iter().find(|rule| rule.name == name) {
        let start = skip_trivia(source, position);
        let matched = interpret_expression(rules, &rule.expression, source, start, source_offset)?;
        let end = matched.end;
        if rule
            .excluded_text
            .iter()
            .any(|excluded| excluded == &source[start..end])
        {
            return Err(ParseFailure::expected(start, name));
        }
        return Ok(RuleNode {
            rule_name: name.to_string(),
            span: Span::new(source_offset + start, source_offset + end),
            text: source[start..end].to_string(),
            children: matched.children,
        });
    }
    Err(ParseFailure::expected(skip_trivia(source, position), name))
}

fn interpret_expression(
    rules: &[SourceAuthoredRule],
    expression: &RuleExpression,
    source: &str,
    position: usize,
    source_offset: usize,
) -> Result<ExpressionMatch, ParseFailure> {
    match expression {
        RuleExpression::Rule(name) => {
            let node = interpret_rule(rules, name, source, position, source_offset)?;
            Ok(ExpressionMatch {
                end: node.span.end.saturating_sub(source_offset),
                children: vec![node],
            })
        }
        RuleExpression::Literal(value) => {
            let start = skip_trivia(source, position);
            let end = source[start..]
                .starts_with(value)
                .then_some(start + value.len())
                .filter(|end| literal_boundary_ok(source, *end, value))
                .ok_or_else(|| ParseFailure::expected(start, literal_expected(value)))?;
            Ok(ExpressionMatch {
                end,
                children: vec![RuleNode {
                    rule_name: format!("literal:{value}"),
                    span: Span::new(source_offset + start, source_offset + end),
                    text: source[start..end].to_string(),
                    children: Vec::new(),
                }],
            })
        }
        RuleExpression::CharacterClass(value) => {
            let start = position;
            let ch = source[start..]
                .chars()
                .next()
                .filter(|ch| character_class_contains(value, *ch))
                .ok_or_else(|| ParseFailure::expected(start, character_class_expected(value)))?;
            let end = start + ch.len_utf8();
            Ok(ExpressionMatch {
                end,
                children: vec![RuleNode {
                    rule_name: format!("class:{value}"),
                    span: Span::new(source_offset + start, source_offset + end),
                    text: source[start..end].to_string(),
                    children: Vec::new(),
                }],
            })
        }
        RuleExpression::UntilLiteral(value) => {
            let start = position;
            let relative_end = source[start..]
                .match_indices(value)
                .find_map(|(index, _)| {
                    let end = start + index + value.len();
                    literal_boundary_ok(source, end, value).then_some(start + index)
                })
                .unwrap_or(source.len());
            Ok(ExpressionMatch {
                end: relative_end,
                children: vec![RuleNode {
                    rule_name: format!("until:{value}"),
                    span: Span::new(source_offset + start, source_offset + relative_end),
                    text: source[start..relative_end].to_string(),
                    children: Vec::new(),
                }],
            })
        }
        RuleExpression::Sequence(items) => {
            let mut end = position;
            let mut children = Vec::new();
            for item in items {
                let matched = interpret_expression(rules, item, source, end, source_offset)?;
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch { end, children })
        }
        RuleExpression::Choice(items) => {
            let mut failure = None;
            for item in items {
                match interpret_expression(rules, item, source, position, source_offset) {
                    Ok(matched) => return Ok(matched),
                    Err(error) => merge_failure(&mut failure, error),
                }
            }
            Err(failure.unwrap_or_else(|| ParseFailure::expected(position, "choice")))
        }
        RuleExpression::Optional(item) => {
            Ok(
                interpret_expression(rules, item, source, position, source_offset).unwrap_or_else(
                    |_| ExpressionMatch {
                        end: position,
                        children: Vec::new(),
                    },
                ),
            )
        }
        RuleExpression::OneOrMore(item) => {
            let first = interpret_expression(rules, item, source, position, source_offset)?;
            let mut end = first.end;
            let mut children = first.children;
            while let Ok(matched) = interpret_expression(rules, item, source, end, source_offset) {
                if matched.end == end {
                    break;
                }
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch { end, children })
        }
        RuleExpression::ZeroOrMore(item) => {
            let mut end = position;
            let mut children = Vec::new();
            while let Ok(matched) = interpret_expression(rules, item, source, end, source_offset) {
                if matched.end == end {
                    break;
                }
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch { end, children })
        }
        RuleExpression::Transform { input, .. } => {
            interpret_expression(rules, input, source, position, source_offset)
        }
    }
}

impl ParseFailure {
    fn expected(position: usize, expected: impl Into<String>) -> Self {
        Self {
            position,
            expected: vec![expected.into()],
        }
    }

    fn into_error(self, source_offset: usize) -> ParseError {
        ParseError {
            span: Span::new(source_offset + self.position, source_offset + self.position),
            expected: self.expected,
        }
    }
}

fn merge_failure(failure: &mut Option<ParseFailure>, next: ParseFailure) {
    match failure {
        Some(current) if current.position > next.position => {}
        Some(current) if current.position == next.position => {
            for expected in next.expected {
                if !current.expected.contains(&expected) {
                    current.expected.push(expected);
                }
            }
        }
        _ => *failure = Some(next),
    }
}

fn literal_expected(value: &str) -> String {
    format!("`{value}`")
}

fn character_class_expected(value: &str) -> String {
    format!("[{value}]")
}

fn character_class_contains(class: &str, ch: char) -> bool {
    let mut chars = class.chars().peekable();
    let mut previous = None;
    while let Some(current) = chars.next() {
        if current == '-'
            && let (Some(start), Some(&end)) = (previous, chars.peek())
        {
            let _ = chars.next();
            if start <= ch && ch <= end {
                return true;
            }
            previous = Some(end);
            continue;
        }
        if current == ch {
            return true;
        }
        previous = Some(current);
    }
    false
}

fn skip_trivia(source: &str, mut position: usize) -> usize {
    loop {
        let rest = &source[position..];
        let skipped = rest
            .char_indices()
            .take_while(|(_, ch)| ch.is_whitespace())
            .last()
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        position += skipped;
        if source[position..].starts_with("//") {
            if let Some(newline) = source[position..].find('\n') {
                position += newline + 1;
                continue;
            }
            return source.len();
        }
        return position;
    }
}

fn literal_boundary_ok(source: &str, end: usize, literal: &str) -> bool {
    if !literal
        .bytes()
        .last()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
    {
        return true;
    }
    source
        .as_bytes()
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'?'))
}

/// Counts parse tree nodes per rule name.
#[must_use]
pub fn rule_counts(root: &RuleNode) -> Vec<RuleCount> {
    let mut counts = Vec::<RuleCount>::new();
    count_rule(root, &mut counts);
    counts
}

/// Count for one parse-tree rule name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleCount {
    /// Stable grammar rule name.
    pub rule_name: String,
    /// Number of parse tree nodes for this rule.
    pub node_count: usize,
}

fn count_rule(node: &RuleNode, counts: &mut Vec<RuleCount>) {
    if let Some(count) = counts
        .iter_mut()
        .find(|count| count.rule_name == node.rule_name)
    {
        count.node_count = count.node_count.saturating_add(1);
    } else {
        counts.push(RuleCount {
            rule_name: node.rule_name.clone(),
            node_count: 1,
        });
    }
    for child in &node.children {
        count_rule(child, counts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_interprets_tiny_grammar() {
        let grammar = "grammar tiny\nrule pair = name `:` type-name\nrule name = [A-Za-z]+ \nrule type-name = `<` name `>`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse =
            interpret_region(&rules, "pair", "radius: <integer>", 100).expect("pair should parse");

        assert_eq!(parse.root.rule_name, "pair");
        assert_eq!(parse.root.span, Span::new(100, 117));
        assert!(
            parse
                .root
                .children
                .iter()
                .any(|child| child.rule_name == "name")
        );
    }

    #[test]
    fn reports_expected_fragments() {
        let grammar = "grammar digits\nrule digits = [0-9]+\nend\n";
        let rules = parse_grammar_source(grammar);
        let error = interpret_region(&rules, "digits", "", 30).expect_err("digits should fail");

        assert_eq!(error.span, Span::new(30, 30));
        assert_eq!(error.expected, vec!["[0-9]"]);
    }

    #[test]
    fn interprets_choice_optional_and_repetition() {
        let grammar = "grammar words\n\
                       rule item = (`a` | `b`) `!`?\n\
                       rule items = item+\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "items", "a! b", 10).expect("items parse");

        assert_eq!(parse.root.span, Span::new(10, 14));
        assert_eq!(
            parse
                .root
                .children
                .iter()
                .map(|child| (child.rule_name.as_str(), child.text.as_str()))
                .collect::<Vec<_>>(),
            vec![("item", "a!"), ("item", "b")]
        );
    }

    #[test]
    fn reports_trailing_input_as_region_error() {
        let grammar = "grammar one\nrule root = `ok`\nend\n";
        let rules = parse_grammar_source(grammar);
        let error = interpret_region(&rules, "root", "ok nope", 5).expect_err("trailing input");

        assert_eq!(error.span, Span::new(8, 8));
        assert_eq!(error.expected, vec!["end of region"]);
    }

    #[test]
    fn parses_action_descriptors_as_inert_data() {
        let grammar = "grammar forms\n\
                       rule form = name => emit-form(name: name, items: item*, required: true, kind: \"thing\")\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let action = rules[0].action.as_ref().expect("action parsed");

        assert_eq!(action.name, "emit-form");
        assert_eq!(
            action.args,
            vec![
                GrammarActionArg::new(
                    "name",
                    GrammarActionTerm::Capture {
                        rule: "name".to_string()
                    }
                ),
                GrammarActionArg::new(
                    "items",
                    GrammarActionTerm::CaptureAll {
                        rule: "item".to_string()
                    }
                ),
                GrammarActionArg::new("required", GrammarActionTerm::Bool(true)),
                GrammarActionArg::new("kind", GrammarActionTerm::Literal("thing".to_string())),
            ]
        );
    }

    #[test]
    fn caller_owned_excluded_text_rejects_matched_rules() {
        let grammar = "grammar names\nrule name = [A-Za-z]+\nend\n";
        let rules = parse_grammar_source(grammar)
            .into_iter()
            .map(|rule| rule.with_excluded_text(vec!["define".to_string()]))
            .collect::<Vec<_>>();

        let error = interpret_region(&rules, "name", "define", 20).expect_err("excluded word");
        assert_eq!(error.span, Span::new(20, 20));
        assert_eq!(error.expected, vec!["name"]);

        let parse = interpret_region(&rules, "name", "circle", 30).expect("non-excluded name");
        assert_eq!(parse.root.text, "circle");
    }
}
