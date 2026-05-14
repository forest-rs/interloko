//! Read-only grammar frontend projection for the base Loko parser.
//!
//! This module owns language-environment identity, grammar metaobjects,
//! grammar-stack resolution, parse activations, and parse provenance for the
//! current hand-written parser. It intentionally does not own AST lowering,
//! semantic lowering, source-authored grammars, macros, semantic actions,
//! staged parsing, or parser rewriting.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::diagnostic::Diagnostic;
use crate::identity::{SourceFileId, StableIdBuilder};
use crate::syntax::{self, CstElement, CstKind, CstNode, Span, SyntaxTree};

const DOMAIN_LANGUAGE_ENVIRONMENT: u64 = 0x4c4f_4b4f_5f45_4e56;
const DOMAIN_GRAMMAR: u64 = 0x4c4f_4b4f_5f47_524d;
const DOMAIN_GRAMMAR_RULE: u64 = 0x4c4f_4b4f_5f52_554c;
const DOMAIN_GRAMMAR_ACTIVATION: u64 = 0x4c4f_4b4f_5f41_4354;
const DOMAIN_PARSE_PROVENANCE: u64 = 0x4c4f_4b4f_5f50_5256;

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
    /// Stable origin label.
    pub origin: String,
    /// Implementation location or conceptual parser entry.
    pub location: String,
    /// Optional source span when a rule is source-authored.
    pub source_span: Option<Span>,
}

/// Read-only language environment metaobject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageEnvironment {
    /// Stable environment id.
    pub id: LanguageEnvironmentId,
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
    /// Stable display name.
    pub name: String,
    /// Parent grammars. Empty for the base grammar.
    pub extends: Vec<GrammarId>,
    /// Direct projected rule ids in parser surface order.
    pub direct_rules: Vec<GrammarRuleId>,
}

/// Read-only direct grammar rule metaobject.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrammarRule {
    /// Stable rule id.
    pub id: GrammarRuleId,
    /// Owning grammar.
    pub grammar: GrammarId,
    /// Rule name.
    pub name: String,
    /// Human-readable projected expression.
    pub expression: String,
    /// Projection/source location information.
    pub projection: GrammarProjection,
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
    /// Source region parsed by this activation.
    pub span: Span,
    /// Rule that produced the root parse node.
    pub root_rule: GrammarRuleId,
    /// Stable reason text.
    pub reason: String,
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
    /// Rule provenance summaries.
    pub rules: Vec<GrammarRuleProvenance>,
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
    /// Resolved grammar stack.
    pub grammar_stack: GrammarStack,
    /// Grammar activations used by this parse.
    pub activations: Vec<GrammarActivation>,
    /// Parse provenance facts.
    pub provenance: ParseProvenance,
    /// Syntax diagnostics from the parser.
    pub diagnostics: Vec<Diagnostic>,
    /// Syntax tree produced by the existing parser.
    pub syntax: SyntaxTree,
}

/// Parses source through the base Loko grammar frontend.
#[must_use]
pub fn parse_file(source_file: SourceFileId, source: &str) -> GrammarParseReport {
    let records = base_loko_records();
    let syntax = syntax::parse_file(source_file, source);
    let root_rule = rule_id_by_name(&records.rules, "source-file")
        .unwrap_or_else(|| GrammarRuleId::new(records.base_grammar, "source-file"));
    let activation = GrammarActivation {
        id: GrammarActivationId::new(source_file, records.base_grammar, "source-file"),
        environment: records.environment.id,
        grammar: records.base_grammar,
        span: Span::new(0, source.len()),
        root_rule,
        reason: "base-loko source file parse".to_string(),
    };
    let provenance_rules = rule_provenance(&records.rules, &syntax.root);
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
        rules: provenance_rules,
    };

    GrammarParseReport {
        source_file,
        environment: records.environment,
        grammars: records.grammars,
        rules: records.rules,
        grammar_stack,
        activations: vec![activation],
        provenance,
        diagnostics: syntax.diagnostics.clone(),
        syntax,
    }
}

