//! Durable session transcripts.
//!
//! A `SessionTranscript` captures a session's mutation history in a form
//! that survives across process boundaries: a base source file plus an
//! ordered sequence of mutation events, each carrying the original
//! `mutation_id` from when the event was applied. Restoration is
//! `compile(base) + replay(events)` through the existing strict-replay
//! machinery; the live image stays ephemeral, only the transcript is
//! durable.
//!
//! The on-disk layout is a structured directory:
//!
//! ```text
//! session.iloko/
//!   transcript.toml
//!   base.loko
//!   mutations/
//!     000000-add-method.loko
//!     000002-add-method.loko
//! ```
//!
//! The TOML manifest carries `format_version`, `source_file_name`,
//! `next_mutation_id`, and one `[[events]]` table per mutation. The
//! `mutations/<NNNNNN>-<kind>.loko` filename pattern preserves stable
//! session event ids (including gaps left by rewind) without
//! reordering. See `docs/eop_mutation.md` for the user-facing contract.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grammar_frontend::GrammarFrontendRegistry;
use crate::sem::{GenericFunctionId, HostMetaRegistry};
use crate::session::{
    AppliedMutation, ReplayOutcome, RuntimeMode, SessionArtifacts, SessionMutation, SourceFile,
    compile_file_with_registries,
};

/// Current on-disk format version. Bumped only by a slice that also
/// supplies a migration story.
pub const FORMAT_VERSION: u32 = 1;

/// In-memory transcript of one session: base source plus the ordered
/// list of mutation events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionTranscript {
    /// On-disk format version recorded in the manifest. Always
    /// [`FORMAT_VERSION`] for transcripts produced by this build.
    pub format_version: u32,
    /// Source file name recorded alongside the base source. Mirrors
    /// the originating `SourceFile::name`.
    pub source_file_name: String,
    /// Pristine base source text. The session is rebuilt by compiling
    /// this source and replaying the events.
    pub base_source: String,
    /// Original session's `next_mutation_id` at the time of save. May
    /// be strictly greater than `max(event.mutation_id) + 1` when a
    /// tail of mutations was dropped by rewind; restore preserves it
    /// exactly so the rewind high-water mark survives.
    pub next_mutation_id: u32,
    /// Events in original application order. May carry gaps in
    /// `mutation_id` values when the originating session was rewound.
    pub events: Vec<TranscriptEvent>,
}

/// One persisted mutation event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEvent {
    /// Stable session event id from the originating session. Preserved
    /// verbatim across save / open / restore.
    pub mutation_id: u32,
    /// Mutation kind. Restored as the matching `SessionMutation`
    /// variant.
    pub kind: TranscriptEventKind,
    /// Human-readable generic name resolved in the originating
    /// session's sem module. Recorded for inspection and future
    /// conflict handling; **not** used to silently rebind during
    /// restore in this slice.
    pub generic_name: String,
    /// Original `GenericFunctionId` value. Used verbatim by restore
    /// so the strict-replay semantics produce the same outcomes the
    /// originating session saw.
    pub generic_id_hint: u32,
    /// Mutation source text (a full Loko `define method ...; end;`
    /// clause for `AddMethod`).
    pub source: String,
}

/// Kind tag for a transcript event.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TranscriptEventKind {
    /// Corresponds to [`SessionMutation::AddMethod`].
    AddMethod,
}

impl TranscriptEventKind {
    /// Stable on-disk label used in manifest values and mutation
    /// filenames.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AddMethod => "add-method",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "add-method" => Some(Self::AddMethod),
            _ => None,
        }
    }
}

/// Structured failure from [`SessionTranscript::save`].
#[non_exhaustive]
#[derive(Debug)]
pub enum TranscriptSaveError {
    /// Filesystem operation failed.
    Io {
        /// Path the operation was targeting.
        path: PathBuf,
        /// Underlying I/O error.
        error: io::Error,
    },
    /// Manifest could not be serialized to TOML.
    Serialize {
        /// Underlying serializer error.
        error: toml::ser::Error,
    },
}

