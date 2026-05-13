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

use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::identity::{ArtifactKey, DefinitionId, DefinitionKind, SourceFileId, StableIdBuilder};
use crate::incremental::{ArtifactEdge, ArtifactReport, QueryArtifact, QueryInvalidation};
use crate::runtime::{CairnHost, HostObjectKind, RuntimeLayout};
use crate::syntax::{CstElement, CstNode, Span, SyntaxTree, TokenKind};
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

impl RuntimeResult {
    /// Stable code for non-successful runtime/session outcomes.
    #[must_use]
    pub const fn code(&self) -> Option<DiagnosticCode> {
        match self {
            Self::ReturnedI64(_) => None,
            Self::MissingMain => Some(DiagnosticCode::RuntimeMissingMain),
            Self::MainHasParameters => Some(DiagnosticCode::RuntimeMainHasParameters),
            Self::Trap { .. } => Some(DiagnosticCode::RuntimeTrap),
            Self::MainReturnedNonI64 => Some(DiagnosticCode::RuntimeMainReturnedNonI64),
        }
    }
}

/// Result of running a named zero-argument function from compiled artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FunctionRunResult {
    /// The function returned an integer result.
    ReturnedI64(i64),
    /// The requested function was not present in the semantic/tape artifacts.
    MissingFunction {
        /// Requested function name.
        name: String,
    },
    /// The requested function exists but expects parameters.
    FunctionHasParameters {
        /// Requested function name.
        name: String,
    },
    /// The artifacts do not contain an emitted tape program.
    TapeUnavailable,
    /// Runtime trapped.
    Trap {
        /// Trap details.
        message: String,
    },
    /// The function returned a non-integer result.
    FunctionReturnedNonI64 {
        /// Requested function name.
        name: String,
    },
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
    /// A semantic generic dispatch-site object.
    DispatchSite,
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
            Self::DispatchSite => "dispatch-site",
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

    /// Returns inspectable generic dispatch-site records in traversal order.
    #[must_use]
    pub fn dispatch_sites(&self) -> Vec<DispatchSiteRecord> {
        collect_dispatch_sites(self)
    }

    /// Returns one dispatch-site record by friendly selector id.
    #[must_use]
    pub fn dispatch_site(&self, id: &str) -> Option<DispatchSiteRecord> {
        self.dispatch_sites().into_iter().find(|site| site.id == id)
    }

    /// Dumps a compact list of generic dispatch sites.
    #[must_use]
    pub fn dump_dispatch_sites(&self) -> String {
        dump_dispatch_sites(self)
    }

    /// Explains one generic dispatch site by friendly selector id.
    #[must_use]
    pub fn explain_dispatch_site(&self, id: &str) -> Option<String> {
        self.dispatch_site(id)
            .map(|site| explain_dispatch_site(self, &site))
    }

    /// Runs a named zero-argument function from this artifact set.
    ///
    /// This is intended for workbench and REPL tooling that needs to evaluate a
    /// generated scratch function without pretending it is `main`.
    #[must_use]
    pub fn run_zero_arg_function_i64(&self, function_name: &str) -> FunctionRunResult {
        self.tape
            .as_ref()
            .map_or(FunctionRunResult::TapeUnavailable, |tape| {
                run_zero_arg_function_i64(&self.sem, tape, function_name)
            })
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

/// Stable backing key for one dispatch-site record.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DispatchSiteKey(
    /// Stable raw identifier.
    pub u64,
);

impl DispatchSiteKey {
    /// Returns the raw key bits.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DispatchSiteKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "dsk{:016x}", self.0)
    }
}

/// Kind of source definition enclosing a dispatch site.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DispatchSiteOwnerKind {
    /// The dispatch site is inside a direct function body.
    Function,
    /// The dispatch site is inside a method body.
    Method,
}

impl DispatchSiteOwnerKind {
    /// Returns the stable text label for this owner kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
        }
    }
}

/// Source definition enclosing a dispatch site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchSiteOwner {
    /// Owner kind.
    pub kind: DispatchSiteOwnerKind,
    /// Stable owner definition id.
    pub definition: DefinitionId,
    /// Stable owner display name.
    pub name: String,
}

/// One method considered by a dispatch-site explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchMethodCandidate {
    /// Method id.
    pub method: sem::MethodId,
    /// Stable method definition id.
    pub definition: DefinitionId,
    /// Method specializer class.
    pub specializer: sem::ClassId,
    /// Distance from the static exact class to the specializer, if known.
    pub distance: Option<usize>,
    /// Whether this is the statically selected method.
    pub selected: bool,
    /// Whether this candidate can only be selected by runtime class dispatch.
    pub runtime_only: bool,
}

/// One rejected method in a dispatch-site explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchMethodRejection {
    /// Method id.
    pub method: sem::MethodId,
    /// Stable method definition id.
    pub definition: DefinitionId,
    /// Method specializer class.
    pub specializer: sem::ClassId,
    /// Stable rejection reason.
    pub reason: String,
}

