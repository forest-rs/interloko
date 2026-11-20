//! Retained compiler sessions for tools and embedders.
//!
//! A session owns its accepted source and compiler artifacts. Source replacement
//! compiles a candidate before publication. Failed edits preserve the accepted
//! revision, code and mutation history. Revisions qualify edits within this
//! session; EOP selectors must be rediscovered after a successful edit.
//! Transport, filesystem access, scheduling and host effects belong to callers.

#![no_std]
#![doc = include_str!("../README.md")]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use interloko_compiler::annotation::AnnotationTarget;
use interloko_compiler::claims::{ClaimDetail, ClaimReport};
use interloko_compiler::diagnostic::Diagnostic;
use interloko_compiler::lower::CodegenPolicy;
use interloko_compiler::sem::HostMetaRegistry;
use interloko_compiler::session::{
    AnnotationOutcome, CompilerSession, DumpStage, EnvironmentObject, ExecutionTraceDetail,
    FunctionCallError, FunctionCallOutcome, FunctionValue, HostFunctionHandler, MutationError,
    MutationOutcome, RewindError, RewindOutcome, RunLimits, RuntimeMode, SessionArtifacts,
    SessionMutation,
};
use interloko_compiler::typing::TypingConfig;

/// Failure to create or change an accepted session.
#[derive(Debug)]
pub enum SessionError {
    /// The caller's revision no longer names the accepted source.
    StaleRevision {
        /// Revision supplied by the caller.
        expected: u64,
        /// Currently accepted revision.
        actual: u64,
    },
    /// Candidate source was rejected; diagnostics use candidate-source spans.
    Compilation {
        /// Structured source diagnostics, without localized prose.
        diagnostics: Vec<Diagnostic>,
        /// Whether lowering or tape emission failed on accepted source; the
        /// diagnostics then carry the internal-compiler-error report.
        emission_failed: bool,
    },
    /// The compiler rejected a domain mutation.
    Mutation(MutationError),
    /// The compiler rejected a rewind.
    Rewind(RewindError),
    /// No further revision can be allocated.
    RevisionExhausted,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision { expected, actual } => {
                write!(f, "stale revision {expected}; current revision is {actual}")
            }
            Self::Compilation {
                diagnostics,
                emission_failed,
            } => write!(
                f,
                "compilation rejected ({} diagnostics, emission_failed={emission_failed})",
                diagnostics.len()
            ),
            Self::Mutation(error) => write!(f, "mutation rejected: {error:?}"),
            Self::Rewind(error) => write!(f, "rewind rejected: {error:?}"),
            Self::RevisionExhausted => f.write_str("session revision exhausted"),
        }
    }
}

impl core::error::Error for SessionError {}

/// One retained, accepted compilation and its explicit host declarations.
#[derive(Debug)]
pub struct Session {
    revision: u64,
    artifacts: SessionArtifacts,
    host_registry: HostMetaRegistry,
}

impl Session {
    /// Compiles initial source with no embedding-declared host functions.
    pub fn new(name: &str, source: &str) -> Result<Self, SessionError> {
        Self::with_host_registry(name, source, HostMetaRegistry::new())
    }

    /// Compiles initial source with explicit embedding-owned declarations.
    pub fn with_host_registry(
        name: &str,
        source: &str,
        host_registry: HostMetaRegistry,
    ) -> Result<Self, SessionError> {
        Self::with_codegen_policy(name, source, host_registry, CodegenPolicy::default())
    }

    /// Compiles initial source with explicit declarations under an explicit
    /// codegen policy, which later source replacements keep.
    ///
    /// The other constructors use [`CodegenPolicy::default`].
    /// [`CodegenPolicy::Off`] keeps every runtime check, which helps when
    /// debugging: under [`CodegenPolicy::Proved`] a numeric send typing
    /// resolved runs inline, so it makes no dispatch, and a trap inside it is
    /// reported at the call.
    pub fn with_codegen_policy(
        name: &str,
        source: &str,
        host_registry: HostMetaRegistry,
        codegen_policy: CodegenPolicy,
    ) -> Result<Self, SessionError> {
        let artifacts = compile(
            name,
            source,
            &host_registry,
            codegen_policy,
            TypingConfig::default(),
        )?;
        Ok(Self {
            revision: 0,
            artifacts,
            host_registry,
        })
    }

    /// Accepted source revision, starting at zero.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Codegen policy the accepted code was lowered under.
    #[must_use]
    pub const fn codegen_policy(&self) -> CodegenPolicy {
        self.artifacts.codegen_policy()
    }

