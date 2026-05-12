//! Compiler session API and inspectable stage artifacts.
//!
//! The session layer owns source-file identity, per-stage artifact snapshots,
//! stable artifact-key reporting, dumps, and edit comparisons. It explicitly
//! does not own language semantics, lowering rules, runtime object storage, or
//! `execution_tape` bytecode internals.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

use execution_tape::trace::TraceMask;
use execution_tape::value::Value;
use execution_tape::vm::{Limits, Vm};

use crate::diagnostic::Diagnostic;
use crate::identity::{ArtifactKey, DefinitionId, DefinitionKind, SourceFileId, StableIdBuilder};
use crate::incremental::{ArtifactEdge, ArtifactReport, QueryArtifact, QueryInvalidation};
use crate::runtime::{CairnHost, HostObjectKind, RuntimeLayout};
use crate::syntax::{CstElement, CstNode, SyntaxTree, TokenKind};
use crate::{ast, lower, sem, syntax, tape};

/// Named source file accepted by a compiler session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    /// Stable source file id.
    pub id: SourceFileId,
    /// Caller-facing source name or path.
    pub name: String,
    /// Source text.
    pub source: String,
}

impl SourceFile {
    /// Creates a named source file using `name` as the stable identity input.
    #[must_use]
    pub fn named(name: &str, source: &str) -> Self {
        Self::with_id(SourceFileId::from_path(name), name, source)
    }

    /// Creates a named source file with an explicit id.
    #[must_use]
    pub fn with_id(id: SourceFileId, name: &str, source: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            source: source.to_string(),
        }
    }
}

/// Runtime mode for a session compile.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    /// Compile only.
    CompileOnly,
    /// Compile and run zero-argument `main` when possible.
    RunMain,
}

/// Result of running `main` through the session API.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeResult {
    /// `main` returned an integer result.
    ReturnedI64(i64),
    /// No `main` function was available.
    MissingMain,
    /// `main` has parameters.
    MainHasParameters,
    /// Runtime trapped.
    Trap {
        /// Trap details.
        message: String,
    },
    /// `main` returned a non-integer result.
    MainReturnedNonI64,
}

/// Stage that can be rendered as a text dump.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DumpStage {
    /// CST/syntax dump.
    Syntax,
    /// AST dump.
    Ast,
    /// Semantic IR dump.
    Sem,
    /// Metaobject protocol summary.
    Mop,
    /// Environment object protocol summary.
    Eop,
    /// Lowered executable IR dump.
    Lowered,
    /// `execution_tape` disassembly.
    Tape,
    /// All stage dumps.
    All,
}

/// Stable identifier for an environment object projected by the session layer.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct EnvironmentObjectId(String);

impl EnvironmentObjectId {
    /// Creates an environment object id from a stable text form.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the stable text form of this id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EnvironmentObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Environment-level kind for a projected object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentKind {
    /// A named source file known to the session.
    SourceFile,
    /// A semantic class metaobject.
    Class,
    /// A semantic slot metaobject.
    Slot,
    /// A semantic accessor metaobject.
    Accessor,
    /// A semantic generic function metaobject.
    Generic,
    /// A semantic method metaobject.
    Method,
    /// A direct function definition.
    Function,
    /// A compiler diagnostic.
    Diagnostic,
    /// A runtime layout row.
    RuntimeLayoutRow,
    /// A query artifact.
    Artifact,
    /// An explicitly projected host-owned environment object.
    HostObject,
}

impl EnvironmentKind {
    /// Returns the stable dump label for this environment kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceFile => "source-file",
            Self::Class => "class",
            Self::Slot => "slot",
            Self::Accessor => "accessor",
            Self::Generic => "generic",
            Self::Method => "method",
            Self::Function => "function",
            Self::Diagnostic => "diagnostic",
            Self::RuntimeLayoutRow => "runtime-layout-row",
            Self::Artifact => "artifact",
            Self::HostObject => "host-object",
        }
    }
}

/// Origin of a projected environment object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentOrigin {
    /// The object is authored by Cairn source.
    SourceAuthored,
    /// The object is declared by explicit host-backed metadata.
    HostBacked,
    /// The object is generated by compiler/session work.
    CompilerGenerated,
    /// The object is contributed by an explicit host projection adapter.
    HostProvided,
}

impl EnvironmentOrigin {
    /// Returns the stable dump label for this origin.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceAuthored => "source-authored",
            Self::HostBacked => "host-backed",
            Self::CompilerGenerated => "compiler-generated",
            Self::HostProvided => "host-provided",
        }
    }
}

/// Storage ownership for a projected environment object.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentStorage {
    /// The object is represented by Cairn source definitions.
    SourceDefined,
    /// The object is represented by explicit host registry metadata.
    HostRegistry,
    /// The object is represented by an adapter-owned host slot bridge.
    HostAdapter,
    /// The object is represented by cached compiler/session artifacts.
    CachedArtifact,
    /// The object is represented by host-owned state outside Cairn.
    HostOwned,
}

impl EnvironmentStorage {
    /// Returns the stable dump label for this storage owner.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceDefined => "source-defined",
            Self::HostRegistry => "host-registry",
            Self::HostAdapter => "host-adapter",
            Self::CachedArtifact => "cached-artifact",
            Self::HostOwned => "host-owned",
        }
    }
}

/// Stable workbench affordance names for environment objects.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentAffordance {
    /// Inspect this object.
    Inspect,
    /// Dump the CST/syntax stage.
    DumpSyntax,
    /// Dump the AST stage.
    DumpAst,
    /// Dump semantic IR.
    DumpSem,
    /// Dump MOP facts.
    DumpMop,
    /// Dump EOP facts.
    DumpEop,
    /// Dump lowered executable IR.
    DumpLowered,
    /// Dump `execution_tape` disassembly.
    DumpTape,
    /// Show generic dispatch information.
    ShowDispatch,
    /// Show runtime layout information.
    ShowRuntimeLayout,
    /// Show query artifact dependencies.
    ShowDependencies,
    /// Show the accessor for a slot.
    ShowAccessor,
    /// Show the slot read by an accessor.
    ShowSlot,
    /// Run executable code.
    Run,
    /// Edit a function or method body.
    EditBody,
    /// Add a method to a generic function.
    AddMethod,
    /// Show artifact reports.
    Artifacts,
    /// Diff two source versions.
    Diff,
    /// Open the source location for an object.
    OpenSource,
    /// Dump query artifacts.
    DumpArtifacts,
    /// Show the slots owned by a class.
    ShowSlots,
}

impl EnvironmentAffordance {
    /// Returns the stable affordance name used in EOP dumps.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::DumpSyntax => "dump-syntax",
            Self::DumpAst => "dump-ast",
            Self::DumpSem => "dump-sem",
            Self::DumpMop => "dump-mop",
            Self::DumpEop => "dump-eop",
            Self::DumpLowered => "dump-lowered",
            Self::DumpTape => "dump-tape",
            Self::ShowDispatch => "show-dispatch",
            Self::ShowRuntimeLayout => "show-runtime-layout",
            Self::ShowDependencies => "show-dependencies",
            Self::ShowAccessor => "show-accessor",
            Self::ShowSlot => "show-slot",
            Self::Run => "run",
            Self::EditBody => "edit-body",
            Self::AddMethod => "add-method",
            Self::Artifacts => "artifacts",
            Self::Diff => "diff",
            Self::OpenSource => "open-source",
            Self::DumpArtifacts => "dump-artifacts",
            Self::ShowSlots => "show-slots",
        }
    }
}

/// Presentation hints for an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresentationFacet {
    /// Human-facing stable title.
    pub title: String,
}

impl PresentationFacet {
    /// Creates a presentation facet with a stable title.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
        }
    }
}

/// Action names exposed by an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffordanceFacet {
    /// Stable affordance names in presentation order.
    pub names: Vec<EnvironmentAffordance>,
}

impl AffordanceFacet {
    /// Creates an affordance facet.
    #[must_use]
    pub fn new(names: Vec<EnvironmentAffordance>) -> Self {
        Self { names }
    }

    /// Returns true when this facet exposes `name`.
    #[must_use]
    pub fn contains(&self, name: EnvironmentAffordance) -> bool {
        self.names.contains(&name)
    }
}

/// Executable behavior summary for an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionFacet {
    /// Stable detail text rendered after `execution`.
    pub detail: String,
}

impl ExecutionFacet {
    /// Creates an execution facet.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Inspection summary for an environment object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectionFacet {
    /// Stable detail text rendered after `inspection`.
    pub detail: String,
}

impl InspectionFacet {
    /// Creates an inspection facet.
    #[must_use]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// Typed EOP record projected by the session or an explicit host adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentObject {
    /// Stable environment object id.
    pub id: EnvironmentObjectId,
    /// Environment-level object kind.
    pub kind: EnvironmentKind,
    /// Meta-level class label for this environment object.
    pub meta_class: String,
    /// Where this object comes from.
    pub origin: EnvironmentOrigin,
    /// Who owns the object's current representation.
    pub storage: EnvironmentStorage,
    /// Presentation hints.
    pub presentation: PresentationFacet,
    /// Supported environment actions.
    pub affordances: AffordanceFacet,
    /// Executable behavior summary, when applicable.
    pub execution: Option<ExecutionFacet>,
    /// Inspection details.
    pub inspection: InspectionFacet,
}

/// Sink for structured environment object projection.
pub trait EnvironmentObjectSink {
    /// Emits one environment object.
    fn emit(&mut self, object: EnvironmentObject);
}

impl EnvironmentObjectSink for Vec<EnvironmentObject> {
    fn emit(&mut self, object: EnvironmentObject) {
        self.push(object);
    }
}

/// Explicit host-owned environment projection seam.
///
/// Implementors contribute host-owned objects into an EOP sink. This is
/// adapter-owned projection, not automatic runtime heap enumeration.
pub trait HostEnvironmentProvider {
    /// Projects host-owned environment objects into `sink`.
    fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink);
}

/// Fully inspected compilation artifacts for one source file.
#[derive(Clone, Debug)]
pub struct SessionArtifacts {
    /// Source file.
    pub source: SourceFile,
    /// Parsed syntax tree.
    pub syntax: SyntaxTree,
    /// Surface AST.
    pub ast: ast::Module,
    /// Semantic IR.
    pub sem: sem::Module,
    /// Lowered executable IR, when lowering succeeded.
    pub lowered: Option<lower::Module>,
    /// Lowering error, when lowering failed.
    pub lower_error: Option<lower::LowerError>,
    /// Verified tape module, when emission succeeded.
    pub tape: Option<tape::TapeModule>,
    /// Tape emission error, when emission failed.
    pub tape_error: Option<tape::TapeError>,
    /// Aggregated diagnostics from the compiler pipeline.
    pub diagnostics: Vec<Diagnostic>,
    /// Runtime result when [`RuntimeMode::RunMain`] was requested.
    pub runtime_result: Option<RuntimeResult>,
    invalidation: QueryInvalidation,
}

impl SessionArtifacts {
    /// Returns stable artifact reports for this compilation.
    #[must_use]
    pub fn artifact_reports(&self) -> Vec<ArtifactReport> {
        self.invalidation.artifact_reports()
    }

    /// Returns structured environment objects projected from these artifacts.
    #[must_use]
    pub fn environment_objects(&self) -> Vec<EnvironmentObject> {
        let mut objects = Vec::new();
        self.project_environment(&mut objects);
        objects
    }

    /// Projects structured environment objects from these artifacts into `sink`.
    pub fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink) {
        project_session_environment(self, sink);
    }

    /// Returns compiler and host-provided environment objects.
    #[must_use]
    pub fn environment_objects_with_host(
        &self,
        host: &dyn HostEnvironmentProvider,
    ) -> Vec<EnvironmentObject> {
        let mut objects = Vec::new();
        self.project_environment_with_host(host, &mut objects);
        objects
    }

    /// Projects compiler and explicit host-provided environment objects.
    pub fn project_environment_with_host(
        &self,
        host: &dyn HostEnvironmentProvider,
        sink: &mut dyn EnvironmentObjectSink,
    ) {
        self.project_environment(sink);
        host.project_environment(sink);
    }

    /// Returns this artifact set's source file identity.
    #[must_use]
    pub const fn source_file(&self) -> SourceFileId {
        self.source.id
    }

    /// Dumps one stage as text.
    #[must_use]
    pub fn dump_stage(&self, stage: DumpStage) -> String {
        match stage {
            DumpStage::Syntax => dump_syntax(&self.syntax),
            DumpStage::Ast => dump_ast(&self.ast),
            DumpStage::Sem => dump_sem(&self.sem),
            DumpStage::Mop => dump_mop(&self.sem),
            DumpStage::Eop => dump_eop(self),
            DumpStage::Lowered => self.lowered.as_ref().map_or_else(
                || format!("lowered unavailable: {:?}\n", self.lower_error),
                lower::Module::dump,
            ),
            DumpStage::Tape => self.tape.as_ref().map_or_else(
                || format!("tape unavailable: {:?}\n", self.tape_error),
                |tape| {
                    format!(
                        "{}",
                        execution_tape::disasm::disassemble_verified(&tape.program)
                    )
                },
            ),
            DumpStage::All => {
                let mut out = String::new();
                for stage in [
                    DumpStage::Syntax,
                    DumpStage::Ast,
                    DumpStage::Sem,
                    DumpStage::Mop,
                    DumpStage::Eop,
                    DumpStage::Lowered,
                    DumpStage::Tape,
                ] {
                    let _ = writeln!(out, "== {stage:?} ==");
                    out.push_str(&self.dump_stage(stage));
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
                out
            }
        }
    }

    /// Dumps stable artifact keys and dependencies.
    #[must_use]
    pub fn dump_artifact_graph(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "artifact graph for {} ({})",
            self.source.name, self.source.id
        );
        let _ = writeln!(out, "artifacts:");
        for report in self.invalidation.artifact_reports() {
            let _ = writeln!(out, "  {} {}", report.key, artifact_label(report.artifact));
        }
        let _ = writeln!(out, "dependencies:");
        for edge in self.invalidation.artifact_edges() {
            let _ = writeln!(
                out,
                "  {} <- {}",
                artifact_label(edge.dependent),
                artifact_label(edge.dependency)
            );
        }
        out
    }
}

