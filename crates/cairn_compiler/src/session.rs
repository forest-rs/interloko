//! Compiler session API and inspectable stage artifacts.
//!
//! The session layer owns source-file identity, per-stage artifact snapshots,
//! stable artifact-key reporting, dumps, and edit comparisons. It explicitly
//! does not own language semantics, lowering rules, runtime object storage, or
//! `execution_tape` bytecode internals.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

use execution_tape::trace::TraceMask;
use execution_tape::value::Value;
use execution_tape::vm::{Limits, Vm};

use crate::diagnostic::Diagnostic;
use crate::identity::{ArtifactKey, DefinitionId, DefinitionKind, SourceFileId, StableIdBuilder};
use crate::incremental::{ArtifactEdge, ArtifactReport, QueryArtifact, QueryInvalidation};
use crate::runtime::CairnHost;
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
    /// Lowered executable IR dump.
    Lowered,
    /// `execution_tape` disassembly.
    Tape,
    /// All stage dumps.
    All,
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
        "dispatcher:{}:{}:{}:",
        generic.definition.raw(),
        generic.name,
        generic.arity
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
    let syntax = syntax::parse_file(file.id, &file.source);
    let ast = ast::lower(&syntax);
    let sem = sem::lower(&ast);
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
        let _ = writeln!(
            out,
            "class {} {} {}",
            class.id, class.definition, class.name
        );
        for slot in &class.slots {
            let type_name = slot.type_name.as_deref().unwrap_or("<integer>");
            let _ = writeln!(
                out,
                "  slot {} {} {} :: {}",
                slot.id, slot.definition, slot.name, type_name
            );
        }
    }
    for generic in &module.generics {
        let _ = writeln!(
            out,
            "generic {} {} {} arity={}",
            generic.id,
            generic.definition,
            generic.name,
            generic.params.len()
        );
    }
    for method in &module.methods {
        let _ = writeln!(
            out,
            "method {} {} {} :: {}",
            method.id, method.definition, method.name, method.param.class_name
        );
        dump_ast_block(&mut out, &method.body, 2);
    }
    for function in &module.functions {
        let _ = writeln!(
            out,
            "function {} {} {}({})",
            function.id,
            function.definition,
            function.name,
            function.params.join(", ")
        );
        dump_ast_block(&mut out, &function.body, 2);
    }
    out
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
            id,
            class_name,
            fields,
            ..
        } => {
            let _ = writeln!(out, "{id} make {class_name}");
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
        let _ = writeln!(
            out,
            "class c{} {} {}",
            class.id.0, class.definition, class.name
        );
        for slot in &class.slots {
            let _ = writeln!(
                out,
                "  slot s{} {} {} :: {}",
                slot.id.0,
                slot.definition,
                slot.name,
                sem_kind_name(module, slot.kind)
            );
        }
    }
    for generic in &module.generics {
        let _ = writeln!(
            out,
            "generic g{} {} {} arity={}",
            generic.id.0, generic.definition, generic.name, generic.arity
        );
    }
    for method in &module.methods {
        let _ = writeln!(
            out,
            "method m{} {} generic=g{} class=c{} result={:?}",
            method.id.0,
            method.definition,
            method.generic.0,
            method.specializer.0,
            method.result_kind
        );
        dump_sem_locals(&mut out, &method.locals, 2);
        dump_sem_block(&mut out, &method.body, 2);
    }
    for function in &module.functions {
        let _ = writeln!(
            out,
            "function f{} {} {} result={:?}",
            function.id.0, function.definition, function.name, function.result_kind
        );
        dump_sem_locals(&mut out, &function.locals, 2);
        dump_sem_block(&mut out, &function.body, 2);
    }
    out
}

fn sem_kind_name(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::I64 => "<integer>".into(),
        sem::ValueKind::Bool => "<boolean>".into(),
        sem::ValueKind::Object(class) => module
            .classes
            .get(class.0 as usize)
            .map_or_else(|| format!("class#{}", class.0), |class| class.name.clone()),
        sem::ValueKind::Error => "<error>".into(),
    }
}

fn sem_kind_fingerprint(module: &sem::Module, kind: sem::ValueKind) -> String {
    match kind {
        sem::ValueKind::I64 => "integer".into(),
        sem::ValueKind::Bool => "boolean".into(),
        sem::ValueKind::Object(class) => module.classes.get(class.0 as usize).map_or_else(
            || format!("class#{}", class.0),
            |class| format!("object#{}", class.definition.raw()),
        ),
        sem::ValueKind::Error => "error".into(),
    }
}

