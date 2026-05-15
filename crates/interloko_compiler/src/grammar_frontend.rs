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

/// A structural frontend form emitted by a trusted staged grammar adapter.
///
/// This is a transitional expander-facing representation. The current frontend
/// still materializes forms back to ordinary Loko source before invoking the
/// existing parser.
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
    /// Transitional raw Loko expression text with original source span.
    LokoText {
        /// Expression source text.
        text: String,
        /// Original staged source span.
        source_span: Span,
    },
}

impl FrontendExpression {
    /// Builds a transitional raw Loko expression form.
    #[must_use]
    pub fn loko_text(text: impl Into<String>, source_span: Span) -> Self {
        Self::LokoText {
            text: text.into(),
            source_span,
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

/// Provider for a registered language environment.
///
/// A provider contributes the grammar records for one environment token. The
/// current built-ins still delegate CST construction to the hand-written parser,
/// but this protocol keeps selection out of hardcoded caller matches.
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
/// All selections currently delegate tokenization and CST construction to the
/// base hand-written parser. The selected environment controls the projected
/// grammar stack, effective rule table, activations, and provenance records.
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
    match expression {
        FrontendExpression::LokoText { text, .. } => source.push_str(text),
    }
}

/// Projects frontend forms into inert source data for dumps and EOP records.
#[must_use]
pub fn frontend_forms_source_data(forms: &[FrontendForm]) -> SourceData {
    SourceData::list(forms.iter().map(frontend_form_source_data).collect())
}

fn frontend_form_source_data(form: &FrontendForm) -> SourceData {
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

fn frontend_expression_source_data(expression: &FrontendExpression) -> SourceData {
    match expression {
        FrontendExpression::LokoText { text, source_span } => SourceData::record(
            "frontend-expression",
            vec![
                source_data::field("kind", SourceData::symbol("loko-text")),
                source_data::field("text", SourceData::string(text.as_str())),
                source_span_data(*source_span),
            ],
        ),
    }
}

fn frontend_slot_source_data(slot: &FrontendSlot) -> SourceData {
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

fn source_span_data(span: Span) -> source_data::SourceDataField {
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
                    &report.diagnostics,
                    &report.syntax.root,
                    activation,
                    &report.effective_rules,
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
    let mut materialized_source = String::new();
    let mut materialized_cursor = 0_usize;
    let mut activations = vec![base_activation];
    let mut staged_regions = Vec::new();
    let mut staged_rule_counts = Vec::new();

    for (index, region) in discovery.regions.iter().enumerate() {
        materialized_source.push_str(&source[materialized_cursor..region.start_marker.start]);

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
                "{} staged region emitted frontend forms for materialization",
                region.adapter.adapter_name()
            ),
        };
        let emission = region
            .adapter
            .parse_region(region.body, region.body_span.start);
        diagnostics.extend(emission.diagnostics);
        let region_materialized_source = materialize_frontend_forms(&emission.forms);
        materialized_source.push_str(&region_materialized_source);
        materialized_cursor = region.end_marker.end;
        staged_rule_counts.push((grammar, region.adapter.provenance_rule_names()));
        let forms_data = frontend_forms_source_data(&emission.forms);

        activations.push(activation.clone());
        staged_regions.push(StagedParseRegion {
            object_slug,
            adapter: region.adapter.adapter_name().to_string(),
            activation: activation.id,
            grammar,
            grammar_slug: region.adapter.grammar_slug().to_string(),
            activation_directive: region.activation_directive,
            source_span: region.body_span,
            forms: emission.forms,
            forms_data,
            materialized_source: region_materialized_source,
            data: emission.data,
        });
    }
    materialized_source.push_str(&source[materialized_cursor..]);

    let mut syntax = syntax::parse_file(source_file, &materialized_source);
    syntax.diagnostics.extend(diagnostics);

    let active_effective_rules = records.effective_rules.to_vec();
    let mut provenance_rules = rule_provenance(&active_effective_rules, &syntax.root);
    for (grammar, rule_names) in staged_rule_counts {
        for rule_name in rule_names {
            add_staged_rule_count(
                &mut provenance_rules,
                &records.effective_rules,
                grammar,
                rule_name,
            );
        }
    }

    let grammar_stack = GrammarStack {
        environment: records.environment.id,
        grammars: records.environment.grammar_stack.clone(),
    };
    let diagnostic_activation = activations
        .last()
        .expect("staged reports always include at least a base activation");
    let grammar_diagnostics = grammar_diagnostic_facts(
        &syntax.diagnostics,
        &syntax.root,
        diagnostic_activation,
        &active_effective_rules,
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
    /// Inert structured protocol data describing the adapter parse.
    pub data: SourceData,
    /// Diagnostics emitted while parsing the staged region.
    pub diagnostics: Vec<Diagnostic>,
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

    fn direct_rules(&self, grammar: GrammarId) -> Vec<GrammarRule> {
        calculator_rules(grammar, self.grammar_slug())
    }

    fn parse_region(&self, body: &str, source_offset: usize) -> StagedRegionEmission {
        parse_calculator_region(body, source_offset)
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

    fn direct_rules(&self, grammar: GrammarId) -> Vec<GrammarRule> {
        records_rules(grammar, self.grammar_slug())
    }

    fn parse_region(&self, body: &str, source_offset: usize) -> StagedRegionEmission {
        parse_records_region(body, source_offset)
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

fn parse_calculator_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    let mut forms = Vec::new();
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
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "calculator",
                "`name = expression`",
            ));
            continue;
        };
        let name = name.trim();
        let expression = expression.trim();
        if name.is_empty() || expression.is_empty() || !valid_calculator_function_name(name) {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "calculator",
                "`name = expression`",
            ));
            continue;
        }
        let expression_offset = trimmed.find(expression).unwrap_or(0);
        let expression_span = Span::new(
            trimmed_start + expression_offset,
            trimmed_start + expression_offset + expression.len(),
        );
        forms.push(FrontendForm::function(
            name,
            FrontendExpression::loko_text(expression, expression_span),
            trimmed_span,
        ));
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
        diagnostics.push(staged_expected_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "calculator",
            "`name = expression`",
        ));
    }

    StagedRegionEmission {
        forms,
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

struct RecordBuilder {
    name: String,
    header_span: Span,
    fields: Vec<FrontendSlot>,
    field_data: Vec<SourceData>,
}

fn parse_records_region(body: &str, source_offset: usize) -> StagedRegionEmission {
    let mut forms = Vec::new();
    let mut diagnostics = Vec::new();
    let mut records = Vec::new();
    let mut current: Option<RecordBuilder> = None;
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

        if current.is_some() {
            if trimmed == "end" {
                let record = current.take().expect("record was just matched");
                let source_span = Span::new(record.header_span.start, trimmed_span.end);
                let fields_data = SourceData::list(record.field_data);
                forms.push(FrontendForm::class(
                    record.name.clone(),
                    "<object>",
                    record.fields,
                    source_span,
                ));
                records.push(SourceData::record(
                    "record-definition",
                    vec![
                        source_data::field("name", SourceData::symbol(record.name.as_str())),
                        source_span_data(source_span),
                        source_data::field("fields", fields_data),
                    ],
                ));
                continue;
            }
            let record = current.as_mut().expect("record was just matched");
            parse_record_field(trimmed, trimmed_span, record, &mut diagnostics);
            continue;
        }

        let Some(name) = trimmed.strip_prefix("record ") else {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "records",
                "`record name`",
            ));
            continue;
        };
        let name = name.trim();
        let Some(class_name) = normalize_record_class_name(name) else {
            diagnostics.push(staged_expected_diagnostic(
                trimmed_span,
                "records",
                "valid record name",
            ));
            continue;
        };
        current = Some(RecordBuilder {
            name: class_name,
            header_span: trimmed_span,
            fields: Vec::new(),
            field_data: Vec::new(),
        });
    }

    if let Some(record) = current {
        diagnostics.push(staged_expected_diagnostic(
            record.header_span,
            "records",
            "`end`",
        ));
    }

    if records.is_empty() && diagnostics.is_empty() {
        diagnostics.push(staged_expected_diagnostic(
            Span::new(source_offset, source_offset + body.len()),
            "records",
            "`record name`",
        ));
    }

    StagedRegionEmission {
        forms,
        data: SourceData::record(
            "records-region",
            vec![
                source_data::field("grammar", SourceData::symbol(RECORDS_GRAMMAR_NAME)),
                source_data::field("records", SourceData::list(records)),
            ],
        ),
        diagnostics,
    }
}