/// Diff between two compiled source versions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDiff {
    /// Definitions present in both versions with equal fingerprints.
    pub preserved: Vec<DefinitionReport>,
    /// Added, removed, or modified definitions.
    pub changed: Vec<DefinitionChange>,
    /// Query artifacts affected by the detected edit shape.
    pub affected: Vec<ArtifactReport>,
}

impl SessionDiff {
    /// Renders this diff as text.
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "preserved definitions:");
        for definition in &self.preserved {
            let _ = writeln!(
                out,
                "  {} {:?} {}",
                definition.id, definition.kind, definition.name
            );
        }
        let _ = writeln!(out, "changed definitions:");
        for change in &self.changed {
            let _ = writeln!(
                out,
                "  {:?} {} {:?} {}",
                change.change, change.definition.id, change.definition.kind, change.definition.name
            );
        }
        let _ = writeln!(out, "affected artifacts:");
        for report in &self.affected {
            let _ = writeln!(out, "  {} {}", report.key, artifact_label(report.artifact));
        }
        out
    }
}

/// Stable definition report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionReport {
    /// Stable definition id.
    pub id: DefinitionId,
    /// Definition kind.
    pub kind: DefinitionKind,
    /// Display name.
    pub name: String,
}

/// Kind of definition change.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DefinitionChangeKind {
    /// Definition was added.
    Added,
    /// Definition was removed.
    Removed,
    /// Definition kept its id but changed content.
    Modified,
}

/// Changed definition report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionChange {
    /// Definition report.
    pub definition: DefinitionReport,
    /// Change kind.
    pub change: DefinitionChangeKind,
}

/// In-memory compiler session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerSession {
    files: Vec<SourceFile>,
}

impl CompilerSession {
    /// Creates an empty session.
    #[must_use]
    pub const fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Adds a named source file and returns its id.
    pub fn add_source(&mut self, name: &str, source: &str) -> SourceFileId {
        let file = SourceFile::named(name, source);
        let id = file.id;
        self.files.push(file);
        id
    }

    /// Adds a source file with an explicit id.
    pub fn add_source_with_id(&mut self, id: SourceFileId, name: &str, source: &str) {
        self.files.push(SourceFile::with_id(id, name, source));
    }

    /// Returns a source file by id.
    #[must_use]
    pub fn source(&self, id: SourceFileId) -> Option<&SourceFile> {
        self.files.iter().find(|file| file.id == id)
    }

    /// Compiles one source by id.
    #[must_use]
    pub fn compile(&self, id: SourceFileId, mode: RuntimeMode) -> Option<SessionArtifacts> {
        self.source(id).map(|file| compile_file(file, mode))
    }

    /// Compiles a named source without storing it in the session.
    #[must_use]
    pub fn compile_source(name: &str, source: &str, mode: RuntimeMode) -> SessionArtifacts {
        compile_file(&SourceFile::named(name, source), mode)
    }

    /// Compiles a named source with host-backed metaobject declarations.
    #[must_use]
    pub fn compile_source_with_host_registry(
        name: &str,
        source: &str,
        mode: RuntimeMode,
        registry: &sem::HostMetaRegistry,
    ) -> SessionArtifacts {
        compile_file_with_host_registry(&SourceFile::named(name, source), mode, registry)
    }

    /// Compiles source with an explicit source file id without storing it.
    #[must_use]
    pub fn compile_source_with_id(
        id: SourceFileId,
        name: &str,
        source: &str,
        mode: RuntimeMode,
    ) -> SessionArtifacts {
        compile_file(&SourceFile::with_id(id, name, source), mode)
    }

    /// Compiles source with an explicit source file id and host-backed registry.
    #[must_use]
    pub fn compile_source_with_id_and_host_registry(
        id: SourceFileId,
        name: &str,
        source: &str,
        mode: RuntimeMode,
        registry: &sem::HostMetaRegistry,
    ) -> SessionArtifacts {
        compile_file_with_host_registry(&SourceFile::with_id(id, name, source), mode, registry)
    }

    /// Diffs two source versions.
    #[must_use]
    pub fn diff(before: &SessionArtifacts, after: &SessionArtifacts) -> SessionDiff {
        diff_artifacts(before, after)
    }
}

/// Incremental compiler session with an in-memory artifact cache.
///
/// This session owns source edit state and cache reporting. It reuses the
/// existing parser, AST lowering, semantic lowering, executable IR lowering,
/// tape emission, and runtime execution stages.
#[derive(Clone, Debug, Default)]
pub struct IncrementalCompilerSession {
    files: Vec<CachedSourceFile>,
}

impl IncrementalCompilerSession {
    /// Creates an empty incremental session.
    #[must_use]
    pub const fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Adds a named source file and returns its stable id.
    pub fn add_source(&mut self, name: &str, source: &str) -> SourceFileId {
        let file = SourceFile::named(name, source);
        let id = file.id;
        self.files.push(CachedSourceFile::new(file));
        id
    }

    /// Adds a source file with an explicit stable id.
    pub fn add_source_with_id(&mut self, id: SourceFileId, name: &str, source: &str) {
        self.files
            .push(CachedSourceFile::new(SourceFile::with_id(id, name, source)));
    }

    /// Edits a source file's text while preserving its stable id and name.
    pub fn edit_source(&mut self, id: SourceFileId, source: &str) -> Option<()> {
        let file = self.file_mut(id)?;
        file.source.source = source.to_string();
        Some(())
    }

    /// Returns a source file by id.
    #[must_use]
    pub fn source(&self, id: SourceFileId) -> Option<&SourceFile> {
        self.files
            .iter()
            .find(|file| file.source.id == id)
            .map(|file| &file.source)
    }

    /// Compiles one source file through the incremental cache.
    pub fn compile(&mut self, id: SourceFileId, mode: RuntimeMode) -> Option<IncrementalUpdate> {
        let index = self.files.iter().position(|file| file.source.id == id)?;
        Some(self.compile_index(index, mode))
    }

    /// Edits and compiles one source file through the incremental cache.
    pub fn edit_and_compile(
        &mut self,
        id: SourceFileId,
        source: &str,
        mode: RuntimeMode,
    ) -> Option<IncrementalUpdate> {
        self.edit_source(id, source)?;
        self.compile(id, mode)
    }

    fn file_mut(&mut self, id: SourceFileId) -> Option<&mut CachedSourceFile> {
        self.files.iter_mut().find(|file| file.source.id == id)
    }

    fn compile_index(&mut self, index: usize, mode: RuntimeMode) -> IncrementalUpdate {
        let source = self.files[index].source.clone();
        let previous_artifacts = self.files[index].artifacts.clone();
        let previous_cache = self.files[index].cache.clone();
        let previous_source = previous_artifacts
            .as_ref()
            .map(|artifacts| artifacts.source.source.clone());
        let artifacts = self.files[index].artifacts_for(&source, mode);
        let diff = previous_artifacts
            .as_ref()
            .map(|before| CompilerSession::diff(before, &artifacts));
        let reports = cache_reports(
            previous_cache.as_ref(),
            diff.as_ref(),
            previous_source.as_deref(),
            &artifacts,
        );
        let cache = ArtifactCache::from_update(&artifacts, &reports);
        let revision = self.files[index].revision.saturating_add(1);

        self.files[index].source = source.clone();
        self.files[index].artifacts = Some(artifacts.clone());
        self.files[index].cache = Some(cache);
        self.files[index].revision = revision;

        IncrementalUpdate {
            source,
            artifacts,
            diff,
            reports,
            revision,
        }
    }
}

#[derive(Clone, Debug)]
struct CachedSourceFile {
    source: SourceFile,
    artifacts: Option<SessionArtifacts>,
    cache: Option<ArtifactCache>,
    revision: u64,
}

impl CachedSourceFile {
    const fn new(source: SourceFile) -> Self {
        Self {
            source,
            artifacts: None,
            cache: None,
            revision: 0,
        }
    }

    fn artifacts_for(&self, source: &SourceFile, mode: RuntimeMode) -> SessionArtifacts {
        if let Some(previous) = &self.artifacts
            && previous.source == *source
        {
            return artifacts_for_mode(previous, mode);
        }
        compile_file(source, mode)
    }
}

fn artifacts_for_mode(previous: &SessionArtifacts, mode: RuntimeMode) -> SessionArtifacts {
    let mut artifacts = previous.clone();
    artifacts.runtime_result = match mode {
        RuntimeMode::CompileOnly => None,
        RuntimeMode::RunMain => previous.runtime_result.clone().or_else(|| {
            previous
                .tape
                .as_ref()
                .map(|tape| run_main(&previous.sem, tape))
        }),
    };
    artifacts
}

/// Result of compiling one file through [`IncrementalCompilerSession`].
#[derive(Clone, Debug)]
pub struct IncrementalUpdate {
    /// Source file after this update.
    pub source: SourceFile,
    /// Current compilation artifacts.
    pub artifacts: SessionArtifacts,
    /// Semantic diff from the previous cached snapshot, if any.
    pub diff: Option<SessionDiff>,
    /// Cache report for every artifact known to this update's graph.
    pub reports: Vec<CacheArtifactReport>,
    /// Monotonic per-file session revision.
    pub revision: u64,
}

impl IncrementalUpdate {
    /// Returns dependency edges for this update's artifact graph.
    #[must_use]
    pub fn dependency_edges(&self) -> &[ArtifactEdge] {
        self.artifacts.invalidation.artifact_edges()
    }

    /// Returns stable artifact reports for this update.
    #[must_use]
    pub fn artifact_reports(&self) -> Vec<ArtifactReport> {
        self.artifacts.artifact_reports()
    }

    /// Dumps cache status, counters, and semantic edit impact.
    #[must_use]
    pub fn dump_cache_report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "incremental report for {} ({}) revision {}",
            self.source.name, self.source.id, self.revision
        );
        let _ = writeln!(out, "summary:");
        let _ = writeln!(out, "  reused {}", self.reused_count());
        let _ = writeln!(out, "  recomputed {}", self.recomputed_count());
        let _ = writeln!(out, "  invalidated {}", self.invalidated_count());
        let _ = writeln!(out, "  affected {}", self.affected_count());
        let _ = writeln!(out, "artifacts:");
        for report in &self.reports {
            let _ = writeln!(
                out,
                "  {:?}{} {} invalidated={} affected={} work={} ticks={}",
                report.action,
                if report.action == CacheAction::Reused {
                    "    "
                } else {
                    ""
                },
                artifact_label(report.artifact),
                report.invalidated,
                report.affected,
                report.metrics.work_units,
                report.metrics.elapsed_ticks
            );
        }
        let _ = writeln!(out, "dependencies:");
        for edge in self.dependency_edges() {
            let _ = writeln!(
                out,
                "  {} <- {}",
                artifact_label(edge.dependent),
                artifact_label(edge.dependency)
            );
        }
        if let Some(diff) = &self.diff {
            out.push_str("diff:\n");
            out.push_str(&diff.dump());
        }
        out
    }

    /// Number of artifacts reused by this update.
    #[must_use]
    pub fn reused_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.action == CacheAction::Reused)
            .count()
    }

    /// Number of artifacts recomputed by this update.
    #[must_use]
    pub fn recomputed_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.action == CacheAction::Recomputed)
            .count()
    }

    /// Number of previously cached artifacts invalidated by this update.
    #[must_use]
    pub fn invalidated_count(&self) -> usize {
        self.reports
            .iter()
            .filter(|report| report.invalidated)
            .count()
    }

    /// Number of artifacts marked semantically affected by this update.
    #[must_use]
    pub fn affected_count(&self) -> usize {
        self.reports.iter().filter(|report| report.affected).count()
    }
}

/// Cache action for one artifact in an incremental update.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CacheAction {
    /// A previous cache entry was valid for this artifact.
    Reused,
    /// The artifact was produced for this update.
    Recomputed,
}

/// Deterministic per-artifact counters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ArtifactMetrics {
    /// Logical work units charged for the artifact action.
    pub work_units: u64,
    /// Logical elapsed ticks charged for the artifact action.
    pub elapsed_ticks: u64,
}

/// Cache status for one artifact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CacheArtifactReport {
    /// Query artifact.
    pub artifact: QueryArtifact,
    /// Stable artifact key.
    pub key: ArtifactKey,
    /// Cache action.
    pub action: CacheAction,
    /// Whether a previous entry existed and was replaced.
    pub invalidated: bool,
    /// Whether semantic edit analysis marked this artifact affected.
    pub affected: bool,
    /// Deterministic counters for this artifact action.
    pub metrics: ArtifactMetrics,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct ArtifactFingerprint(u64);

#[derive(Clone, Debug, Eq, PartialEq)]
struct CachedArtifact {
    artifact: QueryArtifact,
    key: ArtifactKey,
    fingerprint: ArtifactFingerprint,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ArtifactCache {
    entries: Vec<CachedArtifact>,
}

impl ArtifactCache {
    fn from_update(artifacts: &SessionArtifacts, reports: &[CacheArtifactReport]) -> Self {
        Self {
            entries: reports
                .iter()
                .map(|report| CachedArtifact {
                    artifact: report.artifact,
                    key: report.key,
                    fingerprint: artifact_fingerprint(report.artifact, artifacts),
                })
                .collect(),
        }
    }

