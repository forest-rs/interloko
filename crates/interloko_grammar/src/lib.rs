#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// A byte span in parsed source.
///
/// [`interpret_region`] uses spans to report where a [`Parse`] or
/// [`ParseError`] lands in the caller's source. `source_offset` is folded into
/// every produced span so embedded regions can preserve coordinates in a larger
/// file.
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

    /// Returns true when `offset` is inside this span.
    ///
    /// Empty spans contain only their exact start position. Non-empty spans use
    /// the usual half-open byte range: `start <= offset < end`.
    #[must_use]
    pub const fn contains_offset(self, offset: usize) -> bool {
        if self.is_empty() {
            self.start == offset
        } else {
            self.start <= offset && offset < self.end
        }
    }

    /// Returns true when `span` is fully contained by this span.
    #[must_use]
    pub const fn contains_span(self, span: Span) -> bool {
        self.start <= span.start && span.end <= self.end
    }

    /// Returns the smallest span that contains both `self` and `other`.
    ///
    /// The result starts at the minimum of the two starts and ends at the
    /// maximum of the two ends. Useful when assembling a span over a sequence
    /// of child nodes.
    #[must_use]
    pub const fn cover(self, other: Span) -> Span {
        let start = if self.start < other.start {
            self.start
        } else {
            other.start
        };
        let end = if self.end > other.end {
            self.end
        } else {
            other.end
        };
        Span::new(start, end)
    }

    /// Returns the overlapping portion of `self` and `other`, or `None` when
    /// the spans do not overlap.
    ///
    /// Half-open semantics match [`Span::contains_offset`]: touching endpoints
    /// alone do not overlap and yield `None`.
    #[must_use]
    pub const fn intersect(self, other: Span) -> Option<Span> {
        let start = if self.start > other.start {
            self.start
        } else {
            other.start
        };
        let end = if self.end < other.end {
            self.end
        } else {
            other.end
        };
        if start < end {
            Some(Span::new(start, end))
        } else {
            None
        }
    }
}

/// Structured grammar expression used by grammar rules.
///
/// [`parse_rule_expression`] and [`parse_grammar_source`] produce this type.
/// [`interpret_region`] consumes it through [`SourceAuthoredRule`] values when
/// matching source text.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add variants
/// (lookahead, cut, and similar) without forcing a breaking change.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuleExpression<'g> {
    /// A named grammar rule reference.
    Rule(&'g str),
    /// A literal token or delimiter.
    Literal(&'g str),
    /// A character class over a single character.
    CharacterClass(&'g str),
    /// Consume source until the next literal delimiter, without consuming it.
    UntilLiteral(&'g str),
    /// A sequence of expressions.
    Sequence(Vec<RuleExpression<'g>>),
    /// An ordered choice between alternatives.
    Choice(Vec<RuleExpression<'g>>),
    /// An optional expression.
    Optional(Box<RuleExpression<'g>>),
    /// One or more repetitions.
    OneOrMore(Box<RuleExpression<'g>>),
    /// Zero or more repetitions.
    ZeroOrMore(Box<RuleExpression<'g>>),
    /// A structural input expression plus an inert output description.
    ///
    /// Matching uses only `input`; callers can inspect `output` after parsing a
    /// grammar to decide how a matched rule should be interpreted.
    Transform {
        /// Matched input expression.
        input: Box<RuleExpression<'g>>,
        /// Inert output description.
        output: TransformOutput<'g>,
    },
    /// Positive lookahead: match without consuming any input.
    ///
    /// Succeeds when the inner expression matches at the current position but
    /// leaves the matcher position unchanged.
    And(Box<RuleExpression<'g>>),
    /// Negative lookahead: fail when the inner expression would match.
    ///
    /// Succeeds without consuming when the inner expression fails to match at
    /// the current position. Reports
    /// [`ExpectedFragment::NegativeLookahead`] on failure.
    Not(Box<RuleExpression<'g>>),
    /// Committed-choice cut: once the inner expression matches inside an
    /// alternative of an enclosing [`RuleExpression::Choice`], the choice no
    /// longer falls through to sibling alternatives. A subsequent failure in
    /// the same alternative becomes the choice's failure verbatim.
    ///
    /// The commit is bounded by the innermost enclosing [`RuleExpression::Choice`].
    /// It does not bubble past an [`RuleExpression::Optional`], a repetition,
    /// or an outer choice; nor does it cross rule boundaries.
    Cut(Box<RuleExpression<'g>>),
    /// Named subexpression binding.
    ///
    /// Surface syntax: `name:expr` inside a rule body (glued colon,
    /// outside backticks). The matcher tags children produced by
    /// `inner` with the binding `name` so an action can resolve a
    /// specific child by binding rather than by rule name. Useful when
    /// a rule has multiple children of the same rule type and the
    /// action wants to refer to a specific one.
    Named {
        /// Binding name attached to the matched children.
        name: &'g str,
        /// Inner expression whose match contributes the bound children.
        inner: Box<RuleExpression<'g>>,
    },
    /// Parametric rule invocation.
    ///
    /// Surface syntax: `rule-name<arg, arg, ...>` with the angle bracket
    /// glued to the rule name (no intervening whitespace). At match
    /// time the kernel looks up `rule` (which must have declared the
    /// matching number of [`SourceAuthoredRule::params`]) and
    /// substitutes each parameter reference in the body with the
    /// corresponding argument expression.
    Apply {
        /// Name of the parametric rule to invoke.
        rule: &'g str,
        /// Arguments substituted into the rule's formal parameters.
        args: Vec<RuleExpression<'g>>,
    },
    /// Parent-grammar rule reference.
    ///
    /// Surface syntax: `super.rule-name` inside a rule body. Only
    /// meaningful when the containing grammar was built via
    /// [`CompiledGrammar::compile_with_parent`]. The kernel switches
    /// to the parent grammar's own rule lookup and runs `rule-name`
    /// the way the parent would on its own — inner `Rule(...)`
    /// references inside the parent's body resolve against the
    /// parent's grammar (with its own parent chain), not against the
    /// child's overrides. This makes the semantics deterministic
    /// across multi-level inheritance: every `super.foo` resolves to
    /// "the foo this grammar's immediate parent would match," and a
    /// `super.foo` inside the parent's body reaches the grandparent.
    ///
    /// Practical consequence: an override that wants to add an
    /// alternative on top of the parent's behavior (a common case)
    /// composes cleanly via `super`. An override that wants child
    /// rule changes to propagate into the parent's body when reached
    /// through `super` is *not* what this implements — call the
    /// child's rules directly in those cases.
    ///
    /// `Super` is one face of a broader invariant: the matcher
    /// always runs a rule body in the grammar that authored it.
    /// That applies to *every* inherited rule, not only the ones
    /// that contain an explicit `super`. When a child looks up a
    /// rule inherited from an ancestor, the body executes against
    /// the ancestor's lookup — so any `Rule(...)` references inside
    /// resolve in the authoring grammar's scope. `Super` is the
    /// explicit version of that same routing rule, lifted one level
    /// up the chain.
    ///
    /// Currently only bare rule names are accepted after `super.`.
    /// Parametric parent references (`super.rule-name<args>`) are
    /// not supported; the parser falls back to treating the whole
    /// term as a plain rule reference if angle-bracket arguments
    /// follow.
    ///
    /// Matching against a grammar with no parent fails as a normal
    /// `expected rule` parse failure at that position.
    Super(&'g str),
}

/// Inert output attached to a transformed rule expression.
///
/// [`parse_rule_expression`] produces this for `=> ...` transforms. The
/// grammar kernel stores and displays the value, but it never executes actions
/// or assigns language-specific meaning to symbols.
///
/// Marked `#[non_exhaustive]` so future transform output forms can be added
/// without forcing a breaking change.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TransformOutput<'g> {
    /// Deterministic action descriptor.
    Action(GrammarAction<'g>),
    /// Literal string data.
    Literal(&'g str),
    /// Symbolic output token.
    Symbol(&'g str),
}

/// A deterministic structural action attached to a grammar rule.
///
/// [`parse_grammar_action`] and [`parse_grammar_source`] produce this from
/// transform output such as `=> emit-form(name: name)`. The value is inert:
/// callers inspect it alongside a matched [`RuleNode`] and decide what it
/// means.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarAction<'g> {
    /// Action name, such as `emit-class`.
    pub name: &'g str,
    /// Named action arguments.
    pub args: Vec<GrammarActionArg<'g>>,
}

/// One named grammar action argument.
///
/// [`GrammarAction::args`] stores these. Each argument names a capture,
/// capture-all, literal, or boolean term that a caller-owned action evaluator
/// can interpret against a [`RuleNode`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarActionArg<'g> {
    /// Argument name.
    pub name: &'g str,
    /// Argument value expression.
    pub value: GrammarActionTerm<'g>,
}

/// A small grammar action value expression.
///
/// [`parse_grammar_action`] produces these for action arguments. The grammar
/// kernel does not resolve captures; it only records what a caller should look
/// for in a matched [`RuleNode`] tree.
///
/// Marked `#[non_exhaustive]` so future action term forms can be added
/// without forcing a breaking change.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GrammarActionTerm<'g> {
    /// Capture the first child rule with this name.
    Capture {
        /// Captured rule name.
        rule: &'g str,
    },
    /// Capture all child rules with this name.
    CaptureAll {
        /// Captured rule name.
        rule: &'g str,
    },
    /// Capture the first child whose `binding_name` matches.
    ///
    /// Surface syntax: a bare identifier in an action argument value
    /// resolves to this term when the enclosing rule defines a
    /// matching `name:expr` binding. Bindings are scoped to the rule
    /// they appear in; cross-rule binding lookups are not supported.
    CaptureByName {
        /// Captured binding name.
        name: &'g str,
    },
    /// Literal string/symbol data.
    Literal(&'g str),
    /// Literal boolean data.
    Bool(bool),
    /// Literal integer data.
    Integer(i64),
    /// Ordered list of action terms.
    ///
    /// Surface syntax: `[term, term, term]`. Used for projections that
    /// assemble a sequence the evaluator can hand straight to a
    /// downstream consumer.
    List(Vec<GrammarActionTerm<'g>>),
    /// Named record of action terms.
    ///
    /// Surface syntax: `{ key: term, key: term }`. Used for grouped
    /// structural projections that aren't full action calls.
    Record(Vec<GrammarActionArg<'g>>),
    /// Nested action invocation.
    ///
    /// Surface syntax: `name(arg: term, ...)`. The kernel stores the
    /// call as inert data; nested calls let the descriptor capture a
    /// tree of structural projections without exiting the term
    /// vocabulary.
    Call(Box<GrammarAction<'g>>),
}

impl<'g> GrammarAction<'g> {
    /// Creates a grammar action.
    #[must_use]
    pub fn new(name: &'g str, args: Vec<GrammarActionArg<'g>>) -> Self {
        Self { name, args }
    }
}

impl<'g> GrammarActionArg<'g> {
    /// Creates a named grammar action argument.
    #[must_use]
    pub fn new(name: &'g str, value: GrammarActionTerm<'g>) -> Self {
        Self { name, value }
    }
}

impl<'g> RuleExpression<'g> {
    /// Creates a rule reference.
    #[must_use]
    pub const fn rule(name: &'g str) -> Self {
        Self::Rule(name)
    }

    /// Creates a literal token/delimiter expression.
    #[must_use]
    pub const fn literal(value: &'g str) -> Self {
        Self::Literal(value)
    }

    /// Creates a character class expression.
    #[must_use]
    pub const fn character_class(value: &'g str) -> Self {
        Self::CharacterClass(value)
    }

    /// Creates an opaque-region expression that consumes until `value`.
    #[must_use]
    pub const fn until_literal(value: &'g str) -> Self {
        Self::UntilLiteral(value)
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
    pub fn transform(input: Self, output: TransformOutput<'g>) -> Self {
        Self::Transform {
            input: Box::new(input),
            output,
        }
    }

    /// Creates a positive lookahead expression.
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "constructor mirrors variant name"
    )]
    pub fn and(item: Self) -> Self {
        Self::And(Box::new(item))
    }

    /// Creates a negative lookahead expression.
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "constructor mirrors variant name"
    )]
    pub fn not(item: Self) -> Self {
        Self::Not(Box::new(item))
    }

    /// Creates a committed-choice cut expression.
    #[must_use]
    pub fn cut(item: Self) -> Self {
        Self::Cut(Box::new(item))
    }

    /// Creates a named subexpression binding.
    #[must_use]
    pub fn named(name: &'g str, item: Self) -> Self {
        Self::Named {
            name,
            inner: Box::new(item),
        }
    }

    /// Creates a parametric rule invocation.
    #[must_use]
    pub fn apply(rule: &'g str, args: Vec<Self>) -> Self {
        Self::Apply { rule, args }
    }

    /// Creates a parent-grammar rule reference.
    #[must_use]
    pub const fn super_rule(name: &'g str) -> Self {
        Self::Super(name)
    }
}

impl fmt::Display for RuleExpression<'_> {
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
            Self::And(item) => write!(formatter, "&{}", GroupedRuleExpression(item)),
            Self::Not(item) => write!(formatter, "!{}", GroupedRuleExpression(item)),
            Self::Cut(item) => write!(formatter, "^{}", GroupedRuleExpression(item)),
            Self::Named { name, inner } => {
                write!(formatter, "{name}:{}", GroupedRuleExpression(inner))
            }
            Self::Apply { rule, args } => {
                formatter.write_str(rule)?;
                formatter.write_str("<")?;
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{arg}")?;
                }
                formatter.write_str(">")
            }
            Self::Super(name) => write!(formatter, "super.{name}"),
        }
    }
}

impl fmt::Display for TransformOutput<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Action(action) => write!(formatter, "{action}"),
            Self::Literal(value) => write!(formatter, "`{value}`"),
            Self::Symbol(value) => formatter.write_str(value),
        }
    }
}

impl fmt::Display for GrammarAction<'_> {
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

impl fmt::Display for GrammarActionTerm<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capture { rule } => formatter.write_str(rule),
            Self::CaptureAll { rule } => write!(formatter, "{rule}*"),
            Self::CaptureByName { name } => formatter.write_str(name),
            Self::Literal(value) => write!(formatter, "{value:?}"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::List(items) => {
                formatter.write_str("[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{item}")?;
                }
                formatter.write_str("]")
            }
            Self::Record(args) => {
                formatter.write_str("{")?;
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(", ")?;
                    }
                    write!(formatter, "{}: {}", arg.name, arg.value)?;
                }
                formatter.write_str("}")
            }
            Self::Call(action) => write!(formatter, "{action}"),
        }
    }
}

fn collapse_expression<'g>(expression: RuleExpression<'g>) -> RuleExpression<'g> {
    match expression {
        RuleExpression::Sequence(mut items) | RuleExpression::Choice(mut items)
            if items.len() == 1 =>
        {
            items.remove(0)
        }
        expression => expression,
    }
}

struct GroupedRuleExpression<'a, 'g>(&'a RuleExpression<'g>);

impl fmt::Display for GroupedRuleExpression<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RuleExpression::Choice(_) | RuleExpression::Transform { .. } => {
                write!(formatter, "({})", self.0)
            }
            expression => write!(formatter, "{expression}"),
        }
    }
}

/// Whether a rule participates in default trivia skipping.
///
/// [`SourceAuthoredRule::trivia`] selects between leaving whitespace and
/// `//` line comments, including `///` documentation comments, to the kernel
/// ([`TriviaMode::Default`]) and treating them as part of the matched source
/// ([`TriviaMode::Raw`]). Raw rules are
/// useful for tokens with exact internal structure, such as string literals.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add modes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum TriviaMode {
    /// Whitespace and `//` line comments are skipped before every literal and
    /// at the rule boundary.
    #[default]
    Default,
    /// Whitespace and `//` line comments are not skipped at the rule boundary
    /// or before literals inside this rule's body. Rule references inside a
    /// raw rule are still evaluated according to the called rule's own trivia
    /// mode.
    Raw,
}

/// Inert metadata attached to a grammar rule.
///
/// Rule annotations are parsed from modifier tokens in grammar source, such as
/// `rule field @role=record-field = ...` or `rule item @recovery-sync = ...`.
/// The grammar kernel preserves them, but does not interpret their names or
/// values while matching. Callers can use annotations to carry editor,
/// formatter, recovery, documentation, or semantic-role hints alongside the
/// neutral grammar model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuleAnnotation<'g> {
    /// Annotation name without the leading `@`.
    pub name: &'g str,
    /// Optional annotation value after `=`.
    pub value: Option<&'g str>,
}

impl<'g> RuleAnnotation<'g> {
    /// Creates a rule annotation.
    #[must_use]
    pub const fn new(name: &'g str, value: Option<&'g str>) -> Self {
        Self { name, value }
    }

    /// Creates a flag-style annotation with no value.
    #[must_use]
    pub const fn flag(name: &'g str) -> Self {
        Self::new(name, None)
    }

    /// Returns the annotation name.
    #[must_use]
    pub const fn name(&self) -> &'g str {
        self.name
    }

    /// Returns the annotation value, if any.
    #[must_use]
    pub const fn value(&self) -> Option<&'g str> {
        self.value
    }
}

impl fmt::Display for RuleAnnotation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.value {
            Some(value) => write!(formatter, "@{}={value}", self.name),
            None => write!(formatter, "@{}", self.name),
        }
    }
}

/// A rule parsed from grammar source.
///
/// [`parse_grammar_source`] returns a list of these. Pass that list to
/// [`interpret_region`] or [`interpret_rule_error`] to match input. Callers may
/// adjust policy fields, such as [`SourceAuthoredRule::excluded_text`], before
/// interpreting.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add per-rule
/// policy fields (such as trivia mode) without forcing a breaking change.
/// Construct values via [`parse_grammar_source`] and tweak through provided
/// builder methods.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct SourceAuthoredRule<'g> {
    /// Rule name.
    pub name: &'g str,
    /// Rule expression consumed by [`interpret_region`].
    pub expression: RuleExpression<'g>,
    /// Optional deterministic action descriptor parsed from `=> ...`.
    ///
    /// This crate records the action so callers can interpret it later; the
    /// matcher does not execute it.
    pub action: Option<GrammarAction<'g>>,
    /// Complete matched text values rejected after the expression matches.
    ///
    /// This is a neutral escape hatch for identifier-like rules that need a
    /// caller-owned reserved-word policy without baking any one language into
    /// the grammar kernel.
    pub excluded_text: Vec<&'g str>,
    /// Inert rule annotations parsed from `@name` or `@name=value`
    /// modifier tokens in the rule header.
    pub annotations: Vec<RuleAnnotation<'g>>,
    /// Source span in the grammar source.
    pub source_span: Span,
    /// Per-rule trivia policy. See [`TriviaMode`].
    pub trivia: TriviaMode,
    /// Formal parameter names for a parametric rule definition.
    ///
    /// Surface syntax: `rule list-of<elem, sep> = ...`. Each parameter
    /// is a name that, when referenced as a [`RuleExpression::Rule`]
    /// inside the body, is substituted with the argument supplied by
    /// the invoking [`RuleExpression::Apply`]. Empty for non-parametric
    /// rules.
    pub params: Vec<&'g str>,
}

impl<'g> SourceAuthoredRule<'g> {
    /// Returns this rule with complete matched text exclusions attached.
    #[must_use]
    pub fn with_excluded_text(mut self, excluded_text: Vec<&'g str>) -> Self {
        self.excluded_text = excluded_text;
        self
    }

    /// Returns the first annotation with `name`.
    #[must_use]
    pub fn annotation(&self, name: &str) -> Option<&RuleAnnotation<'g>> {
        self.annotations
            .iter()
            .find(|annotation| annotation.name == name)
    }

    /// Returns the first value for annotation `name`.
    #[must_use]
    pub fn annotation_value(&self, name: &str) -> Option<&'g str> {
        self.annotation(name)
            .and_then(|annotation| annotation.value)
    }

    /// Returns every annotation with `name`, in source order.
    pub fn annotations_named<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a RuleAnnotation<'g>> + 'a {
        self.annotations
            .iter()
            .filter(move |annotation| annotation.name == name)
    }
}

/// A grammar with a precomputed name index for fast rule lookup.
///
/// [`CompiledGrammar::compile`] takes a list of rules (typically from
/// [`parse_grammar_source`]) and builds a sorted index alongside them. The
/// `*_compiled` matching entry points ([`interpret_region_compiled`],
/// [`inspect_region_compiled`], [`interpret_rule_error_compiled`]) use that
/// index for O(log n) rule lookups instead of the linear scan used by the
/// slice-taking entry points.
///
/// Build once per grammar and reuse across many parses. The compiled form
/// borrows nothing extra from the parsed grammar source; it owns the rule
/// list and the sorted index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledGrammar<'g> {
    rules: Vec<SourceAuthoredRule<'g>>,
    /// Indices into `rules`, sorted by `rules[i].name`. Ties (which only
    /// occur if a caller manually constructs duplicates outside the
    /// diagnostic-filtered [`parse_grammar_source_with_diagnostics`] path) are
    /// broken by original position so [`CompiledGrammar::rule`] still returns
    /// the first declaration to match the slice-based lookup behavior.
    sorted_indices: Vec<usize>,
    /// Parent grammar in the inheritance chain, set by
    /// [`CompiledGrammar::compile_with_parent`]. Used by the matcher
    /// to route every inherited rule dispatch to the rule's
    /// authoring ancestor — both ordinary `Rule(...)` lookups and
    /// explicit [`RuleExpression::Super`] references walk this chain
    /// via `ancestor()` so a rule body always runs in the grammar
    /// that authored it.
    parent: Option<Box<CompiledGrammar<'g>>>,
    /// Parallel to `rules`: the inheritance depth at which each rule
    /// was originally authored. 0 means this grammar's own direct
    /// rule; 1 means the immediate parent; 2 means the grandparent;
    /// etc. The matcher uses this together with `parent` to switch
    /// the active lookup to the rule's authoring ancestor before
    /// running the rule body — so both ordinary inherited dispatch
    /// and `super` resolve through the right grammar scope.
    rule_owner_depths: Vec<u32>,
}

impl<'g> CompiledGrammar<'g> {
    /// Builds a compiled grammar from a rule list.
    #[must_use]
    pub fn compile(rules: Vec<SourceAuthoredRule<'g>>) -> Self {
        let mut sorted_indices: Vec<usize> = (0..rules.len()).collect();
        sorted_indices.sort_by(|&a, &b| rules[a].name.cmp(rules[b].name).then(a.cmp(&b)));
        let rule_owner_depths = vec![0; rules.len()];
        Self {
            rules,
            sorted_indices,
            parent: None,
            rule_owner_depths,
        }
    }

