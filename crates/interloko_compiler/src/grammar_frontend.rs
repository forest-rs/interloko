//! Read-only grammar frontend projection for the base Loko parser.
//!
//! This module owns language-environment identity, grammar metaobjects,
//! grammar-stack resolution, staged parse activations, structural parse
//! emissions, and parse provenance for the current hand-written parser. It
//! intentionally does not own AST lowering, semantic lowering, source-authored
//! grammars, macros, host semantic actions, or parser rewriting.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::diagnostic::{Diagnostic, DiagnosticArg, DiagnosticCode};
use crate::identity::{SourceFileId, StableIdBuilder};
use crate::source_data::{self, SourceData};
use crate::syntax::{self, CstElement, CstKind, CstNode, Span, SyntaxTree};

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
/// Stable name for the source-selected staged calculator environment.
pub const STAGED_CALCULATOR_ENVIRONMENT_NAME: &str = "StagedCalculatorEnvironment";

const STAGED_CALCULATOR_START: &str = "// staged-loko: calculator";
const STAGED_CALCULATOR_END: &str = "// end-staged-loko";

/// Selects the grammar environment used by the frontend.
///
/// The first slice keeps parsing delegated to the base hand-written parser, but
/// this selector lets callers request a different read-only grammar projection
/// and grammar stack for inspection and incremental provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrammarEnvironmentSelection {
    /// The ordinary Loko grammar projected from the current parser.
    BaseLoko,
    /// A trusted host-provided fixture grammar that extends the base grammar.
    DerivedHost,
}

impl GrammarEnvironmentSelection {
    /// Returns the stable environment name associated with this selection.
    #[must_use]
    pub const fn environment_name(self) -> &'static str {
        match self {
            Self::BaseLoko => BASE_LOKO_ENVIRONMENT_NAME,
            Self::DerivedHost => DERIVED_HOST_ENVIRONMENT_NAME,
        }
    }

    /// Returns the stable command/config token associated with this selection.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BaseLoko => "base-loko",
            Self::DerivedHost => "derived-host",
        }
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
    /// Human-readable projected expression.
    pub expression: String,
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

/// One staged source region and its structural parse result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedParseRegion {
    /// Grammar activation that parsed this region.
    pub activation: GrammarActivationId,
    /// Grammar used for this staged region.
    pub grammar: GrammarId,
    /// Grammar slug used in dumps.
    pub grammar_slug: String,
    /// Original source region parsed by the staged grammar.
    pub source_span: Span,
    /// Ordinary Loko source emitted by the staged grammar.
    pub emitted_source: String,
    /// Inert structural parse result.
    pub data: SourceData,
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
    /// Rule provenance summaries.
    pub rules: Vec<GrammarRuleProvenance>,
    /// Grammar-specific diagnostic facts.
    pub diagnostics: Vec<GrammarDiagnosticFact>,
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
    /// Parse provenance facts.
    pub provenance: ParseProvenance,
    /// Grammar-specific diagnostic facts.
    pub grammar_diagnostics: Vec<GrammarDiagnosticFact>,
    /// Syntax diagnostics from the parser.
    pub diagnostics: Vec<Diagnostic>,
    /// Syntax tree produced by the existing parser.
    pub syntax: SyntaxTree,
}

/// Parses source through the base Loko grammar frontend.
#[must_use]
pub fn parse_file(source_file: SourceFileId, source: &str) -> GrammarParseReport {
    parse_file_with_environment(source_file, source, GrammarEnvironmentSelection::BaseLoko)
}

/// Parses source through the selected grammar frontend.
///
/// All selections currently delegate tokenization and CST construction to the
/// base hand-written parser. The selected environment controls the projected
/// grammar stack, effective rule table, activations, and provenance records.
#[must_use]
pub fn parse_file_with_environment(
    source_file: SourceFileId,
    source: &str,
    environment: GrammarEnvironmentSelection,
) -> GrammarParseReport {
    if environment == GrammarEnvironmentSelection::BaseLoko
        && let Some(report) = parse_staged_calculator_file(source_file, source)
    {
        return report;
    }
    let records = match environment {
        GrammarEnvironmentSelection::BaseLoko => base_loko_records(),
        GrammarEnvironmentSelection::DerivedHost => derived_host_records(),
    };
    parse_file_with_records(source_file, source, records)
}

