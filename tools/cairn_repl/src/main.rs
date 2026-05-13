//! Plain-stdio REPL for Cairn compiler sessions.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::{Arc, Mutex};

use cairn_compiler::diagnostic::Diagnostic;
use cairn_compiler::identity::SourceFileId;
use cairn_compiler::session::{
    DumpStage, EnvironmentObject, FunctionRunResult, IncrementalCompilerSession, RuntimeMode,
    SessionArtifacts,
};
use cairn_diagnostics::{DiagnosticCatalog, DiagnosticRenderer};
use reedline::{
    ColumnarMenu, Completer, EditCommand, Emacs, KeyCode, KeyModifiers, Keybindings, MenuBuilder,
    Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineError, ReedlineEvent, ReedlineMenu, Signal, Span, Suggestion,
    default_emacs_keybindings,
};

type Result<T> = std::result::Result<T, ReplError>;

const SOURCE_NAME: &str = "repl.cairn";
const SCRATCH_FUNCTION: &str = "__cairn_repl_expr";
const COMMANDS: &[&str] = &[
    ":help",
    ":quit",
    ":source",
    ":clear",
    ":dump",
    ":mop",
    ":eop",
    ":artifacts",
    ":dispatch",
    ":inspect",
    ":affordances",
];
const DUMP_STAGES: &[&str] = &[
    "syntax", "ast", "sem", "mop", "eop", "lowered", "tape", "all",
];

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    let stdout_is_terminal = stdout.is_terminal();
    let stderr_is_terminal = stderr.is_terminal();
    let interactive = stdin.is_terminal() && stdout_is_terminal;
    let style = if stdout_is_terminal && stderr_is_terminal {
        OutputStyle::Ansi
    } else {
        OutputStyle::Plain
    };

    let result = if interactive {
        run_interactive_repl(&mut stdout, &mut stderr, style)
    } else {
        run_repl(stdin.lock(), &mut stdout, &mut stderr)
    };

    if let Err(error) = result {
        let _ = writeln!(stderr, "{error}");
        std::process::exit(1);
    }
}

fn run_repl<R, W, E>(input: R, stdout: &mut W, stderr: &mut E) -> Result<()>
where
    R: BufRead,
    W: Write,
    E: Write,
{
    let mut repl = Repl::new(OutputStyle::Plain)?;

    let lines = input.lines();
    for line in lines {
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

fn run_interactive_repl<W, E>(stdout: &mut W, stderr: &mut E, style: OutputStyle) -> Result<()>
where
    W: Write,
    E: Write,
{
    let mut repl = Repl::new(style)?;
    let completion_state = Arc::new(Mutex::new(CompletionState::default()));
    let mut line_editor = reedline_editor(completion_state.clone());

    writeln!(stdout, "cairn_repl")?;
    writeln!(stdout, "type :help for commands, Ctrl-D to quit")?;
    stdout.flush()?;

    loop {
        refresh_completion_state(&completion_state, &repl);
        let prompt = CairnPrompt {
            pending_definition: repl.has_pending_definition(),
        };
        match line_editor.read_line(&prompt)? {
            Signal::Success(line) if !repl.handle_line(&line, stdout, stderr)? => {
                break;
            }
            Signal::Success(_) => {}
            Signal::CtrlD => break,
            Signal::CtrlC => {
                if repl.cancel_pending_definition() {
                    writeln!(stderr, "cancelled definition")?;
                } else {
                    writeln!(stderr, "cancelled")?;
                }
            }
            Signal::ExternalBreak(_) => {}
            _ => {}
        }
    }

    Ok(())
}

fn reedline_editor(completion_state: Arc<Mutex<CompletionState>>) -> Reedline {
    let completion_menu = ColumnarMenu::default()
        .with_name("completion_menu")
        .with_columns(4)
        .with_column_padding(2);
    let mut keybindings = default_emacs_keybindings();
    add_menu_keybindings(&mut keybindings);

    Reedline::create()
        .with_completer(Box::new(CairnCompleter::new(completion_state)))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu)))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_quick_completions(true)
        .with_partial_completions(true)
}