/// Structured failure from [`SessionTranscript::open`].
#[non_exhaustive]
#[derive(Debug)]
pub enum TranscriptLoadError {
    /// Filesystem operation failed.
    Io {
        /// Path the operation was targeting.
        path: PathBuf,
        /// Underlying I/O error.
        error: io::Error,
    },
    /// Manifest could not be parsed.
    Manifest {
        /// Path of the manifest file.
        path: PathBuf,
        /// Underlying deserializer error.
        error: Box<toml::de::Error>,
    },
    /// Manifest declared a `format_version` this build does not know
    /// how to read.
    UnsupportedFormatVersion {
        /// Version found in the manifest.
        found: u32,
        /// Highest version this build supports.
        supported: u32,
    },
    /// Manifest named an event kind this build does not recognize.
    UnknownEventKind {
        /// Event whose kind was unrecognized.
        mutation_id: u32,
        /// Raw kind value from the manifest.
        raw_kind: String,
    },
    /// Manifest event referenced a source file that does not exist.
    MissingEventSource {
        /// Event whose source file was missing.
        mutation_id: u32,
        /// Path the manifest pointed at, relative to the transcript
        /// directory.
        path: PathBuf,
    },
    /// Manifest event's `source` field is not the canonical
    /// `mutations/<NNNNNN>-<kind>.loko` path derived from its
    /// `mutation_id` and `kind`. Absolute paths, paths containing
    /// `..`, paths that escape the transcript directory, and any
    /// non-canonical filename are all rejected so a hand-edited
    /// manifest cannot trick `open` into reading outside the
    /// transcript directory.
    InvalidEventSourcePath {
        /// Event whose source path was rejected.
        mutation_id: u32,
        /// Canonical path the manifest should have used.
        expected: String,
        /// Raw `source` value the manifest declared.
        found: String,
    },
    /// Manifest event ids are not strictly increasing. Catches both
    /// duplicates and reordered ids in a single check.
    EventIdsNotStrictlyIncreasing {
        /// Previously-seen mutation id.
        previous: u32,
        /// Offending mutation id (must be > previous).
        found: u32,
    },
    /// Manifest's `next_mutation_id` is not strictly greater than the
    /// largest event id. Catches hand-edits that would roll the
    /// counter backward and silently re-enable id reuse on the next
    /// `apply_mutation` after restore.
    NextMutationIdNotAboveMaxEvent {
        /// `next_mutation_id` value from the manifest.
        next_mutation_id: u32,
        /// Largest `mutation_id` among the manifest's events.
        max_event_id: u32,
    },
}

impl SessionArtifacts {
    /// Captures this session's mutation history as a structured
    /// [`SessionTranscript`].
    ///
    /// The transcript records the pristine base source, every entry in
    /// the live mutation log (in application order, with original
    /// mutation ids preserved including any gaps from prior rewinds),
    /// and the session's `next_mutation_id` at the time of capture.
    /// No cached artifacts, runtime state, diagnostics, or lowered IR
    /// are recorded.
    #[must_use]
    pub fn to_transcript(&self) -> SessionTranscript {
        let events = self
            .mutations()
            .iter()
            .map(|entry| transcript_event_from_applied(self, entry))
            .collect();
        SessionTranscript {
            format_version: FORMAT_VERSION,
            source_file_name: self.source.name.clone(),
            base_source: self.pristine_source().to_string(),
            next_mutation_id: self.next_mutation_id(),
            events,
        }
    }
}

impl SessionTranscript {
    /// Writes the transcript to the given directory as a manifest plus
    /// base source plus one mutation source file per event.
    ///
    /// `dir` is created if it does not already exist. Existing
    /// transcript files in `dir` are overwritten. Files unrelated to
    /// the transcript layout are left untouched.
    pub fn save(&self, dir: &Path) -> Result<(), TranscriptSaveError> {
        fs::create_dir_all(dir).map_err(|error| TranscriptSaveError::Io {
            path: dir.to_path_buf(),
            error,
        })?;

        let base_path = dir.join("base.loko");
        fs::write(&base_path, &self.base_source).map_err(|error| TranscriptSaveError::Io {
            path: base_path.clone(),
            error,
        })?;

        let mutations_dir = dir.join("mutations");
        if !self.events.is_empty() {
            fs::create_dir_all(&mutations_dir).map_err(|error| TranscriptSaveError::Io {
                path: mutations_dir.clone(),
                error,
            })?;
        }

        let mut manifest_events = Vec::with_capacity(self.events.len());
        for event in &self.events {
            let filename = event_filename(event.mutation_id, event.kind);
            let event_path = mutations_dir.join(&filename);
            fs::write(&event_path, &event.source).map_err(|error| TranscriptSaveError::Io {
                path: event_path.clone(),
                error,
            })?;
            manifest_events.push(ManifestEvent {
                mutation_id: event.mutation_id,
                kind: event.kind.as_str().to_string(),
                generic_name: event.generic_name.clone(),
                generic_id_hint: event.generic_id_hint,
                source: format!("mutations/{filename}"),
            });
        }

        let manifest = Manifest {
            format_version: self.format_version,
            source_file_name: self.source_file_name.clone(),
            next_mutation_id: self.next_mutation_id,
            events: manifest_events,
        };
        let manifest_text = toml::to_string_pretty(&manifest)
            .map_err(|error| TranscriptSaveError::Serialize { error })?;
        let manifest_path = dir.join("transcript.toml");
        fs::write(&manifest_path, manifest_text).map_err(|error| TranscriptSaveError::Io {
            path: manifest_path,
            error,
        })?;
        Ok(())
    }

