//! Structured diagnostics for Cairn compiler stages.

use alloc::string::String;

use crate::syntax::Span;

/// Diagnostic severity.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    /// The compiler could not produce a valid artifact for the affected input.
    Error,
}

/// A compiler diagnostic with a primary source span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Severity level.
    pub severity: Severity,
    /// Primary source span.
    pub span: Span,
    /// Human-readable diagnostic text.
    pub message: String,
}

impl Diagnostic {
    /// Creates an error diagnostic.
    #[must_use]
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            span,
            message: message.into(),
        }
    }
}
