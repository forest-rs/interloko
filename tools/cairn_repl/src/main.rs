//! Plain-stdio REPL for Cairn compiler sessions.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};

use cairn_compiler::diagnostic::Diagnostic;
use cairn_compiler::identity::SourceFileId;
use cairn_compiler::session::{
    DumpStage, EnvironmentObject, FunctionRunResult, IncrementalCompilerSession, RuntimeMode,
    SessionArtifacts,
};
use cairn_diagnostics::{DiagnosticCatalog, DiagnosticRenderer};

type Result<T> = std::result::Result<T, ReplError>;

const SOURCE_NAME: &str = "repl.cairn";
const SCRATCH_FUNCTION: &str = "__cairn_repl_expr";

fn main() {
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();

    if let Err(error) = run_repl(stdin.lock(), &mut stdout, &mut stderr, interactive) {
        let _ = writeln!(stderr, "{error}");
        std::process::exit(1);
    }
}

fn run_repl<R, W, E>(input: R, stdout: &mut W, stderr: &mut E, interactive: bool) -> Result<()>
where
    R: BufRead,
    W: Write,
    E: Write,
{
    let mut repl = Repl::new()?;
    if interactive {
        writeln!(stdout, "cairn_repl")?;
        writeln!(stdout, "type :help for commands")?;
    }

    let mut lines = input.lines();
    loop {
        if interactive {
            if repl.has_pending_definition() {
                write!(stdout, "... ")?;
            } else {
                write!(stdout, "> ")?;
            }
            stdout.flush()?;
        }
        let Some(line) = lines.next() else {
            break;
        };
        let line = line?;
        if !repl.handle_line(&line, stdout, stderr)? {
            break;
        }
    }

    if repl.has_pending_definition() {
        writeln!(stderr, "error: unfinished definition")?;
    }

    Ok(())
}

struct Repl {
    session: IncrementalCompilerSession,
    source_file: SourceFileId,
    durable_source: String,
    pending_definition: String,
    last_artifacts: Option<SessionArtifacts>,
    renderer: DiagnosticRenderer,
}

impl Repl {
    fn new() -> Result<Self> {
        let source_file = SourceFileId::from_path(SOURCE_NAME);
        let mut session = IncrementalCompilerSession::new();
        session.add_source_with_id(source_file, SOURCE_NAME, "");
        Ok(Self {
            session,
            source_file,
            durable_source: String::new(),
            pending_definition: String::new(),
            last_artifacts: None,
            renderer: diagnostic_renderer()?,
        })
    }

    fn has_pending_definition(&self) -> bool {
        !self.pending_definition.is_empty()
    }

    fn handle_line<W, E>(&mut self, line: &str, stdout: &mut W, stderr: &mut E) -> Result<bool>
    where
        W: Write,
        E: Write,
    {
        let trimmed = line.trim();
        if self.has_pending_definition() {
            self.pending_definition.push_str(line);
            self.pending_definition.push('\n');
            if definition_is_complete(&self.pending_definition) {
                self.finish_pending_definition(stdout, stderr)?;
            }
            return Ok(true);
        }

        if trimmed.is_empty() {
            return Ok(true);
        }

        if trimmed.starts_with(':') {
            return self.handle_command(trimmed, stdout, stderr);
        }

        if trimmed.starts_with("define ") {
            self.pending_definition.push_str(line);
            self.pending_definition.push('\n');
            if definition_is_complete(&self.pending_definition) {
                self.finish_pending_definition(stdout, stderr)?;
            }
            return Ok(true);
        }

        self.evaluate_expression(line, stdout, stderr)?;
        Ok(true)
    }