/// Inspectable record for one semantic generic call site.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchSiteRecord {
    /// Friendly selector such as `dispatch:d0`.
    pub id: String,
    /// Stable backing key derived from source, owner, call-site, and generic.
    pub key: DispatchSiteKey,
    /// Zero-based dispatch-site index in module traversal order.
    pub index: usize,
    /// Enclosing function or method.
    pub owner: DispatchSiteOwner,
    /// Source span for the semantic generic call.
    pub span: Span,
    /// Generic function id.
    pub generic: sem::GenericFunctionId,
    /// Stable generic definition id.
    pub generic_definition: DefinitionId,
    /// Static kind of the first dispatch argument, when present.
    pub static_argument_kind: Option<sem::ValueKind>,
    /// Exact static runtime class, when the call argument is an exact
    /// construction/cast expression.
    pub exact_static_class: Option<sem::ClassId>,
    /// Whether the runtime class is still needed to choose the method.
    pub runtime_needed: bool,
    /// Statically selected method, when known.
    pub selected_method: Option<sem::MethodId>,
    /// Applicable methods in dispatcher specificity order.
    pub applicable: Vec<DispatchMethodCandidate>,
    /// Rejected methods and reasons.
    pub rejected: Vec<DispatchMethodRejection>,
    /// Lowered dispatcher function name.
    pub lowered_dispatcher: String,
    /// Related query artifact reports.
    pub related_artifacts: Vec<ArtifactReport>,
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
        builder.mix_str(diagnostic.code.as_str());
        builder.mix_u64(u64::try_from(diagnostic.span.start).unwrap_or(u64::MAX));
        builder.mix_u64(u64::try_from(diagnostic.span.end).unwrap_or(u64::MAX));
        builder.mix_u64(u64::try_from(diagnostic.args.len()).unwrap_or(u64::MAX));
        for arg in &diagnostic.args {
            builder.mix_str(&arg.name);
            builder.mix_str(&arg.value.render());
        }
        builder.mix_u64(u64::try_from(diagnostic.labels.len()).unwrap_or(u64::MAX));
        for label in &diagnostic.labels {
            builder.mix_u64(u64::try_from(label.span.start).unwrap_or(u64::MAX));
            builder.mix_u64(u64::try_from(label.span.end).unwrap_or(u64::MAX));
            if let Some(message) = &label.message {
                builder.mix_str(message);
            }
        }
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
    let mut methods: Vec<_> = module
        .methods
        .iter()
        .filter(|method| method.generic == generic.id)
        .collect();
    methods.sort_by(|left, right| {
        module
            .class_depth(right.specializer)
            .cmp(&module.class_depth(left.specializer))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    for method in methods {
        let specializer = module
            .classes
            .get(method.specializer.0 as usize)
            .map_or(0, |class| class.definition.raw());
        let _ = write!(out, "{}:{specializer}:[", method.definition.raw());
        for class in module
            .classes
            .iter()
            .filter(|class| module.is_same_or_subclass(class.id, method.specializer))
        {
            let _ = write!(out, "{},", class.definition.raw());
        }
        let _ = write!(out, "];");
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
            .map(|diagnostic| {
                usize_to_u64(diagnostic.code.as_str().len())
                    .saturating_add(usize_to_u64(diagnostic.args.len()))
                    .saturating_add(usize_to_u64(diagnostic.labels.len()))
                    .saturating_add(1)
            })
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

fn collect_dispatch_sites(artifacts: &SessionArtifacts) -> Vec<DispatchSiteRecord> {
    let mut sites = Vec::new();
    for function in &artifacts.sem.functions {
        let owner = DispatchSiteOwner {
            kind: DispatchSiteOwnerKind::Function,
            definition: function.definition,
            name: function.name.clone(),
        };
        let mut owner_index = 0;
        collect_dispatch_sites_in_block(
            artifacts,
            owner,
            &function.body,
            &mut owner_index,
            &mut sites,
        );
    }
    for method in &artifacts.sem.methods {
        let generic_name = artifacts
            .sem
            .generics
            .get(method.generic.0 as usize)
            .map_or("<?>", |generic| generic.name.as_str());
        let specializer = class_label(&artifacts.sem, method.specializer);
        let owner = DispatchSiteOwner {
            kind: DispatchSiteOwnerKind::Method,
            definition: method.definition,
            name: format!("{generic_name}({specializer})"),
        };
        let mut owner_index = 0;
        collect_dispatch_sites_in_block(
            artifacts,
            owner,
            &method.body,
            &mut owner_index,
            &mut sites,
        );
    }
    sites
}

fn collect_dispatch_sites_in_block(
    artifacts: &SessionArtifacts,
    owner: DispatchSiteOwner,
    block: &sem::Block,
    owner_index: &mut usize,
    sites: &mut Vec<DispatchSiteRecord>,
) {
    for binding in &block.lets {
        collect_dispatch_sites_in_expr(
            artifacts,
            owner.clone(),
            &binding.value,
            owner_index,
            sites,
        );
    }
    collect_dispatch_sites_in_expr(artifacts, owner, &block.result, owner_index, sites);
}

fn collect_dispatch_sites_in_expr(
    artifacts: &SessionArtifacts,
    owner: DispatchSiteOwner,
    expr: &sem::Expr,
    owner_index: &mut usize,
    sites: &mut Vec<DispatchSiteRecord>,
) {
    match expr {
        sem::Expr::DirectCall { args, .. } => {
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::GenericCall {
            generic,
            args,
            span,
        } => {
            let site = make_dispatch_site(
                artifacts,
                owner.clone(),
                *owner_index,
                sites.len(),
                *generic,
                args,
                *span,
            );
            *owner_index = owner_index.saturating_add(1);
            sites.push(site);
            for arg in args {
                collect_dispatch_sites_in_expr(artifacts, owner.clone(), arg, owner_index, sites);
            }
        }
        sem::Expr::Signal { condition, .. }
        | sem::Expr::ClassOf {
            object: condition, ..
        } => {
            collect_dispatch_sites_in_expr(artifacts, owner, condition, owner_index, sites);
        }
        sem::Expr::InstanceOf { value, .. } | sem::Expr::Cast { value, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner, value, owner_index, sites);
        }
        sem::Expr::MakeObject { class, fields, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), class, owner_index, sites);
            for field in fields {
                collect_dispatch_sites_in_expr(
                    artifacts,
                    owner.clone(),
                    &field.value,
                    owner_index,
                    sites,
                );
            }
        }
        sem::Expr::AccessorCall { receiver, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner, receiver, owner_index, sites);
        }
        sem::Expr::Binary { lhs, rhs, .. } | sem::Expr::Compare { lhs, rhs, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), lhs, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, rhs, owner_index, sites);
        }
        sem::Expr::ValueEqual { lhs, rhs, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), lhs, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, rhs, owner_index, sites);
        }
        sem::Expr::VectorLiteral { elements, .. } => {
            for element in elements {
                collect_dispatch_sites_in_expr(
                    artifacts,
                    owner.clone(),
                    element,
                    owner_index,
                    sites,
                );
            }
        }
        sem::Expr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), condition, owner_index, sites);
            collect_dispatch_sites_in_expr(
                artifacts,
                owner.clone(),
                then_branch,
                owner_index,
                sites,
            );
            collect_dispatch_sites_in_expr(artifacts, owner, else_branch, owner_index, sites);
        }
        sem::Expr::Handle { body, restart, .. } => {
            collect_dispatch_sites_in_expr(artifacts, owner.clone(), body, owner_index, sites);
            collect_dispatch_sites_in_expr(artifacts, owner, &restart.body, owner_index, sites);
        }
        sem::Expr::ConstI64 { .. }
        | sem::Expr::ConstBool { .. }
        | sem::Expr::StringLiteral { .. }
        | sem::Expr::SymbolLiteral { .. }
        | sem::Expr::Local { .. }
        | sem::Expr::ClassObject { .. }
        | sem::Expr::KnownCallable { .. }
        | sem::Expr::Error { .. } => {}
    }
}

fn make_dispatch_site(
    artifacts: &SessionArtifacts,
    owner: DispatchSiteOwner,
    owner_index: usize,
    global_index: usize,
    generic_id: sem::GenericFunctionId,
    args: &[sem::Expr],
    span: Span,
) -> DispatchSiteRecord {
    let module = &artifacts.sem;
    let generic_definition = module.generics.get(generic_id.0 as usize).map_or(
        DefinitionId::new(module.source_file, DefinitionKind::Generic, "<missing>", ""),
        |generic| generic.definition,
    );
    let static_argument_kind = args.first().map(|arg| sem_expr_kind(module, arg));
    let exact_static_class = args.first().and_then(|arg| exact_static_class(module, arg));
    let runtime_needed = exact_static_class.is_none()
        && static_argument_kind.is_some_and(|kind| {
            matches!(kind, sem::ValueKind::Any) || sem_value_kind_class(module, kind).is_some()
        });
    let (applicable, rejected, selected_method) =
        dispatch_site_methods(module, generic_id, static_argument_kind, exact_static_class);
    let lowered_dispatcher = module.generics.get(generic_id.0 as usize).map_or_else(
        || format!("g{}#dispatch", generic_id.0),
        |generic| format!("{}#dispatch", generic.name),
    );
    DispatchSiteRecord {
        id: format!("dispatch:d{global_index}"),
        key: dispatch_site_key(
            artifacts.source.id,
            owner.definition,
            owner_index,
            generic_definition,
        ),
        index: global_index,
        owner,
        span,
        generic: generic_id,
        generic_definition,
        static_argument_kind,
        exact_static_class,
        runtime_needed,
        selected_method,
        applicable,
        rejected,
        lowered_dispatcher,
        related_artifacts: dispatch_site_artifacts(artifacts, generic_id),
    }
}

fn dispatch_site_key(
    source: SourceFileId,
    owner: DefinitionId,
    owner_index: usize,
    generic: DefinitionId,
) -> DispatchSiteKey {
    let mut builder = StableIdBuilder::new(0x4341_4952_4453_5054);
    builder.mix_u64(source.raw());
    builder.mix_u64(owner.raw());
    builder.mix_u64(usize_to_u64(owner_index));
    builder.mix_u64(generic.raw());
    DispatchSiteKey(builder.finish())
}