    fn get(&self, key: ArtifactKey) -> Option<&CachedArtifact> {
        self.entries.iter().find(|entry| entry.key == key)
    }
}

fn cache_reports(
    previous: Option<&ArtifactCache>,
    diff: Option<&SessionDiff>,
    previous_source: Option<&str>,
    artifacts: &SessionArtifacts,
) -> Vec<CacheArtifactReport> {
    let source_changed = previous_source.is_some_and(|source| source != artifacts.source.source);
    let affected = diff.map_or(&[][..], |diff| diff.affected.as_slice());
    artifacts
        .artifact_reports()
        .into_iter()
        .map(|report| {
            let fingerprint = artifact_fingerprint(report.artifact, artifacts);
            let previous = previous.and_then(|cache| cache.get(report.key));
            let action = if previous.is_some_and(|entry| entry.fingerprint == fingerprint) {
                CacheAction::Reused
            } else {
                CacheAction::Recomputed
            };
            let invalidated = previous.is_some()
                && action == CacheAction::Recomputed
                && (source_changed || affected.iter().any(|item| item.key == report.key));
            let affected = affected.iter().any(|item| item.key == report.key);
            let metrics = match action {
                CacheAction::Reused => ArtifactMetrics::default(),
                CacheAction::Recomputed => ArtifactMetrics {
                    work_units: artifact_work_units(report.artifact, artifacts),
                    elapsed_ticks: 1,
                },
            };
            CacheArtifactReport {
                artifact: report.artifact,
                key: report.key,
                action,
                invalidated,
                affected,
                metrics,
            }
        })
        .collect()
}

fn artifact_fingerprint(
    artifact: QueryArtifact,
    artifacts: &SessionArtifacts,
) -> ArtifactFingerprint {
    let mut builder = StableIdBuilder::new(0x4341_4952_4e5f_4341);
    builder.mix_u64(artifact.key(artifacts.source.id).raw());
    match artifact {
        QueryArtifact::Source | QueryArtifact::Parse => {
            builder.mix_str(&artifacts.source.source);
        }
        QueryArtifact::Ast => {
            builder.mix_str(&dump_ast(&artifacts.ast));
        }
        QueryArtifact::Bind => {
            builder.mix_str(&dump_sem(&artifacts.sem));
        }
        QueryArtifact::Diagnostics => {
            mix_diagnostics(&mut builder, &artifacts.diagnostics);
        }
        QueryArtifact::MethodLookup(method) => {
            builder.mix_str(&fingerprint_method_lookup(&artifacts.sem, method));
        }
        QueryArtifact::MethodLowering(method) | QueryArtifact::MethodTape(method) => {
            builder.mix_str(&fingerprint_method_body(&artifacts.sem, method));
        }
        QueryArtifact::GenericDispatcher(generic) => {
            builder.mix_str(&fingerprint_dispatcher(&artifacts.sem, generic));
        }
        QueryArtifact::LoweredModule => {
            builder.mix_str(&fingerprint_lowered(artifacts));
        }
        QueryArtifact::TapeProgram => {
            builder.mix_str(&artifacts.dump_stage(DumpStage::Tape));
        }
        QueryArtifact::RuntimeResult => {
            builder.mix_str(&format!("{:?}", artifacts.runtime_result));
        }
    }
    ArtifactFingerprint(builder.finish())
}

fn mix_diagnostics(builder: &mut StableIdBuilder, diagnostics: &[Diagnostic]) {
    builder.mix_u64(u64::try_from(diagnostics.len()).unwrap_or(u64::MAX));
    for diagnostic in diagnostics {
        builder.mix_str(&diagnostic.message);
        builder.mix_u64(u64::try_from(diagnostic.span.start).unwrap_or(u64::MAX));
        builder.mix_u64(u64::try_from(diagnostic.span.end).unwrap_or(u64::MAX));
    }
}

fn fingerprint_method_lookup(module: &sem::Module, method: DefinitionId) -> String {
    let Some(method) = module.methods.iter().find(|item| item.definition == method) else {
        return "missing-method".into();
    };
    let generic = module
        .generics
        .get(method.generic.0 as usize)
        .map_or(0, |generic| generic.definition.raw());
    let specializer = module
        .classes
        .get(method.specializer.0 as usize)
        .map_or(0, |class| class.definition.raw());
    format!(
        "method-lookup:{}:{}:{}",
        method.definition.raw(),
        generic,
        specializer
    )
}

fn fingerprint_method_body(module: &sem::Module, method: DefinitionId) -> String {
    let Some(method) = module.methods.iter().find(|item| item.definition == method) else {
        return "missing-method-body".into();
    };
    let mut out = fingerprint_method_lookup(module, method.definition);
    out.push(':');
    out.push_str(&fingerprint_sem_block(module, &method.body));
    out
}

fn fingerprint_dispatcher(module: &sem::Module, generic: DefinitionId) -> String {
    let Some(generic) = module
        .generics
        .iter()
        .find(|item| item.definition == generic)
    else {
        return "missing-dispatcher".into();
    };
    let mut out = format!(
        "dispatcher:{}:{}:{}:{}:{}:",
        generic.definition.raw(),
        generic.name,
        generic.arity,
        generic
            .param_kinds
            .iter()
            .map(|kind| kind.map_or_else(
                || "untyped".into(),
                |kind| sem_kind_fingerprint(module, kind)
            ))
            .collect::<Vec<_>>()
            .join(","),
        sem_kind_fingerprint(module, generic.result_kind)
    );
    for method in module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
    {
        let specializer = module
            .classes
            .get(method.specializer.0 as usize)
            .map_or(0, |class| class.definition.raw());
        let _ = write!(out, "{}:{specializer};", method.definition.raw());
    }
    out
}

fn fingerprint_lowered(artifacts: &SessionArtifacts) -> String {
    let Some(lowered) = &artifacts.lowered else {
        return format!("{:?}", artifacts.lower_error);
    };
    strip_span_comments(&lowered.dump())
}

fn strip_span_comments(input: &str) -> String {
    let mut out = String::new();
    for line in input.lines() {
        if let Some((prefix, _)) = line.split_once(" ; span ") {
            out.push_str(prefix);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn artifact_work_units(artifact: QueryArtifact, artifacts: &SessionArtifacts) -> u64 {
    match artifact {
        QueryArtifact::Source => usize_to_u64(artifacts.source.source.len()),
        QueryArtifact::Parse => usize_to_u64(artifacts.source.source.len())
            .saturating_add(usize_to_u64(artifacts.syntax.tokens.len())),
        QueryArtifact::Ast => usize_to_u64(dump_ast(&artifacts.ast).len()),
        QueryArtifact::Bind => usize_to_u64(dump_sem(&artifacts.sem).len()),
        QueryArtifact::Diagnostics => artifacts
            .diagnostics
            .iter()
            .map(|diagnostic| usize_to_u64(diagnostic.message.len()).saturating_add(1))
            .sum(),
        QueryArtifact::MethodLookup(method) => {
            usize_to_u64(fingerprint_method_lookup(&artifacts.sem, method).len())
        }
        QueryArtifact::MethodLowering(method) | QueryArtifact::MethodTape(method) => {
            usize_to_u64(fingerprint_method_body(&artifacts.sem, method).len())
        }
        QueryArtifact::GenericDispatcher(generic) => {
            usize_to_u64(fingerprint_dispatcher(&artifacts.sem, generic).len())
        }
        QueryArtifact::LoweredModule => artifacts.lowered.as_ref().map_or(1, |lowered| {
            usize_to_u64(lowered.functions.len()).saturating_add(
                lowered
                    .functions
                    .iter()
                    .map(|function| usize_to_u64(function.instructions.len()))
                    .sum(),
            )
        }),
        QueryArtifact::TapeProgram => usize_to_u64(artifacts.dump_stage(DumpStage::Tape).len()),
        QueryArtifact::RuntimeResult => 1,
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn compile_file(file: &SourceFile, mode: RuntimeMode) -> SessionArtifacts {
    compile_file_with_host_registry(file, mode, &sem::HostMetaRegistry::new())
}

fn compile_file_with_host_registry(
    file: &SourceFile,
    mode: RuntimeMode,
    registry: &sem::HostMetaRegistry,
) -> SessionArtifacts {
    let syntax = syntax::parse_file(file.id, &file.source);
    let ast = ast::lower(&syntax);
    let sem = sem::lower_with_host_registry(&ast, registry);
    let diagnostics = sem.diagnostics.clone();
    let invalidation = QueryInvalidation::for_module(&sem);

    let (lowered, lower_error) = match lower::lower(&sem) {
        Ok(module) => (Some(module), None),
        Err(error) => (None, Some(error)),
    };
    let (tape, tape_error) = match &lowered {
        Some(lowered) => match tape::emit(lowered) {
            Ok(module) => (Some(module), None),
            Err(error) => (None, Some(error)),
        },
        None => (None, None),
    };
    let runtime_result = match mode {
        RuntimeMode::CompileOnly => None,
        RuntimeMode::RunMain => tape.as_ref().map(|tape| run_main(&sem, tape)),
    };

    SessionArtifacts {
        source: file.clone(),
        syntax,
        ast,
        sem,
        lowered,
        lower_error,
        tape,
        tape_error,
        diagnostics,
        runtime_result,
        invalidation,
    }
}

fn run_main(module: &sem::Module, tape: &tape::TapeModule) -> RuntimeResult {
    let Some(main) = tape.main else {
        return RuntimeResult::MissingMain;
    };
    let Some(main_sem) = module
        .functions
        .iter()
        .find(|function| function.name == "main")
    else {
        return RuntimeResult::MissingMain;
    };
    if !main_sem.params.is_empty() {
        return RuntimeResult::MainHasParameters;
    }

    let host = CairnHost::new(tape.runtime.clone());
    let mut vm = Vm::new(host, Limits::default());
    let output = match vm.run(&tape.program, main, &[], TraceMask::NONE, None) {
        Ok(output) => output,
        Err(trap) => {
            return RuntimeResult::Trap {
                message: format!("{trap:?}"),
            };
        }
    };

    match output.as_slice() {
        [Value::I64(value)] => RuntimeResult::ReturnedI64(*value),
        _ => RuntimeResult::MainReturnedNonI64,
    }
}

fn dump_syntax(tree: &SyntaxTree) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "syntax {} ({})", tree.source_file, tree.source.len());
    dump_cst_node(&mut out, tree, &tree.root, 0);
    out
}

fn dump_cst_node(out: &mut String, tree: &SyntaxTree, node: &CstNode, indent: usize) {
    indent_line(out, indent);
    let _ = writeln!(
        out,
        "{:?} {} span={}..{}",
        node.kind, node.id, node.span.start, node.span.end
    );
    for child in &node.children {
        match child {
            CstElement::Node(child) => dump_cst_node(out, tree, child, indent + 2),
            CstElement::Token(index) => {
                let token = tree.tokens[*index];
                if matches!(token.kind, TokenKind::Whitespace | TokenKind::Comment) {
                    continue;
                }
                indent_line(out, indent + 2);
                let _ = writeln!(
                    out,
                    "token {:?} {:?} span={}..{}",
                    token.kind,
                    token.text(&tree.source),
                    token.span.start,
                    token.span.end
                );
            }
        }
    }
}

fn dump_ast(module: &ast::Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "ast {}", module.source_file);
    for class in &module.classes {
        let superclass = class.superclass.as_deref().unwrap_or("<object>");
        let _ = writeln!(
            out,
            "class {} {} {} superclass={superclass}",
            class.id, class.definition, class.name
        );
        for slot in &class.slots {
            let type_name = slot.type_name.as_deref().unwrap_or("<object>");
            let init_keyword = slot.init_keyword.as_deref().unwrap_or(&slot.name);
            let required = if slot.required_init_keyword {
                " required"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  {} {} {} {} :: {} init-keyword: {}:{} allocation={} mutability={}",
                ast_slot_mutability_prefix(slot.mutability),
                slot.id,
                slot.definition,
                slot.name,
                type_name,
                init_keyword,
                required,
                ast_slot_allocation_label(slot.allocation),
                ast_slot_mutability_label(slot.mutability)
            );
            if let Some(init_value) = &slot.init_value {
                indent_line(&mut out, 4);
                let _ = writeln!(out, "init-value");
                dump_ast_expr(&mut out, init_value, 6);
            }
        }
    }
    for generic in &module.generics {
        let _ = writeln!(
            out,
            "generic {} {} {}({})",
            generic.id,
            generic.definition,
            generic.name,
            generic
                .params
                .iter()
                .map(dump_generic_param)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(return_type) = &generic.return_type {
            indent_line(&mut out, 2);
            let _ = writeln!(out, "returns {}", dump_type_annotation(return_type));
        }
    }
    for method in &module.methods {
        let _ = writeln!(
            out,
            "method {} {} {} :: {}",
            method.id, method.definition, method.name, method.param.class_name
        );
        if let Some(return_type) = &method.return_type {
            indent_line(&mut out, 2);
            let _ = writeln!(out, "returns {}", dump_type_annotation(return_type));
        }
        dump_ast_block(&mut out, &method.body, 2);
    }
    for function in &module.functions {
        let _ = writeln!(
            out,
            "function {} {} {}({})",
            function.id,
            function.definition,
            function.name,
            function
                .params
                .iter()
                .map(dump_function_param)
                .collect::<Vec<_>>()
                .join(", ")
        );
        if let Some(return_type) = &function.return_type {
            indent_line(&mut out, 2);
            let _ = writeln!(out, "returns {}", dump_type_annotation(return_type));
        }
        dump_ast_block(&mut out, &function.body, 2);
    }
    out
}

fn dump_generic_param(param: &ast::GenericParam) -> String {
    param.type_annotation.as_ref().map_or_else(
        || param.name.clone(),
        |annotation| format!("{} :: {}", param.name, dump_type_annotation(annotation)),
    )
}

fn dump_function_param(param: &ast::FunctionParam) -> String {
    param.type_annotation.as_ref().map_or_else(
        || param.name.clone(),
        |annotation| format!("{} :: {}", param.name, dump_type_annotation(annotation)),
    )
}

fn dump_type_annotation(annotation: &ast::TypeAnnotation) -> String {
    if annotation.class_object {
        format!("class {}", annotation.name)
    } else {
        annotation.name.clone()
    }
}

fn ast_slot_allocation_label(allocation: ast::SlotAllocation) -> &'static str {
    match allocation {
        ast::SlotAllocation::Instance => "instance",
    }
}

fn ast_slot_mutability_prefix(mutability: ast::SlotMutability) -> &'static str {
    match mutability {
        ast::SlotMutability::Mutable => "slot",
        ast::SlotMutability::Constant => "constant slot",
    }
}

fn ast_slot_mutability_label(mutability: ast::SlotMutability) -> &'static str {
    match mutability {
        ast::SlotMutability::Mutable => "mutable",
        ast::SlotMutability::Constant => "constant",
    }
}

fn dump_ast_block(out: &mut String, block: &ast::Block, indent: usize) {
    indent_line(out, indent);
    let _ = writeln!(out, "block {}", block.id);
    for binding in &block.lets {
        indent_line(out, indent + 2);
        let _ = writeln!(
            out,
            "let {} {} {}",
            binding.id, binding.definition, binding.name
        );
        dump_ast_expr(out, &binding.value, indent + 4);
    }
    indent_line(out, indent + 2);
    let _ = writeln!(out, "result");
    dump_ast_expr(out, &block.result, indent + 4);
}

fn dump_ast_expr(out: &mut String, expr: &ast::Expr, indent: usize) {
    indent_line(out, indent);
    match expr {
        ast::Expr::Number { id, value, .. } => {
            let _ = writeln!(out, "{id} number {value}");
        }
        ast::Expr::Bool { id, value, .. } => {
            let _ = writeln!(out, "{id} bool {value}");
        }
        ast::Expr::Name { id, name, .. } => {
            let _ = writeln!(out, "{id} name {name}");
        }
        ast::Expr::ClassName { id, class_name, .. } => {
            let _ = writeln!(out, "{id} class-name {class_name}");
        }
        ast::Expr::InstanceOf {
            id,
            value,
            class_name,
            ..
        } => {
            let _ = writeln!(out, "{id} instance? {class_name}");
            dump_ast_expr(out, value, indent + 2);
        }
        ast::Expr::Cast {
            id,
            class_name,
            value,
            ..
        } => {
            let _ = writeln!(out, "{id} as {class_name}");
            dump_ast_expr(out, value, indent + 2);
        }
        ast::Expr::Call {
            id, callee, args, ..
        } => {
            let _ = writeln!(out, "{id} call {callee}");
            for arg in args {
                dump_ast_expr(out, arg, indent + 2);
            }
        }
        ast::Expr::Signal { id, condition, .. } => {
            let _ = writeln!(out, "{id} signal");
            dump_ast_expr(out, condition, indent + 2);
        }
        ast::Expr::MakeObject {
            id, class, fields, ..
        } => {
            let _ = writeln!(out, "{id} make");
            dump_ast_expr(out, class, indent + 2);
            for field in fields {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "field {} {}", field.id, field.name);
                dump_ast_expr(out, &field.value, indent + 4);
            }
        }
        ast::Expr::SlotRead {
            id, receiver, slot, ..
        } => {
            let _ = writeln!(out, "{id} slot {slot}");
            dump_ast_expr(out, receiver, indent + 2);
        }
        ast::Expr::Binary {
            id, op, lhs, rhs, ..
        } => {
            let _ = writeln!(out, "{id} binary {op:?}");
            dump_ast_expr(out, lhs, indent + 2);
            dump_ast_expr(out, rhs, indent + 2);
        }
        ast::Expr::Compare {
            id, op, lhs, rhs, ..
        } => {
            let _ = writeln!(out, "{id} compare {op:?}");
            dump_ast_expr(out, lhs, indent + 2);
            dump_ast_expr(out, rhs, indent + 2);
        }
        ast::Expr::If {
            id,
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            let _ = writeln!(out, "{id} if");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "condition");
            dump_ast_expr(out, condition, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "then");
            dump_ast_expr(out, then_branch, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "else");
            dump_ast_expr(out, else_branch, indent + 4);
        }
        ast::Expr::Handle {
            id, body, restart, ..
        } => {
            let _ = writeln!(out, "{id} handle");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "body");
            dump_ast_expr(out, body, indent + 4);
            indent_line(out, indent + 2);
            if let Some(condition_name) = &restart.condition_name {
                let _ = writeln!(
                    out,
                    "restart {} {condition_name} :: {}",
                    restart.id, restart.class_name
                );
            } else {
                let _ = writeln!(out, "restart {} {}", restart.id, restart.class_name);
            }
            dump_ast_expr(out, &restart.body, indent + 4);
        }
        ast::Expr::Error { id, .. } => {
            let _ = writeln!(out, "{id} error");
        }
    }
}

