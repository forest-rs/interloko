//! Grammar frontend projection for the base Loko parser.
//!
//! This module owns language-environment identity, grammar metaobjects,
//! grammar-stack resolution, staged parse activations, structural parse
//! emissions, and parse provenance for the source-authored grammar parser. It
//! intentionally does not own AST lowering, semantic lowering, macros, or host
//! semantic actions.

mod actions;
mod base_loko_primary;
mod calculator;
mod interpreter;
mod records;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

pub use interloko_grammar::{
    GrammarAction, GrammarActionArg, GrammarActionTerm, RuleExpression, TransformOutput,
};

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::{SourceFileId, StableIdBuilder};
use crate::source_data::{self, SourceData};
use crate::syntax::{self, CstElement, CstKind, CstNode, Span, SyntaxTree};

impl From<interloko_grammar::Span> for Span {
    fn from(span: interloko_grammar::Span) -> Self {
        Self::new(span.start, span.end)
    }
}

/// Stable source of a projected grammar environment fact.
///
/// This is compiler/frontend metadata used for grammar MOP/EOP projection. It
/// is intentionally not part of `interloko_grammar`, whose parser kernel does
/// not branch on projection provenance while matching input.
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

const DOMAIN_LANGUAGE_ENVIRONMENT: u64 = 0x4c4f_4b4f_5f45_4e56;
const DOMAIN_GRAMMAR: u64 = 0x4c4f_4b4f_5f47_524d;
const DOMAIN_GRAMMAR_RULE: u64 = 0x4c4f_4b4f_5f52_554c;
const DOMAIN_GRAMMAR_ACTIVATION: u64 = 0x4c4f_4b4f_5f41_4354;
const DOMAIN_PARSE_PROVENANCE: u64 = 0x4c4f_4b4f_5f50_5256;

/// Stable name for the base language environment projected by the compiler.
pub const BASE_LOKO_ENVIRONMENT_NAME: &str = "BaseLokoEnvironment";
/// Stable name for the base grammar projected by the compiler.
pub const BASE_LOKO_GRAMMAR_NAME: &str = "BaseLokoGrammar";
/// Stable name for the trusted derived host environment fixture.
pub const DERIVED_HOST_ENVIRONMENT_NAME: &str = "DerivedHostEnvironment";
/// Stable name for the trusted derived host grammar fixture.
pub const DERIVED_HOST_GRAMMAR_NAME: &str = "DerivedHostGrammar";
/// Stable name for the tiny staged calculator grammar prototype.
pub const CALCULATOR_GRAMMAR_NAME: &str = "CalculatorRegionGrammar";
/// Stable name for the trusted staged records grammar prototype.
pub const RECORDS_GRAMMAR_NAME: &str = "RecordsRegionGrammar";
/// Stable name for the directive-activated staged grammar environment.
pub const STAGED_LOKO_ENVIRONMENT_NAME: &str = "StagedLokoEnvironment";

/// Stable registry token for the base language environment.
pub const BASE_LOKO_ENVIRONMENT_SLUG: &str = "base-loko";
/// Stable registry token for the trusted host-derived language environment.
pub const DERIVED_HOST_ENVIRONMENT_SLUG: &str = "derived-host";
/// Stable registry token for the directive-activated staged environment.
pub const STAGED_LOKO_ENVIRONMENT_SLUG: &str = "staged-loko";
/// Stable registry token for the calculator staged grammar.
pub const CALCULATOR_GRAMMAR_SLUG: &str = "calculator";
/// Stable registry token for the records staged grammar.
pub const RECORDS_GRAMMAR_SLUG: &str = "records";

const GRAMMAR_ACTIVATE_PREFIX: &str = "// grammar-activate:";
const STAGED_LOKO_PREFIX: &str = "// staged-loko:";
const STAGED_LOKO_END: &str = "// end-staged-loko";
const EMIT_FRONTEND_FORMS_CAPABILITIES: &[GrammarExtensionCapability] =
    &[GrammarExtensionCapability::EmitFrontendForms];
const CALCULATOR_EMITTED_FORM_KINDS: &[&str] = &["frontend-function"];
const RECORDS_EMITTED_FORM_KINDS: &[&str] = &["frontend-class"];

/// Capability required by a staged grammar extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrammarExtensionCapability {
    /// The extension may emit inert frontend forms for later materialization.
    EmitFrontendForms,
}

impl GrammarExtensionCapability {
    /// Returns the stable capability token used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmitFrontendForms => "emit-frontend-forms",
        }
    }
}

/// Selects the grammar environment used by the frontend.
///
/// Selections are registry tokens rather than a closed enum so new host or
/// source-authored language environments can be registered without adding a new
/// compiler variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarEnvironmentSelection {
    slug: String,
}

impl GrammarEnvironmentSelection {
    /// Creates a grammar environment selection from a registry token.
    #[must_use]
    pub fn new(slug: impl Into<String>) -> Self {
        Self { slug: slug.into() }
    }

    /// Returns the ordinary base Loko grammar environment selection.
    #[must_use]
    pub fn base_loko() -> Self {
        Self::new(BASE_LOKO_ENVIRONMENT_SLUG)
    }

    /// Returns the trusted host-derived fixture grammar environment selection.
    #[must_use]
    pub fn derived_host() -> Self {
        Self::new(DERIVED_HOST_ENVIRONMENT_SLUG)
    }

    /// Returns the stable command/config token associated with this selection.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.slug
    }
}

impl Default for GrammarEnvironmentSelection {
    fn default() -> Self {
        Self::base_loko()
    }
}

impl fmt::Display for GrammarEnvironmentSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for GrammarEnvironmentSelection {
    fn from(slug: &str) -> Self {
        Self::new(slug)
    }
}

impl From<String> for GrammarEnvironmentSelection {
    fn from(slug: String) -> Self {
        Self::new(slug)
    }
}

/// Stable id for a language environment.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct LanguageEnvironmentId(
    /// Stable raw identifier.
    pub u64,
);

impl LanguageEnvironmentId {
    /// Builds a language environment id from a stable name.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_LANGUAGE_ENVIRONMENT);
        builder.mix_str(name);
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for LanguageEnvironmentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lenv{:016x}", self.0)
    }
}

/// Stable id for a grammar.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct GrammarId(
    /// Stable raw identifier.
    pub u64,
);

impl GrammarId {
    /// Builds a grammar id from a stable name.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_GRAMMAR);
        builder.mix_str(name);
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for GrammarId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "grm{:016x}", self.0)
    }
}

/// Stable id for a projected grammar rule.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct GrammarRuleId(
    /// Stable raw identifier.
    pub u64,
);

impl GrammarRuleId {
    /// Builds a grammar rule id from the owning grammar and rule name.
    #[must_use]
    pub fn new(grammar: GrammarId, name: &str) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_GRAMMAR_RULE);
        builder.mix_u64(grammar.raw());
        builder.mix_str(name);
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for GrammarRuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "grule{:016x}", self.0)
    }
}

/// Stable id for one parse activation.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct GrammarActivationId(
    /// Stable raw identifier.
    pub u64,
);

impl GrammarActivationId {
    /// Builds a grammar activation id.
    #[must_use]
    pub fn new(source_file: SourceFileId, grammar: GrammarId, name: &str) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_GRAMMAR_ACTIVATION);
        builder.mix_u64(source_file.raw());
        builder.mix_u64(grammar.raw());
        builder.mix_str(name);
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for GrammarActivationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "gact{:016x}", self.0)
    }
}

/// Stable id for parse provenance.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ParseProvenanceId(
    /// Stable raw identifier.
    pub u64,
);

impl ParseProvenanceId {
    /// Builds a parse provenance id.
    #[must_use]
    pub fn new(source_file: SourceFileId, environment: LanguageEnvironmentId) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_PARSE_PROVENANCE);
        builder.mix_u64(source_file.raw());
        builder.mix_u64(environment.raw());
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ParseProvenanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "pprv{:016x}", self.0)
    }
}

/// Projection information for a grammar rule record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarProjection {
    /// Stable projection origin.
    pub origin: ProjectionOrigin,
    /// Implementation location or conceptual parser entry.
    pub location: String,
    /// Optional source span when a rule is source-authored.
    pub source_span: Option<Span>,
}

/// Returns inert source data for a grammar rule expression.
#[must_use]
pub fn rule_expression_source_data(expression: &RuleExpression<'_>) -> SourceData {
    match expression {
        RuleExpression::Rule(name) => SourceData::record(
            "rule-ref",
            vec![source_data::field("name", SourceData::symbol(*name))],
        ),
        RuleExpression::Literal(value) => SourceData::record(
            "literal",
            vec![source_data::field("value", SourceData::string(*value))],
        ),
        RuleExpression::CharacterClass(value) => SourceData::record(
            "character-class",
            vec![source_data::field("value", SourceData::string(*value))],
        ),
        RuleExpression::UntilLiteral(value) => SourceData::record(
            "until-literal",
            vec![source_data::field("value", SourceData::string(*value))],
        ),
        RuleExpression::Sequence(items) => SourceData::record(
            "sequence",
            vec![source_data::field(
                "items",
                SourceData::list(items.iter().map(rule_expression_source_data).collect()),
            )],
        ),
        RuleExpression::Choice(items) => SourceData::record(
            "choice",
            vec![source_data::field(
                "items",
                SourceData::list(items.iter().map(rule_expression_source_data).collect()),
            )],
        ),
        RuleExpression::Optional(item) => SourceData::record(
            "optional",
            vec![source_data::field(
                "item",
                rule_expression_source_data(item),
            )],
        ),
        RuleExpression::ZeroOrMore(item) => SourceData::record(
            "zero-or-more",
            vec![source_data::field(
                "item",
                rule_expression_source_data(item),
            )],
        ),
        RuleExpression::OneOrMore(item) => SourceData::record(
            "one-or-more",
            vec![source_data::field(
                "item",
                rule_expression_source_data(item),
            )],
        ),
        RuleExpression::Transform { input, output } => SourceData::record(
            "transform",
            vec![
                source_data::field("input", rule_expression_source_data(input)),
                source_data::field("output", transform_output_source_data(output)),
            ],
        ),
        RuleExpression::And(item) => SourceData::record(
            "positive-lookahead",
            vec![source_data::field(
                "item",
                rule_expression_source_data(item),
            )],
        ),
        RuleExpression::Not(item) => SourceData::record(
            "negative-lookahead",
            vec![source_data::field(
                "item",
                rule_expression_source_data(item),
            )],
        ),
        RuleExpression::Cut(item) => SourceData::record(
            "cut",
            vec![source_data::field(
                "item",
                rule_expression_source_data(item),
            )],
        ),
        RuleExpression::Named { name, inner } => SourceData::record(
            "named-subexpression",
            vec![
                source_data::field("name", SourceData::symbol(*name)),
                source_data::field("inner", rule_expression_source_data(inner)),
            ],
        ),
        RuleExpression::Apply { rule, args } => SourceData::record(
            "apply",
            vec![
                source_data::field("rule", SourceData::symbol(*rule)),
                source_data::field(
                    "args",
                    SourceData::list(args.iter().map(rule_expression_source_data).collect()),
                ),
            ],
        ),
        RuleExpression::Super(name) => SourceData::record(
            "super-rule-ref",
            vec![source_data::field("name", SourceData::symbol(*name))],
        ),
        _ => SourceData::record("unknown-rule-expression", Vec::new()),
    }
}

fn transform_output_source_data(output: &TransformOutput<'_>) -> SourceData {
    match output {
        TransformOutput::Action(action) => grammar_action_source_data(action),
        TransformOutput::Literal(value) => SourceData::string(*value),
        TransformOutput::Symbol(value) => SourceData::symbol(*value),
        _ => SourceData::record("unknown-transform-output", Vec::new()),
    }
}

/// Returns inert source data for a grammar action descriptor.
#[must_use]
pub fn grammar_action_source_data(action: &GrammarAction<'_>) -> SourceData {
    SourceData::record(
        "grammar-action",
        vec![
            source_data::field("name", SourceData::symbol(action.name)),
            source_data::field(
                "args",
                SourceData::list(
                    action
                        .args
                        .iter()
                        .map(grammar_action_arg_source_data)
                        .collect(),
                ),
            ),
        ],
    )
}

fn grammar_action_arg_source_data(arg: &GrammarActionArg<'_>) -> SourceData {
    SourceData::record(
        "grammar-action-arg",
        vec![
            source_data::field("name", SourceData::symbol(arg.name)),
            source_data::field("value", grammar_action_term_source_data(&arg.value)),
        ],
    )
}

fn grammar_action_term_source_data(term: &GrammarActionTerm<'_>) -> SourceData {
    match term {
        GrammarActionTerm::Capture { rule } => SourceData::record(
            "capture",
            vec![source_data::field("rule", SourceData::symbol(*rule))],
        ),
        GrammarActionTerm::CaptureAll { rule } => SourceData::record(
            "capture-all",
            vec![source_data::field("rule", SourceData::symbol(*rule))],
        ),
        GrammarActionTerm::CaptureByName { name } => SourceData::record(
            "capture-by-name",
            vec![source_data::field("name", SourceData::symbol(*name))],
        ),
        GrammarActionTerm::Literal(value) => SourceData::record(
            "literal",
            vec![source_data::field("value", SourceData::string(*value))],
        ),
        GrammarActionTerm::Bool(value) => SourceData::record(
            "bool",
            vec![source_data::field("value", SourceData::bool(*value))],
        ),
        GrammarActionTerm::Integer(value) => SourceData::record(
            "integer",
            vec![source_data::field("value", SourceData::integer(*value))],
        ),
        GrammarActionTerm::List(items) => SourceData::record(
            "list",
            vec![source_data::field(
                "items",
                SourceData::list(items.iter().map(grammar_action_term_source_data).collect()),
            )],
        ),
        GrammarActionTerm::Record(fields) => SourceData::record(
            "record",
            vec![source_data::field(
                "fields",
                SourceData::list(fields.iter().map(grammar_action_arg_source_data).collect()),
            )],
        ),
        GrammarActionTerm::Call(action) => SourceData::record(
            "call",
            vec![source_data::field(
                "action",
                grammar_action_source_data(action),
            )],
        ),
        _ => SourceData::record("unknown-grammar-action-term", Vec::new()),
    }
}

/// Read-only language environment metaobject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageEnvironment {
    /// Stable environment id.
    pub id: LanguageEnvironmentId,
    /// Stable environment slug used in object ids.
    pub slug: String,
    /// Stable display name.
    pub name: String,
    /// Grammars active for this environment in stack order.
    pub grammar_stack: Vec<GrammarId>,
}

/// Read-only grammar metaobject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Grammar {
    /// Stable grammar id.
    pub id: GrammarId,
    /// Stable grammar slug used in object ids.
    pub slug: String,
    /// Stable display name.
    pub name: String,
    /// Parent grammars. Empty for the base grammar.
    pub extends: Vec<GrammarId>,
    /// Direct projected rule ids in parser surface order.
    pub direct_rules: Vec<GrammarRuleId>,
    /// Effective rule ids after inheritance and overrides.
    pub effective_rules: Vec<GrammarRuleId>,
}

/// Read-only direct grammar rule metaobject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarRule {
    /// Stable rule id.
    pub id: GrammarRuleId,
    /// Owning grammar.
    pub grammar: GrammarId,
    /// Owning grammar slug used in object ids.
    pub grammar_slug: String,
    /// Rule name.
    pub name: String,
    /// Structured projected expression.
    pub expression: RuleExpression<'static>,
    /// Deterministic structural action attached to this rule, when any.
    pub action: Option<GrammarAction<'static>>,
    /// Inert structural expression data.
    pub expression_data: SourceData,
    /// Projection/source location information.
    pub projection: GrammarProjection,
}

/// How an effective grammar rule was resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveGrammarRuleResolution {
    /// The effective rule is defined directly by the target grammar.
    Direct,
    /// The effective rule is inherited from a parent grammar.
    Inherited,
    /// The effective rule is an overriding direct rule.
    Override,
}

impl EffectiveGrammarRuleResolution {
    /// Stable text used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Inherited => "inherited",
            Self::Override => "override",
        }
    }
}

/// Read-only effective grammar rule metaobject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveGrammarRule {
    /// Stable effective rule id.
    pub id: GrammarRuleId,
    /// Grammar whose effective rule table owns this rule.
    pub grammar: GrammarId,
    /// Owning grammar slug used in object ids.
    pub grammar_slug: String,
    /// Rule name.
    pub name: String,
    /// Direct rule selected by this effective rule.
    pub selected_rule: GrammarRuleId,
    /// Grammar that provides the selected direct rule.
    pub selected_rule_grammar: GrammarId,
    /// Direct rule overridden by this effective rule, when any.
    pub overridden_rule: Option<GrammarRuleId>,
    /// Resolution relationship.
    pub resolution: EffectiveGrammarRuleResolution,
}

/// Resolved grammar stack for one parse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarStack {
    /// Owning language environment.
    pub environment: LanguageEnvironmentId,
    /// Grammars in activation order.
    pub grammars: Vec<GrammarId>,
}

/// One grammar activation over a source region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarActivation {
    /// Stable activation id.
    pub id: GrammarActivationId,
    /// Active language environment.
    pub environment: LanguageEnvironmentId,
    /// Active grammar.
    pub grammar: GrammarId,
    /// Active grammar slug used in object ids.
    pub grammar_slug: String,
    /// Stable activation slug used in object ids.
    pub object_slug: String,
    /// Source region parsed by this activation.
    pub span: Span,
    /// Rule that produced the root parse node.
    pub root_rule: GrammarRuleId,
    /// Stable reason text.
    pub reason: String,
}