fn dispatch_site_methods(
    module: &sem::Module,
    generic_id: sem::GenericFunctionId,
    static_kind: Option<sem::ValueKind>,
    exact_class: Option<sem::ClassId>,
) -> (
    Vec<DispatchMethodCandidate>,
    Vec<DispatchMethodRejection>,
    Option<sem::MethodId>,
) {
    let methods = dispatch_methods(module, generic_id);
    let selected_method = exact_class.and_then(|actual| {
        methods
            .iter()
            .find(|method| module.is_same_or_subclass(actual, method.specializer))
            .map(|method| method.id)
    });
    let mut applicable = Vec::new();
    let mut rejected = Vec::new();

    for method in methods {
        match (static_kind, exact_class) {
            (_, Some(actual)) if module.is_same_or_subclass(actual, method.specializer) => {
                applicable.push(DispatchMethodCandidate {
                    method: method.id,
                    definition: method.definition,
                    specializer: method.specializer,
                    distance: module.inheritance_distance(actual, method.specializer),
                    selected: selected_method == Some(method.id),
                    runtime_only: false,
                });
            }
            (_, Some(actual)) => rejected.push(DispatchMethodRejection {
                method: method.id,
                definition: method.definition,
                specializer: method.specializer,
                reason: exact_class_rejection_reason(module, actual, method.specializer),
            }),
            (Some(kind), None)
                if sem_value_kind_class(module, kind).is_some_and(|static_class| {
                    classes_overlap(module, static_class, method.specializer)
                }) =>
            {
                let static_class = sem_value_kind_class(module, kind).expect("class was checked");
                applicable.push(DispatchMethodCandidate {
                    method: method.id,
                    definition: method.definition,
                    specializer: method.specializer,
                    distance: module.inheritance_distance(static_class, method.specializer),
                    selected: false,
                    runtime_only: true,
                });
            }
            (Some(kind), None) if sem_value_kind_class(module, kind).is_some() => {
                let static_class = sem_value_kind_class(module, kind).expect("class was checked");
                rejected.push(DispatchMethodRejection {
                    method: method.id,
                    definition: method.definition,
                    specializer: method.specializer,
                    reason: format!(
                        "disjoint from static argument {}",
                        class_label(module, static_class)
                    ),
                });
            }
            (Some(sem::ValueKind::Any), None) => {
                applicable.push(DispatchMethodCandidate {
                    method: method.id,
                    definition: method.definition,
                    specializer: method.specializer,
                    distance: None,
                    selected: false,
                    runtime_only: true,
                });
            }
            (Some(kind), None) => rejected.push(DispatchMethodRejection {
                method: method.id,
                definition: method.definition,
                specializer: method.specializer,
                reason: format!(
                    "contract mismatch: dispatch argument is {}",
                    sem_kind_label(module, kind)
                ),
            }),
            (None, None) => rejected.push(DispatchMethodRejection {
                method: method.id,
                definition: method.definition,
                specializer: method.specializer,
                reason: "contract mismatch: missing dispatch argument".into(),
            }),
        }
    }

    (applicable, rejected, selected_method)
}

fn exact_class_rejection_reason(
    module: &sem::Module,
    actual: sem::ClassId,
    specializer: sem::ClassId,
) -> String {
    if module.is_same_or_subclass(specializer, actual) {
        format!(
            "requires subclass {} but exact class is {}",
            class_label(module, specializer),
            class_label(module, actual)
        )
    } else {
        format!("disjoint from exact class {}", class_label(module, actual))
    }
}

fn classes_overlap(module: &sem::Module, left: sem::ClassId, right: sem::ClassId) -> bool {
    module.is_same_or_subclass(left, right) || module.is_same_or_subclass(right, left)
}

fn dispatch_site_artifacts(
    artifacts: &SessionArtifacts,
    generic_id: sem::GenericFunctionId,
) -> Vec<ArtifactReport> {
    let mut reports = Vec::new();
    for method in artifacts
        .sem
        .methods
        .iter()
        .filter(|method| method.generic == generic_id)
    {
        push_artifact_report(
            &mut reports,
            artifacts,
            QueryArtifact::MethodLookup(method.definition),
        );
    }
    if let Some(generic) = artifacts.sem.generics.get(generic_id.0 as usize) {
        push_artifact_report(
            &mut reports,
            artifacts,
            QueryArtifact::GenericDispatcher(generic.definition),
        );
    }
    push_artifact_report(&mut reports, artifacts, QueryArtifact::LoweredModule);
    push_artifact_report(&mut reports, artifacts, QueryArtifact::TapeProgram);
    reports
}

fn push_artifact_report(
    reports: &mut Vec<ArtifactReport>,
    artifacts: &SessionArtifacts,
    artifact: QueryArtifact,
) {
    let key = artifacts.invalidation.key_for(artifact);
    if reports.iter().all(|report| report.key != key) {
        reports.push(ArtifactReport { artifact, key });
    }
}

fn dump_dispatch_sites(artifacts: &SessionArtifacts) -> String {
    let sites = artifacts.dispatch_sites();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "dispatch sites for {} ({})",
        artifacts.source.name, artifacts.source.id
    );
    if sites.is_empty() {
        out.push_str("  none\n");
        return out;
    }
    for site in &sites {
        let generic = generic_label(&artifacts.sem, site.generic);
        let static_kind = site.static_argument_kind.map_or_else(
            || "missing".into(),
            |kind| sem_kind_label(&artifacts.sem, kind),
        );
        let selected = site.selected_method.map_or_else(
            || "runtime".into(),
            |method| method_label(&artifacts.sem, method),
        );
        let _ = writeln!(
            out,
            "  {} key={} owner={} {} span={}..{} generic={} static-arg={} runtime-needed={} selected={}",
            site.id,
            site.key,
            site.owner.kind.as_str(),
            site.owner.name,
            site.span.start,
            site.span.end,
            generic,
            static_kind,
            site.runtime_needed,
            selected
        );
    }
    out
}

fn explain_dispatch_site(artifacts: &SessionArtifacts, site: &DispatchSiteRecord) -> String {
    let module = &artifacts.sem;
    let mut out = String::new();
    let _ = writeln!(out, "dispatch {}", site.id);
    let _ = writeln!(out, "  key: {}", site.key);
    let _ = writeln!(
        out,
        "  owner: {} {} definition={}",
        site.owner.kind.as_str(),
        site.owner.name,
        site.owner.definition
    );
    let _ = writeln!(
        out,
        "  source: {} span={}..{}",
        artifacts.source.name, site.span.start, site.span.end
    );
    let _ = writeln!(
        out,
        "  generic: {} definition={} contract={}",
        generic_label(module, site.generic),
        site.generic_definition,
        generic_contract_label(module, site.generic)
    );
    let static_kind = site
        .static_argument_kind
        .map_or_else(|| "missing".into(), |kind| sem_kind_label(module, kind));
    let exact_class = site
        .exact_static_class
        .map_or_else(|| "unknown".into(), |class| class_label(module, class));
    let selected = site.selected_method.map_or_else(
        || "runtime-needed".into(),
        |method| method_label(module, method),
    );
    let _ = writeln!(out, "  argument:");
    let _ = writeln!(out, "    static-kind: {static_kind}");
    let _ = writeln!(out, "    exact-class: {exact_class}");
    let _ = writeln!(out, "    runtime-needed: {}", site.runtime_needed);
    let _ = writeln!(out, "    selected: {selected}");
    let _ = writeln!(out, "  applicable methods:");
    if site.applicable.is_empty() {
        out.push_str("    none\n");
    } else {
        for candidate in &site.applicable {
            let selection = if candidate.selected {
                "selected"
            } else if candidate.runtime_only {
                "runtime-only"
            } else {
                "shadowed"
            };
            let distance = candidate
                .distance
                .map_or_else(|| "unknown".into(), |distance| distance.to_string());
            let _ = writeln!(
                out,
                "    {} definition={} specializer={} distance={} {}",
                method_label(module, candidate.method),
                candidate.definition,
                class_label(module, candidate.specializer),
                distance,
                selection
            );
        }
    }
    let _ = writeln!(out, "  rejected methods:");
    if site.rejected.is_empty() {
        out.push_str("    none\n");
    } else {
        for rejected in &site.rejected {
            let _ = writeln!(
                out,
                "    {} definition={} specializer={} reason={}",
                method_label(module, rejected.method),
                rejected.definition,
                class_label(module, rejected.specializer),
                rejected.reason
            );
        }
    }
    let _ = writeln!(out, "  lowering:");
    let _ = writeln!(out, "    dispatcher: {}", site.lowered_dispatcher);
    let _ = writeln!(out, "    order:");
    for method in dispatch_methods(module, site.generic) {
        let _ = writeln!(
            out,
            "      {} specializer={} applies-to={}",
            method_label(module, method.id),
            class_label(module, method.specializer),
            sem_dispatch_applicability_label(module, method.specializer)
        );
    }
    let _ = writeln!(out, "  related artifacts:");
    for report in &site.related_artifacts {
        let _ = writeln!(
            out,
            "    {} {}",
            report.key,
            artifact_label(report.artifact)
        );
    }
    out
}