    /// Reads a transcript directory previously written by [`Self::save`].
    pub fn open(dir: &Path) -> Result<Self, TranscriptLoadError> {
        let manifest_path = dir.join("transcript.toml");
        let manifest_text =
            fs::read_to_string(&manifest_path).map_err(|error| TranscriptLoadError::Io {
                path: manifest_path.clone(),
                error,
            })?;
        let manifest: Manifest =
            toml::from_str(&manifest_text).map_err(|error| TranscriptLoadError::Manifest {
                path: manifest_path,
                error: Box::new(error),
            })?;

        // Strict equality: this build only reads transcripts produced
        // by the same FORMAT_VERSION. A future slice that introduces
        // migration tooling can relax this; until then, reject both
        // forward (newer) and backward (older / 0 / unknown) versions
        // so a hand-edited manifest cannot quietly bypass schema
        // checks by claiming a different version.
        if manifest.format_version != FORMAT_VERSION {
            return Err(TranscriptLoadError::UnsupportedFormatVersion {
                found: manifest.format_version,
                supported: FORMAT_VERSION,
            });
        }

        let base_path = dir.join("base.loko");
        let base_source =
            fs::read_to_string(&base_path).map_err(|error| TranscriptLoadError::Io {
                path: base_path,
                error,
            })?;

        // Two-pass open: validate every event entry (kind, strict
        // monotonicity, canonical source path) and the next-id
        // counter against the manifest first, then read all event
        // source files. The split matters because a bad-but-late
        // event would otherwise cause earlier event-file reads
        // before the failure surfaced. Reading any event file before
        // the whole manifest is known good gives a hostile manifest
        // a partial-effect window we don't want to leave open.
        let mut validated = Vec::with_capacity(manifest.events.len());
        let mut max_event_id: Option<u32> = None;
        for entry in manifest.events {
            let kind = TranscriptEventKind::from_str(&entry.kind).ok_or_else(|| {
                TranscriptLoadError::UnknownEventKind {
                    mutation_id: entry.mutation_id,
                    raw_kind: entry.kind.clone(),
                }
            })?;
            if let Some(previous) = max_event_id
                && entry.mutation_id <= previous
            {
                return Err(TranscriptLoadError::EventIdsNotStrictlyIncreasing {
                    previous,
                    found: entry.mutation_id,
                });
            }
            let expected_source = format!("mutations/{}", event_filename(entry.mutation_id, kind));
            if entry.source != expected_source {
                return Err(TranscriptLoadError::InvalidEventSourcePath {
                    mutation_id: entry.mutation_id,
                    expected: expected_source,
                    found: entry.source,
                });
            }
            max_event_id = Some(entry.mutation_id);
            validated.push(ValidatedEvent {
                mutation_id: entry.mutation_id,
                kind,
                generic_name: entry.generic_name,
                generic_id_hint: entry.generic_id_hint,
                expected_source,
            });
        }

        // The next-id counter must sit strictly above the highest
        // event id. Equality or lower would mean the next
        // apply_mutation on restored artifacts could recycle an
        // already-assigned id, breaking the stable session event id
        // invariant. Empty event lists are unconstrained: any
        // next_mutation_id (including 0) is valid. This check is
        // still part of the manifest-validation pass: if it fails,
        // no event source file has been read yet.
        if let Some(max_event_id) = max_event_id
            && manifest.next_mutation_id <= max_event_id
        {
            return Err(TranscriptLoadError::NextMutationIdNotAboveMaxEvent {
                next_mutation_id: manifest.next_mutation_id,
                max_event_id,
            });
        }

        // Pass two: now that the manifest is fully validated, read
        // each event's source file.
        let mut events = Vec::with_capacity(validated.len());
        for validated_event in validated {
            let event_path = dir.join(&validated_event.expected_source);
            let source = fs::read_to_string(&event_path).map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => TranscriptLoadError::MissingEventSource {
                    mutation_id: validated_event.mutation_id,
                    path: PathBuf::from(&validated_event.expected_source),
                },
                _ => TranscriptLoadError::Io {
                    path: event_path.clone(),
                    error,
                },
            })?;
            events.push(TranscriptEvent {
                mutation_id: validated_event.mutation_id,
                kind: validated_event.kind,
                generic_name: validated_event.generic_name,
                generic_id_hint: validated_event.generic_id_hint,
                source,
            });
        }

        Ok(Self {
            format_version: manifest.format_version,
            source_file_name: manifest.source_file_name,
            base_source,
            next_mutation_id: manifest.next_mutation_id,
            events,
        })
    }

    /// Rebuilds a live session from this transcript: compiles the base
    /// source against the supplied registries, then replays the events
    /// through the existing strict-replay machinery.
    ///
    /// The returned [`ReplayOutcome`] carries the applied events, any
    /// first failure, and the ephemeral [`SessionArtifacts`] the caller
    /// may adopt. The ephemeral session's `next_mutation_id` is set to
    /// the transcript's recorded value (preserving the rewind
    /// high-water mark) rather than the `max(applied) + 1` value that
    /// the apply loop would otherwise produce.
    ///
    /// Restore does not silently rebind by name. If the base source's
    /// sem has reordered generic ids since save, restore surfaces the
    /// standard replay failure for that case (`GenericNameMismatch` or
    /// `UnknownGeneric`) rather than guessing which generic the
    /// original mutation meant.
    ///
    /// The restored session uses the default soft typing configuration;
    /// transcripts do not record typing configuration.
    #[must_use]
    pub fn restore(
        &self,
        grammar_registry: GrammarFrontendRegistry,
        host_registry: &HostMetaRegistry,
    ) -> ReplayOutcome {
        let base = SourceFile::named(&self.source_file_name, &self.base_source);
        let mut ephemeral = compile_file_with_registries(
            &base,
            RuntimeMode::CompileOnly,
            grammar_registry,
            host_registry,
            crate::typing::TypingConfig::default(),
        );

        let mut applied: Vec<AppliedMutation> = Vec::new();
        let mut failed = None;
        for (log_index, event) in self.events.iter().enumerate() {
            let request = match event.kind {
                TranscriptEventKind::AddMethod => SessionMutation::AddMethod {
                    generic: GenericFunctionId(event.generic_id_hint),
                    method_source: event.source.clone(),
                },
            };
            match ephemeral.apply_session_mutation_with_id(request.clone(), event.mutation_id) {
                Ok(_) => {
                    if let Some(last) = ephemeral.mutations().last() {
                        applied.push(last.clone());
                    }
                }
                Err(cause) => {
                    failed = Some(crate::session::ReplayFailure {
                        mutation_id: event.mutation_id,
                        log_index,
                        request,
                        cause,
                    });
                    break;
                }
            }
        }

        // Preserve the originating session's high-water mark, not the
        // max(applied) + 1 value the apply path computed. They differ
        // when the originating session's log was truncated by a rewind:
        // dropped ids are permanently retired in the originating
        // session, and the transcript carries that through.
        ephemeral.set_next_mutation_id(self.next_mutation_id);

        ReplayOutcome {
            applied,
            failed,
            artifacts: ephemeral,
        }
    }
}