/// A structural frontend form emitted by a trusted staged grammar adapter.
///
/// This is a transitional expander-facing representation. The current frontend
/// lowers these forms directly into the AST. Materialized Loko remains available
/// as an inspectable debug projection, but it is not the semantic compiler input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendForm {
    /// Original staged source span that produced this form.
    pub source_span: Span,
    /// Specific emitted form.
    pub kind: FrontendFormKind,
}

impl FrontendForm {
    /// Builds a zero-argument function form.
    #[must_use]
    pub fn function(name: impl Into<String>, body: FrontendExpression, source_span: Span) -> Self {
        Self {
            source_span,
            kind: FrontendFormKind::Function(FrontendFunction {
                name: name.into(),
                body,
            }),
        }
    }

    /// Builds a class form.
    #[must_use]
    pub fn class(
        name: impl Into<String>,
        superclass: impl Into<String>,
        slots: Vec<FrontendSlot>,
        source_span: Span,
    ) -> Self {
        Self {
            source_span,
            kind: FrontendFormKind::Class(FrontendClass {
                name: name.into(),
                superclass: superclass.into(),
                slots,
            }),
        }
    }
}

/// Specific structural frontend form variants.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrontendFormKind {
    /// A zero-argument function definition.
    Function(FrontendFunction),
    /// A class definition with direct slots.
    Class(FrontendClass),
}

/// Structural frontend function form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendFunction {
    /// Function name.
    pub name: String,
    /// Function body expression.
    pub body: FrontendExpression,
}

/// Structural frontend class form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendClass {
    /// Class name, including angle brackets when appropriate.
    pub name: String,
    /// Superclass name, including angle brackets when appropriate.
    pub superclass: String,
    /// Direct slots emitted for this class.
    pub slots: Vec<FrontendSlot>,
}

/// Structural frontend slot declaration form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendSlot {
    /// Slot source span in the staged region.
    pub source_span: Span,
    /// Slot name.
    pub name: String,
    /// Slot type name, including angle brackets when appropriate.
    pub type_name: String,
    /// Whether this slot should be emitted as `constant slot`.
    pub constant: bool,
    /// Optional init keyword name without the trailing colon.
    pub init_keyword: Option<String>,
    /// Whether the init keyword is required.
    pub required_init_keyword: bool,
}

impl FrontendSlot {
    /// Builds a mutable slot with an optional init keyword.
    #[must_use]
    pub fn new(name: impl Into<String>, type_name: impl Into<String>, source_span: Span) -> Self {
        Self {
            source_span,
            name: name.into(),
            type_name: type_name.into(),
            constant: false,
            init_keyword: None,
            required_init_keyword: false,
        }
    }

    /// Returns a copy of this slot marked with an init keyword.
    #[must_use]
    pub fn with_init_keyword(mut self, keyword: impl Into<String>, required: bool) -> Self {
        self.init_keyword = Some(keyword.into());
        self.required_init_keyword = required;
        self
    }
}

/// Structural frontend expression form.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrontendExpression {
    /// Integer literal.
    Integer {
        /// Literal value.
        value: i64,
        /// Original staged source span.
        source_span: Span,
    },
    /// Name reference.
    Name {
        /// Referenced name.
        name: String,
        /// Original staged source span.
        source_span: Span,
    },
    /// Direct call expression.
    Call {
        /// Callee name.
        callee: String,
        /// Argument expressions.
        args: Vec<FrontendExpression>,
        /// Original staged source span.
        source_span: Span,
    },
    /// Binary expression.
    Binary {
        /// Operator.
        op: FrontendBinaryOp,
        /// Left operand.
        lhs: alloc::boxed::Box<FrontendExpression>,
        /// Right operand.
        rhs: alloc::boxed::Box<FrontendExpression>,
        /// Original staged source span.
        source_span: Span,
    },
}

/// Structural frontend binary operator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FrontendBinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
}

impl FrontendBinaryOp {
    /// Returns this operator's debug source token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
        }
    }
}

/// One staged source region and its structural parse result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedParseRegion {
    /// Stable region slug used in object ids.
    pub object_slug: String,
    /// Registered staged grammar adapter.
    pub adapter: String,
    /// Grammar activation that parsed this region.
    pub activation: GrammarActivationId,
    /// Grammar used for this staged region.
    pub grammar: GrammarId,
    /// Grammar slug used in dumps.
    pub grammar_slug: String,
    /// Parser driver used for this staged region.
    pub driver: StagedParseDriver,
    /// Optional parser oracle/fallback status for this staged region.
    pub oracle: StagedParseOracle,
    /// Comment directive that activated the staged grammar adapter.
    pub activation_directive: Span,
    /// Original source region parsed by the staged grammar.
    pub source_span: Span,
    /// Structural frontend forms emitted by the staged grammar.
    pub forms: Vec<FrontendForm>,
    /// Inert structural projection of `forms`.
    pub forms_data: SourceData,
    /// Ordinary Loko source materialized from frontend forms.
    pub materialized_source: String,
    /// Inert structural parse result.
    pub data: SourceData,
    /// Rule applications recorded by the staged grammar parser.
    pub rule_applications: Vec<GrammarRuleApplication>,
}

/// Parser driver used for one staged source region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagedParseDriver {
    /// Source-authored grammar interpreted by `grammar_frontend::interpreter`.
    InterpretedGrammar,
}

impl StagedParseDriver {
    /// Returns the stable label used in dumps, source data, and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InterpretedGrammar => "interpreted-grammar",
        }
    }
}

/// Parser oracle/fallback status for one staged source region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagedParseOracle {
    /// No legacy oracle or fallback participated in this staged-region parse.
    None,
}

impl StagedParseOracle {
    /// Returns the stable label used in dumps, source data, and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }
}

/// Registered staged grammar extension descriptor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarExtensionDescriptor {
    /// Stable adapter token.
    pub adapter: String,
    /// Grammar slug projected by this extension.
    pub grammar_slug: String,
    /// Human-facing grammar name.
    pub grammar_name: String,
    /// Directive that enables this extension for later staged regions.
    pub activation_directive: String,
    /// Directive that starts a staged region for this extension.
    pub region_start_directive: String,
    /// Directive that ends a staged region for this extension.
    pub region_end_directive: String,
    /// Human-readable accepted region shape.
    pub accepted_region: String,
    /// Root rule used for staged-region provenance.
    pub root_rule: String,
    /// Frontend form kinds this extension may emit.
    pub emitted_form_kinds: Vec<String>,
    /// Rule names charged when this staged adapter emits forms.
    pub provenance_rule_names: Vec<String>,
    /// Capabilities required by this extension.
    pub capabilities: Vec<GrammarExtensionCapability>,
}

/// Concrete use of a staged grammar extension in one parse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarExtensionActivation {
    /// Registered staged grammar adapter token.
    pub adapter: String,
    /// Grammar activation that parsed the source region.
    pub activation: GrammarActivationId,
    /// Grammar used for this staged region.
    pub grammar: GrammarId,
    /// Grammar slug used in object ids and dumps.
    pub grammar_slug: String,
    /// Stable region slug used in object ids.
    pub object_slug: String,
    /// Comment directive that enabled this adapter.
    pub activation_directive: Span,
    /// Comment directive that starts this staged region.
    pub start_marker: Span,
    /// Comment directive that ends this staged region.
    pub end_marker: Span,
    /// Original source region parsed by this extension.
    pub source_span: Span,
    /// Capabilities exercised by this activation.
    pub capabilities: Vec<GrammarExtensionCapability>,
    /// Number of frontend forms emitted by this activation.
    pub emitted_form_count: usize,
}

/// One frontend form emitted by a staged grammar extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarExtensionEmission {
    /// Registered staged grammar adapter token.
    pub adapter: String,
    /// Grammar activation that emitted the form.
    pub activation: GrammarActivationId,
    /// Stable region slug used in object ids.
    pub object_slug: String,
    /// Zero-based emitted form index within the region.
    pub form_index: usize,
    /// Stable emitted frontend form kind.
    pub form_kind: String,
    /// Source span that produced the emitted form.
    pub source_span: Span,
    /// Inert structural emitted form data.
    pub data: SourceData,
}

/// Provenance rule charged to a staged grammar extension activation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarExtensionProvenance {
    /// Registered staged grammar adapter token.
    pub adapter: String,
    /// Grammar activation associated with the rule.
    pub activation: GrammarActivationId,
    /// Stable region slug used in object ids.
    pub object_slug: String,
    /// Grammar that owns the effective rule.
    pub grammar: GrammarId,
    /// Effective rule charged to the activation.
    pub rule: GrammarRuleId,
    /// Human-readable rule name.
    pub rule_name: String,
    /// Number of parse/projection nodes associated with this rule.
    pub node_count: usize,
}

/// Per-rule parse provenance summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarRuleProvenance {
    /// Grammar rule.
    pub rule: GrammarRuleId,
    /// Rule name.
    pub rule_name: String,
    /// Number of CST nodes produced for this rule.
    pub node_count: usize,
}

/// One grammar rule application recorded from parser output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarRuleApplication {
    /// Parse activation that applied the rule.
    pub activation: GrammarActivationId,
    /// Grammar whose effective rule was applied.
    pub grammar: GrammarId,
    /// Effective rule applied by the parser.
    pub rule: GrammarRuleId,
    /// Stable rule name.
    pub rule_name: String,
    /// Source span covered by the parser node.
    pub span: Span,
}

/// How a grammar diagnostic was associated with parser provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrammarDiagnosticContext {
    /// The diagnostic span exactly matched a recorded rule application.
    ExactRuleApplication,
    /// The diagnostic span was contained by a recorded rule application.
    ContainingRuleApplication,
    /// No recorded application matched, so the activation root rule was used.
    RootRuleFallback,
}

impl GrammarDiagnosticContext {
    /// Returns the stable label used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactRuleApplication => "exact-rule-application",
            Self::ContainingRuleApplication => "containing-rule-application",
            Self::RootRuleFallback => "root-rule-fallback",
        }
    }
}

/// Grammar-specific view of one parser diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarDiagnosticFact {
    /// Zero-based diagnostic index in the parse report.
    pub diagnostic_index: usize,
    /// Stable diagnostic code.
    pub code: DiagnosticCode,
    /// Primary source span.
    pub span: Span,
    /// Language environment active for the diagnostic.
    pub environment: LanguageEnvironmentId,
    /// Grammar active for the diagnostic.
    pub grammar: GrammarId,
    /// Parse activation active for the diagnostic.
    pub activation: GrammarActivationId,
    /// Effective grammar rule active at or nearest the diagnostic span.
    pub rule: GrammarRuleId,
    /// Effective rule name active at or nearest the diagnostic span.
    pub rule_name: String,
    /// How the diagnostic was associated with parser provenance.
    pub context: GrammarDiagnosticContext,
    /// Span of the matched parser application, when one was available.
    pub application_span: Option<Span>,
    /// Source fragment found at the diagnostic span, when the span is not
    /// zero-width.
    pub unexpected_fragment: Option<String>,
    /// Structured expected fragment, when the syntax diagnostic provides one.
    pub expected_fragment: Option<String>,
}

/// Parse provenance for the frontend invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseProvenance {
    /// Stable provenance id.
    pub id: ParseProvenanceId,
    /// Source file parsed.
    pub source_file: SourceFileId,
    /// Resolved environment.
    pub environment: LanguageEnvironmentId,
    /// Resolved stack.
    pub grammar_stack: GrammarStack,
    /// Activations that parsed the source.
    pub activations: Vec<GrammarActivationId>,
    /// Individual parser rule applications.
    pub applications: Vec<GrammarRuleApplication>,
    /// Rule provenance summaries.
    pub rules: Vec<GrammarRuleProvenance>,
    /// Grammar-specific diagnostic facts.
    pub diagnostics: Vec<GrammarDiagnosticFact>,
}

/// Source text variant consumed by the base grammar parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendParseSource {
    /// Parser consumed the original source file text.
    OriginalSource,
    /// Parser consumed a derived source where staged regions were blanked and
    /// materialized frontend forms were inserted through the staged pipeline.
    StagedRegionsBlanked,
}

impl FrontendParseSource {
    /// Returns the stable source token used in dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OriginalSource => "original-source",
            Self::StagedRegionsBlanked => "staged-regions-blanked",
        }
    }
}

/// Owned frontend parse artifact preserved after CST projection.
///
/// The borrowed `interloko_grammar` parse tree is converted into
/// [`SourceData`] before it enters this report so sessions can keep parse
/// provenance without carrying parser lifetimes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontendParseArtifact {
    /// Source file parsed.
    pub source_file: SourceFileId,
    /// Language environment selected for the parse.
    pub environment: LanguageEnvironmentId,
    /// Grammar used by the base source parse.
    pub grammar: GrammarId,
    /// Activation used by the base source parse.
    pub activation: GrammarActivationId,
    /// Root rule used by the base source parse.
    pub root_rule: GrammarRuleId,
    /// Source variant consumed by the parser.
    pub source: FrontendParseSource,
    /// Span of the source parsed by this artifact.
    pub source_span: Span,
    /// Owned, navigable parse tree produced by the kernel matcher.
    ///
    /// This is the lossless form: spans, raw text, and child
    /// structure are preserved exactly as the matcher built them.
    /// Editor consumers and tools that want position-based queries
    /// (`rule_at_offset`, `smallest_covering_span`, `path_to_offset`)
    /// should walk this tree.
    ///
    /// Spans and `text` on this tree describe the *parsed* source —
    /// which may differ from the original file when
    /// [`Self::source`] is [`FrontendParseSource::StagedRegionsBlanked`].
    /// In that mode the parser consumed a copy of the file with
    /// staged regions blanked out, so `parse_tree.text` does *not*
    /// slice the original file across blanked spans. Tools that
    /// need original-file text should keep the file contents
    /// alongside and use spans rather than the embedded `text`.
    pub parse_tree: Option<interloko_grammar::OwnedRuleNode>,
    /// Inert structural projection of the parse tree as `SourceData`.
    ///
    /// Equivalent in content to [`Self::parse_tree`] but flattened to
    /// generic data records — convenient for dumps, golden tests, and
    /// tools that traverse parser facts without a parse-tree
    /// dependency. Populated alongside [`Self::parse_tree`].
    pub parse_tree_data: Option<SourceData>,
    /// Number of CST nodes produced by projection.
    pub cst_node_count: usize,
    /// Number of parser diagnostics attached to the syntax tree.
    pub diagnostic_count: usize,
}

/// Complete report for parsing one source through a grammar frontend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarParseReport {
    /// Source file parsed.
    pub source_file: SourceFileId,
    /// Resolved language environment.
    pub environment: LanguageEnvironment,
    /// Projected grammar records.
    pub grammars: Vec<Grammar>,
    /// Projected direct grammar rules.
    pub rules: Vec<GrammarRule>,
    /// Projected effective grammar rules.
    pub effective_rules: Vec<EffectiveGrammarRule>,
    /// Resolved grammar stack.
    pub grammar_stack: GrammarStack,
    /// Grammar activations used by this parse.
    pub activations: Vec<GrammarActivation>,
    /// Staged parse regions, if any.
    pub staged_regions: Vec<StagedParseRegion>,
    /// Concrete staged grammar extension activations in this parse.
    pub extension_activations: Vec<GrammarExtensionActivation>,
    /// Frontend forms emitted by staged grammar extensions.
    pub extension_emissions: Vec<GrammarExtensionEmission>,
    /// Provenance rules charged to staged grammar extensions.
    pub extension_provenance: Vec<GrammarExtensionProvenance>,
    /// Parse provenance facts.
    pub provenance: ParseProvenance,
    /// Grammar-specific diagnostic facts.
    pub grammar_diagnostics: Vec<GrammarDiagnosticFact>,
    /// Frontend parse artifact before AST conversion.
    pub frontend_parse: FrontendParseArtifact,
    /// Syntax diagnostics from the parser.
    pub diagnostics: Vec<Diagnostic>,
    /// Syntax tree produced by the active grammar parser.
    pub syntax: SyntaxTree,
}

/// Normalized parser product used by parser shadow comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserProductSummary {
    /// Stable parser implementation label.
    pub implementation: String,
    /// Number of normalized parser nodes in the compared subset.
    pub cst_node_count: usize,
    /// Number of top-level CST definition nodes.
    pub top_level_node_count: usize,
    /// Ordered top-level rule signature in the compared subset.
    pub top_level_signature: String,
    /// Number of diagnostics produced by the parser.
    pub diagnostic_count: usize,
    /// Number of recorded grammar rule applications.
    pub rule_application_count: usize,
}

impl ParserProductSummary {
    /// Creates a normalized parser product summary.
    #[must_use]
    pub fn new(
        implementation: impl Into<String>,
        cst_node_count: usize,
        top_level_node_count: usize,
        diagnostic_count: usize,
        rule_application_count: usize,
    ) -> Self {
        Self {
            implementation: implementation.into(),
            cst_node_count,
            top_level_node_count,
            top_level_signature: String::new(),
            diagnostic_count,
            rule_application_count,
        }
    }

    /// Creates a normalized parser product summary with a top-level signature.
    #[must_use]
    pub fn with_top_level_signature(
        implementation: impl Into<String>,
        cst_node_count: usize,
        top_level_node_count: usize,
        top_level_signature: impl Into<String>,
        diagnostic_count: usize,
        rule_application_count: usize,
    ) -> Self {
        Self {
            implementation: implementation.into(),
            cst_node_count,
            top_level_node_count,
            top_level_signature: top_level_signature.into(),
            diagnostic_count,
            rule_application_count,
        }
    }
}

