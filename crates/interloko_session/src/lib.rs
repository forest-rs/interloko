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

use execution_tape::trace::TraceMask;
use interloko_compiler::diagnostic::Diagnostic;
use interloko_compiler::sem::HostMetaRegistry;
use interloko_compiler::session::{
    CompilerSession, DumpStage, EnvironmentObject, FunctionCallError, FunctionCallOutcome,
    FunctionValue, HostFunctionHandler, MutationError, MutationOutcome, RewindError, RewindOutcome,
    RuntimeMode, SessionArtifacts, SessionMutation,
};

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
        /// Whether failure happened after source diagnostics, during emission.
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
        let artifacts = compile(name, source, &host_registry)?;
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
    /// replay facilities before choosing which source to publish.
    pub fn replace_source(&mut self, expected: u64, source: &str) -> Result<u64, SessionError> {
        let next = self.next_revision(expected)?;
        let candidate = compile(&self.artifacts.source.name, source, &self.host_registry)?;
        self.artifacts = candidate;
        self.revision = next;
        Ok(next)
    }

    /// Applies a domain mutation atomically and advances the accepted revision.
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

    /// Calls accepted code with owned scalar arguments and optional tracing.
    pub fn call(
        &self,
        function: &str,
        arguments: &[FunctionValue],
        trace: TraceMask,
    ) -> Result<FunctionCallOutcome, FunctionCallError> {
        self.artifacts.call_function(function, arguments, trace)
    }

    /// Calls accepted code with explicit embedding-owned host behavior.
    pub fn call_with_host(
        &self,
        function: &str,
        arguments: &[FunctionValue],
        trace: TraceMask,
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
) -> Result<SessionArtifacts, SessionError> {
    let artifacts = CompilerSession::compile_source_with_host_registry(
        name,
        source,
        RuntimeMode::CompileOnly,
        host,
    );
    if !artifacts.diagnostics.is_empty() || artifacts.tape.is_none() {
        return Err(SessionError::Compilation {
            emission_failed: artifacts.tape.is_none() && artifacts.diagnostics.is_empty(),
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
            session.call("answer", &[], TraceMask::NONE).unwrap().value,
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
            session.call("answer", &[], TraceMask::NONE).unwrap().value,
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