fn dump_sem(module: &sem::Module) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "sem {}", module.source_file);
    for class in &module.classes {
        let superclass = sem_superclass_label(module, class.superclass);
        let _ = writeln!(
            out,
            "class c{} {} {} origin={} superclass={superclass} finalized={} direct-slots={} effective-slots={}",
            class.id.0,
            class.definition,
            class.name,
            sem_class_origin_label(class.origin),
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len()
        );
        for slot in &class.direct_slots {
            let _ = writeln!(
                out,
                "  direct slot s{} {} {} :: {} origin={} accessor {} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                sem_slot_origin_label(slot.origin),
                slot.accessor.definition,
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
        }
        for slot in &class.effective_slots {
            let source = if slot.source_class == class.id {
                "direct"
            } else {
                "inherited"
            };
            let _ = writeln!(
                out,
                "  effective slot s{} {} {} :: {} source={} c{}.s{} origin={} accessor {} reads c{}.s{} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                source,
                slot.source_class.0,
                slot.source_slot.0,
                sem_slot_origin_label(slot.origin),
                slot.accessor.definition,
                slot.accessor.class.0,
                slot.accessor.slot.0,
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_effective_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
        }
    }
    for generic in &module.generics {
        let params = generic
            .params
            .iter()
            .zip(&generic.param_kinds)
            .map(|(name, kind)| match kind {
                Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
                None => format!("{name}:untyped"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let return_annotation = generic.return_annotation.map_or_else(
            || "inferred".to_string(),
            |kind| sem_kind_label(module, kind),
        );
        let _ = writeln!(
            out,
            "generic g{} {} {}({params}) result={:?} return={return_annotation}",
            generic.id.0, generic.definition, generic.name, generic.result_kind
        );
    }
    for method in &module.methods {
        let return_annotation = method.return_annotation.map_or_else(
            || "inferred".to_string(),
            |kind| sem_kind_label(module, kind),
        );
        let _ = writeln!(
            out,
            "method m{} {} generic=g{} class=c{} result={:?} return={return_annotation}",
            method.id.0,
            method.definition,
            method.generic.0,
            method.specializer.0,
            method.result_kind
        );
        dump_sem_locals(module, &mut out, &method.locals, 2);
        dump_sem_block(&mut out, &method.body, 2);
    }
    for function in &module.functions {
        let params = function
            .params
            .iter()
            .zip(&function.param_kinds)
            .map(|(local, kind)| {
                let name = function
                    .locals
                    .get(local.0 as usize)
                    .map_or("?", |local| local.name.as_str());
                format!("{name}:{}", sem_kind_label(module, *kind))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let return_annotation = function.return_annotation.map_or_else(
            || "inferred".to_string(),
            |kind| sem_kind_label(module, kind),
        );
        let _ = writeln!(
            out,
            "function f{} {} {}({params}) result={:?} return={return_annotation}",
            function.id.0, function.definition, function.name, function.result_kind
        );
        dump_sem_locals(module, &mut out, &function.locals, 2);
        dump_sem_block(&mut out, &function.body, 2);
    }
    out
}

fn dump_mop(module: &sem::Module) -> String {
    let runtime = RuntimeLayout::from_sem(module);
    let mut out = String::new();
    let _ = writeln!(out, "mop {}", module.source_file);

    let _ = writeln!(out, "classes:");
    for class in &module.classes {
        let runtime_class = runtime.classes.get(class.id.0 as usize);
        let runtime_kind =
            runtime_class.map_or("missing", |class| runtime_object_kind_label(class.kind));
        let superclass = sem_superclass_label(module, class.superclass);
        let _ = writeln!(
            out,
            "  c{} {} {} origin={} superclass={superclass} finalized={} direct-slots={} effective-slots={} runtime={runtime_kind}",
            class.id.0,
            class.definition,
            class.name,
            sem_class_origin_label(class.origin),
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len()
        );
        let _ = writeln!(out, "    direct-slots:");
        for slot in &class.direct_slots {
            let _ = writeln!(
                out,
                "      slot s{} {} {} :: {} kind={} origin={} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                sem_kind_label(module, slot.kind),
                sem_slot_origin_label(slot.origin),
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
            let _ = writeln!(
                out,
                "      accessor {} {}(c{}) -> {} reads c{}.s{}",
                slot.accessor.definition,
                slot.accessor.name,
                slot.accessor.class.0,
                sem_kind_label(module, slot.accessor.result_kind),
                slot.accessor.class.0,
                slot.accessor.slot.0
            );
        }
        let _ = writeln!(out, "    effective-slots:");
        for slot in &class.effective_slots {
            let runtime_slot = runtime_class.and_then(|class| class.slots.get(slot.id.0 as usize));
            let storage = runtime_slot.map_or("missing", |slot| {
                if slot.host_backed {
                    "host-backed"
                } else {
                    "native"
                }
            });
            let source = if slot.source_class == class.id {
                "direct"
            } else {
                "inherited"
            };
            let _ = writeln!(
                out,
                "      slot s{} {} {} :: {} kind={} source={} c{}.s{} origin={} storage={} init-keyword: {}:{} init-value={} allocation={} mutability={} runtime={}",
                slot.id.0,
                slot.definition,
                slot.name,
                slot.type_name,
                sem_kind_label(module, slot.kind),
                source,
                slot.source_class.0,
                slot.source_slot.0,
                sem_slot_origin_label(slot.origin),
                storage,
                slot.init_keyword,
                if slot.required_init_keyword {
                    " required"
                } else {
                    ""
                },
                sem_effective_slot_init_value_label(slot),
                sem_slot_allocation_label(slot.allocation),
                sem_slot_mutability_label(slot.mutability),
                sem_runtime_repr(slot.kind)
            );
            let _ = writeln!(
                out,
                "      accessor {} {}(c{}) -> {} reads c{}.s{}",
                slot.accessor.definition,
                slot.accessor.name,
                slot.accessor.class.0,
                sem_kind_label(module, slot.accessor.result_kind),
                slot.accessor.class.0,
                slot.accessor.slot.0
            );
        }
    }

    let _ = writeln!(out, "generics:");
    for generic in &module.generics {
        let params = generic
            .params
            .iter()
            .zip(&generic.param_kinds)
            .map(|(name, kind)| match kind {
                Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
                None => format!("{name}:untyped"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "  g{} {} {}({params}) -> {}",
            generic.id.0,
            generic.definition,
            generic.name,
            sem_kind_label(module, generic.result_kind)
        );
        for method in module
            .methods
            .iter()
            .filter(|method| method.generic == generic.id)
        {
            let class = module.classes.get(method.specializer.0 as usize);
            let class_name = class.map_or("<?>", |class| class.name.as_str());
            let class_origin = class.map_or_else(
                || "missing".into(),
                |class| sem_class_origin_label(class.origin),
            );
            let _ = writeln!(
                out,
                "    method m{} {} specializer=c{} {} origin={} -> {}",
                method.id.0,
                method.definition,
                method.specializer.0,
                class_name,
                class_origin,
                sem_kind_label(module, method.result_kind)
            );
        }
    }

    let _ = writeln!(out, "dispatch table:");
    for generic in &module.generics {
        let _ = writeln!(out, "  g{} {}", generic.id.0, generic.name);
        for method in module
            .methods
            .iter()
            .filter(|method| method.generic == generic.id)
        {
            let class = module.classes.get(method.specializer.0 as usize);
            let class_name = class.map_or("<?>", |class| class.name.as_str());
            let class_origin = class.map_or_else(
                || "missing".into(),
                |class| sem_class_origin_label(class.origin),
            );
            let _ = writeln!(
                out,
                "    c{} {} origin={} -> m{}",
                method.specializer.0, class_name, class_origin, method.id.0
            );
        }
    }

    let _ = writeln!(out, "runtime layout:");
    for (index, class) in runtime.classes.iter().enumerate() {
        let _ = writeln!(
            out,
            "  c{index} {} kind={}",
            class.name,
            runtime_object_kind_label(class.kind)
        );
        for (slot_index, slot) in class.slots.iter().enumerate() {
            let storage = if slot.host_backed {
                "host-backed"
            } else {
                "native"
            };
            let _ = writeln!(
                out,
                "    s{slot_index} {} kind={} storage={} runtime={}",
                slot.name,
                sem_kind_label(module, slot.kind),
                storage,
                sem_runtime_repr(slot.kind)
            );
        }
    }

    out
}

fn runtime_object_kind_label(kind: HostObjectKind) -> &'static str {
    match kind {
        HostObjectKind::Native => "native",
        HostObjectKind::HostBacked => "host-backed",
    }
}

fn dump_eop(artifacts: &SessionArtifacts) -> String {
    render_environment_objects(&artifacts.source, &artifacts.environment_objects())
}

fn render_environment_objects(source: &SourceFile, objects: &[EnvironmentObject]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "eop {} {}", source.id, source.name);
    let _ = writeln!(out, "objects:");
    for object in objects {
        let _ = writeln!(out, "  object {}", object.id);
        let _ = writeln!(
            out,
            "    kind={} class={} origin={} storage={}",
            object.kind.as_str(),
            object.meta_class,
            object.origin.as_str(),
            object.storage.as_str()
        );
        let _ = writeln!(
            out,
            "    presentation title=\"{}\"",
            object.presentation.title
        );
        let _ = writeln!(
            out,
            "    affordances {}",
            render_affordances(&object.affordances)
        );
        if let Some(execution) = &object.execution {
            let _ = writeln!(out, "    execution {}", execution.detail);
        }
        let _ = writeln!(out, "    inspection {}", object.inspection.detail);
    }
    out
}

fn render_affordances(affordances: &AffordanceFacet) -> String {
    let mut out = String::new();
    for (index, affordance) in affordances.names.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(affordance.as_str());
    }
    out
}

struct EnvironmentObjectParts {
    id: String,
    kind: EnvironmentKind,
    meta_class: &'static str,
    origin: EnvironmentOrigin,
    storage: EnvironmentStorage,
    title: String,
    affordances: Vec<EnvironmentAffordance>,
    execution: Option<String>,
    inspection: String,
}

fn environment_object(parts: EnvironmentObjectParts) -> EnvironmentObject {
    EnvironmentObject {
        id: EnvironmentObjectId::new(parts.id),
        kind: parts.kind,
        meta_class: parts.meta_class.to_string(),
        origin: parts.origin,
        storage: parts.storage,
        presentation: PresentationFacet::new(parts.title),
        affordances: AffordanceFacet::new(parts.affordances),
        execution: parts.execution.map(ExecutionFacet::new),
        inspection: InspectionFacet::new(parts.inspection),
    }
}

fn project_session_environment(artifacts: &SessionArtifacts, sink: &mut dyn EnvironmentObjectSink) {
    use EnvironmentAffordance as Aff;

    let module = &artifacts.sem;
    let runtime = RuntimeLayout::from_sem(module);
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("source:{}", artifacts.source.id),
        kind: EnvironmentKind::SourceFile,
        meta_class: "<source-file>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: artifacts.source.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSyntax,
            Aff::DumpAst,
            Aff::DumpSem,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::Artifacts,
            Aff::Diff,
        ]),
        execution: None,
        inspection: format!(
            "source-file={} bytes={}",
            artifacts.source.id,
            artifacts.source.source.len()
        ),
    }));

    for class in &module.classes {
        project_environment_class(module, &runtime, sink, class);
    }
    for generic in &module.generics {
        project_environment_generic(module, sink, generic);
    }
    for method in &module.methods {
        project_environment_method(module, sink, method);
    }
    for function in &module.functions {
        project_environment_function(module, sink, function);
    }
    for (index, diagnostic) in artifacts.diagnostics.iter().enumerate() {
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("diagnostic:d{index}"),
            kind: EnvironmentKind::Diagnostic,
            meta_class: "<diagnostic>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: diagnostic.message.clone(),
            affordances: Vec::from([Aff::Inspect, Aff::OpenSource, Aff::DumpSem, Aff::DumpEop]),
            execution: None,
            inspection: format!(
                "severity={:?} span={}..{}",
                diagnostic.severity, diagnostic.span.start, diagnostic.span.end
            ),
        }));
    }
    for (index, class) in runtime.classes.iter().enumerate() {
        project_environment_runtime_class(module, sink, index, class);
    }
    for report in artifacts.artifact_reports() {
        let artifact = artifact_label(report.artifact);
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("artifact:{}", report.key),
            kind: EnvironmentKind::Artifact,
            meta_class: "<query-artifact>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: artifact.clone(),
            affordances: Vec::from([Aff::Inspect, Aff::ShowDependencies, Aff::DumpArtifacts]),
            execution: None,
            inspection: format!("key={} artifact={artifact}", report.key),
        }));
    }
}