    /// Lowers the accepted revision again under `policy` and publishes it,
    /// advancing the revision.
    ///
    /// Source, typing and the mutation log are kept; only the generated code
    /// changes, and results and runtime outcome codes do not. Setting the
    /// policy the code already has changes nothing and keeps the revision.
    pub fn set_codegen_policy(
        &mut self,
        expected: u64,
        policy: CodegenPolicy,
    ) -> Result<u64, SessionError> {
        let next = self.next_revision(expected)?;
        if policy == self.codegen_policy() {
            return Ok(self.revision);
        }
        let candidate = accepted(self.artifacts.with_codegen_policy(policy))?;
        self.artifacts = candidate;
        self.revision = next;
        Ok(next)
    }

    /// Typing configuration the accepted revision's diagnostics were
    /// reported under.
    #[must_use]
    pub const fn typing_config(&self) -> TypingConfig {
        self.artifacts.typing.config
    }

    /// Reports typing outcomes under `config` from now on, re-deriving the
    /// accepted revision's typing diagnostics, and advances the revision.
    ///
    /// Like the codegen policy, the configuration is a session input rather
    /// than a source edit: source, code and the mutation log are kept, source
    /// replacement and mutations keep the configuration, and rewind does not
    /// undo it. Verdicts and generated code do not change, only the severity
    /// each verdict is reported at; the candidate must still be accepted, so
    /// a configuration that turns a decision into a diagnostic is rejected.
    /// Setting the configuration already in use changes nothing and keeps
    /// the revision.
    pub fn set_typing_config(
        &mut self,
        expected: u64,
        config: TypingConfig,
    ) -> Result<u64, SessionError> {
        let next = self.next_revision(expected)?;
        if config == self.typing_config() {
            return Ok(self.revision);
        }
        let candidate = accepted(self.artifacts.with_typing_config(config))?;
        self.artifacts = candidate;
        self.revision = next;
        Ok(next)
    }

    /// Reports what annotating `target` with `type_source` would change in
    /// the accepted revision, without changing it: the dry run of
    /// [`SessionMutation::Annotate`].
    pub fn preview_annotation(
        &self,
        target: &AnnotationTarget,
        type_source: &str,
    ) -> Result<AnnotationOutcome, SessionError> {
        self.artifacts
            .preview_annotation(target, type_source)
            .map_err(SessionError::Mutation)
    }

    /// Returns the claim report of the function, method, or constant `id`
    /// names (a `claims:`, `function:`, `method:`, or `constant:` object id,
    /// or a `def…` id) in the accepted revision, stamped with that revision.
    ///
    /// The report says what typing established, what the generated code
    /// relies on, which checks stay at run time and why, and what contracts
    /// and closed worlds support it; see [`interloko_compiler::claims`]. It
    /// is stale once [`ClaimReport::revision`] is not [`Self::revision`].
    /// Returns `None` when `id` names no such definition.
    #[must_use]
    pub fn claims(&self, id: &str, detail: ClaimDetail) -> Option<ClaimReport> {
        self.artifacts
            .claim_report(id, detail)
            .map(|report| report.at_revision(self.revision))
    }

    /// Whether `report` describes the accepted revision.
    #[must_use]
    pub fn is_current(&self, report: &ClaimReport) -> bool {
        report.revision == Some(self.revision) && report.basis == self.artifacts.claims_basis()
    }

    /// Accepted compiler evidence, including source, grammar and artifact facts.
    #[must_use]
    pub const fn artifacts(&self) -> &SessionArtifacts {
        &self.artifacts
    }

    /// Discovers the current revision's environment objects and affordances.
    #[must_use]
    pub fn objects(&self) -> Vec<EnvironmentObject> {
        self.artifacts.environment_objects()
    }

    /// Inspects an EOP selector within the current accepted revision.
    #[must_use]
    pub fn inspect(&self, object_id: &str) -> Option<EnvironmentObject> {
        self.objects()
            .into_iter()
            .find(|object| object.id.as_str() == object_id)
    }

    /// Renders a compiler stage for inspection; structured artifacts remain available.
    #[must_use]
    pub fn dump(&self, stage: DumpStage) -> String {
        self.artifacts.dump_stage(stage)
    }

    /// Compiles and publishes replacement source after checking the expected revision.
    ///
    /// Replacement starts a new source base and clears the previous base's mutation
    /// log. Callers wanting to retain mutations should use the compiler's explicit
    /// replay facilities before choosing which source to publish. The candidate is
    /// compiled under the session's current codegen policy and typing
    /// configuration, and runs under its limits.
    pub fn replace_source(&mut self, expected: u64, source: &str) -> Result<u64, SessionError> {
        let next = self.next_revision(expected)?;
        let mut candidate = compile(
            &self.artifacts.source.name,
            source,
            &self.host_registry,
            self.codegen_policy(),
            self.typing_config(),
        )?;
        candidate.set_run_limits(self.run_limits());
        self.artifacts = candidate;
        self.revision = next;
        Ok(next)
    }