    fn handle_command<W, E>(
        &mut self,
        command_line: &str,
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<bool>
    where
        W: Write,
        E: Write,
    {
        let mut parts = command_line.split_whitespace();
        let command = parts.next().unwrap_or_default();
        let args: Vec<&str> = parts.collect();

        match command {
            ":help" => {
                writeln!(stdout, "{}", help_text())?;
            }
            ":quit" => return Ok(false),
            ":source" => self.print_source(stdout)?,
            ":clear" => {
                self.clear();
                writeln!(stdout, "cleared")?;
            }
            ":dump" => {
                let Some(stage) = parse_optional_stage(&args, stderr)? else {
                    return Ok(true);
                };
                let artifacts = self.ensure_artifacts(stderr)?;
                write!(stdout, "{}", artifacts.dump_stage(stage))?;
            }
            ":mop" => {
                let artifacts = self.ensure_artifacts(stderr)?;
                write!(stdout, "{}", artifacts.dump_stage(DumpStage::Mop))?;
            }
            ":eop" => {
                let artifacts = self.ensure_artifacts(stderr)?;
                write!(stdout, "{}", artifacts.dump_stage(DumpStage::Eop))?;
            }
            ":artifacts" => {
                let artifacts = self.ensure_artifacts(stderr)?;
                write!(stdout, "{}", artifacts.dump_artifact_graph())?;
            }
            ":dispatch" => self.print_dispatch(&args, stdout, stderr)?,
            ":inspect" => self.print_object(&args, stdout, stderr)?,
            ":affordances" => self.print_affordances(&args, stdout, stderr)?,
            _ => writeln!(stderr, "error: unknown command `{command}`")?,
        }

        Ok(true)
    }

    fn finish_pending_definition<W, E>(&mut self, stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let mut candidate_source = self.durable_source.clone();
        if !candidate_source.is_empty() && !candidate_source.ends_with("\n\n") {
            candidate_source.push('\n');
        }
        candidate_source.push_str(self.pending_definition.trim_end());
        candidate_source.push_str("\n\n");
        self.pending_definition.clear();

        let artifacts = self.compile_source(candidate_source.clone(), stderr, false)?;
        if artifacts.diagnostics.is_empty() {
            self.durable_source = candidate_source;
            self.last_artifacts = Some(artifacts);
            writeln!(stdout, "ok")?;
        } else {
            self.restore_durable_cache()?;
        }
        Ok(())
    }

    fn evaluate_expression<W, E>(
        &mut self,
        expression: &str,
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let source = self.source_with_scratch(expression);
        let artifacts = self.compile_source(source, stderr, true)?;
        if !artifacts.diagnostics.is_empty() {
            return Ok(());
        }

        match artifacts.run_zero_arg_function_i64(SCRATCH_FUNCTION) {
            FunctionRunResult::ReturnedI64(value) => writeln!(stdout, "{value}")?,
            result => writeln!(stderr, "runtime error: {}", function_run_message(&result))?,
        }
        Ok(())
    }

    fn source_with_scratch(&self, expression: &str) -> String {
        let mut source = self.durable_source.clone();
        if !source.is_empty() && !source.ends_with('\n') {
            source.push('\n');
        }
        let _ = writeln!(source, "define function {SCRATCH_FUNCTION}()");
        let _ = writeln!(source, "  {expression}");
        source.push_str("end;\n");
        source
    }

    fn compile_source<E>(
        &mut self,
        source: String,
        stderr: &mut E,
        remember_artifacts: bool,
    ) -> Result<SessionArtifacts>
    where
        E: Write,
    {
        let update = self
            .session
            .edit_and_compile(self.source_file, &source, RuntimeMode::CompileOnly)
            .ok_or(ReplError::MissingSource)?;
        self.print_diagnostics(&update.artifacts.diagnostics, stderr)?;
        if remember_artifacts {
            self.last_artifacts = Some(update.artifacts.clone());
        }
        Ok(update.artifacts)
    }

    fn ensure_artifacts<E>(&mut self, stderr: &mut E) -> Result<SessionArtifacts>
    where
        E: Write,
    {
        if let Some(artifacts) = &self.last_artifacts {
            return Ok(artifacts.clone());
        }
        self.compile_source(self.durable_source.clone(), stderr, true)
    }

    fn restore_durable_cache(&mut self) -> Result<()> {
        let update = self
            .session
            .edit_and_compile(
                self.source_file,
                &self.durable_source,
                RuntimeMode::CompileOnly,
            )
            .ok_or(ReplError::MissingSource)?;
        self.last_artifacts = Some(update.artifacts);
        Ok(())
    }

    fn print_diagnostics<E>(&self, diagnostics: &[Diagnostic], stderr: &mut E) -> Result<()>
    where
        E: Write,
    {
        if diagnostics.is_empty() {
            return Ok(());
        }
        writeln!(stderr, "diagnostics:")?;
        for diagnostic in diagnostics {
            writeln!(
                stderr,
                "  {:?} {} {}..{} {}",
                diagnostic.severity,
                diagnostic.code,
                diagnostic.span.start,
                diagnostic.span.end,
                self.renderer.render(diagnostic)?
            )?;
        }
        Ok(())
    }

    fn print_source<W>(&self, stdout: &mut W) -> Result<()>
    where
        W: Write,
    {
        if self.durable_source.trim().is_empty() {
            writeln!(stdout, "(empty)")?;
        } else {
            write!(stdout, "{}", self.durable_source)?;
        }
        Ok(())
    }

    fn print_dispatch<W, E>(&mut self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        if args.len() > 1 {
            writeln!(stderr, "usage: :dispatch [dispatch-site-id]")?;
            return Ok(());
        }
        let artifacts = self.ensure_artifacts(stderr)?;
        if let Some(id) = args.first() {
            if let Some(explanation) = artifacts.explain_dispatch_site(id) {
                write!(stdout, "{explanation}")?;
            } else {
                writeln!(stderr, "error: unknown dispatch site `{id}`")?;
            }
        } else {
            write!(stdout, "{}", artifacts.dump_dispatch_sites())?;
        }
        Ok(())
    }

    fn print_object<W, E>(&mut self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(id) = single_object_id(args, ":inspect <object-id>", stderr)? else {
            return Ok(());
        };
        let artifacts = self.ensure_artifacts(stderr)?;
        if let Some(object) = find_object(&artifacts, &id) {
            write!(stdout, "{}", format_environment_object(&object))?;
        } else {
            writeln!(stderr, "error: unknown object `{id}`")?;
        }
        Ok(())
    }

    fn print_affordances<W, E>(
        &mut self,
        args: &[&str],
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(id) = single_object_id(args, ":affordances <object-id>", stderr)? else {
            return Ok(());
        };
        let artifacts = self.ensure_artifacts(stderr)?;
        if let Some(object) = find_object(&artifacts, &id) {
            write!(stdout, "{}", format_affordances(&object))?;
        } else {
            writeln!(stderr, "error: unknown object `{id}`")?;
        }
        Ok(())
    }

    fn clear(&mut self) {
        self.durable_source.clear();
        self.pending_definition.clear();
        self.last_artifacts = None;
        self.session = IncrementalCompilerSession::new();
        self.session
            .add_source_with_id(self.source_file, SOURCE_NAME, "");
    }
}

fn definition_is_complete(source: &str) -> bool {
    let trimmed = source.trim_end();
    trimmed.ends_with("end;") || (trimmed.starts_with("define generic ") && trimmed.ends_with(';'))
}

fn parse_optional_stage<E>(args: &[&str], stderr: &mut E) -> Result<Option<DumpStage>>
where
    E: Write,
{
    match args {
        [] => Ok(Some(DumpStage::All)),
        [stage] => match parse_stage(stage) {
            Some(stage) => Ok(Some(stage)),
            None => {
                writeln!(
                    stderr,
                    "stage must be one of syntax|ast|sem|mop|eop|lowered|tape|all"
                )?;
                Ok(None)
            }
        },
        _ => {
            writeln!(
                stderr,
                "usage: :dump [syntax|ast|sem|mop|eop|lowered|tape|all]"
            )?;
            Ok(None)
        }
    }
}

fn parse_stage(stage: &str) -> Option<DumpStage> {
    match stage {
        "syntax" => Some(DumpStage::Syntax),
        "ast" => Some(DumpStage::Ast),
        "sem" => Some(DumpStage::Sem),
        "mop" => Some(DumpStage::Mop),
        "eop" => Some(DumpStage::Eop),
        "lowered" => Some(DumpStage::Lowered),
        "tape" => Some(DumpStage::Tape),
        "all" => Some(DumpStage::All),
        _ => None,
    }
}

fn single_object_id<E>(args: &[&str], usage: &str, stderr: &mut E) -> Result<Option<String>>
where
    E: Write,
{
    match args {
        [id] => Ok(Some((*id).to_string())),
        _ => {
            writeln!(stderr, "usage: {usage}")?;
            Ok(None)
        }
    }
}

fn find_object(artifacts: &SessionArtifacts, id: &str) -> Option<EnvironmentObject> {
    artifacts
        .environment_objects()
        .into_iter()
        .find(|object| object.id.as_str() == id)
}

fn format_environment_object(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "object {}", object.id);
    let _ = writeln!(out, "kind: {}", object.kind.as_str());
    let _ = writeln!(out, "class: {}", object.meta_class);
    let _ = writeln!(out, "origin: {}", object.origin.as_str());
    let _ = writeln!(out, "storage: {}", object.storage.as_str());
    let _ = writeln!(out, "presentation:");
    let _ = writeln!(out, "  title: {}", object.presentation.title);
    let _ = writeln!(out, "affordances:");
    for affordance in &object.affordances.names {
        let _ = writeln!(out, "  {}", affordance.as_str());
    }
    let _ = writeln!(out, "execution:");
    if let Some(execution) = &object.execution {
        let _ = writeln!(out, "  {}", execution.detail);
    } else {
        let _ = writeln!(out, "  <none>");
    }
    let _ = writeln!(out, "inspection:");
    let _ = writeln!(out, "  {}", object.inspection.detail);
    out
}