fn project_environment_class(
    module: &sem::Module,
    runtime: &RuntimeLayout,
    sink: &mut dyn EnvironmentObjectSink,
    class: &sem::Class,
) {
    use EnvironmentAffordance as Aff;

    let runtime_class = runtime.classes.get(class.id.0 as usize);
    let runtime_kind =
        runtime_class.map_or("missing", |class| runtime_object_kind_label(class.kind));
    let superclass = sem_superclass_label(module, class.superclass);
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("class:c{}", class.id.0),
        kind: EnvironmentKind::Class,
        meta_class: "<class>",
        origin: eop_class_origin(class.origin),
        storage: eop_class_storage(class.origin),
        title: class.name.clone(),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowRuntimeLayout,
            Aff::ShowSlots,
        ]),
        execution: None,
        inspection: format!(
            "definition={} class-id=c{} superclass={superclass} finalized={} direct-slots={} effective-slots={} runtime={runtime_kind}",
            class.definition,
            class.id.0,
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len()
        ),
    }));

    for slot in &class.effective_slots {
        project_environment_slot(module, sink, class, slot);
        project_environment_accessor(module, sink, class, slot);
    }
}

fn project_environment_slot(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    class: &sem::Class,
    slot: &sem::EffectiveSlot,
) {
    use EnvironmentAffordance as Aff;

    let source = if slot.source_class == class.id {
        "direct"
    } else {
        "inherited"
    };
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("slot:c{}.s{}", class.id.0, slot.id.0),
        kind: EnvironmentKind::Slot,
        meta_class: "<slot>",
        origin: eop_slot_origin(slot.origin),
        storage: eop_slot_storage(slot.origin),
        title: format!("{}.{}", class.name, slot.name),
        affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::ShowAccessor]),
        execution: None,
        inspection: format!(
            "definition={} owner=class:c{} slot-id=s{} source={} source-slot=class:c{}.s{} kind={} type={} init-keyword={}:{} init-value={} allocation={} mutability={} runtime={}",
            slot.definition,
            class.id.0,
            slot.id.0,
            source,
            slot.source_class.0,
            slot.source_slot.0,
            sem_kind_label(module, slot.kind),
            slot.type_name,
            slot.init_keyword,
            if slot.required_init_keyword {
                " required"
            } else {
                ""
            },
            sem_effective_slot_init_value_label(slot),
            sem_slot_allocation_label(slot.allocation),
            sem_slot_mutability_label(slot.mutability),
            sem_runtime_repr(slot.kind)
        ),
    }));
}

fn project_environment_accessor(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    class: &sem::Class,
    slot: &sem::EffectiveSlot,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("accessor:c{}.s{}", class.id.0, slot.id.0),
        kind: EnvironmentKind::Accessor,
        meta_class: "<accessor>",
        origin: eop_slot_origin(slot.origin),
        storage: eop_slot_storage(slot.origin),
        title: slot.accessor.name.clone(),
        affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::ShowSlot]),
        execution: Some(format!(
            "callable=known result={} reads=slot:c{}.s{}",
            sem_kind_label(module, slot.accessor.result_kind),
            class.id.0,
            slot.id.0
        )),
        inspection: format!(
            "definition={} owner=class:c{} slot=s{} source-slot=class:c{}.s{}",
            slot.accessor.definition,
            class.id.0,
            slot.id.0,
            slot.source_class.0,
            slot.source_slot.0
        ),
    }));
}

fn project_environment_generic(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    generic: &sem::GenericFunction,
) {
    use EnvironmentAffordance as Aff;

    let params = generic
        .params
        .iter()
        .zip(&generic.param_kinds)
        .map(|(name, kind)| match kind {
            Some(kind) => format!("{name}:{}", sem_kind_label(module, *kind)),
            None => format!("{name}:untyped"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let method_count = module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
        .count();
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("generic:g{}", generic.id.0),
        kind: EnvironmentKind::Generic,
        meta_class: "<generic-function>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: format!(
            "{}({params}) -> {}",
            generic.name,
            sem_kind_label(module, generic.result_kind)
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpMop,
            Aff::DumpEop,
            Aff::ShowDispatch,
            Aff::AddMethod,
        ]),
        execution: Some(format!(
            "callable=generic arity={} methods={method_count}",
            generic.arity
        )),
        inspection: format!(
            "definition={} generic-id=g{}",
            generic.definition, generic.id.0
        ),
    }));
}

fn project_environment_method(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    method: &sem::Method,
) {
    use EnvironmentAffordance as Aff;

    let generic = module.generics.get(method.generic.0 as usize);
    let generic_name = generic.map_or("<?>", |generic| generic.name.as_str());
    let class = module.classes.get(method.specializer.0 as usize);
    let class_name = class.map_or("<?>", |class| class.name.as_str());
    let class_origin = class.map_or_else(
        || "missing".into(),
        |class| sem_class_origin_label(class.origin),
    );
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("method:m{}", method.id.0),
        kind: EnvironmentKind::Method,
        meta_class: "<method>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: format!(
            "{generic_name}({class_name}) -> {}",
            sem_kind_label(module, method.result_kind)
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSem,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
            Aff::ShowDependencies,
            Aff::EditBody,
        ]),
        execution: Some(format!(
            "callable=method participates-in=generic:g{} specializer=class:c{}",
            method.generic.0, method.specializer.0
        )),
        inspection: format!(
            "definition={} method-id=m{} specializer-origin={class_origin}",
            method.definition, method.id.0
        ),
    }));
}

fn project_environment_function(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    function: &sem::Function,
) {
    use EnvironmentAffordance as Aff;

    let params = function
        .params
        .iter()
        .zip(&function.param_kinds)
        .map(|(local, kind)| {
            let name = function
                .locals
                .get(local.0 as usize)
                .map_or("?", |local| local.name.as_str());
            format!("{name}:{}", sem_kind_label(module, *kind))
        })
        .collect::<Vec<_>>()
        .join(", ");
    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("function:f{}", function.id.0),
        kind: EnvironmentKind::Function,
        meta_class: "<function>",
        origin: EnvironmentOrigin::SourceAuthored,
        storage: EnvironmentStorage::SourceDefined,
        title: format!(
            "{}({params}) -> {}",
            function.name,
            sem_kind_label(module, function.result_kind)
        ),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::DumpSem,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
            Aff::Run,
            Aff::ShowDependencies,
            Aff::EditBody,
        ]),
        execution: Some(format!("callable=direct arity={}", function.params.len())),
        inspection: format!(
            "definition={} function-id=f{}",
            function.definition, function.id.0
        ),
    }));
}

fn project_environment_runtime_class(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    index: usize,
    class: &crate::runtime::RuntimeClass,
) {
    use EnvironmentAffordance as Aff;

    sink.emit(environment_object(EnvironmentObjectParts {
        id: format!("runtime-class:c{index}"),
        kind: EnvironmentKind::RuntimeLayoutRow,
        meta_class: "<runtime-class>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("runtime {}", class.name),
        affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::DumpTape]),
        execution: None,
        inspection: format!(
            "class-id=c{index} storage-kind={} slots={}",
            runtime_object_kind_label(class.kind),
            class.slots.len()
        ),
    }));
    for (slot_index, slot) in class.slots.iter().enumerate() {
        let storage = if slot.host_backed {
            "host-backed"
        } else {
            "native"
        };
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("runtime-slot:c{index}.s{slot_index}"),
            kind: EnvironmentKind::RuntimeLayoutRow,
            meta_class: "<runtime-slot>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: format!("runtime {}.{}", class.name, slot.name),
            affordances: Vec::from([Aff::Inspect, Aff::DumpMop, Aff::DumpEop, Aff::DumpTape]),
            execution: None,
            inspection: format!(
                "class-id=c{index} slot-id=s{slot_index} storage={} kind={} runtime={}",
                storage,
                sem_kind_label(module, slot.kind),
                sem_runtime_repr(slot.kind)
            ),
        }));
    }
}

fn eop_class_origin(origin: sem::ClassOrigin) -> EnvironmentOrigin {
    match origin {
        sem::ClassOrigin::Native => EnvironmentOrigin::SourceAuthored,
        sem::ClassOrigin::HostBacked { .. } => EnvironmentOrigin::HostBacked,
    }
}

fn eop_class_storage(origin: sem::ClassOrigin) -> EnvironmentStorage {
    match origin {
        sem::ClassOrigin::Native => EnvironmentStorage::SourceDefined,
        sem::ClassOrigin::HostBacked { .. } => EnvironmentStorage::HostRegistry,
    }
}

fn eop_slot_origin(origin: sem::SlotOrigin) -> EnvironmentOrigin {
    match origin {
        sem::SlotOrigin::Native => EnvironmentOrigin::SourceAuthored,
        sem::SlotOrigin::HostBacked { .. } => EnvironmentOrigin::HostBacked,
    }
}

fn eop_slot_storage(origin: sem::SlotOrigin) -> EnvironmentStorage {
    match origin {
        sem::SlotOrigin::Native => EnvironmentStorage::SourceDefined,
        sem::SlotOrigin::HostBacked { .. } => EnvironmentStorage::HostAdapter,
    }
}

fn sem_kind_label(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Object(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class#{}", class.0),
            |class| format!("object({})", class.name),
        ),
        sem::ValueKind::ClassObject(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class-object#{}", class.0),
            |class| format!("class-object({})", class.name),
        ),
        sem::ValueKind::KnownCallable(callable) => {
            format!("known-callable({})", sem_callable_label(module, callable))
        }
        sem::ValueKind::Error => "error".into(),
    }
}

fn sem_kind_label_for_expr(kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Object(class) => format!("object(c{})", class.0),
        sem::ValueKind::ClassObject(class) => format!("class-object(c{})", class.0),
        sem::ValueKind::KnownCallable(callable) => {
            format!("known-callable({})", sem_callable_label_for_expr(callable))
        }
        sem::ValueKind::Error => "error".into(),
    }
}

fn sem_kind_fingerprint(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Object(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class#{}", class.0),
            |class| format!("object#{}", class.definition.raw()),
        ),
        sem::ValueKind::ClassObject(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class-object#{}", class.0),
            |class| format!("class-object#{}", class.definition.raw()),
        ),
        sem::ValueKind::KnownCallable(callable) => sem_callable_fingerprint(module, callable),
        sem::ValueKind::Error => "error".into(),
    }
}

fn sem_runtime_repr(kind: sem::ValueKind) -> &'static str {
    match kind {
        sem::ValueKind::Any => "boxed-any",
        sem::ValueKind::I64 => "i64",
        sem::ValueKind::Bool => "bool",
        sem::ValueKind::Object(_) => "object-handle",
        sem::ValueKind::ClassObject(_) => "class-id",
        sem::ValueKind::KnownCallable(_) => "compile-time-callable",
        sem::ValueKind::Error => "error",
    }
}

fn sem_slot_init_value_label(slot: &sem::Slot) -> &'static str {
    if slot.init_value.is_some() {
        "yes"
    } else {
        "none"
    }
}

fn sem_effective_slot_init_value_label(slot: &sem::EffectiveSlot) -> &'static str {
    if slot.init_value.is_some() {
        "yes"
    } else {
        "none"
    }
}

fn sem_slot_allocation_label(allocation: sem::SlotAllocation) -> &'static str {
    match allocation {
        sem::SlotAllocation::Instance => "instance",
    }
}

fn sem_slot_mutability_label(mutability: sem::SlotMutability) -> &'static str {
    match mutability {
        sem::SlotMutability::Mutable => "mutable",
        sem::SlotMutability::Constant => "constant",
    }
}