fn parse_record_field(
    trimmed: &str,
    span: Span,
    record: &mut RecordBuilder,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some((name, type_name)) = trimmed.split_once(':') else {
        diagnostics.push(staged_expected_diagnostic(
            span,
            "records",
            "`field: <type>`",
        ));
        return;
    };
    let name = name.trim();
    let type_name = type_name.trim().trim_end_matches(';').trim();
    if !valid_record_field_name(name) {
        diagnostics.push(
            staged_expected_diagnostic(span, "records", "valid record field")
                .with_arg(DiagnosticArg::text("field", name)),
        );
        return;
    }
    if record.fields.iter().any(|field| field.name == name) {
        diagnostics.push(
            staged_expected_diagnostic(span, "records", "unique record field")
                .with_arg(DiagnosticArg::text("field", name)),
        );
        return;
    }
    if !valid_type_name(type_name) {
        diagnostics.push(
            staged_expected_diagnostic(span, "records", "field type name")
                .with_arg(DiagnosticArg::text("field", name)),
        );
        return;
    }

    record
        .fields
        .push(FrontendSlot::new(name, type_name, span).with_init_keyword(name, true));
    record.field_data.push(SourceData::record(
        "record-field",
        vec![
            source_data::field("name", SourceData::symbol(name)),
            source_data::field("type", SourceData::symbol(type_name)),
            source_span_data(span),
        ],
    ));
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

fn push_report_diagnostic(report: &mut GrammarParseReport, diagnostic: Diagnostic) {
    report.diagnostics.push(diagnostic);
    if let Some(activation) = report.activations.first() {
        report.grammar_diagnostics = grammar_diagnostic_facts(
            &report.diagnostics,
            &report.syntax.root,
            activation,
            &report.effective_rules,
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

fn normalize_record_class_name(name: &str) -> Option<String> {
    if valid_type_name(name) {
        return Some(name.to_string());
    }
    if valid_record_type_fragment(name) {
        return Some(format!("<{name}>"));
    }
    None
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
        grammar_slug: DERIVED_HOST_ENVIRONMENT_SLUG.to_string(),
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
        direct_rules: vec![override_rule.id],
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

fn records_rules(grammar: GrammarId, grammar_slug: &str) -> Vec<GrammarRule> {
    const RULES: &[(&str, &str, &str)] = &[
        (
            "records-region",
            "record-definition*",
            "grammar_frontend.rs:parse_records_region",
        ),
        (
            "record-definition",
            "`record` name record-field* `end` => class form",
            "grammar_frontend.rs:parse_records_region",
        ),
        (
            "record-field",
            "name `:` type-name => required slot form",
            "grammar_frontend.rs:parse_records_region",
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

/// Returns the stable EOP id for a staged parse region.
#[must_use]
pub fn staged_parse_region_object_id(region: &StagedParseRegion) -> String {
    format!(
        "staged-region:{}/{}",
        region.grammar_slug, region.object_slug
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
    fn grammar_diagnostics_record_active_rule_and_expected_fragment() {
        let report = parse_file_with_environment(
            SourceFileId::from_path("bad.loko"),
            "define function main()\nend",
            GrammarEnvironmentSelection::derived_host(),
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
        let source = "// grammar-activate: calculator\n\n\
                      // staged-loko: calculator\n\
                      helper = 40\n\
                      // end-staged-loko\n\n\
                      // staged-loko: calculator\n\
                      main = helper() + 2\n\
                      // end-staged-loko\n";
        let report = parse_file(SourceFileId::from_path("staged.loko"), source);

        assert_eq!(report.environment.name, STAGED_LOKO_ENVIRONMENT_NAME);
        assert_eq!(report.activations.len(), 3);
        assert_eq!(report.staged_regions.len(), 2);
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
                .contains("define function helper()\n  40\nend")
        );
        assert!(
            report
                .syntax
                .source
                .contains("define function main()\n  helper() + 2\nend")
        );
        assert_eq!(report.staged_regions[0].adapter, "calculator");
        assert_eq!(report.staged_regions[0].object_slug, "r0");
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
            source_data::dump(&report.staged_regions[1].data)
                .contains("calculator-function(name: main")
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
                    == Some("`name = expression`".to_string())
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
            source_data::dump(&report.staged_regions[0].forms_data)
                .contains("frontend-class(name: <circle>")
        );
        assert!(
            report.staged_regions[0]
                .materialized_source
                .contains("define class <circle> (<object>)")
        );
        assert!(
            report
                .syntax
                .source
                .contains("required-init-keyword: radius:")
        );
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
                    == Some("`field: <type>`".to_string())
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
                    == Some("`record name`".to_string())
                    && diagnostic_arg(diagnostic, "adapter") == Some("records".to_string()))
        );
    }
}