fn transcript_event_from_applied(
    artifacts: &SessionArtifacts,
    entry: &AppliedMutation,
) -> TranscriptEvent {
    match &entry.request {
        SessionMutation::AddMethod {
            generic,
            method_source,
        } => {
            let generic_name = artifacts
                .sem
                .generics
                .get(generic.0 as usize)
                .map_or_else(|| "<unresolved>".to_string(), |g| g.name.clone());
            TranscriptEvent {
                mutation_id: entry.mutation_id,
                kind: TranscriptEventKind::AddMethod,
                generic_name,
                generic_id_hint: generic.0,
                source: method_source.clone(),
            }
        }
    }
}

fn event_filename(mutation_id: u32, kind: TranscriptEventKind) -> String {
    format!("{mutation_id:06}-{}.loko", kind.as_str())
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    format_version: u32,
    source_file_name: String,
    next_mutation_id: u32,
    #[serde(default)]
    events: Vec<ManifestEvent>,
}

#[derive(Serialize, Deserialize)]
struct ManifestEvent {
    mutation_id: u32,
    kind: String,
    generic_name: String,
    generic_id_hint: u32,
    source: String,
}

/// Internal record produced by `open`'s first pass: a manifest event
/// that has passed all structural validation (known kind, strictly
/// increasing id, canonical source path). The second pass turns this
/// into a public [`TranscriptEvent`] by reading the event's source
/// file.
struct ValidatedEvent {
    mutation_id: u32,
    kind: TranscriptEventKind,
    generic_name: String,
    generic_id_hint: u32,
    expected_source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use crate::sem::{self, HostMetaRegistry};
    use crate::session::{
        CompilerSession, FunctionCallError, FunctionValue, RuntimeMode, SessionArtifacts,
        SessionMutation,
    };