fn parse_file_with_records(
    source_file: SourceFileId,
    source: &str,
    records: GrammarFrontendRecords,
) -> GrammarParseReport {
    let syntax = syntax::parse_file(source_file, source);
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
    let active_effective_rules = records
        .effective_rules
        .iter()
        .filter(|rule| rule.grammar == active_grammar)
        .cloned()
        .collect::<Vec<_>>();
    let provenance_rules = rule_provenance(&active_effective_rules, &syntax.root);
    let grammar_diagnostics = grammar_diagnostic_facts(
        &syntax.diagnostics,
        &syntax.root,
        &activation,
        &active_effective_rules,
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
        provenance,
        grammar_diagnostics,
        diagnostics: syntax.diagnostics.clone(),
        syntax,
    }
}

fn parse_staged_calculator_file(
    source_file: SourceFileId,
    source: &str,
) -> Option<GrammarParseReport> {
    let region = find_staged_calculator_region(source)?;
    let records = staged_calculator_records();
    let base_grammar = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let calculator_grammar = GrammarId::new(CALCULATOR_GRAMMAR_NAME);
    let base_root_rule =
        effective_rule_id_by_name(&records.effective_rules, base_grammar, "source-file")
            .unwrap_or_else(|| GrammarRuleId::new(base_grammar, "effective:source-file"));
    let calculator_root_rule = effective_rule_id_by_name(
        &records.effective_rules,
        calculator_grammar,
        "calculator-region",
    )
    .unwrap_or_else(|| GrammarRuleId::new(calculator_grammar, "effective:calculator-region"));

    let prefix_activation = GrammarActivation {
        id: GrammarActivationId::new(source_file, base_grammar, "staged-prefix"),
        environment: records.environment.id,
        grammar: base_grammar,
        grammar_slug: "base-loko".to_string(),
        object_slug: "prefix".to_string(),
        span: Span::new(0, region.start_marker.start),
        root_rule: base_root_rule,
        reason: "base-loko prefix parse selected calculator staged grammar".to_string(),
    };
    let staged_activation = GrammarActivation {
        id: GrammarActivationId::new(source_file, calculator_grammar, "calculator-region"),
        environment: records.environment.id,
        grammar: calculator_grammar,
        grammar_slug: "calculator".to_string(),
        object_slug: "calculator-region".to_string(),
        span: region.body_span,
        root_rule: calculator_root_rule,
        reason: "calculator staged region emitted ordinary Loko source".to_string(),
    };

    let mut diagnostics = Vec::new();
    if !prefix_selects_calculator(source_file, region.prefix) {
        diagnostics.push(
            Diagnostic::error(region.start_marker, DiagnosticCode::SyntaxExpectedToken).with_arg(
                DiagnosticArg::text("expected", "#calculator in staged prefix"),
            ),
        );
    }
    let emission = parse_calculator_region(region.body, region.body_span.start);
    diagnostics.extend(emission.diagnostics);

    let materialized_source = materialized_staged_source(region.prefix, &emission.emitted_source);
    let mut syntax = syntax::parse_file(source_file, &materialized_source);
    syntax.diagnostics.extend(diagnostics);

    let active_effective_rules = records
        .effective_rules
        .iter()
        .filter(|rule| rule.grammar == base_grammar || rule.grammar == calculator_grammar)
        .cloned()
        .collect::<Vec<_>>();
    let mut provenance_rules = rule_provenance(&active_effective_rules, &syntax.root);
    add_staged_rule_count(
        &mut provenance_rules,
        &records.effective_rules,
        calculator_grammar,
        "calculator-region",
    );
    add_staged_rule_count(
        &mut provenance_rules,
        &records.effective_rules,
        calculator_grammar,
        "calculator-function",
    );

    let activations = vec![prefix_activation, staged_activation.clone()];
    let grammar_stack = GrammarStack {
        environment: records.environment.id,
        grammars: records.environment.grammar_stack.clone(),
    };
    let staged_effective_rules = records
        .effective_rules
        .iter()
        .filter(|rule| rule.grammar == calculator_grammar)
        .cloned()
        .collect::<Vec<_>>();
    let grammar_diagnostics = grammar_diagnostic_facts(
        &syntax.diagnostics,
        &syntax.root,
        &staged_activation,
        &staged_effective_rules,
    );
    let provenance = ParseProvenance {
        id: ParseProvenanceId::new(source_file, records.environment.id),
        source_file,
        environment: records.environment.id,
        grammar_stack: grammar_stack.clone(),
        activations: activations.iter().map(|activation| activation.id).collect(),
        rules: provenance_rules,
        diagnostics: grammar_diagnostics.clone(),
    };
    let staged_regions = vec![StagedParseRegion {
        activation: staged_activation.id,
        grammar: calculator_grammar,
        grammar_slug: "calculator".to_string(),
        source_span: region.body_span,
        emitted_source: emission.emitted_source,
        data: emission.data,
    }];

    Some(GrammarParseReport {
        source_file,
        environment: records.environment,
        grammars: records.grammars,
        rules: records.rules,
        effective_rules: records.effective_rules,
        grammar_stack,
        activations,
        staged_regions,
        provenance,
        grammar_diagnostics,
        diagnostics: syntax.diagnostics.clone(),
        syntax,
    })
}

