//! Diagnostic presentation adapters for Cairn tools.
//!
//! `interloko_compiler` emits structured diagnostic facts. This crate owns the
//! presentation step that turns those facts into user-facing text through
//! MessageFormat catalogs.

use std::error::Error;
use std::fmt;

use interloko_compiler::diagnostic::{Diagnostic, DiagnosticArgValue, DiagnosticCode};
use message_format::{MessageCatalog, runtime};
use message_format_resource_toml::parse_resource_toml;

/// Built-in English diagnostic catalog source.
pub const ENGLISH_CATALOG_TOML: &str = include_str!("../diagnostics/en.mf2.toml");

/// Built-in pseudo-localized diagnostic catalog source.
pub const PSEUDO_CATALOG_TOML: &str = include_str!("../diagnostics/pseudo.mf2.toml");

/// A compiled MessageFormat diagnostic catalog.
#[derive(Clone, Debug)]
pub struct DiagnosticCatalog {
    catalog: MessageCatalog,
}

impl DiagnosticCatalog {
    /// Compiles the built-in English diagnostic catalog.
    pub fn english() -> Result<Self, DiagnosticCatalogError> {
        Self::from_toml_source("cairn-diagnostics/en", ENGLISH_CATALOG_TOML)
    }

    /// Compiles the built-in pseudo-localized diagnostic catalog.
    pub fn pseudo() -> Result<Self, DiagnosticCatalogError> {
        Self::from_toml_source("cairn-diagnostics/pseudo", PSEUDO_CATALOG_TOML)
    }

    /// Compiles a diagnostic catalog from a MessageFormat resource TOML source.
    ///
    /// Message IDs should match [`DiagnosticCode::as_str`].
    pub fn from_toml_source(
        name: impl Into<String>,
        source: &str,
    ) -> Result<Self, DiagnosticCatalogError> {
        let name = name.into();
        let input = parse_resource_toml(name, source)
            .map_err(|err| DiagnosticCatalogError::ResourceToml(err.to_string()))?;
        let (catalog, _source_map) = MessageCatalog::compile_resources(
            [input],
            message_format::compiler::CompileOptions::default(),
        )
        .map_err(|err| DiagnosticCatalogError::Compile(format!("{err:?}")))?;
        Ok(Self { catalog })
    }

    /// Returns true when this catalog contains a message for `code`.
    #[must_use]
    pub fn contains(&self, code: DiagnosticCode) -> bool {
        self.catalog.resolve(code.as_str()).is_ok()
    }
}

/// MessageFormat-backed diagnostic renderer.
#[derive(Clone, Debug)]
pub struct DiagnosticRenderer {
    catalog: DiagnosticCatalog,
}

impl DiagnosticRenderer {
    /// Creates a renderer backed by `catalog`.
    #[must_use]
    pub fn new(catalog: DiagnosticCatalog) -> Self {
        Self { catalog }
    }

    /// Creates a renderer using the built-in English catalog.
    pub fn english() -> Result<Self, DiagnosticCatalogError> {
        DiagnosticCatalog::english().map(Self::new)
    }

    /// Creates a renderer using the built-in pseudo-localized catalog.
    pub fn pseudo() -> Result<Self, DiagnosticCatalogError> {
        DiagnosticCatalog::pseudo().map(Self::new)
    }

