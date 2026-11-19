//! Stable-ish compiler identities and query artifact keys.
//!
//! This module owns durable identity construction for one compiler session and
//! across small edits to a single source file. It explicitly does not own
//! parsing, binding, semantic resolution, or cache scheduling.

use core::fmt;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

const DOMAIN_SOURCE_FILE: u64 = 0x01;
const DOMAIN_SYNTAX_NODE: u64 = 0x02;
const DOMAIN_AST_NODE: u64 = 0x03;
const DOMAIN_DEFINITION: u64 = 0x04;
const DOMAIN_ARTIFACT: u64 = 0x05;

/// Stable identifier for a source file.
///
/// `SourceFileId` is derived from a logical path when the caller has one. Tests
/// and scratch compilation use [`SourceFileId::ANONYMOUS`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SourceFileId(
    /// Stable raw identifier.
    pub u64,
);

impl SourceFileId {
    /// Source id used when the caller does not provide a logical path.
    pub const ANONYMOUS: Self = Self(0x4341_4952_4e5f_5352);

    /// Builds a source id from a stable logical path.
    #[must_use]
    pub fn from_path(path: &str) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_SOURCE_FILE);
        builder.mix_str(path);
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable-ish CST node identity.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SyntaxNodeId(
    /// Stable raw identifier.
    pub u64,
);

impl SyntaxNodeId {
    /// Starts a builder for one syntax node.
    #[must_use]
    pub fn builder(source_file: SourceFileId, kind_tag: u64) -> StableIdBuilder {
        let mut builder = StableIdBuilder::new(DOMAIN_SYNTAX_NODE);
        builder.mix_u64(source_file.raw());
        builder.mix_u64(kind_tag);
        builder
    }

    /// Builds a syntax node id from a completed stable-id builder.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable-ish AST node identity.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AstNodeId(
    /// Stable raw identifier.
    pub u64,
);

impl AstNodeId {
    /// Builds an AST node id from its source CST node id.
    #[must_use]
    pub fn from_syntax(syntax: SyntaxNodeId) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_AST_NODE);
        builder.mix_u64(syntax.raw());
        Self(builder.finish())
    }

    /// Builds an AST child id from a syntax parent and stable local name.
    #[must_use]
    pub fn child(parent: SyntaxNodeId, tag: u64, name: &str) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_AST_NODE);
        builder.mix_u64(parent.raw());
        builder.mix_u64(tag);
        builder.mix_str(name);
        Self(builder.finish())
    }

    /// Builds an AST node id for a compiler-generated node with source provenance.
    #[must_use]
    pub fn generated(
        source_file: SourceFileId,
        tag: u64,
        name: &str,
        start: usize,
        end: usize,
    ) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_AST_NODE);
        builder.mix_u64(source_file.raw());
        builder.mix_u64(tag);
        builder.mix_str(name);
        builder.mix_u64(u64::try_from(start).unwrap_or(u64::MAX));
        builder.mix_u64(u64::try_from(end).unwrap_or(u64::MAX));
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Kind of named definition that can receive a stable [`DefinitionId`].
///
/// Each variant's discriminant is its stable tag, mixed into the ids of
/// its definitions. A new variant takes one past the highest tag, and a
/// removed variant's tag is never reused; the compiler rejects two
/// variants with the same tag.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum DefinitionKind {
    /// Language environment definition.
    LanguageEnvironment = 7,
    /// Grammar definition.
    Grammar = 8,
    /// Grammar rule definition.
    GrammarRule = 9,
    /// Semantic action definition.
    SemanticAction = 10,
    /// Class definition.
    Class = 1,
    /// Slot definition within a class.
    Slot = 2,
    /// Generic function declaration.
    Generic = 3,
    /// Method definition.
    Method = 4,
    /// Direct function definition.
    Function = 5,
    /// Immutable top-level constant definition.
    Constant = 11,
    /// Local binding.
    Local = 6,
}

impl DefinitionKind {
    const fn tag(self) -> u64 {
        self as u64
    }
}

/// Stable identity for a named compiler definition.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DefinitionId(
    /// Stable raw identifier.
    pub u64,
);

impl DefinitionId {
    /// Builds a stable definition id from a source, kind, name, and discriminator.
    ///
    /// The discriminator is empty for globally named definitions. Nested or
    /// overloaded definitions use it to include their owner or specializer.
    #[must_use]
    pub fn new(
        source_file: SourceFileId,
        kind: DefinitionKind,
        name: &str,
        discriminator: &str,
    ) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_DEFINITION);
        builder.mix_u64(source_file.raw());
        builder.mix_u64(kind.tag());
        builder.mix_str(name);
        builder.mix_str(discriminator);
        Self(builder.finish())
    }

    /// Returns the raw identifier bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Kind of compiler artifact tracked by incremental invalidation.