fn sem_callable_label(module: &sem::Module, callable: sem::KnownCallable) -> String {
    match callable {
        sem::KnownCallable::DirectFunction(function) => {
            module.functions.get(function.0 as usize).map_or_else(
                || format!("function#{}", function.0),
                |function| format!("function {}", function.name),
            )
        }
        sem::KnownCallable::GenericFunction(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic#{}", generic.0),
                |generic| format!("generic {}", generic.name),
            )
        }
        sem::KnownCallable::SlotAccessor { class, slot, .. } => module
            .classes
            .get(class.0 as usize)
            .and_then(|class| {
                class
                    .effective_slots
                    .get(slot.0 as usize)
                    .map(|slot| format!("accessor {}.{}", class.name, slot.name))
            })
            .unwrap_or_else(|| format!("accessor c{}.s{}", class.0, slot.0)),
    }
}

fn sem_callable_label_for_expr(callable: sem::KnownCallable) -> String {
    match callable {
        sem::KnownCallable::DirectFunction(function) => format!("function f{}", function.0),
        sem::KnownCallable::GenericFunction(generic) => format!("generic g{}", generic.0),
        sem::KnownCallable::SlotAccessor { class, slot, .. } => {
            format!("accessor c{}.s{}", class.0, slot.0)
        }
    }
}

fn sem_callable_fingerprint(module: &sem::Module, callable: sem::KnownCallable) -> String {
    match callable {
        sem::KnownCallable::DirectFunction(function) => {
            module.functions.get(function.0 as usize).map_or_else(
                || format!("function#{}", function.0),
                |function| format!("function#{}", function.definition.raw()),
            )
        }
        sem::KnownCallable::GenericFunction(generic) => {
            module.generics.get(generic.0 as usize).map_or_else(
                || format!("generic#{}", generic.0),
                |generic| format!("generic#{}", generic.definition.raw()),
            )
        }
        sem::KnownCallable::SlotAccessor {
            accessor,
            class,
            slot,
        } => {
            let class_definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let slot_definition = module
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or(0, |slot| slot.definition.raw());
            format!(
                "accessor#{}:{class_definition}.{slot_definition}",
                accessor.raw()
            )
        }
    }
}

fn dump_sem_locals(module: &sem::Module, out: &mut String, locals: &[sem::Local], indent: usize) {
    for local in locals {
        indent_line(out, indent);
        let _ = writeln!(
            out,
            "local l{} {} {} {}",
            local.id.0,
            local.definition,
            local.name,
            sem_kind_label(module, local.kind)
        );
    }
}

fn dump_sem_block(out: &mut String, block: &sem::Block, indent: usize) {
    for binding in &block.lets {
        indent_line(out, indent);
        let _ = writeln!(out, "let l{}", binding.local.0);
        dump_sem_expr(out, &binding.value, indent + 2);
    }
    indent_line(out, indent);
    let _ = writeln!(out, "result");
    dump_sem_expr(out, &block.result, indent + 2);
}

fn dump_sem_expr(out: &mut String, expr: &sem::Expr, indent: usize) {
    indent_line(out, indent);
    match expr {
        sem::Expr::ConstI64 { value, .. } => {
            let _ = writeln!(out, "const.i64 {value}");
        }
        sem::Expr::ConstBool { value, .. } => {
            let _ = writeln!(out, "const.bool {value}");
        }
        sem::Expr::Local {
            local, result_kind, ..
        } => {
            let _ = writeln!(
                out,
                "local l{} as {}",
                local.0,
                sem_kind_label_for_expr(*result_kind)
            );
        }
        sem::Expr::ClassObject { class, .. } => {
            let _ = writeln!(out, "class-object c{}", class.0);
        }
        sem::Expr::KnownCallable { callable, .. } => {
            let _ = writeln!(
                out,
                "known-callable {}",
                sem_callable_label_for_expr(*callable)
            );
        }
        sem::Expr::DirectCall { function, args, .. } => {
            let _ = writeln!(out, "direct-call f{}", function.0);
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::GenericCall { generic, args, .. } => {
            let _ = writeln!(out, "generic-call g{}", generic.0);
            for arg in args {
                dump_sem_expr(out, arg, indent + 2);
            }
        }
        sem::Expr::Signal {
            condition,
            condition_class,
            result_kind,
            ..
        } => {
            let _ = writeln!(out, "signal c{} result={result_kind:?}", condition_class.0);
            dump_sem_expr(out, condition, indent + 2);
        }
        sem::Expr::ClassOf { object, class, .. } => {
            let _ = writeln!(out, "class-of c{}", class.0);
            dump_sem_expr(out, object, indent + 2);
        }
        sem::Expr::InstanceOf { value, class, .. } => {
            let _ = writeln!(out, "instance? c{}", class.0);
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::Cast { value, class, .. } => {
            let _ = writeln!(out, "as c{}", class.0);
            dump_sem_expr(out, value, indent + 2);
        }
        sem::Expr::MakeObject {
            class,
            class_id,
            fields,
            ..
        } => {
            let _ = writeln!(out, "make c{}", class_id.0);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "class");
            dump_sem_expr(out, class, indent + 4);
            for field in fields {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "field s{}", field.slot.0);
                dump_sem_expr(out, &field.value, indent + 4);
            }
        }
        sem::Expr::AccessorCall {
            accessor,
            receiver,
            class,
            slot,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "accessor-call {accessor} c{}.s{} result={result_kind:?}",
                class.0, slot.0
            );
            dump_sem_expr(out, receiver, indent + 2);
        }
        sem::Expr::Binary { op, lhs, rhs, .. } => {
            let _ = writeln!(out, "binary {op:?}");
            dump_sem_expr(out, lhs, indent + 2);
            dump_sem_expr(out, rhs, indent + 2);
        }
        sem::Expr::Compare { op, lhs, rhs, .. } => {
            let _ = writeln!(out, "compare {op:?}");
            dump_sem_expr(out, lhs, indent + 2);
            dump_sem_expr(out, rhs, indent + 2);
        }
        sem::Expr::If {
            condition,
            refinement,
            then_branch,
            else_branch,
            ..
        } => {
            let _ = writeln!(out, "if");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "condition");
            dump_sem_expr(out, condition, indent + 4);
            if let Some(refinement) = refinement {
                indent_line(out, indent + 2);
                let _ = writeln!(
                    out,
                    "refinement l{} => {}",
                    refinement.local.0,
                    sem_kind_label_for_expr(refinement.kind)
                );
            }
            indent_line(out, indent + 2);
            let _ = writeln!(out, "then");
            dump_sem_expr(out, then_branch, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "else");
            dump_sem_expr(out, else_branch, indent + 4);
        }
        sem::Expr::Handle { body, restart, .. } => {
            let condition = restart
                .condition_local
                .map_or(String::from("none"), |local| format!("l{}", local.0));
            let _ = writeln!(
                out,
                "handle restart c{} condition={condition} result={:?}",
                restart.specializer.0, restart.result_kind
            );
            indent_line(out, indent + 2);
            let _ = writeln!(out, "body");
            dump_sem_expr(out, body, indent + 4);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "restart");
            dump_sem_expr(out, &restart.body, indent + 4);
        }
        sem::Expr::Error { .. } => {
            let _ = writeln!(out, "error");
        }
    }
}

fn sem_class_origin_label(origin: sem::ClassOrigin) -> String {
    match origin {
        sem::ClassOrigin::Native => "native".into(),
        sem::ClassOrigin::HostBacked { host_class } => {
            format!("host-backed(hc{})", host_class.0)
        }
    }
}

fn sem_superclass_label(module: &sem::Module, superclass: Option<sem::ClassId>) -> String {
    superclass.map_or_else(
        || "<object>".into(),
        |class| {
            module.classes.get(class.0 as usize).map_or_else(
                || format!("c{}:<missing>", class.0),
                |class| format!("c{}:{}", class.id.0, class.name),
            )
        },
    )
}

fn sem_slot_origin_label(origin: sem::SlotOrigin) -> String {
    match origin {
        sem::SlotOrigin::Native => "native".into(),
        sem::SlotOrigin::HostBacked {
            host_class,
            host_slot,
        } => format!("host-backed(hc{}.hs{})", host_class.0, host_slot.0),
    }
}

fn diff_artifacts(before: &SessionArtifacts, after: &SessionArtifacts) -> SessionDiff {
    let before_defs = definitions(&before.sem);
    let after_defs = definitions(&after.sem);
    let mut preserved = Vec::new();
    let mut changed = Vec::new();

    for before in &before_defs {
        if let Some(after) = after_defs
            .iter()
            .find(|candidate| candidate.report.id == before.report.id)
        {
            if before.fingerprint == after.fingerprint {
                preserved.push(after.report.clone());
            } else {
                changed.push(DefinitionChange {
                    definition: after.report.clone(),
                    change: DefinitionChangeKind::Modified,
                });
            }
        } else {
            changed.push(DefinitionChange {
                definition: before.report.clone(),
                change: DefinitionChangeKind::Removed,
            });
        }
    }
    for after in &after_defs {
        if !before_defs
            .iter()
            .any(|candidate| candidate.report.id == after.report.id)
        {
            changed.push(DefinitionChange {
                definition: after.report.clone(),
                change: DefinitionChangeKind::Added,
            });
        }
    }

    let affected = affected_artifacts(after, &changed);

    SessionDiff {
        preserved,
        changed,
        affected,
    }
}

fn affected_artifacts(
    after: &SessionArtifacts,
    changed: &[DefinitionChange],
) -> Vec<ArtifactReport> {
    if changed.is_empty() {
        return Vec::new();
    }
    let only_modified_methods = changed.iter().all(|change| {
        change.change == DefinitionChangeKind::Modified
            && change.definition.kind == DefinitionKind::Method
    });
    let mut graph = QueryInvalidation::for_module(&after.sem);
    let artifacts = if only_modified_methods {
        let mut artifacts = Vec::new();
        for change in changed {
            artifacts.extend(graph.method_definition_changed(change.definition.id));
        }
        artifacts
    } else {
        graph.source_changed()
    };
    let mut reports = Vec::new();
    for artifact in artifacts {
        let key = graph.key_for(artifact);
        if reports
            .iter()
            .all(|report: &ArtifactReport| report.key != key)
        {
            reports.push(ArtifactReport { artifact, key });
        }
    }
    reports
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DefinitionFingerprint {
    report: DefinitionReport,
    fingerprint: String,
}

fn definitions(module: &sem::Module) -> Vec<DefinitionFingerprint> {
    let mut definitions = Vec::new();
    for class in &module.classes {
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: class.definition,
                kind: DefinitionKind::Class,
                name: class.name.clone(),
            },
            fingerprint: format!(
                "class:{}:{}:super={}:direct={}:effective={}",
                class.name,
                sem_class_origin_label(class.origin),
                sem_superclass_label(module, class.superclass),
                class
                    .direct_slots
                    .iter()
                    .map(|slot| format!(
                        "{}={}:{}:accessor={}:{}:{}:{}:{}:{}",
                        slot.name,
                        sem_kind_fingerprint(module, slot.kind),
                        sem_slot_origin_label(slot.origin),
                        slot.accessor.definition.raw(),
                        slot.init_keyword,
                        slot.required_init_keyword,
                        slot_init_fingerprint(module, slot),
                        sem_slot_allocation_label(slot.allocation),
                        sem_slot_mutability_label(slot.mutability)
                    ))
                    .collect::<Vec<_>>()
                    .join(","),
                class
                    .effective_slots
                    .iter()
                    .map(|slot| format!(
                        "{}={}:{}:source=c{}.s{}:accessor={}:{}:{}:{}:{}:{}",
                        slot.name,
                        sem_kind_fingerprint(module, slot.kind),
                        sem_slot_origin_label(slot.origin),
                        slot.source_class.0,
                        slot.source_slot.0,
                        slot.accessor.definition.raw(),
                        slot.init_keyword,
                        slot.required_init_keyword,
                        effective_slot_init_fingerprint(module, slot),
                        sem_slot_allocation_label(slot.allocation),
                        sem_slot_mutability_label(slot.mutability)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        });
        for slot in &class.direct_slots {
            definitions.push(DefinitionFingerprint {
                report: DefinitionReport {
                    id: slot.definition,
                    kind: DefinitionKind::Slot,
                    name: format!("{}.{}", class.name, slot.name),
                },
                fingerprint: format!(
                    "slot:{}:{}:{}:{}:accessor={}:{}:{}:{}:{}:{}",
                    class.name,
                    slot.name,
                    sem_kind_fingerprint(module, slot.kind),
                    sem_slot_origin_label(slot.origin),
                    slot.accessor.definition.raw(),
                    slot.init_keyword,
                    slot.required_init_keyword,
                    slot_init_fingerprint(module, slot),
                    sem_slot_allocation_label(slot.allocation),
                    sem_slot_mutability_label(slot.mutability)
                ),
            });
        }
    }
    for generic in &module.generics {
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: generic.definition,
                kind: DefinitionKind::Generic,
                name: generic.name.clone(),
            },
            fingerprint: format!(
                "generic:{}:{}:{}:{}",
                generic.name,
                generic
                    .param_kinds
                    .iter()
                    .map(|kind| kind.map_or_else(
                        || "untyped".into(),
                        |kind| sem_kind_fingerprint(module, kind)
                    ))
                    .collect::<Vec<_>>()
                    .join(","),
                generic.return_annotation.map_or_else(
                    || "inferred".into(),
                    |kind| sem_kind_fingerprint(module, kind)
                ),
                generic.arity
            ),
        });
    }
    for method in &module.methods {
        let generic = module
            .generics
            .get(method.generic.0 as usize)
            .map_or("?", |generic| generic.name.as_str());
        let specializer = module
            .classes
            .get(method.specializer.0 as usize)
            .map_or("?", |class| class.name.as_str());
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: method.definition,
                kind: DefinitionKind::Method,
                name: format!("{generic}({specializer})"),
            },
            fingerprint: format!(
                "method:{generic}:{specializer}:{}:{}",
                method.return_annotation.map_or_else(
                    || "inferred".into(),
                    |kind| sem_kind_fingerprint(module, kind)
                ),
                fingerprint_sem_block(module, &method.body)
            ),
        });
    }
    for function in &module.functions {
        definitions.push(DefinitionFingerprint {
            report: DefinitionReport {
                id: function.definition,
                kind: DefinitionKind::Function,
                name: function.name.clone(),
            },
            fingerprint: format!(
                "function:{}:{}:{}:{}:{}",
                function.name,
                function.params.len(),
                function
                    .param_kinds
                    .iter()
                    .map(|kind| sem_kind_fingerprint(module, *kind))
                    .collect::<Vec<_>>()
                    .join(","),
                function.return_annotation.map_or_else(
                    || "inferred".into(),
                    |kind| sem_kind_fingerprint(module, kind)
                ),
                fingerprint_sem_block(module, &function.body)
            ),
        });
    }
    definitions
}