    /// Builds a compiled grammar that inherits from `parent`.
    ///
    /// Child rules take precedence over parent rules with the same name
    /// (override). Parent rules that aren't overridden are included so
    /// that references from child bodies resolve through the chain.
    /// The parent itself is cloned in flattened form; further-up
    /// ancestors are already merged into `parent` if it was itself built
    /// via `compile_with_parent`.
    ///
    /// This entry point is infallible: it does not check whether the
    /// merged grammar is structurally valid (for example whether every
    /// [`RuleExpression::Apply`] supplies the right number of
    /// arguments). The recommended workflow is:
    ///
    /// 1. Parse source with [`parse_grammar_source_full`] or
    ///    [`parse_grammar_source_with_diagnostics`]; source-time
    ///    diagnostics already catch arity mismatches for parametric
    ///    rules defined in the same source.
    /// 2. Build the compiled form with this method, or with
    ///    [`try_compile_with_parent`] if the caller wants inherited
    ///    arity mismatches to surface as errors at compile time.
    /// 3. Optionally run [`validate_parametric_arity`] against the
    ///    compiled grammar before interpreting, to catch cross-grammar
    ///    issues this method skipped.
    ///
    /// [`try_compile_with_parent`]: CompiledGrammar::try_compile_with_parent
    /// [`validate_parametric_arity`]: CompiledGrammar::validate_parametric_arity
    #[must_use]
    pub fn compile_with_parent(
        rules: Vec<SourceAuthoredRule<'g>>,
        parent: &CompiledGrammar<'g>,
    ) -> Self {
        // Child's own rules are at depth 0 (authored in this grammar).
        // Parent rules not overridden by the child are flattened in
        // for fast `Rule(name)` lookup; their depth is `parent's
        // depth for that rule + 1`, so the matcher can route `super`
        // references inside the inherited body to the rule's
        // authoring ancestor rather than to the child's parent.
        let mut merged = rules;
        let mut depths = vec![0_u32; merged.len()];
        for (parent_index, parent_rule) in parent.rules.iter().enumerate() {
            if !merged.iter().any(|rule| rule.name == parent_rule.name) {
                merged.push(parent_rule.clone());
                depths.push(parent.rule_owner_depths[parent_index] + 1);
            }
        }
        let mut sorted_indices: Vec<usize> = (0..merged.len()).collect();
        sorted_indices.sort_by(|&a, &b| merged[a].name.cmp(merged[b].name).then(a.cmp(&b)));
        Self {
            rules: merged,
            sorted_indices,
            parent: Some(Box::new(parent.clone())),
            rule_owner_depths: depths,
        }
    }

    /// Strict variant of [`compile_with_parent`] that runs
    /// [`validate_parametric_arity`] on the merged grammar and returns
    /// the diagnostics as an `Err` instead of producing a possibly
    /// inconsistent grammar.
    ///
    /// Returns `Ok(compiled)` when the merged grammar passes all
    /// post-compile validations the kernel currently runs (today: only
    /// parametric arity). Returns `Err(diagnostics)` when at least one
    /// validation failed; callers can render the diagnostics and refuse
    /// to interpret.
    ///
    /// Use this when the caller cannot tolerate silently degraded
    /// interpretation of an inherited grammar. Prefer the infallible
    /// [`compile_with_parent`] when the caller wants to inspect a
    /// grammar that may be in-progress.
    ///
    /// [`compile_with_parent`]: CompiledGrammar::compile_with_parent
    /// [`validate_parametric_arity`]: CompiledGrammar::validate_parametric_arity
    pub fn try_compile_with_parent(
        rules: Vec<SourceAuthoredRule<'g>>,
        parent: &CompiledGrammar<'g>,
    ) -> Result<Self, Vec<GrammarSourceDiagnostic>> {
        let compiled = Self::compile_with_parent(rules, parent);
        let diagnostics = compiled.validate_parametric_arity();
        if diagnostics.is_empty() {
            Ok(compiled)
        } else {
            Err(diagnostics)
        }
    }

    /// Returns the underlying rule list in its original declaration order.
    #[must_use]
    pub fn rules(&self) -> &[SourceAuthoredRule<'g>] {
        &self.rules
    }

    /// Looks up the first rule with `name`, returning `None` when not found.
    #[must_use]
    pub fn rule(&self, name: &str) -> Option<&SourceAuthoredRule<'g>> {
        self.rule_with_depth(name).map(|(rule, _)| rule)
    }

    /// Like [`rule`] but also returns the inheritance depth at which
    /// the rule was originally authored. The matcher uses the depth to
    /// route `super` references to the rule's authoring ancestor.
    ///
    /// [`rule`]: CompiledGrammar::rule
    fn rule_with_depth(&self, name: &str) -> Option<(&SourceAuthoredRule<'g>, u32)> {
        let rules = self.rules.as_slice();
        let indices = self.sorted_indices.as_slice();
        let position = indices.partition_point(|&idx| rules[idx].name < name);
        indices.get(position).and_then(|&idx| {
            let rule = &rules[idx];
            (rule.name == name).then_some((rule, self.rule_owner_depths[idx]))
        })
    }

    /// Walks the parent chain `depth` levels and returns the resulting
    /// grammar (or `self` for `depth == 0`). Returns `None` when the
    /// chain is shorter than `depth`.
    fn ancestor(&self, depth: u32) -> Option<&CompiledGrammar<'g>> {
        let mut current = self;
        for _ in 0..depth {
            current = current.parent.as_deref()?;
        }
        Some(current)
    }

    /// Returns arity-mismatch diagnostics for every parametric Apply
    /// invocation in the grammar whose target is part of this compiled
    /// rule set.
    ///
    /// Source-time validation (`parse_grammar_source_with_diagnostics`)
    /// already catches arity mismatches whose target is declared in the
    /// same source. Run this after [`compile_with_parent`] to surface
    /// cross-grammar arity mismatches that the source-time pass had to
    /// skip because the target rule lived in the parent. Invocations
    /// whose target is not present in the merged rule set are still
    /// skipped here — the reference is unresolved and will surface as
    /// a normal parse-time rule-lookup failure.
    ///
    /// Use [`try_compile_with_parent`] when the caller wants this check
    /// to run automatically and fail closed instead of returning a
    /// possibly inconsistent grammar.
    ///
    /// [`compile_with_parent`]: CompiledGrammar::compile_with_parent
    /// [`try_compile_with_parent`]: CompiledGrammar::try_compile_with_parent
    #[must_use]
    pub fn validate_parametric_arity(&self) -> Vec<GrammarSourceDiagnostic> {
        let mut diagnostics = Vec::new();
        for rule in &self.rules {
            walk_apply_nodes(&rule.expression, &mut |target, args| {
                if let Some(declared) = self.rule(target)
                    && declared.params.len() != args.len()
                {
                    diagnostics.push(GrammarSourceDiagnostic::new(
                        rule.source_span,
                        GrammarSourceDiagnosticCode::ParametricRuleArityMismatch,
                    ));
                }
            });
        }
        diagnostics
    }

    fn lookup(&self) -> RuleLookup<'_, 'g> {
        RuleLookup {
            rules: &self.rules,
            sorted_indices: Some(&self.sorted_indices),
            grammar: Some(self),
        }
    }
}

#[derive(Clone, Copy)]
struct RuleLookup<'a, 'g> {
    rules: &'a [SourceAuthoredRule<'g>],
    sorted_indices: Option<&'a [usize]>,
    /// Compiled grammar this lookup belongs to. `None` for slice-mode
    /// lookups built from raw rule slices, which do not support
    /// inheritance (`Super`) since there is no parent chain.
    grammar: Option<&'a CompiledGrammar<'g>>,
}

impl<'a, 'g> RuleLookup<'a, 'g> {
    fn from_slice(rules: &'a [SourceAuthoredRule<'g>]) -> Self {
        Self {
            rules,
            sorted_indices: None,
            grammar: None,
        }
    }

    fn rule(self, name: &str) -> Option<&'a SourceAuthoredRule<'g>> {
        self.rule_with_depth(name).map(|(rule, _)| rule)
    }

    fn rule_with_depth(self, name: &str) -> Option<(&'a SourceAuthoredRule<'g>, u32)> {
        if let Some(grammar) = self.grammar {
            grammar.rule_with_depth(name)
        } else if let Some(indices) = self.sorted_indices {
            let position = indices.partition_point(|&idx| self.rules[idx].name < name);
            indices.get(position).and_then(|&idx| {
                let rule = &self.rules[idx];
                (rule.name == name).then_some((rule, 0))
            })
        } else {
            self.rules
                .iter()
                .find(|rule| rule.name == name)
                .map(|rule| (rule, 0))
        }
    }

    fn ancestor(self, depth: u32) -> Option<RuleLookup<'a, 'g>> {
        if depth == 0 {
            return Some(self);
        }
        self.grammar
            .and_then(|g| g.ancestor(depth))
            .map(|g| g.lookup())
    }

    fn parent(self) -> Option<RuleLookup<'a, 'g>> {
        self.grammar
            .and_then(|g| g.parent.as_deref())
            .map(|p| p.lookup())
    }

    /// Resolves `name` to its rule definition and the lookup scope
    /// the rule was authored in. Both `Rule` dispatch and parametric
    /// `Apply` go through this helper so the "switch active lookup
    /// to the rule's authoring ancestor" invariant cannot drift
    /// between call sites.
    ///
    /// Panics when [`CompiledGrammar::rule_owner_depths`] records a
    /// depth that the parent chain cannot satisfy — that would mean
    /// a kernel invariant was broken at compile time, not that the
    /// caller passed bad data. Returning `None` is reserved for the
    /// "name not found" case.
    fn resolve_rule(self, name: &str) -> Option<(&'a SourceAuthoredRule<'g>, RuleLookup<'a, 'g>)> {
        let (rule, depth) = self.rule_with_depth(name)?;
        let active = self
            .ancestor(depth)
            .expect("rule_owner_depths invariant: ancestor must exist for any recorded depth");
        Some((rule, active))
    }

    fn memo_scope(self) -> usize {
        self.grammar.map_or_else(
            || self.rules.as_ptr().cast::<()>() as usize,
            |grammar| core::ptr::from_ref(grammar).cast::<()>() as usize,
        )
    }
}

/// Structural role of a [`RuleNode`].
///
/// [`RuleNode::role`] returns this value so callers can distinguish named
/// grammar-rule nodes from anonymous syntax leaves without parsing
/// [`RuleNodeKind`] labels themselves. This is presentation and query metadata;
/// it does not affect matching.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add roles without
/// forcing a breaking change on downstream pattern matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuleNodeRole {
    /// A node produced by a named grammar rule.
    NamedRule,
    /// A node produced by a literal token expression.
    Literal,
    /// A node produced by a character-class expression.
    CharacterClass,
    /// A node produced by an opaque `until(...)` expression.
    OpaqueRegion,
    /// A synthetic node inserted by recovery to cover source that
    /// the matcher could not parse. Only produced by recovering
    /// entry points such as [`interpret_region_recovering`]; never
    /// produced by ordinary parsing.
    RecoveryError,
}

/// Stable kind metadata for a [`RuleNode`].
///
/// The kind borrows names, literal tokens, and delimiter text from the grammar
/// rule that produced the node. [`RuleNode::text`] separately borrows the
/// matched bytes from the parsed source.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add node kinds
/// without forcing a breaking change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RuleNodeKind<'g> {
    /// A node produced by a named grammar rule.
    Rule {
        /// Matched grammar rule name.
        name: &'g str,
    },
    /// A node produced by a literal expression.
    Literal {
        /// Literal token from the grammar.
        value: &'g str,
    },
    /// A node produced by a character-class expression.
    CharacterClass {
        /// Character-class expression from the grammar.
        value: &'g str,
    },
    /// A node produced by an opaque `until(...)` expression.
    OpaqueRegion {
        /// Delimiter that ended the opaque region.
        delimiter: &'g str,
    },
    /// Synthetic node inserted by recovering parse paths to cover a
    /// source region the matcher could not parse. The node's
    /// [`RuleNode::span`] and [`RuleNode::text`] describe the
    /// skipped region. Only produced by recovering entry points
    /// such as [`interpret_region_recovering`].
    RecoveryError,
}

impl<'g> RuleNodeKind<'g> {
    /// Returns the structural role for this node kind.
    #[must_use]
    pub const fn role(self) -> RuleNodeRole {
        match self {
            Self::Rule { .. } => RuleNodeRole::NamedRule,
            Self::Literal { .. } => RuleNodeRole::Literal,
            Self::CharacterClass { .. } => RuleNodeRole::CharacterClass,
            Self::OpaqueRegion { .. } => RuleNodeRole::OpaqueRegion,
            Self::RecoveryError => RuleNodeRole::RecoveryError,
        }
    }

    /// Returns the named grammar rule, if this is a named-rule node.
    #[must_use]
    pub const fn name(self) -> Option<&'g str> {
        match self {
            Self::Rule { name } => Some(name),
            Self::Literal { .. }
            | Self::CharacterClass { .. }
            | Self::OpaqueRegion { .. }
            | Self::RecoveryError => None,
        }
    }

    /// Returns the literal grammar token, if this is a literal node.
    #[must_use]
    pub const fn literal(self) -> Option<&'g str> {
        match self {
            Self::Literal { value } => Some(value),
            Self::Rule { .. }
            | Self::CharacterClass { .. }
            | Self::OpaqueRegion { .. }
            | Self::RecoveryError => None,
        }
    }

    /// Returns the character-class expression, if this is a character-class node.
    #[must_use]
    pub const fn character_class(self) -> Option<&'g str> {
        match self {
            Self::CharacterClass { value } => Some(value),
            Self::Rule { .. }
            | Self::Literal { .. }
            | Self::OpaqueRegion { .. }
            | Self::RecoveryError => None,
        }
    }

    /// Returns the opaque delimiter, if this is an opaque-region node.
    #[must_use]
    pub const fn opaque_delimiter(self) -> Option<&'g str> {
        match self {
            Self::OpaqueRegion { delimiter } => Some(delimiter),
            Self::Rule { .. }
            | Self::Literal { .. }
            | Self::CharacterClass { .. }
            | Self::RecoveryError => None,
        }
    }

    /// Returns the stable display label used in diagnostics and reports.
    #[must_use]
    pub fn label(self) -> RuleNodeKindLabel<'g> {
        RuleNodeKindLabel(self)
    }
}

/// Display wrapper returned by [`RuleNodeKind::label`].
pub struct RuleNodeKindLabel<'g>(RuleNodeKind<'g>);

impl fmt::Display for RuleNodeKindLabel<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RuleNodeKind::Rule { name } => formatter.write_str(name),
            RuleNodeKind::Literal { value } => write!(formatter, "literal:{value}"),
            RuleNodeKind::CharacterClass { value } => write!(formatter, "class:{value}"),
            RuleNodeKind::OpaqueRegion { delimiter } => write!(formatter, "until:{delimiter}"),
            RuleNodeKind::RecoveryError => formatter.write_str("recovery-error"),
        }
    }
}

impl RuleNodeRole {
    /// Returns the stable role token used in dumps and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NamedRule => "named-rule",
            Self::Literal => "literal",
            Self::CharacterClass => "character-class",
            Self::OpaqueRegion => "opaque-region",
            Self::RecoveryError => "recovery-error",
        }
    }
}

impl fmt::Display for RuleNodeRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Parse tree node produced by interpreted matching.
///
/// [`interpret_region`] returns a [`Parse`] whose [`Parse::root`] is a
/// `RuleNode`. Callers usually inspect these nodes directly or combine them
/// with [`GrammarAction`] descriptors from matching [`SourceAuthoredRule`]
/// values.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add per-node
/// metadata without forcing a breaking change.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RuleNode<'g, 's> {
    /// Grammar node kind.
    pub kind: RuleNodeKind<'g>,
    /// Matched source span.
    pub span: Span,
    /// Matched source text borrowed from the parsed region.
    pub text: &'s str,
    /// Child rule/literal/class nodes produced while matching this rule.
    pub children: Vec<RuleNode<'g, 's>>,
    /// Optional binding name attached by a parent `name:expr` wrapper.
    ///
    /// When a [`RuleExpression::Named`] participates in the match, the
    /// matcher tags each child this expression produced with the binding
    /// `name`. Actions can resolve such children by name via
    /// [`RuleNode::find_by_binding`].
    pub binding_name: Option<&'g str>,
}

impl<'g, 's> RuleNode<'g, 's> {
    /// Returns the structural role of this parse-tree node.
    #[must_use]
    pub fn role(&self) -> RuleNodeRole {
        self.kind.role()
    }

    /// Returns the grammar rule name when this is a named-rule node.
    #[must_use]
    pub fn name(&self) -> Option<&'g str> {
        self.kind.name()
    }

    /// Returns true when this node is a named grammar rule with `rule_name`.
    #[must_use]
    pub fn is_rule(&self, rule_name: &str) -> bool {
        self.name() == Some(rule_name)
    }

    /// Returns true when this node came from a named grammar rule.
    #[must_use]
    pub fn is_named(&self) -> bool {
        self.role() == RuleNodeRole::NamedRule
    }

    /// Returns true when this node came from a literal, class, or opaque syntax
    /// expression rather than a named grammar rule.
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        !self.is_named()
    }

    /// Returns the literal token text for literal nodes.
    #[must_use]
    pub fn literal(&self) -> Option<&str> {
        self.kind.literal()
    }

    /// Returns the character-class expression for character-class nodes.
    #[must_use]
    pub fn character_class(&self) -> Option<&str> {
        self.kind.character_class()
    }

    /// Returns the delimiter for opaque-region nodes.
    #[must_use]
    pub fn opaque_delimiter(&self) -> Option<&str> {
        self.kind.opaque_delimiter()
    }

    /// Returns a preorder iterator over this node and all descendants.
    #[must_use]
    pub fn walk(&self) -> RuleNodeWalk<'_, 'g, 's> {
        RuleNodeWalk { stack: vec![self] }
    }

    /// Returns the first named grammar-rule node with `rule_name`.
    #[must_use]
    pub fn find_rule(&self, rule_name: &str) -> Option<&RuleNode<'g, 's>> {
        self.walk().find(|node| node.is_rule(rule_name))
    }

    /// Returns all named grammar-rule nodes with `rule_name`.
    #[must_use]
    pub fn find_rules(&self, rule_name: &str) -> Vec<&RuleNode<'g, 's>> {
        self.walk().filter(|node| node.is_rule(rule_name)).collect()
    }

    /// Returns all nodes with the requested structural role.
    #[must_use]
    pub fn find_by_role(&self, role: RuleNodeRole) -> Vec<&RuleNode<'g, 's>> {
        self.walk().filter(|node| node.role() == role).collect()
    }

    /// Returns the first direct child whose binding name matches.
    ///
    /// Bindings are attached by a parent `name:expr` wrapper; see
    /// [`RuleExpression::Named`]. This helper searches only the direct
    /// children of this node — the typical case where an action wants
    /// the child a binding refers to.
    #[must_use]
    pub fn find_by_binding(&self, name: &str) -> Option<&RuleNode<'g, 's>> {
        self.children
            .iter()
            .find(|child| child.binding_name == Some(name))
    }

    /// Returns the smallest node that fully contains `span`.
    ///
    /// This is useful when a caller has a source byte range and wants the most
    /// specific parse-tree node that explains it.
    #[must_use]
    pub fn smallest_covering_span(&self, span: Span) -> Option<&RuleNode<'g, 's>> {
        if !self.span.contains_span(span) {
            return None;
        }
        self.children
            .iter()
            .filter_map(|child| child.smallest_covering_span(span))
            .min_by_key(|node| node.span.end.saturating_sub(node.span.start))
            .or(Some(self))
    }

    /// Returns every node in this subtree whose span is fully contained in
    /// `span`, in preorder, including this node when it qualifies.
    ///
    /// Useful when a caller has an edit region or selection range and wants
    /// every parse-tree node that is contained in it.
    #[must_use]
    pub fn descendants_in_span(&self, span: Span) -> Vec<&RuleNode<'g, 's>> {
        self.walk()
            .filter(|node| span.contains_span(node.span))
            .collect()
    }

    /// Returns the path of nodes covering `offset`, root first.
    ///
    /// The returned vector starts at this node and walks down through the
    /// children that contain `offset`, ending at the smallest covering node.
    /// Returns an empty vector when this node does not contain `offset`.
    #[must_use]
    pub fn path_to_offset(&self, offset: usize) -> Vec<&RuleNode<'g, 's>> {
        let mut path = Vec::new();
        if !self.span.contains_offset(offset) {
            return path;
        }
        let mut current = self;
        path.push(current);
        loop {
            let next = current
                .children
                .iter()
                .find(|child| child.span.contains_offset(offset));
            match next {
                Some(child) => {
                    path.push(child);
                    current = child;
                }
                None => return path,
            }
        }
    }
}

impl RuleNode<'_, '_> {
    /// Returns the deepest *named-rule* node whose span contains `offset`.
    ///
    /// Filters out literal, character-class, and opaque-region nodes
    /// so the result is the most specific named grammar rule active
    /// at the offset — useful for editor consumers that want to ask
    /// "what rule is the cursor in?" without seeing punctuation
    /// tokens.
    ///
    /// In a successful root parse this practically always returns
    /// some named rule: when the offset falls on punctuation or
    /// trivia, the walk falls back to the enclosing rule whose span
    /// covers that text. `None` is reserved for the case where
    /// `offset` lies entirely outside this subtree's span.
    #[must_use]
    pub fn rule_at_offset(&self, offset: usize) -> Option<&RuleNode<'_, '_>> {
        self.path_to_offset(offset)
            .into_iter()
            .rev()
            .find(|node| node.is_named())
    }
}

/// Owned, lifetime-free parse tree that mirrors [`RuleNode`].
///
/// The borrowed [`RuleNode`] is convenient inside the parser but it
/// borrows from both the grammar source and the parsed text, so it
/// can't be carried across those lifetimes. [`OwnedRuleNode`] is the
/// long-lived form for downstream consumers (compiler frontend,
/// editors, doc generators): produced once via
/// [`RuleNode::to_owned_tree`] and queried freely thereafter.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add
/// per-node metadata without breaking downstream pattern matches.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct OwnedRuleNode {
    /// Grammar node kind, with owned strings.
    pub kind: OwnedRuleNodeKind,
    /// Matched source span.
    pub span: Span,
    /// Matched source text (owned).
    pub text: String,
    /// Child rule/literal/class nodes produced while matching this rule.
    pub children: Vec<OwnedRuleNode>,
    /// Optional binding name attached by a parent `name:expr` wrapper.
    pub binding_name: Option<String>,
}

/// Owned analogue of [`RuleNodeKind`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OwnedRuleNodeKind {
    /// A node produced by a named grammar rule.
    Rule {
        /// Matched grammar rule name.
        name: String,
    },
    /// A node produced by a literal expression.
    Literal {
        /// Literal token from the grammar.
        value: String,
    },
    /// A node produced by a character-class expression.
    CharacterClass {
        /// Character-class expression from the grammar.
        value: String,
    },
    /// A node produced by an opaque `until(...)` expression.
    OpaqueRegion {
        /// Delimiter that ended the opaque region.
        delimiter: String,
    },
    /// Synthetic node inserted by recovering parse paths to cover a
    /// region the matcher could not parse. See [`RuleNodeKind::RecoveryError`].
    RecoveryError,
}

impl OwnedRuleNodeKind {
    /// Returns the structural role for this node kind.
    #[must_use]
    pub const fn role(&self) -> RuleNodeRole {
        match self {
            Self::Rule { .. } => RuleNodeRole::NamedRule,
            Self::Literal { .. } => RuleNodeRole::Literal,
            Self::CharacterClass { .. } => RuleNodeRole::CharacterClass,
            Self::OpaqueRegion { .. } => RuleNodeRole::OpaqueRegion,
            Self::RecoveryError => RuleNodeRole::RecoveryError,
        }
    }

    /// Returns the grammar rule name when this is a named-rule kind.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Rule { name } => Some(name.as_str()),
            _ => None,
        }
    }
}

impl OwnedRuleNode {
    /// Builds an owned copy of `node`, cloning all borrowed strings.
    #[must_use]
    pub fn from_rule_node(node: &RuleNode<'_, '_>) -> Self {
        Self {
            kind: match node.kind {
                RuleNodeKind::Rule { name } => OwnedRuleNodeKind::Rule {
                    name: name.to_string(),
                },
                RuleNodeKind::Literal { value } => OwnedRuleNodeKind::Literal {
                    value: value.to_string(),
                },
                RuleNodeKind::CharacterClass { value } => OwnedRuleNodeKind::CharacterClass {
                    value: value.to_string(),
                },
                RuleNodeKind::OpaqueRegion { delimiter } => OwnedRuleNodeKind::OpaqueRegion {
                    delimiter: delimiter.to_string(),
                },
                RuleNodeKind::RecoveryError => OwnedRuleNodeKind::RecoveryError,
            },
            span: node.span,
            text: node.text.to_string(),
            children: node.children.iter().map(Self::from_rule_node).collect(),
            binding_name: node.binding_name.map(ToString::to_string),
        }
    }