struct StagedCalculatorRegion<'source> {
    prefix: &'source str,
    body: &'source str,
    start_marker: Span,
    body_span: Span,
}

struct CalculatorRegionEmission {
    emitted_source: String,
    data: SourceData,
    diagnostics: Vec<Diagnostic>,
}

fn find_staged_calculator_region(source: &str) -> Option<StagedCalculatorRegion<'_>> {
    let mut offset = 0_usize;
    let mut start_marker = None;
    let mut body_start = None;
    for line in source.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        let trimmed = line.trim();
        if trimmed == STAGED_CALCULATOR_START {
            start_marker = Some(Span::new(line_start, line_end));
            body_start = Some(line_end);
            break;
        }
        offset = line_end;
    }
    let start_marker = start_marker?;
    let body_start = body_start?;
    let mut body_end = source.len();
    offset = body_start;
    for line in source[body_start..].split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        if line.trim() == STAGED_CALCULATOR_END {
            body_end = line_start;
            break;
        }
        offset = line_end;
    }
    Some(StagedCalculatorRegion {
        prefix: &source[..start_marker.start],
        body: &source[body_start..body_end],
        start_marker,
        body_span: Span::new(body_start, body_end),
    })
}

fn prefix_selects_calculator(source_file: SourceFileId, prefix: &str) -> bool {
    let syntax = syntax::parse_file(source_file, prefix);
    syntax.tokens.iter().any(|token| {
        token.kind == syntax::TokenKind::Symbol && token.text(&syntax.source) == "#calculator"
    })
}