fn slot_init_fingerprint(module: &sem::Module, slot: &sem::Slot) -> String {
    let Some(init_value) = &slot.init_value else {
        return "none".into();
    };
    let mut out = String::new();
    fingerprint_sem_expr(module, init_value, &mut out);
    out
}

fn effective_slot_init_fingerprint(module: &sem::Module, slot: &sem::EffectiveSlot) -> String {
    let Some(init_value) = &slot.init_value else {
        return "none".into();
    };
    let mut out = String::new();
    fingerprint_sem_expr(module, init_value, &mut out);
    out
}

fn fingerprint_sem_block(module: &sem::Module, block: &sem::Block) -> String {
    let mut out = String::new();
    for binding in &block.lets {
        let _ = write!(out, "let:{}=", binding.local.0);
        fingerprint_sem_expr(module, &binding.value, &mut out);
        out.push(';');
    }
    out.push_str("result=");
    fingerprint_sem_expr(module, &block.result, &mut out);
    out
}

fn fingerprint_sem_expr(module: &sem::Module, expr: &sem::Expr, out: &mut String) {
    match expr {
        sem::Expr::ConstI64 { value, .. } => {
            let _ = write!(out, "i64({value})");
        }
        sem::Expr::ConstBool { value, .. } => {
            let _ = write!(out, "bool({value})");
        }
        sem::Expr::Local {
            local, result_kind, ..
        } => {
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(out, "local({}:{result})", local.0);
        }
        sem::Expr::ClassObject { class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "class-object({definition})");
        }
        sem::Expr::KnownCallable { callable, .. } => {
            let fingerprint = sem_callable_fingerprint(module, *callable);
            let _ = write!(out, "known-callable({fingerprint})");
        }
        sem::Expr::DirectCall { function, args, .. } => {
            let definition = module
                .functions
                .get(function.0 as usize)
                .map_or(0, |function| function.definition.raw());
            let _ = write!(out, "call({definition}:");
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::GenericCall { generic, args, .. } => {
            let definition = module
                .generics
                .get(generic.0 as usize)
                .map_or(0, |generic| generic.definition.raw());
            let _ = write!(out, "generic({definition}:");
            fingerprint_args(module, args, out);
            out.push(')');
        }
        sem::Expr::Signal {
            condition,
            condition_class,
            result_kind,
            ..
        } => {
            let definition = module
                .classes
                .get(condition_class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "signal({definition}:{result_kind:?}:");
            fingerprint_sem_expr(module, condition, out);
            out.push(')');
        }
        sem::Expr::ClassOf { object, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "class-of({definition}:");
            fingerprint_sem_expr(module, object, out);
            out.push(')');
        }
        sem::Expr::InstanceOf { value, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "instance?({definition}:");
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::Cast { value, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "as({definition}:");
            fingerprint_sem_expr(module, value, out);
            out.push(')');
        }
        sem::Expr::MakeObject {
            class,
            class_id,
            fields,
            ..
        } => {
            let definition = module
                .classes
                .get(class_id.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "make({definition}:");
            fingerprint_sem_expr(module, class, out);
            out.push(':');
            for field in fields {
                let kind = module
                    .classes
                    .get(class_id.0 as usize)
                    .and_then(|class| class.effective_slots.get(field.slot.0 as usize))
                    .map_or(String::from("error"), |slot| {
                        sem_kind_fingerprint(module, slot.kind)
                    });
                let _ = write!(out, "slot{}:{kind}=", field.slot.0);
                fingerprint_sem_expr(module, &field.value, out);
                out.push(',');
            }
            out.push(')');
        }
        sem::Expr::AccessorCall {
            accessor,
            receiver,
            class,
            slot,
            result_kind,
            ..
        } => {
            let class_definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let slot_definition = module
                .classes
                .get(class.0 as usize)
                .and_then(|class| class.effective_slots.get(slot.0 as usize))
                .map_or(0, |slot| slot.definition.raw());
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(
                out,
                "accessor({}:{}.{}:{result}:",
                accessor.raw(),
                class_definition,
                slot_definition
            );
            fingerprint_sem_expr(module, receiver, out);
            out.push(')');
        }
        sem::Expr::Binary { op, lhs, rhs, .. } => {
            let _ = write!(out, "binary({op:?}:");
            fingerprint_sem_expr(module, lhs, out);
            out.push(',');
            fingerprint_sem_expr(module, rhs, out);
            out.push(')');
        }
        sem::Expr::Compare { op, lhs, rhs, .. } => {
            let _ = write!(out, "compare({op:?}:");
            fingerprint_sem_expr(module, lhs, out);
            out.push(',');
            fingerprint_sem_expr(module, rhs, out);
            out.push(')');
        }
        sem::Expr::If {
            condition,
            refinement,
            then_branch,
            else_branch,
            ..
        } => {
            out.push_str("if(");
            fingerprint_sem_expr(module, condition, out);
            if let Some(refinement) = refinement {
                let kind = sem_kind_fingerprint(module, refinement.kind);
                let _ = write!(out, "|refine{}={kind}", refinement.local.0);
            }
            out.push('?');
            fingerprint_sem_expr(module, then_branch, out);
            out.push(':');
            fingerprint_sem_expr(module, else_branch, out);
            out.push(')');
        }
        sem::Expr::Handle { body, restart, .. } => {
            let definition = module
                .classes
                .get(restart.specializer.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let condition = restart.condition_local.map_or(u32::MAX, |local| local.0);
            let _ = write!(
                out,
                "handle({definition}:{:?}:condition={condition}:body=",
                restart.result_kind
            );
            fingerprint_sem_expr(module, body, out);
            out.push_str(":restart=");
            fingerprint_sem_expr(module, &restart.body, out);
            out.push(')');
        }
        sem::Expr::Error { .. } => {
            out.push_str("error");
        }
    }
}

fn fingerprint_args(module: &sem::Module, args: &[sem::Expr], out: &mut String) {
    for arg in args {
        fingerprint_sem_expr(module, arg, out);
        out.push(',');
    }
}

fn artifact_label(artifact: QueryArtifact) -> String {
    match artifact {
        QueryArtifact::Source => "source".into(),
        QueryArtifact::Parse => "parse".into(),
        QueryArtifact::Ast => "ast".into(),
        QueryArtifact::Bind => "bind".into(),
        QueryArtifact::Diagnostics => "diagnostics".into(),
        QueryArtifact::MethodLookup(id) => format!("method-lookup {id}"),
        QueryArtifact::MethodLowering(id) => format!("method-lowering {id}"),
        QueryArtifact::MethodTape(id) => format!("method-tape {id}"),
        QueryArtifact::GenericDispatcher(id) => format!("generic-dispatcher {id}"),
        QueryArtifact::LoweredModule => "lowered-module".into(),
        QueryArtifact::TapeProgram => "tape-program".into(),
        QueryArtifact::RuntimeResult => "runtime-result".into(),
    }
}