/// Status for parser shadow comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParserShadowStatus {
    /// No oracle parser is available yet.
    OracleUnavailable,
    /// Primary and oracle summaries match.
    Equivalent,
    /// Primary and oracle summaries differ.
    Diverged,
}

impl ParserShadowStatus {
    /// Returns the stable status token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OracleUnavailable => "oracle-unavailable",
            Self::Equivalent => "equivalent",
            Self::Diverged => "diverged",
        }
    }
}

/// One parser shadow comparison difference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserShadowDivergence {
    /// Compared field name.
    pub field: String,
    /// Primary parser value.
    pub primary: String,
    /// Oracle parser value.
    pub oracle: String,
}

/// Parser shadow comparison report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParserShadowReport {
    /// Primary parser product.
    pub primary: ParserProductSummary,
    /// Optional oracle parser product.
    pub oracle: Option<ParserProductSummary>,
    /// Comparison status.
    pub status: ParserShadowStatus,
    /// Diffable summary divergences.
    pub divergences: Vec<ParserShadowDivergence>,
}

impl ParserShadowReport {
    /// Builds a report for a primary parser without an available oracle.
    #[must_use]
    pub fn oracle_unavailable(primary: ParserProductSummary) -> Self {
        Self {
            primary,
            oracle: None,
            status: ParserShadowStatus::OracleUnavailable,
            divergences: Vec::new(),
        }
    }
}

/// Provider for a registered language environment.
///
/// A provider contributes the grammar records for one environment token and
/// keeps language-environment selection out of hardcoded caller matches.
pub trait GrammarEnvironmentProvider: Sync {
    /// Stable registry token accepted by source headers and tools.
    fn environment_slug(&self) -> &'static str;

    /// Stable human-facing environment name.
    fn environment_name(&self) -> &'static str;

    /// Projected grammar records for this environment.
    fn records(&self) -> GrammarFrontendRecords;

    /// Whether this environment scans source for staged grammar regions.
    fn discovers_staged_regions(&self) -> bool {
        false
    }
}

/// Adapter protocol for directive-activated staged grammar regions.
///
/// Staged adapters parse only their delimited source region and emit inert
/// frontend forms. They do not execute host code or bypass the ordinary
/// syntax-to-AST-to-semantic pipeline.
pub trait StagedGrammarAdapter: Sync {
    /// Stable adapter token used in `// grammar-activate:` and
    /// `// staged-loko:` directives.
    fn adapter_name(&self) -> &'static str;

    /// Stable human-facing grammar name.
    fn grammar_name(&self) -> &'static str;

    /// Stable EOP/object slug for this grammar.
    fn grammar_slug(&self) -> &'static str;

    /// Root rule used for staged-region provenance.
    fn root_rule(&self) -> &'static str;

    /// Rule names charged when this staged adapter emits forms.
    fn provenance_rule_names(&self) -> &'static [&'static str];

    /// Capabilities required by this staged grammar extension.
    fn required_capabilities(&self) -> &'static [GrammarExtensionCapability] {
        EMIT_FRONTEND_FORMS_CAPABILITIES
    }

    /// Human-readable accepted staged region shape.
    fn accepted_region(&self) -> &'static str {
        "comment-delimited staged Loko region"
    }

    /// Frontend form kinds this staged grammar extension may emit.
    fn emitted_form_kinds(&self) -> &'static [&'static str];

    /// Returns the structured extension descriptor for this adapter.
    fn extension_descriptor(&self) -> GrammarExtensionDescriptor {
        GrammarExtensionDescriptor {
            adapter: self.adapter_name().to_string(),
            grammar_slug: self.grammar_slug().to_string(),
            grammar_name: self.grammar_name().to_string(),
            activation_directive: format!("{GRAMMAR_ACTIVATE_PREFIX} {}", self.adapter_name()),
            region_start_directive: format!("{STAGED_LOKO_PREFIX} {}", self.adapter_name()),
            region_end_directive: STAGED_LOKO_END.to_string(),
            accepted_region: self.accepted_region().to_string(),
            root_rule: self.root_rule().to_string(),
            emitted_form_kinds: self
                .emitted_form_kinds()
                .iter()
                .map(|kind| (*kind).to_string())
                .collect(),
            provenance_rule_names: self
                .provenance_rule_names()
                .iter()
                .map(|rule| (*rule).to_string())
                .collect(),
            capabilities: self.required_capabilities().to_vec(),
        }
    }

    /// Direct grammar rules projected for this staged grammar.
    fn direct_rules(&self, grammar: GrammarId) -> Vec<GrammarRule>;

    /// Parses a delimited source region into frontend forms and diagnostics.
    fn parse_region(&self, body: &str, source_offset: usize) -> StagedRegionEmission;
}

/// Registry of language environments and staged grammar adapters.
#[derive(Clone, Copy)]
pub struct GrammarFrontendRegistry {
    environment_providers: &'static [&'static dyn GrammarEnvironmentProvider],
    staged_adapters: &'static [&'static dyn StagedGrammarAdapter],
}

impl fmt::Debug for GrammarFrontendRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrammarFrontendRegistry")
            .field("environment_slugs", &self.environment_slugs())
            .field("staged_adapter_names", &self.staged_adapter_names())
            .finish()
    }
}

impl PartialEq for GrammarFrontendRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.environment_slugs() == other.environment_slugs()
            && self.staged_adapter_names() == other.staged_adapter_names()
    }
}

impl Eq for GrammarFrontendRegistry {}

impl Default for GrammarFrontendRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl GrammarFrontendRegistry {
    /// Creates a registry from explicit static providers.
    #[must_use]
    pub const fn new(
        environment_providers: &'static [&'static dyn GrammarEnvironmentProvider],
        staged_adapters: &'static [&'static dyn StagedGrammarAdapter],
    ) -> Self {
        Self {
            environment_providers,
            staged_adapters,
        }
    }

    /// Returns the built-in registry used by sessions and workbench tools.
    #[must_use]
    pub const fn builtin() -> Self {
        Self::new(
            &BUILTIN_GRAMMAR_ENVIRONMENTS,
            &BUILTIN_STAGED_GRAMMAR_ADAPTERS,
        )
    }

    /// Finds a language environment provider by registry token.
    #[must_use]
    pub fn environment_provider(
        &self,
        slug: &str,
    ) -> Option<&'static dyn GrammarEnvironmentProvider> {
        self.environment_providers
            .iter()
            .copied()
            .find(|provider| provider.environment_slug() == slug)
    }

    /// Returns the registered language environment providers in resolution order.
    #[must_use]
    pub const fn environment_providers(
        &self,
    ) -> &'static [&'static dyn GrammarEnvironmentProvider] {
        self.environment_providers
    }

    /// Converts a registry token into a selection if the environment exists.
    #[must_use]
    pub fn environment_selection(&self, slug: &str) -> Option<GrammarEnvironmentSelection> {
        self.environment_provider(slug)
            .map(|provider| GrammarEnvironmentSelection::new(provider.environment_slug()))
    }

    /// Returns the registered language environment tokens.
    #[must_use]
    pub fn environment_slugs(&self) -> Vec<&'static str> {
        self.environment_providers
            .iter()
            .map(|provider| provider.environment_slug())
            .collect()
    }

    /// Finds a staged grammar adapter by registry token.
    #[must_use]
    pub fn staged_adapter(&self, name: &str) -> Option<&'static dyn StagedGrammarAdapter> {
        self.staged_adapters
            .iter()
            .copied()
            .find(|adapter| adapter.adapter_name() == name)
    }

    /// Returns the registered staged grammar adapters in resolution order.
    #[must_use]
    pub const fn staged_adapters(&self) -> &'static [&'static dyn StagedGrammarAdapter] {
        self.staged_adapters
    }

    /// Returns the registered staged adapter tokens.
    #[must_use]
    pub fn staged_adapter_names(&self) -> Vec<&'static str> {
        self.staged_adapters
            .iter()
            .map(|adapter| adapter.adapter_name())
            .collect()
    }

    /// Returns descriptors for the registered staged grammar extensions.
    #[must_use]
    pub fn extension_descriptors(&self) -> Vec<GrammarExtensionDescriptor> {
        self.staged_adapters
            .iter()
            .map(|adapter| adapter.extension_descriptor())
            .collect()
    }

    /// Returns the descriptor for one registered staged grammar extension.
    #[must_use]
    pub fn extension_descriptor(&self, name: &str) -> Option<GrammarExtensionDescriptor> {
        self.staged_adapter(name)
            .map(|adapter| adapter.extension_descriptor())
    }

    fn staged_records(&self, adapter_names: &[&str]) -> GrammarFrontendRecords {
        staged_registry_records(adapter_names, self)
    }
}

/// Parses source through the base Loko grammar frontend.
#[must_use]
pub fn parse_file(source_file: SourceFileId, source: &str) -> GrammarParseReport {
    parse_file_with_environment(
        source_file,
        source,
        GrammarEnvironmentSelection::base_loko(),
    )
}

/// Parses source through the selected grammar frontend.
///
/// The selected environment controls the projected grammar stack, effective
/// rule table, activations, and provenance records.
#[must_use]
pub fn parse_file_with_environment(
    source_file: SourceFileId,
    source: &str,
    environment: GrammarEnvironmentSelection,
) -> GrammarParseReport {
    parse_file_with_registry(
        source_file,
        source,
        environment,
        &GrammarFrontendRegistry::builtin(),
    )
}

/// Parses source through an explicit grammar frontend registry.
///
/// Unknown environment selections still produce a report, using the base parser
/// plus a structured diagnostic, so session callers do not need to handle a
/// separate parse failure path.
#[must_use]
pub fn parse_file_with_registry(
    source_file: SourceFileId,
    source: &str,
    environment: GrammarEnvironmentSelection,
    registry: &GrammarFrontendRegistry,
) -> GrammarParseReport {
    if let Some(provider) = registry.environment_provider(environment.as_str()) {
        if provider.discovers_staged_regions()
            && let Some(report) = parse_staged_file(source_file, source, registry)
        {
            return report;
        }
        return parse_file_with_records(source_file, source, provider.records());
    }

    let mut report = parse_file_with_records(source_file, source, base_loko_records());
    push_report_diagnostic(
        &mut report,
        unknown_environment_diagnostic(Span::new(0, 0), environment.as_str(), registry),
    );
    report
}

/// Builds the current parser shadow report for a grammar parse report.
///
/// The recursive-descent oracle has been removed. The report now summarizes the
/// active source-authored grammar parser product.
#[must_use]
pub fn parser_shadow_report(report: &GrammarParseReport) -> ParserShadowReport {
    ParserShadowReport::oracle_unavailable(top_level_shadow_summary(
        "source-authored-base-loko-grammar",
        &report.syntax.root,
        report.diagnostics.len(),
    ))
}

/// Compares two normalized parser products.
#[must_use]
pub fn compare_parser_products(
    primary: ParserProductSummary,
    oracle: ParserProductSummary,
) -> ParserShadowReport {
    let divergences = parser_product_divergences(&primary, &oracle);
    let status = if divergences.is_empty() {
        ParserShadowStatus::Equivalent
    } else {
        ParserShadowStatus::Diverged
    };
    ParserShadowReport {
        primary,
        oracle: Some(oracle),
        status,
        divergences,
    }
}

/// Compiles the grammar that drives parsing for `environment_slug`.
///
/// Returns `derived-host`'s parametric grammar when the slug names that
/// environment; otherwise returns plain base-loko. Unknown slugs map
/// to base-loko so the fallback path in
/// [`parse_file_with_registry`] still produces a parse.
fn compile_grammar_for_environment(environment_slug: &str) -> interpreter::CompiledGrammar {
    if environment_slug == DERIVED_HOST_ENVIRONMENT_SLUG {
        interpreter::compile_derived_host_grammar()
    } else {
        interpreter::compile_base_loko_grammar()
    }
}

fn parse_file_with_records(
    source_file: SourceFileId,
    source: &str,
    records: GrammarFrontendRecords,
) -> GrammarParseReport {
    let grammar = compile_grammar_for_environment(&records.environment.slug);
    let (syntax, parse_tree, parse_tree_data) =
        match base_loko_primary::parse_file(&grammar, source_file, source) {
            Ok(success) => (
                success.syntax,
                Some(success.parse_tree),
                Some(success.parse_tree_data),
            ),
            Err(diagnostic) => (
                syntax_tree_with_diagnostic(source_file, source, diagnostic),
                None,
                None,
            ),
        };
    let active_grammar = records.active_grammar;
    let active_grammar_slug = records
        .grammars
        .iter()
        .find(|grammar| grammar.id == active_grammar)
        .map(|grammar| grammar.slug.clone())
        .unwrap_or_else(|| stable_name("ActiveGrammar"));
    let root_rule =
        effective_rule_id_by_name(&records.effective_rules, active_grammar, "source-file")
            .or_else(|| rule_id_by_name(&records.rules, "source-file"))
            .unwrap_or_else(|| GrammarRuleId::new(active_grammar, "source-file"));
    let activation = GrammarActivation {
        id: GrammarActivationId::new(source_file, active_grammar, "source-file"),
        environment: records.environment.id,
        grammar: active_grammar,
        grammar_slug: active_grammar_slug,
        object_slug: "source-file".to_string(),
        span: Span::new(0, source.len()),
        root_rule,
        reason: format!(
            "{} source file parse delegated to base parser",
            records.environment.slug
        ),
    };
    let frontend_parse = FrontendParseArtifact {
        source_file,
        environment: records.environment.id,
        grammar: active_grammar,
        activation: activation.id,
        root_rule,
        source: FrontendParseSource::OriginalSource,
        source_span: Span::new(0, source.len()),
        parse_tree,
        parse_tree_data,
        cst_node_count: cst_node_count(&syntax.root),
        diagnostic_count: syntax.diagnostics.len(),
    };
    let active_effective_rules = records
        .effective_rules
        .iter()
        .filter(|rule| rule.grammar == active_grammar)
        .cloned()
        .collect::<Vec<_>>();
    let applications = rule_applications(&active_effective_rules, &activation, &syntax.root);
    let provenance_rules = rule_provenance(&active_effective_rules, &applications);
    let grammar_diagnostics = grammar_diagnostic_facts(
        source,
        &syntax.diagnostics,
        &activation,
        &active_effective_rules,
        &applications,
    );
    let grammar_stack = GrammarStack {
        environment: records.environment.id,
        grammars: records.environment.grammar_stack.clone(),
    };
    let provenance = ParseProvenance {
        id: ParseProvenanceId::new(source_file, records.environment.id),
        source_file,
        environment: records.environment.id,
        grammar_stack: grammar_stack.clone(),
        activations: vec![activation.id],
        applications,
        rules: provenance_rules,
        diagnostics: grammar_diagnostics.clone(),
    };

    GrammarParseReport {
        source_file,
        environment: records.environment,
        grammars: records.grammars,
        rules: records.rules,
        effective_rules: records.effective_rules,
        grammar_stack,
        activations: vec![activation],
        staged_regions: Vec::new(),
        extension_activations: Vec::new(),
        extension_emissions: Vec::new(),
        extension_provenance: Vec::new(),
        provenance,
        grammar_diagnostics,
        frontend_parse,
        diagnostics: syntax.diagnostics.clone(),
        syntax,
    }
}