    /// Returns the structural role of this parse-tree node.
    #[must_use]
    pub fn role(&self) -> RuleNodeRole {
        self.kind.role()
    }

    /// Returns the grammar rule name when this is a named-rule node.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.kind.name()
    }

    /// Returns true when this node is a named grammar rule with `rule_name`.
    #[must_use]
    pub fn is_rule(&self, rule_name: &str) -> bool {
        self.name() == Some(rule_name)
    }

    /// Returns true when this node came from a named grammar rule.
    #[must_use]
    pub fn is_named(&self) -> bool {
        self.role() == RuleNodeRole::NamedRule
    }

    /// Returns a preorder iterator over this node and all descendants.
    pub fn walk(&self) -> OwnedRuleNodeWalk<'_> {
        OwnedRuleNodeWalk { stack: vec![self] }
    }

    /// Returns the smallest node that fully contains `span`.
    #[must_use]
    pub fn smallest_covering_span(&self, span: Span) -> Option<&OwnedRuleNode> {
        if !self.span.contains_span(span) {
            return None;
        }
        self.children
            .iter()
            .filter_map(|child| child.smallest_covering_span(span))
            .min_by_key(|node| node.span.end.saturating_sub(node.span.start))
            .or(Some(self))
    }

    /// Returns every node in this subtree whose span is fully contained
    /// in `span`, in preorder.
    #[must_use]
    pub fn descendants_in_span(&self, span: Span) -> Vec<&OwnedRuleNode> {
        self.walk()
            .filter(|node| span.contains_span(node.span))
            .collect()
    }

    /// Returns the path of nodes covering `offset`, root first.
    #[must_use]
    pub fn path_to_offset(&self, offset: usize) -> Vec<&OwnedRuleNode> {
        let mut path = Vec::new();
        if !self.span.contains_offset(offset) {
            return path;
        }
        let mut current = self;
        path.push(current);
        loop {
            let next = current
                .children
                .iter()
                .find(|child| child.span.contains_offset(offset));
            match next {
                Some(child) => {
                    path.push(child);
                    current = child;
                }
                None => return path,
            }
        }
    }

    /// Returns the deepest *named-rule* node whose span contains `offset`.
    ///
    /// Filters out literal, character-class, and opaque-region nodes
    /// so the result is the most specific named grammar rule active
    /// at the offset — useful for editor consumers that want to ask
    /// "what rule is the cursor in?" without seeing punctuation
    /// tokens.
    ///
    /// In a successful root parse this practically always returns
    /// some named rule: when the offset falls on punctuation or
    /// trivia, the walk falls back to the enclosing rule whose span
    /// covers that text. `None` is reserved for the case where
    /// `offset` lies entirely outside this subtree's span.
    #[must_use]
    pub fn rule_at_offset(&self, offset: usize) -> Option<&OwnedRuleNode> {
        self.path_to_offset(offset)
            .into_iter()
            .rev()
            .find(|node| node.is_named())
    }
}

impl RuleNode<'_, '_> {
    /// Builds an owned, lifetime-free copy of this parse tree.
    #[must_use]
    pub fn to_owned_tree(&self) -> OwnedRuleNode {
        OwnedRuleNode::from_rule_node(self)
    }
}

/// Preorder iterator returned by [`OwnedRuleNode::walk`].
pub struct OwnedRuleNodeWalk<'a> {
    stack: Vec<&'a OwnedRuleNode>,
}

impl<'a> Iterator for OwnedRuleNodeWalk<'a> {
    type Item = &'a OwnedRuleNode;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        self.stack.extend(node.children.iter().rev());
        Some(node)
    }
}

/// Preorder iterator returned by [`RuleNode::walk`].
///
/// The iterator yields the root node first, then each child subtree in source
/// order.
pub struct RuleNodeWalk<'a, 'g, 's> {
    stack: Vec<&'a RuleNode<'g, 's>>,
}

impl<'a, 'g, 's> Iterator for RuleNodeWalk<'a, 'g, 's> {
    type Item = &'a RuleNode<'g, 's>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        self.stack.extend(node.children.iter().rev());
        Some(node)
    }
}

/// Interpreted parse result returned by [`interpret_region`].
///
/// The root is the node for the requested root rule. If complete-region parsing
/// fails, callers receive [`ParseError`] instead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parse<'g, 's> {
    /// Root parse node.
    pub root: RuleNode<'g, 's>,
}

/// Partial parse report returned by [`inspect_region`].
///
/// This is for inspection and tooling. It is not recovery and it does not make
/// an incomplete parse successful: [`ParseReport::error`] is populated whenever
/// complete-region parsing failed. When the root rule matched before trailing
/// input failed, [`ParseReport::root`] preserves that matched tree so callers
/// can still inspect it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseReport<'g, 's> {
    /// Matched root node, if the root rule itself matched.
    pub root: Option<RuleNode<'g, 's>>,
    /// Complete-region parse failure, if parsing did not consume the region.
    pub error: Option<ParseError<'g>>,
    /// Parser work counters collected while producing this report.
    pub metrics: ParseMetrics,
}

impl<'g, 's> ParseReport<'g, 's> {
    /// Returns true when the report represents a complete successful parse.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.root.is_some() && self.error.is_none()
    }

    /// Converts this report into the success/failure result shape used
    /// by [`interpret_region`].
    pub fn into_result(self) -> Result<Parse<'g, 's>, ParseError<'g>> {
        match (self.root, self.error) {
            (Some(root), None) => Ok(Parse { root }),
            (_, Some(error)) => Err(error),
            (None, None) => Err(ParseError {
                span: Span::default(),
                expected: Vec::new(),
            }),
        }
    }
}

/// Result of a recovering parse over a top-level repetition.
///
/// Returned by [`interpret_region_recovering`]. The root node always
/// covers the requested region; unparsable spans are represented as
/// [`RuleNodeKind::RecoveryError`] children inline in the root's
/// `children`. The `errors` vector lists every parse failure the
/// recovery had to skip past, in source order — editor
/// consumers can use both: the tree drives navigation, the errors
/// drive diagnostics.
///
/// When the root rule is not a recoverable shape (not a top-level
/// `Rule(...)*` / `Rule(...)+` repetition) the matcher falls back to
/// [`inspect_region_compiled`] and surfaces the result through this
/// shape; in that case [`Self::root`] may be `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport<'g, 's> {
    /// Recovered root node spanning the whole requested region, or
    /// `None` when the matcher could not begin the root rule at all
    /// (only possible on the non-repetition fallback path).
    pub root: Option<RuleNode<'g, 's>>,
    /// Parse failures the recovery skipped past, in source order.
    pub errors: Vec<ParseError<'g>>,
    /// Parser work counters collected while producing this report.
    pub metrics: ParseMetrics,
}

impl<'g, 's> RecoveryReport<'g, 's> {
    fn from_inspect(report: ParseReport<'g, 's>) -> Self {
        Self {
            root: report.root,
            errors: report.error.into_iter().collect(),
            metrics: report.metrics,
        }
    }
}

/// Parser work counters for one interpreted parse.
///
/// Reports return this value so callers can inspect whether packrat
/// memoization is doing useful work without changing parse semantics.
/// The counters are per parse only; the kernel does not keep a
/// persistent cache across calls. Memo hits, misses, and entries cover
/// both named-rule dispatch and parametric-rule application.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ParseMetrics {
    /// Number of concrete named-rule dispatches requested.
    pub rule_attempts: usize,
    /// Number of dispatches or parametric applications satisfied from
    /// the per-parse memo table.
    pub memo_hits: usize,
    /// Number of dispatches or parametric applications that missed the
    /// memo table and evaluated the rule body.
    pub memo_misses: usize,
    /// Number of entries stored in all memo tables at the end of the parse.
    pub memo_entries: usize,
}

/// One expected fragment in an interpreted parse failure.
///
/// [`ParseError::expected`] stores these. The fragments borrow rule names and
/// literal tokens from the grammar; callers can render them with
/// [`fmt::Display`] when building diagnostics or reports.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add fragments
/// (for example, negative lookahead) without forcing a breaking change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ExpectedFragment<'g> {
    /// A named grammar rule.
    Rule(&'g str),
    /// A literal token.
    Literal(&'g str),
    /// A character-class expression.
    CharacterClass(&'g str),
    /// End of the parsed region.
    EndOfRegion,
    /// An ordered-choice expression.
    Choice,
    /// A negative-lookahead expression matched, which means the expression
    /// could not consume here.
    NegativeLookahead,
}

impl fmt::Display for ExpectedFragment<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rule(name) => formatter.write_str(name),
            Self::Literal(value) => write!(formatter, "`{value}`"),
            Self::CharacterClass(value) => write!(formatter, "[{value}]"),
            Self::EndOfRegion => formatter.write_str("end of region"),
            Self::Choice => formatter.write_str("choice"),
            Self::NegativeLookahead => formatter.write_str("not <expression>"),
        }
    }
}

/// Interpreted parse failure returned by [`interpret_region`] and
/// [`interpret_rule_error`].
///
/// The failure is structural: [`ParseError::span`] reports the unexpected
/// source fragment when one exists, or a zero-width expectation point at EOF
/// or between fragments. [`ParseError::expected`] lists the fragments that
/// could have matched there. Rendering, localization, labels, and recovery
/// policy belong to the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError<'g> {
    /// Error span in the caller's coordinate system.
    ///
    /// [`interpret_region`] and [`interpret_rule_error`] add their
    /// `source_offset` argument to the internal failure span before returning
    /// this span.
    pub span: Span,
    /// Expected fragments at the error span.
    ///
    /// These are parser fragments such as rule names, literal tokens, character
    /// classes, or `end of region`; callers decide how to render them.
    pub expected: Vec<ExpectedFragment<'g>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpressionMatch<'g, 's> {
    end: usize,
    children: Vec<RuleNode<'g, 's>>,
    /// Set when a [`RuleExpression::Cut`] inside this match has committed.
    /// Propagates up through Sequence / Optional / repetition; cleared by
    /// [`RuleExpression::Choice`] on success.
    committed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParseFailure<'g> {
    span: Span,
    expected: Vec<ExpectedFragment<'g>>,
    /// Set when a [`RuleExpression::Cut`] earlier in the failing sequence has
    /// committed. [`RuleExpression::Choice`] returns the failure as the
    /// choice's failure (no fallthrough) and clears the flag so an outer
    /// choice can still try its alternatives.
    committed: bool,
}

struct ParseContext<'g, 's> {
    source: &'s str,
    source_offset: usize,
    memo: Vec<MemoEntry<'g, 's>>,
    apply_memo: Vec<ApplyMemoEntry<'g, 's>>,
    active_rules: Vec<MemoKey<'g>>,
    metrics: ParseMetrics,
}

impl<'g, 's> ParseContext<'g, 's> {
    fn new(source: &'s str, source_offset: usize) -> Self {
        Self {
            source,
            source_offset,
            memo: Vec::new(),
            apply_memo: Vec::new(),
            active_rules: Vec::new(),
            metrics: ParseMetrics::default(),
        }
    }

    fn finish_metrics(&self) -> ParseMetrics {
        ParseMetrics {
            memo_entries: self.memo.len().saturating_add(self.apply_memo.len()),
            ..self.metrics
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MemoKey<'g> {
    scope: usize,
    name: &'g str,
    position: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MemoEntry<'g, 's> {
    key: MemoKey<'g>,
    result: Result<RuleNode<'g, 's>, ParseFailure<'g>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ApplyMemoKey<'g> {
    scope: usize,
    caller_scope: usize,
    name: &'g str,
    position: usize,
    args: Vec<RuleExpression<'g>>,
    params: Vec<ParamBindingMemoKey<'g>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ApplyMemoEntry<'g, 's> {
    key: ApplyMemoKey<'g>,
    result: Result<ExpressionMatch<'g, 's>, ParseFailure<'g>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParamBindingMemoKey<'g> {
    name: &'g str,
    arg: RuleExpression<'g>,
    caller_scope: usize,
    caller_bindings: Vec<ParamBindingMemoKey<'g>>,
}

/// Stable code for a [`GrammarSourceDiagnostic`].
///
/// Callers can pattern-match on the code to localize messages, classify
/// failures, or filter expected diagnostics in tests. The associated
/// [`GrammarSourceDiagnosticCode::message`] returns a static default rendering.
///
/// Marked `#[non_exhaustive]` so future kernel commits can add codes without
/// forcing a breaking change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GrammarSourceDiagnosticCode {
    /// A non-empty line did not begin with `rule `, `grammar`, `end`, or `//`.
    MissingRuleKeyword,
    /// A `rule` line had no `=` separating the name from the body.
    MissingEquals,
    /// A `rule` line had an empty name (for example `rule  = body`).
    EmptyName,
    /// A `rule` line had an empty body (for example `rule name =`).
    EmptyBody,
    /// A rule name appeared more than once in the same grammar source.
    DuplicateName,
    /// A `rule` line carried an unrecognized modifier token between the
    /// name and the `=`. Recognized modifier tokens are `raw`, `@name`, and
    /// `@name=value`.
    UnknownRuleModifier,
    /// A parametric rule invocation (`rule-name<arg, ...>`) supplies a
    /// different number of arguments than the target rule's declared
    /// formal parameters.
    ///
    /// Only fires when the target rule is defined in the same grammar
    /// source; references to externally-supplied (e.g. parent-grammar)
    /// rules are not validated here.
    ParametricRuleArityMismatch,
}

impl GrammarSourceDiagnosticCode {
    /// Returns the stable identifier used in dumps and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingRuleKeyword => "missing-rule-keyword",
            Self::MissingEquals => "missing-equals",
            Self::EmptyName => "empty-name",
            Self::EmptyBody => "empty-body",
            Self::DuplicateName => "duplicate-name",
            Self::UnknownRuleModifier => "unknown-rule-modifier",
            Self::ParametricRuleArityMismatch => "parametric-rule-arity-mismatch",
        }
    }

    /// Returns a static human-readable description for this code.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::MissingRuleKeyword => "line must begin with `rule`, `grammar`, `end`, or `//`",
            Self::MissingEquals => "rule is missing `=` between name and body",
            Self::EmptyName => "rule name must not be empty",
            Self::EmptyBody => "rule body must not be empty",
            Self::DuplicateName => "rule name was already declared in this grammar",
            Self::UnknownRuleModifier => {
                "unrecognized rule modifier; expected `raw`, `@name`, `@name=value`, or nothing"
            }
            Self::ParametricRuleArityMismatch => {
                "parametric rule invocation has wrong number of arguments"
            }
        }
    }
}

impl fmt::Display for GrammarSourceDiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structural diagnostic produced by [`parse_grammar_source_with_diagnostics`].
///
/// Each diagnostic carries the span of the offending grammar-source line and a
/// stable [`GrammarSourceDiagnosticCode`]. Use the accessors rather than the
/// fields so the in-memory layout can evolve later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrammarSourceDiagnostic {
    span: Span,
    code: GrammarSourceDiagnosticCode,
}

impl GrammarSourceDiagnostic {
    const fn new(span: Span, code: GrammarSourceDiagnosticCode) -> Self {
        Self { span, code }
    }

    /// Returns the grammar-source span this diagnostic refers to.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> GrammarSourceDiagnosticCode {
        self.code
    }

    /// Returns a static human-readable description.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        self.code.message()
    }
}

impl fmt::Display for GrammarSourceDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}..{}: {}",
            self.code.as_str(),
            self.span.start,
            self.span.end,
            self.code.message(),
        )
    }
}

/// Parses the small line-oriented grammar source format.
///
/// Returns one [`SourceAuthoredRule`] per `rule ... = ...` line. Pass the
/// returned slice to [`interpret_region`] to match source text, or inspect the
/// rules to build documentation, projections, or caller-owned action tables.
///
/// This entry point silently discards malformed lines. Use
/// [`parse_grammar_source_with_diagnostics`] when grammar-source diagnostics
/// are needed; [`parse_grammar_source_full`] also surfaces the
/// `grammar X extends Y` header for inheritance-aware callers.
#[must_use]
pub fn parse_grammar_source(source: &str) -> Vec<SourceAuthoredRule<'_>> {
    parse_grammar_source_with_diagnostics(source).0
}

/// Header information captured from the leading `grammar ...` line.
///
/// Empty when the source does not declare a `grammar` header. The
/// `extends` field is populated when the header carries the
/// `extends OTHER-NAME` clause: `grammar X extends Y`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GrammarSourceHeader<'g> {
    /// Declared grammar name (the `X` in `grammar X`). `None` when the
    /// source has no `grammar` header.
    pub name: Option<&'g str>,
    /// Optional parent grammar name (the `Y` in `grammar X extends Y`).
    pub extends: Option<&'g str>,
}

/// Bundle returned by [`parse_grammar_source_full`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedGrammarSource<'g> {
    /// Captured `grammar` header.
    pub header: GrammarSourceHeader<'g>,
    /// Rules parsed from `rule ... = ...` lines.
    pub rules: Vec<SourceAuthoredRule<'g>>,
    /// Structural diagnostics from grammar-source parsing.
    pub diagnostics: Vec<GrammarSourceDiagnostic>,
}

/// Parses grammar source and returns rules, diagnostics, and the
/// optional `grammar X extends Y` header.
///
/// Equivalent to [`parse_grammar_source_with_diagnostics`] but also
/// surfaces the header so inheritance-aware callers can resolve
/// parent grammars.
#[must_use]
pub fn parse_grammar_source_full(source: &str) -> ParsedGrammarSource<'_> {
    let (rules, diagnostics, header) = parse_grammar_source_inner(source);
    ParsedGrammarSource {
        header,
        rules,
        diagnostics,
    }
}

/// Parses grammar source and returns rules alongside structural diagnostics.
///
/// Diagnostics are stable, code-tagged structural reports for malformed lines:
/// missing `rule` keyword, missing `=`, empty name, empty body, or duplicate
/// rule names. The first occurrence of a duplicate name wins (matching the
/// existing first-wins lookup semantics); subsequent duplicates are reported
/// and skipped. Empty lines, comments, the `grammar` header, and the trailing
/// `end` are not reported.
#[must_use]
pub fn parse_grammar_source_with_diagnostics(
    source: &str,
) -> (Vec<SourceAuthoredRule<'_>>, Vec<GrammarSourceDiagnostic>) {
    let (rules, diagnostics, _header) = parse_grammar_source_inner(source);
    (rules, diagnostics)
}

fn parse_grammar_source_inner(
    source: &str,
) -> (
    Vec<SourceAuthoredRule<'_>>,
    Vec<GrammarSourceDiagnostic>,
    GrammarSourceHeader<'_>,
) {
    let mut rules = Vec::<SourceAuthoredRule<'_>>::new();
    let mut diagnostics = Vec::new();
    let mut header = GrammarSourceHeader::default();
    let mut offset = 0_usize;
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed == "end" {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("grammar ") {
            // Header lines: `grammar X` or `grammar X extends Y`.
            // First header wins; subsequent ones are ignored silently
            // (consistent with how malformed lines beyond the first are
            // currently handled).
            if header.name.is_none() {
                let (name, extends) = if let Some((name, parent)) = rest.split_once(" extends ") {
                    (name.trim(), Some(parent.trim()))
                } else {
                    (rest.trim(), None)
                };
                if !name.is_empty() {
                    header.name = Some(name);
                }
                if let Some(parent) = extends
                    && !parent.is_empty()
                {
                    header.extends = Some(parent);
                }
            }
            continue;
        }
        let leading = line.find(trimmed).unwrap_or(0);
        let trimmed_start = line_start + leading;
        let trimmed_span = Span::new(trimmed_start, trimmed_start + trimmed.len());
        let Some(rest) = trimmed.strip_prefix("rule ") else {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::MissingRuleKeyword,
            ));
            continue;
        };
        let Some((name, expression)) = split_rule_definition(rest) else {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::MissingEquals,
            ));
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::EmptyName,
            ));
            continue;
        }
        let Some((base_name, params, modifier_remainder)) = extract_rule_name_header(name) else {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::EmptyName,
            ));
            continue;
        };
        let Some((trivia, annotations)) = parse_rule_modifiers(modifier_remainder) else {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::UnknownRuleModifier,
            ));
            continue;
        };
        let name = base_name;
        if expression.trim().is_empty() {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::EmptyBody,
            ));
            continue;
        }
        if rules.iter().any(|rule| rule.name == name) {
            diagnostics.push(GrammarSourceDiagnostic::new(
                trimmed_span,
                GrammarSourceDiagnosticCode::DuplicateName,
            ));
            continue;
        }
        let parsed = parse_rule_expression(expression);
        let action = match &parsed {
            RuleExpression::Transform {
                output: TransformOutput::Action(action),
                ..
            } => Some(action.clone()),
            _ => None,
        };
        rules.push(SourceAuthoredRule {
            name,
            expression: parsed,
            action,
            excluded_text: Vec::new(),
            annotations,
            source_span: trimmed_span,
            trivia,
            params,
        });
    }
    validate_parametric_arity(&rules, &mut diagnostics);
    (rules, diagnostics, header)
}

fn split_rule_definition(input: &str) -> Option<(&str, &str)> {
    if let Some(index) = find_spaced_rule_separator(input) {
        return Some((&input[..index], &input[index + '='.len_utf8()..]));
    }
    let (header, body) = input.split_once('=')?;
    (!header.contains('@')).then_some((header, body))
}

fn find_spaced_rule_separator(input: &str) -> Option<usize> {
    let mut in_literal = false;
    for (index, ch) in input.char_indices() {
        match ch {
            '`' => in_literal = !in_literal,
            '=' if !in_literal
                && input[..index]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
                && input[index + ch.len_utf8()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace) =>
            {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

/// Walks each rule's body for [`RuleExpression::Apply`] invocations and
/// reports arity mismatches against locally-declared parametric rules.
///
/// Apply nodes that reference a rule not present in `rules` are skipped
/// — they may be supplied by a parent grammar via inheritance, and
/// cross-grammar arity checks need access to the merged rule set.
fn validate_parametric_arity<'g>(
    rules: &[SourceAuthoredRule<'g>],
    diagnostics: &mut Vec<GrammarSourceDiagnostic>,
) {
    for rule in rules {
        walk_apply_nodes(&rule.expression, &mut |target, args| {
            if let Some(declared) = rules.iter().find(|candidate| candidate.name == target)
                && declared.params.len() != args.len()
            {
                diagnostics.push(GrammarSourceDiagnostic::new(
                    rule.source_span,
                    GrammarSourceDiagnosticCode::ParametricRuleArityMismatch,
                ));
            }
        });
    }
}

fn walk_apply_nodes<'g, F>(expression: &RuleExpression<'g>, visit: &mut F)
where
    F: FnMut(&'g str, &[RuleExpression<'g>]),
{
    match expression {
        RuleExpression::Apply { rule, args } => {
            visit(rule, args);
            for arg in args {
                walk_apply_nodes(arg, visit);
            }
        }
        RuleExpression::Sequence(items) | RuleExpression::Choice(items) => {
            for item in items {
                walk_apply_nodes(item, visit);
            }
        }
        RuleExpression::Optional(item)
        | RuleExpression::OneOrMore(item)
        | RuleExpression::ZeroOrMore(item)
        | RuleExpression::And(item)
        | RuleExpression::Not(item)
        | RuleExpression::Cut(item) => walk_apply_nodes(item, visit),
        RuleExpression::Transform { input, .. } => walk_apply_nodes(input, visit),
        RuleExpression::Named { inner, .. } => walk_apply_nodes(inner, visit),
        RuleExpression::Rule(_)
        | RuleExpression::Super(_)
        | RuleExpression::Literal(_)
        | RuleExpression::CharacterClass(_)
        | RuleExpression::UntilLiteral(_) => {}
    }
}

fn parse_rule_modifiers(input: &str) -> Option<(TriviaMode, Vec<RuleAnnotation<'_>>)> {
    let mut trivia = TriviaMode::Default;
    let mut annotations = Vec::new();
    for modifier in split_whitespace_top_level(input) {
        if modifier == "raw" {
            trivia = TriviaMode::Raw;
        } else {
            let annotation = parse_rule_annotation(modifier)?;
            annotations.push(annotation);
        }
    }
    Some((trivia, annotations))
}

fn parse_rule_annotation(input: &str) -> Option<RuleAnnotation<'_>> {
    let body = input.strip_prefix('@')?;
    let (name, value) = match body.split_once('=') {
        Some((name, value)) => (name.trim(), Some(value.trim())),
        None => (body.trim(), None),
    };
    if !valid_annotation_name(name) || value.is_some_and(str::is_empty) {
        return None;
    }
    Some(RuleAnnotation::new(name, value))
}

fn valid_annotation_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(is_named_binding_char)
}