///
/// Each variant's discriminant is its stable tag, mixed into artifact keys,
/// and the order kinds sort in. A new variant takes one past the highest
/// tag, and a removed variant's tag is never reused; the compiler rejects
/// two variants with the same tag.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
#[repr(u8)]
pub enum ArtifactKind {
    /// Source text input.
    Source = 1,
    /// Language environment resolution result.
    EnvironmentResolution = 13,
    /// Resolved grammar stack result.
    GrammarStack = 14,
    /// Grammar frontend parse report.
    GrammarParse = 15,
    /// Parse/CST result.
    Parse = 2,
    /// AST lowering result.
    Ast = 3,
    /// Binding and semantic symbol table result.
    Bind = 4,
    /// Aggregated compiler diagnostics.
    Diagnostics = 5,
    /// Method lookup result.
    MethodLookup = 6,
    /// Lowered method body.
    MethodLowering = 7,
    /// Tape method body.
    MethodTape = 8,
    /// Generic dispatcher body.
    GenericDispatcher = 9,
    /// Whole lowered executable IR module.
    LoweredModule = 10,
    /// Whole tape program.
    TapeProgram = 11,
    /// Runtime result for a requested `main` run.
    RuntimeResult = 12,
    /// Soft typing decisions for one function, method, or constant.
    DefinitionTyping = 17,
    /// Soft typing result for the whole module.
    Typing = 16,
    /// Session-owned soft typing configuration input.
    TypingConfig = 18,
    /// Semantic output of one function, method, generic, or constant.
    DefinitionSem = 19,
    /// Type contract of one definition: what code compiled against it trusts.
    Contract = 20,
    /// Method set and specializers of one generic.
    MethodTable = 21,
    /// Cone of one class: its subclasses and whether it is closed.
    ClassCone = 22,
    /// Session-owned codegen policy input.
    CodegenPolicy = 23,
    /// Whether one definition is held to strict typing, and why.
    Strictness = 24,
}

impl ArtifactKind {
    /// Returns the stable kebab-case artifact kind label used in dumps and tools.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::EnvironmentResolution => "environment-resolution",
            Self::GrammarStack => "grammar-stack",
            Self::GrammarParse => "grammar-parse",
            Self::Parse => "parse",
            Self::Ast => "ast",
            Self::Bind => "bind",
            Self::Diagnostics => "diagnostics",
            Self::MethodLookup => "method-lookup",
            Self::MethodLowering => "method-lowering",
            Self::MethodTape => "method-tape",
            Self::GenericDispatcher => "generic-dispatcher",
            Self::LoweredModule => "lowered-module",
            Self::TapeProgram => "tape-program",
            Self::RuntimeResult => "runtime-result",
            Self::DefinitionTyping => "definition-typing",
            Self::Typing => "typing",
            Self::TypingConfig => "typing-config",
            Self::DefinitionSem => "definition-sem",
            Self::Contract => "contract",
            Self::MethodTable => "method-table",
            Self::ClassCone => "class-cone",
            Self::CodegenPolicy => "codegen-policy",
            Self::Strictness => "strictness",
        }
    }

    const fn tag(self) -> u64 {
        self as u64
    }
}

/// Stable key for an incremental compiler artifact.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ArtifactKey {
    /// Source file that owns the artifact.
    pub source_file: SourceFileId,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Optional definition owning this artifact.
    pub definition: Option<DefinitionId>,
    raw: u64,
}

impl ArtifactKey {
    /// Builds an artifact key.
    #[must_use]
    pub fn new(
        source_file: SourceFileId,
        kind: ArtifactKind,
        definition: Option<DefinitionId>,
    ) -> Self {
        let mut builder = StableIdBuilder::new(DOMAIN_ARTIFACT);
        builder.mix_u64(source_file.raw());
        builder.mix_u64(kind.tag());
        builder.mix_u64(definition.map_or(0, DefinitionId::raw));
        Self {
            source_file,
            kind,
            definition,
            raw: builder.finish(),
        }
    }

    /// Returns the raw key used by the invalidation backend.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

/// Tiny deterministic hash builder for compiler-local stable IDs.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StableIdBuilder {
    state: u64,
}

impl StableIdBuilder {
    /// Starts a stable id in a domain.
    #[must_use]
    pub const fn new(domain: u64) -> Self {
        Self {
            state: FNV_OFFSET ^ domain,
        }
    }

    /// Mixes an integer into this id.
    pub fn mix_u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.mix_byte(byte);
        }
    }

    /// Mixes UTF-8 text into this id.
    pub fn mix_str(&mut self, value: &str) {
        self.mix_u64(u64::try_from(value.len()).unwrap_or(u64::MAX));
        for byte in value.as_bytes() {
            self.mix_byte(*byte);
        }
    }

    /// Returns the completed raw id.
    #[must_use]
    pub const fn finish(self) -> u64 {
        self.state
    }

    fn mix_byte(&mut self, byte: u8) {
        self.state ^= u64::from(byte);
        self.state = self.state.wrapping_mul(FNV_PRIME);
    }
}

impl fmt::Display for SourceFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sf{:016x}", self.0)
    }
}

impl fmt::Display for SyntaxNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "syn{:016x}", self.0)
    }
}

impl fmt::Display for AstNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ast{:016x}", self.0)
    }
}

impl fmt::Display for DefinitionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "def{:016x}", self.0)
    }
}

impl fmt::Display for ArtifactKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "art{:016x}", self.raw)
    }
}