fn parse_calculator_region(body: &str, source_offset: usize) -> CalculatorRegionEmission {
    let mut emitted_source = String::new();
    let mut diagnostics = Vec::new();
    let mut functions = Vec::new();
    let mut offset = source_offset;

    for line in body.split_inclusive('\n') {
        let line_start = offset;
        let line_end = offset + line.len();
        offset = line_end;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let leading = line.find(trimmed).unwrap_or(0);
        let trimmed_start = line_start + leading;
        let trimmed_span = Span::new(trimmed_start, trimmed_start + trimmed.len());
        let Some((name, expression)) = trimmed.split_once('=') else {
            diagnostics.push(calculator_diagnostic(trimmed_span, "`name = expression`"));
            continue;
        };
        let name = name.trim();
        let expression = expression.trim();
        if name.is_empty() || expression.is_empty() || !valid_calculator_function_name(name) {
            diagnostics.push(calculator_diagnostic(trimmed_span, "`name = expression`"));
            continue;
        }
        emitted_source.push_str("define function ");
        emitted_source.push_str(name);
        emitted_source.push_str("()\n  ");
        emitted_source.push_str(expression);
        emitted_source.push_str("\nend\n");
        functions.push(SourceData::record(
            "calculator-function",
            vec![
                source_data::field("name", SourceData::symbol(name)),
                source_data::field("expression", SourceData::string(expression)),
                source_data::field(
                    "source-start",
                    SourceData::integer(usize_to_source_i64(trimmed_span.start)),
                ),
                source_data::field(
                    "source-end",
                    SourceData::integer(usize_to_source_i64(trimmed_span.end)),
                ),
            ],
        ));
    }

    if functions.is_empty() && diagnostics.is_empty() {
        diagnostics.push(calculator_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "`name = expression`",
        ));
    }

    CalculatorRegionEmission {
        emitted_source,
        data: SourceData::record(
            "calculator-region",
            vec![
                source_data::field("grammar", SourceData::symbol(CALCULATOR_GRAMMAR_NAME)),
                source_data::field("functions", SourceData::list(functions)),
            ],
        ),
        diagnostics,
    }
}

fn calculator_diagnostic(span: Span, expected: &str) -> Diagnostic {
    Diagnostic::error(span, DiagnosticCode::SyntaxExpectedToken)
        .with_arg(DiagnosticArg::text("expected", expected))
}

fn valid_calculator_function_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'?'))
}

fn materialized_staged_source(prefix: &str, emitted_source: &str) -> String {
    let mut materialized = String::new();
    materialized.push_str(prefix);
    if !materialized.ends_with('\n') {
        materialized.push('\n');
    }
    materialized.push_str(emitted_source);
    materialized
}

fn add_staged_rule_count(
    rules: &mut [GrammarRuleProvenance],
    effective_rules: &[EffectiveGrammarRule],
    grammar: GrammarId,
    name: &str,
) {
    let Some(rule) = effective_rules
        .iter()
        .find(|rule| rule.grammar == grammar && rule.name == name)
    else {
        return;
    };
    if let Some(provenance) = rules.iter_mut().find(|item| item.rule == rule.id) {
        provenance.node_count = provenance.node_count.saturating_add(1);
    }
}