fn syntax_tree_with_diagnostic(
    source_file: SourceFileId,
    source: &str,
    diagnostic: Diagnostic,
) -> SyntaxTree {
    let tokens = syntax::lex(source);
    SyntaxTree {
        source_file,
        source: source.into(),
        tokens,
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

/// Materializes frontend forms into ordinary Loko source.
#[must_use]
pub fn materialize_frontend_forms(forms: &[FrontendForm]) -> String {
    let mut source = String::new();
    for form in forms {
        materialize_frontend_form(form, &mut source);
    }
    source
}

fn materialize_frontend_form(form: &FrontendForm, source: &mut String) {
    match &form.kind {
        FrontendFormKind::Function(function) => {
            source.push_str("define function ");
            source.push_str(&function.name);
            source.push_str("()\n  ");
            materialize_frontend_expression(&function.body, source);
            source.push_str("\nend\n");
        }
        FrontendFormKind::Class(class) => {
            source.push_str("define class ");
            source.push_str(&class.name);
            source.push_str(" (");
            source.push_str(&class.superclass);
            source.push_str(")\n");
            for slot in &class.slots {
                source.push_str("  ");
                if slot.constant {
                    source.push_str("constant ");
                }
                source.push_str("slot ");
                source.push_str(&slot.name);
                source.push_str(" :: ");
                source.push_str(&slot.type_name);
                if let Some(keyword) = &slot.init_keyword {
                    if slot.required_init_keyword {
                        source.push_str(", required-init-keyword: ");
                    } else {
                        source.push_str(", init-keyword: ");
                    }
                    source.push_str(keyword);
                    source.push(':');
                }
                source.push_str(";\n");
            }
            source.push_str("end\n");
        }
    }
}

fn materialize_frontend_expression(expression: &FrontendExpression, source: &mut String) {
    materialize_frontend_expression_with_precedence(expression, source, 0);
}

fn materialize_frontend_expression_with_precedence(
    expression: &FrontendExpression,
    source: &mut String,
    parent_precedence: u8,
) {
    let precedence = frontend_expression_precedence(expression);
    let needs_parens = precedence < parent_precedence;
    if needs_parens {
        source.push('(');
    }
    match expression {
        FrontendExpression::Integer { value, .. } => source.push_str(&value.to_string()),
        FrontendExpression::Name { name, .. } => source.push_str(name),
        FrontendExpression::Call { callee, args, .. } => {
            source.push_str(callee);
            source.push('(');
            for (index, arg) in args.iter().enumerate() {
                if index != 0 {
                    source.push_str(", ");
                }
                materialize_frontend_expression_with_precedence(arg, source, 0);
            }
            source.push(')');
        }
        FrontendExpression::Binary { op, lhs, rhs, .. } => {
            materialize_frontend_expression_with_precedence(lhs, source, precedence);
            source.push(' ');
            source.push_str(op.as_str());
            source.push(' ');
            let rhs_precedence = match op {
                FrontendBinaryOp::Sub => precedence + 1,
                FrontendBinaryOp::Add | FrontendBinaryOp::Mul => precedence,
            };
            materialize_frontend_expression_with_precedence(rhs, source, rhs_precedence);
        }
    }
    if needs_parens {
        source.push(')');
    }
}

const fn frontend_expression_precedence(expression: &FrontendExpression) -> u8 {
    match expression {
        FrontendExpression::Binary {
            op: FrontendBinaryOp::Add | FrontendBinaryOp::Sub,
            ..
        } => 1,
        FrontendExpression::Binary {
            op: FrontendBinaryOp::Mul,
            ..
        } => 2,
        FrontendExpression::Integer { .. }
        | FrontendExpression::Name { .. }
        | FrontendExpression::Call { .. } => 3,
    }
}

/// Projects frontend forms into inert source data for dumps and EOP records.
#[must_use]
pub fn frontend_forms_source_data(forms: &[FrontendForm]) -> SourceData {
    SourceData::list(forms.iter().map(frontend_form_source_data).collect())
}

/// Returns the stable frontend form kind token for one emitted form.
#[must_use]
pub fn frontend_form_kind_name(form: &FrontendForm) -> &'static str {
    match &form.kind {
        FrontendFormKind::Function(_) => "frontend-function",
        FrontendFormKind::Class(_) => "frontend-class",
    }
}

pub(super) fn frontend_form_source_data(form: &FrontendForm) -> SourceData {
    match &form.kind {
        FrontendFormKind::Function(function) => SourceData::record(
            "frontend-function",
            vec![
                source_data::field("name", SourceData::symbol(function.name.as_str())),
                source_span_data(form.source_span),
                source_data::field("body", frontend_expression_source_data(&function.body)),
            ],
        ),
        FrontendFormKind::Class(class) => SourceData::record(
            "frontend-class",
            vec![
                source_data::field("name", SourceData::symbol(class.name.as_str())),
                source_data::field("superclass", SourceData::symbol(class.superclass.as_str())),
                source_span_data(form.source_span),
                source_data::field(
                    "slots",
                    SourceData::list(class.slots.iter().map(frontend_slot_source_data).collect()),
                ),
            ],
        ),
    }
}

pub(super) fn frontend_expression_source_data(expression: &FrontendExpression) -> SourceData {
    match expression {
        FrontendExpression::Integer { value, source_span } => SourceData::record(
            "frontend-integer",
            vec![
                source_data::field("value", SourceData::integer(*value)),
                source_span_data(*source_span),
            ],
        ),
        FrontendExpression::Name { name, source_span } => SourceData::record(
            "frontend-name",
            vec![
                source_data::field("name", SourceData::symbol(name.as_str())),
                source_span_data(*source_span),
            ],
        ),
        FrontendExpression::Call {
            callee,
            args,
            source_span,
        } => SourceData::record(
            "frontend-call",
            vec![
                source_data::field("callee", SourceData::symbol(callee.as_str())),
                source_data::field(
                    "args",
                    SourceData::list(args.iter().map(frontend_expression_source_data).collect()),
                ),
                source_span_data(*source_span),
            ],
        ),
        FrontendExpression::Binary {
            op,
            lhs,
            rhs,
            source_span,
        } => SourceData::record(
            "frontend-binary",
            vec![
                source_data::field("op", SourceData::symbol(op.as_str())),
                source_data::field("lhs", frontend_expression_source_data(lhs)),
                source_data::field("rhs", frontend_expression_source_data(rhs)),
                source_span_data(*source_span),
            ],
        ),
    }
}

pub(super) fn frontend_slot_source_data(slot: &FrontendSlot) -> SourceData {
    let init_keyword = slot
        .init_keyword
        .as_deref()
        .map_or_else(|| SourceData::symbol("none"), SourceData::symbol);
    SourceData::record(
        "frontend-slot",
        vec![
            source_data::field("name", SourceData::symbol(slot.name.as_str())),
            source_data::field("type", SourceData::symbol(slot.type_name.as_str())),
            source_data::field("constant", SourceData::bool(slot.constant)),
            source_data::field("init-keyword", init_keyword),
            source_data::field(
                "required-init-keyword",
                SourceData::bool(slot.required_init_keyword),
            ),
            source_span_data(slot.source_span),
        ],
    )
}

fn source_span_data(span: impl Into<Span>) -> source_data::SourceDataField {
    let span = span.into();
    source_data::field(
        "source-span",
        SourceData::record(
            "span",
            vec![
                source_data::field(
                    "start",
                    SourceData::integer(usize_to_source_i64(span.start)),
                ),
                source_data::field("end", SourceData::integer(usize_to_source_i64(span.end))),
            ],
        ),
    )
}

fn parse_staged_file(
    source_file: SourceFileId,
    source: &str,
    registry: &GrammarFrontendRegistry,
) -> Option<GrammarParseReport> {
    let discovery = discover_staged_regions(source, registry);
    if discovery.regions.is_empty() {
        if !discovery.diagnostics.is_empty() {
            let mut report = parse_file_with_records(source_file, source, base_loko_records());
            report.diagnostics.extend(discovery.diagnostics);
            if let Some(activation) = report.activations.first() {
                report.grammar_diagnostics = grammar_diagnostic_facts(
                    source,
                    &report.diagnostics,
                    activation,
                    &report.effective_rules,
                    &report.provenance.applications,
                );
                report.provenance.diagnostics = report.grammar_diagnostics.clone();
            }
            return Some(report);
        }
        return None;
    }
    let adapter_names = staged_adapter_names(&discovery.regions);
    let records = registry.staged_records(&adapter_names);
    let base_grammar = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let base_root_rule =
        effective_rule_id_by_name(&records.effective_rules, base_grammar, "source-file")
            .unwrap_or_else(|| GrammarRuleId::new(base_grammar, "effective:source-file"));

    let base_activation = GrammarActivation {
        id: GrammarActivationId::new(source_file, base_grammar, "source-file"),
        environment: records.environment.id,
        grammar: base_grammar,
        grammar_slug: BASE_LOKO_ENVIRONMENT_SLUG.to_string(),
        object_slug: "source-file".to_string(),
        span: Span::new(0, source.len()),
        root_rule: base_root_rule,
        reason: "base-loko source parse with staged grammar activations".to_string(),
    };

    let mut diagnostics = discovery.diagnostics;
    let mut activations = vec![base_activation];
    let mut staged_regions = Vec::new();
    let mut extension_activations = Vec::new();
    let mut extension_emissions = Vec::new();
    let mut staged_rule_counts = Vec::new();

    for (index, region) in discovery.regions.iter().enumerate() {
        let grammar = GrammarId::new(region.adapter.grammar_name());
        let root_rule = effective_rule_id_by_name(
            &records.effective_rules,
            grammar,
            region.adapter.root_rule(),
        )
        .unwrap_or_else(|| {
            GrammarRuleId::new(
                grammar,
                &format!("effective:{}", region.adapter.root_rule()),
            )
        });
        let object_slug = format!("r{index}");
        let activation_name = format!("{}-{object_slug}", region.adapter.adapter_name());
        let activation = GrammarActivation {
            id: GrammarActivationId::new(source_file, grammar, &activation_name),
            environment: records.environment.id,
            grammar,
            grammar_slug: region.adapter.grammar_slug().to_string(),
            object_slug: object_slug.clone(),
            span: region.body_span,
            root_rule,
            reason: format!(
                "{} staged region emitted frontend forms for direct AST lowering",
                region.adapter.adapter_name()
            ),
        };
        let emission = region
            .adapter
            .parse_region(region.body, region.body_span.start);
        diagnostics.extend(emission.diagnostics);
        let region_materialized_source = materialize_frontend_forms(&emission.forms);
        staged_rule_counts.extend(
            emission
                .rule_counts
                .iter()
                .map(|count| (grammar, count.rule_name.clone(), count.node_count)),
        );
        let staged_rule_applications = staged_rule_applications(
            grammar,
            &records.effective_rules,
            &activation,
            &emission.rule_applications,
        );
        let forms_data = frontend_forms_source_data(&emission.forms);

        extension_activations.push(GrammarExtensionActivation {
            adapter: region.adapter.adapter_name().to_string(),
            activation: activation.id,
            grammar,
            grammar_slug: region.adapter.grammar_slug().to_string(),
            object_slug: object_slug.clone(),
            activation_directive: region.activation_directive,
            start_marker: region.start_marker,
            end_marker: region.end_marker,
            source_span: region.body_span,
            capabilities: region.adapter.required_capabilities().to_vec(),
            emitted_form_count: emission.forms.len(),
        });
        for (form_index, form) in emission.forms.iter().enumerate() {
            extension_emissions.push(GrammarExtensionEmission {
                adapter: region.adapter.adapter_name().to_string(),
                activation: activation.id,
                object_slug: object_slug.clone(),
                form_index,
                form_kind: frontend_form_kind_name(form).to_string(),
                source_span: form.source_span,
                data: frontend_form_source_data(form),
            });
        }

        activations.push(activation.clone());
        staged_regions.push(StagedParseRegion {
            object_slug,
            adapter: region.adapter.adapter_name().to_string(),
            activation: activation.id,
            grammar,
            grammar_slug: region.adapter.grammar_slug().to_string(),
            driver: emission.driver,
            oracle: emission.oracle,
            activation_directive: region.activation_directive,
            source_span: region.body_span,
            forms: emission.forms,
            forms_data,
            materialized_source: region_materialized_source,
            data: emission.data,
            rule_applications: staged_rule_applications,
        });
    }

    let parser_source = source_with_staged_regions_blanked(source, &discovery.regions);
    let staged_grammar = interpreter::compile_base_loko_grammar();
    let (mut syntax, parse_tree, parse_tree_data) =
        match base_loko_primary::parse_file(&staged_grammar, source_file, &parser_source) {
            Ok(success) => (
                success.syntax,
                Some(success.parse_tree),
                Some(success.parse_tree_data),
            ),
            Err(diagnostic) => (
                syntax_tree_with_diagnostic(source_file, &parser_source, diagnostic),
                None,
                None,
            ),
        };
    syntax.diagnostics.extend(diagnostics);

    let active_effective_rules = records.effective_rules.to_vec();
    let applications = rule_applications(&active_effective_rules, &activations[0], &syntax.root);
    let mut provenance_rules = rule_provenance(&active_effective_rules, &applications);
    for (grammar, rule_name, count) in staged_rule_counts {
        add_staged_rule_count(
            &mut provenance_rules,
            &records.effective_rules,
            grammar,
            &rule_name,
            count,
        );
    }
    let extension_provenance = extension_provenance_records(
        &staged_regions,
        &records.effective_rules,
        &provenance_rules,
        registry,
    );

    let grammar_stack = GrammarStack {
        environment: records.environment.id,
        grammars: records.environment.grammar_stack.clone(),
    };
    let diagnostic_activation = activations
        .last()
        .expect("staged reports always include at least a base activation");
    let grammar_diagnostics = grammar_diagnostic_facts(
        source,
        &syntax.diagnostics,
        diagnostic_activation,
        &active_effective_rules,
        &applications,
    );
    let frontend_parse = FrontendParseArtifact {
        source_file,
        environment: records.environment.id,
        grammar: base_grammar,
        activation: activations[0].id,
        root_rule: base_root_rule,
        source: FrontendParseSource::StagedRegionsBlanked,
        source_span: Span::new(0, source.len()),
        parse_tree,
        parse_tree_data,
        cst_node_count: cst_node_count(&syntax.root),
        diagnostic_count: syntax.diagnostics.len(),
    };
    let provenance = ParseProvenance {
        id: ParseProvenanceId::new(source_file, records.environment.id),
        source_file,
        environment: records.environment.id,
        grammar_stack: grammar_stack.clone(),
        activations: activations.iter().map(|activation| activation.id).collect(),
        applications,
        rules: provenance_rules,
        diagnostics: grammar_diagnostics.clone(),
    };

    Some(GrammarParseReport {
        source_file,
        environment: records.environment,
        grammars: records.grammars,
        rules: records.rules,
        effective_rules: records.effective_rules,
        grammar_stack,
        activations,
        staged_regions,
        extension_activations,
        extension_emissions,
        extension_provenance,
        provenance,
        grammar_diagnostics,
        frontend_parse,
        diagnostics: syntax.diagnostics.clone(),
        syntax,
    })
}

fn source_with_staged_regions_blanked(
    source: &str,
    regions: &[DiscoveredStagedRegion<'_>],
) -> String {
    let mut parser_source = String::with_capacity(source.len());
    let mut cursor = 0_usize;
    for region in regions {
        parser_source.push_str(&source[cursor..region.start_marker.start]);
        push_blank_preserving_newlines(
            &mut parser_source,
            &source[region.start_marker.start..region.end_marker.end],
        );
        cursor = region.end_marker.end;
    }
    parser_source.push_str(&source[cursor..]);
    parser_source
}

fn push_blank_preserving_newlines(output: &mut String, source: &str) {
    for ch in source.chars() {
        if ch == '\n' || ch == '\r' {
            output.push(ch);
        } else {
            output.push(' ');
        }
    }
}

struct DiscoveredStagedRegion<'source> {
    adapter: &'static dyn StagedGrammarAdapter,
    activation_directive: Span,
    start_marker: Span,
    end_marker: Span,
    body: &'source str,
    body_span: Span,
}

struct StagedRegionDiscovery<'source> {
    regions: Vec<DiscoveredStagedRegion<'source>>,
    diagnostics: Vec<Diagnostic>,
}

struct ActiveStagedGrammar {
    adapter: &'static dyn StagedGrammarAdapter,
    directive: Span,
}

struct SourceLine<'source> {
    start: usize,
    end: usize,
    text: &'source str,
}

/// Output emitted by a staged grammar adapter for one source region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedRegionEmission {
    /// Frontend forms emitted by the adapter.
    pub forms: Vec<FrontendForm>,
    /// Parser driver used to produce this emission.
    pub driver: StagedParseDriver,
    /// Parser oracle/fallback status for this emission.
    pub oracle: StagedParseOracle,
    /// Inert structured protocol data describing the adapter parse.
    pub data: SourceData,
    /// Rule counts charged by the staged grammar parser.
    pub rule_counts: Vec<StagedRuleCount>,
    /// Rule applications recorded by the staged grammar parser.
    pub rule_applications: Vec<StagedRuleApplication>,
    /// Diagnostics emitted while parsing the staged region.
    pub diagnostics: Vec<Diagnostic>,
}

/// One staged grammar rule application emitted before grammar ids are resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedRuleApplication {
    /// Stable grammar rule name.
    pub rule_name: String,
    /// Source span covered by this rule application.
    pub span: Span,
}

/// Number of parser nodes charged to one staged grammar rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedRuleCount {
    /// Stable grammar rule name.
    pub rule_name: String,
    /// Number of parse tree nodes for this rule.
    pub node_count: usize,
}

struct BaseLokoEnvironmentProvider;
struct DerivedHostEnvironmentProvider;
struct CalculatorStagedGrammar;
struct RecordsStagedGrammar;

static BASE_LOKO_ENVIRONMENT_PROVIDER: BaseLokoEnvironmentProvider = BaseLokoEnvironmentProvider;
static DERIVED_HOST_ENVIRONMENT_PROVIDER: DerivedHostEnvironmentProvider =
    DerivedHostEnvironmentProvider;
static CALCULATOR_STAGED_GRAMMAR: CalculatorStagedGrammar = CalculatorStagedGrammar;
static RECORDS_STAGED_GRAMMAR: RecordsStagedGrammar = RecordsStagedGrammar;
static BUILTIN_GRAMMAR_ENVIRONMENTS: [&dyn GrammarEnvironmentProvider; 2] = [
    &BASE_LOKO_ENVIRONMENT_PROVIDER,
    &DERIVED_HOST_ENVIRONMENT_PROVIDER,
];
static BUILTIN_STAGED_GRAMMAR_ADAPTERS: [&dyn StagedGrammarAdapter; 2] =
    [&CALCULATOR_STAGED_GRAMMAR, &RECORDS_STAGED_GRAMMAR];
const CALCULATOR_PROVENANCE_RULES: &[&str] = &["calculator-region", "calculator-function"];
const RECORDS_PROVENANCE_RULES: &[&str] = &["records-region", "record-definition", "record-field"];

impl GrammarEnvironmentProvider for BaseLokoEnvironmentProvider {
    fn environment_slug(&self) -> &'static str {
        BASE_LOKO_ENVIRONMENT_SLUG
    }

    fn environment_name(&self) -> &'static str {
        BASE_LOKO_ENVIRONMENT_NAME
    }

    fn records(&self) -> GrammarFrontendRecords {
        base_loko_records()
    }

    fn discovers_staged_regions(&self) -> bool {
        true
    }
}

impl GrammarEnvironmentProvider for DerivedHostEnvironmentProvider {
    fn environment_slug(&self) -> &'static str {
        DERIVED_HOST_ENVIRONMENT_SLUG
    }

    fn environment_name(&self) -> &'static str {
        DERIVED_HOST_ENVIRONMENT_NAME
    }

    fn records(&self) -> GrammarFrontendRecords {
        derived_host_records()
    }
}