/// Parses one grammar rule expression.
///
/// This is useful for tests, tools, or host-projected grammars that already
/// have the rule name and only need the right-hand side. Use
/// [`parse_grammar_source`] when parsing complete grammar files.
#[must_use]
pub fn parse_rule_expression(expression: &str) -> RuleExpression<'_> {
    let trimmed = expression.trim();
    if let Some((input, output)) = split_transform_top_level(trimmed) {
        let parsed_input = parse_rule_expression(input);
        // Collect `name:expr` binding names from the matched portion so the
        // action parser can resolve bare identifiers to CaptureByName when a
        // binding exists.
        let mut bindings = Vec::new();
        collect_binding_names(&parsed_input, &mut bindings);
        return RuleExpression::transform(
            parsed_input,
            parse_transform_output(output.trim(), &bindings),
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
    let mut angle_depth = 0_i32;
    let mut in_literal = false;
    let mut chars = input.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '`' => in_literal = !in_literal,
            '(' if !in_literal => depth += 1,
            ')' if !in_literal => depth -= 1,
            '<' if !in_literal => angle_depth += 1,
            '>' if !in_literal && angle_depth > 0 => angle_depth -= 1,
            '=' if !in_literal
                && depth == 0
                && angle_depth == 0
                && matches!(chars.peek(), Some((_, '>'))) =>
            {
                let output_start = index + "=>".len();
                return Some((input[..index].trim(), input[output_start..].trim()));
            }
            _ => {}
        }
    }
    None
}

fn parse_transform_output<'g>(output: &'g str, bindings: &[&'g str]) -> TransformOutput<'g> {
    if let Some(action) = parse_grammar_action_with_bindings(output, bindings) {
        return TransformOutput::Action(action);
    }
    output
        .strip_prefix('`')
        .and_then(|inner| inner.strip_suffix('`'))
        .map_or_else(|| TransformOutput::Symbol(output), TransformOutput::Literal)
}

/// Parses an action descriptor.
///
/// Returns `None` when the input is not shaped like `name(arg: value, ...)`.
/// [`parse_grammar_source`] calls this for transform output after `=>`.
#[must_use]
pub fn parse_grammar_action(output: &str) -> Option<GrammarAction<'_>> {
    parse_grammar_action_with_bindings(output, &[])
}

/// Same as [`parse_grammar_action`] but resolves bare identifiers to
/// [`GrammarActionTerm::CaptureByName`] when they match one of the
/// provided binding names (typically the binding set of the rule the
/// action is attached to).
#[must_use]
pub fn parse_grammar_action_with_bindings<'g>(
    output: &'g str,
    bindings: &[&'g str],
) -> Option<GrammarAction<'g>> {
    let trimmed = output.trim();
    let (name, args) = trimmed.split_once('(')?;
    let args = args.strip_suffix(')')?;
    Some(GrammarAction::new(
        name.trim(),
        split_action_args(args)
            .into_iter()
            .filter_map(|arg| parse_grammar_action_arg(arg, bindings))
            .collect(),
    ))
}

fn parse_grammar_action_arg<'g>(
    arg: &'g str,
    bindings: &[&'g str],
) -> Option<GrammarActionArg<'g>> {
    let (name, value) = arg.split_once(':')?;
    Some(GrammarActionArg::new(
        name.trim(),
        parse_grammar_action_term(value.trim(), bindings),
    ))
}

fn parse_grammar_action_term<'g>(value: &'g str, bindings: &[&'g str]) -> GrammarActionTerm<'g> {
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
        return GrammarActionTerm::Literal(inner);
    }
    if let Ok(integer) = trimmed.parse::<i64>() {
        return GrammarActionTerm::Integer(integer);
    }
    if let Some(inner) = trimmed
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        return GrammarActionTerm::List(
            split_action_args(inner)
                .into_iter()
                .map(|item| parse_grammar_action_term(item, bindings))
                .collect(),
        );
    }
    if let Some(inner) = trimmed
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
    {
        return GrammarActionTerm::Record(
            split_action_args(inner)
                .into_iter()
                .filter_map(|arg| parse_grammar_action_arg(arg, bindings))
                .collect(),
        );
    }
    if trimmed.ends_with(')')
        && trimmed.contains('(')
        && let Some(action) = parse_grammar_action_with_bindings(trimmed, bindings)
    {
        return GrammarActionTerm::Call(Box::new(action));
    }
    if let Some(rule) = trimmed.strip_suffix('*') {
        return GrammarActionTerm::CaptureAll { rule: rule.trim() };
    }
    if bindings.contains(&trimmed) {
        return GrammarActionTerm::CaptureByName { name: trimmed };
    }
    GrammarActionTerm::Capture { rule: trimmed }
}