    fn run_scalar(
        artifacts: &SessionArtifacts,
        name: &str,
    ) -> Result<FunctionValue, FunctionCallError> {
        artifacts
            .call_function(name, &[], None)
            .map(|outcome| outcome.value)
    }
    use tempfile::tempdir;

    const FIXTURE_SOURCE: &str = "\
define class <circle> (<object>)
  slot radius :: <integer>;
end;

define class <square> (<object>)
  slot side :: <integer>;
end;

define class <triangle> (<object>)
  slot base :: <integer>;
end;

define generic area(x);

define method area(c :: <circle>)
  c.radius * c.radius
end;

define function score_circle()
  area(make(<circle>, radius: 10))
end;

define function score_square()
  area(make(<square>, side: 7))
end;

define function score_triangle()
  area(make(<triangle>, base: 9))
end;
";

    const SQUARE_METHOD: &str = "\
define method area(s :: <square>)
  s.side * s.side
end;
";

    const TRIANGLE_METHOD: &str = "\
define method area(t :: <triangle>)
  t.base * t.base
end;
";

    fn fixture() -> SessionArtifacts {
        CompilerSession::compile_source("main.loko", FIXTURE_SOURCE, RuntimeMode::CompileOnly)
    }

    fn area_generic(artifacts: &SessionArtifacts) -> sem::GenericFunctionId {
        artifacts
            .sem
            .generics
            .iter()
            .find(|generic| generic.name == "area")
            .map(|generic| generic.id)
            .expect("fixture should declare `area`")
    }