    /// Renders a diagnostic through the catalog.
    ///
    /// Missing catalog entries, missing arguments reported by MessageFormat,
    /// and formatting errors are returned as hard presentation failures.
    pub fn render(&self, diagnostic: &Diagnostic) -> Result<String, DiagnosticRenderError> {
        if !self.catalog.contains(diagnostic.code) {
            return Err(DiagnosticRenderError::MissingCatalogEntry {
                code: diagnostic.code,
            });
        }
        let runtime_catalog = self.catalog.catalog.as_runtime_catalog();
        let mut formatter = runtime::Formatter::new(runtime_catalog, runtime::NoopHost)
            .map_err(|err| DiagnosticRenderError::Format(format!("{err:?}")))?;
        let handle = formatter
            .resolve(diagnostic.code.as_str())
            .map_err(|err| DiagnosticRenderError::Format(format!("{err:?}")))?;
        let args = diagnostic_args(runtime_catalog, diagnostic);
        let mut rendered = String::new();
        let mut sink = StringSink { out: &mut rendered };
        let diagnostics = formatter
            .format_to(handle, &args, &mut sink)
            .map_err(|err| DiagnosticRenderError::Format(format!("{err:?}")))?;
        if !diagnostics.is_empty() {
            return Err(DiagnosticRenderError::FormatDiagnostics(format!(
                "{diagnostics:?}"
            )));
        }
        Ok(rendered)
    }
}

struct StringSink<'a> {
    out: &'a mut String,
}

impl runtime::FormatSink for StringSink<'_> {
    fn literal(&mut self, value: &str) {
        self.out.push_str(value);
    }

    fn expression(&mut self, value: &str) {
        self.out.push_str(value);
    }

    fn markup_open(&mut self, _name: &str, _options: &[runtime::FormatOption<'_>]) {}

    fn markup_close(&mut self, _name: &str, _options: &[runtime::FormatOption<'_>]) {}
}

/// Error returned when compiling a diagnostic catalog fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticCatalogError {
    /// The resource TOML adapter rejected the catalog source.
    ResourceToml(String),
    /// MessageFormat compilation rejected the catalog source.
    Compile(String),
}

impl fmt::Display for DiagnosticCatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceToml(message) => {
                write!(formatter, "diagnostic catalog resource error: {message}")
            }
            Self::Compile(message) => {
                write!(formatter, "diagnostic catalog compile error: {message}")
            }
        }
    }
}

impl Error for DiagnosticCatalogError {}

/// Error returned when catalog-backed diagnostic rendering fails.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticRenderError {
    /// The selected catalog does not define the diagnostic code.
    MissingCatalogEntry {
        /// Missing diagnostic code.
        code: DiagnosticCode,
    },
    /// MessageFormat reported recoverable formatting diagnostics.
    FormatDiagnostics(String),
    /// MessageFormat could not format the message.
    Format(String),
}

impl fmt::Display for DiagnosticRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCatalogEntry { code } => {
                write!(formatter, "diagnostic catalog is missing `{code}`")
            }
            Self::FormatDiagnostics(message) => {
                write!(
                    formatter,
                    "diagnostic catalog formatting diagnostics: {message}"
                )
            }
            Self::Format(message) => {
                write!(formatter, "diagnostic catalog format error: {message}")
            }
        }
    }
}

impl Error for DiagnosticRenderError {}

fn diagnostic_args<'a>(
    catalog: &'a runtime::Catalog,
    diagnostic: &Diagnostic,
) -> runtime::MessageArgs<'a> {
    let mut args = runtime::MessageArgs::with_capacity(catalog, diagnostic.args.len());
    for arg in &diagnostic.args {
        let _ = args.insert(&arg.name, diagnostic_value(&arg.value));
    }
    args
}