impl StagedGrammarAdapter for CalculatorStagedGrammar {
    fn adapter_name(&self) -> &'static str {
        CALCULATOR_GRAMMAR_SLUG
    }

    fn grammar_name(&self) -> &'static str {
        CALCULATOR_GRAMMAR_NAME
    }

    fn grammar_slug(&self) -> &'static str {
        CALCULATOR_GRAMMAR_SLUG
    }

    fn root_rule(&self) -> &'static str {
        "calculator-region"
    }

    fn provenance_rule_names(&self) -> &'static [&'static str] {
        CALCULATOR_PROVENANCE_RULES
    }

    fn accepted_region(&self) -> &'static str {
        "`name = expression` calculator function lines"
    }

    fn emitted_form_kinds(&self) -> &'static [&'static str] {
        CALCULATOR_EMITTED_FORM_KINDS
    }

    fn direct_rules(&self, grammar: GrammarId) -> Vec<GrammarRule> {
        calculator::rules(grammar, self.grammar_slug())
    }

    fn parse_region(&self, body: &str, source_offset: usize) -> StagedRegionEmission {
        calculator::parse_region(body, source_offset)
    }
}

impl StagedGrammarAdapter for RecordsStagedGrammar {
    fn adapter_name(&self) -> &'static str {
        RECORDS_GRAMMAR_SLUG
    }

    fn grammar_name(&self) -> &'static str {
        RECORDS_GRAMMAR_NAME
    }

    fn grammar_slug(&self) -> &'static str {
        RECORDS_GRAMMAR_SLUG
    }

    fn root_rule(&self) -> &'static str {
        "records-region"
    }

    fn provenance_rule_names(&self) -> &'static [&'static str] {
        RECORDS_PROVENANCE_RULES
    }

    fn accepted_region(&self) -> &'static str {
        "`record name` blocks with typed fields"
    }

    fn emitted_form_kinds(&self) -> &'static [&'static str] {
        RECORDS_EMITTED_FORM_KINDS
    }

    fn direct_rules(&self, grammar: GrammarId) -> Vec<GrammarRule> {
        records::rules(grammar, self.grammar_slug())
    }

    fn parse_region(&self, body: &str, source_offset: usize) -> StagedRegionEmission {
        records::parse_region(body, source_offset)
    }
}

fn discover_staged_regions<'source>(
    source: &'source str,
    registry: &GrammarFrontendRegistry,
) -> StagedRegionDiscovery<'source> {
    let mut regions = Vec::new();
    let mut diagnostics = Vec::new();
    let mut active = None;
    let mut offset = 0_usize;

    while let Some(line) = next_source_line(source, offset) {
        let trimmed = line.text.trim();
        if let Some(name) = trimmed.strip_prefix(GRAMMAR_ACTIVATE_PREFIX) {
            let directive_span = Span::new(line.start, line.end);
            let name = name.trim();
            match registry.staged_adapter(name) {
                Some(adapter) => {
                    active = Some(ActiveStagedGrammar {
                        adapter,
                        directive: directive_span,
                    });
                }
                None => {
                    active = None;
                    diagnostics.push(unknown_staged_grammar_diagnostic(directive_span, registry));
                }
            }
            offset = line.end;
            continue;
        }

        if let Some(name) = trimmed.strip_prefix(STAGED_LOKO_PREFIX) {
            let name = name.trim();
            let start_marker = Span::new(line.start, line.end);
            let Some(adapter) = registry.staged_adapter(name) else {
                diagnostics.push(unknown_staged_grammar_diagnostic(start_marker, registry));
                offset = line.end;
                continue;
            };
            let activation_directive = match &active {
                Some(active) if active.adapter.adapter_name() == adapter.adapter_name() => {
                    active.directive
                }
                _ => {
                    diagnostics.push(missing_activation_diagnostic(start_marker, name));
                    start_marker
                }
            };
            let body_start = line.end;
            let (body_end, end_marker, next_offset) =
                find_staged_region_end(source, body_start, &mut diagnostics);
            regions.push(DiscoveredStagedRegion {
                adapter,
                activation_directive,
                start_marker,
                end_marker,
                body: &source[body_start..body_end],
                body_span: Span::new(body_start, body_end),
            });
            offset = next_offset;
            continue;
        }

        offset = line.end;
    }

    StagedRegionDiscovery {
        regions,
        diagnostics,
    }
}

fn next_source_line(source: &str, offset: usize) -> Option<SourceLine<'_>> {
    if offset >= source.len() {
        return None;
    }
    let rest = &source[offset..];
    let line_len = rest.find('\n').map_or(rest.len(), |index| index + 1);
    Some(SourceLine {
        start: offset,
        end: offset + line_len,
        text: &source[offset..offset + line_len],
    })
}

fn find_staged_region_end(
    source: &str,
    body_start: usize,
    diagnostics: &mut Vec<Diagnostic>,
) -> (usize, Span, usize) {
    let mut offset = body_start;
    while let Some(line) = next_source_line(source, offset) {
        if line.text.trim() == STAGED_LOKO_END {
            return (line.start, Span::new(line.start, line.end), line.end);
        }
        offset = line.end;
    }
    let span = Span::new(source.len(), source.len());
    diagnostics.push(expected_syntax_diagnostic(span, STAGED_LOKO_END));
    (source.len(), span, source.len())
}

fn staged_adapter_names(regions: &[DiscoveredStagedRegion<'_>]) -> Vec<&'static str> {
    let mut names = Vec::new();
    for region in regions {
        let name = region.adapter.adapter_name();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

fn unknown_staged_grammar_diagnostic(span: Span, registry: &GrammarFrontendRegistry) -> Diagnostic {
    expected_syntax_diagnostic(
        span,
        &format!(
            "registered staged grammar ({})",
            registry.staged_adapter_names().join(", ")
        ),
    )
}

fn missing_activation_diagnostic(span: Span, name: &str) -> Diagnostic {
    Diagnostic::error(span, DiagnosticCode::SyntaxExpectedToken).with_arg(DiagnosticArg::text(
        "expected",
        format!("{GRAMMAR_ACTIVATE_PREFIX} {name}"),
    ))
}

fn expected_syntax_diagnostic(span: Span, expected: &str) -> Diagnostic {
    Diagnostic::error(span, DiagnosticCode::SyntaxExpectedToken)
        .with_arg(DiagnosticArg::text("expected", expected))
}

fn unknown_environment_diagnostic(
    span: Span,
    environment: &str,
    registry: &GrammarFrontendRegistry,
) -> Diagnostic {
    Diagnostic::error(span, DiagnosticCode::SyntaxExpectedToken)
        .with_arg(DiagnosticArg::text(
            "expected",
            format!(
                "registered grammar environment ({})",
                registry.environment_slugs().join(", ")
            ),
        ))
        .with_arg(DiagnosticArg::text("environment", environment))
}

fn staged_expected_diagnostic(span: Span, adapter: &str, expected: &str) -> Diagnostic {
    expected_syntax_diagnostic(span, expected).with_arg(DiagnosticArg::text("adapter", adapter))
}

fn staged_action_error_diagnostic(error: actions::GrammarActionError, adapter: &str) -> Diagnostic {
    let mut diagnostic = staged_expected_diagnostic(error.span, adapter, error.expected.as_ref());
    if let Some(rule) = error.rule {
        diagnostic = diagnostic.with_arg(DiagnosticArg::text("rule", rule.to_string()));
    }
    if let Some(action) = error.action {
        diagnostic = diagnostic.with_arg(DiagnosticArg::text("action", action.to_string()));
    }
    if let Some(arg) = error.arg {
        diagnostic = diagnostic.with_arg(DiagnosticArg::text("argument", arg.to_string()));
    }
    if let Some(field) = error.field {
        diagnostic = diagnostic.with_arg(DiagnosticArg::text("field", field.to_string()));
    }
    diagnostic
}

fn push_report_diagnostic(report: &mut GrammarParseReport, diagnostic: Diagnostic) {
    report.diagnostics.push(diagnostic);
    if let Some(activation) = report.activations.first() {
        report.grammar_diagnostics = grammar_diagnostic_facts(
            &report.syntax.source,
            &report.diagnostics,
            activation,
            &report.effective_rules,
            &report.provenance.applications,
        );
        report.provenance.diagnostics = report.grammar_diagnostics.clone();
    }
}

fn valid_calculator_function_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'?'))
}

fn valid_type_name(name: &str) -> bool {
    let Some(inner) = name
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    else {
        return false;
    };
    valid_record_type_fragment(inner)
}

fn valid_record_type_fragment(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_record_field_name(name: &str) -> bool {
    valid_calculator_function_name(name)
}

fn add_staged_rule_count(
    rules: &mut [GrammarRuleProvenance],
    effective_rules: &[EffectiveGrammarRule],
    grammar: GrammarId,
    name: &str,
    count: usize,
) {
    let Some(rule) = effective_rules
        .iter()
        .find(|rule| rule.grammar == grammar && rule.name == name)
    else {
        return;
    };
    if let Some(provenance) = rules.iter_mut().find(|item| item.rule == rule.id) {
        provenance.node_count = provenance.node_count.saturating_add(count);
    }
}

fn staged_rule_applications(
    grammar: GrammarId,
    effective_rules: &[EffectiveGrammarRule],
    activation: &GrammarActivation,
    applications: &[StagedRuleApplication],
) -> Vec<GrammarRuleApplication> {
    applications
        .iter()
        .map(|application| GrammarRuleApplication {
            activation: activation.id,
            grammar,
            rule: effective_rule_id_by_name(effective_rules, grammar, &application.rule_name)
                .unwrap_or_else(|| {
                    GrammarRuleId::new(grammar, &format!("effective:{}", application.rule_name))
                }),
            rule_name: application.rule_name.clone(),
            span: application.span,
        })
        .collect()
}

fn extension_provenance_records(
    regions: &[StagedParseRegion],
    effective_rules: &[EffectiveGrammarRule],
    provenance_rules: &[GrammarRuleProvenance],
    registry: &GrammarFrontendRegistry,
) -> Vec<GrammarExtensionProvenance> {
    let mut records = Vec::new();
    for region in regions {
        let Some(adapter) = registry.staged_adapter(&region.adapter) else {
            continue;
        };
        for rule_name in adapter.provenance_rule_names() {
            let Some(rule) = effective_rules
                .iter()
                .find(|rule| rule.grammar == region.grammar && rule.name == *rule_name)
            else {
                continue;
            };
            let node_count = provenance_rules
                .iter()
                .find(|provenance| provenance.rule == rule.id)
                .map_or(0, |provenance| provenance.node_count);
            records.push(GrammarExtensionProvenance {
                adapter: region.adapter.clone(),
                activation: region.activation,
                object_slug: region.object_slug.clone(),
                grammar: region.grammar,
                rule: rule.id,
                rule_name: (*rule_name).to_string(),
                node_count,
            });
        }
    }
    records
}

fn usize_to_source_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Returns the projected base Loko grammar records without parsing source.
#[must_use]
pub fn base_loko_records() -> GrammarFrontendRecords {
    let environment_id = LanguageEnvironmentId::new(BASE_LOKO_ENVIRONMENT_NAME);
    let grammar_id = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let rules = base_loko_rules(grammar_id, BASE_LOKO_ENVIRONMENT_SLUG);
    let direct_rules = rules.iter().map(|rule| rule.id).collect();
    let effective_rules = direct_effective_rules(grammar_id, BASE_LOKO_ENVIRONMENT_SLUG, &rules);
    let effective_rule_ids = effective_rules.iter().map(|rule| rule.id).collect();
    let grammar = Grammar {
        id: grammar_id,
        slug: BASE_LOKO_ENVIRONMENT_SLUG.to_string(),
        name: BASE_LOKO_GRAMMAR_NAME.to_string(),
        extends: Vec::new(),
        direct_rules,
        effective_rules: effective_rule_ids,
    };
    let environment = LanguageEnvironment {
        id: environment_id,
        slug: BASE_LOKO_ENVIRONMENT_SLUG.to_string(),
        name: BASE_LOKO_ENVIRONMENT_NAME.to_string(),
        grammar_stack: vec![grammar_id],
    };
    GrammarFrontendRecords {
        environment,
        active_grammar: grammar_id,
        grammars: vec![grammar],
        rules,
        effective_rules,
    }
}

/// Slug used by `derived_host.grammar`'s `grammar` header.
const DERIVED_HOST_GRAMMAR_HEADER_NAME: &str = "derived-host";
/// Slug used in `derived_host.grammar`'s `extends` clause.
const DERIVED_HOST_PARENT_HEADER_NAME: &str = "base-loko";

/// Returns the trusted host-derived grammar records without parsing source.
///
/// The grammar source lives in `derived_host.grammar` (loaded via
/// `interpreter::DERIVED_HOST_GRAMMAR_SOURCE`); the hand-rolled
/// `function-definition` override that appears in the records is
/// still a fixture marked [`ProjectionOrigin::TrustedHostFixture`].
/// Lifting that override into the source file needs a `super`-style
/// parent-rule reference (the override's body is a self-delegation,
/// which under normal inheritance would recurse into itself).
#[must_use]
pub fn derived_host_records() -> GrammarFrontendRecords {
    let parsed =
        interloko_grammar::parse_grammar_source_full(interpreter::DERIVED_HOST_GRAMMAR_SOURCE);
    assert!(
        parsed.diagnostics.is_empty(),
        "derived_host.grammar produced source diagnostics: {:?}",
        parsed.diagnostics,
    );
    assert_eq!(
        parsed.header.name,
        Some(DERIVED_HOST_GRAMMAR_HEADER_NAME),
        "derived_host.grammar must declare `grammar derived-host`",
    );
    assert_eq!(
        parsed.header.extends,
        Some(DERIVED_HOST_PARENT_HEADER_NAME),
        "derived_host.grammar must declare `extends base-loko`",
    );

    let base_environment = base_loko_records();
    let base_grammar_id = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let derived_environment_id = LanguageEnvironmentId::new(DERIVED_HOST_ENVIRONMENT_NAME);
    let derived_grammar_id = GrammarId::new(DERIVED_HOST_GRAMMAR_NAME);
    let rules = base_environment.rules;
    let mut grammars = base_environment.grammars;
    let base_effective_rules = base_environment.effective_rules;
    let derived_effective_rules = derived_effective_rules(
        base_grammar_id,
        derived_grammar_id,
        DERIVED_HOST_ENVIRONMENT_SLUG,
        &rules,
    );
    let derived_effective_rule_ids = derived_effective_rules
        .iter()
        .map(|rule| rule.id)
        .collect::<Vec<_>>();
    grammars.push(Grammar {
        id: derived_grammar_id,
        slug: DERIVED_HOST_ENVIRONMENT_SLUG.to_string(),
        name: DERIVED_HOST_GRAMMAR_NAME.to_string(),
        extends: vec![base_grammar_id],
        direct_rules: Vec::new(),
        effective_rules: derived_effective_rule_ids,
    });
    let mut effective_rules = base_effective_rules;
    effective_rules.extend(derived_effective_rules);
    let environment = LanguageEnvironment {
        id: derived_environment_id,
        slug: DERIVED_HOST_ENVIRONMENT_SLUG.to_string(),
        name: DERIVED_HOST_ENVIRONMENT_NAME.to_string(),
        grammar_stack: vec![base_grammar_id, derived_grammar_id],
    };
    GrammarFrontendRecords {
        environment,
        active_grammar: derived_grammar_id,
        grammars,
        rules,
        effective_rules,
    }
}

/// Returns the directive-activated staged calculator records without parsing source.
#[must_use]
pub fn staged_calculator_records() -> GrammarFrontendRecords {
    GrammarFrontendRegistry::builtin().staged_records(&[CALCULATOR_GRAMMAR_SLUG])
}

fn staged_registry_records(
    adapter_names: &[&str],
    registry: &GrammarFrontendRegistry,
) -> GrammarFrontendRecords {
    let base_environment = base_loko_records();
    let base_grammar_id = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let mut rules = base_environment.rules;
    let mut grammars = base_environment.grammars;
    let mut effective_rules = base_environment.effective_rules;
    let mut grammar_stack = vec![base_grammar_id];
    let mut active_grammar = base_grammar_id;

    for adapter_name in adapter_names {
        let Some(adapter) = registry.staged_adapter(adapter_name) else {
            continue;
        };
        let grammar_id = GrammarId::new(adapter.grammar_name());
        if grammar_stack.contains(&grammar_id) {
            active_grammar = grammar_id;
            continue;
        }
        let adapter_rules = adapter.direct_rules(grammar_id);
        let adapter_direct_rule_ids = adapter_rules.iter().map(|rule| rule.id).collect();
        rules.extend(adapter_rules);
        let adapter_effective_rules =
            staged_effective_rules(base_grammar_id, grammar_id, adapter.grammar_slug(), &rules);
        let adapter_effective_rule_ids = adapter_effective_rules
            .iter()
            .map(|rule| rule.id)
            .collect::<Vec<_>>();
        grammars.push(Grammar {
            id: grammar_id,
            slug: adapter.grammar_slug().to_string(),
            name: adapter.grammar_name().to_string(),
            extends: vec![base_grammar_id],
            direct_rules: adapter_direct_rule_ids,
            effective_rules: adapter_effective_rule_ids,
        });
        effective_rules.extend(adapter_effective_rules);
        grammar_stack.push(grammar_id);
        active_grammar = grammar_id;
    }

    let environment_id = LanguageEnvironmentId::new(STAGED_LOKO_ENVIRONMENT_NAME);
    let environment = LanguageEnvironment {
        id: environment_id,
        slug: STAGED_LOKO_ENVIRONMENT_SLUG.to_string(),
        name: STAGED_LOKO_ENVIRONMENT_NAME.to_string(),
        grammar_stack,
    };
    GrammarFrontendRecords {
        environment,
        active_grammar,
        grammars,
        rules,
        effective_rules,
    }
}

/// Projected grammar frontend records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarFrontendRecords {
    /// Language environment.
    pub environment: LanguageEnvironment,
    /// Active grammar id.
    pub active_grammar: GrammarId,
    /// Projected grammars.
    pub grammars: Vec<Grammar>,
    /// Projected direct rules.
    pub rules: Vec<GrammarRule>,
    /// Projected effective rules.
    pub effective_rules: Vec<EffectiveGrammarRule>,
}

fn base_loko_rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    const RULES: &[(&str, &str, &str)] = &[
        ("source-file", "definition*", "syntax.rs:Parser::parse"),
        (
            "definition",
            "class-definition | function-definition | generic-definition | method-definition | constant-definition",
            "syntax.rs:Parser::parse",
        ),
        (
            "class-definition",
            "`define class` type-name superclass? slot-declaration* `end`",
            "syntax.rs:Parser::parse_class",
        ),
        (
            "slot-declaration",
            "(`slot` | `constant slot`) name type-annotation? slot-option* `;`?",
            "syntax.rs:Parser::parse_slot_decl",
        ),
        (
            "slot-option",
            "init-keyword | required-init-keyword | init-value",
            "syntax.rs:Parser::parse_slot_option",
        ),
        (
            "generic-definition",
            "`define generic` name parameter-list result-annotation? `;`?",
            "syntax.rs:Parser::parse_generic",
        ),
        (
            "method-definition",
            "`define method` name parameter-list result-annotation? body `end`",
            "syntax.rs:Parser::parse_method",
        ),
        (
            "function-definition",
            "`define function` name parameter-list result-annotation? body `end`",
            "syntax.rs:Parser::parse_function",
        ),
        (
            "constant-definition",
            "`define constant` name type-annotation? `=` expression `;`",
            "base_loko_primary::BaseLokoCstBuilder::build_constant",
        ),
        (
            "parameter-list",
            "`(` parameter* `)`",
            "syntax.rs:Parser::parse_parameter_list",
        ),
        ("body", "let* expression", "syntax.rs:Parser::parse_body"),
        (
            "let-statement",
            "`let` name type-annotation? `=` expression `;`",
            "syntax.rs:Parser::parse_let",
        ),
        (
            "expression",
            "handle-expression | if-expression | assignment-expression",
            "syntax.rs:Parser::parse_expr",
        ),
        (
            "handle-expression",
            "`handle` expression `with restart` binding type-annotation expression `end`",
            "syntax.rs:Parser::parse_handle",
        ),
        (
            "restart-clause",
            "`with restart` name type-annotation expression",
            "syntax.rs:Parser::parse_restart_clause",
        ),
        (
            "if-expression",
            "`if` expression `then` expression `else` expression `end`",
            "syntax.rs:Parser::parse_if",
        ),
        (
            "assignment-expression",
            "comparison-expression (`:=` assignment-expression)?",
            "syntax.rs:Parser::parse_assignment",
        ),
        (
            "comparison-expression",
            "additive-expression comparison-op additive-expression?",
            "syntax.rs:Parser::parse_compare",
        ),
        (
            "additive-expression",
            "multiplicative-expression ((`+` | `-`) multiplicative-expression)*",
            "syntax.rs:Parser::parse_add",
        ),
        (
            "multiplicative-expression",
            "unary-expression (`*` unary-expression)*",
            "syntax.rs:Parser::parse_mul",
        ),
        (
            "unary-expression",
            "`-` unary-expression | postfix-expression",
            "syntax.rs:Parser::parse_unary",
        ),
        (
            "postfix-expression",
            "primary-expression (`.` name)*",
            "syntax.rs:Parser::parse_postfix",
        ),
        (
            "primary-expression",
            "literal | name | type-name | call | parenthesized | vector",
            "syntax.rs:Parser::parse_primary",
        ),
        (
            "vector-expression",
            "`#[` expression* `]`",
            "syntax.rs:Parser::parse_vector_literal",
        ),
        (
            "argument-list",
            "`(` (keyword-argument | expression)* `)`",
            "syntax.rs:Parser::parse_arg_list",
        ),
        (
            "keyword-argument",
            "keyword-name `:` expression",
            "syntax.rs:Parser::parse_keyword_arg",
        ),
    ];

    RULES
        .iter()
        .map(|(name, expression, location)| GrammarRule {
            id: GrammarRuleId::new(grammar, name),
            grammar,
            grammar_slug: grammar_slug.to_string(),
            name: (*name).to_string(),
            expression: projected_rule_expression(expression),
            action: parse_grammar_action(
                expression
                    .split_once("=>")
                    .map_or("", |(_, output)| output.trim()),
            ),
            expression_data: grammar_rule_expression_data(name, expression, location),
            projection: GrammarProjection {
                origin: ProjectionOrigin::CompilerProjected,
                location: (*location).to_string(),
                source_span: None,
            },
        })
        .collect()
}