fn usize_to_source_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Returns the projected base Loko grammar records without parsing source.
#[must_use]
pub fn base_loko_records() -> GrammarFrontendRecords {
    let environment_id = LanguageEnvironmentId::new(BASE_LOKO_ENVIRONMENT_NAME);
    let grammar_id = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let rules = base_loko_rules(grammar_id, "base-loko");
    let direct_rules = rules.iter().map(|rule| rule.id).collect();
    let effective_rules = direct_effective_rules(grammar_id, "base-loko", &rules);
    let effective_rule_ids = effective_rules.iter().map(|rule| rule.id).collect();
    let grammar = Grammar {
        id: grammar_id,
        slug: "base-loko".to_string(),
        name: BASE_LOKO_GRAMMAR_NAME.to_string(),
        extends: Vec::new(),
        direct_rules,
        effective_rules: effective_rule_ids,
    };
    let environment = LanguageEnvironment {
        id: environment_id,
        slug: "base-loko".to_string(),
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

/// Returns the trusted host-derived grammar records without parsing source.
#[must_use]
pub fn derived_host_records() -> GrammarFrontendRecords {
    let base_environment = base_loko_records();
    let base_grammar_id = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let derived_environment_id = LanguageEnvironmentId::new(DERIVED_HOST_ENVIRONMENT_NAME);
    let derived_grammar_id = GrammarId::new(DERIVED_HOST_GRAMMAR_NAME);
    let override_rule = GrammarRule {
        id: GrammarRuleId::new(derived_grammar_id, "function-definition"),
        grammar: derived_grammar_id,
        grammar_slug: "derived-host".to_string(),
        name: "function-definition".to_string(),
        expression:
            "host-projected override delegates function-definition parsing to BaseLokoGrammar"
                .to_string(),
        expression_data: SourceData::record(
            "host-grammar-rule",
            vec![
                source_data::field("name", SourceData::symbol("function-definition")),
                source_data::field("extends", SourceData::symbol("BaseLokoGrammar")),
                source_data::field("delegates-to", SourceData::symbol("BaseLokoGrammar")),
            ],
        ),
        projection: GrammarProjection {
            origin: "trusted-host-fixture".to_string(),
            location: "host:DerivedHostGrammar/function-definition".to_string(),
            source_span: None,
        },
    };
    let mut rules = base_environment.rules;
    rules.push(override_rule.clone());
    let mut grammars = base_environment.grammars;
    let base_effective_rules = base_environment.effective_rules;
    let derived_effective_rules =
        derived_effective_rules(base_grammar_id, derived_grammar_id, "derived-host", &rules);
    let derived_effective_rule_ids = derived_effective_rules
        .iter()
        .map(|rule| rule.id)
        .collect::<Vec<_>>();
    grammars.push(Grammar {
        id: derived_grammar_id,
        slug: "derived-host".to_string(),
        name: DERIVED_HOST_GRAMMAR_NAME.to_string(),
        extends: vec![base_grammar_id],
        direct_rules: vec![override_rule.id],
        effective_rules: derived_effective_rule_ids,
    });
    let mut effective_rules = base_effective_rules;
    effective_rules.extend(derived_effective_rules);
    let environment = LanguageEnvironment {
        id: derived_environment_id,
        slug: "derived-host".to_string(),
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

/// Returns the source-selected staged calculator records without parsing source.
#[must_use]
pub fn staged_calculator_records() -> GrammarFrontendRecords {
    let base_environment = base_loko_records();
    let base_grammar_id = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
    let calculator_environment_id = LanguageEnvironmentId::new(STAGED_CALCULATOR_ENVIRONMENT_NAME);
    let calculator_grammar_id = GrammarId::new(CALCULATOR_GRAMMAR_NAME);
    let calculator_rules = calculator_rules(calculator_grammar_id, "calculator");
    let calculator_direct_rule_ids = calculator_rules.iter().map(|rule| rule.id).collect();
    let mut rules = base_environment.rules;
    rules.extend(calculator_rules.clone());
    let mut grammars = base_environment.grammars;
    let base_effective_rules = base_environment.effective_rules;
    let calculator_effective_rules =
        calculator_effective_rules(base_grammar_id, calculator_grammar_id, "calculator", &rules);
    let calculator_effective_rule_ids = calculator_effective_rules
        .iter()
        .map(|rule| rule.id)
        .collect::<Vec<_>>();
    grammars.push(Grammar {
        id: calculator_grammar_id,
        slug: "calculator".to_string(),
        name: CALCULATOR_GRAMMAR_NAME.to_string(),
        extends: vec![base_grammar_id],
        direct_rules: calculator_direct_rule_ids,
        effective_rules: calculator_effective_rule_ids,
    });
    let mut effective_rules = base_effective_rules;
    effective_rules.extend(calculator_effective_rules);
    let environment = LanguageEnvironment {
        id: calculator_environment_id,
        slug: "staged-calculator".to_string(),
        name: STAGED_CALCULATOR_ENVIRONMENT_NAME.to_string(),
        grammar_stack: vec![base_grammar_id, calculator_grammar_id],
    };
    GrammarFrontendRecords {
        environment,
        active_grammar: calculator_grammar_id,
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
            grammar_slug: grammar_slug.to_string(),
            name: (*name).to_string(),
            expression: (*expression).to_string(),
            expression_data: grammar_rule_expression_data(name, expression, location),
            projection: GrammarProjection {
                origin: "compiler-projected".to_string(),
                location: (*location).to_string(),
                source_span: None,
            },
        })
        .collect()
}

fn calculator_rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    const RULES: &[(&str, &str, &str)] = &[
        (
            "calculator-region",
            "calculator-function*",
            "grammar_frontend.rs:parse_calculator_region",
        ),
        (
            "calculator-function",
            "name `=` expression => `define function name() expression end`",
            "grammar_frontend.rs:parse_calculator_region",
        ),
    ];

    RULES
        .iter()
        .map(|(name, expression, location)| GrammarRule {
            id: GrammarRuleId::new(grammar, name),
            grammar,
            grammar_slug: grammar_slug.to_string(),
            name: (*name).to_string(),
            expression: (*expression).to_string(),
            expression_data: grammar_rule_expression_data(name, expression, location),
            projection: GrammarProjection {
                origin: "compiler-projected-prototype".to_string(),
                location: (*location).to_string(),
                source_span: None,
            },
        })
        .collect()
}

fn grammar_rule_expression_data(name: &str, expression: &str, location: &str) -> SourceData {
    SourceData::record(
        "grammar-rule",
        vec![
            source_data::field("name", SourceData::symbol(name)),
            source_data::field("expression", SourceData::string(expression)),
            source_data::field("location", SourceData::string(location)),
        ],
    )
}

fn calculator_effective_rules(
    base_grammar: GrammarId,
    calculator_grammar: GrammarId,
    calculator_slug: &str,
    rules: &[GrammarRule],
) -> Vec<EffectiveGrammarRule> {
    let mut effective_rules =
        derived_effective_rules(base_grammar, calculator_grammar, calculator_slug, rules);
    effective_rules.extend(
        rules
            .iter()
            .filter(|rule| rule.grammar == calculator_grammar)
            .map(|rule| EffectiveGrammarRule {
                id: GrammarRuleId::new(calculator_grammar, &format!("effective:{}", rule.name)),
                grammar: calculator_grammar,
                grammar_slug: calculator_slug.to_string(),
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

fn rule_provenance(rules: &[EffectiveGrammarRule], root: &CstNode) -> Vec<GrammarRuleProvenance> {
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

fn grammar_diagnostic_facts(
    diagnostics: &[Diagnostic],
    root: &CstNode,
    activation: &GrammarActivation,
    effective_rules: &[EffectiveGrammarRule],
) -> Vec<GrammarDiagnosticFact> {
    diagnostics
        .iter()
        .enumerate()
        .map(|(diagnostic_index, diagnostic)| {
            let rule_name = cst_rule_name_at_span(root, diagnostic.span).unwrap_or("source-file");
            let rule = effective_rules
                .iter()
                .find(|rule| rule.name == rule_name)
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
                rule_name: rule_name.to_string(),
                expected_fragment: diagnostic_arg(diagnostic, "expected"),
            }
        })
        .collect()
}

fn diagnostic_arg(diagnostic: &Diagnostic, name: &str) -> Option<String> {
    diagnostic
        .args
        .iter()
        .find(|arg| arg.name == name)
        .map(|arg| arg.value.render())
}

fn cst_rule_name_at_span(root: &CstNode, span: Span) -> Option<&'static str> {
    let mut best = None;
    find_cst_rule_name_at_span(root, span, &mut best);
    best.map(|(_, name)| name)
}

fn find_cst_rule_name_at_span(
    node: &CstNode,
    span: Span,
    best: &mut Option<(usize, &'static str)>,
) {
    if !span_contains(node.span, span) {
        return;
    }
    if let Some(name) = cst_rule_name(node.kind) {
        let width = node.span.end.saturating_sub(node.span.start);
        if best.is_none_or(|(best_width, _)| width <= best_width) {
            *best = Some((width, name));
        }
    }
    for child in &node.children {
        if let CstElement::Node(child) = child {
            find_cst_rule_name_at_span(child, span, best);
        }
    }
}

fn span_contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start
        && (inner.end <= outer.end || inner.start == inner.end && inner.start <= outer.end)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_host_records_distinguish_direct_and_effective_rules() {
        let records = derived_host_records();
        let derived_grammar = GrammarId::new(DERIVED_HOST_GRAMMAR_NAME);
        let base_grammar = GrammarId::new(BASE_LOKO_GRAMMAR_NAME);
        let grammar = records
            .grammars
            .iter()
            .find(|grammar| grammar.id == derived_grammar)
            .expect("derived grammar exists");

        assert_eq!(grammar.extends, vec![base_grammar]);
        assert_eq!(grammar.direct_rules.len(), 1);
        assert_eq!(grammar.effective_rules.len(), 25);

        let direct_override = records
            .rules
            .iter()
            .find(|rule| rule.grammar == derived_grammar && rule.name == "function-definition")
            .expect("derived direct override");
        assert_eq!(direct_override.projection.origin, "trusted-host-fixture");
        assert_eq!(
            source_data::dump(&direct_override.expression_data),
            "host-grammar-rule(name: function-definition, extends: BaseLokoGrammar, delegates-to: BaseLokoGrammar)"
        );

        let effective_override = records
            .effective_rules
            .iter()
            .find(|rule| rule.grammar == derived_grammar && rule.name == "function-definition")
            .expect("derived effective override");
        assert_eq!(
            effective_override.resolution,
            EffectiveGrammarRuleResolution::Override
        );
        assert_eq!(effective_override.selected_rule, direct_override.id);
        assert!(effective_override.overridden_rule.is_some());

        let inherited = records
            .effective_rules
            .iter()
            .find(|rule| rule.grammar == derived_grammar && rule.name == "class-definition")
            .expect("derived inherited rule");
        assert_eq!(
            inherited.resolution,
            EffectiveGrammarRuleResolution::Inherited
        );
        assert_eq!(inherited.selected_rule_grammar, base_grammar);
    }

    #[test]
    fn derived_host_parse_uses_derived_activation_and_effective_root_rule() {
        let report = parse_file_with_environment(
            SourceFileId::from_path("example.loko"),
            "define function main() 1 end",
            GrammarEnvironmentSelection::DerivedHost,
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
    fn grammar_diagnostics_record_active_rule_and_expected_fragment() {
        let report = parse_file_with_environment(
            SourceFileId::from_path("bad.loko"),
            "define function main()\nend",
            GrammarEnvironmentSelection::DerivedHost,
        );
        let fact = report
            .grammar_diagnostics
            .first()
            .expect("syntax diagnostic fact");

        assert_eq!(fact.code, DiagnosticCode::SyntaxExpectedExpression);
        assert_eq!(fact.grammar, GrammarId::new(DERIVED_HOST_GRAMMAR_NAME));
        assert_eq!(fact.rule_name, "body");
        assert_eq!(fact.expected_fragment, None);
        assert_eq!(report.provenance.diagnostics, report.grammar_diagnostics);
    }

    #[test]
    fn staged_calculator_region_emits_ordinary_loko_source() {
        let source = "define function choose_grammar()\n  #calculator\nend\n\n\
                      // staged-loko: calculator\n\
                      main = 40 + 2\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("staged.loko"), source);

        assert_eq!(report.environment.name, STAGED_CALCULATOR_ENVIRONMENT_NAME);
        assert_eq!(report.activations.len(), 2);
        assert_eq!(report.staged_regions.len(), 1);
        assert!(
            report
                .grammar_stack
                .grammars
                .contains(&GrammarId::new(CALCULATOR_GRAMMAR_NAME))
        );
        assert!(
            report
                .syntax
                .source
                .contains("define function main()\n  40 + 2\nend")
        );
        assert!(
            source_data::dump(&report.staged_regions[0].data)
                .contains("calculator-function(name: main")
        );
    }
}