fn format_affordances(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "affordances {}", object.id);
    for affordance in &object.affordances.names {
        let _ = writeln!(out, "{}", affordance.as_str());
    }
    out
}

fn function_run_message(result: &FunctionRunResult) -> String {
    match result {
        FunctionRunResult::ReturnedI64(value) => format!("returned {value}"),
        FunctionRunResult::MissingFunction { name } => format!("missing function `{name}`"),
        FunctionRunResult::FunctionHasParameters { name } => {
            format!("function `{name}` has parameters")
        }
        FunctionRunResult::TapeUnavailable => "tape unavailable".into(),
        FunctionRunResult::Trap { message } => format!("trap {message}"),
        FunctionRunResult::FunctionReturnedNonI64 { name } => {
            format!("function `{name}` returned a non-integer result")
        }
    }
}

fn diagnostic_renderer() -> Result<DiagnosticRenderer> {
    if let Ok(path) = env::var("CAIRN_DIAGNOSTIC_CATALOG") {
        let source = fs::read_to_string(&path).map_err(|error| ReplError::Io {
            path: path.clone(),
            error,
        })?;
        let catalog = DiagnosticCatalog::from_toml_source(path, &source)?;
        return Ok(DiagnosticRenderer::new(catalog));
    }

    let renderer = match env::var("CAIRN_DIAGNOSTIC_LOCALE")
        .unwrap_or_else(|_| "en".into())
        .as_str()
    {
        "en" => DiagnosticRenderer::english().map_err(ReplError::from),
        "pseudo" => DiagnosticRenderer::pseudo().map_err(ReplError::from),
        locale => Err(ReplError::UnsupportedLocale(locale.into())),
    }?;
    Ok(renderer)
}