fn grammar_rule_expression_data(
    name: &str,
    expression: &'static str,
    location: &str,
) -> SourceData {
    let structured = projected_rule_expression(expression);
    grammar_rule_expression_data_from_expr(name, &structured, Some(expression), location)
}

fn grammar_rule_expression_data_from_expr(
    name: &str,
    expression: &RuleExpression<'_>,
    text: Option<&str>,
    location: &str,
) -> SourceData {
    let mut fields = vec![
        source_data::field("name", SourceData::symbol(name)),
        source_data::field("expression", rule_expression_source_data(expression)),
    ];
    if let Some(text) = text {
        fields.push(source_data::field("text", SourceData::string(text)));
    }
    fields.push(source_data::field("location", SourceData::string(location)));
    SourceData::record("grammar-rule", fields)
}

fn projected_rule_expression(expression: &'static str) -> RuleExpression<'static> {
    interloko_grammar::parse_rule_expression(expression)
}

fn parse_grammar_action(output: &'static str) -> Option<GrammarAction<'static>> {
    interloko_grammar::parse_grammar_action(output)
}

fn staged_effective_rules(
    base_grammar: GrammarId,
    staged_grammar: GrammarId,
    staged_slug: &str,
    rules: &[GrammarRule],
) -> Vec<EffectiveGrammarRule> {
    let mut effective_rules =
        derived_effective_rules(base_grammar, staged_grammar, staged_slug, rules);
    effective_rules.extend(
        rules
            .iter()
            .filter(|rule| rule.grammar == staged_grammar)
            .map(|rule| EffectiveGrammarRule {
                id: GrammarRuleId::new(staged_grammar, &format!("effective:{}", rule.name)),
                grammar: staged_grammar,
                grammar_slug: staged_slug.to_string(),
                name: rule.name.clone(),
                selected_rule: rule.id,
                selected_rule_grammar: rule.grammar,
                overridden_rule: None,
                resolution: EffectiveGrammarRuleResolution::Direct,
            }),
    );
    effective_rules
}

fn direct_effective_rules(
    grammar: GrammarId,
    grammar_slug: &str,
    direct_rules: &[GrammarRule],
) -> Vec<EffectiveGrammarRule> {
    direct_rules
        .iter()
        .map(|rule| EffectiveGrammarRule {
            id: GrammarRuleId::new(grammar, &format!("effective:{}", rule.name)),
            grammar,
            grammar_slug: grammar_slug.to_string(),
            name: rule.name.clone(),
            selected_rule: rule.id,
            selected_rule_grammar: rule.grammar,
            overridden_rule: None,
            resolution: EffectiveGrammarRuleResolution::Direct,
        })
        .collect()
}

fn derived_effective_rules(
    base_grammar: GrammarId,
    derived_grammar: GrammarId,
    derived_slug: &str,
    rules: &[GrammarRule],
) -> Vec<EffectiveGrammarRule> {
    let base_rules = rules
        .iter()
        .filter(|rule| rule.grammar == base_grammar)
        .collect::<Vec<_>>();
    let derived_rules = rules
        .iter()
        .filter(|rule| rule.grammar == derived_grammar)
        .collect::<Vec<_>>();
    base_rules
        .into_iter()
        .map(|base_rule| {
            if let Some(override_rule) = derived_rules
                .iter()
                .find(|candidate| candidate.name == base_rule.name)
            {
                EffectiveGrammarRule {
                    id: GrammarRuleId::new(
                        derived_grammar,
                        &format!("effective:{}", base_rule.name),
                    ),
                    grammar: derived_grammar,
                    grammar_slug: derived_slug.to_string(),
                    name: base_rule.name.clone(),
                    selected_rule: override_rule.id,
                    selected_rule_grammar: override_rule.grammar,
                    overridden_rule: Some(base_rule.id),
                    resolution: EffectiveGrammarRuleResolution::Override,
                }
            } else {
                EffectiveGrammarRule {
                    id: GrammarRuleId::new(
                        derived_grammar,
                        &format!("effective:{}", base_rule.name),
                    ),
                    grammar: derived_grammar,
                    grammar_slug: derived_slug.to_string(),
                    name: base_rule.name.clone(),
                    selected_rule: base_rule.id,
                    selected_rule_grammar: base_rule.grammar,
                    overridden_rule: None,
                    resolution: EffectiveGrammarRuleResolution::Inherited,
                }
            }
        })
        .collect()
}

fn rule_id_by_name(rules: &[GrammarRule], name: &str) -> Option<GrammarRuleId> {
    rules
        .iter()
        .find(|rule| rule.name == name)
        .map(|rule| rule.id)
}

fn effective_rule_id_by_name(
    rules: &[EffectiveGrammarRule],
    grammar: GrammarId,
    name: &str,
) -> Option<GrammarRuleId> {
    rules
        .iter()
        .find(|rule| rule.grammar == grammar && rule.name == name)
        .map(|rule| rule.id)
}

fn rule_applications(
    rules: &[EffectiveGrammarRule],
    activation: &GrammarActivation,
    root: &CstNode,
) -> Vec<GrammarRuleApplication> {
    let mut applications = Vec::new();
    collect_rule_applications(rules, activation, root, &mut applications);
    applications
}

fn top_level_shadow_summary(
    implementation: &'static str,
    root: &CstNode,
    diagnostic_count: usize,
) -> ParserProductSummary {
    let signature = top_level_rule_signature(root);
    let top_level_count = signature.len();
    ParserProductSummary::with_top_level_signature(
        implementation,
        normalized_shadow_node_count(top_level_count),
        top_level_count,
        signature.join(","),
        diagnostic_count,
        normalized_shadow_node_count(top_level_count),
    )
}

fn top_level_rule_signature(root: &CstNode) -> Vec<&'static str> {
    root.children
        .iter()
        .filter_map(|child| match child {
            CstElement::Node(child) => cst_top_level_rule_name(child.kind),
            CstElement::Token(_) => None,
        })
        .collect()
}

fn normalized_shadow_node_count(top_level_count: usize) -> usize {
    top_level_count.saturating_add(1)
}

fn cst_node_count(root: &CstNode) -> usize {
    1 + root
        .children
        .iter()
        .filter_map(|child| match child {
            CstElement::Node(child) => Some(cst_node_count(child)),
            CstElement::Token(_) => None,
        })
        .sum::<usize>()
}

fn parser_product_divergences(
    primary: &ParserProductSummary,
    oracle: &ParserProductSummary,
) -> Vec<ParserShadowDivergence> {
    let mut divergences = Vec::new();
    push_parser_product_divergence(
        &mut divergences,
        "normalized-node-count",
        primary.cst_node_count,
        oracle.cst_node_count,
    );
    push_parser_product_divergence(
        &mut divergences,
        "top-level-node-count",
        primary.top_level_node_count,
        oracle.top_level_node_count,
    );
    if primary.top_level_signature != oracle.top_level_signature {
        divergences.push(ParserShadowDivergence {
            field: "top-level-signature".to_string(),
            primary: primary.top_level_signature.clone(),
            oracle: oracle.top_level_signature.clone(),
        });
    }
    push_parser_product_divergence(
        &mut divergences,
        "diagnostic-count",
        primary.diagnostic_count,
        oracle.diagnostic_count,
    );
    push_parser_product_divergence(
        &mut divergences,
        "rule-application-count",
        primary.rule_application_count,
        oracle.rule_application_count,
    );
    divergences
}

fn push_parser_product_divergence(
    divergences: &mut Vec<ParserShadowDivergence>,
    field: &str,
    primary: usize,
    oracle: usize,
) {
    if primary != oracle {
        divergences.push(ParserShadowDivergence {
            field: field.to_string(),
            primary: primary.to_string(),
            oracle: oracle.to_string(),
        });
    }
}

fn collect_rule_applications(
    rules: &[EffectiveGrammarRule],
    activation: &GrammarActivation,
    node: &CstNode,
    applications: &mut Vec<GrammarRuleApplication>,
) {
    if let Some(name) = cst_rule_name(node.kind)
        && let Some(rule) = rules
            .iter()
            .find(|rule| rule.grammar == activation.grammar && rule.name == name)
    {
        applications.push(GrammarRuleApplication {
            activation: activation.id,
            grammar: activation.grammar,
            rule: rule.id,
            rule_name: rule.name.clone(),
            span: node.span,
        });
    }
    for child in &node.children {
        if let CstElement::Node(child) = child {
            collect_rule_applications(rules, activation, child, applications);
        }
    }
}

fn rule_provenance(
    rules: &[EffectiveGrammarRule],
    applications: &[GrammarRuleApplication],
) -> Vec<GrammarRuleProvenance> {
    let mut counts = rules
        .iter()
        .map(|rule| (rule.id, rule.name.clone(), 0_usize))
        .collect::<Vec<_>>();
    for application in applications {
        if let Some((_, _, count)) = counts
            .iter_mut()
            .find(|(rule, _, _)| *rule == application.rule)
        {
            *count = count.saturating_add(1);
        }
    }
    counts
        .into_iter()
        .map(|(rule, rule_name, node_count)| GrammarRuleProvenance {
            rule,
            rule_name,
            node_count,
        })
        .collect()
}

fn grammar_diagnostic_facts(
    source: &str,
    diagnostics: &[Diagnostic],
    activation: &GrammarActivation,
    effective_rules: &[EffectiveGrammarRule],
    applications: &[GrammarRuleApplication],
) -> Vec<GrammarDiagnosticFact> {
    diagnostics
        .iter()
        .enumerate()
        .map(|(diagnostic_index, diagnostic)| {
            let context = diagnostic_rule_context(diagnostic.span, activation, applications);
            let rule = effective_rules
                .iter()
                .find(|rule| rule.id == context.rule)
                .or_else(|| {
                    effective_rules
                        .iter()
                        .find(|rule| rule.name == "source-file")
                });
            GrammarDiagnosticFact {
                diagnostic_index,
                code: diagnostic.code,
                span: diagnostic.span,
                environment: activation.environment,
                grammar: activation.grammar,
                activation: activation.id,
                rule: rule.map_or(activation.root_rule, |rule| rule.id),
                rule_name: context.rule_name,
                context: context.context,
                application_span: context.application_span,
                unexpected_fragment: unexpected_fragment(source, diagnostic.span),
                expected_fragment: diagnostic_arg(diagnostic, "expected"),
            }
        })
        .collect()
}

fn unexpected_fragment(source: &str, span: Span) -> Option<String> {
    if span.is_empty() {
        return None;
    }
    source
        .get(span.start..span.end)
        .filter(|fragment| !fragment.is_empty())
        .map(ToString::to_string)
}

struct DiagnosticRuleContext {
    rule: GrammarRuleId,
    rule_name: String,
    context: GrammarDiagnosticContext,
    application_span: Option<Span>,
}

fn diagnostic_rule_context(
    span: Span,
    activation: &GrammarActivation,
    applications: &[GrammarRuleApplication],
) -> DiagnosticRuleContext {
    if let Some(application) = applications
        .iter()
        .find(|application| application.span == span)
    {
        return DiagnosticRuleContext {
            rule: application.rule,
            rule_name: application.rule_name.clone(),
            context: GrammarDiagnosticContext::ExactRuleApplication,
            application_span: Some(application.span),
        };
    }

    let containing = applications
        .iter()
        .filter(|application| span_contains(application.span, span))
        .min_by_key(|application| {
            (
                application.span.end.saturating_sub(application.span.start),
                application.span.start,
            )
        });
    if let Some(application) = containing {
        return DiagnosticRuleContext {
            rule: application.rule,
            rule_name: application.rule_name.clone(),
            context: GrammarDiagnosticContext::ContainingRuleApplication,
            application_span: Some(application.span),
        };
    }

    DiagnosticRuleContext {
        rule: activation.root_rule,
        rule_name: "source-file".to_string(),
        context: GrammarDiagnosticContext::RootRuleFallback,
        application_span: None,
    }
}

fn diagnostic_arg(diagnostic: &Diagnostic, name: &str) -> Option<String> {
    diagnostic
        .args
        .iter()
        .find(|arg| arg.name == name)
        .map(|arg| arg.value.render())
}

fn span_contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start
        && (inner.end <= outer.end || inner.start == inner.end && inner.start <= outer.end)
}