fn add_menu_keybindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu("completion_menu".to_string()),
            ReedlineEvent::MenuNext,
        ]),
    );
    keybindings.add_binding(
        KeyModifiers::ALT,
        KeyCode::Enter,
        ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
    );
}

struct Repl {
    session: IncrementalCompilerSession,
    source_file: SourceFileId,
    durable_source: String,
    pending_definition: String,
    last_artifacts: Option<SessionArtifacts>,
    renderer: DiagnosticRenderer,
    style: OutputStyle,
}

impl Repl {
    fn new(style: OutputStyle) -> Result<Self> {
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
            style,
        })
    }

    fn has_pending_definition(&self) -> bool {
        !self.pending_definition.is_empty()
    }

    fn cancel_pending_definition(&mut self) -> bool {
        if self.pending_definition.is_empty() {
            return false;
        }
        self.pending_definition.clear();
        true
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
            writeln!(stdout, "{}", self.style.status("ok"))?;
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
            FunctionRunResult::ReturnedI64(value) => {
                writeln!(stdout, "{}", self.style.value(value))?
            }
            result => writeln!(
                stderr,
                "{} {}",
                self.style.error_label("runtime error:"),
                function_run_message(&result)
            )?,
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
        writeln!(stderr, "{}", self.style.error_label("diagnostics:"))?;
        for diagnostic in diagnostics {
            let severity = format!("{:?}", diagnostic.severity);
            writeln!(
                stderr,
                "  {} {} {}..{} {}",
                self.style.severity(&severity),
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

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum OutputStyle {
    Plain,
    Ansi,
}

impl OutputStyle {
    fn status(self, value: &str) -> String {
        match self {
            Self::Plain => value.to_string(),
            Self::Ansi => format!("\x1b[32m{value}\x1b[0m"),
        }
    }

    fn value(self, value: i64) -> String {
        match self {
            Self::Plain => value.to_string(),
            Self::Ansi => format!("\x1b[36m{value}\x1b[0m"),
        }
    }

    fn severity(self, value: &str) -> String {
        match self {
            Self::Plain => value.to_string(),
            Self::Ansi => format!("\x1b[31m{value}\x1b[0m"),
        }
    }

    fn error_label(self, value: &str) -> String {
        match self {
            Self::Plain => value.to_string(),
            Self::Ansi => format!("\x1b[31m{value}\x1b[0m"),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct CompletionState {
    object_ids: Vec<String>,
    dispatch_ids: Vec<String>,
}

impl CompletionState {
    fn from_repl(repl: &Repl) -> Self {
        let mut object_ids = Vec::new();
        let mut dispatch_ids = Vec::new();
        if let Some(artifacts) = &repl.last_artifacts {
            object_ids = artifacts
                .environment_objects()
                .into_iter()
                .map(|object| object.id.as_str().to_string())
                .collect();
            object_ids.sort();
            object_ids.dedup();

            dispatch_ids = artifacts
                .dispatch_sites()
                .into_iter()
                .map(|site| site.id)
                .collect();
        }
        Self {
            object_ids,
            dispatch_ids,
        }
    }
}

fn refresh_completion_state(state: &Arc<Mutex<CompletionState>>, repl: &Repl) {
    if let Ok(mut state) = state.lock() {
        *state = CompletionState::from_repl(repl);
    }
}

#[derive(Clone, Debug)]
struct CairnCompleter {
    state: Arc<Mutex<CompletionState>>,
}

impl CairnCompleter {
    fn new(state: Arc<Mutex<CompletionState>>) -> Self {
        Self { state }
    }
}

impl Completer for CairnCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let before_cursor = line.get(..pos).unwrap_or(line);
        if !before_cursor.starts_with(':') {
            return Vec::new();
        }

        let (token_start, token) = completion_token(before_cursor, pos);
        let command = before_cursor.split_whitespace().next().unwrap_or_default();
        let candidates: Vec<String> = if !before_cursor[..token_start].contains(char::is_whitespace)
        {
            COMMANDS
                .iter()
                .map(|command| (*command).to_string())
                .collect()
        } else {
            match command {
                ":dump" => DUMP_STAGES
                    .iter()
                    .map(|stage| (*stage).to_string())
                    .collect(),
                ":dispatch" => self
                    .state
                    .lock()
                    .map(|state| state.dispatch_ids.clone())
                    .unwrap_or_default(),
                ":inspect" | ":affordances" => self
                    .state
                    .lock()
                    .map(|state| state.object_ids.clone())
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        };

        candidates
            .into_iter()
            .filter(|candidate| candidate.starts_with(token))
            .map(|candidate| Suggestion {
                value: candidate,
                span: Span::new(token_start, pos),
                append_whitespace: true,
                ..Default::default()
            })
            .collect()
    }
}

fn completion_token(before_cursor: &str, pos: usize) -> (usize, &str) {
    let token_start = before_cursor
        .rfind(char::is_whitespace)
        .map_or(0, |index| index + 1);
    (token_start, &before_cursor[token_start..pos])
}

struct CairnPrompt {
    pending_definition: bool,
}

impl Prompt for CairnPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        if self.pending_definition {
            "define".into()
        } else {
            "cairn".into()
        }
    }

    fn render_prompt_right(&self) -> std::borrow::Cow<'_, str> {
        "".into()
    }

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> std::borrow::Cow<'_, str> {
        if self.pending_definition {
            "... ".into()
        } else {
            "> ".into()
        }
    }

    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<'_, str> {
        "  | ".into()
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> std::borrow::Cow<'_, str> {
        let status = match history_search.status {
            PromptHistorySearchStatus::Passing => "history",
            PromptHistorySearchStatus::Failing => "history?",
        };
        format!("({status}: {}) ", history_search.term).into()
    }
}