fn diagnostic_value(value: &DiagnosticArgValue) -> runtime::Value {
    match value {
        DiagnosticArgValue::Text(value) | DiagnosticArgValue::Id(value) => value.clone().into(),
        DiagnosticArgValue::Number(value) => (*value).into(),
        DiagnosticArgValue::Unsigned(value) => i64::try_from(*value)
            .map(runtime::Value::Int)
            .unwrap_or_else(|_| runtime::Value::Str(value.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use interloko_compiler::diagnostic::{DiagnosticArg, DiagnosticCode};
    use interloko_compiler::syntax::Span;

    use super::*;

    #[test]
    fn english_catalog_covers_all_diagnostic_codes() {
        let catalog = DiagnosticCatalog::english().expect("english catalog compiles");

        for code in DiagnosticCode::ALL {
            assert!(
                catalog.contains(*code),
                "english catalog should contain {}",
                code.as_str()
            );
        }
    }

    #[test]
    fn pseudo_catalog_covers_all_diagnostic_codes() {
        let catalog = DiagnosticCatalog::pseudo().expect("pseudo catalog compiles");

        for code in DiagnosticCode::ALL {
            assert!(
                catalog.contains(*code),
                "pseudo catalog should contain {}",
                code.as_str()
            );
        }
    }

    #[test]
    fn renders_from_code_and_args_not_fallback_text() {
        let catalog = DiagnosticCatalog::from_toml_source(
            "pseudo",
            r#"
[messages]
"sem.unresolved-local" = "PSEUDO local={ $local }"
"#,
        )
        .expect("pseudo catalog compiles");
        let renderer = DiagnosticRenderer::new(catalog);
        let diagnostic = Diagnostic::error(Span::new(0, 7), DiagnosticCode::UnresolvedLocal)
            .with_arg(DiagnosticArg::text("local", "missing"));

        assert_eq!(
            renderer.render(&diagnostic).expect("diagnostic renders"),
            "PSEUDO local=missing"
        );
    }

    #[test]
    fn missing_catalog_entry_fails_loudly() {
        let catalog = DiagnosticCatalog::from_toml_source(
            "partial",
            r#"
[messages]
"sem.unresolved-class" = "class { $class }"
"#,
        )
        .expect("partial catalog compiles");
        let renderer = DiagnosticRenderer::new(catalog);
        let diagnostic = Diagnostic::error(Span::new(0, 7), DiagnosticCode::UnresolvedLocal)
            .with_arg(DiagnosticArg::text("local", "missing"));

        assert_eq!(
            renderer.render(&diagnostic),
            Err(DiagnosticRenderError::MissingCatalogEntry {
                code: DiagnosticCode::UnresolvedLocal,
            })
        );
    }

    #[test]
    fn missing_required_argument_fails_loudly() {
        let catalog = DiagnosticCatalog::from_toml_source(
            "missing-arg",
            r#"
[messages]
"sem.unresolved-local" = "local={ $local }"
"#,
        )
        .expect("catalog compiles");
        let renderer = DiagnosticRenderer::new(catalog);
        let diagnostic = Diagnostic::error(Span::new(0, 7), DiagnosticCode::UnresolvedLocal);

        assert!(matches!(
            renderer.render(&diagnostic),
            Err(DiagnosticRenderError::FormatDiagnostics(_))
                | Err(DiagnosticRenderError::Format(_))
        ));
    }

    #[test]
    fn converts_all_diagnostic_arg_value_shapes() {
        let catalog = DiagnosticCatalog::from_toml_source(
            "args",
            r#"
[messages]
"lower.invalid-semantic" = "{ $artifact } { $delta } { $count } { $huge }"
"#,
        )
        .expect("arg catalog compiles");
        let renderer = DiagnosticRenderer::new(catalog);
        let diagnostic = Diagnostic::error(Span::new(0, 7), DiagnosticCode::LowerInvalidSemantic)
            .with_arg(DiagnosticArg::id("artifact", "art1"))
            .with_arg(DiagnosticArg::number("delta", -1))
            .with_arg(DiagnosticArg::unsigned("count", 2))
            .with_arg(DiagnosticArg::unsigned("huge", u64::MAX));

        assert_eq!(
            renderer.render(&diagnostic).expect("diagnostic renders"),
            "art1 -1 2 18446744073709551615"
        );
    }

    #[test]
    fn uncategorized_diagnostics_render_through_catalog() {
        let renderer = DiagnosticRenderer::english().expect("english catalog compiles");
        let diagnostic = Diagnostic::error(Span::new(0, 7), DiagnosticCode::Uncategorized);

        assert_eq!(
            renderer.render(&diagnostic).expect("diagnostic renders"),
            "uncategorized diagnostic"
        );
    }
}