    fn fixture_with_two_mutations() -> SessionArtifacts {
        let mut artifacts = fixture();
        let area = area_generic(&artifacts);
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: SQUARE_METHOD.into(),
            })
            .expect("square mutation should succeed");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: TRIANGLE_METHOD.into(),
            })
            .expect("triangle mutation should succeed");
        artifacts
    }

    #[test]
    fn to_transcript_then_save_and_open_round_trips_in_memory() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();

        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save should succeed");
        let reloaded = SessionTranscript::open(dir.path()).expect("open should succeed");

        assert_eq!(reloaded, transcript);
    }

    #[test]
    fn save_open_restore_reproduces_dispatch_behavior() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();

        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");
        let reloaded = SessionTranscript::open(dir.path()).expect("open");

        let outcome =
            reloaded.restore(GrammarFrontendRegistry::builtin(), &HostMetaRegistry::new());
        assert!(
            outcome.failed.is_none(),
            "restore should succeed: {:?}",
            outcome.failed
        );
        assert_eq!(outcome.applied.len(), 2);

        let restored = outcome.artifacts;
        let circle = run_scalar(&restored, "score_circle");
        assert!(matches!(circle, Ok(FunctionValue::Integer(100))));
        let square = run_scalar(&restored, "score_square");
        assert!(matches!(square, Ok(FunctionValue::Integer(49))));
        let triangle = run_scalar(&restored, "score_triangle");
        assert!(matches!(triangle, Ok(FunctionValue::Integer(81))));
    }

    #[test]
    fn save_open_restore_preserves_id_gaps_and_next_mutation_id() {
        // Apply, apply, rewind to first, apply again -> log has ids
        // [0, 2] with a permanent gap at 1; next_mutation_id is 3.
        let mut artifacts = fixture();
        let area = area_generic(&artifacts);
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: SQUARE_METHOD.into(),
            })
            .expect("first apply");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: TRIANGLE_METHOD.into(),
            })
            .expect("second apply");
        artifacts.rewind_to(Some(0)).expect("rewind to first");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: TRIANGLE_METHOD.into(),
            })
            .expect("post-rewind apply");
        assert_eq!(
            artifacts
                .mutations()
                .iter()
                .map(|entry| entry.mutation_id)
                .collect::<Vec<_>>(),
            Vec::from([0u32, 2u32])
        );
        assert_eq!(artifacts.next_mutation_id(), 3);

        let transcript = artifacts.to_transcript();
        assert_eq!(transcript.next_mutation_id, 3);
        let event_ids: Vec<u32> = transcript
            .events
            .iter()
            .map(|event| event.mutation_id)
            .collect();
        assert_eq!(event_ids, Vec::from([0u32, 2u32]));

        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");
        let reloaded = SessionTranscript::open(dir.path()).expect("open");

        let outcome =
            reloaded.restore(GrammarFrontendRegistry::builtin(), &HostMetaRegistry::new());
        assert!(outcome.failed.is_none());

        let restored = outcome.artifacts;
        let restored_ids: Vec<u32> = restored
            .mutations()
            .iter()
            .map(|entry| entry.mutation_id)
            .collect();
        assert_eq!(restored_ids, Vec::from([0u32, 2u32]));
        // The rewind high-water mark survives: max(applied)+1 == 3
        // would be the apply path's default, but the transcript's
        // explicit next_mutation_id=3 happens to coincide here. The
        // restore path sets the counter from the transcript regardless,
        // so a later transcript with a larger gap (e.g. apply 0, apply
        // 1, rewind to 0 -> next_mutation_id=2 with only id 0 in the
        // log) would still preserve the high-water mark. See the
        // dedicated test below.
        assert_eq!(restored.next_mutation_id(), 3);
    }

    #[test]
    fn save_open_restore_preserves_high_water_mark_above_max_applied_id() {
        // Apply, apply, rewind to first -> log is [0], next_mutation_id
        // is 2 (id 1 is permanently retired). On restore, max(applied)
        // + 1 would yield 1; the transcript's explicit
        // next_mutation_id=2 must override.
        let mut artifacts = fixture();
        let area = area_generic(&artifacts);
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: SQUARE_METHOD.into(),
            })
            .expect("first apply");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: TRIANGLE_METHOD.into(),
            })
            .expect("second apply");
        artifacts.rewind_to(Some(0)).expect("rewind to first");
        assert_eq!(artifacts.next_mutation_id(), 2);

        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");
        let reloaded = SessionTranscript::open(dir.path()).expect("open");

        let outcome =
            reloaded.restore(GrammarFrontendRegistry::builtin(), &HostMetaRegistry::new());
        assert!(outcome.failed.is_none());
        let restored = outcome.artifacts;
        assert_eq!(restored.next_mutation_id(), 2);
        let next = restored.clone().apply_mutation(SessionMutation::AddMethod {
            generic: area,
            method_source: TRIANGLE_METHOD.into(),
        });
        // Either the mutation succeeds with id 2 (since dispatch
        // works) or it gets rejected for some other reason; in either
        // case the id allocation must NOT recycle the retired id 1.
        match next {
            Ok(outcome) => assert_eq!(outcome.mutation_id, 2),
            Err(_) => {
                // Counter still reflects 2 even after a rejected
                // attempt (failures don't advance it).
                assert_eq!(restored.next_mutation_id(), 2);
            }
        }
    }

    #[test]
    fn restore_against_base_modified_to_remove_generic_surfaces_replay_failure() {
        // Build a transcript that records the fixture's base + two
        // mutations, then hand-edit the in-memory transcript to swap
        // base_source for a base with no `area` generic. Restore must
        // surface the replay failure rather than guessing.
        let artifacts = fixture_with_two_mutations();
        let mut transcript = artifacts.to_transcript();
        transcript.base_source = "\
define function noop()
  0
end;
"
        .into();

        let outcome =
            transcript.restore(GrammarFrontendRegistry::builtin(), &HostMetaRegistry::new());

        let failure = outcome
            .failed
            .expect("changed base should surface a replay failure");
        assert_eq!(failure.mutation_id, 0);
        assert!(outcome.applied.is_empty());
    }

    #[test]
    fn save_only_writes_transcript_layout_files() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        let mut top_level: BTreeSet<String> = BTreeSet::new();
        for entry in fs::read_dir(dir.path()).expect("read_dir") {
            let entry = entry.expect("dir entry");
            top_level.insert(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(
            top_level,
            BTreeSet::from([
                "transcript.toml".to_string(),
                "base.loko".to_string(),
                "mutations".to_string(),
            ])
        );

        let mut mutation_files: BTreeSet<String> = BTreeSet::new();
        for entry in fs::read_dir(dir.path().join("mutations")).expect("mutations dir") {
            let entry = entry.expect("mutations entry");
            mutation_files.insert(entry.file_name().to_string_lossy().into_owned());
        }
        assert_eq!(
            mutation_files,
            BTreeSet::from([
                "000000-add-method.loko".to_string(),
                "000001-add-method.loko".to_string(),
            ])
        );
    }

    #[test]
    fn open_with_malformed_manifest_returns_manifest_error() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("transcript.toml"), "this = is { not toml")
            .expect("write malformed manifest");
        fs::write(dir.path().join("base.loko"), "// placeholder\n").expect("write base");

        let err = SessionTranscript::open(dir.path())
            .expect_err("malformed manifest should surface load error");
        assert!(
            matches!(err, TranscriptLoadError::Manifest { .. }),
            "expected Manifest error, got {err:?}"
        );
    }

    #[test]
    fn open_with_missing_event_source_returns_structured_error() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        // Delete the second mutation's source file out from under the
        // manifest.
        fs::remove_file(dir.path().join("mutations/000001-add-method.loko"))
            .expect("remove event file");

        let err = SessionTranscript::open(dir.path())
            .expect_err("missing event source should surface load error");
        match err {
            TranscriptLoadError::MissingEventSource {
                mutation_id, path, ..
            } => {
                assert_eq!(mutation_id, 1);
                assert_eq!(path, PathBuf::from("mutations/000001-add-method.loko"));
            }
            other => panic!("expected MissingEventSource, got {other:?}"),
        }
    }

    #[test]
    fn open_with_unknown_event_kind_returns_structured_error() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        // Hand-edit the manifest to declare an unknown event kind.
        let manifest_path = dir.path().join("transcript.toml");
        let text = fs::read_to_string(&manifest_path).expect("read manifest");
        let edited = text.replace("kind = \"add-method\"", "kind = \"add-pentagon\"");
        fs::write(&manifest_path, edited).expect("rewrite manifest");

        let err = SessionTranscript::open(dir.path())
            .expect_err("unknown kind should surface load error");
        match err {
            TranscriptLoadError::UnknownEventKind { raw_kind, .. } => {
                assert_eq!(raw_kind, "add-pentagon");
            }
            other => panic!("expected UnknownEventKind, got {other:?}"),
        }
    }

    fn rewrite_manifest(dir: &Path, replacements: &[(&str, &str)]) {
        let manifest_path = dir.join("transcript.toml");
        let mut text = fs::read_to_string(&manifest_path).expect("read manifest");
        for (from, to) in replacements {
            assert!(
                text.contains(from),
                "manifest does not contain `{from}`:\n{text}"
            );
            text = text.replace(from, to);
        }
        fs::write(&manifest_path, text).expect("rewrite manifest");
    }

    #[test]
    fn open_rejects_event_source_with_parent_component() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        rewrite_manifest(
            dir.path(),
            &[(
                "source = \"mutations/000000-add-method.loko\"",
                "source = \"../outside.loko\"",
            )],
        );

        let err = SessionTranscript::open(dir.path())
            .expect_err("parent-component source path must be rejected");
        match err {
            TranscriptLoadError::InvalidEventSourcePath {
                mutation_id,
                expected,
                found,
            } => {
                assert_eq!(mutation_id, 0);
                assert_eq!(expected, "mutations/000000-add-method.loko");
                assert_eq!(found, "../outside.loko");
            }
            other => panic!("expected InvalidEventSourcePath, got {other:?}"),
        }
    }

    #[test]
    fn open_rejects_absolute_event_source_path() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        rewrite_manifest(
            dir.path(),
            &[(
                "source = \"mutations/000000-add-method.loko\"",
                "source = \"/etc/passwd\"",
            )],
        );

        let err =
            SessionTranscript::open(dir.path()).expect_err("absolute source path must be rejected");
        assert!(
            matches!(err, TranscriptLoadError::InvalidEventSourcePath { .. }),
            "expected InvalidEventSourcePath, got {err:?}"
        );
    }

    #[test]
    fn open_rejects_duplicate_mutation_ids() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        // Hand-edit the second event's mutation_id to match the first.
        rewrite_manifest(
            dir.path(),
            &[
                ("mutation_id = 1", "mutation_id = 0"),
                // Source path is derived from id+kind, so it now
                // points at the first event's file - still a valid
                // canonical path for the (duplicated) id, so the
                // monotonicity check fires before any source-path
                // mismatch.
                (
                    "source = \"mutations/000001-add-method.loko\"",
                    "source = \"mutations/000000-add-method.loko\"",
                ),
            ],
        );

        let err = SessionTranscript::open(dir.path())
            .expect_err("duplicate mutation_id must be rejected");
        match err {
            TranscriptLoadError::EventIdsNotStrictlyIncreasing { previous, found } => {
                assert_eq!(previous, 0);
                assert_eq!(found, 0);
            }
            other => panic!("expected EventIdsNotStrictlyIncreasing, got {other:?}"),
        }
    }

    #[test]
    fn open_rejects_out_of_order_mutation_ids() {
        // Build a transcript with events at ids 5 and 9, then
        // hand-edit so they appear in reverse order. The reordered
        // manifest violates strict monotonicity.
        let mut artifacts = fixture();
        let area = area_generic(&artifacts);
        // Bump the next_mutation_id to 5 by burning ids 0..4 (apply +
        // rewind keeps high-water mark moving). Simpler approach for
        // a test: apply once, rewind, apply (id 1), rewind, apply
        // (id 2), ... etc. is tedious. Instead, hand-edit the
        // manifest from a small fixture-derived transcript.
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: SQUARE_METHOD.into(),
            })
            .expect("first apply");
        artifacts
            .apply_mutation(SessionMutation::AddMethod {
                generic: area,
                method_source: TRIANGLE_METHOD.into(),
            })
            .expect("second apply");
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        // Swap the two events' positions in the manifest by swapping
        // their mutation_ids. The source paths must also swap to stay
        // canonical, so each event's source field still matches the
        // (new) mutation_id; the failure that fires is the strict
        // monotonicity check, not the path check.
        rewrite_manifest(
            dir.path(),
            &[
                ("mutation_id = 0", "mutation_id = 0xPLACEHOLDER"),
                ("mutation_id = 1", "mutation_id = 0"),
                ("mutation_id = 0xPLACEHOLDER", "mutation_id = 1"),
                ("000000-add-method.loko", "000000-PLACEHOLDER.loko"),
                ("000001-add-method.loko", "000000-add-method.loko"),
                ("000000-PLACEHOLDER.loko", "000001-add-method.loko"),
            ],
        );

        let err = SessionTranscript::open(dir.path())
            .expect_err("reordered mutation_ids must be rejected");
        match err {
            TranscriptLoadError::EventIdsNotStrictlyIncreasing { previous, found } => {
                assert_eq!(previous, 1);
                assert_eq!(found, 0);
            }
            other => panic!("expected EventIdsNotStrictlyIncreasing, got {other:?}"),
        }
    }

    #[test]
    fn open_rejects_next_mutation_id_not_above_max_event_id() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        // The valid transcript has next_mutation_id = 2 and events
        // [0, 1]. Hand-edit next_mutation_id down to 1 (== max event
        // id) so the next apply on restored artifacts would recycle
        // id 1. open() must reject.
        rewrite_manifest(
            dir.path(),
            &[("next_mutation_id = 2", "next_mutation_id = 1")],
        );

        let err = SessionTranscript::open(dir.path())
            .expect_err("next_mutation_id <= max(event_id) must be rejected");
        match err {
            TranscriptLoadError::NextMutationIdNotAboveMaxEvent {
                next_mutation_id,
                max_event_id,
            } => {
                assert_eq!(next_mutation_id, 1);
                assert_eq!(max_event_id, 1);
            }
            other => panic!("expected NextMutationIdNotAboveMaxEvent, got {other:?}"),
        }
    }

    #[test]
    fn open_rejects_format_version_zero() {
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        rewrite_manifest(dir.path(), &[("format_version = 1", "format_version = 0")]);

        let err =
            SessionTranscript::open(dir.path()).expect_err("format_version = 0 must be rejected");
        match err {
            TranscriptLoadError::UnsupportedFormatVersion { found, supported } => {
                assert_eq!(found, 0);
                assert_eq!(supported, FORMAT_VERSION);
            }
            other => panic!("expected UnsupportedFormatVersion, got {other:?}"),
        }
    }

    #[test]
    fn open_validates_manifest_before_reading_any_event_source_file() {
        // Two-pass discipline check: a structural error on a later
        // event must surface without triggering a file-read attempt
        // on an earlier event. Set up a valid transcript, then delete
        // every event source file AND corrupt the last event's path
        // so structural validation fails. A one-pass implementation
        // that reads each event file immediately after validating it
        // would surface MissingEventSource for event 0; the two-pass
        // implementation surfaces InvalidEventSourcePath for event 1
        // because no file reads happen until the whole manifest
        // checks out.
        let artifacts = fixture_with_two_mutations();
        let transcript = artifacts.to_transcript();
        let dir = tempdir().expect("tempdir");
        transcript.save(dir.path()).expect("save");

        fs::remove_file(dir.path().join("mutations/000000-add-method.loko"))
            .expect("remove first event file");
        fs::remove_file(dir.path().join("mutations/000001-add-method.loko"))
            .expect("remove second event file");

        rewrite_manifest(
            dir.path(),
            &[(
                "source = \"mutations/000001-add-method.loko\"",
                "source = \"../escape.loko\"",
            )],
        );

        let err = SessionTranscript::open(dir.path())
            .expect_err("structural error on late event should surface");
        match err {
            TranscriptLoadError::InvalidEventSourcePath { mutation_id, .. } => {
                assert_eq!(
                    mutation_id, 1,
                    "the structural failure must be reported for the late event, \
                     not the missing earlier event file"
                );
            }
            TranscriptLoadError::MissingEventSource { mutation_id, .. } => panic!(
                "open should not have attempted to read event {mutation_id} before \
                 validating the full manifest"
            ),
            other => panic!("expected InvalidEventSourcePath, got {other:?}"),
        }
    }
}