/// Returns the projected base Loko grammar records without parsing source.
#[must_use]
pub fn base_loko_records() -> BaseLokoRecords {
    let environment_id = LanguageEnvironmentId::new("BaseLokoEnvironment");
    let grammar_id = GrammarId::new("BaseLokoGrammar");
    let rules = base_loko_rules(grammar_id);
    let direct_rules = rules.iter().map(|rule| rule.id).collect();
    let grammar = Grammar {
        id: grammar_id,
        name: "BaseLokoGrammar".to_string(),
        extends: Vec::new(),
        direct_rules,
    };
    let environment = LanguageEnvironment {
        id: environment_id,
        name: "BaseLokoEnvironment".to_string(),
        grammar_stack: vec![grammar_id],
    };
    BaseLokoRecords {
        environment,
        base_grammar: grammar_id,
        grammars: vec![grammar],
        rules,
    }
}

/// Projected base grammar records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseLokoRecords {
    /// Base language environment.
    pub environment: LanguageEnvironment,
    /// Base grammar id.
    pub base_grammar: GrammarId,
    /// Projected grammars.
    pub grammars: Vec<Grammar>,
    /// Projected direct rules.
    pub rules: Vec<GrammarRule>,
}

fn base_loko_rules(grammar: GrammarId) -> Vec<GrammarRule> {
    const RULES: &[(&str, &str, &str)] = &[
        ("source-file", "definition*", "syntax.rs:Parser::parse"),
        (
            "definition",
            "class-definition | function-definition | generic-definition | method-definition",
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
            "parameter-list",
            "`(` parameter* `)`",
            "syntax.rs:Parser::parse_parameter_list",
        ),
        ("body", "let* expression", "syntax.rs:Parser::parse_body"),
        (
            "let-statement",
            "`let` name `=` expression `;`",
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
            name: (*name).to_string(),
            expression: (*expression).to_string(),
            projection: GrammarProjection {
                origin: "compiler-projected".to_string(),
                location: (*location).to_string(),
                source_span: None,
            },
        })
        .collect()
}

fn rule_id_by_name(rules: &[GrammarRule], name: &str) -> Option<GrammarRuleId> {
    rules
        .iter()
        .find(|rule| rule.name == name)
        .map(|rule| rule.id)
}

fn rule_provenance(rules: &[GrammarRule], root: &CstNode) -> Vec<GrammarRuleProvenance> {
    let mut counts = rules
        .iter()
        .map(|rule| (rule.id, rule.name.clone(), 0_usize))
        .collect::<Vec<_>>();
    count_node(root, &mut counts);
    counts
        .into_iter()
        .map(|(rule, rule_name, node_count)| GrammarRuleProvenance {
            rule,
            rule_name,
            node_count,
        })
        .collect()
}

fn count_node(node: &CstNode, counts: &mut [(GrammarRuleId, String, usize)]) {
    if let Some(name) = cst_rule_name(node.kind)
        && let Some((_, _, count)) = counts
            .iter_mut()
            .find(|(_, rule_name, _)| rule_name == name)
    {
        *count = count.saturating_add(1);
    }
    for child in &node.children {
        if let CstElement::Node(child) = child {
            count_node(child, counts);
        }
    }
}

fn cst_rule_name(kind: CstKind) -> Option<&'static str> {
    match kind {
        CstKind::Root => Some("source-file"),
        CstKind::Function => Some("function-definition"),
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

/// Returns the stable EOP id for a language environment.
#[must_use]
pub fn language_environment_object_id(environment: &LanguageEnvironment) -> String {
    format!("language-environment:{}", stable_name(&environment.name))
}

/// Returns the stable EOP id for a grammar.
#[must_use]
pub fn grammar_object_id(grammar: &Grammar) -> String {
    format!("grammar:{}", stable_name(&grammar.name))
}

/// Returns the stable EOP id for a grammar rule.
#[must_use]
pub fn grammar_rule_object_id(rule: &GrammarRule) -> String {
    format!("grammar-rule:base-loko/{}", rule.name)
}

/// Returns the stable EOP id for a grammar activation.
#[must_use]
pub fn grammar_activation_object_id(activation: &GrammarActivation) -> String {
    let _ = activation;
    "grammar-activation:base-loko/source-file".to_string()
}

fn stable_name(name: &str) -> String {
    if matches!(name, "BaseLokoEnvironment" | "BaseLokoGrammar") {
        return "base-loko".to_string();
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