fn split_action_args(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_string = false;
    let mut in_literal = false;
    let mut paren_depth = 0_i32;
    let mut bracket_depth = 0_i32;
    let mut brace_depth = 0_i32;
    for (index, ch) in input.char_indices() {
        match ch {
            '"' if !in_literal => in_string = !in_string,
            '`' if !in_string => in_literal = !in_literal,
            '(' if !in_string && !in_literal => paren_depth += 1,
            ')' if !in_string && !in_literal => paren_depth -= 1,
            '[' if !in_string && !in_literal => bracket_depth += 1,
            ']' if !in_string && !in_literal => bracket_depth -= 1,
            '{' if !in_string && !in_literal => brace_depth += 1,
            '}' if !in_string && !in_literal => brace_depth -= 1,
            ',' if !in_string
                && !in_literal
                && paren_depth == 0
                && bracket_depth == 0
                && brace_depth == 0 =>
            {
                parts.push(input[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(input[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

/// Recognizes a `name:rest` binding prefix on a rule term.
///
/// Returns `Some((name, rest))` when `input` begins with an identifier
/// shaped like a rule name immediately followed by a colon that is
/// neither doubled (`::` is a literal token elsewhere) nor inside a
/// backtick literal. The function tolerates trailing characters in
/// `rest` and lets the caller decide how to parse them.
fn parse_named_prefix(input: &str) -> Option<(&str, &str)> {
    let mut chars = input.char_indices();
    let mut name_end = None;
    for (index, ch) in chars.by_ref() {
        if is_named_binding_char(ch) {
            name_end = Some(index + ch.len_utf8());
            continue;
        }
        if ch == ':' {
            let name_end = name_end?;
            // Reject `::` — that's a literal token (e.g. base_loko's `::`).
            let after_colon = index + ch.len_utf8();
            if input[after_colon..].starts_with(':') {
                return None;
            }
            let name = &input[..name_end];
            let rest = input[after_colon..].trim_start();
            if rest.is_empty() {
                return None;
            }
            return Some((name, rest));
        }
        return None;
    }
    None
}

fn is_named_binding_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '?' | '$')
}

/// Collects every `name:expr` binding name in this rule expression.
///
/// Used by the action parser so that bare identifiers in action
/// arguments resolve to [`GrammarActionTerm::CaptureByName`] when a
/// binding of that name exists on the matched rule, falling back to
/// [`GrammarActionTerm::Capture`] when no binding is in scope.
fn collect_binding_names<'g>(expression: &RuleExpression<'g>, out: &mut Vec<&'g str>) {
    match expression {
        RuleExpression::Named { name, inner } => {
            if !out.contains(name) {
                out.push(name);
            }
            collect_binding_names(inner, out);
        }
        RuleExpression::Sequence(items) | RuleExpression::Choice(items) => {
            for item in items {
                collect_binding_names(item, out);
            }
        }
        RuleExpression::Optional(item)
        | RuleExpression::OneOrMore(item)
        | RuleExpression::ZeroOrMore(item)
        | RuleExpression::And(item)
        | RuleExpression::Not(item)
        | RuleExpression::Cut(item) => collect_binding_names(item, out),
        RuleExpression::Transform { input, .. } => collect_binding_names(input, out),
        RuleExpression::Apply { args, .. } => {
            for arg in args {
                collect_binding_names(arg, out);
            }
        }
        RuleExpression::Rule(_)
        | RuleExpression::Super(_)
        | RuleExpression::Literal(_)
        | RuleExpression::CharacterClass(_)
        | RuleExpression::UntilLiteral(_) => {}
    }
}

fn parse_rule_term(term: &str) -> RuleExpression<'_> {
    let trimmed = term.trim();
    if let Some(inner) = trimmed.strip_prefix('&') {
        return RuleExpression::and(parse_rule_term(inner));
    }
    if let Some(inner) = trimmed.strip_prefix('!') {
        return RuleExpression::not(parse_rule_term(inner));
    }
    if let Some(inner) = trimmed.strip_prefix('^') {
        return RuleExpression::cut(parse_rule_term(inner));
    }
    if let Some((name, rest)) = parse_named_prefix(trimmed) {
        return RuleExpression::named(name, parse_rule_term(rest));
    }
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
    if let Some((rule, args)) = parse_apply_term(trimmed) {
        return RuleExpression::apply(rule, args);
    }
    if let Some(name) = trimmed.strip_prefix("super.") {
        let name = name.trim();
        if !name.is_empty() && name.chars().all(is_named_binding_char) {
            return RuleExpression::Super(name);
        }
    }
    RuleExpression::rule(trimmed)
}

/// Extracts the base name, any parametric definition parameters, and the
/// trailing modifier token from a rule-definition header.
///
/// Returns `(base, params, remainder)` where `remainder` is the trimmed
/// text after `base<params>`. For non-parametric rules `params` is empty.
fn extract_rule_name_header(name: &str) -> Option<(&str, Vec<&str>, &str)> {
    let trimmed = name.trim();
    let mut base_end = trimmed.len();
    for (idx, ch) in trimmed.char_indices() {
        if !is_named_binding_char(ch) {
            base_end = idx;
            break;
        }
    }
    let base = &trimmed[..base_end];
    if base.is_empty() {
        return None;
    }
    let rest = trimmed[base_end..].trim_start();
    if let Some(inside) = rest.strip_prefix('<') {
        // Find the matching `>` at angle-depth zero (outside backticks).
        let mut depth = 1_i32;
        let mut in_literal = false;
        let mut end_index = None;
        for (idx, ch) in inside.char_indices() {
            match ch {
                '`' => in_literal = !in_literal,
                '<' if !in_literal => depth += 1,
                '>' if !in_literal => {
                    depth -= 1;
                    if depth == 0 {
                        end_index = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end_index = end_index?;
        let params_body = &inside[..end_index];
        let after = inside[end_index + 1..].trim_start();
        let params: Vec<&str> = params_body
            .split(',')
            .map(str::trim)
            .filter(|param| !param.is_empty())
            .collect();
        Some((base, params, after))
    } else {
        Some((base, Vec::new(), rest))
    }
}

/// Recognizes a parametric rule invocation of the shape
/// `rule-name<arg, arg, ...>`.
///
/// The angle bracket must be glued to the rule name (no whitespace) and
/// the closing `>` must be the final byte of the trimmed term. Returns
/// `None` when the shape doesn't match so the caller can fall back to a
/// plain rule reference.
fn parse_apply_term(input: &str) -> Option<(&str, Vec<RuleExpression<'_>>)> {
    let trimmed = input.trim();
    if !trimmed.ends_with('>') {
        return None;
    }
    let mut name_end = None;
    for (idx, ch) in trimmed.char_indices() {
        if is_named_binding_char(ch) {
            continue;
        }
        if ch == '<' && idx > 0 {
            name_end = Some(idx);
        }
        break;
    }
    let name_end = name_end?;
    let name = &trimmed[..name_end];
    // Strip the closing `>` (the final byte) and the opening `<`.
    let body = &trimmed[name_end + 1..trimmed.len() - 1];
    let args = split_top_level(body, ',')
        .into_iter()
        .map(parse_rule_expression)
        .collect();
    Some((name, args))
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0_i32;
    let mut angle_depth = 0_i32;
    let mut in_literal = false;
    for (index, ch) in input.char_indices() {
        match ch {
            '`' => in_literal = !in_literal,
            '(' if !in_literal => depth += 1,
            ')' if !in_literal => depth -= 1,
            '<' if !in_literal => angle_depth += 1,
            '>' if !in_literal && angle_depth > 0 => angle_depth -= 1,
            _ if ch == delimiter && !in_literal && depth == 0 && angle_depth == 0 => {
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
    let mut angle_depth = 0_i32;
    let mut in_literal = false;
    let mut in_class = false;
    for (index, ch) in input.char_indices() {
        if start.is_none()
            && (!ch.is_whitespace() || in_literal || in_class || depth > 0 || angle_depth > 0)
        {
            start = Some(index);
        }
        match ch {
            '`' if !in_class => in_literal = !in_literal,
            '[' if !in_literal => in_class = true,
            ']' if !in_literal => in_class = false,
            '(' if !in_literal && !in_class => depth += 1,
            ')' if !in_literal && !in_class => depth -= 1,
            '<' if !in_literal && !in_class => angle_depth += 1,
            '>' if !in_literal && !in_class && angle_depth > 0 => angle_depth -= 1,
            _ if ch.is_whitespace()
                && !in_literal
                && !in_class
                && depth == 0
                && angle_depth == 0 =>
            {
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
///
/// `rules` normally comes from [`parse_grammar_source`]. On success, returns a
/// [`Parse`] tree rooted at `root_rule`. On failure, returns a [`ParseError`].
/// `source_offset` is added to every reported [`Span`], which lets callers
/// parse an embedded region while preserving outer source coordinates.
///
/// Use [`interpret_region_compiled`] with a pre-built [`CompiledGrammar`] for
/// repeated calls against the same rules; this slice-taking entry point falls
/// back to a linear name lookup.
pub fn interpret_region<'g, 's>(
    rules: &[SourceAuthoredRule<'g>],
    root_rule: &'g str,
    source: &'s str,
    source_offset: usize,
) -> Result<Parse<'g, 's>, ParseError<'g>> {
    inspect_region(rules, root_rule, source, source_offset).into_result()
}

/// Same as [`interpret_region`] but accepts a pre-built [`CompiledGrammar`].
///
/// Lookups use the compiled name index (binary search) instead of the linear
/// scan used by the slice-taking entry point. Behavior is otherwise
/// identical.
pub fn interpret_region_compiled<'g, 's>(
    grammar: &CompiledGrammar<'g>,
    root_rule: &'g str,
    source: &'s str,
    source_offset: usize,
) -> Result<Parse<'g, 's>, ParseError<'g>> {
    inspect_region_compiled(grammar, root_rule, source, source_offset).into_result()
}

/// Interprets `root_rule` and returns the matched root plus any complete-region
/// failure.
///
/// This is the inspection-oriented sibling of [`interpret_region`]. Use it when
/// a caller wants to display a partial tree after the root rule matched but
/// trailing input prevented a complete parse.
#[must_use]
pub fn inspect_region<'g, 's>(
    rules: &[SourceAuthoredRule<'g>],
    root_rule: &'g str,
    source: &'s str,
    source_offset: usize,
) -> ParseReport<'g, 's> {
    inspect_region_with(
        RuleLookup::from_slice(rules),
        root_rule,
        source,
        source_offset,
    )
}

/// Same as [`inspect_region`] but accepts a pre-built [`CompiledGrammar`].
#[must_use]
pub fn inspect_region_compiled<'g, 's>(
    grammar: &CompiledGrammar<'g>,
    root_rule: &'g str,
    source: &'s str,
    source_offset: usize,
) -> ParseReport<'g, 's> {
    inspect_region_with(grammar.lookup(), root_rule, source, source_offset)
}

/// Recovering parse over a top-level repetition.
///
/// Editor-style entry point: when the root rule's body is
/// `Rule(inner)*` or `Rule(inner)+`, the matcher repeatedly tries
/// `inner` and skips forward past per-iteration failures, emitting a
/// [`RuleNodeKind::RecoveryError`] child for each skipped region.
/// Returns a [`RecoveryReport`] with a root that always spans the
/// whole input plus a list of every recovered failure.
///
/// For non-repetition root rules this falls back to
/// [`inspect_region_compiled`] and wraps the result in a
/// [`RecoveryReport`] — the report's root may be missing if the root
/// rule itself failed to match at all.
#[must_use]
pub fn interpret_region_recovering<'g, 's>(
    grammar: &CompiledGrammar<'g>,
    root_rule: &'g str,
    source: &'s str,
    source_offset: usize,
) -> RecoveryReport<'g, 's> {
    let Some(rule) = grammar.rule(root_rule) else {
        let report = inspect_region_compiled(grammar, root_rule, source, source_offset);
        return RecoveryReport::from_inspect(report);
    };
    let iteration_target = match &rule.expression {
        RuleExpression::ZeroOrMore(inner) | RuleExpression::OneOrMore(inner) => {
            match inner.as_ref() {
                RuleExpression::Rule(name) => Some(*name),
                _ => None,
            }
        }
        _ => None,
    };
    let Some(iteration_target) = iteration_target else {
        let report = inspect_region_compiled(grammar, root_rule, source, source_offset);
        return RecoveryReport::from_inspect(report);
    };

    let lookup = grammar.lookup();
    let raw = matches!(rule.trivia, TriviaMode::Raw);
    let mut context = ParseContext::new(source, source_offset);
    let mut children: Vec<RuleNode<'g, 's>> = Vec::new();
    let mut errors: Vec<ParseError<'g>> = Vec::new();
    let mut position = 0_usize;

    while position < source.len() {
        let try_position = if raw {
            position
        } else {
            skip_trivia(source, position)
        };
        if try_position >= source.len() {
            break;
        }
        match interpret_rule(&mut context, lookup, iteration_target, try_position) {
            Ok(node) => {
                let advance = node.span.end.saturating_sub(source_offset);
                if advance <= position {
                    // Zero-width match — avoid an infinite loop by
                    // treating it as a failure and skipping forward.
                    errors.push(
                        ParseFailure::expected_at_source(
                            source,
                            try_position,
                            ExpectedFragment::Rule(iteration_target),
                            raw,
                        )
                        .into_error(source_offset),
                    );
                    position = next_char_boundary(source, try_position);
                    continue;
                }
                children.push(node);
                position = advance;
            }
            Err(failure) => {
                errors.push(failure.into_error(source_offset));
                let recovery_start = try_position;
                let mut scan = next_char_boundary(source, try_position);
                let mut recovered: Option<RuleNode<'g, 's>> = None;
                while scan < source.len() {
                    let scan_position = if raw { scan } else { skip_trivia(source, scan) };
                    if scan_position >= source.len() {
                        break;
                    }
                    if let Ok(node) =
                        interpret_rule(&mut context, lookup, iteration_target, scan_position)
                    {
                        recovered = Some(node);
                        break;
                    }
                    scan = next_char_boundary(source, scan);
                }
                let recovery_end = match &recovered {
                    Some(node) => node.span.start.saturating_sub(source_offset),
                    None => source.len(),
                };
                if recovery_end > recovery_start {
                    children.push(RuleNode {
                        kind: RuleNodeKind::RecoveryError,
                        span: Span::new(
                            source_offset + recovery_start,
                            source_offset + recovery_end,
                        ),
                        text: &source[recovery_start..recovery_end],
                        children: Vec::new(),
                        binding_name: None,
                    });
                }
                if let Some(node) = recovered {
                    let advance = node.span.end.saturating_sub(source_offset);
                    children.push(node);
                    position = advance;
                } else {
                    position = source.len();
                }
            }
        }
    }

    let root = RuleNode {
        kind: RuleNodeKind::Rule { name: root_rule },
        span: Span::new(source_offset, source_offset + source.len()),
        text: source,
        children,
        binding_name: None,
    };
    RecoveryReport {
        root: Some(root),
        errors,
        metrics: context.finish_metrics(),
    }
}

fn next_char_boundary(source: &str, position: usize) -> usize {
    if position >= source.len() {
        return source.len();
    }
    let mut next = position + 1;
    while next < source.len() && !source.is_char_boundary(next) {
        next += 1;
    }
    next
}

fn inspect_region_with<'g, 's>(
    lookup: RuleLookup<'_, 'g>,
    root_rule: &'g str,
    source: &'s str,
    source_offset: usize,
) -> ParseReport<'g, 's> {
    let mut context = ParseContext::new(source, source_offset);
    let root_raw = lookup
        .rule(root_rule)
        .is_some_and(|rule| matches!(rule.trivia, TriviaMode::Raw));
    let root = match interpret_rule(&mut context, lookup, root_rule, 0) {
        Ok(root) => root,
        Err(failure) => {
            return ParseReport {
                root: None,
                error: Some(failure.into_error(source_offset)),
                metrics: context.finish_metrics(),
            };
        }
    };
    let region_end = root.span.end.saturating_sub(source_offset);
    // Raw root rules keep trailing trivia as unconsumed input; default
    // rules let trailing whitespace and `//` comments slide.
    let trailing = if root_raw {
        region_end
    } else {
        skip_trivia(source, region_end)
    };
    if trailing == source.len() {
        ParseReport {
            root: Some(root),
            error: None,
            metrics: context.finish_metrics(),
        }
    } else {
        ParseReport {
            root: Some(root),
            error: Some(
                ParseFailure::expected_at_source(
                    source,
                    trailing,
                    ExpectedFragment::EndOfRegion,
                    root_raw,
                )
                .into_error(source_offset),
            ),
            metrics: context.finish_metrics(),
        }
    }
}

/// Interprets one rule and returns the failure that rule would produce.
///
/// This helper is for diagnostics and recovery. It does not require the full
/// region to be consumed, and it returns the [`ParseError`] that matching
/// `rule_name` at `position` would produce.
#[must_use]
pub fn interpret_rule_error<'g>(
    rules: &[SourceAuthoredRule<'g>],
    rule_name: &'g str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> ParseError<'g> {
    interpret_rule_error_with(
        RuleLookup::from_slice(rules),
        rule_name,
        source,
        position,
        source_offset,
    )
}

/// Same as [`interpret_rule_error`] but accepts a pre-built [`CompiledGrammar`].
#[must_use]
pub fn interpret_rule_error_compiled<'g>(
    grammar: &CompiledGrammar<'g>,
    rule_name: &'g str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> ParseError<'g> {
    interpret_rule_error_with(grammar.lookup(), rule_name, source, position, source_offset)
}

fn interpret_rule_error_with<'g>(
    lookup: RuleLookup<'_, 'g>,
    rule_name: &'g str,
    source: &str,
    position: usize,
    source_offset: usize,
) -> ParseError<'g> {
    let raw = lookup
        .rule(rule_name)
        .is_some_and(|rule| matches!(rule.trivia, TriviaMode::Raw));
    let mut context = ParseContext::new(source, source_offset);
    interpret_rule(&mut context, lookup, rule_name, position)
        .err()
        .unwrap_or_else(|| {
            ParseFailure::expected_at_source(
                source,
                position,
                ExpectedFragment::Rule(rule_name),
                raw,
            )
        })
        .into_error(source_offset)
}

fn interpret_rule<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    name: &'g str,
    position: usize,
) -> Result<RuleNode<'g, 's>, ParseFailure<'g>> {
    context.metrics.rule_attempts = context.metrics.rule_attempts.saturating_add(1);
    if let Some((rule, active)) = lookup.resolve_rule(name) {
        let key = MemoKey {
            scope: active.memo_scope(),
            name,
            position,
        };
        if let Some(entry) = context.memo.iter().find(|entry| entry.key == key) {
            context.metrics.memo_hits = context.metrics.memo_hits.saturating_add(1);
            return entry.result.clone();
        }
        // Direct left recursion is handled by seed/grow below. Other cycles
        // at the same input position have no supported growing interpretation;
        // reject that path instead of overflowing the host stack.
        if context.active_rules.contains(&key) {
            return Err(ParseFailure::expected_at_source(
                context.source,
                position,
                ExpectedFragment::Rule(name),
                false,
            ));
        }
        context.metrics.memo_misses = context.metrics.memo_misses.saturating_add(1);
        // `resolve_rule` switches to the rule's authoring grammar so
        // `super` references inside an inherited rule resolve against
        // that rule's own parent chain, not against whichever grammar
        // the lookup started in.
        context.active_rules.push(key);
        let result = interpret_rule_definition(context, active, rule, name, position);
        context.active_rules.pop();
        context.memo.push(MemoEntry {
            key,
            result: result.clone(),
        });
        return result;
    }
    // Rule not found: we have no per-rule trivia context, so report the
    // unexpected fragment using the default (trivia-skipping) heuristic.
    let key = MemoKey {
        scope: lookup.memo_scope(),
        name,
        position,
    };
    if let Some(entry) = context.memo.iter().find(|entry| entry.key == key) {
        context.metrics.memo_hits = context.metrics.memo_hits.saturating_add(1);
        return entry.result.clone();
    }
    context.metrics.memo_misses = context.metrics.memo_misses.saturating_add(1);
    let result = Err(ParseFailure::expected_at_source(
        context.source,
        position,
        ExpectedFragment::Rule(name),
        false,
    ));
    context.memo.push(MemoEntry {
        key,
        result: result.clone(),
    });
    result
}

/// Runs `rule`'s body using `lookup` for inner rule references and
/// `name` as the label on the produced node.
///
/// The caller (`interpret_rule`) is responsible for selecting the
/// right `lookup`: it routes to the rule's authoring ancestor using
/// the rule-owner depth tracked in [`CompiledGrammar`], so any
/// `Rule(...)` or `Super(...)` references inside `rule`'s body
/// resolve against the grammar that actually authored the rule —
/// not against an arbitrary descendant the rule was looked up
/// through.
fn interpret_rule_definition<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    rule: &SourceAuthoredRule<'g>,
    name: &'g str,
    position: usize,
) -> Result<RuleNode<'g, 's>, ParseFailure<'g>> {
    let raw = matches!(rule.trivia, TriviaMode::Raw);
    let start = if raw {
        position
    } else {
        skip_trivia(context.source, position)
    };
    // Direct left recursion: when the rule's body is a Choice with at
    // least one alternative starting with a self-reference, seed from
    // the non-LR alternatives and iteratively grow with the LR tails.
    // This is the classical PEG-style left-recursion technique
    // (Sergio Medeiros' bounded recursion / Warth's grow-the-seed
    // without packrat memoization).
    if let Some((non_lr_alts, lr_tails)) = direct_lr_partitions(&rule.expression, name) {
        return interpret_left_recursive_rule(
            context,
            lookup,
            rule,
            name,
            start,
            raw,
            &non_lr_alts,
            &lr_tails,
        );
    }
    let matched = interpret_expression(context, lookup, &[], &rule.expression, start, raw)?;
    let end = matched.end;
    if rule
        .excluded_text
        .iter()
        .any(|excluded| *excluded == &context.source[start..end])
    {
        return Err(ParseFailure::expected_at_source(
            context.source,
            start,
            ExpectedFragment::Rule(name),
            raw,
        ));
    }
    Ok(RuleNode {
        kind: RuleNodeKind::Rule { name },
        span: Span::new(context.source_offset + start, context.source_offset + end),
        text: &context.source[start..end],
        children: matched.children,
        binding_name: None,
    })
}

/// Detects direct left recursion in a rule body.
///
/// Returns `Some((non_lr_alts, lr_tails))` when `expression` is a
/// [`RuleExpression::Choice`] with at least one alternative whose
/// leading element is `Rule(rule_name)`. `non_lr_alts` is the list of
/// alternatives that do not self-reference; `lr_tails` is the list of
/// per-LR-alternative tails (everything after the leading self
/// reference). Returns `None` for rules that aren't directly left
/// recursive — the caller uses the ordinary recursive descent path.
type LrTail<'r, 'g> = (Option<&'g str>, Vec<&'r RuleExpression<'g>>);

fn direct_lr_partitions<'r, 'g>(
    expression: &'r RuleExpression<'g>,
    rule_name: &str,
) -> Option<(Vec<&'r RuleExpression<'g>>, Vec<LrTail<'r, 'g>>)> {
    let RuleExpression::Choice(alts) = expression else {
        return None;
    };
    let mut non_lr = Vec::new();
    let mut lr_tails = Vec::new();
    for alt in alts {
        if let Some(entry) = lr_alternative_tail(alt, rule_name) {
            lr_tails.push(entry);
        } else {
            non_lr.push(alt);
        }
    }
    if lr_tails.is_empty() {
        None
    } else {
        Some((non_lr, lr_tails))
    }
}

/// Detects whether `alt` is a left-recursive alternative for
/// `rule_name`. Returns `(binding, tail)` where `binding` carries the
/// `name:expr` label applied to the leading self-reference (if any) and
/// `tail` is the rest of the sequence after that self-reference.
///
/// Shapes recognized as LR:
/// - bare `Rule(self)` → `(None, [])`
/// - `Named { name, inner: Rule(self) }` → `(Some(name), [])`
/// - `Sequence([Rule(self), tail...])` → `(None, [tail...])`
/// - `Sequence([Named { name, inner: Rule(self) }, tail...])`
///   → `(Some(name), [tail...])`
///
/// Other shapes — including a `Named` wrapper around an entire
/// sequence whose first item is the self-reference — fall through so
/// the caller treats them as non-LR. Those forms are rare and the
/// binding semantics are ambiguous; users who hit them should rewrite
/// to one of the supported shapes.
fn lr_alternative_tail<'r, 'g>(
    alt: &'r RuleExpression<'g>,
    rule_name: &str,
) -> Option<LrTail<'r, 'g>> {
    if let Some(binding) = peel_self_reference(alt, rule_name) {
        return Some((binding, Vec::new()));
    }
    let items = match alt {
        RuleExpression::Sequence(items) => items.as_slice(),
        _ => return None,
    };
    let first = items.first()?;
    let binding = peel_self_reference(first, rule_name)?;
    Some((binding, items[1..].iter().collect()))
}

/// Returns `Some(binding_name)` when `expr` is `Rule(rule_name)` or a
/// `Named { inner: Rule(rule_name) }` wrapper around it. `binding_name`
/// is `None` for the bare reference and `Some(name)` for the
/// `name:expr` wrapper.
fn peel_self_reference<'g>(expr: &RuleExpression<'g>, rule_name: &str) -> Option<Option<&'g str>> {
    match expr {
        RuleExpression::Rule(name) if *name == rule_name => Some(None),
        RuleExpression::Named { name, inner } => match inner.as_ref() {
            RuleExpression::Rule(inner_name) if *inner_name == rule_name => Some(Some(*name)),
            _ => None,
        },
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn interpret_left_recursive_rule<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    rule: &SourceAuthoredRule<'g>,
    name: &'g str,
    start: usize,
    raw: bool,
    non_lr_alts: &[&RuleExpression<'g>],
    lr_tails: &[LrTail<'_, 'g>],
) -> Result<RuleNode<'g, 's>, ParseFailure<'g>> {
    // Seed: match the first successful non-LR alternative.
    let mut accumulated: Option<ParseFailure<'g>> = None;
    let mut seed: Option<ExpressionMatch<'g, 's>> = None;
    for alt in non_lr_alts {
        match interpret_expression(context, lookup, &[], alt, start, raw) {
            Ok(matched) => {
                seed = Some(matched);
                break;
            }
            Err(failure) => merge_failure(&mut accumulated, failure),
        }
    }
    let seed = seed.ok_or_else(|| {
        accumulated.unwrap_or_else(|| {
            ParseFailure::expected_at_source(
                context.source,
                start,
                ExpectedFragment::Rule(name),
                raw,
            )
        })
    })?;
    let mut current_end = seed.end;
    let mut current_node = RuleNode {
        kind: RuleNodeKind::Rule { name },
        span: Span::new(
            context.source_offset + start,
            context.source_offset + current_end,
        ),
        text: &context.source[start..current_end],
        children: seed.children,
        binding_name: None,
    };
    // Grow: at each iteration try each LR tail at the current end and
    // wrap the previous match into a new rule node when an LR tail
    // extends. The tail's `binding` (from `name:expr` on the leading
    // self-reference, if any) is applied to the previous match when it
    // becomes a child of the new outer node.
    loop {
        let mut grew = false;
        for (binding, tail) in lr_tails {
            let mut tail_end = current_end;
            let mut tail_children = Vec::new();
            let mut tail_ok = true;
            for item in tail {
                match interpret_expression(context, lookup, &[], item, tail_end, raw) {
                    Ok(matched) => {
                        tail_end = matched.end;
                        tail_children.extend(matched.children);
                    }
                    Err(_) => {
                        tail_ok = false;
                        break;
                    }
                }
            }
            if tail_ok && tail_end > current_end {
                let mut prior = current_node.clone();
                prior.binding_name = *binding;
                let mut combined = Vec::with_capacity(tail_children.len() + 1);
                combined.push(prior);
                combined.extend(tail_children);
                current_end = tail_end;
                current_node = RuleNode {
                    kind: RuleNodeKind::Rule { name },
                    span: Span::new(
                        context.source_offset + start,
                        context.source_offset + current_end,
                    ),
                    text: &context.source[start..current_end],
                    children: combined,
                    binding_name: None,
                };
                grew = true;
                break;
            }
        }
        if !grew {
            break;
        }
    }
    // Excluded-text check applies to the final matched span.
    if rule
        .excluded_text
        .iter()
        .any(|excluded| *excluded == &context.source[start..current_end])
    {
        return Err(ParseFailure::expected_at_source(
            context.source,
            start,
            ExpectedFragment::Rule(name),
            raw,
        ));
    }
    Ok(current_node)
}

/// Parameter binding active while interpreting a parametric rule body.
///
/// Each binding remembers the argument expression plus the *caller's*
/// execution context (its lookup and its own active bindings) at the
/// moment the parametric `Apply` was invoked. When the body refers to
/// a parameter, the argument is interpreted in that caller frame so
/// references inside the argument resolve through the caller's
/// grammar and through the caller's own parameter scope — important
/// for chained parametric calls like
/// `trailing-list-of<elem, sep> = list-of<elem, sep> sep?` where the
/// inner `Apply`'s argument expressions are themselves parameter
/// references from the outer frame.
#[derive(Clone, Copy)]
struct ParamBinding<'a, 'g> {
    name: &'g str,
    arg: &'a RuleExpression<'g>,
    caller_lookup: RuleLookup<'a, 'g>,
    caller_bindings: &'a [ParamBinding<'a, 'g>],
}

fn params_memo_key<'g>(params: &[ParamBinding<'_, 'g>]) -> Vec<ParamBindingMemoKey<'g>> {
    params
        .iter()
        .map(|param| ParamBindingMemoKey {
            name: param.name,
            arg: param.arg.clone(),
            caller_scope: param.caller_lookup.memo_scope(),
            caller_bindings: params_memo_key(param.caller_bindings),
        })
        .collect()
}

// Keep recursive dispatch small: a single match containing every interpreter
// operation reserves a large debug-build stack frame even for leaf rules.
fn interpret_expression<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Rule(_) | RuleExpression::Super(_) => {
            interpret_reference(context, lookup, params, expression, position, raw)
        }
        RuleExpression::Literal(_)
        | RuleExpression::CharacterClass(_)
        | RuleExpression::UntilLiteral(_) => {
            interpret_atom(context, lookup, params, expression, position, raw)
        }
        RuleExpression::Sequence(_) => {
            interpret_sequence(context, lookup, params, expression, position, raw)
        }
        RuleExpression::Choice(_) => {
            interpret_choice(context, lookup, params, expression, position, raw)
        }
        RuleExpression::Optional(_)
        | RuleExpression::OneOrMore(_)
        | RuleExpression::ZeroOrMore(_) => {
            interpret_repetition(context, lookup, params, expression, position, raw)
        }
        RuleExpression::Transform { .. }
        | RuleExpression::And(_)
        | RuleExpression::Not(_)
        | RuleExpression::Cut(_)
        | RuleExpression::Named { .. } => {
            interpret_modifier(context, lookup, params, expression, position, raw)
        }
        RuleExpression::Apply { .. } => {
            interpret_application(context, lookup, params, expression, position, raw)
        }
    }
}

fn interpret_reference<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Rule(name) => {
            // Inside a parametric body, a rule name that matches a
            // parameter dispatches to the bound argument expression
            // — interpreted in the *caller's* lookup so anything the
            // caller wrote (e.g. `Rule("digit")` for a child rule)
            // still resolves in the caller's grammar. The parameter
            // binding does not propagate into the recursive call,
            // so nested parametric invocations get a fresh frame.
            if let Some(binding) = params.iter().find(|b| b.name == *name) {
                return interpret_expression(
                    context,
                    binding.caller_lookup,
                    binding.caller_bindings,
                    binding.arg,
                    position,
                    raw,
                );
            }
            // Cut commits never cross rule boundaries; the rule's own choice
            // already cleared the flag on success, and we deliberately drop
            // it on failure too.
            let node = interpret_rule(context, lookup, name, position).map_err(|mut failure| {
                failure.committed = false;
                failure
            })?;
            Ok(ExpressionMatch {
                end: node.span.end.saturating_sub(context.source_offset),
                children: vec![node],
                committed: false,
            })
        }
        RuleExpression::Super(name) => {
            // Walk one level up from the *active* grammar and let the
            // normal rule lookup take over. Because `interpret_rule`
            // routes to the rule's authoring ancestor, `super.foo`
            // here always reaches the foo that the active grammar's
            // immediate parent would match — including the case
            // where that foo is itself inherited from further up.
            let parent_lookup = lookup.parent().ok_or_else(|| {
                ParseFailure::expected_at_source(
                    context.source,
                    position,
                    ExpectedFragment::Rule(name),
                    raw,
                )
            })?;
            let node =
                interpret_rule(context, parent_lookup, name, position).map_err(|mut failure| {
                    failure.committed = false;
                    failure
                })?;
            Ok(ExpressionMatch {
                end: node.span.end.saturating_sub(context.source_offset),
                children: vec![node],
                committed: false,
            })
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

fn interpret_atom<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    _lookup: RuleLookup<'_, 'g>,
    _params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Literal(value) => {
            let start = if raw {
                position
            } else {
                skip_trivia(context.source, position)
            };
            let end = context.source[start..]
                .starts_with(value)
                .then_some(start + value.len())
                .filter(|end| literal_boundary_ok(context.source, *end, value))
                .ok_or_else(|| {
                    ParseFailure::expected_at_source(
                        context.source,
                        start,
                        ExpectedFragment::Literal(value),
                        raw,
                    )
                })?;
            Ok(ExpressionMatch {
                end,
                children: vec![RuleNode {
                    kind: RuleNodeKind::Literal { value },
                    span: Span::new(context.source_offset + start, context.source_offset + end),
                    text: &context.source[start..end],
                    children: Vec::new(),
                    binding_name: None,
                }],
                committed: false,
            })
        }
        RuleExpression::CharacterClass(value) => {
            let start = position;
            let ch = context.source[start..]
                .chars()
                .next()
                .filter(|ch| character_class_contains(value, *ch))
                .ok_or_else(|| {
                    ParseFailure::expected_at_source(
                        context.source,
                        start,
                        ExpectedFragment::CharacterClass(value),
                        raw,
                    )
                })?;
            let end = start + ch.len_utf8();
            Ok(ExpressionMatch {
                end,
                children: vec![RuleNode {
                    kind: RuleNodeKind::CharacterClass { value },
                    span: Span::new(context.source_offset + start, context.source_offset + end),
                    text: &context.source[start..end],
                    children: Vec::new(),
                    binding_name: None,
                }],
                committed: false,
            })
        }
        RuleExpression::UntilLiteral(value) => {
            let start = position;
            let relative_end = context.source[start..]
                .match_indices(value)
                .find_map(|(index, _)| {
                    let end = start + index + value.len();
                    literal_boundary_ok(context.source, end, value).then_some(start + index)
                })
                .unwrap_or(context.source.len());
            Ok(ExpressionMatch {
                end: relative_end,
                children: vec![RuleNode {
                    kind: RuleNodeKind::OpaqueRegion { delimiter: value },
                    span: Span::new(
                        context.source_offset + start,
                        context.source_offset + relative_end,
                    ),
                    text: &context.source[start..relative_end],
                    children: Vec::new(),
                    binding_name: None,
                }],
                committed: false,
            })
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

fn interpret_sequence<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Sequence(items) => {
            let mut end = position;
            let mut children = Vec::new();
            let mut committed = false;
            for item in items {
                match interpret_expression(context, lookup, params, item, end, raw) {
                    Ok(matched) => {
                        committed |= matched.committed;
                        end = matched.end;
                        children.extend(matched.children);
                    }
                    Err(mut failure) => {
                        failure.committed |= committed;
                        return Err(failure);
                    }
                }
            }
            Ok(ExpressionMatch {
                end,
                children,
                committed,
            })
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

fn interpret_choice<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Choice(items) => {
            let mut failure = None;
            for item in items {
                match interpret_expression(context, lookup, params, item, position, raw) {
                    Ok(matched) => {
                        return Ok(ExpressionMatch {
                            end: matched.end,
                            children: matched.children,
                            committed: false,
                        });
                    }
                    Err(error) => {
                        if error.committed {
                            return Err(ParseFailure {
                                committed: false,
                                ..error
                            });
                        }
                        merge_failure(&mut failure, error);
                    }
                }
            }
            Err(failure.unwrap_or_else(|| {
                ParseFailure::expected_at_source(
                    context.source,
                    position,
                    ExpectedFragment::Choice,
                    raw,
                )
            }))
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

fn interpret_repetition<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Optional(item) => {
            // Cut commits bounded by the innermost choice; an Optional
            // contains the commit, on both the matched and skipped paths.
            match interpret_expression(context, lookup, params, item, position, raw) {
                Ok(mut matched) => {
                    matched.committed = false;
                    Ok(matched)
                }
                Err(_) => Ok(ExpressionMatch {
                    end: position,
                    children: Vec::new(),
                    committed: false,
                }),
            }
        }
        RuleExpression::OneOrMore(item) => {
            // A repetition boundary contains any inner cut commit. Both the
            // required first match and later successful iterations have
            // their committed flag cleared before propagating outward.
            let first = interpret_expression(context, lookup, params, item, position, raw)
                .map_err(|mut failure| {
                    failure.committed = false;
                    failure
                })?;
            let mut end = first.end;
            let mut children = first.children;
            while let Ok(matched) = interpret_expression(context, lookup, params, item, end, raw) {
                if matched.end == end {
                    break;
                }
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch {
                end,
                children,
                committed: false,
            })
        }
        RuleExpression::ZeroOrMore(item) => {
            // Repetition boundary contains the cut commit; iteration
            // failures are silently swallowed by the loop, so only success
            // propagation needs the explicit clear.
            let mut end = position;
            let mut children = Vec::new();
            while let Ok(matched) = interpret_expression(context, lookup, params, item, end, raw) {
                if matched.end == end {
                    break;
                }
                end = matched.end;
                children.extend(matched.children);
            }
            Ok(ExpressionMatch {
                end,
                children,
                committed: false,
            })
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

fn interpret_modifier<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Transform { input, .. } => {
            interpret_expression(context, lookup, params, input, position, raw)
        }
        RuleExpression::And(inner) => {
            // Lookahead is non-consuming inspection; an inner cut commit
            // must not extend out to the enclosing choice scope.
            interpret_expression(context, lookup, params, inner, position, raw).map_err(
                |mut failure| {
                    failure.committed = false;
                    failure
                },
            )?;
            Ok(ExpressionMatch {
                end: position,
                children: Vec::new(),
                committed: false,
            })
        }
        RuleExpression::Not(inner) => {
            match interpret_expression(context, lookup, params, inner, position, raw) {
                Ok(_) => Err(ParseFailure::expected_at_source(
                    context.source,
                    position,
                    ExpectedFragment::NegativeLookahead,
                    raw,
                )),
                Err(_) => Ok(ExpressionMatch {
                    end: position,
                    children: Vec::new(),
                    committed: false,
                }),
            }
        }
        RuleExpression::Cut(inner) => {
            let mut matched = interpret_expression(context, lookup, params, inner, position, raw)?;
            matched.committed = true;
            Ok(matched)
        }
        RuleExpression::Named { name, inner } => {
            let matched = interpret_expression(context, lookup, params, inner, position, raw)?;
            let children = matched
                .children
                .into_iter()
                .map(|mut child| {
                    child.binding_name = Some(name);
                    child
                })
                .collect();
            Ok(ExpressionMatch {
                end: matched.end,
                children,
                committed: matched.committed,
            })
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

fn interpret_application<'g, 's>(
    context: &mut ParseContext<'g, 's>,
    lookup: RuleLookup<'_, 'g>,
    params: &[ParamBinding<'_, 'g>],
    expression: &RuleExpression<'g>,
    position: usize,
    raw: bool,
) -> Result<ExpressionMatch<'g, 's>, ParseFailure<'g>> {
    match expression {
        RuleExpression::Apply { rule, args } => {
            // Switch the active lookup to the parametric target's
            // authoring ancestor (so any `Rule(...)` / `Super(...)`
            // refs in the target's body resolve in the body's own
            // grammar), but remember the caller's lookup on each
            // parameter binding so argument expressions — which were
            // written in the caller's grammar — still resolve through
            // the caller's scope when the body references them by
            // parameter name. This is the kernel's analogue of "args
            // are evaluated by the caller, bodies are evaluated by
            // the callee."
            let (target, active) = lookup.resolve_rule(rule).ok_or_else(|| {
                ParseFailure::expected_at_source(
                    context.source,
                    position,
                    ExpectedFragment::Rule(rule),
                    raw,
                )
            })?;
            let key = ApplyMemoKey {
                scope: active.memo_scope(),
                caller_scope: lookup.memo_scope(),
                name: rule,
                position,
                args: args.clone(),
                params: params_memo_key(params),
            };
            if let Some(entry) = context.apply_memo.iter().find(|entry| entry.key == key) {
                context.metrics.memo_hits = context.metrics.memo_hits.saturating_add(1);
                return entry.result.clone();
            }
            context.metrics.memo_misses = context.metrics.memo_misses.saturating_add(1);
            let bindings: Vec<ParamBinding<'_, 'g>> = target
                .params
                .iter()
                .zip(args.iter())
                .map(|(name, arg)| ParamBinding {
                    name,
                    arg,
                    caller_lookup: lookup,
                    caller_bindings: params,
                })
                .collect();
            let result = interpret_expression(
                context,
                active,
                &bindings,
                &target.expression,
                position,
                raw,
            );
            context.apply_memo.push(ApplyMemoEntry {
                key,
                result: result.clone(),
            });
            result
        }
        _ => unreachable!("expression dispatched to the matching interpreter"),
    }
}

impl<'g> ParseFailure<'g> {
    fn expected_at_source(
        source: &str,
        position: usize,
        expected: ExpectedFragment<'g>,
        raw: bool,
    ) -> Self {
        Self {
            span: unexpected_fragment_span(source, position, raw),
            expected: vec![expected],
            committed: false,
        }
    }

    const fn position(&self) -> usize {
        self.span.start
    }

    fn into_error(self, source_offset: usize) -> ParseError<'g> {
        ParseError {
            span: Span::new(
                source_offset + self.span.start,
                source_offset + self.span.end,
            ),
            expected: self.expected,
        }
    }
}

fn merge_failure<'g>(failure: &mut Option<ParseFailure<'g>>, next: ParseFailure<'g>) {
    match failure {
        Some(current) if current.position() > next.position() => {}
        Some(current) if current.position() == next.position() => {
            if next.span.end > current.span.end {
                current.span.end = next.span.end;
            }
            for expected in next.expected {
                if !current.expected.contains(&expected) {
                    current.expected.push(expected);
                }
            }
        }
        _ => *failure = Some(next),
    }
}

fn unexpected_fragment_span(source: &str, position: usize, raw: bool) -> Span {
    let start = if raw {
        position
    } else {
        skip_trivia(source, position)
    };
    let Some(first) = source[start..].chars().next() else {
        return Span::new(start, start);
    };
    if is_delimiter_like(first) {
        return Span::new(start, start + first.len_utf8());
    }
    let end = source[start..]
        .char_indices()
        .skip(1)
        .find_map(|(index, ch)| {
            (ch.is_whitespace() || is_delimiter_like(ch)).then_some(start + index)
        })
        .unwrap_or(source.len());
    Span::new(start, end)
}

fn is_delimiter_like(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':' | '.' | '"' | '\'' | '`'
    )
}

fn character_class_contains(class: &str, ch: char) -> bool {
    let (negated, body) = match class.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, class),
    };
    let bytes = body.as_bytes();
    let mut found = false;
    let mut index = 0;
    while index < bytes.len() {
        let head = &body[index..];
        let current = head.chars().next().expect("non-empty head");
        let current_len = current.len_utf8();

        if current == '\\' && index + 1 < bytes.len() {
            let escaped = body[index + 1..].chars().next().expect("non-empty escape");
            if shorthand_class_matches(escaped, ch) || escape_value(escaped) == ch {
                found = true;
            }
            index += 1 + escaped.len_utf8();
            continue;
        }

        let after_current = index + current_len;
        if after_current < bytes.len() && bytes[after_current] == b'-' {
            let after_dash = after_current + 1;
            if after_dash < bytes.len() {
                let end = body[after_dash..].chars().next().expect("non-empty end");
                if end != '\\' {
                    if current <= ch && ch <= end {
                        found = true;
                    }
                    index = after_dash + end.len_utf8();
                    continue;
                }
            }
        }

        if current == ch {
            found = true;
        }
        index = after_current;
    }
    found ^ negated
}

fn shorthand_class_matches(escape: char, ch: char) -> bool {
    match escape {
        'd' => ch.is_ascii_digit(),
        'D' => !ch.is_ascii_digit(),
        'w' => ch == '_' || ch.is_ascii_alphanumeric(),
        'W' => !(ch == '_' || ch.is_ascii_alphanumeric()),
        's' => matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c'),
        'S' => !matches!(ch, ' ' | '\t' | '\n' | '\r' | '\x0c'),
        _ => false,
    }
}

fn escape_value(escape: char) -> char {
    match escape {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        other => other,
    }
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

/// Returns the 1-based line and byte column of `offset` inside `source`.
///
/// Lines are split by `'\n'` (CRLF is treated as a trailing carriage return
/// on the previous line). Columns count bytes from the start of the line; for
/// ASCII grammars this matches character columns. When `offset` is past the
/// end of `source`, the location is clamped to one past the final byte of the
/// last line so callers can render end-of-file diagnostics.
#[must_use]
pub fn line_column_at_byte_offset(source: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(source.len());
    let mut line = 1_usize;
    let mut line_start = 0_usize;
    for (index, byte) in source.as_bytes().iter().enumerate().take(clamped) {
        if *byte == b'\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    let column = clamped - line_start + 1;
    (line, column)
}

/// Counts parse tree nodes per rule name.
///
/// Pass a [`RuleNode`], usually [`Parse::root`], to summarize which grammar
/// nodes contributed to a parse. This includes anonymous literal, character
/// class, and opaque-region nodes. Use [`named_rule_counts`] when the caller
/// wants counts for grammar rules only.
#[must_use]
pub fn rule_counts<'g>(root: &RuleNode<'g, '_>) -> Vec<RuleCount<'g>> {
    let mut counts = Vec::<RuleCount>::new();
    count_rule(root, &mut counts, true);
    counts
}

/// Counts named grammar-rule nodes per rule name.
///
/// Pass a [`RuleNode`], usually [`Parse::root`], to summarize which named
/// grammar rules contributed to a parse. `interloko_compiler` uses this for
/// staged-region rule provenance so literal/class helper nodes do not appear as
/// grammar rules.
#[must_use]
pub fn named_rule_counts<'g>(root: &RuleNode<'g, '_>) -> Vec<RuleCount<'g>> {
    let mut counts = Vec::<RuleCount>::new();
    count_rule(root, &mut counts, false);
    counts
}

/// Count for one parse-tree rule name returned by [`rule_counts`].
///
/// This is a reporting/provenance record, not parser input. It lets callers
/// show which rules were exercised without walking the parse tree themselves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleCount<'g> {
    /// Stable parse-tree node kind.
    pub node_kind: RuleNodeKind<'g>,
    /// Number of parse tree nodes for this rule.
    pub node_count: usize,
}

impl<'g> RuleCount<'g> {
    /// Returns the node label used when displaying this count.
    #[must_use]
    pub fn label(&self) -> RuleNodeKindLabel<'g> {
        self.node_kind.label()
    }

    /// Returns the named grammar rule, if this count is for a named-rule node.
    #[must_use]
    pub const fn rule_name(&self) -> Option<&'g str> {
        self.node_kind.name()
    }
}

fn count_rule<'g>(
    node: &RuleNode<'g, '_>,
    counts: &mut Vec<RuleCount<'g>>,
    include_anonymous: bool,
) {
    if include_anonymous || node.is_named() {
        if let Some(count) = counts.iter_mut().find(|count| count.node_kind == node.kind) {
            count.node_count = count.node_count.saturating_add(1);
        } else {
            counts.push(RuleCount {
                node_kind: node.kind,
                node_count: 1,
            });
        }
    }
    for child in &node.children {
        count_rule(child, counts, include_anonymous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovering_parse_skips_broken_middle_and_resumes() {
        // source-file = definition*; definition starts with `define`.
        // The middle "BAD STUFF" can't match definition; recovery
        // skips it, emits one RecoveryError node, and matches the
        // trailing definition.
        let grammar = CompiledGrammar::compile(parse_grammar_source(
            "grammar tiny\n\
             rule source-file = definition*\n\
             rule definition = `define` name\n\
             rule name = [A-Za-z]+\n\
             end\n",
        ));
        let source = "define alpha\nBAD STUFF\ndefine beta";
        let report = interpret_region_recovering(&grammar, "source-file", source, 0);

        let root = report
            .root
            .as_ref()
            .expect("recovery always returns a root");
        let definitions: Vec<_> = root
            .children
            .iter()
            .filter(|child| child.is_rule("definition"))
            .collect();
        assert_eq!(definitions.len(), 2, "two definitions matched");
        assert!(definitions[0].text.starts_with("define alpha"));
        assert!(definitions[1].text.starts_with("define beta"));

        let recoveries: Vec<_> = root
            .children
            .iter()
            .filter(|child| child.role() == RuleNodeRole::RecoveryError)
            .collect();
        assert_eq!(recoveries.len(), 1, "one skipped region in between");
        let recovery = recoveries[0];
        assert!(
            recovery.text.contains("BAD STUFF"),
            "recovery node covers the skipped text: {:?}",
            recovery.text,
        );

        assert!(!report.errors.is_empty(), "recovery records the failure");
    }

    #[test]
    fn recovering_parse_emits_trailing_error_when_no_resync() {
        // source-file = definition*; the trailing junk does not
        // begin another definition, so recovery scans to EOF and
        // emits one trailing RecoveryError node.
        let grammar = CompiledGrammar::compile(parse_grammar_source(
            "grammar tiny\n\
             rule source-file = definition*\n\
             rule definition = `define` name\n\
             rule name = [A-Za-z]+\n\
             end\n",
        ));
        let source = "define alpha\nGARBAGE GARBAGE";
        let report = interpret_region_recovering(&grammar, "source-file", source, 0);

        let root = report
            .root
            .as_ref()
            .expect("recovery always returns a root");
        let definitions: Vec<_> = root
            .children
            .iter()
            .filter(|child| child.is_rule("definition"))
            .collect();
        assert_eq!(definitions.len(), 1);

        let recoveries: Vec<_> = root
            .children
            .iter()
            .filter(|child| child.role() == RuleNodeRole::RecoveryError)
            .collect();
        assert_eq!(recoveries.len(), 1);
        assert!(recoveries[0].text.contains("GARBAGE"));
    }

    #[test]
    fn recovering_parse_falls_back_for_non_repetition_root() {
        // Root rule is a single literal — not a repetition. Recovery
        // falls back to inspect-style parsing. Successful input
        // produces a root with no errors.
        let grammar = CompiledGrammar::compile(parse_grammar_source(
            "grammar tiny\nrule greeting = `hello`\nend\n",
        ));
        let report = interpret_region_recovering(&grammar, "greeting", "hello", 0);
        assert!(report.errors.is_empty());
        let root = report.root.expect("greeting matched");
        assert_eq!(root.text, "hello");
    }

    #[test]
    fn recovering_parse_round_trips_clean_input() {
        // No recovery needed; recovery report should match ordinary
        // parse output for the well-formed prefix.
        let grammar = CompiledGrammar::compile(parse_grammar_source(
            "grammar tiny\n\
             rule source-file = definition*\n\
             rule definition = `define` name\n\
             rule name = [A-Za-z]+\n\
             end\n",
        ));
        let report = interpret_region_recovering(
            &grammar,
            "source-file",
            "define alpha\ndefine beta\ndefine gamma",
            0,
        );
        assert!(report.errors.is_empty());
        let root = report.root.expect("clean input parses");
        let definition_count = root
            .children
            .iter()
            .filter(|child| child.is_rule("definition"))
            .count();
        assert_eq!(definition_count, 3);
        assert!(
            root.children
                .iter()
                .all(|child| child.role() != RuleNodeRole::RecoveryError),
            "no recovery nodes on clean input",
        );
    }

    #[test]
    fn packrat_memo_reuses_successful_rule_application() {
        // Both alternatives start by matching `item` at byte 0.
        // The first alternative fails on `x`; the second should reuse
        // the earlier successful `item` match before accepting `y`.
        let rules = parse_grammar_source(
            "grammar tiny\n\
             rule root = item `x` | item `y`\n\
             rule item = [a]\n\
             end\n",
        );
        let report = inspect_region(&rules, "root", "ay", 0);

        assert!(report.is_success());
        assert_eq!(report.metrics.memo_hits, 1);
        assert_eq!(report.metrics.memo_misses, 2);
        assert_eq!(report.metrics.memo_entries, 2);
    }

    #[test]
    fn packrat_memo_reuses_failed_rule_application() {
        // The first alternative records that `item` cannot match at
        // byte 0. The second alternative should reuse that failure
        // rather than matching `item` again.
        let rules = parse_grammar_source(
            "grammar tiny\n\
             rule root = item `x` | item `y`\n\
             rule item = [a]\n\
             end\n",
        );
        let report = inspect_region(&rules, "root", "by", 0);

        assert!(!report.is_success());
        assert_eq!(report.metrics.memo_hits, 1);
        assert_eq!(report.metrics.memo_misses, 2);
        assert_eq!(report.metrics.memo_entries, 2);
    }

    #[test]
    fn packrat_memo_keys_inherited_rules_by_authoring_scope() {
        // The bare `item` lookup through child and the explicit
        // `super.item` lookup both resolve to the same parent-authored
        // rule at byte 0. They should share one memo entry.
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule item = [a]\nend\n",
        ));
        let child = CompiledGrammar::compile_with_parent(
            parse_grammar_source("grammar child\nrule root = item `x` | super.item `y`\nend\n"),
            &parent,
        );
        let report = inspect_region_compiled(&child, "root", "ay", 0);

        assert!(report.is_success());
        assert_eq!(report.metrics.memo_hits, 1);
        assert_eq!(report.metrics.memo_misses, 2);
        assert_eq!(report.metrics.memo_entries, 2);
    }

    #[test]
    fn packrat_memo_reuses_parametric_application() {
        // Both alternatives invoke the same `list-of<digit, `,`>` apply
        // at byte 0. The first alternative then fails on `x`; the second
        // should reuse the whole parametric application before accepting
        // `y`, not re-run the list structure and its inner digit rules.
        let rules = parse_grammar_source(
            "grammar tiny\n\
             rule root = list-of<digit, `,`> `x` | list-of<digit, `,`> `y`\n\
             rule list-of<elem, sep> = elem (sep elem)*\n\
             rule digit = [0-9]\n\
             end\n",
        );
        let report = inspect_region(&rules, "root", "1,2y", 0);

        assert!(report.is_success());
        assert_eq!(report.metrics.memo_hits, 1);
        assert_eq!(report.metrics.memo_misses, 4);
        assert_eq!(report.metrics.memo_entries, 4);
    }

    #[test]
    fn owned_rule_node_round_trips_from_borrowed_tree() {
        // Build a borrowed parse tree, convert to the owned form,
        // and confirm the structure and texts survive intact.
        let grammar = "grammar tiny\nrule pair = name `:` type-name\nrule name = [A-Za-z]+ \nrule type-name = `<` name `>`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse =
            interpret_region(&rules, "pair", "radius: <integer>", 0).expect("pair should parse");
        let owned = parse.root.to_owned_tree();

        assert_eq!(owned.name(), Some("pair"));
        assert_eq!(owned.span, Span::new(0, 17));
        assert_eq!(owned.text, "radius: <integer>");
        assert!(owned.walk().any(|node| node.is_rule("type-name")));
    }

    #[test]
    fn owned_rule_node_rule_at_offset_returns_deepest_named_rule() {
        // `radius: <integer>` — at the `r` (offset 0) the deepest
        // named rule is `name`. At the `i` inside `<integer>`
        // (offset 9) the deepest named rule is the inner `name`
        // child of `type-name`. At the `:` (offset 6) no named
        // rule contains the offset; the only thing covering it is
        // a literal child, so rule_at_offset returns the enclosing
        // `pair` rule.
        let grammar = "grammar tiny\nrule pair = name `:` type-name\nrule name = [A-Za-z]+ \nrule type-name = `<` name `>`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse =
            interpret_region(&rules, "pair", "radius: <integer>", 0).expect("pair should parse");
        let owned = parse.root.to_owned_tree();

        let at_radius = owned.rule_at_offset(0).expect("rule at radius");
        assert_eq!(at_radius.name(), Some("name"));
        assert_eq!(at_radius.text, "radius");

        let at_integer = owned.rule_at_offset(9).expect("rule at integer");
        // The inner `name` inside `type-name` is the deepest named
        // rule at this offset.
        assert_eq!(at_integer.name(), Some("name"));
        assert_eq!(at_integer.text, "integer");

        let at_colon = owned.rule_at_offset(6).expect("pair encloses colon");
        assert_eq!(at_colon.name(), Some("pair"));
    }

    #[test]
    fn owned_rule_node_path_and_enclosing_span_helpers_match_borrowed() {
        // Sanity-check that the owned-form path_to_offset and
        // smallest_covering_span agree with the borrowed-form
        // versions on the same source. Mirrors the borrowed tests
        // so future changes to either side stay in lockstep.
        let grammar = "grammar tiny\nrule pair = name `:` type-name\nrule name = [A-Za-z]+ \nrule type-name = `<` name `>`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse =
            interpret_region(&rules, "pair", "radius: <integer>", 0).expect("pair should parse");
        let owned = parse.root.to_owned_tree();

        let borrowed_path = parse.root.path_to_offset(9);
        let owned_path = owned.path_to_offset(9);
        assert_eq!(borrowed_path.len(), owned_path.len());
        for (borrowed_node, owned_node) in borrowed_path.iter().zip(owned_path.iter()) {
            assert_eq!(borrowed_node.name(), owned_node.name());
            assert_eq!(borrowed_node.span, owned_node.span);
        }

        let span = Span::new(9, 16);
        let borrowed_cover = parse
            .root
            .smallest_covering_span(span)
            .expect("borrowed covers");
        let owned_cover = owned.smallest_covering_span(span).expect("owned covers");
        assert_eq!(borrowed_cover.name(), owned_cover.name());
        assert_eq!(borrowed_cover.span, owned_cover.span);
    }

    #[test]
    fn parses_and_interprets_tiny_grammar() {
        let grammar = "grammar tiny\nrule pair = name `:` type-name\nrule name = [A-Za-z]+ \nrule type-name = `<` name `>`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse =
            interpret_region(&rules, "pair", "radius: <integer>", 100).expect("pair should parse");

        assert_eq!(parse.root.name(), Some("pair"));
        assert_eq!(parse.root.span, Span::new(100, 117));
        assert!(
            parse
                .root
                .children
                .iter()
                .any(|child| child.is_rule("name"))
        );
    }

    #[test]
    fn exposes_node_roles_and_queries() {
        let grammar = "grammar tiny\nrule pair = name `:` type-name\nrule name = [A-Za-z_]+ \nrule type-name = `<` name `>`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse =
            interpret_region(&rules, "pair", "radius: <integer>", 100).expect("pair should parse");

        assert_eq!(parse.root.role(), RuleNodeRole::NamedRule);
        assert!(parse.root.is_named());

        let colon = parse
            .root
            .find_by_role(RuleNodeRole::Literal)
            .into_iter()
            .find(|node| node.literal() == Some(":"))
            .expect("literal colon node");
        assert_eq!(colon.text, ":");
        assert!(colon.is_anonymous());

        let class_nodes = parse.root.find_by_role(RuleNodeRole::CharacterClass);
        assert!(
            class_nodes
                .iter()
                .any(|node| node.character_class() == Some("A-Za-z_"))
        );

        let names = parse.root.find_rules("name");
        assert_eq!(names.len(), 2);
        assert_eq!(names[0].text, "radius");
        assert_eq!(names[1].text, "integer");

        let covering = parse
            .root
            .smallest_covering_span(Span::new(109, 116))
            .expect("covering type name");
        assert_eq!(covering.name(), Some("name"));
        assert_eq!(covering.text, "integer");

        let walked = parse
            .root
            .walk()
            .map(|node| alloc::format!("{}", node.kind.label()))
            .collect::<Vec<_>>();
        assert_eq!(walked[0], "pair");
        assert!(walked.iter().any(|item| item == "type-name"));

        let all_counts = rule_counts(&parse.root);
        assert!(
            all_counts
                .iter()
                .any(|count| count.node_kind == RuleNodeKind::Literal { value: ":" })
        );
        let named_counts = named_rule_counts(&parse.root);
        assert!(named_counts.iter().all(|count| count.rule_name().is_some()));
        assert!(
            named_counts
                .iter()
                .any(|count| count.rule_name() == Some("pair"))
        );
    }

    #[test]
    fn reports_expected_fragments() {
        let grammar = "grammar digits\nrule digits = [0-9]+\nend\n";
        let rules = parse_grammar_source(grammar);
        let error = interpret_region(&rules, "digits", "", 30).expect_err("digits should fail");

        assert_eq!(error.span, Span::new(30, 30));
        assert_eq!(
            error.expected,
            vec![ExpectedFragment::CharacterClass("0-9")]
        );
    }

    #[test]
    fn reports_unexpected_fragments_for_literal_failures() {
        let grammar = "grammar declarations\n\
                       rule declaration = `define` (`class` | `generic`)\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let error =
            interpret_region(&rules, "declaration", "define classs", 50).expect_err("bad keyword");

        assert_eq!(error.span, Span::new(57, 63));
        assert_eq!(
            error.expected,
            vec![
                ExpectedFragment::Literal("class"),
                ExpectedFragment::Literal("generic")
            ]
        );
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
                .map(|child| (child.name().unwrap_or(""), child.text))
                .collect::<Vec<_>>(),
            vec![("item", "a!"), ("item", "b")]
        );
    }

    #[test]
    fn reports_trailing_input_as_region_error() {
        let grammar = "grammar one\nrule root = `ok`\nend\n";
        let rules = parse_grammar_source(grammar);
        let error = interpret_region(&rules, "root", "ok nope", 5).expect_err("trailing input");

        assert_eq!(error.span, Span::new(8, 12));
        assert_eq!(error.expected, vec![ExpectedFragment::EndOfRegion]);
    }

    #[test]
    fn reports_partial_root_for_trailing_input() {
        let grammar = "grammar one\nrule root = `ok`\nend\n";
        let rules = parse_grammar_source(grammar);
        let report = inspect_region(&rules, "root", "ok nope", 5);

        assert!(!report.is_success());
        assert_eq!(report.root.as_ref().map(|node| node.text), Some("ok"));
        assert_eq!(
            report.error.as_ref().map(|error| error.span),
            Some(Span::new(8, 12))
        );
        assert_eq!(
            report.error.as_ref().map(|error| error.expected.as_slice()),
            Some(&[ExpectedFragment::EndOfRegion][..])
        );

        let root_failure = inspect_region(&rules, "root", "nope", 5);
        assert!(root_failure.root.is_none());
        assert_eq!(
            root_failure
                .error
                .as_ref()
                .map(|error| error.expected.as_slice()),
            Some(&[ExpectedFragment::Literal("ok")][..])
        );
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
                GrammarActionArg::new("name", GrammarActionTerm::Capture { rule: "name" }),
                GrammarActionArg::new("items", GrammarActionTerm::CaptureAll { rule: "item" }),
                GrammarActionArg::new("required", GrammarActionTerm::Bool(true)),
                GrammarActionArg::new("kind", GrammarActionTerm::Literal("thing")),
            ]
        );
    }

    #[test]
    fn ordinary_rule_cycles_fail_at_the_original_source_position() {
        for grammar in [
            "grammar cycle\nrule root = root\nend\n",
            "grammar cycle\nrule root = other\nrule other = root\nend\n",
        ] {
            let rules = parse_grammar_source(grammar);
            let error = interpret_region(&rules, "root", "x", 37).expect_err("cyclic path");
            assert_eq!(error.span, Span::new(37, 38));
            assert!(error.expected.contains(&ExpectedFragment::Rule("root")));
        }
    }

    #[test]
    fn cyclic_choice_path_leaves_a_noncyclic_alternative_available() {
        let grammar = "grammar cycle\nrule root = first | `ok`\nrule first = second\nrule second = first\nend\n";
        let rules = parse_grammar_source(grammar);
        let parsed = interpret_region(&rules, "root", "ok", 37).expect("fallback alternative");
        assert_eq!(parsed.root.text, "ok");
        assert_eq!(parsed.root.span, Span::new(37, 39));
    }

    #[test]
    fn left_recursive_rule_matches_left_associatively() {
        // Classic left-recursive expression grammar.
        let grammar = "grammar tiny\n\
                       rule expr = expr `+` term | term\n\
                       rule term = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "expr", "1+2+3", 0).expect("expr parse");
        // The root represents the outermost `expr`. Left-associative: the
        // first child is itself an expr that matched `1+2`; the last two
        // tokens are the `+` literal and the `3` term.
        assert_eq!(parse.root.text, "1+2+3");
        let inner_expr = parse
            .root
            .children
            .iter()
            .find(|child| child.is_rule("expr"))
            .expect("nested expr");
        assert_eq!(inner_expr.text, "1+2");
    }

    #[test]
    fn left_recursive_rule_falls_through_when_seed_fails() {
        let grammar = "grammar tiny\n\
                       rule expr = expr `+` term | term\n\
                       rule term = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        // No digit at all → seed (term) fails; expr fails.
        let error = interpret_region(&rules, "expr", "+5", 0).expect_err("no seed");
        assert!(
            error
                .expected
                .iter()
                .any(|fragment| matches!(fragment, ExpectedFragment::CharacterClass(_)))
        );
    }

    #[test]
    fn left_recursive_rule_stops_growing_on_tail_failure() {
        let grammar = "grammar tiny\n\
                       rule expr = expr `+` term | term\n\
                       rule term = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        // Seed matches `1`, tail starts `+2` which extends, then `;3`
        // doesn't match `+`, so growth stops at `1+2`.
        let parse = interpret_region(&rules, "expr", "1+2", 0).expect("partial parse");
        assert_eq!(parse.root.text, "1+2");
    }

    #[test]
    fn left_recursive_rule_with_bare_self_alternative_does_not_loop() {
        // `expr = expr | base` is a degenerate direct-LR shape: the LR
        // alternative is a bare self-reference (no Sequence wrapper).
        // The seed-and-grow path must treat it as a no-tail LR
        // alternative so the matcher exits with the base seed rather
        // than recursing into itself forever.
        let grammar = "grammar tiny\nrule expr = expr | `a`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "expr", "a", 0).expect("base seed");
        assert_eq!(parse.root.text, "a");
    }

    #[test]
    fn left_recursive_rule_with_named_self_reference_matches_and_binds() {
        // The leading self-reference carries a `lhs:` binding. The LR
        // detector must peel the Named wrapper, treat the alternative
        // as LR, and tag the previous match with `lhs` when growing.
        let grammar = "grammar tiny\n\
                       rule expr = lhs:expr `+` term | term\n\
                       rule term = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "expr", "1+2+3", 0).expect("expr parse");
        assert_eq!(parse.root.text, "1+2+3");
        let lhs = parse
            .root
            .find_by_binding("lhs")
            .expect("lhs binding on inner expr");
        // The bound child is itself an `expr` rule that matched `1+2`.
        assert_eq!(lhs.name(), Some("expr"));
        assert_eq!(lhs.text, "1+2");
    }

    #[test]
    fn left_recursive_rule_with_named_self_reference_does_not_loop_on_seed_failure() {
        // Even when the seed (term) can't match, the matcher must fail
        // cleanly instead of recursing into the Named-wrapped self
        // reference indefinitely.
        let grammar = "grammar tiny\n\
                       rule expr = lhs:expr `+` term | term\n\
                       rule term = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let error = interpret_region(&rules, "expr", "+5", 0).expect_err("no seed");
        assert!(
            error
                .expected
                .iter()
                .any(|fragment| matches!(fragment, ExpectedFragment::CharacterClass(_)))
        );
    }

    #[test]
    fn grammar_source_header_captures_name_and_parent() {
        let parsed =
            parse_grammar_source_full("grammar derived extends base-loko\nrule x = `a`\nend\n");
        assert_eq!(parsed.header.name, Some("derived"));
        assert_eq!(parsed.header.extends, Some("base-loko"));
        assert_eq!(parsed.rules.len(), 1);
        assert!(parsed.diagnostics.is_empty());
    }

    #[test]
    fn grammar_source_header_without_extends() {
        let parsed = parse_grammar_source_full("grammar tiny\nrule x = `a`\nend\n");
        assert_eq!(parsed.header.name, Some("tiny"));
        assert_eq!(parsed.header.extends, None);
    }

    #[test]
    fn compiled_grammar_with_parent_inherits_lookups() {
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule digit = [0-9]\nrule shared = `parent`\nend\n",
        ));
        let child_rules = parse_grammar_source("grammar child\nrule shared = `child`\nend\n");
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        // Child override wins for `shared`.
        let shared = child.rule("shared").expect("shared in child");
        let parse = interpret_region_compiled(&child, "shared", "child", 0).expect("child");
        assert_eq!(parse.root.text, "child");
        assert_eq!(shared.name, "shared");

        // Inherited rule resolves through the chain.
        let digit = child.rule("digit").expect("digit inherited");
        assert_eq!(digit.name, "digit");
        let parse = interpret_region_compiled(&child, "digit", "5", 0).expect("digit");
        assert_eq!(parse.root.text, "5");
    }

    #[test]
    fn compiled_grammar_with_parent_chains_through_ancestor_references() {
        // Parent defines `name`, child defines a rule that references `name`.
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule name = [A-Za-z]+\nend\n",
        ));
        let child_rules = parse_grammar_source("grammar child\nrule greeting = `hi` name\nend\n");
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        let parse =
            interpret_region_compiled(&child, "greeting", "hi Bob", 0).expect("greeting parse");
        assert_eq!(parse.root.text, "hi Bob");
    }

    #[test]
    fn compiled_grammar_validate_parametric_arity_catches_inherited_mismatch() {
        // Parent declares the parametric rule; child mis-invokes it.
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule list-of<elem, sep> = elem (sep elem)*\nend\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child\nrule digits = list-of<digit>\nrule digit = [0-9]\nend\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        // Source-time validation didn't see list-of (it was inherited);
        // the compile-time pass picks up the mismatch.
        let diagnostics = child.validate_parametric_arity();
        let mismatch = diagnostics
            .iter()
            .find(|d| d.code() == GrammarSourceDiagnosticCode::ParametricRuleArityMismatch)
            .expect("inherited arity diagnostic");
        // Span points at the offending rule in the child.
        let digits = child.rule("digits").expect("digits rule");
        assert_eq!(mismatch.span(), digits.source_span);
    }

    #[test]
    fn compiled_grammar_validate_parametric_arity_is_quiet_when_well_formed() {
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule list-of<elem, sep> = elem (sep elem)*\nend\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child\nrule digits = list-of<digit, `,`>\nrule digit = [0-9]\nend\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);
        assert!(child.validate_parametric_arity().is_empty());
    }

    #[test]
    fn super_rule_dispatches_to_parent_definition() {
        // Child overrides `greeting` and uses super to splice in the
        // parent's previous definition before adding its own suffix.
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule greeting = `hello`\nend\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child extends parent\nrule greeting = super.greeting `world`\nend\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);
        let parse = interpret_region_compiled(&child, "greeting", "hello world", 0)
            .expect("super splices in parent definition");
        assert_eq!(parse.root.text, "hello world");
    }

    #[test]
    fn super_rule_runs_parent_body_with_parents_lookup() {
        // `super.outer` runs the parent's `outer` rule with the
        // parent's own grammar in scope. The parent's body references
        // `inner`; that `Rule("inner")` resolves to the parent's
        // inner, NOT to the child's override. The child's override
        // only applies to references inside the child's own bodies.
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule outer = inner\nrule inner = `1`\nend\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child extends parent\n\
             rule outer = super.outer `2`\n\
             rule inner = `9`\n\
             end\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);
        let parse = interpret_region_compiled(&child, "outer", "12", 0)
            .expect("super.outer matches parent's inner, not child's override");
        assert_eq!(parse.root.text, "12");
        let child_inner_parse = interpret_region_compiled(&child, "inner", "9", 0)
            .expect("child's own inner still uses the override");
        assert_eq!(child_inner_parse.root.text, "9");
    }

    #[test]
    fn super_rule_without_parent_fails_to_match() {
        let grammar = CompiledGrammar::compile(parse_grammar_source(
            "grammar lonely\nrule greeting = super.greeting\nend\n",
        ));
        let result = interpret_region_compiled(&grammar, "greeting", "hello", 0);
        assert!(
            result.is_err(),
            "super reference against a parent-less grammar must fail to match",
        );
    }

    #[test]
    fn super_rule_inside_inherited_rule_targets_the_rules_own_parent() {
        // grandparent: item = "g"
        // parent extends grandparent:
        //     item = "p"
        //     wrapper = super.item    // refers to parent's super -> grandparent.item
        // child extends parent (no overrides).
        //
        // When child invokes `wrapper`, the rule body it runs was
        // authored in `parent`. Its `super.item` must therefore reach
        // grandparent.item ("g"), not parent.item ("p"). A naive
        // implementation that used `child.parent` for super would
        // incorrectly resolve to parent.item.
        let grandparent = CompiledGrammar::compile(parse_grammar_source(
            "grammar grand\nrule item = `g`\nend\n",
        ));
        let parent_rules = parse_grammar_source(
            "grammar parent extends grand\n\
             rule item = `p`\n\
             rule wrapper = super.item\n\
             end\n",
        );
        let parent = CompiledGrammar::compile_with_parent(parent_rules, &grandparent);
        let child_rules = parse_grammar_source("grammar child extends parent\nend\n");
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        let wrapper_match = interpret_region_compiled(&child, "wrapper", "g", 0)
            .expect("inherited wrapper resolves super through its authoring parent");
        assert_eq!(wrapper_match.root.text, "g");

        // Sanity: parent itself sees the same behavior.
        let parent_wrapper_match = interpret_region_compiled(&parent, "wrapper", "g", 0)
            .expect("parent's own wrapper still matches");
        assert_eq!(parent_wrapper_match.root.text, "g");

        // Child invoking `item` directly resolves to parent's item
        // (the closest definition in the chain), not grandparent's.
        let item_match = interpret_region_compiled(&child, "item", "p", 0)
            .expect("inherited item resolves to parent's effective definition");
        assert_eq!(item_match.root.text, "p");
    }

    #[test]
    fn super_rule_inside_inherited_parametric_rule_targets_authoring_parent() {
        // Same shape as super_rule_inside_inherited_rule_targets_the_rules_own_parent,
        // but the inherited rule is *parametric* and reached via
        // Apply rather than via a plain Rule reference. Without
        // owner-aware routing in the Apply arm, super.item inside
        // wrapper<x>'s substituted body would resolve through the
        // child's parent (= parent) and incorrectly reach parent.item.
        let grandparent = CompiledGrammar::compile(parse_grammar_source(
            "grammar grand\nrule item = `g`\nend\n",
        ));
        let parent_rules = parse_grammar_source(
            "grammar parent extends grand\n\
             rule item = `p`\n\
             rule wrapper<x> = x super.item\n\
             end\n",
        );
        let parent = CompiledGrammar::compile_with_parent(parent_rules, &grandparent);
        let child_rules =
            parse_grammar_source("grammar child extends parent\nrule root = wrapper<`!`>\nend\n");
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        let parse = interpret_region_compiled(&child, "root", "!g", 0)
            .expect("inherited parametric wrapper resolves super through authoring parent");
        assert_eq!(parse.root.text, "!g");
    }

    #[test]
    fn super_rule_inherited_parametric_inner_ref_uses_parent_scope() {
        // Parent defines a parametric wrapper that calls an inner
        // rule by name. Child inherits the parametric wrapper and
        // overrides the inner rule with a different body. The
        // inherited wrapper's `Rule("inner")` reference must resolve
        // in the parent's scope (where the wrapper was authored), so
        // the child's override does NOT apply when the wrapper is
        // reached through inheritance — matching the verbatim
        // semantics used for ordinary inherited bodies.
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\n\
             rule inner = `1`\n\
             rule wrapper<x> = x inner\n\
             end\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child extends parent\n\
             rule inner = `9`\n\
             rule root = wrapper<`!`>\n\
             end\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        let parse = interpret_region_compiled(&child, "root", "!1", 0)
            .expect("inherited parametric wrapper resolves inner in parent scope");
        assert_eq!(parse.root.text, "!1");

        // Sanity: child invoking inner directly still hits the override.
        let direct = interpret_region_compiled(&child, "inner", "9", 0)
            .expect("child's own inner still uses override");
        assert_eq!(direct.root.text, "9");
    }

    #[test]
    fn super_rule_chained_inherited_parametrics_pop_caller_frames() {
        // Grandparent owns a base parametric `pair<x, y>`.
        // Parent inherits it and defines a wrapper parametric
        // `outer<x, y>` whose body is itself an Apply of `pair<x, y>`.
        // Child inherits both parametrics and invokes
        // `outer<digit, `,`>`. The full binding-frame stack:
        //
        //   child Apply outer<digit, `,`>
        //     -> outer is inherited from parent; body runs in parent
        //        scope with frame [{x=digit, y=`,`, caller=child}].
        //     -> outer body invokes pair<x, y>.
        //         pair is inherited (originally from grandparent);
        //         body runs in grandparent scope with frame
        //         [{x=Rule(x), y=Rule(y), caller=parent frame}].
        //     -> pair body refers to Rule(x) and Rule(y).
        //         x resolves via inner frame -> arg is Rule(x);
        //         interpret in caller (parent) frame -> outer's x
        //         binding -> arg is Rule(digit); interpret in
        //         caller (child) frame with empty bindings -> the
        //         child-defined `digit` rule matches.
        //
        // The test pins that each parameter ref pops to the right
        // frame across multiple inherited Apply hops.
        let grandparent = CompiledGrammar::compile(parse_grammar_source(
            "grammar grand\nrule pair<x, y> = x y\nend\n",
        ));
        let parent_rules = parse_grammar_source(
            "grammar parent extends grand\nrule outer<x, y> = pair<x, y>\nend\n",
        );
        let parent = CompiledGrammar::compile_with_parent(parent_rules, &grandparent);
        let child_rules = parse_grammar_source(
            "grammar child extends parent\n\
             rule digit = [0-9]\n\
             rule root = outer<digit, `,`>\n\
             end\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        let parse = interpret_region_compiled(&child, "root", "1,", 0)
            .expect("chained inherited parametrics route bindings through every frame");
        assert_eq!(parse.root.text, "1,");
    }

    #[test]
    fn super_rule_resolves_to_immediate_parent_in_multi_level_chain() {
        // Grandparent: digits = "1"
        // Parent extends grandparent: digits = super.digits "2" -> matches "12"
        // Child extends parent: digits = super.digits "3" -> matches "123"
        // The test pins that each grammar's `super.digits` reaches its
        // immediate parent's effective digits (the one the parent
        // would match if invoked directly), not the first ancestor's
        // direct rule, and not a flattened "current" rule.
        // Digit literals are used so the literal-boundary check
        // doesn't reject adjacent characters as identifier
        // continuations the way alphabetic literals would.
        let grandparent = CompiledGrammar::compile(parse_grammar_source(
            "grammar grand\nrule digits = `1`\nend\n",
        ));
        let parent_rules = parse_grammar_source(
            "grammar parent extends grand\nrule digits = super.digits `2`\nend\n",
        );
        let parent = CompiledGrammar::compile_with_parent(parent_rules, &grandparent);
        let child_rules = parse_grammar_source(
            "grammar child extends parent\nrule digits = super.digits `3`\nend\n",
        );
        let child = CompiledGrammar::compile_with_parent(child_rules, &parent);

        let parent_match = interpret_region_compiled(&parent, "digits", "12", 0)
            .expect("parent's super.digits reaches grandparent's digits");
        assert_eq!(parent_match.root.text, "12");

        let child_match = interpret_region_compiled(&child, "digits", "123", 0)
            .expect("child's super.digits reaches parent's effective digits");
        assert_eq!(child_match.root.text, "123");
    }

    #[test]
    fn super_rule_round_trips_through_display() {
        use alloc::string::ToString as _;
        let parsed = parse_grammar_source("grammar t\nrule r = super.greeting\nend\n");
        let body = &parsed.first().expect("rule r parsed").expression;
        assert!(matches!(body, RuleExpression::Super(name) if *name == "greeting"));
        assert_eq!(body.to_string(), "super.greeting");
    }

    #[test]
    fn try_compile_with_parent_returns_err_on_inherited_arity_mismatch() {
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule list-of<elem, sep> = elem (sep elem)*\nend\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child\nrule digits = list-of<digit>\nrule digit = [0-9]\nend\n",
        );
        let error = CompiledGrammar::try_compile_with_parent(child_rules, &parent)
            .expect_err("strict compile should reject mismatched arity");
        assert!(
            error
                .iter()
                .any(|d| d.code() == GrammarSourceDiagnosticCode::ParametricRuleArityMismatch)
        );
    }

    #[test]
    fn try_compile_with_parent_returns_ok_when_well_formed() {
        let parent = CompiledGrammar::compile(parse_grammar_source(
            "grammar parent\nrule list-of<elem, sep> = elem (sep elem)*\nend\n",
        ));
        let child_rules = parse_grammar_source(
            "grammar child\nrule digits = list-of<digit, `,`>\nrule digit = [0-9]\nend\n",
        );
        let compiled = CompiledGrammar::try_compile_with_parent(child_rules, &parent)
            .expect("well-formed grammar compiles");
        let parse =
            interpret_region_compiled(&compiled, "digits", "1,2,3", 0).expect("digits parse");
        assert_eq!(parse.root.text, "1,2,3");
    }

    #[test]
    fn parametric_rule_definition_records_params() {
        let grammar = "grammar tiny\n\
                       rule list-of<elem, sep> = elem (sep elem)*\n\
                       rule digit = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let list_of = rules
            .iter()
            .find(|rule| rule.name == "list-of")
            .expect("list-of declared");
        assert_eq!(list_of.params, vec!["elem", "sep"]);
    }

    #[test]
    fn parametric_rule_invocation_substitutes_args() {
        let grammar = "grammar tiny\n\
                       rule list-of<elem, sep> = elem (sep elem)*\n\
                       rule digits = list-of<digit, `,`>\n\
                       rule digit = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "digits", "1,2,3", 0).expect("digits parse");
        assert_eq!(parse.root.text, "1,2,3");
        let digits = parse.root.find_rules("digit");
        assert_eq!(digits.len(), 3);
        assert_eq!(digits[0].text, "1");
        assert_eq!(digits[1].text, "2");
        assert_eq!(digits[2].text, "3");
    }

    #[test]
    fn parametric_rule_arity_mismatch_reports_diagnostic() {
        // `list-of` expects two params; invocation supplies one.
        let grammar = "grammar tiny\n\
                       rule list-of<elem, sep> = elem (sep elem)*\n\
                       rule digits = list-of<digit>\n\
                       rule digit = [0-9]\n\
                       end\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        // The rule still parses — diagnostics surface the arity error
        // alongside the rule list so callers can decide whether to fail.
        assert_eq!(rules.len(), 3);
        let arity_diag = diagnostics
            .iter()
            .find(|d| d.code() == GrammarSourceDiagnosticCode::ParametricRuleArityMismatch)
            .expect("arity-mismatch diagnostic");
        // The diagnostic points at the offending rule's source span.
        let digits = rules
            .iter()
            .find(|rule| rule.name == "digits")
            .expect("digits rule");
        assert_eq!(arity_diag.span(), digits.source_span);
    }

    #[test]
    fn parametric_rule_arity_skips_unknown_targets() {
        // Apply to a rule not defined locally — could be inherited from a
        // parent grammar; do not flag here.
        let grammar = "grammar tiny\n\
                       rule digits = list-of<digit, `,`>\n\
                       rule digit = [0-9]\n\
                       end\n";
        let (_rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert!(
            !diagnostics
                .iter()
                .any(|d| d.code() == GrammarSourceDiagnosticCode::ParametricRuleArityMismatch),
            "should not flag unknown parametric targets (may be inherited): {diagnostics:?}"
        );
    }

    #[test]
    fn parametric_rule_display_round_trips() {
        let expr = RuleExpression::apply(
            "list-of",
            vec![RuleExpression::rule("digit"), RuleExpression::literal(",")],
        );
        assert_eq!(alloc::format!("{expr}"), "list-of<digit, `,`>");
    }

    #[test]
    fn parametric_rules_compose_with_named_subexpressions() {
        let grammar = "grammar tiny\n\
                       rule list-of<elem, sep> = elem (sep elem)*\n\
                       rule pair = lhs:digit `:` rhs:list-of<digit, `,`>\n\
                       rule digit = [0-9]\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "pair", "1:2,3,4", 0).expect("pair parse");
        // The lhs binding tags the first digit; the rhs binding tags
        // each digit that comes from inside list-of.
        let lhs = parse.root.find_by_binding("lhs").expect("lhs digit");
        assert_eq!(lhs.text, "1");
        let rhs_count = parse
            .root
            .children
            .iter()
            .filter(|child| child.binding_name == Some("rhs"))
            .count();
        assert!(
            rhs_count >= 1,
            "rhs binding should tag at least one child, got {rhs_count}"
        );
    }

    #[test]
    fn parses_named_subexpression_binds_children() {
        // Two `name` children share a rule; the binding picks the second.
        let grammar = "grammar tiny\n\
                       rule pair = name `::` rhs:name\n\
                       rule name = [A-Za-z]+\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "pair", "first::second", 0).expect("pair parse");
        let root = parse.root;

        let rhs = root.find_by_binding("rhs").expect("rhs binding present");
        assert_eq!(rhs.text, "second");

        let names = root.find_rules("name");
        assert_eq!(names.len(), 2);
        assert_eq!(names[0].text, "first");
        assert_eq!(names[1].text, "second");
        // Only the second name carries the binding.
        assert_eq!(names[0].binding_name, None);
        assert_eq!(names[1].binding_name, Some("rhs"));
    }

    #[test]
    fn action_capture_resolves_named_binding_when_in_scope() {
        let grammar = "grammar tiny\n\
                       rule binding = lhs:name `::` rhs:name => emit-binding(lhs: lhs, rhs: rhs, fallback: name)\n\
                       rule name = [A-Za-z]+\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let action = rules
            .iter()
            .find(|rule| rule.name == "binding")
            .and_then(|rule| rule.action.as_ref())
            .expect("binding action");
        // `lhs` and `rhs` are in scope → CaptureByName.
        // `name` is the rule (not a binding) → falls back to Capture.
        assert_eq!(
            action.args,
            vec![
                GrammarActionArg::new("lhs", GrammarActionTerm::CaptureByName { name: "lhs" }),
                GrammarActionArg::new("rhs", GrammarActionTerm::CaptureByName { name: "rhs" }),
                GrammarActionArg::new("fallback", GrammarActionTerm::Capture { rule: "name" }),
            ]
        );
    }

    #[test]
    fn named_subexpression_display_round_trips() {
        let expr = RuleExpression::sequence(vec![
            RuleExpression::named("lhs", RuleExpression::rule("name")),
            RuleExpression::literal("::"),
            RuleExpression::named("rhs", RuleExpression::rule("name")),
        ]);
        assert_eq!(alloc::format!("{expr}"), "lhs:name `::` rhs:name");
    }

    #[test]
    fn parses_integer_action_terms() {
        let action = parse_grammar_action("emit-x(count: 42, offset: -1)")
            .expect("action with integer terms");
        assert_eq!(
            action.args,
            vec![
                GrammarActionArg::new("count", GrammarActionTerm::Integer(42)),
                GrammarActionArg::new("offset", GrammarActionTerm::Integer(-1)),
            ]
        );
    }

    #[test]
    fn parses_list_action_terms() {
        let action =
            parse_grammar_action("emit-x(items: [name, item*, 7])").expect("action with list term");
        assert_eq!(
            action.args,
            vec![GrammarActionArg::new(
                "items",
                GrammarActionTerm::List(vec![
                    GrammarActionTerm::Capture { rule: "name" },
                    GrammarActionTerm::CaptureAll { rule: "item" },
                    GrammarActionTerm::Integer(7),
                ])
            )]
        );
    }

    #[test]
    fn parses_record_action_terms() {
        let action = parse_grammar_action("emit-x(meta: { kind: \"slot\", count: 3 })")
            .expect("action with record term");
        let expected = GrammarActionTerm::Record(vec![
            GrammarActionArg::new("kind", GrammarActionTerm::Literal("slot")),
            GrammarActionArg::new("count", GrammarActionTerm::Integer(3)),
        ]);
        assert_eq!(action.args, vec![GrammarActionArg::new("meta", expected)]);
    }

    #[test]
    fn parses_nested_action_call_terms() {
        let action = parse_grammar_action("emit-x(body: emit-y(value: name))")
            .expect("action with nested call");
        let nested = GrammarAction::new(
            "emit-y",
            vec![GrammarActionArg::new(
                "value",
                GrammarActionTerm::Capture { rule: "name" },
            )],
        );
        assert_eq!(
            action.args,
            vec![GrammarActionArg::new(
                "body",
                GrammarActionTerm::Call(Box::new(nested))
            )]
        );
    }

    #[test]
    fn action_terms_round_trip_through_display() {
        let action = GrammarAction::new(
            "emit-form",
            vec![
                GrammarActionArg::new("count", GrammarActionTerm::Integer(42)),
                GrammarActionArg::new(
                    "items",
                    GrammarActionTerm::List(vec![
                        GrammarActionTerm::Capture { rule: "name" },
                        GrammarActionTerm::Integer(7),
                    ]),
                ),
                GrammarActionArg::new(
                    "meta",
                    GrammarActionTerm::Record(vec![GrammarActionArg::new(
                        "kind",
                        GrammarActionTerm::Literal("slot"),
                    )]),
                ),
                GrammarActionArg::new(
                    "body",
                    GrammarActionTerm::Call(Box::new(GrammarAction::new(
                        "emit-y",
                        vec![GrammarActionArg::new(
                            "value",
                            GrammarActionTerm::Capture { rule: "name" },
                        )],
                    ))),
                ),
            ],
        );
        let rendered = alloc::format!("{action}");
        let reparsed = parse_grammar_action(&rendered).expect("reparses");
        assert_eq!(action, reparsed);
    }

    #[test]
    fn caller_owned_excluded_text_rejects_matched_rules() {
        let grammar = "grammar names\nrule name = [A-Za-z]+\nend\n";
        let rules = parse_grammar_source(grammar)
            .into_iter()
            .map(|rule| rule.with_excluded_text(vec!["define"]))
            .collect::<Vec<_>>();

        let error = interpret_region(&rules, "name", "define", 20).expect_err("excluded word");
        assert_eq!(error.span, Span::new(20, 26));
        assert_eq!(error.expected, vec![ExpectedFragment::Rule("name")]);

        let parse = interpret_region(&rules, "name", "circle", 30).expect("non-excluded name");
        assert_eq!(parse.root.text, "circle");
    }

    #[test]
    fn parse_grammar_source_with_diagnostics_is_empty_on_clean_grammar() {
        let grammar = "grammar tiny\nrule pair = name `:` name\nrule name = [A-Za-z]+\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(rules.len(), 2);
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn parse_grammar_source_preserves_rule_annotations() {
        let grammar = "\
grammar metadata
rule field @role=record-field @recover raw = name `:` type-name
rule name = [A-Za-z]+
rule type-name = `<` name `>`
end
";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert!(diagnostics.is_empty());
        let field = rules
            .iter()
            .find(|rule| rule.name == "field")
            .expect("field rule");

        assert_eq!(field.trivia, TriviaMode::Raw);
        assert_eq!(
            field.annotations,
            vec![
                RuleAnnotation::new("role", Some("record-field")),
                RuleAnnotation::flag("recover"),
            ]
        );
        assert_eq!(field.annotation_value("role"), Some("record-field"));
        assert!(field.annotation("recover").is_some());
        assert_eq!(
            field
                .annotations_named("role")
                .map(|annotation| annotation.value())
                .collect::<Vec<_>>(),
            vec![Some("record-field")]
        );
    }

    #[test]
    fn parse_grammar_source_reports_missing_equals() {
        let grammar = "grammar broken\nrule bad-rule no equals here\nrule good = `ok`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "good");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::MissingEquals
        );
    }

    #[test]
    fn parse_grammar_source_reports_missing_rule_keyword() {
        let grammar = "grammar broken\nnot-a-rule = `a`\nrule good = `ok`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "good");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::MissingRuleKeyword
        );
    }

    #[test]
    fn parse_grammar_source_reports_empty_name_and_body() {
        let grammar = "grammar oops\nrule  = `a`\nrule x =\nend\n";
        let (_rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        let codes = diagnostics
            .iter()
            .map(GrammarSourceDiagnostic::code)
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            vec![
                GrammarSourceDiagnosticCode::EmptyName,
                GrammarSourceDiagnosticCode::EmptyBody,
            ]
        );
    }

    #[test]
    fn parse_grammar_source_reports_duplicate_rule_names() {
        let grammar = "grammar dup\nrule x = `a`\nrule x = `b`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(rules.len(), 1);
        let parse = interpret_region(&rules, "x", "a", 0).expect("first rule wins");
        assert_eq!(parse.root.text, "a");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::DuplicateName
        );
    }

    #[test]
    fn parse_grammar_source_reports_bad_rule_annotation_modifier() {
        let grammar = "grammar bad\nrule x @role= = `a`\nrule y = `b`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "y");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::UnknownRuleModifier
        );
    }

    #[test]
    fn parse_grammar_source_does_not_use_annotation_equals_as_rule_separator() {
        let grammar = "grammar bad\nrule x @role=definition\nrule y = `b`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "y");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::MissingEquals
        );
    }

    #[test]
    fn parse_grammar_source_legacy_shim_discards_diagnostics() {
        let grammar = "grammar broken\nrule x = `a`\nrule x = `b`\nend\n";
        let rules = parse_grammar_source(grammar);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "x");
    }

    #[test]
    fn parse_grammar_source_accepts_equals_in_literal_body() {
        let grammar = "grammar ops\nrule equals = `=`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert!(diagnostics.is_empty());
        let parse = interpret_region(&rules, "equals", "=", 0).expect("equals literal");
        assert_eq!(parse.root.text, "=");
    }

    #[test]
    fn parse_grammar_source_preserves_full_span_without_trailing_newline() {
        let grammar = "rule bad with no equals";
        let (_rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::MissingEquals
        );
        // The span must cover the full trimmed line, not drop the last byte.
        assert_eq!(diagnostics[0].span(), Span::new(0, grammar.len()));
    }

    #[test]
    fn parse_grammar_source_action_survives_literal_arrow_in_body() {
        // The rule body contains a literal `=>` token *before* the real
        // transform separator. A naive split on the first `=>` would split
        // inside the literal; we must derive the action from the parsed
        // Transform expression instead.
        let grammar = "grammar tiny\n\
                       rule arrow = `=>` => emit-arrow(value: name)\n\
                       rule name = [A-Za-z]+\n\
                       end\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        assert!(diagnostics.is_empty());
        let arrow_rule = rules
            .iter()
            .find(|rule| rule.name == "arrow")
            .expect("arrow rule");
        let action = arrow_rule.action.as_ref().expect("emit-arrow action");
        assert_eq!(action.name, "emit-arrow");
        assert_eq!(
            action.args,
            vec![GrammarActionArg::new(
                "value",
                GrammarActionTerm::Capture { rule: "name" }
            )]
        );
    }

    #[test]
    fn raw_rule_does_not_skip_internal_whitespace() {
        let grammar = "grammar tiny\nrule pair raw = `(` `)`\nend\n";
        let rules = parse_grammar_source(grammar);
        assert!(matches!(rules[0].trivia, TriviaMode::Raw));
        // No whitespace between `(` and `)`: matches.
        let parse = interpret_region(&rules, "pair", "()", 0).expect("()");
        assert_eq!(parse.root.text, "()");
        // Whitespace between: raw rule's Literal does not skip trivia, so the
        // inner `)` literal fails.
        interpret_region(&rules, "pair", "( )", 0).expect_err("space rejected");
    }

    #[test]
    fn non_raw_rule_skips_internal_whitespace() {
        let grammar = "grammar tiny\nrule pair = `(` `)`\nend\n";
        let rules = parse_grammar_source(grammar);
        assert!(matches!(rules[0].trivia, TriviaMode::Default));
        // Default trivia mode skips whitespace before each literal.
        let parse = interpret_region(&rules, "pair", "( )", 0).expect("space accepted");
        assert_eq!(parse.root.text, "( )");
    }

    #[test]
    fn raw_rule_matches_strings_with_until_delimiter() {
        let grammar = "grammar tiny\n\
                       rule string-literal raw = `\"` until(`\"`) `\"`\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "string-literal", "\"hello world\"", 0)
            .expect("string literal");
        assert_eq!(parse.root.text, "\"hello world\"");
    }

    #[test]
    fn raw_rule_modifier_unknown_modifier_diagnostic() {
        let grammar = "grammar oops\nrule foo bogus = `a`\nrule good = `b`\nend\n";
        let (rules, diagnostics) = parse_grammar_source_with_diagnostics(grammar);
        // Rules with an unknown modifier are skipped entirely so that no
        // garbage rule named "foo bogus" leaks into lookup.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "good");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code(),
            GrammarSourceDiagnosticCode::UnknownRuleModifier
        );
    }

    #[test]
    fn raw_root_rejects_trailing_region_whitespace() {
        let grammar = "grammar tiny\nrule pair raw = `(` `)`\nend\n";
        let rules = parse_grammar_source(grammar);
        // Raw root rules do not let trailing whitespace slide; the trailing
        // spaces are unconsumed input and must produce an EndOfRegion error.
        let error = interpret_region(&rules, "pair", "()   ", 0).expect_err("trailing whitespace");
        assert_eq!(
            error.expected,
            vec![ExpectedFragment::EndOfRegion],
            "expected EndOfRegion, got {:?}",
            error.expected
        );
        assert_eq!(error.span.start, 2, "should point at the trailing space");
    }

    #[test]
    fn raw_rule_failure_span_points_at_internal_whitespace() {
        let grammar = "grammar tiny\nrule pair raw = `(` `)`\nend\n";
        let rules = parse_grammar_source(grammar);
        // Inside a raw rule, the missing-literal failure should land on the
        // unexpected whitespace itself, not on a non-whitespace character past
        // any trivia skip.
        let error = interpret_region(&rules, "pair", "( )", 0).expect_err("interior space");
        assert_eq!(error.span.start, 1, "should point at the space");
        assert_eq!(error.span.end, 2);
    }

    #[test]
    fn inspect_region_raw_root_reports_trailing_trivia() {
        // The raw root contract also applies to the report-oriented API:
        // inspect_region returns the matched root *and* a complete-region
        // failure pointing at the unconsumed trailing whitespace.
        let grammar = "grammar tiny\nrule pair raw = `(` `)`\nend\n";
        let rules = parse_grammar_source(grammar);
        let report = inspect_region(&rules, "pair", "()   ", 0);
        let root = report.root.as_ref().expect("root matched");
        assert_eq!(root.text, "()");
        let error = report.error.as_ref().expect("trailing trivia rejected");
        assert_eq!(error.expected, vec![ExpectedFragment::EndOfRegion]);
        assert_eq!(error.span.start, 2);
    }

    #[test]
    fn cut_prevents_fallthrough_after_match() {
        let grammar = "grammar tiny\nrule x = ^`a` `b` | `c`\nend\n";
        let rules = parse_grammar_source(grammar);
        // No cut hit: the c alternative is still reachable.
        let parse = interpret_region(&rules, "x", "c", 0).expect("c branch");
        assert_eq!(parse.root.text, "c");

        // Cut fires after matching `a`; `b` then fails and the choice does
        // not fall through to `c`. The expected fragment should be `b`, not `c`.
        let error = interpret_region(&rules, "x", "a", 0).expect_err("a without b");
        assert!(
            error
                .expected
                .iter()
                .any(|fragment| matches!(fragment, ExpectedFragment::Literal("b"))),
            "expected `b` in {:?}",
            error.expected,
        );
        assert!(
            !error
                .expected
                .iter()
                .any(|fragment| matches!(fragment, ExpectedFragment::Literal("c"))),
            "should not fall through to `c` in {:?}",
            error.expected,
        );
    }

    #[test]
    fn cut_inside_optional_does_not_bubble() {
        let grammar = "grammar tiny\nrule x = (^`a` `b`)? `c`\nend\n";
        let rules = parse_grammar_source(grammar);
        // `a` matches, cut fires, then `b` fails. The Optional swallows the
        // committed failure, so `c` still matches.
        let parse = interpret_region(&rules, "x", "c", 0).expect("optional skipped");
        assert_eq!(parse.root.text, "c");
    }

    #[test]
    fn cut_inside_one_or_more_does_not_bubble() {
        // A committed failure inside the required first match of a
        // repetition must not abort the enclosing choice. After matching
        // `a` and failing `b`, the outer choice should still fall through
        // to the `a` alternative.
        let grammar = "grammar tiny\nrule x = (^`a` `b`)+ | `a`\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "x", "a", 0).expect("falls through to `a`");
        assert_eq!(parse.root.text, "a");
    }

    #[test]
    fn cut_inside_lookahead_does_not_bubble() {
        // Positive lookahead is non-consuming; a committed failure inside
        // the predicate must not commit the enclosing choice.
        let grammar = "grammar tiny\nrule x = &(^`a` `b`) `c` | `a`\nend\n";
        let rules = parse_grammar_source(grammar);
        // After cut matches `a` and `b` fails inside the lookahead, the
        // committed flag is cleared and the choice falls through to `a`.
        let parse = interpret_region(&rules, "x", "a", 0).expect("falls through to `a`");
        assert_eq!(parse.root.text, "a");
    }

    #[test]
    fn cut_round_trips_in_display() {
        let expr = RuleExpression::cut(RuleExpression::literal("a"));
        assert_eq!(alloc::format!("{expr}"), "^`a`");
    }

    #[test]
    fn positive_lookahead_accepts_without_consuming() {
        let grammar = "grammar tiny\n\
                       rule pair = &name name\n\
                       rule name = [A-Za-z]+\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "pair", "hello", 0).expect("name lookahead");
        assert_eq!(parse.root.text, "hello");
        // The positive lookahead leaves no children of its own; only the
        // captured `name` should remain.
        assert_eq!(
            parse
                .root
                .children
                .iter()
                .filter(|child| child.is_rule("name"))
                .count(),
            1
        );
    }

    #[test]
    fn negative_lookahead_rejects_match_and_reports_fragment() {
        let grammar = "grammar tiny\n\
                       rule item = !`end` name\n\
                       rule name = [A-Za-z]+\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        interpret_region(&rules, "item", "begin", 0).expect("non-end name");
        let error = interpret_region(&rules, "item", "end", 0).expect_err("end rejected");
        assert!(
            error
                .expected
                .iter()
                .any(|fragment| matches!(fragment, ExpectedFragment::NegativeLookahead)),
            "got {:?}",
            error.expected
        );
    }

    #[test]
    fn negative_lookahead_does_not_consume() {
        let grammar = "grammar tiny\n\
                       rule both = !`x` name `!`?\n\
                       rule name = [A-Za-z]+\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "both", "foo!", 0).expect("foo with bang");
        assert_eq!(parse.root.text, "foo!");
    }

    #[test]
    fn lookahead_round_trips_in_display() {
        let expr = RuleExpression::and(RuleExpression::rule("name"));
        assert_eq!(alloc::format!("{expr}"), "&name");
        let expr = RuleExpression::not(RuleExpression::literal("end"));
        assert_eq!(alloc::format!("{expr}"), "!`end`");
        let expr = RuleExpression::not(RuleExpression::choice(vec![
            RuleExpression::literal("a"),
            RuleExpression::literal("b"),
        ]));
        assert_eq!(alloc::format!("{expr}"), "!(`a` | `b`)");
    }

    #[test]
    fn character_class_handles_trailing_dash() {
        assert!(character_class_contains("A-Z?-", '?'));
        assert!(character_class_contains("A-Z?-", '-'));
        assert!(character_class_contains("A-Z?-", 'A'));
        assert!(character_class_contains("A-Z?-", 'Z'));
        assert!(!character_class_contains("A-Z?-", '_'));
        // base_loko style: trailing dash inside larger class.
        assert!(character_class_contains("A-Za-z0-9_?-", '-'));
        assert!(character_class_contains("A-Za-z0-9_?-", '_'));
        assert!(character_class_contains("A-Za-z0-9_?-", 'x'));
        assert!(!character_class_contains("A-Za-z0-9_?-", '$'));
    }

    #[test]
    fn character_class_supports_negation() {
        assert!(!character_class_contains("^a-z", 'a'));
        assert!(!character_class_contains("^a-z", 'm'));
        assert!(character_class_contains("^a-z", 'A'));
        assert!(character_class_contains("^a-z", '0'));
    }

    #[test]
    fn character_class_supports_escapes() {
        assert!(character_class_contains(r"\]", ']'));
        assert!(character_class_contains(r"\-", '-'));
        assert!(character_class_contains(r"\\", '\\'));
        assert!(character_class_contains(r"\n", '\n'));
        assert!(character_class_contains(r"\t", '\t'));
        assert!(!character_class_contains(r"\]", '['));
    }

    #[test]
    fn character_class_supports_ascii_shorthands() {
        assert!(character_class_contains(r"\d", '5'));
        assert!(!character_class_contains(r"\d", 'x'));
        assert!(character_class_contains(r"\D", 'x'));
        assert!(character_class_contains(r"\w", '_'));
        assert!(character_class_contains(r"\w", 'a'));
        assert!(character_class_contains(r"\w", '0'));
        assert!(!character_class_contains(r"\w", ' '));
        assert!(character_class_contains(r"\s", ' '));
        assert!(character_class_contains(r"\s", '\t'));
        assert!(!character_class_contains(r"\S", ' '));
    }

    #[test]
    fn character_class_preserves_existing_grammars() {
        // `[$A-Za-z_]` from base_loko `name` rule.
        assert!(character_class_contains("$A-Za-z_", '$'));
        assert!(character_class_contains("$A-Za-z_", 'a'));
        assert!(character_class_contains("$A-Za-z_", 'Z'));
        assert!(character_class_contains("$A-Za-z_", '_'));
        assert!(!character_class_contains("$A-Za-z_", '0'));
        // `[A-Za-z]` and `[0-9]` from records and calculator.
        assert!(character_class_contains("A-Za-z", 'q'));
        assert!(character_class_contains("0-9", '5'));
        assert!(!character_class_contains("0-9", 'a'));
    }

    #[test]
    fn compiled_grammar_matches_slice_behavior() {
        let grammar = "grammar tiny\n\
                       rule pair = name `:` type-name\n\
                       rule name = [A-Za-z_]+\n\
                       rule type-name = `<` name `>`\n\
                       end\n";
        let rules = parse_grammar_source(grammar);
        let compiled = CompiledGrammar::compile(rules.clone());

        // Lookup parity.
        for rule in compiled.rules() {
            assert!(compiled.rule(rule.name).is_some(), "rule {}", rule.name);
        }
        assert!(compiled.rule("missing").is_none());

        // Parse parity for success.
        let from_slice =
            interpret_region(&rules, "pair", "radius: <integer>", 100).expect("slice parse");
        let from_compiled = interpret_region_compiled(&compiled, "pair", "radius: <integer>", 100)
            .expect("compiled parse");
        assert_eq!(from_slice, from_compiled);

        // Parse parity for failure.
        let slice_err = interpret_region(&rules, "pair", "", 0).expect_err("slice err");
        let compiled_err =
            interpret_region_compiled(&compiled, "pair", "", 0).expect_err("compiled err");
        assert_eq!(slice_err, compiled_err);

        // Inspect parity for trailing input.
        let slice_report = inspect_region(&rules, "pair", "radius: <integer> trail", 0);
        let compiled_report =
            inspect_region_compiled(&compiled, "pair", "radius: <integer> trail", 0);
        assert_eq!(slice_report, compiled_report);

        // interpret_rule_error parity.
        let slice_rule_err = interpret_rule_error(&rules, "name", "123", 0, 0);
        let compiled_rule_err = interpret_rule_error_compiled(&compiled, "name", "123", 0, 0);
        assert_eq!(slice_rule_err, compiled_rule_err);
    }

    #[test]
    fn span_cover_and_intersect() {
        let lhs = Span::new(5, 10);
        let rhs = Span::new(8, 14);

        assert_eq!(lhs.cover(rhs), Span::new(5, 14));
        assert_eq!(rhs.cover(lhs), Span::new(5, 14));
        assert_eq!(lhs.intersect(rhs), Some(Span::new(8, 10)));
        assert_eq!(rhs.intersect(lhs), Some(Span::new(8, 10)));

        let disjoint = Span::new(10, 12);
        assert_eq!(lhs.intersect(disjoint), None, "touching half-open spans");
        assert_eq!(disjoint.intersect(lhs), None);

        let nested = Span::new(6, 9);
        assert_eq!(lhs.intersect(nested), Some(nested));
    }

    #[test]
    fn descendants_in_span_returns_contained_nodes() {
        let grammar = "grammar tiny\nrule pair = name `:` name\nrule name = [A-Za-z]+\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "pair", "alpha: beta", 0).expect("pair parse");

        let outer = parse.root.descendants_in_span(parse.root.span);
        assert!(
            outer.len() >= 3,
            "outer span should contain at least the two names and root, got {}",
            outer.len()
        );
        assert!(outer.iter().any(|node| node.is_rule("pair")));

        let alpha_only = parse.root.descendants_in_span(Span::new(0, 5));
        let names: Vec<_> = alpha_only
            .iter()
            .filter(|node| node.is_rule("name"))
            .map(|node| node.text)
            .collect();
        assert_eq!(names, vec!["alpha"]);
    }

    #[test]
    fn path_to_offset_walks_to_smallest_covering_node() {
        let grammar = "grammar tiny\nrule pair = name `:` name\nrule name = [A-Za-z]+\nend\n";
        let rules = parse_grammar_source(grammar);
        let parse = interpret_region(&rules, "pair", "alpha: beta", 0).expect("pair parse");

        let path = parse.root.path_to_offset(8);
        assert!(!path.is_empty(), "8 should be inside the second name");
        assert_eq!(path[0].name(), Some("pair"));
        let last = path.last().expect("non-empty path");
        assert!(
            last.is_rule("name") || last.role() == RuleNodeRole::CharacterClass,
            "deepest node should be inside the name token, got {:?}",
            last.kind
        );
        assert!(last.text.contains('e') || last.text.contains('b'));

        let outside = parse.root.path_to_offset(parse.root.span.end + 1);
        assert!(outside.is_empty());
    }

    #[test]
    fn line_column_at_byte_offset_reports_1_based_positions() {
        let source = "abc\ndef\nghi";
        assert_eq!(line_column_at_byte_offset(source, 0), (1, 1));
        assert_eq!(line_column_at_byte_offset(source, 2), (1, 3));
        assert_eq!(line_column_at_byte_offset(source, 3), (1, 4)); // the newline itself
        assert_eq!(line_column_at_byte_offset(source, 4), (2, 1));
        assert_eq!(line_column_at_byte_offset(source, 9), (3, 2));
        // Clamped past the end.
        assert_eq!(line_column_at_byte_offset(source, source.len() + 5), (3, 4));
    }

    #[test]
    fn grammar_source_diagnostic_code_round_trips_as_str() {
        for code in [
            GrammarSourceDiagnosticCode::MissingRuleKeyword,
            GrammarSourceDiagnosticCode::MissingEquals,
            GrammarSourceDiagnosticCode::EmptyName,
            GrammarSourceDiagnosticCode::EmptyBody,
            GrammarSourceDiagnosticCode::DuplicateName,
        ] {
            assert!(!code.as_str().is_empty());
            assert!(!code.message().is_empty());
        }
    }
}