fn help_text() -> &'static str {
    "commands:
  :help
  :quit
  :source
  :clear
  :dump [syntax|ast|sem|mop|eop|lowered|tape|all]
  :mop
  :eop
  :artifacts
  :dispatch [dispatch-site-id]
  :inspect <object-id>
  :affordances <object-id>"
}

#[derive(Debug)]
enum ReplError {
    Io { path: String, error: io::Error },
    Stdio(io::Error),
    DiagnosticCatalog(cairn_diagnostics::DiagnosticCatalogError),
    DiagnosticRender(cairn_diagnostics::DiagnosticRenderError),
    UnsupportedLocale(String),
    MissingSource,
}

impl fmt::Display for ReplError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(formatter, "{path}: {error}"),
            Self::Stdio(error) => write!(formatter, "{error}"),
            Self::DiagnosticCatalog(error) => write!(formatter, "{error}"),
            Self::DiagnosticRender(error) => write!(formatter, "{error}"),
            Self::UnsupportedLocale(locale) => {
                write!(formatter, "unsupported diagnostic locale `{locale}`")
            }
            Self::MissingSource => write!(formatter, "internal error: missing REPL source file"),
        }
    }
}

impl Error for ReplError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { error, .. } | Self::Stdio(error) => Some(error),
            Self::DiagnosticCatalog(error) => Some(error),
            Self::DiagnosticRender(error) => Some(error),
            Self::UnsupportedLocale(_) | Self::MissingSource => None,
        }
    }
}

impl From<io::Error> for ReplError {
    fn from(error: io::Error) -> Self {
        Self::Stdio(error)
    }
}

impl From<cairn_diagnostics::DiagnosticCatalogError> for ReplError {
    fn from(error: cairn_diagnostics::DiagnosticCatalogError) -> Self {
        Self::DiagnosticCatalog(error)
    }
}

impl From<cairn_diagnostics::DiagnosticRenderError> for ReplError {
    fn from(error: cairn_diagnostics::DiagnosticRenderError) -> Self {
        Self::DiagnosticRender(error)
    }
}