fn sem_expr_kind(module: &sem::Module, expr: &sem::Expr) -> sem::ValueKind {
    match expr {
        sem::Expr::ConstI64 { .. } | sem::Expr::Binary { .. } => sem::ValueKind::I64,
        sem::Expr::ConstBool { .. }
        | sem::Expr::Compare { .. }
        | sem::Expr::ValueEqual { .. }
        | sem::Expr::InstanceOf { .. } => sem::ValueKind::Bool,
        sem::Expr::StringLiteral { class, .. } => sem::ValueKind::String(*class),
        sem::Expr::SymbolLiteral { class, .. } => sem::ValueKind::Symbol(*class),
        sem::Expr::VectorLiteral { class, .. } => sem::ValueKind::Vector(*class),
        sem::Expr::Local { result_kind, .. } => *result_kind,
        sem::Expr::ClassObject { class, .. } | sem::Expr::ClassOf { class, .. } => {
            sem::ValueKind::ClassObject(*class)
        }
        sem::Expr::KnownCallable { callable, .. } => sem::ValueKind::KnownCallable(*callable),
        sem::Expr::DirectCall { function, .. } => module
            .functions
            .get(function.0 as usize)
            .map_or(sem::ValueKind::Error, |function| function.result_kind),
        sem::Expr::GenericCall { generic, .. } => module
            .generics
            .get(generic.0 as usize)
            .map_or(sem::ValueKind::Error, |generic| generic.result_kind),
        sem::Expr::Signal { result_kind, .. } => *result_kind,
        sem::Expr::Handle { restart, .. } => restart.result_kind,
        sem::Expr::Cast { class, .. } => sem_builtin_value_kind_for_class(module, *class)
            .unwrap_or(sem::ValueKind::Object(*class)),
        sem::Expr::MakeObject {
            class_id: class, ..
        } => sem::ValueKind::Object(*class),
        sem::Expr::AccessorCall { result_kind, .. } => *result_kind,
        sem::Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            let then_kind = sem_expr_kind(module, then_branch);
            let else_kind = sem_expr_kind(module, else_branch);
            if then_kind == else_kind {
                then_kind
            } else {
                sem::ValueKind::Error
            }
        }
        sem::Expr::Error { .. } => sem::ValueKind::Error,
    }
}

fn exact_static_class(module: &sem::Module, expr: &sem::Expr) -> Option<sem::ClassId> {
    match expr {
        sem::Expr::ConstI64 { .. } => sem_builtin_class(module, sem::BuiltinValueClass::Integer),
        sem::Expr::MakeObject { class_id, .. } => Some(*class_id),
        sem::Expr::StringLiteral { class, .. }
        | sem::Expr::SymbolLiteral { class, .. }
        | sem::Expr::VectorLiteral { class, .. } => Some(*class),
        sem::Expr::Cast { value, .. } => exact_static_class(module, value),
        sem::Expr::If {
            then_branch,
            else_branch,
            ..
        } => {
            let then_class = exact_static_class(module, then_branch)?;
            let else_class = exact_static_class(module, else_branch)?;
            (then_class == else_class).then_some(then_class)
        }
        _ => None,
    }
}

fn sem_value_kind_class(module: &sem::Module, kind: sem::ValueKind) -> Option<sem::ClassId> {
    match kind {
        sem::ValueKind::Object(class)
        | sem::ValueKind::String(class)
        | sem::ValueKind::Symbol(class)
        | sem::ValueKind::Vector(class) => Some(class),
        sem::ValueKind::I64 => sem_builtin_class(module, sem::BuiltinValueClass::Integer),
        sem::ValueKind::Any
        | sem::ValueKind::Bool
        | sem::ValueKind::ClassObject(_)
        | sem::ValueKind::KnownCallable(_)
        | sem::ValueKind::Error => None,
    }
}

fn sem_builtin_class(module: &sem::Module, kind: sem::BuiltinValueClass) -> Option<sem::ClassId> {
    module
        .classes
        .iter()
        .find(|class| class.origin == sem::ClassOrigin::BuiltinValue { kind })
        .map(|class| class.id)
}

fn sem_builtin_value_kind_for_class(
    module: &sem::Module,
    class: sem::ClassId,
) -> Option<sem::ValueKind> {
    match module.classes.get(class.0 as usize)?.origin {
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::String,
        } => Some(sem::ValueKind::String(class)),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Symbol,
        } => Some(sem::ValueKind::Symbol(class)),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Integer,
        } => Some(sem::ValueKind::I64),
        sem::ClassOrigin::BuiltinValue {
            kind: sem::BuiltinValueClass::Vector,
        } => Some(sem::ValueKind::Vector(class)),
        sem::ClassOrigin::Native | sem::ClassOrigin::HostBacked { .. } => None,
    }
}

fn class_label(module: &sem::Module, class_id: sem::ClassId) -> String {
    module.classes.get(class_id.0 as usize).map_or_else(
        || format!("c{}:<missing>", class_id.0),
        |class| format!("c{}:{}", class.id.0, class.name),
    )
}

fn generic_label(module: &sem::Module, generic_id: sem::GenericFunctionId) -> String {
    module.generics.get(generic_id.0 as usize).map_or_else(
        || format!("g{}:<missing>", generic_id.0),
        |generic| format!("g{}:{}", generic.id.0, generic.name),
    )
}

fn method_label(module: &sem::Module, method_id: sem::MethodId) -> String {
    module.methods.get(method_id.0 as usize).map_or_else(
        || format!("m{}:<missing>", method_id.0),
        |method| {
            let generic = module
                .generics
                .get(method.generic.0 as usize)
                .map_or("<?>", |generic| generic.name.as_str());
            format!(
                "m{}:{}({})",
                method.id.0,
                generic,
                class_label(module, method.specializer)
            )
        },
    )
}

fn generic_contract_label(module: &sem::Module, generic_id: sem::GenericFunctionId) -> String {
    module.generics.get(generic_id.0 as usize).map_or_else(
        || "missing".into(),
        |generic| {
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
            format!(
                "{}({params}) -> {}",
                generic.name,
                sem_kind_label(module, generic.result_kind)
            )
        },
    )
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
    match run_zero_arg_function_i64(module, tape, "main") {
        FunctionRunResult::ReturnedI64(value) => RuntimeResult::ReturnedI64(value),
        FunctionRunResult::MissingFunction { .. } | FunctionRunResult::TapeUnavailable => {
            RuntimeResult::MissingMain
        }
        FunctionRunResult::FunctionHasParameters { .. } => RuntimeResult::MainHasParameters,
        FunctionRunResult::Trap { message } => RuntimeResult::Trap { message },
        FunctionRunResult::FunctionReturnedNonI64 { .. } => RuntimeResult::MainReturnedNonI64,
    }
}