#[derive(Debug)]
enum ReplError {
    Io { path: String, error: io::Error },
    Stdio(io::Error),
    DiagnosticCatalog(cairn_diagnostics::DiagnosticCatalogError),
    DiagnosticRender(cairn_diagnostics::DiagnosticRenderError),
    LineEditor(ReedlineError),
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
            Self::LineEditor(error) => write!(formatter, "{error}"),
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
            Self::LineEditor(error) => Some(error),
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

impl From<ReedlineError> for ReplError {
    fn from(error: ReedlineError) -> Self {
        Self::LineEditor(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_values(completer: &mut CairnCompleter, line: &str) -> Vec<String> {
        completer
            .complete(line, line.len())
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect()
    }

    #[test]
    fn completes_commands_and_dump_stages() {
        let state = Arc::new(Mutex::new(CompletionState::default()));
        let mut completer = CairnCompleter::new(state);

        assert!(completion_values(&mut completer, ":du").contains(&":dump".to_string()));
        assert!(completion_values(&mut completer, ":dump l").contains(&"lowered".to_string()));
        assert!(completion_values(&mut completer, "not-a-command").is_empty());
    }

    #[test]
    fn completes_object_and_dispatch_ids_from_last_artifacts() {
        let mut repl = Repl::new(OutputStyle::Plain).expect("create repl");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for line in [
            "define class <circle> (<object>)",
            "  slot radius :: <integer>;",
            "end;",
            "define generic area(shape :: <object>) => <integer>;",
            "define method area(shape :: <circle>) => <integer>",
            "  shape.radius * shape.radius",
            "end;",
            "area(make(<circle>, radius: 3))",
        ] {
            assert!(
                repl.handle_line(line, &mut stdout, &mut stderr)
                    .expect("handle line")
            );
        }
        assert_eq!(stderr, Vec::<u8>::new());

        let state = CompletionState::from_repl(&repl);
        assert!(state.object_ids.iter().any(|id| id == "function:f0"));
        assert!(state.dispatch_ids.iter().any(|id| id == "dispatch:d0"));

        let state = Arc::new(Mutex::new(state));
        let mut completer = CairnCompleter::new(state);
        assert!(
            completion_values(&mut completer, ":inspect function:")
                .contains(&"function:f0".to_string())
        );
        assert!(
            completion_values(&mut completer, ":dispatch dispatch:")
                .contains(&"dispatch:d0".to_string())
        );
    }

    #[test]
    fn ansi_style_is_separate_from_plain_transcript_style() {
        assert_eq!(OutputStyle::Plain.value(42), "42");
        assert_eq!(OutputStyle::Ansi.value(42), "\x1b[36m42\x1b[0m");
    }
}