fn cst_rule_name(kind: CstKind) -> Option<&'static str> {
    match kind {
        CstKind::Root => Some("source-file"),
        CstKind::Function => Some("function-definition"),
        CstKind::Constant => Some("constant-definition"),
        CstKind::Class => Some("class-definition"),
        CstKind::SlotDecl => Some("slot-declaration"),
        CstKind::SlotOption => Some("slot-option"),
        CstKind::Generic => Some("generic-definition"),
        CstKind::Method => Some("method-definition"),
        CstKind::ParameterList => Some("parameter-list"),
        CstKind::Body => Some("body"),
        CstKind::LetStmt => Some("let-statement"),
        CstKind::BinaryExpr => None,
        CstKind::UnaryExpr => Some("unary-expression"),
        CstKind::CallExpr | CstKind::ArgList => Some("argument-list"),
        CstKind::SlotExpr => Some("postfix-expression"),
        CstKind::SlotAssignExpr => Some("assignment-expression"),
        CstKind::KeywordArg => Some("keyword-argument"),
        CstKind::NameExpr
        | CstKind::NumberExpr
        | CstKind::BoolExpr
        | CstKind::StringExpr
        | CstKind::SymbolExpr
        | CstKind::KeywordExpr
        | CstKind::TypeExpr => Some("primary-expression"),
        CstKind::VectorExpr => Some("vector-expression"),
        CstKind::IfExpr => Some("if-expression"),
        CstKind::HandleExpr => Some("handle-expression"),
        CstKind::RestartClause => Some("restart-clause"),
        CstKind::Error => None,
    }
}

fn cst_top_level_rule_name(kind: CstKind) -> Option<&'static str> {
    match kind {
        CstKind::Class => Some("class-definition"),
        CstKind::Generic => Some("generic-definition"),
        CstKind::Method => Some("method-definition"),
        CstKind::Function => Some("function-definition"),
        CstKind::Constant => Some("constant-definition"),
        _ => None,
    }
}

/// Returns the stable EOP id for a language environment.
#[must_use]
pub fn language_environment_object_id(environment: &LanguageEnvironment) -> String {
    format!("language-environment:{}", environment.slug)
}

/// Returns the stable EOP id for a grammar.
#[must_use]
pub fn grammar_object_id(grammar: &Grammar) -> String {
    format!("grammar:{}", grammar.slug)
}

/// Returns the stable EOP id for a grammar rule.
#[must_use]
pub fn grammar_rule_object_id(rule: &GrammarRule) -> String {
    format!("grammar-rule:{}/{}", rule.grammar_slug, rule.name)
}

/// Returns the stable EOP id for a grammar action attached to a rule.
#[must_use]
pub fn grammar_action_object_id(rule: &GrammarRule) -> Option<String> {
    rule.action
        .as_ref()
        .map(|_| format!("grammar-action:{}/{}", rule.grammar_slug, rule.name))
}

/// Returns the stable EOP id for an effective grammar rule.
#[must_use]
pub fn effective_grammar_rule_object_id(rule: &EffectiveGrammarRule) -> String {
    format!("effective-grammar-rule:{}/{}", rule.grammar_slug, rule.name)
}

/// Returns the stable EOP id for a grammar activation.
#[must_use]
pub fn grammar_activation_object_id(activation: &GrammarActivation) -> String {
    format!(
        "grammar-activation:{}/{}",
        activation.grammar_slug, activation.object_slug
    )
}

/// Returns the stable EOP id for a staged parse region.
#[must_use]
pub fn staged_parse_region_object_id(region: &StagedParseRegion) -> String {
    format!(
        "staged-region:{}/{}",
        region.grammar_slug, region.object_slug
    )
}

/// Returns the stable EOP id for a registered grammar extension descriptor.
#[must_use]
pub fn grammar_extension_object_id(descriptor: &GrammarExtensionDescriptor) -> String {
    format!("grammar-extension:{}", descriptor.adapter)
}

/// Returns the stable EOP id for a grammar extension activation.
#[must_use]
pub fn grammar_extension_activation_object_id(activation: &GrammarExtensionActivation) -> String {
    format!(
        "grammar-extension-activation:{}/{}",
        activation.adapter, activation.object_slug
    )
}

/// Returns the stable EOP id for one emitted frontend form.
#[must_use]
pub fn grammar_extension_emission_object_id(emission: &GrammarExtensionEmission) -> String {
    format!(
        "grammar-extension-emission:{}/{}/f{}",
        emission.adapter, emission.object_slug, emission.form_index
    )
}

/// Returns the stable EOP id for one extension provenance rule.
#[must_use]
pub fn grammar_extension_provenance_object_id(provenance: &GrammarExtensionProvenance) -> String {
    format!(
        "grammar-extension-provenance:{}/{}/{}",
        provenance.adapter, provenance.object_slug, provenance.rule_name
    )
}