fn run_zero_arg_function_i64(
    module: &sem::Module,
    tape: &tape::TapeModule,
    function_name: &str,
) -> FunctionRunResult {
    let Some(function) = module
        .functions
        .iter()
        .find(|function| function.name == function_name)
    else {
        return FunctionRunResult::MissingFunction {
            name: function_name.to_string(),
        };
    };
    if !function.params.is_empty() {
        return FunctionRunResult::FunctionHasParameters {
            name: function_name.to_string(),
        };
    }
    let Some(func) = tape.functions.get(function.id.0 as usize).copied() else {
        return FunctionRunResult::MissingFunction {
            name: function_name.to_string(),
        };
    };

    let host = CairnHost::new(tape.runtime.clone());
    let mut vm = Vm::new(host, Limits::default());
    let output = match vm.run(&tape.program, func, &[], TraceMask::NONE, None) {
        Ok(output) => output,
        Err(trap) => {
            return FunctionRunResult::Trap {
                message: format!("{trap:?}"),
            };
        }
    };

    match output.as_slice() {
        [Value::I64(value)] => FunctionRunResult::ReturnedI64(*value),
        _ => FunctionRunResult::FunctionReturnedNonI64 {
            name: function_name.to_string(),
        },
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
        ast::Expr::String { id, value, .. } => {
            let _ = writeln!(out, "{id} string {:?}", value);
        }
        ast::Expr::Symbol { id, name, .. } => {
            let _ = writeln!(out, "{id} symbol #{name}");
        }
        ast::Expr::Vector { id, elements, .. } => {
            let _ = writeln!(out, "{id} vector elements={}", elements.len());
            for element in elements {
                dump_ast_expr(out, element, indent + 2);
            }
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
        ast::Expr::UnaryMinus { id, expr, .. } => {
            let _ = writeln!(out, "{id} unary Minus");
            dump_ast_expr(out, expr, indent + 2);
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
        let superclass_chain = sem_superclass_chain_label(module, class.id);
        let _ = writeln!(
            out,
            "class c{} {} {} origin={} superclass={superclass} chain={superclass_chain} finalized={} direct-slots={} effective-slots={} init-plan={}",
            class.id.0,
            class.definition,
            class.name,
            sem_class_origin_label(class.origin),
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len(),
            sem_initialization_plan_summary(module, class)
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
        let _ = writeln!(out, "  initialization plan");
        for step in &class.initialization_plan.steps {
            let _ = writeln!(
                out,
                "    step s{} {}",
                step.slot.0,
                sem_initialization_step_detail(module, class, step)
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
        let superclass_chain = sem_superclass_chain_label(module, class.id);
        let _ = writeln!(
            out,
            "  c{} {} {} origin={} superclass={superclass} chain={superclass_chain} finalized={} direct-slots={} effective-slots={} init-plan={} runtime={runtime_kind}",
            class.id.0,
            class.definition,
            class.name,
            sem_class_origin_label(class.origin),
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len(),
            sem_initialization_plan_summary(module, class)
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
        let _ = writeln!(out, "    initialization-plan:");
        let _ = writeln!(
            out,
            "      accepted-keywords={}",
            sem_initialization_keywords(class)
        );
        let _ = writeln!(out, "      initialize-methods=deferred");
        for step in &class.initialization_plan.steps {
            let _ = writeln!(
                out,
                "      step s{} {}",
                step.slot.0,
                sem_initialization_step_detail(module, class, step)
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
                "    method m{} {} specializer=c{} {} origin={} -> {} applicable={}",
                method.id.0,
                method.definition,
                method.specializer.0,
                class_name,
                class_origin,
                sem_kind_label(module, method.result_kind),
                sem_dispatch_applicability_label(module, method.specializer)
            );
        }
    }

    let _ = writeln!(out, "dispatch table:");
    for generic in &module.generics {
        let _ = writeln!(out, "  g{} {}", generic.id.0, generic.name);
        for method in dispatch_methods(module, generic.id) {
            let class = module.classes.get(method.specializer.0 as usize);
            let class_name = class.map_or("<?>", |class| class.name.as_str());
            let class_origin = class.map_or_else(
                || "missing".into(),
                |class| sem_class_origin_label(class.origin),
            );
            let _ = writeln!(
                out,
                "    m{} specializer=c{} {} origin={} applies-to={}",
                method.id.0,
                method.specializer.0,
                class_name,
                class_origin,
                sem_dispatch_applicability_label(module, method.specializer)
            );
        }
    }

    let _ = writeln!(out, "runtime layout:");
    for (index, class) in runtime.classes.iter().enumerate() {
        let sem_class = module.classes.get(index);
        let superclass = sem_superclass_label(module, class.superclass);
        let _ = writeln!(
            out,
            "  c{index} {} kind={} superclass={superclass}",
            class.name,
            runtime_object_kind_label(class.kind)
        );
        for (slot_index, slot) in class.slots.iter().enumerate() {
            let storage = if slot.host_backed {
                "host-backed"
            } else {
                "native"
            };
            let source = sem_class
                .and_then(|sem_class| {
                    sem_class
                        .effective_slots
                        .get(slot_index)
                        .map(|effective| sem_effective_slot_source_label(sem_class, effective))
                })
                .unwrap_or_else(|| "source=missing".into());
            let _ = writeln!(
                out,
                "    s{slot_index} {} kind={} storage={} {source} runtime={}",
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
        HostObjectKind::BuiltinValue { .. } => "builtin-value",
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

fn render_diagnostic_labels(diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    out.push('[');
    for (index, label) in diagnostic.labels.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let style = match label.style {
            crate::diagnostic::DiagnosticLabelStyle::Primary => "primary",
            crate::diagnostic::DiagnosticLabelStyle::Secondary => "secondary",
        };
        let _ = write!(out, "{style}:{}..{}", label.span.start, label.span.end);
        if let Some(message) = &label.message {
            let _ = write!(out, ":\"{message}\"");
        }
    }
    out.push(']');
    out
}

fn render_diagnostic_args(diagnostic: &Diagnostic) -> String {
    let mut out = String::new();
    out.push('[');
    for (index, arg) in diagnostic.args.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let _ = write!(out, "{}={}", arg.name, arg.value.render());
    }
    out.push(']');
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
    for site in artifacts.dispatch_sites() {
        project_environment_dispatch_site(module, sink, &site);
    }
    for (index, diagnostic) in artifacts.diagnostics.iter().enumerate() {
        sink.emit(environment_object(EnvironmentObjectParts {
            id: format!("diagnostic:d{index}"),
            kind: EnvironmentKind::Diagnostic,
            meta_class: "<diagnostic>",
            origin: EnvironmentOrigin::CompilerGenerated,
            storage: EnvironmentStorage::CachedArtifact,
            title: diagnostic.code.to_string(),
            affordances: Vec::from([Aff::Inspect, Aff::OpenSource, Aff::DumpSem, Aff::DumpEop]),
            execution: None,
            inspection: format!(
                "code={} severity={:?} span={}..{} labels={} args={}",
                diagnostic.code,
                diagnostic.severity,
                diagnostic.span.start,
                diagnostic.span.end,
                render_diagnostic_labels(diagnostic),
                render_diagnostic_args(diagnostic)
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
    let superclass_chain = sem_superclass_chain_label(module, class.id);
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
            "definition={} class-id=c{} superclass={superclass} chain={superclass_chain} finalized={} direct-slots={} effective-slots={} init-plan={} accepted-keywords={} initialize-methods=deferred override=explicit-init-args-over-defaults runtime={runtime_kind}",
            class.definition,
            class.id.0,
            class.finalized,
            class.direct_slots.len(),
            class.effective_slots.len(),
            sem_initialization_plan_summary(module, class),
            sem_initialization_keywords(class)
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
    let dispatch = dispatch_methods(module, generic.id)
        .iter()
        .map(|method| {
            format!(
                "m{}=>{}",
                method.id.0,
                sem_dispatch_applicability_label(module, method.specializer)
            )
        })
        .collect::<Vec<_>>()
        .join(";");
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
            "definition={} generic-id=g{} dispatch={dispatch}",
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
    let applicable = sem_dispatch_applicability_label(module, method.specializer);
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
            "definition={} method-id=m{} specializer-origin={class_origin} applies-to={applicable}",
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

fn project_environment_dispatch_site(
    module: &sem::Module,
    sink: &mut dyn EnvironmentObjectSink,
    site: &DispatchSiteRecord,
) {
    use EnvironmentAffordance as Aff;

    let generic = generic_label(module, site.generic);
    let static_kind = site
        .static_argument_kind
        .map_or_else(|| "missing".into(), |kind| sem_kind_label(module, kind));
    let exact_class = site
        .exact_static_class
        .map_or_else(|| "unknown".into(), |class| class_label(module, class));
    let selected = site.selected_method.map_or_else(
        || "runtime-needed".into(),
        |method| method_label(module, method),
    );
    let artifact_keys = site
        .related_artifacts
        .iter()
        .map(|report| report.key.to_string())
        .collect::<Vec<_>>()
        .join(",");
    sink.emit(environment_object(EnvironmentObjectParts {
        id: site.id.clone(),
        kind: EnvironmentKind::DispatchSite,
        meta_class: "<dispatch-site>",
        origin: EnvironmentOrigin::CompilerGenerated,
        storage: EnvironmentStorage::CachedArtifact,
        title: format!("{generic} in {}", site.owner.name),
        affordances: Vec::from([
            Aff::Inspect,
            Aff::ShowDispatch,
            Aff::ShowDependencies,
            Aff::DumpLowered,
            Aff::DumpTape,
            Aff::DumpEop,
            Aff::OpenSource,
        ]),
        execution: Some(format!(
            "dispatcher={} runtime-needed={} selected={selected}",
            site.lowered_dispatcher, site.runtime_needed
        )),
        inspection: format!(
            "key={} owner={} {} span={}..{} generic={} contract={} static-arg={} exact-class={} applicable={} rejected={} artifacts={artifact_keys}",
            site.key,
            site.owner.kind.as_str(),
            site.owner.name,
            site.span.start,
            site.span.end,
            generic,
            generic_contract_label(module, site.generic),
            static_kind,
            exact_class,
            site.applicable.len(),
            site.rejected.len()
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

    let superclass = sem_superclass_label(module, class.superclass);
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
            "class-id=c{index} storage-kind={} superclass={superclass} slots={}",
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
        let source = module
            .classes
            .get(index)
            .and_then(|sem_class| {
                sem_class
                    .effective_slots
                    .get(slot_index)
                    .map(|effective| sem_effective_slot_source_label(sem_class, effective))
            })
            .unwrap_or_else(|| "source=missing".into());
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
                "class-id=c{index} slot-id=s{slot_index} storage={} kind={} {source} runtime={}",
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
        sem::ClassOrigin::BuiltinValue { .. } => EnvironmentOrigin::CompilerGenerated,
        sem::ClassOrigin::HostBacked { .. } => EnvironmentOrigin::HostBacked,
    }
}

fn eop_class_storage(origin: sem::ClassOrigin) -> EnvironmentStorage {
    match origin {
        sem::ClassOrigin::Native => EnvironmentStorage::SourceDefined,
        sem::ClassOrigin::BuiltinValue { .. } => EnvironmentStorage::CachedArtifact,
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

fn value_class_label(module: &sem::Module, label: &str, class: sem::ClassId) -> String {
    module.classes.get(class.0 as usize).map_or_else(
        || format!("{label}(class#{})", class.0),
        |class| format!("{label}({})", class.name),
    )
}

fn value_class_fingerprint(module: &sem::Module, label: &str, class: sem::ClassId) -> String {
    module.classes.get(class.0 as usize).map_or_else(
        || format!("{label}#{}", class.0),
        |class| format!("{label}#{}", class.definition.raw()),
    )
}

fn sem_kind_label(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::Any => "object-top".into(),
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::String(class) => value_class_label(module, "string", class),
        sem::ValueKind::Symbol(class) => value_class_label(module, "symbol", class),
        sem::ValueKind::Vector(class) => value_class_label(module, "vector", class),
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
        sem::ValueKind::String(class) => format!("string(c{})", class.0),
        sem::ValueKind::Symbol(class) => format!("symbol(c{})", class.0),
        sem::ValueKind::Vector(class) => format!("vector(c{})", class.0),
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
        sem::ValueKind::String(class) => value_class_fingerprint(module, "string", class),
        sem::ValueKind::Symbol(class) => value_class_fingerprint(module, "symbol", class),
        sem::ValueKind::Vector(class) => value_class_fingerprint(module, "vector", class),
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
        sem::ValueKind::String(_)
        | sem::ValueKind::Symbol(_)
        | sem::ValueKind::Vector(_)
        | sem::ValueKind::Object(_) => "object-handle",
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
        sem::Expr::StringLiteral { value, class, .. } => {
            let _ = writeln!(out, "literal.string {:?} class=c{}", value, class.0);
        }
        sem::Expr::SymbolLiteral { name, class, .. } => {
            let _ = writeln!(out, "literal.symbol #{name} class=c{}", class.0);
        }
        sem::Expr::VectorLiteral {
            elements, class, ..
        } => {
            let _ = writeln!(
                out,
                "literal.vector class=c{} elements={}",
                class.0,
                elements.len()
            );
            for element in elements {
                dump_sem_expr(out, element, indent + 2);
            }
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
            init_args,
            fields,
            ..
        } => {
            let _ = writeln!(out, "make c{}", class_id.0);
            indent_line(out, indent + 2);
            let _ = writeln!(out, "class");
            dump_sem_expr(out, class, indent + 4);
            if !init_args.is_empty() {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "init-args");
                for arg in init_args {
                    indent_line(out, indent + 4);
                    let _ = writeln!(out, "{}: -> s{}", arg.keyword, arg.slot.0);
                    dump_sem_expr(out, &arg.value, indent + 6);
                }
            }
            for field in fields {
                indent_line(out, indent + 2);
                let _ = writeln!(
                    out,
                    "field s{} source={}",
                    field.slot.0,
                    sem_field_init_source_label(field.source)
                );
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
        sem::Expr::ValueEqual { lhs, rhs, kind, .. } => {
            let _ = writeln!(out, "value-eq {}", sem_kind_label_for_expr(*kind));
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
        sem::ClassOrigin::BuiltinValue { kind } => format!("builtin-value({})", kind.name()),
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

fn sem_superclass_chain_label(module: &sem::Module, class: sem::ClassId) -> String {
    let mut parts = Vec::new();
    for class_id in module.superclass_chain(class) {
        parts.push(module.classes.get(class_id.0 as usize).map_or_else(
            || format!("c{}:<missing>", class_id.0),
            |class| format!("c{}:{}", class.id.0, class.name),
        ));
    }
    parts.push("<object>".into());
    parts.join(" -> ")
}

fn sem_effective_slot_source_label(class: &sem::Class, slot: &sem::EffectiveSlot) -> String {
    let source = if slot.source_class == class.id {
        "direct"
    } else {
        "inherited"
    };
    format!(
        "source={source} source-slot=c{}.s{}",
        slot.source_class.0, slot.source_slot.0
    )
}

fn sem_initialization_plan_summary(module: &sem::Module, class: &sem::Class) -> String {
    let steps = class
        .initialization_plan
        .steps
        .iter()
        .map(|step| sem_initialization_step_summary(module, step))
        .collect::<Vec<_>>();
    format!("[{}]", steps.join(","))
}

fn sem_initialization_keywords(class: &sem::Class) -> String {
    let keywords = class
        .initialization_plan
        .steps
        .iter()
        .map(|step| format!("{}:", step.init_keyword))
        .collect::<Vec<_>>();
    format!("[{}]", keywords.join(","))
}

fn sem_initialization_step_summary(module: &sem::Module, step: &sem::InitializationStep) -> String {
    format!(
        "s{}:{}:{}:{}:default={}:{}",
        step.slot.0,
        step.init_keyword,
        sem_initialization_source_label(step),
        sem_initialization_required_label(step),
        yes_no(step.has_default),
        sem_kind_label(module, step.kind)
    )
}

fn sem_initialization_step_detail(
    module: &sem::Module,
    class: &sem::Class,
    step: &sem::InitializationStep,
) -> String {
    let slot_name = class
        .effective_slots
        .get(step.slot.0 as usize)
        .map_or("<missing>", |slot| slot.name.as_str());
    format!(
        "slot={slot_name} keyword={}: source={} c{}.s{} kind={} type={} required={} default={} override=explicit-init-arg",
        step.init_keyword,
        sem_initialization_source_label(step),
        step.source_class.0,
        step.source_slot.0,
        sem_kind_label(module, step.kind),
        step.type_name,
        yes_no(step.required_init_keyword),
        yes_no(step.has_default)
    )
}

fn sem_initialization_source_label(step: &sem::InitializationStep) -> &'static str {
    if step.inherited {
        "inherited"
    } else {
        "direct"
    }
}

fn sem_initialization_required_label(step: &sem::InitializationStep) -> &'static str {
    if step.required_init_keyword {
        "required"
    } else {
        "optional"
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn sem_dispatch_applicability_label(module: &sem::Module, specializer: sem::ClassId) -> String {
    let applicable = module
        .classes
        .iter()
        .filter_map(|class| {
            module
                .inheritance_distance(class.id, specializer)
                .map(|distance| format!("c{}:{}@{distance}", class.id.0, class.name))
        })
        .collect::<Vec<_>>();
    format!("[{}]", applicable.join(","))
}

fn dispatch_methods(module: &sem::Module, generic: sem::GenericFunctionId) -> Vec<&sem::Method> {
    let mut methods = module
        .methods
        .iter()
        .filter(|method| method.generic == generic)
        .collect::<Vec<_>>();
    methods.sort_by(|left, right| {
        module
            .class_depth(right.specializer)
            .cmp(&module.class_depth(left.specializer))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
    methods
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
        sem::Expr::StringLiteral { value, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "string({definition}:{value:?})");
        }
        sem::Expr::SymbolLiteral { name, class, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "symbol({definition}:#{name})");
        }
        sem::Expr::VectorLiteral {
            elements, class, ..
        } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "vector({definition}:");
            fingerprint_args(module, elements, out);
            out.push(')');
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
            init_args,
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
            for arg in init_args {
                let _ = write!(out, "arg{}:slot{}=", arg.keyword, arg.slot.0);
                fingerprint_sem_expr(module, &arg.value, out);
                out.push(',');
            }
            out.push(':');
            for field in fields {
                let kind = module
                    .classes
                    .get(class_id.0 as usize)
                    .and_then(|class| class.effective_slots.get(field.slot.0 as usize))
                    .map_or(String::from("error"), |slot| {
                        sem_kind_fingerprint(module, slot.kind)
                    });
                let _ = write!(
                    out,
                    "slot{}:{}:{kind}=",
                    field.slot.0,
                    sem_field_init_source_label(field.source)
                );
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
        sem::Expr::ValueEqual { lhs, rhs, kind, .. } => {
            let kind = sem_kind_fingerprint(module, *kind);
            let _ = write!(out, "value-eq({kind}:");
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

fn sem_field_init_source_label(source: sem::FieldInitSource) -> &'static str {
    match source {
        sem::FieldInitSource::Explicit => "explicit",
        sem::FieldInitSource::Default => "default",
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

    fn class_id(artifacts: &SessionArtifacts, name: &str) -> sem::ClassId {
        artifacts
            .sem
            .classes
            .iter()
            .find(|class| class.name == name)
            .map(|class| class.id)
            .expect("class should exist")
    }

    fn method_for_class(artifacts: &SessionArtifacts, name: &str) -> sem::MethodId {
        let class = class_id(artifacts, name);
        artifacts
            .sem
            .methods
            .iter()
            .find(|method| method.specializer == class)
            .map(|method| method.id)
            .expect("method should exist")
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
        assert!(mop.contains("initialization-plan:"));
        assert!(mop.contains("initialize-methods=deferred"));
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
    fn session_runs_named_zero_arg_functions() {
        let artifacts = CompilerSession::compile_source(
            "scratch.cairn",
            "define function helper(x)\n  x + 1\nend;\n\
             define function __cairn_repl_expr_0()\n  helper(41)\nend;\n\
             define function object_result()\n  make(<box>, value: 1)\nend;\n\
             define class <box> (<object>)\n  slot value :: <integer>;\nend;",
            RuntimeMode::CompileOnly,
        );

        assert_eq!(artifacts.diagnostics, Vec::new());
        assert_eq!(
            artifacts.run_zero_arg_function_i64("__cairn_repl_expr_0"),
            FunctionRunResult::ReturnedI64(42)
        );
        assert_eq!(
            artifacts.run_zero_arg_function_i64("missing"),
            FunctionRunResult::MissingFunction {
                name: "missing".into()
            }
        );
        assert_eq!(
            artifacts.run_zero_arg_function_i64("helper"),
            FunctionRunResult::FunctionHasParameters {
                name: "helper".into()
            }
        );
        assert_eq!(
            artifacts.run_zero_arg_function_i64("object_result"),
            FunctionRunResult::FunctionReturnedNonI64 {
                name: "object_result".into()
            }
        );
    }

    #[test]
    fn dispatch_sites_explain_exact_class_selection() {
        let artifacts =
            CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::CompileOnly);
        let sites = artifacts.dispatch_sites();

        assert_eq!(sites.len(), 1);
        let site = &sites[0];
        assert_eq!(site.id, "dispatch:d0");
        assert_eq!(site.owner.name, "main");
        assert_eq!(site.owner.kind, DispatchSiteOwnerKind::Function);
        assert_eq!(
            site.exact_static_class,
            Some(class_id(&artifacts, "<circle>"))
        );
        assert!(!site.runtime_needed);
        assert_eq!(
            site.selected_method,
            Some(method_for_class(&artifacts, "<circle>"))
        );
        assert_eq!(site.applicable.len(), 1);
        assert!(site.rejected.is_empty());
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::MethodLookup(_)))
        );
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| matches!(report.artifact, QueryArtifact::GenericDispatcher(_)))
        );
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| report.artifact == QueryArtifact::LoweredModule)
        );
        assert!(
            site.related_artifacts
                .iter()
                .any(|report| report.artifact == QueryArtifact::TapeProgram)
        );

        let list = artifacts.dump_dispatch_sites();
        assert!(list.contains("dispatch:d0"));
        assert!(list.contains("static-arg=object(<circle>)"));
        let explanation = artifacts
            .explain_dispatch_site("dispatch:d0")
            .expect("dispatch site should exist");
        assert!(explanation.contains("owner: function main"));
        assert!(explanation.contains("generic: g0:area"));
        assert!(explanation.contains("static-kind: object(<circle>)"));
        assert!(explanation.contains("runtime-needed: false"));
        assert!(explanation.contains("selected"));
        assert!(explanation.contains("dispatcher: area#dispatch"));
        assert!(explanation.contains("related artifacts:"));
    }

    #[test]
    fn dispatch_sites_choose_child_before_parent() {
        let source = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <shape>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                      define function main()\n  score(make(<circle>))\nend;";
        let artifacts =
            CompilerSession::compile_source("score.cairn", source, RuntimeMode::CompileOnly);
        let site = artifacts.dispatch_site("dispatch:d0").expect("site exists");

        assert_eq!(
            site.exact_static_class,
            Some(class_id(&artifacts, "<circle>"))
        );
        assert_eq!(
            site.selected_method,
            Some(method_for_class(&artifacts, "<circle>"))
        );
        assert_eq!(site.applicable.len(), 2);
        assert!(site.applicable[0].selected);
        assert_eq!(
            site.applicable[0].specializer,
            class_id(&artifacts, "<circle>")
        );
        assert_eq!(
            site.applicable[1].specializer,
            class_id(&artifacts, "<shape>")
        );
        assert!(!site.applicable[1].runtime_only);
        let explanation = artifacts.explain_dispatch_site("dispatch:d0").unwrap();
        assert!(explanation.contains("m1:score(c1:<circle>)"));
        assert!(explanation.contains("m0:score(c0:<shape>)"));
        assert!(explanation.contains("shadowed"));
    }

    #[test]
    fn dispatch_sites_mark_parent_typed_calls_runtime_needed() {
        let source = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <shape>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                      define function call-score(s :: <shape>) => <integer>\n  score(s)\nend;";
        let artifacts =
            CompilerSession::compile_source("score.cairn", source, RuntimeMode::CompileOnly);
        let site = artifacts.dispatch_site("dispatch:d0").expect("site exists");

        assert_eq!(
            site.static_argument_kind,
            Some(sem::ValueKind::Object(class_id(&artifacts, "<shape>")))
        );
        assert_eq!(site.exact_static_class, None);
        assert!(site.runtime_needed);
        assert_eq!(site.selected_method, None);
        assert_eq!(site.applicable.len(), 2);
        assert!(
            site.applicable
                .iter()
                .all(|candidate| candidate.runtime_only)
        );
        let explanation = artifacts.explain_dispatch_site("dispatch:d0").unwrap();
        assert!(explanation.contains("runtime-needed: true"));
        assert!(explanation.contains("selected: runtime-needed"));
    }

    #[test]
    fn dispatch_sites_report_rejected_sibling_methods() {
        let source = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define class <square> (<shape>)\n  slot side :: <integer>, init-value: 3;\nend;\n\
                      define generic area(x :: <shape>) => <integer>;\n\
                      define method area(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                      define method area(s :: <square>) => <integer>\n  s.side\nend;\n\
                      define function main()\n  area(make(<circle>))\nend;";
        let artifacts =
            CompilerSession::compile_source("area.cairn", source, RuntimeMode::CompileOnly);
        let site = artifacts.dispatch_site("dispatch:d0").expect("site exists");

        assert_eq!(
            site.selected_method,
            Some(method_for_class(&artifacts, "<circle>"))
        );
        assert!(site.rejected.iter().any(|rejected| {
            rejected.specializer == class_id(&artifacts, "<square>")
                && rejected.reason.contains("disjoint")
        }));
        let explanation = artifacts.explain_dispatch_site("dispatch:d0").unwrap();
        assert!(explanation.contains("rejected methods:"));
        assert!(explanation.contains("c2:<square>"));
        assert!(explanation.contains("reason=disjoint"));
    }

    #[test]
    fn dispatch_site_ids_survive_harmless_trivia_edits() {
        let before = CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.cairn",
            "\n// same dispatch with harmless trivia\n\
             define class <circle>   (   <object>   )\n  slot radius :: <integer>;\nend;\n\
             define generic area(x);\n\
             define method area(c :: <circle>)\n  c.radius   *   c.radius\nend;\n\
             define function main()\n  area( make( <circle>, radius: 10 ) )\nend;",
            RuntimeMode::CompileOnly,
        );
        let before_sites = before.dispatch_sites();
        let after_sites = after.dispatch_sites();

        assert_eq!(before_sites.len(), 1);
        assert_eq!(after_sites.len(), 1);
        assert_eq!(before_sites[0].id, after_sites[0].id);
        assert_eq!(before_sites[0].key, after_sites[0].key);
    }

    #[test]
    fn dispatch_explanations_change_with_method_and_class_facts() {
        let before = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <object>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define function main()\n  score(make(<circle>))\nend;";
        let with_child = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                          define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                          define generic score(x :: <object>) => <integer>;\n\
                          define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                          define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                          define function main()\n  score(make(<circle>))\nend;";
        let class_changed = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                             define class <circle> (<object>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                             define generic score(x :: <object>) => <integer>;\n\
                             define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                             define method score(c :: <circle>) => <integer>\n  c.radius\nend;\n\
                             define function main()\n  score(make(<circle>))\nend;";

        let before =
            CompilerSession::compile_source("score.cairn", before, RuntimeMode::CompileOnly);
        let with_child =
            CompilerSession::compile_source("score.cairn", with_child, RuntimeMode::CompileOnly);
        let class_changed =
            CompilerSession::compile_source("score.cairn", class_changed, RuntimeMode::CompileOnly);

        let before_site = before.dispatch_site("dispatch:d0").expect("site exists");
        let with_child_site = with_child
            .dispatch_site("dispatch:d0")
            .expect("site exists");
        let class_changed_site = class_changed
            .dispatch_site("dispatch:d0")
            .expect("site exists");

        assert_eq!(
            before_site.selected_method,
            Some(method_for_class(&before, "<shape>"))
        );
        assert_eq!(
            with_child_site.selected_method,
            Some(method_for_class(&with_child, "<circle>"))
        );
        assert!(class_changed_site.rejected.iter().any(|rejected| {
            rejected.specializer == class_id(&class_changed, "<shape>")
                && rejected.reason.contains("disjoint")
        }));

        let diff = CompilerSession::diff(&before, &with_child);
        let generic = with_child.sem.generics[0].definition;
        assert!(diff.changed.iter().any(|change| {
            change.definition.kind == DefinitionKind::Method
                && change.change == DefinitionChangeKind::Added
        }));
        assert!(
            diff.affected
                .iter()
                .any(|report| report.artifact == QueryArtifact::GenericDispatcher(generic))
        );
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
        assert!(class.inspection.detail.contains("init-plan=[s0:radius"));
        assert!(
            class
                .inspection
                .detail
                .contains("accepted-keywords=[radius:]")
        );
        assert!(
            class
                .inspection
                .detail
                .contains("initialize-methods=deferred")
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

        let dispatch = eop_object(&objects, "dispatch:d0");
        assert_eq!(dispatch.kind, EnvironmentKind::DispatchSite);
        assert!(
            dispatch
                .affordances
                .contains(EnvironmentAffordance::ShowDispatch)
        );
        assert!(dispatch.inspection.detail.contains("generic=g0:area"));

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
        assert!(mop.contains("m0 specializer=c0 <rust-circle> origin=host-backed(hc0)"));
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
    fn session_projects_structured_diagnostic_records() {
        let artifacts = CompilerSession::compile_source(
            "bad.cairn",
            "define function main()\n  missing\nend;",
            RuntimeMode::CompileOnly,
        );
        assert_eq!(artifacts.diagnostics.len(), 1);
        let diagnostic = &artifacts.diagnostics[0];
        assert_eq!(diagnostic.code, DiagnosticCode::UnresolvedLocal);
        assert!(
            diagnostic
                .args
                .iter()
                .any(|arg| arg.name == "local" && arg.value.render() == "missing")
        );

        let objects = artifacts.environment_objects();
        let diagnostic_object = eop_object(&objects, "diagnostic:d0");
        assert_eq!(diagnostic_object.kind, EnvironmentKind::Diagnostic);
        assert!(
            diagnostic_object
                .presentation
                .title
                .contains("sem.unresolved-local")
        );
        assert!(
            diagnostic_object
                .inspection
                .detail
                .contains("code=sem.unresolved-local")
        );
        assert!(
            diagnostic_object
                .inspection
                .detail
                .contains("args=[local=missing]")
        );
        assert!(!diagnostic_object.inspection.detail.contains("rendered="));
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
        assert_eq!(
            before.sem.classes[0].initialization_plan,
            after.sem.classes[0].initialization_plan
        );
        assert!(
            after
                .dump_stage(DumpStage::Sem)
                .contains("field s0 source=default")
        );
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
    fn parent_method_edit_affects_subclass_dispatch_artifacts() {
        let before = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                      define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                      define generic score(x :: <shape>) => <integer>;\n\
                      define method score(s :: <shape>) => <integer>\n  s.size\nend;\n\
                      define function main()\n  score(make(<circle>))\nend;";
        let after = "define class <shape> (<object>)\n  slot size :: <integer>, init-value: 1;\nend;\n\
                     define class <circle> (<shape>)\n  slot radius :: <integer>, init-value: 2;\nend;\n\
                     define generic score(x :: <shape>) => <integer>;\n\
                     define method score(s :: <shape>) => <integer>\n  s.size + 1\nend;\n\
                     define function main()\n  score(make(<circle>))\nend;";
        let mut session = IncrementalCompilerSession::new();
        let file = session.add_source("score.cairn", before);
        let first = session
            .compile(file, RuntimeMode::CompileOnly)
            .expect("source should exist");
        let method = first.artifacts.sem.methods[0].definition;
        let generic = first.artifacts.sem.generics[0].definition;

        let update = session
            .edit_and_compile(file, after, RuntimeMode::CompileOnly)
            .expect("source should exist");

        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLookup(method)),
            Some(CacheAction::Reused)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodLowering(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::MethodTape(method)),
            Some(CacheAction::Recomputed)
        );
        assert_eq!(
            cache_action(&update, QueryArtifact::GenericDispatcher(generic)),
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