    /// Applies a domain mutation atomically and advances the accepted revision.
    ///
    /// [`SessionMutation::Annotate`] is the typing adjustment: its outcome's
    /// [`MutationOutcome::annotation`] reports which typing decisions the
    /// annotation changed.
    pub fn mutate(
        &mut self,
        expected: u64,
        mutation: SessionMutation,
    ) -> Result<MutationOutcome, SessionError> {
        let next = self.next_revision(expected)?;
        let outcome = self
            .artifacts
            .apply_mutation(mutation)
            .map_err(SessionError::Mutation)?;
        self.revision = next;
        Ok(outcome)
    }

    /// Rewinds the compiler mutation log, advancing the revision on success.
    /// This does not rewind external host effects or runtime heap state.
    pub fn rewind(
        &mut self,
        expected: u64,
        target: Option<u32>,
    ) -> Result<RewindOutcome, SessionError> {
        let next = self.next_revision(expected)?;
        let outcome = self
            .artifacts
            .rewind_to(target)
            .map_err(SessionError::Rewind)?;
        self.revision = next;
        Ok(outcome)
    }

    /// Limits each call of the accepted code runs under.
    #[must_use]
    pub const fn run_limits(&self) -> RunLimits {
        self.artifacts.run_limits()
    }

    /// Runs later calls under `limits`: fuel, which host calls are charged
    /// against too, and call depth. A runtime setting rather than a source
    /// edit, it keeps the revision, and source replacement, mutations and
    /// rewind keep it.
    pub fn set_run_limits(&mut self, limits: RunLimits) {
        self.artifacts.set_run_limits(limits);
    }

    /// Calls accepted code with owned scalar arguments and optional tracing.
    pub fn call(
        &self,
        function: &str,
        arguments: &[FunctionValue],
        trace: Option<ExecutionTraceDetail>,
    ) -> Result<FunctionCallOutcome, FunctionCallError> {
        self.artifacts.call_function(function, arguments, trace)
    }

    /// Calls accepted code with explicit embedding-owned host behavior.
    pub fn call_with_host(
        &self,
        function: &str,
        arguments: &[FunctionValue],
        trace: Option<ExecutionTraceDetail>,
        handler: &mut dyn HostFunctionHandler,
    ) -> Result<FunctionCallOutcome, FunctionCallError> {
        self.artifacts
            .call_function_with_host(function, arguments, trace, handler)
    }

    fn next_revision(&self, expected: u64) -> Result<u64, SessionError> {
        if expected != self.revision {
            return Err(SessionError::StaleRevision {
                expected,
                actual: self.revision,
            });
        }
        self.revision
            .checked_add(1)
            .ok_or(SessionError::RevisionExhausted)
    }
}

fn compile(
    name: &str,
    source: &str,
    host: &HostMetaRegistry,
    codegen_policy: CodegenPolicy,
    typing_config: TypingConfig,
) -> Result<SessionArtifacts, SessionError> {
    let mut compiler = CompilerSession::new();
    compiler.set_codegen_policy(codegen_policy);
    compiler.set_typing_config(typing_config);
    let file = compiler.add_source(name, source);
    let artifacts = compiler
        .compile_with_host_registry(file, RuntimeMode::CompileOnly, host)
        .expect("source was just added");
    accepted(artifacts)
}

/// Accepts artifacts with no diagnostics and an emitted tape.
fn accepted(artifacts: SessionArtifacts) -> Result<SessionArtifacts, SessionError> {
    if !artifacts.diagnostics.is_empty() || artifacts.tape.is_none() {
        return Err(SessionError::Compilation {
            emission_failed: artifacts.emission_failed(),
            diagnostics: artifacts.diagnostics,
        });
    }
    Ok(artifacts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_edits_preserve_code_and_revision_and_stale_edits_fail() {
        let mut session = Session::new("test.loko", "define function answer() 42 end;").unwrap();
        assert!(
            session
                .replace_source(0, "define function answer() missing end;")
                .is_err()
        );
        assert_eq!(session.revision(), 0);
        assert_eq!(
            session.call("answer", &[], None).unwrap().value,
            FunctionValue::Integer(42)
        );
        session
            .replace_source(0, "define function answer() 43 end;")
            .unwrap();
        assert!(matches!(
            session.replace_source(0, ""),
            Err(SessionError::StaleRevision { actual: 1, .. })
        ));
        assert_eq!(
            session.call("answer", &[], None).unwrap().value,
            FunctionValue::Integer(43)
        );
        let object = session
            .objects()
            .into_iter()
            .find(|object| object.id.as_str() == "function:f0")
            .unwrap();
        assert_eq!(session.inspect(object.id.as_str()), Some(object));
    }
}