fn stable_name(name: &str) -> String {
    if matches!(name, "BaseLokoEnvironment" | "BaseLokoGrammar") {
        return BASE_LOKO_ENVIRONMENT_SLUG.to_string();
    }
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use interloko_grammar::Span as GrammarSpan;

    #[test]
    fn builtin_registry_resolves_environment_and_staged_tokens() {
        let registry = GrammarFrontendRegistry::builtin();

        assert_eq!(
            registry.environment_slugs(),
            vec![BASE_LOKO_ENVIRONMENT_SLUG, DERIVED_HOST_ENVIRONMENT_SLUG]
        );
        assert_eq!(
            registry
                .environment_selection(BASE_LOKO_ENVIRONMENT_SLUG)
                .expect("base environment registered")
                .as_str(),
            BASE_LOKO_ENVIRONMENT_SLUG
        );
        assert_eq!(
            registry
                .environment_provider(DERIVED_HOST_ENVIRONMENT_SLUG)
                .expect("derived environment registered")
                .environment_name(),
            DERIVED_HOST_ENVIRONMENT_NAME
        );
        assert_eq!(
            registry.staged_adapter_names(),
            vec![CALCULATOR_GRAMMAR_SLUG, RECORDS_GRAMMAR_SLUG]
        );
        assert_eq!(
            registry
                .staged_adapter(RECORDS_GRAMMAR_SLUG)
                .expect("records staged adapter registered")
                .grammar_name(),
            RECORDS_GRAMMAR_NAME
        );
        let descriptors = registry.extension_descriptors();
        assert_eq!(descriptors.len(), 2);
        let records = registry
            .extension_descriptor(RECORDS_GRAMMAR_SLUG)
            .expect("records extension descriptor");
        assert_eq!(records.adapter, RECORDS_GRAMMAR_SLUG);
        assert_eq!(records.grammar_name, RECORDS_GRAMMAR_NAME);
        assert_eq!(records.activation_directive, "// grammar-activate: records");
        assert_eq!(records.region_start_directive, "// staged-loko: records");
        assert_eq!(
            records.capabilities,
            vec![GrammarExtensionCapability::EmitFrontendForms]
        );
        assert_eq!(records.emitted_form_kinds, vec!["frontend-class"]);
        assert_eq!(
            records.provenance_rule_names,
            vec!["records-region", "record-definition", "record-field"]
        );

        let calculator_grammar = registry.staged_records(&[CALCULATOR_GRAMMAR_SLUG]);
        let calculator_function = calculator_grammar
            .rules
            .iter()
            .find(|rule| {
                rule.grammar_slug == CALCULATOR_GRAMMAR_SLUG && rule.name == "calculator-function"
            })
            .expect("calculator rule");
        assert_eq!(
            calculator_function.projection.origin,
            ProjectionOrigin::SourceAuthored
        );
        assert!(calculator_function.projection.source_span.is_some());
        assert_eq!(
            calculator_function.projection.location,
            "resource:grammars/calculator.grammar/calculator-function"
        );
        assert_eq!(
            calculator_function.expression.to_string(),
            "name `=` expression => emit-function(name: name, body: expression)"
        );

        let records_grammar = registry.staged_records(&[RECORDS_GRAMMAR_SLUG]);
        let record_definition = records_grammar
            .rules
            .iter()
            .find(|rule| {
                rule.grammar_slug == RECORDS_GRAMMAR_SLUG && rule.name == "record-definition"
            })
            .expect("records rule");
        assert_eq!(
            record_definition.projection.origin,
            ProjectionOrigin::SourceAuthored
        );
        assert!(record_definition.projection.source_span.is_some());
        assert_eq!(
            record_definition.projection.location,
            "resource:grammars/records.grammar/record-definition"
        );
        assert_eq!(
            record_definition.expression.to_string(),
            "`record` record-name record-field+ `end` => emit-class(name: record-name, superclass: \"<object>\", slots: record-field*)"
        );
    }

    #[test]
    fn source_authored_rule_interpreter_matches_tiny_grammar() {
        let grammar = "grammar tiny\n\
                       rule pair = name `:` type-name => pair-form\n\
                       rule name = [A-Za-z_] [A-Za-z0-9_?-]*\n\
                       rule type-name = `<` name `>`\n\
                       end\n";
        let rules = interpreter::parse_source_authored_grammar(grammar);
        let parse = interpreter::interpret_region_result(&rules, "pair", "radius: <integer>", 100)
            .expect("pair parses");
        let node = parse.root;

        assert_eq!(node.name(), Some("pair"));
        assert_eq!(node.span, GrammarSpan::new(100, 117));
        assert_eq!(
            node.children
                .iter()
                .filter(|child| child.is_named())
                .map(|child| (child.name().unwrap_or(""), child.text))
                .collect::<Vec<_>>(),
            Vec::from([("name", "radius"), ("type-name", "<integer>")])
        );
    }

    #[test]
    fn source_authored_rule_interpreter_matches_character_classes_and_plus() {
        let grammar = "grammar tiny\n\
                       rule identifier = [A-Za-z_] [A-Za-z0-9_?-]*\n\
                       rule digits = [0-9]+\n\
                       end\n";
        let rules = interpreter::parse_source_authored_grammar(grammar);

        let identifier = interpreter::interpret_region_result(&rules, "identifier", "area?", 10)
            .expect("identifier parses")
            .root;
        assert_eq!(identifier.span, GrammarSpan::new(10, 15));
        assert_eq!(identifier.text, "area?");
        assert_eq!(
            identifier
                .children
                .iter()
                .filter(|child| child.character_class().is_some())
                .map(|child| child.text)
                .collect::<Vec<_>>(),
            Vec::from(["a", "r", "e", "a", "?"])
        );

        let digits = interpreter::interpret_region_result(&rules, "digits", "123", 20)
            .expect("digits parse")
            .root;
        assert_eq!(digits.span, GrammarSpan::new(20, 23));
        assert_eq!(digits.text, "123");

        let error = interpreter::interpret_region_result(&rules, "digits", "", 30)
            .expect_err("one-or-more requires one match");
        assert_eq!(error.span, GrammarSpan::new(30, 30));
        assert_eq!(
            error.expected,
            Vec::from([interloko_grammar::ExpectedFragment::CharacterClass("0-9")])
        );
    }

    #[test]
    fn source_authored_rule_interpreter_matches_until_literal_regions() {
        let grammar = "grammar tiny\n\
                       rule block = `begin` until(`end`) `end`\n\
                       end\n";
        let rules = interpreter::parse_source_authored_grammar(grammar);
        let node = interpreter::interpret_region_result(&rules, "block", "begin 1 + 2\nend", 40)
            .expect("block parses")
            .root;

        assert_eq!(node.name(), Some("block"));
        assert_eq!(node.span, GrammarSpan::new(40, 55));
        assert_eq!(
            node.children
                .iter()
                .find(|child| child.opaque_delimiter() == Some("end"))
                .map(|child| child.text),
            Some(" 1 + 2\n")
        );
        assert_eq!(
            rules.rules()[0].expression.to_string(),
            "`begin` until(`end`) `end`"
        );
    }

    #[test]
    fn source_authored_rule_interpreter_matches_hyphenated_names() {
        let grammar = "grammar tiny\n\
                       rule name = [A-Za-z_] [A-Za-z0-9_?-]*\n\
                       end\n";
        let rules = interpreter::parse_source_authored_grammar(grammar);
        let node = interpreter::interpret_region_result(&rules, "name", "required-init-keyword", 0)
            .expect("name parses")
            .root;
        assert_eq!(node.text, "required-init-keyword");
    }

    #[test]
    fn source_authored_rule_interpreter_matches_slot_with_required_keyword() {
        let grammar = include_str!("../grammars/base_loko.grammar");
        let rules = interpreter::parse_source_authored_grammar(grammar);
        let source = "slot radius :: <integer>, required-init-keyword: radius:;";
        let node = interpreter::interpret_region_result(&rules, "slot-declaration", source, 0)
            .expect("slot parses")
            .root;
        assert_eq!(node.text, source);
    }

    #[test]
    fn source_authored_rule_interpreter_matches_class_with_required_keyword_slot() {
        let grammar = include_str!("../grammars/base_loko.grammar");
        let rules = interpreter::parse_source_authored_grammar(grammar);
        let source = "define class <circle> (<object>)\n                        slot radius :: <integer>, required-init-keyword: radius:;\n                      end";
        let node = interpreter::interpret_region_result(&rules, "class-definition", source, 0)
            .expect("class parses")
            .root;
        assert_eq!(node.text, source);
    }

    #[test]
    fn source_authored_rule_interpreter_reports_unknown_rule_refs() {
        let grammar = "grammar tiny\n\
                       rule root = missing-rule\n\
                       end\n";
        let rules = interpreter::parse_source_authored_grammar(grammar);
        let error = interpreter::interpret_region_result(&rules, "root", "anything", 40)
            .expect_err("unknown rule cannot be interpreted by hidden builtins");

        assert_eq!(error.span, GrammarSpan::new(40, 48));
        assert_eq!(
            error.expected,
            Vec::from([interloko_grammar::ExpectedFragment::Rule("missing-rule")])
        );
    }

    #[test]
    fn unknown_environment_selection_reports_registry_expectation() {
        let report = parse_file_with_registry(
            SourceFileId::from_path("unknown-grammar.loko"),
            "define function main() 1 end",
            GrammarEnvironmentSelection::new("missing-grammar"),
            &GrammarFrontendRegistry::builtin(),
        );

        let diagnostic = report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.args.iter().any(|arg| arg.name == "environment"))
            .expect("unknown grammar environment diagnostic");
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.value.render().contains("base-loko, derived-host"))
        );
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.value.render() == "missing-grammar")
        );
    }

    #[test]
    fn derived_host_records_reflect_no_direct_overrides() {
        // derived_host.grammar currently has no rule overrides, so the
        // records must report zero direct rules and every effective
        // rule must resolve as `Inherited` from base-loko. This
        // matches what the parser actually compiles via
        // `compile_derived_host_grammar` — provenance and behavior
        // agree.
        let records = derived_host_records();
        let derived_grammar = GrammarId::new(DERIVED_HOST_GRAMMAR_NAME);
        let base_grammar = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
        let grammar = records
            .grammars
            .iter()
            .find(|grammar| grammar.id == derived_grammar)
            .expect("derived grammar exists");

        assert_eq!(grammar.extends, vec![base_grammar]);
        assert!(grammar.direct_rules.is_empty());
        assert_eq!(grammar.effective_rules.len(), 26);

        assert!(
            !records
                .rules
                .iter()
                .any(|rule| rule.grammar == derived_grammar),
            "derived-host must not contribute any direct rules until the source declares overrides",
        );

        for effective in records
            .effective_rules
            .iter()
            .filter(|rule| rule.grammar == derived_grammar)
        {
            assert_eq!(
                effective.resolution,
                EffectiveGrammarRuleResolution::Inherited,
                "every derived-host effective rule must be Inherited (rule `{}`)",
                effective.name,
            );
            assert_eq!(effective.selected_rule_grammar, base_grammar);
        }
    }

    #[test]
    fn derived_host_parsed_function_definition_resolves_as_inherited() {
        // A function defined in derived-host source must be attributed
        // to the inherited base-loko rule, not a fictional override.
        // This pins the records-vs-parser invariant after the synthetic
        // override was removed.
        let report = parse_file_with_environment(
            SourceFileId::from_path("derived.loko"),
            "define function main() 1 end",
            GrammarEnvironmentSelection::derived_host(),
        );
        let derived_grammar = GrammarId::new(DERIVED_HOST_GRAMMAR_NAME);
        let base_grammar = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);

        let effective = report
            .effective_rules
            .iter()
            .find(|rule| rule.grammar == derived_grammar && rule.name == "function-definition")
            .expect("derived-host has an effective function-definition entry");
        assert_eq!(
            effective.resolution,
            EffectiveGrammarRuleResolution::Inherited
        );
        assert_eq!(effective.selected_rule_grammar, base_grammar);

        let application_rule_names: Vec<&str> = report
            .provenance
            .applications
            .iter()
            .map(|app| app.rule_name.as_str())
            .collect();
        assert!(
            application_rule_names.contains(&"function-definition"),
            "parser must report at least one function-definition application",
        );
    }

    #[test]
    fn derived_host_grammar_source_compiles_against_base_loko() {
        // Validate that derived_host.grammar parses cleanly, declares the
        // expected `extends base-loko` header, and compiles through
        // `try_compile_with_parent` without arity diagnostics. This pins
        // the source file as a real consumer of Phase 2's `extends`
        // parsing and `try_compile_with_parent` API.
        let base_source = include_str!("../grammars/base_loko.grammar");
        let parsed_base = interloko_grammar::parse_grammar_source_full(base_source);
        assert!(parsed_base.diagnostics.is_empty());
        let base_compiled = interloko_grammar::CompiledGrammar::compile(parsed_base.rules);

        let parsed_derived = interloko_grammar::parse_grammar_source_full(
            super::interpreter::DERIVED_HOST_GRAMMAR_SOURCE,
        );
        assert!(parsed_derived.diagnostics.is_empty());
        assert_eq!(parsed_derived.header.name, Some("derived-host"));
        assert_eq!(parsed_derived.header.extends, Some("base-loko"));

        interloko_grammar::CompiledGrammar::try_compile_with_parent(
            parsed_derived.rules,
            &base_compiled,
        )
        .expect("derived_host.grammar must compile against base-loko");
    }

    #[test]
    fn derived_host_parse_uses_derived_activation_and_effective_root_rule() {
        let report = parse_file_with_environment(
            SourceFileId::from_path("example.loko"),
            "define function main() 1 end",
            GrammarEnvironmentSelection::derived_host(),
        );
        let derived_grammar = GrammarId::new(DERIVED_HOST_GRAMMAR_NAME);
        let activation = report.activations.first().expect("source-file activation");

        assert_eq!(report.environment.slug, "derived-host");
        assert_eq!(activation.grammar, derived_grammar);
        assert_eq!(activation.grammar_slug, "derived-host");
        assert!(
            report
                .effective_rules
                .iter()
                .any(|rule| rule.id == activation.root_rule
                    && rule.grammar == derived_grammar
                    && rule.name == "source-file")
        );
    }

    #[test]
    fn frontend_parse_artifact_preserves_parse_tree_and_cst_projection() {
        let source_file = SourceFileId::from_path("parse-artifact.loko");
        let source = "define function main()\n  42\nend";
        let report = parse_file(source_file, source);

        assert_eq!(report.frontend_parse.source_file, source_file);
        assert_eq!(report.frontend_parse.environment, report.environment.id);
        assert_eq!(report.frontend_parse.grammar, report.activations[0].grammar);
        assert_eq!(report.frontend_parse.activation, report.activations[0].id);
        assert_eq!(
            report.frontend_parse.root_rule,
            report.activations[0].root_rule
        );
        assert_eq!(
            report.frontend_parse.source,
            FrontendParseSource::OriginalSource
        );
        assert_eq!(
            report.frontend_parse.source_span,
            Span::new(0, source.len())
        );
        assert_eq!(
            report.frontend_parse.diagnostic_count,
            report.syntax.diagnostics.len()
        );
        assert_eq!(
            report.frontend_parse.cst_node_count,
            cst_node_count(&report.syntax.root)
        );
        assert_eq!(report.syntax.root.child_nodes(CstKind::Function).count(), 1);

        let parse_tree = report
            .frontend_parse
            .parse_tree
            .as_ref()
            .expect("successful parse preserves an owned parse-tree artifact");
        assert_eq!(parse_tree.name(), Some("source-file"));
        assert!(
            parse_tree
                .walk()
                .any(|node| node.is_rule("function-definition")),
            "parse tree contains the function-definition rule"
        );

        let parse_tree_data = report
            .frontend_parse
            .parse_tree_data
            .as_ref()
            .expect("source-data projection alongside the typed tree");
        let dump = source_data::dump(parse_tree_data);
        assert!(dump.contains("rule: source-file"));
        assert!(dump.contains("rule: function-definition"));
    }

    #[test]
    fn grammar_diagnostics_record_active_rule_and_expected_fragment() {
        let report = parse_file_with_environment(
            SourceFileId::from_path("bad.loko"),
            "define function main(",
            GrammarEnvironmentSelection::derived_host(),
        );
        let fact = report
            .grammar_diagnostics
            .first()
            .expect("syntax diagnostic fact");

        assert_eq!(fact.code, DiagnosticCode::SyntaxExpectedToken);
        assert_eq!(fact.grammar, GrammarId::new(DERIVED_HOST_GRAMMAR_NAME));
        assert_eq!(fact.rule_name, "source-file");
        assert_eq!(
            fact.context,
            GrammarDiagnosticContext::ContainingRuleApplication
        );
        assert_eq!(
            fact.application_span,
            Some(Span::new(0, "define function main(".len()))
        );
        assert_eq!(fact.unexpected_fragment, None);
        assert!(fact.expected_fragment.is_some());
        assert_eq!(report.provenance.diagnostics, report.grammar_diagnostics);
    }

    #[test]
    fn base_loko_expected_token_diagnostic_points_at_unexpected_token() {
        let report = parse_file(
            SourceFileId::from_path("bad-keyword.loko"),
            "define classs foo (<object>) end;",
        );
        let diagnostic = report.diagnostics.first().expect("syntax diagnostic");

        assert_eq!(diagnostic.code, DiagnosticCode::SyntaxExpectedToken);
        assert_eq!(diagnostic.span, Span::new(7, 13));
        assert_eq!(
            diagnostic_arg(diagnostic, "expected"),
            Some("`class` or `generic` or `method` or `function` or `constant`".to_string())
        );
        let fact = report
            .grammar_diagnostics
            .first()
            .expect("grammar diagnostic fact");
        assert_eq!(fact.unexpected_fragment.as_deref(), Some("classs"));
    }

    #[test]
    fn base_loko_expected_token_diagnostic_keeps_eof_span() {
        let report = parse_file(
            SourceFileId::from_path("bad-eof.loko"),
            "define function main(",
        );
        let diagnostic = report.diagnostics.first().expect("syntax diagnostic");

        assert_eq!(diagnostic.code, DiagnosticCode::SyntaxExpectedToken);
        assert_eq!(
            diagnostic.span,
            Span::new("define function main(".len(), "define function main(".len())
        );
        let fact = report
            .grammar_diagnostics
            .first()
            .expect("grammar diagnostic fact");
        assert_eq!(fact.unexpected_fragment, None);
    }

    #[test]
    fn staged_calculator_region_emits_structural_frontend_forms() {
        let source = "// grammar-activate: calculator\n\n\
                      // staged-loko: calculator\n\
                      helper = 40\n\
                      // end-staged-loko\n\n\
                      // staged-loko: calculator\n\
                      main = helper() + 2\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("staged.loko"), source);

        assert_eq!(report.environment.name, STAGED_LOKO_ENVIRONMENT_NAME);
        assert_eq!(
            report.frontend_parse.source,
            FrontendParseSource::StagedRegionsBlanked
        );
        assert!(report.frontend_parse.parse_tree.is_some());
        assert_eq!(report.activations.len(), 3);
        assert_eq!(report.staged_regions.len(), 2);
        assert!(
            report
                .grammar_stack
                .grammars
                .contains(&GrammarId::new(CALCULATOR_GRAMMAR_NAME))
        );
        assert!(!report.syntax.source.contains("define function helper()"));
        assert!(!report.syntax.source.contains("define function main()"));
        assert_eq!(report.staged_regions[0].adapter, "calculator");
        assert_eq!(report.staged_regions[0].object_slug, "r0");
        assert!(
            report.staged_regions[0]
                .rule_applications
                .iter()
                .any(|application| application.rule_name == "calculator-function")
        );
        assert_eq!(
            report.staged_regions[0].driver,
            StagedParseDriver::InterpretedGrammar
        );
        assert_eq!(report.staged_regions[0].oracle, StagedParseOracle::None);
        assert!(
            source_data::dump(&report.staged_regions[0].data)
                .contains("driver: interpreted-grammar")
        );
        assert!(source_data::dump(&report.staged_regions[0].data).contains("oracle: none"));
        assert!(
            source_data::dump(&report.staged_regions[1].data).contains("rule: calculator-region")
        );
        assert_eq!(report.extension_activations.len(), 2);
        assert_eq!(report.extension_activations[0].adapter, "calculator");
        assert_eq!(report.extension_activations[0].emitted_form_count, 1);
        assert_eq!(
            report.extension_activations[0].capabilities,
            vec![GrammarExtensionCapability::EmitFrontendForms]
        );
        assert_eq!(report.extension_emissions.len(), 2);
        assert_eq!(report.extension_emissions[0].form_kind, "frontend-function");
        assert!(
            source_data::dump(&report.extension_emissions[1].data)
                .contains("frontend-function(name: main")
        );
        assert!(source_data::dump(&report.extension_emissions[1].data).contains("frontend-binary"));
        assert!(!source_data::dump(&report.extension_emissions[1].data).contains("loko-text"));
        assert!(
            report
                .extension_provenance
                .iter()
                .any(|provenance| provenance.adapter == "calculator"
                    && provenance.object_slug == "r1"
                    && provenance.rule_name == "calculator-function"
                    && provenance.node_count > 0)
        );
        assert!(
            source_data::dump(&report.staged_regions[1].forms_data)
                .contains("frontend-function(name: main")
        );
        assert!(
            report.staged_regions[1]
                .materialized_source
                .contains("define function main()")
        );
        assert!(
            report.staged_regions[1]
                .materialized_source
                .contains("helper() + 2")
        );
        assert!(
            source_data::dump(&report.staged_regions[1].data).contains("grammar-action-output")
        );
        assert!(
            source_data::dump(&report.staged_regions[1].data)
                .contains("frontend-function(name: main")
        );
    }

    #[test]
    fn staged_region_requires_activation_directive() {
        let source = "// staged-loko: calculator\n\
                      main = 40 + 2\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("staged.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("// grammar-activate: calculator".to_string()))
        );
    }

    #[test]
    fn unknown_staged_activation_reports_registered_adapter_expectation() {
        let source = "// grammar-activate: missing\n\
                      define function main()\n  1\nend\n";
        let report = parse_file(SourceFileId::from_path("staged.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("registered staged grammar (calculator, records)".to_string()))
        );
    }

    #[test]
    fn staged_calculator_region_reports_malformed_lines() {
        let source = "// grammar-activate: calculator\n\
                      // staged-loko: calculator\n\
                      main 40 + 2\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("calculator.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("`=`".to_string())
                    && diagnostic_arg(diagnostic, "adapter") == Some("calculator".to_string()))
        );
    }

    #[test]
    fn staged_records_region_emits_class_frontend_forms() {
        let source = "// grammar-activate: records\n\n\
                      // staged-loko: records\n\
                      record circle\n\
                        radius: <integer>\n\
                      end\n\
                      // end-staged-loko\n\n\
                      define function main()\n\
                        let c = make(<circle>, radius: 10);\n\
                        radius(c)\n\
                      end\n";
        let report = parse_file(SourceFileId::from_path("records.loko"), source);

        assert_eq!(report.environment.name, STAGED_LOKO_ENVIRONMENT_NAME);
        assert!(
            report
                .grammar_stack
                .grammars
                .contains(&GrammarId::new(RECORDS_GRAMMAR_NAME))
        );
        assert_eq!(report.staged_regions.len(), 1);
        assert_eq!(report.staged_regions[0].adapter, "records");
        assert!(
            report.staged_regions[0]
                .rule_applications
                .iter()
                .any(|application| application.rule_name == "record-definition")
        );
        assert!(
            report.staged_regions[0]
                .rule_applications
                .iter()
                .any(|application| application.rule_name == "record-field")
        );
        assert_eq!(
            report.staged_regions[0].driver,
            StagedParseDriver::InterpretedGrammar
        );
        assert_eq!(report.staged_regions[0].oracle, StagedParseOracle::None);
        assert!(
            source_data::dump(&report.staged_regions[0].data)
                .contains("driver: interpreted-grammar")
        );
        assert!(source_data::dump(&report.staged_regions[0].data).contains("oracle: none"));
        assert!(source_data::dump(&report.staged_regions[0].data).contains("rule: records-region"));
        assert!(
            report
                .extension_provenance
                .iter()
                .any(|provenance| provenance.rule_name == "record-field"
                    && provenance.node_count == 1)
        );
        assert!(
            source_data::dump(&report.staged_regions[0].forms_data)
                .contains("frontend-class(name: <circle>")
        );
        assert!(
            report.staged_regions[0]
                .materialized_source
                .contains("define class <circle> (<object>)")
        );
        assert!(
            report.staged_regions[0]
                .materialized_source
                .contains("required-init-keyword: radius:")
        );
        assert!(!report.syntax.source.contains("define class <circle>"));
        assert!(report.diagnostics.is_empty());
    }

    #[test]
    fn staged_records_region_reports_duplicate_fields() {
        let source = "// grammar-activate: records\n\
                      // staged-loko: records\n\
                      record circle\n\
                        radius: <integer>\n\
                        radius: <integer>\n\
                      end\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("records.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("unique record field".to_string())
                    && diagnostic_arg(diagnostic, "adapter") == Some("records".to_string())
                    && diagnostic_arg(diagnostic, "field") == Some("radius".to_string()))
        );
    }

    #[test]
    fn staged_records_region_reports_malformed_fields() {
        let source = "// grammar-activate: records\n\
                      // staged-loko: records\n\
                      record circle\n\
                        radius <integer>\n\
                      end\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("records.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("`:`".to_string())
                    && diagnostic_arg(diagnostic, "adapter") == Some("records".to_string()))
        );
    }

    #[test]
    fn staged_records_region_reports_invalid_fields() {
        let source = "// grammar-activate: records\n\
                      // staged-loko: records\n\
                      record circle\n\
                        123: <integer>\n\
                      end\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("records.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("valid record field".to_string())
                    && diagnostic_arg(diagnostic, "adapter") == Some("records".to_string())
                    && diagnostic_arg(diagnostic, "rule") == Some("record-field".to_string())
                    && diagnostic_arg(diagnostic, "action") == Some("emit-slot".to_string())
                    && diagnostic_arg(diagnostic, "argument") == Some("name".to_string())
                    && diagnostic_arg(diagnostic, "field") == Some("123".to_string()))
        );
    }

    #[test]
    fn staged_records_region_reports_malformed_record_syntax() {
        let source = "// grammar-activate: records\n\
                      // staged-loko: records\n\
                      circle\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("records.loko"), source);

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic_arg(diagnostic, "expected")
                    == Some("`record`".to_string())
                    && diagnostic_arg(diagnostic, "adapter") == Some("records".to_string()))
        );
    }

    #[test]
    fn parser_shadow_report_summarizes_base_loko_grammar_parser() {
        let source_file = SourceFileId::from_path("shadow.loko");
        let source = "define class <circle> (<object>)\n\
                        slot radius :: <integer>, required-init-keyword: radius:;\n\
                      end\n\
                      define generic area(shape :: <object>) => <integer>;\n\
                      define method area(c :: <circle>) => <integer>\n\
                        c.radius * c.radius\n\
                      end\n\
                      define function main()\n\
                        area(make(<circle>, radius: 10))\n\
                      end";
        let report = parse_file(source_file, source);

        let shadow = parser_shadow_report(&report);
        assert_eq!(shadow.status, ParserShadowStatus::OracleUnavailable);
        assert_eq!(
            shadow.primary.implementation,
            "source-authored-base-loko-grammar"
        );
        assert_eq!(shadow.primary.cst_node_count, 5);
        assert_eq!(shadow.primary.top_level_node_count, 4);
        assert_eq!(
            shadow.primary.top_level_signature,
            "class-definition,generic-definition,method-definition,function-definition"
        );
        assert_eq!(shadow.primary.diagnostic_count, 0);
        assert_eq!(shadow.primary.rule_application_count, 5);
        assert_eq!(shadow.oracle, None);
        assert_eq!(shadow.divergences, Vec::new());
    }

    #[test]
    fn grammar_primary_base_loko_builds_cst_for_current_surface() {
        let source_file = SourceFileId::from_path("grammar-primary.loko");
        let source = "define class <circle> (<object>)\n\
                        slot radius :: <integer>, required-init-keyword: radius:;\n\
                      end\n\
                      define generic area(shape :: <object>) => <integer>;\n\
                      define method area(c :: <circle>) => <integer>\n\
                        c.radius * c.radius\n\
                      end\n\
                      define function main()\n\
                        area(make(<circle>, radius: 10))\n\
                      end";
        let report = parse_file(source_file, source);

        assert_eq!(report.diagnostics, Vec::new());
        assert_eq!(report.syntax.root.child_nodes(CstKind::Class).count(), 1);
        assert_eq!(report.syntax.root.child_nodes(CstKind::Generic).count(), 1);
        assert_eq!(report.syntax.root.child_nodes(CstKind::Method).count(), 1);
        assert_eq!(report.syntax.root.child_nodes(CstKind::Function).count(), 1);
        assert!(
            report
                .syntax
                .root
                .child_nodes(CstKind::Class)
                .next()
                .expect("class")
                .child_nodes(CstKind::SlotDecl)
                .next()
                .expect("slot")
                .child_nodes(CstKind::SlotOption)
                .count()
                == 1
        );

        let module = crate::ast::lower(&report.syntax);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.classes.len(), 1);
        assert_eq!(module.generics.len(), 1);
        assert_eq!(module.methods.len(), 1);
        assert_eq!(module.functions.len(), 1);
        assert_eq!(module.classes[0].slots[0].name, "radius");
        assert_eq!(module.functions[0].name, "main");
    }

    #[test]
    fn grammar_primary_base_loko_lowers_dylan_style_keyword_symbols() {
        let source_file = SourceFileId::from_path("grammar-primary-keywords.loko");
        let source = "define function main()\n\
                        fallback:\n\
                      end";
        let report = parse_file(source_file, source);

        assert_eq!(report.diagnostics, Vec::new());
        let module = crate::ast::lower(&report.syntax);
        assert_eq!(module.diagnostics, Vec::new());
        assert_eq!(module.functions.len(), 1);
        assert!(matches!(
            &module.functions[0].body.result,
            crate::ast::Expr::Symbol { name, .. } if name == "fallback"
        ));
    }

    #[test]
    fn parser_shadow_comparison_reports_equivalence_and_divergence() {
        let primary = ParserProductSummary::new("primary", 10, 2, 0, 8);
        let equivalent = compare_parser_products(
            primary.clone(),
            ParserProductSummary::new("oracle", 10, 2, 0, 8),
        );
        assert_eq!(equivalent.status, ParserShadowStatus::Equivalent);
        assert_eq!(equivalent.divergences, Vec::new());

        let diverged =
            compare_parser_products(primary, ParserProductSummary::new("oracle", 9, 2, 1, 8));
        assert_eq!(diverged.status, ParserShadowStatus::Diverged);
        assert_eq!(
            diverged
                .divergences
                .iter()
                .map(|divergence| divergence.field.as_str())
                .collect::<Vec<_>>(),
            Vec::from(["normalized-node-count", "diagnostic-count"])
        );
    }
}