fn dump_sem_locals(out: &mut String, locals: &[sem::Local], indent: usize) {
    for local in locals {
        indent_line(out, indent);
        let _ = writeln!(
            out,
            "local l{} {} {} {:?}",
            local.id.0, local.definition, local.name, local.kind
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
        sem::Expr::Local { local, .. } => {
            let _ = writeln!(out, "local l{}", local.0);
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
        sem::Expr::MakeObject { class, fields, .. } => {
            let _ = writeln!(out, "make c{}", class.0);
            for field in fields {
                indent_line(out, indent + 2);
                let _ = writeln!(out, "field s{}", field.slot.0);
                dump_sem_expr(out, &field.value, indent + 4);
            }
        }
        sem::Expr::SlotRead {
            receiver,
            class,
            slot,
            result_kind,
            ..
        } => {
            let _ = writeln!(
                out,
                "slot-read c{}.s{} result={result_kind:?}",
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
            then_branch,
            else_branch,
            ..
        } => {
            let _ = writeln!(out, "if");
            indent_line(out, indent + 2);
            let _ = writeln!(out, "condition");
            dump_sem_expr(out, condition, indent + 4);
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
                "class:{}:{}",
                class.name,
                class
                    .slots
                    .iter()
                    .map(|slot| format!(
                        "{}={}",
                        slot.name,
                        sem_kind_fingerprint(module, slot.kind)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        });
        for slot in &class.slots {
            definitions.push(DefinitionFingerprint {
                report: DefinitionReport {
                    id: slot.definition,
                    kind: DefinitionKind::Slot,
                    name: format!("{}.{}", class.name, slot.name),
                },
                fingerprint: format!(
                    "slot:{}:{}:{}",
                    class.name,
                    slot.name,
                    sem_kind_fingerprint(module, slot.kind)
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
            fingerprint: format!("generic:{}:{}", generic.name, generic.arity),
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
                "method:{generic}:{specializer}:{}",
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
                "function:{}:{}:{}",
                function.name,
                function.params.len(),
                fingerprint_sem_block(module, &function.body)
            ),
        });
    }
    definitions
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
        sem::Expr::Local { local, .. } => {
            let _ = write!(out, "local({})", local.0);
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
        sem::Expr::MakeObject { class, fields, .. } => {
            let definition = module
                .classes
                .get(class.0 as usize)
                .map_or(0, |class| class.definition.raw());
            let _ = write!(out, "make({definition}:");
            for field in fields {
                let kind = module
                    .classes
                    .get(class.0 as usize)
                    .and_then(|class| class.slots.get(field.slot.0 as usize))
                    .map_or(String::from("error"), |slot| {
                        sem_kind_fingerprint(module, slot.kind)
                    });
                let _ = write!(out, "slot{}:{kind}=", field.slot.0);
                fingerprint_sem_expr(module, &field.value, out);
                out.push(',');
            }
            out.push(')');
        }
        sem::Expr::SlotRead {
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
                .and_then(|class| class.slots.get(slot.0 as usize))
                .map_or(0, |slot| slot.definition.raw());
            let result = sem_kind_fingerprint(module, *result_kind);
            let _ = write!(out, "slot({class_definition}.{slot_definition}:{result}:");
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
            then_branch,
            else_branch,
            ..
        } => {
            out.push_str("if(");
            fingerprint_sem_expr(module, condition, out);
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

    const AREA: &str = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                        define generic area(x);\n\
                        define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                        define function main()\n  area(make(<circle>, radius: 10))\nend;";

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
                .dump_stage(DumpStage::Lowered)
                .contains("area#dispatch")
        );
        assert!(
            artifacts
                .dump_stage(DumpStage::Tape)
                .contains("program=\"cairn\"")
        );
        assert!(artifacts.dump_artifact_graph().contains("dependencies:"));
    }

    #[test]
    fn session_diff_preserves_ids_for_harmless_edit() {
        let before = CompilerSession::compile_source("area.cairn", AREA, RuntimeMode::CompileOnly);
        let after = CompilerSession::compile_source(
            "area.cairn",
            "\n// same program with trivia edits\n\
             define class <circle> (<object>)\n  slot radius;\nend;\n\
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
            "define class <circle> (<object>)\n  slot radius;\nend;\n\
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
                 define class <circle> (<object>)\n  slot radius;\nend;\n\
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
        let before = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                      define class <square> (<object>)\n  slot side;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                     define class <square> (<object>)\n  slot side;\nend;\n\
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
        let before = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                      define class <square> (<object>)\n  slot side;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  if c.radius > 0 then c.radius * c.radius else 0 end\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                     define class <square> (<object>)\n  slot side;\nend;\n\
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
        let before = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                      define class <square> (<object>)\n  slot side;\nend;\n\
                      define class <invalid-radius> (<object>)\n  slot fallback;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  handle if c.radius < 0 then signal(make(<invalid-radius>, fallback: 0)) else c.radius * c.radius end with restart e :: <invalid-radius> e.fallback end\nend;\n\
                      define method area(s :: <square>)\n  s.side * s.side\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 0 - 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                     define class <square> (<object>)\n  slot side;\nend;\n\
                     define class <invalid-radius> (<object>)\n  slot fallback;\nend;\n\
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
    fn incremental_definition_edit_marks_broad_dependents_affected() {
        let before = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                      define generic area(x);\n\
                      define method area(c :: <circle>)\n  c.radius * c.radius\nend;\n\
                      define function main()\n  area(make(<circle>, radius: 10))\nend;";
        let after = "define class <circle> (<object>)\n  slot radius;\nend;\n\
                     define class <square> (<object>)\n  slot side;\nend;\n\
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