fn indent_line(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: &str = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                        define generic area(x);\n\
                        define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                        define function main()\n  area(make(<circle>, radius: 10))\nend;";

    fn eop_object<'a>(objects: &'a [EnvironmentObject], id: &str) -> &'a EnvironmentObject {
        objects
            .iter()
            .find(|object| object.id.as_str() == id)
            .expect("environment object should exist")
    }

    #[test]
    fn environment_affordance_names_are_stable() {
        let names = [
            (EnvironmentAffordance::Inspect, "inspect"),
            (EnvironmentAffordance::DumpSyntax, "dump-syntax"),
            (EnvironmentAffordance::DumpAst, "dump-ast"),
            (EnvironmentAffordance::DumpSem, "dump-sem"),
            (EnvironmentAffordance::DumpMop, "dump-mop"),
            (EnvironmentAffordance::DumpEop, "dump-eop"),
            (EnvironmentAffordance::DumpLowered, "dump-lowered"),
            (EnvironmentAffordance::DumpTape, "dump-tape"),
            (EnvironmentAffordance::ShowDispatch, "show-dispatch"),
            (
                EnvironmentAffordance::ShowRuntimeLayout,
                "show-runtime-layout",
            ),
            (EnvironmentAffordance::ShowDependencies, "show-dependencies"),
            (EnvironmentAffordance::ShowAccessor, "show-accessor"),
            (EnvironmentAffordance::ShowSlot, "show-slot"),
            (EnvironmentAffordance::Run, "run"),
            (EnvironmentAffordance::EditBody, "edit-body"),
            (EnvironmentAffordance::AddMethod, "add-method"),
        ];

        for (affordance, name) in names {
            assert_eq!(affordance.as_str(), name);
        }
    }

    #[test]
    fn session_collects_stages_artifacts_and_runtime() {
        let artifacts = CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::RunMain);

        assert_eq!(artifacts.diagnostics, Vec::new());
        assert!(artifacts.lowered.is_some());
        assert!(artifacts.tape.is_some());
        assert_eq!(
            artifacts.runtime_result,
            Some(RuntimeResult::ReturnedI64(100))
        );
        assert!(
            artifacts
                .artifact_reports()
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::TapeProgram))
        );
        assert!(artifacts.dump_stage(DumpStage::Syntax).contains("Function"));
        assert!(artifacts.dump_stage(DumpStage::Ast).contains("function"));
        assert!(
            artifacts
                .dump_stage(DumpStage::Ast)
                .contains("radius :: <integer>")
        );
        assert!(artifacts.dump_stage(DumpStage::Sem).contains("slot s0"));
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains(":: <integer>")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("generic-call")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("class-object")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Sem)
                .contains("accessor-call")
        );
        let mop = artifacts.dump_stage(DumpStage::Mop);
        assert!(mop.contains("mop"));
        assert!(mop.contains("classes:"));
        assert!(mop.contains("origin=native"));
        assert!(mop.contains("runtime=native"));
        assert!(mop.contains("dispatch table:"));
        let eop = artifacts.dump_stage(DumpStage::Eop);
        assert!(eop.contains("eop"));
        assert!(eop.contains("object source:"));
        assert!(eop.contains("kind=source-file"));
        assert!(eop.contains("object class:c0"));
        assert!(eop.contains("object generic:g0"));
        assert!(eop.contains("object method:m0"));
        assert!(eop.contains("object function:f0"));
        assert!(eop.contains("object artifact:"));
        assert!(
            artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("area#dispatch")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("slot.read")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Tape)
                .contains("program=\"cairn\"")
        );
        assert!(artifacts.dump_artifact_graph().contains("dependencies:"));
    }

    #[test]
    fn session_projects_structured_eop_records() {
        let artifacts = CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::RunMain);
        let objects = artifacts.environment_objects();

        let source = eop_object(&objects, &format!("source:{}", artifacts.source.id));
        assert_eq!(source.kind, EnvironmentKind::SourceFile);
        assert_eq!(source.origin, EnvironmentOrigin::SourceAuthored);
        assert_eq!(source.storage, EnvironmentStorage::SourceDefined);
        assert!(source.affordances.contains(EnvironmentAffordance::DumpEop));

        let class = eop_object(&objects, "class:c0");
        assert_eq!(class.kind, EnvironmentKind::Class);
        assert_eq!(class.origin, EnvironmentOrigin::SourceAuthored);
        assert_eq!(class.storage, EnvironmentStorage::SourceDefined);
        assert!(
            class
                .affordances
                .contains(EnvironmentAffordance::ShowRuntimeLayout)
        );

        let slot = eop_object(&objects, "slot:c0.s0");
        assert_eq!(slot.kind, EnvironmentKind::Slot);
        assert!(
            slot.affordances
                .contains(EnvironmentAffordance::ShowAccessor)
        );

        let accessor = eop_object(&objects, "accessor:c0.s0");
        assert_eq!(accessor.kind, EnvironmentKind::Accessor);
        assert!(
            accessor
                .execution
                .as_ref()
                .is_some_and(|execution| execution.detail.contains("reads=slot:c0.s0"))
        );

        let generic = eop_object(&objects, "generic:g0");
        assert_eq!(generic.kind, EnvironmentKind::Generic);
        assert!(
            generic
                .affordances
                .contains(EnvironmentAffordance::AddMethod)
        );

        let method = eop_object(&objects, "method:m0");
        assert_eq!(method.kind, EnvironmentKind::Method);
        assert!(
            method
                .execution
                .as_ref()
                .is_some_and(|execution| execution.detail.contains("participates-in=generic:g0"))
        );

        let function = eop_object(&objects, "function:f0");
        assert_eq!(function.kind, EnvironmentKind::Function);
        assert!(function.affordances.contains(EnvironmentAffordance::Run));

        let runtime_class = eop_object(&objects, "runtime-class:c0");
        assert_eq!(runtime_class.kind, EnvironmentKind::RuntimeLayoutRow);
        assert!(
            runtime_class
                .inspection
                .detail
                .contains("storage-kind=native")
        );

        assert!(
            objects
                .iter()
                .any(|object| object.kind == EnvironmentKind::Artifact
                    && object.storage == EnvironmentStorage::CachedArtifact)
        );
    }

    #[test]
    fn session_dumps_host_backed_metaobjects() {
        let mut registry = sem::HostMetaRegistry::new();
        registry.add_class(
            sem::HostClass::new("<rust-circle>").with_slot(sem::HostSlot::new(
                "radius",
                sem::ValueKind::I64,
                "<integer>",
            )),
        );
        let artifacts = CompilerSession::compile_source_with_host_registry(
            "host.cairn",
            "define generic area(x :: <object>) => <integer>;\n\
             define method area(c :: <rust-circle>) => <integer>\n  c.radius * c.radius\nend;",
            RuntimeMode::CompileOnly,
            &registry,
        );

        assert_eq!(artifacts.diagnostics, Vec::new());
        let sem = artifacts.dump_stage(DumpStage::Sem);
        assert!(sem.contains("class c0"));
        assert!(sem.contains("origin=host-backed(hc0)"));
        assert!(sem.contains("slot s0"));
        assert!(sem.contains("origin=host-backed(hc0.hs0)"));
        let mop = artifacts.dump_stage(DumpStage::Mop);
        assert!(mop.contains("<rust-circle> origin=host-backed(hc0)"));
        assert!(mop.contains("runtime=host-backed"));
        assert!(mop.contains("storage=host-backed"));
        assert!(mop.contains("c0 <rust-circle> origin=host-backed(hc0) -> m0"));
        let eop = artifacts.dump_stage(DumpStage::Eop);
        assert!(eop.contains("object class:c0"));
        assert!(eop.contains("origin=host-backed storage=host-registry"));
        assert!(eop.contains("object slot:c0.s0"));
        assert!(eop.contains("storage=host-adapter"));
        assert!(eop.contains("object runtime-class:c0"));

        let objects = artifacts.environment_objects();
        let class = eop_object(&objects, "class:c0");
        assert_eq!(class.origin, EnvironmentOrigin::HostBacked);
        assert_eq!(class.storage, EnvironmentStorage::HostRegistry);
        let slot = eop_object(&objects, "slot:c0.s0");
        assert_eq!(slot.origin, EnvironmentOrigin::HostBacked);
        assert_eq!(slot.storage, EnvironmentStorage::HostAdapter);
    }

    struct ExampleHostProvider;

    impl HostEnvironmentProvider for ExampleHostProvider {
        fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink) {
            sink.emit(EnvironmentObject {
                id: EnvironmentObjectId::new("host:rust-circle"),
                kind: EnvironmentKind::HostObject,
                meta_class: "<host-object>".to_string(),
                origin: EnvironmentOrigin::HostProvided,
                storage: EnvironmentStorage::HostOwned,
                presentation: PresentationFacet::new("Rust circle instance"),
                affordances: AffordanceFacet::new(Vec::from([
                    EnvironmentAffordance::Inspect,
                    EnvironmentAffordance::DumpEop,
                    EnvironmentAffordance::ShowRuntimeLayout,
                ])),
                execution: None,
                inspection: InspectionFacet::new("provider=example type=RustCircle"),
            });
        }
    }

    #[test]
    fn host_environment_provider_projects_explicit_records() {
        let artifacts =
            CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::CompileOnly);
        let objects = artifacts.environment_objects_with_host(&ExampleHostProvider);

        let host = eop_object(&objects, "host:rust-circle");
        assert_eq!(host.kind, EnvironmentKind::HostObject);
        assert_eq!(host.origin, EnvironmentOrigin::HostProvided);
        assert_eq!(host.storage, EnvironmentStorage::HostOwned);
        assert!(host.affordances.contains(EnvironmentAffordance::DumpEop));
        assert!(host.inspection.detail.contains("type=RustCircle"));

        assert!(
            objects
                .iter()
                .any(|object| object.id.as_str() == "class:c0")
        );
    }

    #[test]
    fn session_diff_preserves_ids_for_harmless_edit() {
        let before = CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.cairn",
            "\n// same program with trivia edits\n\
             define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius   *   c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.changed.is_empty());
        assert!(diff.affected.is_empty());
        assert!(
            diff.preserved
                .iter()
                .any(|definition| definition.kind == DefinitionKind::Method)
        );
    }

    #[test]
    fn session_diff_reports_focused_method_artifacts() {
        let before = CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.cairn",
            "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius + c.radius\nend;\n\
             define function main()\n  area(make(<circle>, radius: 10))\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(
            diff.changed
                .iter()
                .any(|change| change.definition.kind == DefinitionKind::Method
                    && change.change == DefinitionChangeKind::Modified)
        );
        assert!(
            diff.affected
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::MethodLookup(_)))
        );
        assert!(
            diff.affected
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::TapeProgram))
        );
    }

    #[test]
    fn incremental_first_compile_populates_cache_and_repeat_reuses_all() {
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", AREA);

        let first = session
            .compile(file, RuntimeMode::RunMain)
            .expect("source should exist");
        assert_eq!(first.recomputed_count(), first.reports.len());
        assert_eq!(first.reused_count(), 0);
        assert_eq!(
            first.artifacts.runtime_result,
            Some(RuntimeResult::ReturnedI64(100))
        );

        let second = session
            .compile(file, RuntimeMode::RunMain)
            .expect("source should exist");
        assert_eq!(second.reused_count(), second.reports.len());
        assert_eq!(second.recomputed_count(), 0);
    }

    #[test]
    fn incremental_trivia_edit_reuses_most_artifacts() {
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", AREA);
        let _ = session
            .compile(file, RuntimeMode::RunMain)
            .expect("source should exist");

        let update = session
            .edit_and_compile(
                file,
                "\n// same definitions, with harmless trivia changes\n\
                 define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                 define generic area(x);\n\
                 define method area(c :: <circle>)\n  c.radius   *   c.radius\nend;\n\
                 define function main()\n  area(make(<circle>, radius: 10))\nend;",
                RuntimeMode::RunMain,
            )
            .expect("source should exist");
        let method = update.artifacts.sem.methods[0].definition;

        assert!(update.reused_count() > update.recomputed_count());
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::Source),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_method_body_edit_preserves_unrelated_method_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius + c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_branch_edit_recomputes_only_affected_method_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  if c.radius > 0 then c.radius * c.radius else 0 end\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  if c.radius > 0 then c.radius + c.radius else 0 end\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_restart_handler_edit_recomputes_only_affected_method_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define class <invalid-radius> (<object>)\n  slot fallback :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  handle if c.radius < 0 then signal(make(<invalid-radius>, fallback: 0)) else c.radius * c.radius end with restart e :: <invalid-radius> e.fallback end\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 0 - 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define class <invalid-radius> (<object>)\n  slot fallback :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  handle if c.radius < 0 then signal(make(<invalid-radius>, fallback: 0)) else c.radius * c.radius end with restart e :: <invalid-radius> e.fallback + 1 end\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 0 - 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
    }

    #[test]
    fn incremental_slot_type_edit_recomputes_dependent_artifacts() {
        let before = "define class <flag> (<object>)\n  slot value :: <boolean>;\nend;\n\
                      define generic score(x);\n\
                      define method score(f :: <flag>)\n  if f.value then 1 else 0 end\nend;\n\
                      define function main()\n  score(make(<flag>, value: #t))\nend;";
        let after = "define class <flag> (<object>)\n  slot value :: <integer>;\nend;\n\
                     define generic score(x);\n\
                     define method score(f :: <flag>)\n  f.value\nend;\n\
                     define function main()\n  score(make(<flag>, value: 1))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("flag.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let method = first.artifacts.sem.methods[0].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("value :: <integer>")
        );
    }

    #[test]
    fn incremental_slot_keyword_edit_reuses_lowering_when_runtime_layout_is_unchanged() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>, init-keyword: radius:;\nend;\n\
                      define function main()\n  make(<circle>, radius: 10).radius\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>, init-keyword: r:;\nend;\n\
                     define function main()\n  make(<circle>, r: 10).radius\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("circle.cairn", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::Bind),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Reused)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("init-keyword: r:")
        );
    }

    #[test]
    fn session_diff_preserves_ids_for_slot_declaration_trivia() {
        let before = CompilerSession::compile_source(
            "circle.cairn",
            "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 10;\nend;\n\
             define function main()\n  make(<circle>).radius\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "circle.cairn",
            "\n// same slot facts with trivia edits\n\
             define class <circle> (<object>)\n  slot radius   ::   <integer>,   init-value:   10;\nend;\n\
             define function main()\n  make(<circle>).radius\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.changed.is_empty());
        assert!(diff.affected.is_empty());
        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Slot && definition.name == "<circle>.radius"
        }));
    }

    #[test]
    fn session_diff_preserves_ids_for_superclass_trivia() {
        let before = CompilerSession::compile_source(
            "point.cairn",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "point.cairn",
            "\n// same inheritance facts with trivia edits\n\
             define class <shape> (<object>)\n  slot x   ::   <integer>, init-value: 1;\nend;\n\
             define class <point>   (   <shape>   )\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.changed.is_empty());
        assert!(diff.affected.is_empty());
        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Class && definition.name == "<point>"
        }));
    }

    #[test]
    fn parent_slot_edit_affects_child_finalization_and_executable_artifacts() {
        let before = "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
                      define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
                      define function main()\n  make(<point>).x\nend;";
        let after = "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 3;\nend;\n\
                     define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
                     define function main()\n  make(<point>).x\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("point.cairn", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.definition.name == "<shape>.x"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Class
                && change.definition.name == "<point>"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Mop)
                .contains("source=inherited")
        );
    }

    #[test]
    fn child_slot_edit_does_not_mark_parent_definitions_changed() {
        let before = CompilerSession::compile_source(
            "point.cairn",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<point>).y\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "point.cairn",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 3;\nend;\n\
             define function main()\n  make(<point>).y\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(!diff.changed.iter().any(|change| {
            change.definition.name == "<shape>" || change.definition.name == "<shape>.x"
        }));
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot && change.definition.name == "<point>.y"
        }));
    }

    #[test]
    fn adding_subclass_preserves_parent_definition_ids() {
        let before = CompilerSession::compile_source(
            "shape.cairn",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define function main()\n  make(<shape>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let after = CompilerSession::compile_source(
            "shape.cairn",
            "define class <shape> (<object>)\n  slot x :: <integer>, init-value: 1;\nend;\n\
             define class <point> (<shape>)\n  slot y :: <integer>, init-value: 2;\nend;\n\
             define function main()\n  make(<shape>).x\nend;",
            RuntimeMode::CompileOnly,
        );
        let diff = CompilerSession::diff(&before, &after);

        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Class && definition.name == "<shape>"
        }));
        assert!(diff.preserved.iter().any(|definition| {
            definition.kind == DefinitionKind::Slot && definition.name == "<shape>.x"
        }));
        assert!(!diff.changed.iter().any(|change| {
            change.definition.name == "<shape>" || change.definition.name == "<shape>.x"
        }));
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Class
                && change.definition.name == "<point>"
                && change.change == DefinitionChangeKind::Added
        }));
    }

    #[test]
    fn incremental_slot_init_value_edit_recomputes_executable_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 10;\nend;\n\
                      define function main()\n  make(<circle>).radius\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 11;\nend;\n\
                     define function main()\n  make(<circle>).radius\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("circle.cairn", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::Bind),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("init-value=yes")
        );
    }

    #[test]
    fn incremental_slot_mutability_edit_reuses_executable_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 10;\nend;\n\
                      define function main()\n  make(<circle>).radius\nend;";
        let after = "define class <circle> (<object>)\n  constant slot radius :: <integer>, init-value: 10;\nend;\n\
                     define function main()\n  make(<circle>).radius\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("circle.cairn", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Slot
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::Bind),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Reused)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("mutability=constant")
        );
    }

    #[test]
    fn incremental_definition_edit_marks_broad_dependents_affected() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", before);
        let _ = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert!(update.invalidated_count() > 0);
        assert!(cache_report(&update, QueryArtifact::Bind).is_some_and(|report| report.affected));
        assert!(
            cache_report(&update, QueryArtifact::LoweredModule)
                .is_some_and(|report| report.affected)
        );
        assert!(
            cache_report(&update, QueryArtifact::TapeProgram).is_some_and(|report| report.affected)
        );
    }

    #[test]
    fn incremental_class_reference_edit_recomputes_only_dependent_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  let klass = <circle>;\n  area(make(klass, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function main()\n  let klass = <square>;\n  area(make(klass, side: 9))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Function
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("class-object c1")
        );
    }

    #[test]
    fn incremental_callable_target_edit_recomputes_only_function_artifacts() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define function inc(x)\n  x + 1\nend;\n\
                      define function dec(x)\n  x - 1\nend;\n\
                      define function main()\n  let f = inc;\n  apply(f, 10) + area(make(<circle>, radius: 2))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define function inc(x)\n  x + 1\nend;\n\
                     define function dec(x)\n  x - 1\nend;\n\
                     define function main()\n  let f = dec;\n  apply(f, 10) + area(make(<circle>, radius: 2))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("callable.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let method = first.artifacts.sem.methods[0].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Function
                && change.definition.name == "main"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("known-callable function f1")
        );
    }

    #[test]
    fn incremental_function_signature_edit_recomputes_callers_without_method_recompute() {
        let before = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                      define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function hold(x)\n  x\nend;\n\
                      define function main()\n  let ignored = hold(7);\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius :: <integer>;\nend;\n\
                     define class <square> (<object>)\n  slot side :: <integer>;\nend;\n\
                     define generic area(x);\n\
                     define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                     define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                     define function hold(x :: <object>) => <object>\n  x\nend;\n\
                     define function main()\n  let ignored = hold(7);\n  area(make(<circle>, radius: 10))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("area.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let circle = first.artifacts.sem.methods[0].definition;
        let square = first.artifacts.sem.methods[1].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let diff = update.diff.as_ref().expect("edit should produce a diff");

        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Function
                && change.definition.name == "hold"
                && change.change == DefinitionChangeKind::Modified
        }));
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(circle)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(square)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::LoweredModule),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::TapeProgram),
            Some(CacheAction::Recomputed)
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Sem)
                .contains("hold(x:object-top) result=Any return=object-top")
        );
        assert!(
            update
                .artifacts
                .dump_stage(DumpStage::Lowered)
                .contains("box.any")
        );
    }

    fn cache_action(update: &IncrementalUpdate, artifact: QueryArtifact) -> Option<CacheAction> {
        cache_report(update, artifact).map(|report| report.action)
    }

    fn cache_report(
        update: &IncrementalUpdate,
        artifact: QueryArtifact,
    ) -> Option<&CacheArtifactReport> {
        update
            .reports
            .iter()
            .find(|report| report.artifact == artifact)
    }
}
