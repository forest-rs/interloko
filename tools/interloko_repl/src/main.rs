//! Plain-stdio REPL for Interloko compiler sessions.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use interloko_compiler::diagnostic::{Diagnostic, DiagnosticCode};
use interloko_compiler::grammar_frontend;
use interloko_compiler::identity::{ArtifactKey, SourceFileId};
use interloko_compiler::sem;
use interloko_compiler::session::{
    DumpStage, EnvironmentAffordance, EnvironmentKind, EnvironmentObject, EnvironmentOrigin,
    FunctionValue, IncrementalCompilerSession, RuntimeMode, SessionArtifacts, TopLevelFormKind,
    TopLevelFormSpan, TraceMask,
};
use interloko_compiler::syntax::{self, TokenKind};
use interloko_diagnostics::{DiagnosticCatalog, DiagnosticRenderer};
use nu_ansi_term::{Color, Style as AnsiStyle};
use reedline::{
    ColumnarMenu, Completer, EditCommand, Emacs, Highlighter, KeyCode, KeyModifiers, Keybindings,
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus, Reedline,
    ReedlineError, ReedlineEvent, ReedlineMenu, Signal, Span, StyledText, Suggestion,
    default_emacs_keybindings,
};

type Result<T> = std::result::Result<T, ReplError>;

const SOURCE_NAME: &str = "repl.loko";
const SCRATCH_FUNCTION: &str = "__interloko_repl_expr";
const COMMANDS: &[&str] = &[
    ":help",
    ":quit",
    ":source",
    ":forms",
    ":run",
    ":load",
    ":write",
    ":reload",
    ":reset",
    ":clear",
    ":cancel",
    ":clear-pending",
    ":pending",
    ":dump",
    ":mop",
    ":eop",
    ":artifacts",
    ":dispatch",
    ":explain",
    ":inspect",
    ":affordances",
    ":show-form",
    ":drop-form",
    ":invoke",
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
    let mut line_editor = reedline_editor(completion_state.clone(), style);

    writeln!(stdout, "interloko_repl")?;
    writeln!(stdout, "type :help for commands, Ctrl-D to quit")?;
    stdout.flush()?;

    loop {
        refresh_completion_state(&completion_state, &repl);
        let prompt = InterlokoPrompt {
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

fn reedline_editor(completion_state: Arc<Mutex<CompletionState>>, style: OutputStyle) -> Reedline {
    let completion_menu = ColumnarMenu::default()
        .with_name("completion_menu")
        .with_columns(4)
        .with_column_padding(2);
    let mut keybindings = default_emacs_keybindings();
    add_menu_keybindings(&mut keybindings);

    let editor = Reedline::create()
        .with_completer(Box::new(InterlokoCompleter::new(completion_state)))
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu)))
        .with_edit_mode(Box::new(Emacs::new(keybindings)))
        .with_quick_completions(true)
        .with_partial_completions(true);
    if style == OutputStyle::Ansi {
        editor.with_highlighter(Box::new(InterlokoHighlighter))
    } else {
        editor
    }
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
    source_buffer: ReplSourceBuffer,
    pending_definition: String,
    last_artifacts: Option<SessionArtifacts>,
    last_loaded_path: Option<PathBuf>,
    last_checkpoint: Option<BufferCheckpoint>,
    dirty: bool,
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
            source_buffer: ReplSourceBuffer::default(),
            pending_definition: String::new(),
            last_artifacts: None,
            last_loaded_path: None,
            last_checkpoint: None,
            dirty: false,
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
            if trimmed.starts_with(':') {
                return self.handle_pending_command(trimmed, stdout, stderr);
            }
            self.pending_definition.push_str(line);
            self.pending_definition.push('\n');
            if pending_definition_state(&self.pending_definition) == PendingDefinitionState::Ready {
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

        if trimmed.starts_with("define ") || trimmed.starts_with("///") {
            self.pending_definition.push_str(line);
            self.pending_definition.push('\n');
            if pending_definition_state(&self.pending_definition) == PendingDefinitionState::Ready {
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
            ":forms" => self.print_forms(stdout)?,
            ":run" => self.run_function(&args, stdout, stderr)?,
            ":load" => self.load_file(&args, stdout, stderr)?,
            ":write" => self.write_file(&args, stdout, stderr)?,
            ":reload" => self.reload_file(&args, stdout, stderr)?,
            ":reset" => {
                self.reset();
                writeln!(stdout, "reset")?;
            }
            ":clear" => {
                self.reset();
                writeln!(stdout, "cleared")?;
            }
            ":cancel" | ":clear-pending" => {
                if self.cancel_pending_definition() {
                    writeln!(stdout, "cancelled pending definition")?;
                } else {
                    writeln!(stdout, "no pending definition")?;
                }
            }
            ":pending" => self.print_pending(stdout)?,
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
            ":explain" => self.print_explanation(&args, stdout, stderr)?,
            ":inspect" => self.print_object(&args, stdout, stderr)?,
            ":affordances" => self.print_affordances(&args, stdout, stderr)?,
            ":show-form" => self.show_form(&args, stdout, stderr)?,
            ":drop-form" => self.drop_form_command(&args, stdout, stderr)?,
            ":invoke" => self.invoke_affordance(&args, stdout, stderr)?,
            _ => writeln!(stderr, "error: unknown command `{command}`")?,
        }

        Ok(true)
    }

    fn handle_pending_command<W, E>(
        &mut self,
        command_line: &str,
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<bool>
    where
        W: Write,
        E: Write,
    {
        match command_line {
            ":cancel" | ":clear-pending" => {
                self.pending_definition.clear();
                writeln!(stdout, "cancelled pending definition")?;
            }
            ":pending" => self.print_pending(stdout)?,
            ":help" => writeln!(
                stdout,
                "definition pending; finish with `end;`, view with :pending, or use :cancel"
            )?,
            ":quit" => {
                self.pending_definition.clear();
                return Ok(false);
            }
            _ => {
                writeln!(
                    stderr,
                    "error: definition pending; finish with `end;` or use :cancel"
                )?;
            }
        }
        Ok(true)
    }

    fn finish_pending_definition<W, E>(&mut self, stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let definition_text = self.pending_definition.trim_end().to_string();
        let mut candidate_buffer = self.source_buffer.clone();
        candidate_buffer.push_definition(definition_text);
        let candidate_source = candidate_buffer.rendered_source();
        self.pending_definition.clear();

        let artifacts = self.compile_source(candidate_source, stderr, false)?;
        if !artifacts.has_errors() {
            candidate_buffer
                .rebuild_from_form_spans(&artifacts.syntax.source, &artifacts.top_level_forms());
            self.source_buffer = candidate_buffer;
            self.last_artifacts = Some(artifacts);
            self.dirty = true;
            writeln!(stdout, "{}", self.style.status("ok"))?;
        } else {
            self.restore_accepted_cache()?;
        }
        Ok(())
    }

    fn run_function<W, E>(&mut self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let [name] = args else {
            writeln!(stderr, "usage: :run <zero-arg-function>")?;
            return Ok(());
        };
        let artifacts = self.ensure_artifacts(stderr)?;
        match artifacts.call_function(name, &[], TraceMask::NONE) {
            Ok(outcome) => writeln!(stdout, "{}", self.style.value(&outcome.value))?,
            Err(error) => writeln!(
                stderr,
                "{} {}",
                self.style.error_label("runtime error:"),
                error
            )?,
        }
        Ok(())
    }

    fn load_file<W, E>(&mut self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(path) = single_path(args, ":load <file>", stderr)? else {
            return Ok(());
        };
        let path = PathBuf::from(path);
        let source = match read_source_file(&path) {
            Ok(source) => source,
            Err(error) => {
                writeln!(stderr, "error: {error}")?;
                return Ok(());
            }
        };
        let artifacts = self.compile_source(source.clone(), stderr, false)?;
        if artifacts.has_errors() {
            self.restore_accepted_cache()?;
            return Ok(());
        }

        self.source_buffer = ReplSourceBuffer::from_form_spans(
            &source,
            &artifacts.top_level_forms(),
            SourceOrigin::Loaded,
        );
        self.pending_definition.clear();
        self.last_artifacts = Some(artifacts);
        self.last_loaded_path = Some(path.clone());
        self.last_checkpoint = Some(BufferCheckpoint::Load);
        self.dirty = false;
        writeln!(
            stdout,
            "loaded {} ({} forms)",
            path.display(),
            self.source_buffer.len()
        )?;
        Ok(())
    }

    fn write_file<W, E>(&mut self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(path) = single_path(args, ":write <file>", stderr)? else {
            return Ok(());
        };
        let path = PathBuf::from(path);
        let source = self.source_buffer.rendered_source();
        if let Err(error) = fs::write(&path, source) {
            writeln!(stderr, "error: {}: {error}", path.display())?;
            return Ok(());
        }
        self.last_checkpoint = Some(BufferCheckpoint::Write);
        self.dirty = false;
        writeln!(
            stdout,
            "wrote {} ({} forms)",
            path.display(),
            self.source_buffer.len()
        )?;
        Ok(())
    }

    fn reload_file<W, E>(&mut self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        if !args.is_empty() {
            writeln!(stderr, "usage: :reload")?;
            return Ok(());
        }
        let Some(path) = self.last_loaded_path.clone() else {
            writeln!(stderr, "error: no file has been loaded")?;
            return Ok(());
        };
        let discarded_typed = self.source_buffer.count_origin(SourceOrigin::Typed);
        let source = match read_source_file(&path) {
            Ok(source) => source,
            Err(error) => {
                writeln!(stderr, "error: {error}")?;
                return Ok(());
            }
        };
        let artifacts = self.compile_source(source.clone(), stderr, false)?;
        if artifacts.has_errors() {
            self.restore_accepted_cache()?;
            return Ok(());
        }

        self.source_buffer = ReplSourceBuffer::from_form_spans(
            &source,
            &artifacts.top_level_forms(),
            SourceOrigin::Loaded,
        );
        self.pending_definition.clear();
        self.last_artifacts = Some(artifacts);
        self.last_checkpoint = Some(BufferCheckpoint::Load);
        self.dirty = false;
        if discarded_typed == 0 {
            writeln!(
                stdout,
                "reloaded {} ({} forms)",
                path.display(),
                self.source_buffer.len()
            )?;
        } else {
            writeln!(
                stdout,
                "reloaded {} ({} forms); discarded {} typed definitions",
                path.display(),
                self.source_buffer.len(),
                discarded_typed
            )?;
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
        if artifacts.has_errors() {
            return Ok(());
        }

        match artifacts.call_function(SCRATCH_FUNCTION, &[], TraceMask::NONE) {
            Ok(outcome) => writeln!(stdout, "{}", self.style.value(&outcome.value))?,
            Err(error) => writeln!(
                stderr,
                "{} {}",
                self.style.error_label("runtime error:"),
                error
            )?,
        }
        Ok(())
    }

    fn source_with_scratch(&self, expression: &str) -> String {
        let mut source = self.source_buffer.rendered_source();
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
        self.compile_source(self.source_buffer.rendered_source(), stderr, true)
    }

    fn restore_accepted_cache(&mut self) -> Result<()> {
        let source = self.source_buffer.rendered_source();
        let update = self
            .session
            .edit_and_compile(self.source_file, &source, RuntimeMode::CompileOnly)
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
        let source = self.source_buffer.rendered_source();
        if source.trim().is_empty() {
            writeln!(stdout, "(empty)")?;
        } else {
            writeln!(
                stdout,
                "source {SOURCE_NAME} ({} forms, {} lines, {})",
                self.source_buffer.len(),
                source.lines().count(),
                self.buffer_status()
            )?;
            for (index, line) in source.lines().enumerate() {
                writeln!(stdout, "{:>4} | {line}", index + 1)?;
            }
        }
        Ok(())
    }

    fn print_forms<W>(&self, stdout: &mut W) -> Result<()>
    where
        W: Write,
    {
        if self.source_buffer.is_empty() {
            writeln!(stdout, "(no accepted forms)")?;
            return Ok(());
        }
        writeln!(stdout, "accepted forms")?;
        for entry in self.source_buffer.entries() {
            let form = &entry.form;
            writeln!(
                stdout,
                "  {} {} {} {} lines {}..{}",
                form.id(),
                entry.origin.as_str(),
                form.kind,
                form.name,
                form.line_start,
                form.line_end
            )?;
        }
        Ok(())
    }

    fn print_pending<W>(&self, stdout: &mut W) -> Result<()>
    where
        W: Write,
    {
        if self.pending_definition.trim().is_empty() {
            writeln!(stdout, "(no pending definition)")?;
        } else {
            writeln!(stdout, "pending definition")?;
            for (index, line) in self.pending_definition.lines().enumerate() {
                writeln!(stdout, "{:>4} | {line}", index + 1)?;
            }
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
        match self.resolve_repl_object_reference(&artifacts, &id) {
            ReplObjectReference::Form(entry) => {
                write!(stdout, "{}", format_form_object(&entry, &artifacts))?;
            }
            ReplObjectReference::Environment(object) => {
                write!(stdout, "{}", format_environment_object(&object))?;
            }
            ReplObjectReference::AmbiguousEnvironment(objects) => {
                write_ambiguous_object_reference(stderr, &id, &objects)?;
            }
            ReplObjectReference::UnknownEnvironment => {
                writeln!(stderr, "error: unknown object `{id}`")?;
            }
            ReplObjectReference::UnknownForm => writeln!(stderr, "error: unknown form `{id}`")?,
        }
        Ok(())
    }

    fn print_explanation<W, E>(
        &mut self,
        args: &[&str],
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(id) = single_object_id(args, ":explain <object-id|name>", stderr)? else {
            return Ok(());
        };
        let artifacts = self.ensure_artifacts(stderr)?;
        match self.resolve_repl_object_reference(&artifacts, &id) {
            ReplObjectReference::Form(entry) => {
                write!(stdout, "{}", format_form_explanation(&entry, &artifacts))?;
            }
            ReplObjectReference::Environment(object) => {
                write!(
                    stdout,
                    "{}",
                    format_environment_explanation(&object, &artifacts)
                )?;
            }
            ReplObjectReference::AmbiguousEnvironment(objects) => {
                write_ambiguous_object_reference(stderr, &id, &objects)?;
            }
            ReplObjectReference::UnknownEnvironment => {
                writeln!(stderr, "error: unknown object `{id}`")?;
            }
            ReplObjectReference::UnknownForm => writeln!(stderr, "error: unknown form `{id}`")?,
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
        match self.resolve_repl_object_reference(&artifacts, &id) {
            ReplObjectReference::Form(entry) => {
                write!(stdout, "{}", format_form_affordances(&entry))?;
            }
            ReplObjectReference::Environment(object) => {
                write!(stdout, "{}", format_affordances(&object))?;
            }
            ReplObjectReference::AmbiguousEnvironment(objects) => {
                write_ambiguous_object_reference(stderr, &id, &objects)?;
            }
            ReplObjectReference::UnknownEnvironment => {
                writeln!(stderr, "error: unknown object `{id}`")?;
            }
            ReplObjectReference::UnknownForm => writeln!(stderr, "error: unknown form `{id}`")?,
        }
        Ok(())
    }

    fn show_form<W, E>(&self, args: &[&str], stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(id) = single_object_id(args, ":show-form <form-id>", stderr)? else {
            return Ok(());
        };
        if let Some(entry) = self.source_buffer.find_form(&id) {
            write_form_source(entry, stdout)?;
        } else {
            writeln!(stderr, "error: unknown form `{id}`")?;
        }
        Ok(())
    }

    fn invoke_affordance<W, E>(
        &mut self,
        args: &[&str],
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some((id, affordance)) = object_id_and_affordance(args, stderr)? else {
            return Ok(());
        };
        let artifacts = self.ensure_artifacts(stderr)?;
        match self.resolve_repl_object_reference(&artifacts, &id) {
            ReplObjectReference::Form(entry) => match affordance.as_str() {
                "inspect" => {
                    write!(stdout, "{}", format_form_object(&entry, &artifacts))?;
                }
                "show-defined-objects" => {
                    write!(
                        stdout,
                        "{}",
                        format_form_defined_objects(&entry, &artifacts)
                    )?;
                }
                "show-grammar" => {
                    write!(stdout, "{}", format_form_grammar(&entry, &artifacts))?;
                }
                "show-parse" => {
                    write!(stdout, "{}", format_form_parse(&entry, &artifacts))?;
                }
                "show-cst" => {
                    write!(stdout, "{}", format_form_cst(&entry, &artifacts))?;
                }
                "show-tokens" => {
                    write!(stdout, "{}", format_form_tokens(&entry, &artifacts))?;
                }
                "show-rule-applications" => {
                    write!(
                        stdout,
                        "{}",
                        format_form_rule_applications(&entry, &artifacts)
                    )?;
                }
                "show-diagnostics" => {
                    write!(stdout, "{}", format_form_diagnostics(&entry, &artifacts))?;
                }
                "show-documentation" => {
                    write!(stdout, "{}", format_form_documentation(&entry, &artifacts))?;
                }
                "show-source" => write_form_source(&entry, stdout)?,
                "drop" => self.drop_form_by_id(&id, stdout, stderr)?,
                _ => writeln!(
                    stderr,
                    "error: object `{}` has no affordance `{affordance}`",
                    entry.form.id()
                )?,
            },
            ReplObjectReference::Environment(object) => {
                let Some(environment_affordance) = EnvironmentAffordance::from_name(&affordance)
                else {
                    writeln!(stderr, "error: unknown affordance `{affordance}`")?;
                    return Ok(());
                };
                self.invoke_environment_affordance(
                    &artifacts,
                    &object,
                    environment_affordance,
                    stdout,
                    stderr,
                )?;
            }
            ReplObjectReference::AmbiguousEnvironment(objects) => {
                write_ambiguous_object_reference(stderr, &id, &objects)?;
            }
            ReplObjectReference::UnknownEnvironment => {
                let Some(_) = EnvironmentAffordance::from_name(&affordance) else {
                    writeln!(stderr, "error: unknown affordance `{affordance}`")?;
                    return Ok(());
                };
                writeln!(stderr, "error: unknown object `{id}`")?;
            }
            ReplObjectReference::UnknownForm => writeln!(stderr, "error: unknown form `{id}`")?,
        }
        Ok(())
    }

    fn resolve_repl_object_reference(
        &self,
        artifacts: &SessionArtifacts,
        reference: &str,
    ) -> ReplObjectReference {
        if let Some(entry) = self.source_buffer.find_form(reference).cloned() {
            return ReplObjectReference::Form(entry);
        }
        if is_form_like_id(reference) {
            return ReplObjectReference::UnknownForm;
        }

        match resolve_environment_object_reference(artifacts, reference) {
            EnvironmentObjectReference::One(object) => ReplObjectReference::Environment(object),
            EnvironmentObjectReference::Ambiguous(objects) => {
                ReplObjectReference::AmbiguousEnvironment(objects)
            }
            EnvironmentObjectReference::None => ReplObjectReference::UnknownEnvironment,
        }
    }

    fn invoke_environment_affordance<W, E>(
        &mut self,
        artifacts: &SessionArtifacts,
        object: &EnvironmentObject,
        affordance: EnvironmentAffordance,
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        if !object.affordances.contains(affordance) {
            writeln!(
                stderr,
                "error: object `{}` does not expose affordance `{}`",
                object.id.as_str(),
                affordance.as_str()
            )?;
            return Ok(());
        }

        match affordance {
            EnvironmentAffordance::Inspect => {
                write!(stdout, "{}", format_environment_object(object))?
            }
            EnvironmentAffordance::DumpSyntax => {
                write_invoked_object_stage(artifacts, object, DumpStage::Syntax, stdout)?
            }
            EnvironmentAffordance::DumpAst => {
                write_invoked_object_stage(artifacts, object, DumpStage::Ast, stdout)?
            }
            EnvironmentAffordance::DumpSem => {
                write_invoked_object_stage(artifacts, object, DumpStage::Sem, stdout)?
            }
            EnvironmentAffordance::DumpMop => {
                write_invoked_object_stage(artifacts, object, DumpStage::Mop, stdout)?
            }
            EnvironmentAffordance::DumpEop => {
                write_invoked_object_stage(artifacts, object, DumpStage::Eop, stdout)?
            }
            EnvironmentAffordance::DumpLowered => {
                write_invoked_object_stage(artifacts, object, DumpStage::Lowered, stdout)?
            }
            EnvironmentAffordance::DumpTape => {
                write_invoked_object_stage(artifacts, object, DumpStage::Tape, stdout)?
            }
            EnvironmentAffordance::ShowDependencies | EnvironmentAffordance::DumpArtifacts => {
                write!(stdout, "{}", artifacts.dump_artifact_graph())?
            }
            EnvironmentAffordance::ShowDocumentation => {
                write!(stdout, "{}", format_environment_documentation(object))?
            }
            EnvironmentAffordance::Artifacts => {
                write!(stdout, "{}", artifacts.dump_artifact_graph())?
            }
            EnvironmentAffordance::ShowSlot => self.print_linked_environment_object(
                artifacts,
                object,
                "accessor:",
                "slot:",
                stdout,
                stderr,
            )?,
            EnvironmentAffordance::ShowAccessor => self.print_linked_environment_object(
                artifacts,
                object,
                "slot:",
                "accessor:",
                stdout,
                stderr,
            )?,
            EnvironmentAffordance::ShowDispatch => {
                if object.kind == EnvironmentKind::DispatchSite {
                    if let Some(explanation) = artifacts.explain_dispatch_site(object.id.as_str()) {
                        write!(stdout, "{explanation}")?;
                    } else {
                        writeln!(
                            stderr,
                            "error: unknown dispatch site `{}`",
                            object.id.as_str()
                        )?;
                    }
                } else {
                    write!(stdout, "{}", artifacts.dump_mop_dispatch_table())?;
                }
            }
            EnvironmentAffordance::ShowRuntimeLayout => {
                write!(stdout, "{}", artifacts.dump_mop_runtime_layout())?
            }
            EnvironmentAffordance::ShowGrammarStack => {
                write!(stdout, "{}", artifacts.dump_grammar_stack())?
            }
            EnvironmentAffordance::ShowGrammars => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_environment(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowDirectRules => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_direct_grammar_rules(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowEffectiveRules => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_effective_grammar_rules(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowActivations => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_activations(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowGrammarRule => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_rule(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowExpression => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_expression(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowAction => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_action(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowCaptures => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_action_captures(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowActionOutput => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_action_output(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::ShowParseTrace => {
                write!(stdout, "{}", artifacts.dump_parse_trace())?
            }
            EnvironmentAffordance::ShowParserShadow => {
                write!(stdout, "{}", artifacts.dump_parser_shadow_report())?
            }
            EnvironmentAffordance::ShowGrammarRegistry => {
                write!(stdout, "{}", artifacts.dump_grammar_registry())?
            }
            EnvironmentAffordance::ShowGrammarExtension => write_optional_affordance(
                stdout,
                stderr,
                artifacts.explain_grammar_extension(object.id.as_str()),
                affordance,
                object.id.as_str(),
            )?,
            EnvironmentAffordance::Run => {
                self.run_environment_object(artifacts, object, stdout, stderr)?
            }
            EnvironmentAffordance::AddMethod | EnvironmentAffordance::EditBody => writeln!(
                stderr,
                "error: mutating affordance `{}` is not executable by interloko_repl in this slice",
                affordance.as_str()
            )?,
            EnvironmentAffordance::Diff => writeln!(
                stderr,
                "error: affordance `{}` needs a second source version and is not executable by interloko_repl in this slice",
                affordance.as_str()
            )?,
            EnvironmentAffordance::OpenSource => writeln!(
                stderr,
                "error: affordance `{}` needs editor integration and is not executable by interloko_repl in this slice",
                affordance.as_str()
            )?,
            EnvironmentAffordance::ShowSlots => {
                write_class_slots(artifacts, object, stdout, stderr)?
            }
        }
        Ok(())
    }

    fn print_linked_environment_object<W, E>(
        &self,
        artifacts: &SessionArtifacts,
        object: &EnvironmentObject,
        from_prefix: &str,
        to_prefix: &str,
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(suffix) = object.id.as_str().strip_prefix(from_prefix) else {
            writeln!(
                stderr,
                "error: affordance is not linked from `{}`",
                object.id.as_str()
            )?;
            return Ok(());
        };
        let linked_id = format!("{to_prefix}{suffix}");
        if let Some(linked) = find_object(artifacts, &linked_id) {
            write!(stdout, "{}", format_environment_object(&linked))?;
        } else {
            writeln!(stderr, "error: unknown object `{linked_id}`")?;
        }
        Ok(())
    }

    fn run_environment_object<W, E>(
        &self,
        artifacts: &SessionArtifacts,
        object: &EnvironmentObject,
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(name) = function_name_for_object(artifacts, object) else {
            writeln!(
                stderr,
                "error: affordance `run` requires a named function object"
            )?;
            return Ok(());
        };
        match artifacts.call_function(name, &[], TraceMask::NONE) {
            Ok(outcome) => writeln!(stdout, "{}", self.style.value(&outcome.value))?,
            Err(error) => writeln!(
                stderr,
                "{} {}",
                self.style.error_label("runtime error:"),
                error
            )?,
        }
        Ok(())
    }

    fn drop_form_command<W, E>(
        &mut self,
        args: &[&str],
        stdout: &mut W,
        stderr: &mut E,
    ) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(id) = single_object_id(args, ":drop-form <form-id>", stderr)? else {
            return Ok(());
        };
        self.drop_form_by_id(&id, stdout, stderr)
    }

    fn drop_form_by_id<W, E>(&mut self, id: &str, stdout: &mut W, stderr: &mut E) -> Result<()>
    where
        W: Write,
        E: Write,
    {
        let Some(index) = self.source_buffer.form_index(id) else {
            writeln!(stderr, "error: unknown form `{id}`")?;
            return Ok(());
        };
        let dropped_id = self.source_buffer.entries()[index].form.id();
        let Some(mut candidate_buffer) = self.source_buffer.without_form(index) else {
            writeln!(stderr, "error: unknown form `{id}`")?;
            return Ok(());
        };
        let candidate_source = candidate_buffer.rendered_source();
        let artifacts = self.compile_source(candidate_source, stderr, false)?;
        if artifacts.has_errors() {
            self.restore_accepted_cache()?;
            return Ok(());
        }

        candidate_buffer
            .rebuild_from_form_spans(&artifacts.syntax.source, &artifacts.top_level_forms());
        self.source_buffer = candidate_buffer;
        self.last_artifacts = Some(artifacts);
        self.dirty = true;
        writeln!(stdout, "dropped {dropped_id}")?;
        Ok(())
    }

    fn reset(&mut self) {
        self.source_buffer.clear();
        self.pending_definition.clear();
        self.last_artifacts = None;
        self.last_loaded_path = None;
        self.last_checkpoint = None;
        self.dirty = false;
        self.session = IncrementalCompilerSession::new();
        self.session
            .add_source_with_id(self.source_file, SOURCE_NAME, "");
    }

    fn buffer_status(&self) -> &'static str {
        if self.dirty {
            match self.last_checkpoint {
                Some(BufferCheckpoint::Load) => "dirty since load",
                Some(BufferCheckpoint::Write) => "dirty since write",
                None => "unsaved",
            }
        } else {
            match self.last_checkpoint {
                Some(_) => "clean",
                None => "unsaved",
            }
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ReplSourceBuffer {
    entries: Vec<AcceptedDefinition>,
}

impl ReplSourceBuffer {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn entries(&self) -> &[AcceptedDefinition] {
        &self.entries
    }

    fn form_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.entries.iter().map(|entry| entry.form.id())
    }

    fn form_index(&self, id: &str) -> Option<usize> {
        parse_form_id(id).filter(|index| *index < self.entries.len())
    }

    fn find_form(&self, id: &str) -> Option<&AcceptedDefinition> {
        self.form_index(id)
            .and_then(|index| self.entries.get(index))
    }

    fn without_form(&self, index: usize) -> Option<Self> {
        if index >= self.entries.len() {
            return None;
        }
        let mut buffer = self.clone();
        buffer.entries.remove(index);
        for (index, entry) in buffer.entries.iter_mut().enumerate() {
            entry.form.index = index;
        }
        Some(buffer)
    }

    fn push_definition(&mut self, source: String) {
        self.push_definition_with_origin(source, SourceOrigin::Typed);
    }

    fn push_definition_with_origin(&mut self, source: String, origin: SourceOrigin) {
        let span_start = self.rendered_source().len();
        let span_end = span_start + source.trim_end().len();
        let line_start = self.next_source_line();
        let line_end = line_start + source.lines().count().saturating_sub(1);
        let form = FormRecord::from_definition(
            self.entries.len(),
            &source,
            line_start,
            line_end,
            syntax::Span::new(span_start, span_end),
            None,
        );
        self.entries.push(AcceptedDefinition {
            source,
            origin,
            form,
        });
    }

    fn from_form_spans(source: &str, forms: &[TopLevelFormSpan], origin: SourceOrigin) -> Self {
        let mut buffer = Self::default();
        for entry in source_entries_from_form_spans(source, forms) {
            buffer.push_span_backed_entry(entry, origin);
        }
        buffer
    }

    fn rebuild_from_form_spans(&mut self, source: &str, forms: &[TopLevelFormSpan]) {
        let origins: Vec<_> = self.entries.iter().map(|entry| entry.origin).collect();
        let entries = source_entries_from_form_spans(source, forms);
        if entries.len() != forms.len() || forms.len() != origins.len() {
            return;
        }

        self.entries.clear();
        // Parser forms and buffer entries are both source-ordered; keep origins
        // paired by index when a successful typed edit refreshes span facts.
        for (entry, origin) in entries.into_iter().zip(origins) {
            self.push_span_backed_entry(entry, origin);
        }
    }

    fn push_span_backed_entry(&mut self, entry: SourceBufferEntry, origin: SourceOrigin) {
        let next_line = self.next_source_line();
        let form_start = entry.form_start.saturating_sub(entry.chunk_start);
        let form_end = entry.form_end.saturating_sub(entry.chunk_start);
        let line_start = next_line + line_number_for_offset(&entry.source, form_start) - 1;
        let line_end = next_line + line_number_for_offset(&entry.source, form_end) - 1;
        let form = FormRecord::from_definition(
            self.entries.len(),
            &entry.source[form_start..form_end],
            line_start,
            line_end,
            syntax::Span::new(entry.form_start, entry.form_end),
            Some(entry.kind),
        );
        self.entries.push(AcceptedDefinition {
            source: entry.source,
            origin,
            form,
        });
    }

    fn rendered_source(&self) -> String {
        let mut source = String::new();
        for entry in &self.entries {
            source.push_str(entry.source.trim_end());
            source.push_str("\n\n");
        }
        source
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn count_origin(&self, origin: SourceOrigin) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.origin == origin)
            .count()
    }

    fn next_source_line(&self) -> usize {
        let source = self.rendered_source();
        if source.trim().is_empty() {
            1
        } else {
            source.lines().count() + 1
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceBufferEntry {
    source: String,
    chunk_start: usize,
    form_start: usize,
    form_end: usize,
    kind: TopLevelFormKind,
}

fn source_entries_from_form_spans(
    source: &str,
    forms: &[TopLevelFormSpan],
) -> Vec<SourceBufferEntry> {
    // Preserve meaningful inter-form material without inventing syntax:
    // comments/directives before a form attach to that following form, while
    // trailing material after the last form remains with the previous entry.
    let mut entries = Vec::new();
    let mut chunk_start = 0;
    for form in forms {
        let chunk_end = form.span.end.min(source.len());
        let entry_start = if source[chunk_start..form.span.start.min(source.len())]
            .trim()
            .is_empty()
        {
            form.span.start
        } else {
            chunk_start
        };
        if entry_start <= chunk_end {
            entries.push(SourceBufferEntry {
                source: source[entry_start..chunk_end].trim_end().to_string(),
                chunk_start: entry_start,
                form_start: form.span.start,
                form_end: form.span.end,
                kind: form.kind,
            });
            chunk_start = chunk_end;
        }
    }

    if let Some(last) = entries.last_mut() {
        last.source.push_str(&source[chunk_start..]);
    }

    debug_assert_eq!(entries.len(), forms.len());
    entries
}

fn line_number_for_offset(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AcceptedDefinition {
    source: String,
    origin: SourceOrigin,
    form: FormRecord,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum SourceOrigin {
    Typed,
    Loaded,
}

impl SourceOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Typed => "typed",
            Self::Loaded => "loaded",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum BufferCheckpoint {
    Load,
    Write,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FormRecord {
    // This is a display position within the current REPL source buffer, not a stable session id.
    index: usize,
    kind: String,
    name: String,
    line_start: usize,
    line_end: usize,
    span: syntax::Span,
    parsed_kind: Option<TopLevelFormKind>,
}

impl FormRecord {
    fn from_definition(
        index: usize,
        source: &str,
        line_start: usize,
        line_end: usize,
        span: syntax::Span,
        parsed_kind: Option<TopLevelFormKind>,
    ) -> Self {
        let header = source
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .unwrap_or_default();
        let (kind, name) = definition_header_label(header);
        Self {
            index,
            kind,
            name,
            line_start,
            line_end,
            span,
            parsed_kind,
        }
    }

    fn id(&self) -> String {
        format!("form:f{}", self.index)
    }
}

fn parse_form_id(id: &str) -> Option<usize> {
    let raw = id.strip_prefix("form:").unwrap_or(id);
    raw.strip_prefix('f')?.parse().ok()
}

fn is_form_like_id(id: &str) -> bool {
    let raw = id.strip_prefix("form:").unwrap_or(id);
    raw.starts_with('f') && raw[1..].chars().all(|value| value.is_ascii_digit())
}

fn definition_header_label(header: &str) -> (String, String) {
    let mut parts = header.split_whitespace();
    if parts.next() != Some("define") {
        return ("definition".into(), "<unknown>".into());
    }
    // This intentionally summarizes the first header line only; the compiler owns real parsing.
    let kind = parts.next().unwrap_or("definition");
    let name = parts
        .next()
        .map(|raw| {
            raw.trim_end_matches(';')
                .split_once('(')
                .map_or(raw, |(before, _)| before)
                .to_string()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "<unknown>".into());
    (kind.to_string(), name)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PendingDefinitionState {
    Incomplete,
    Ready,
}

fn pending_definition_state(source: &str) -> PendingDefinitionState {
    if matches!(
        pending_definition_boundary(source),
        Some(PendingDefinitionState::Incomplete)
    ) {
        return PendingDefinitionState::Incomplete;
    }

    let tree = grammar_frontend::parse_file(SourceFileId::ANONYMOUS, source).syntax;
    if tree.diagnostics.is_empty() {
        return PendingDefinitionState::Ready;
    }

    let trimmed_end = source.trim_end().len();
    // Keep this at-EOF allowlist in sync when the parser grows new syntax
    // diagnostic codes. Otherwise genuinely incomplete input, such as a future
    // unterminated string diagnostic, could stop waiting and print too early.
    if tree.diagnostics.iter().all(|diagnostic| {
        matches!(
            diagnostic.code,
            DiagnosticCode::SyntaxExpectedToken | DiagnosticCode::SyntaxExpectedExpression
        ) && diagnostic.span.start >= trimmed_end
            && diagnostic.span.end >= trimmed_end
    }) {
        PendingDefinitionState::Incomplete
    } else {
        PendingDefinitionState::Ready
    }
}

fn pending_definition_boundary(source: &str) -> Option<PendingDefinitionState> {
    let tokens: Vec<_> = syntax::lex(source)
        .into_iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::Whitespace | TokenKind::Comment | TokenKind::DocComment
            )
        })
        .collect();
    let [first, second, ..] = tokens.as_slice() else {
        return Some(PendingDefinitionState::Incomplete);
    };
    if first.kind != TokenKind::Define {
        return None;
    }

    match second.kind {
        TokenKind::Generic => tokens
            .last()
            .is_some_and(|token| token.kind == TokenKind::Semicolon)
            .then_some(PendingDefinitionState::Ready)
            .or(Some(PendingDefinitionState::Incomplete)),
        TokenKind::Class | TokenKind::Function | TokenKind::Method => {
            block_definition_boundary(&tokens)
        }
        _ => None,
    }
}

fn block_definition_boundary(tokens: &[syntax::Token]) -> Option<PendingDefinitionState> {
    let mut depth = 0usize;
    let mut previous = None;
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::Class | TokenKind::Function | TokenKind::Method
                if previous == Some(TokenKind::Define) =>
            {
                depth += 1;
            }
            TokenKind::If | TokenKind::Handle => {
                depth += 1;
            }
            TokenKind::End if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return if tokens[index + 1..]
                        .iter()
                        .all(|token| token.kind == TokenKind::Semicolon)
                    {
                        Some(PendingDefinitionState::Ready)
                    } else {
                        None
                    };
                }
            }
            _ => {}
        }
        previous = Some(token.kind);
    }
    Some(PendingDefinitionState::Incomplete)
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
                    "stage must be one of syntax|ast|sem|mop|eop|lowered|tape|typing|all"
                )?;
                Ok(None)
            }
        },
        _ => {
            writeln!(
                stderr,
                "usage: :dump [syntax|ast|sem|mop|eop|lowered|tape|typing|all]"
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
        "typing" => Some(DumpStage::Typing),
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

fn object_id_and_affordance<E>(args: &[&str], stderr: &mut E) -> Result<Option<(String, String)>>
where
    E: Write,
{
    match args {
        [id, affordance] => Ok(Some(((*id).to_string(), (*affordance).to_string()))),
        _ => {
            writeln!(stderr, "usage: :invoke <object-id> <affordance>")?;
            Ok(None)
        }
    }
}

fn single_path<'a, E>(args: &'a [&str], usage: &str, stderr: &mut E) -> Result<Option<&'a str>>
where
    E: Write,
{
    match args {
        [path] => Ok(Some(*path)),
        _ => {
            writeln!(stderr, "usage: {usage}")?;
            Ok(None)
        }
    }
}

fn read_source_file(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|error| ReplError::Io {
        path: path.display().to_string(),
        error,
    })
}

fn find_object(artifacts: &SessionArtifacts, id: &str) -> Option<EnvironmentObject> {
    artifacts
        .environment_objects()
        .into_iter()
        .find(|object| object.id.as_str() == id)
}

fn write_optional_affordance<W, E>(
    stdout: &mut W,
    stderr: &mut E,
    explanation: Option<String>,
    affordance: EnvironmentAffordance,
    object_id: &str,
) -> Result<()>
where
    W: Write,
    E: Write,
{
    if let Some(explanation) = explanation {
        write!(stdout, "{explanation}")?;
    } else {
        writeln!(
            stderr,
            "error: affordance `{}` is not available for `{}`",
            affordance.as_str(),
            object_id
        )?;
    }
    Ok(())
}

enum ReplObjectReference {
    Form(AcceptedDefinition),
    Environment(Box<EnvironmentObject>),
    AmbiguousEnvironment(Vec<EnvironmentObject>),
    UnknownEnvironment,
    UnknownForm,
}

enum EnvironmentObjectReference {
    One(Box<EnvironmentObject>),
    Ambiguous(Vec<EnvironmentObject>),
    None,
}

fn resolve_environment_object_reference(
    artifacts: &SessionArtifacts,
    reference: &str,
) -> EnvironmentObjectReference {
    if let Some(object) = find_object(artifacts, reference) {
        return EnvironmentObjectReference::One(Box::new(object));
    }

    let mut objects = artifacts
        .environment_objects()
        .into_iter()
        .filter(|object| object_matches_reference(object, reference))
        .collect::<Vec<_>>();
    objects.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    objects.dedup_by(|left, right| left.id == right.id);

    match objects.len() {
        0 => EnvironmentObjectReference::None,
        1 => EnvironmentObjectReference::One(Box::new(objects.remove(0))),
        _ => EnvironmentObjectReference::Ambiguous(objects),
    }
}

fn object_matches_reference(object: &EnvironmentObject, reference: &str) -> bool {
    if is_class_reference(reference) {
        return object.kind == EnvironmentKind::Class && object.presentation.title == reference;
    }

    if reference.contains(':') {
        return false;
    }

    object_lookup_names(object)
        .iter()
        .any(|name| name == reference)
}

fn is_class_reference(reference: &str) -> bool {
    reference.starts_with('<') && reference.ends_with('>') && reference.len() > 2
}

fn object_lookup_names(object: &EnvironmentObject) -> Vec<String> {
    match object.kind {
        // Only objects with language-facing names participate in this lookup.
        // Grammar, artifact, diagnostic, mutation, and source objects still use
        // their stable EOP ids so future named kinds have to opt in here.
        EnvironmentKind::Class => Vec::new(),
        EnvironmentKind::Generic
        | EnvironmentKind::Method
        | EnvironmentKind::Function
        | EnvironmentKind::Constant
        | EnvironmentKind::Accessor => presentation_title_name(&object.presentation.title)
            .into_iter()
            .collect(),
        EnvironmentKind::Slot => object
            .presentation
            .title
            .rsplit_once('.')
            .map_or_else(Vec::new, |(_, slot)| vec![slot.to_string()]),
        EnvironmentKind::LanguageEnvironment => {
            object_id_tail(object.id.as_str(), "language-environment:")
                .into_iter()
                .collect()
        }
        EnvironmentKind::Grammar => object_id_tail(object.id.as_str(), "grammar:")
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn object_id_tail(id: &str, prefix: &str) -> Option<String> {
    id.strip_prefix(prefix)
        .filter(|tail| !tail.is_empty())
        .map(ToString::to_string)
}

fn presentation_title_name(title: &str) -> Option<String> {
    let end = title
        .find('(')
        .or_else(|| title.find(char::is_whitespace))
        .unwrap_or(title.len());
    let name = title[..end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn write_ambiguous_object_reference<W>(
    stderr: &mut W,
    reference: &str,
    objects: &[EnvironmentObject],
) -> Result<()>
where
    W: Write,
{
    writeln!(stderr, "error: ambiguous object reference `{reference}`")?;
    writeln!(stderr, "candidates:")?;
    for object in objects {
        writeln!(
            stderr,
            "  {} {}",
            object.id.as_str(),
            object.presentation.title
        )?;
    }
    Ok(())
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
    let _ = writeln!(out, "source:");
    let _ = writeln!(
        out,
        "  file: {}",
        object.source.source_file.map_or_else(
            || "<none>".to_string(),
            |source_file| source_file.to_string()
        )
    );
    let _ = writeln!(
        out,
        "  definition: {}",
        object
            .source
            .definition
            .map_or_else(|| "<none>".to_string(), |definition| definition.to_string())
    );
    let _ = writeln!(
        out,
        "  span: {}",
        object.source.span.map_or_else(
            || "<none>".to_string(),
            |span| format!("{}..{}", span.start, span.end)
        )
    );
    let _ = writeln!(out, "documentation:");
    write_documentation_lines(&mut out, &object.documentation.lines, "  ");
    let _ = writeln!(out, "artifacts:");
    if object.artifacts.keys.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for key in &object.artifacts.keys {
            let _ = writeln!(out, "  {}", format_artifact_key(key));
        }
    }
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

fn format_artifact_key(key: &ArtifactKey) -> String {
    let definition = key
        .definition
        .map_or_else(|| "<none>".to_string(), |definition| definition.to_string());
    format!(
        "{} ({}, source={}, definition={})",
        key,
        key.kind.as_str(),
        key.source_file,
        definition
    )
}

fn format_affordances(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "affordances {}", object.id);
    for affordance in &object.affordances.names {
        let _ = writeln!(out, "{}", affordance.as_str());
    }
    out
}

fn format_environment_documentation(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "documentation {}", object.id);
    write_documentation_lines(&mut out, &object.documentation.lines, "");
    out
}

fn format_form_explanation(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let form = &entry.form;
    let defined = form_defined_objects(entry, artifacts);
    let grammar = form_grammar_context(entry, artifacts);
    let parse_view = artifacts.parse_view_for_span(form.span);
    let mut out = String::new();
    let _ = writeln!(out, "explain {}", form.id());
    let _ = writeln!(
        out,
        "summary: {} {} `{}` lines {}..{} span {}..{}",
        entry.origin.as_str(),
        form.kind,
        form.name,
        form.line_start,
        form.line_end,
        form.span.start,
        form.span.end
    );
    let _ = writeln!(out, "defined objects:");
    if defined.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for object in &defined {
            let primary = if form_primary_object(entry, object) {
                " primary"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  {} {}{}",
                object.id.as_str(),
                object.presentation.title,
                primary
            );
        }
    }
    let _ = writeln!(out, "grammar:");
    write_form_grammar_context(&mut out, &grammar, "  ");
    let _ = writeln!(
        out,
        "parse: {} nodes, {} tokens, {} rule applications, {} diagnostics",
        parse_view.nodes.len(),
        parse_view.tokens.len(),
        parse_view.rule_applications.len(),
        parse_view.diagnostics.len()
    );
    let doc_comments = form_doc_comments(entry, artifacts);
    let _ = writeln!(out, "documentation:");
    write_documentation_lines(&mut out, &doc_comments, "  ");
    let _ = writeln!(out, "try:");
    let _ = writeln!(out, "  :inspect {}", form.id());
    let _ = writeln!(out, "  :invoke {} show-documentation", form.id());
    let _ = writeln!(out, "  :invoke {} show-source", form.id());
    let _ = writeln!(out, "  :invoke {} show-defined-objects", form.id());
    let _ = writeln!(out, "  :invoke {} show-grammar", form.id());
    let _ = writeln!(out, "  :invoke {} show-parse", form.id());
    out
}

fn format_environment_explanation(
    object: &EnvironmentObject,
    artifacts: &SessionArtifacts,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "explain {}", object.id);
    let _ = writeln!(out, "summary: {}", object.presentation.title);
    let _ = writeln!(
        out,
        "kind: {} · origin: {} · storage: {}",
        object.kind.as_str(),
        object.origin.as_str(),
        object.storage.as_str()
    );
    let _ = writeln!(out, "source:");
    if let Some(span) = object.source.span {
        let line = line_number_for_offset(&artifacts.source.source, span.start);
        let _ = writeln!(out, "  line {line}, span {}..{}", span.start, span.end);
    } else {
        let _ = writeln!(out, "  <none>");
    }
    let _ = writeln!(out, "documentation:");
    write_documentation_lines(&mut out, &object.documentation.lines, "  ");
    let _ = writeln!(out, "defined by:");
    let defining_forms = source_forms_defining_object(artifacts, object.id.as_str());
    if defining_forms.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for form in defining_forms {
            let _ = writeln!(
                out,
                "  {} {} span {}..{}",
                form.id,
                form.kind.as_str(),
                form.span.start,
                form.span.end
            );
        }
    }
    let _ = writeln!(out, "affordances:");
    for affordance in &object.affordances.names {
        let _ = writeln!(out, "  {}", affordance.as_str());
    }
    let _ = writeln!(out, "try:");
    let _ = writeln!(out, "  :inspect {}", object.id);
    let _ = writeln!(out, "  :affordances {}", object.id);
    for affordance in object
        .affordances
        .names
        .iter()
        .filter(|affordance| affordance.as_str() != "inspect")
        .take(4)
    {
        let _ = writeln!(out, "  :invoke {} {}", object.id, affordance.as_str());
    }
    out
}

fn source_forms_defining_object(
    artifacts: &SessionArtifacts,
    object_id: &str,
) -> Vec<interloko_compiler::session::SourceFormProvenance> {
    artifacts
        .source_forms()
        .into_iter()
        .filter(|form| {
            form.produced_object_ids
                .iter()
                .any(|produced| produced == object_id)
        })
        .collect()
}

fn all_invokable_affordance_names() -> Vec<String> {
    let mut names: Vec<String> = FORM_AFFORDANCES
        .iter()
        .map(|affordance| (*affordance).to_string())
        .chain(
            EnvironmentAffordance::all()
                .iter()
                .map(|affordance| affordance.as_str().to_string()),
        )
        .collect();
    names.sort();
    names.dedup();
    names
}

fn function_name_for_object<'a>(
    artifacts: &'a SessionArtifacts,
    object: &EnvironmentObject,
) -> Option<&'a str> {
    if object.kind != EnvironmentKind::Function {
        return None;
    }
    let index: usize = object
        .id
        .as_str()
        .strip_prefix("function:f")?
        .parse()
        .ok()?;
    artifacts
        .sem
        .functions
        .get(index)
        .map(|function| function.name.as_str())
}

fn write_invoked_object_stage<W>(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
    stage: DumpStage,
    out: &mut W,
) -> Result<()>
where
    W: Write,
{
    let dump = artifacts
        .dump_environment_object_stage(object.id.as_str(), stage)
        .expect("environment object existence was checked before invoking affordance");
    write!(out, "{dump}")?;
    Ok(())
}

fn write_class_slots<W, E>(
    artifacts: &SessionArtifacts,
    object: &EnvironmentObject,
    out: &mut W,
    err: &mut E,
) -> Result<()>
where
    W: Write,
    E: Write,
{
    let Some(index) = object_index(object, "class:c") else {
        writeln!(
            err,
            "slots unavailable: `{}` is not a class object",
            object.id.as_str()
        )?;
        return Ok(());
    };
    let Some(class) = artifacts.sem.classes.get(index) else {
        writeln!(
            err,
            "slots unavailable: unknown class `{}`",
            object.id.as_str()
        )?;
        return Ok(());
    };

    writeln!(out, "slots {} {}", object.id.as_str(), class.name)?;
    writeln!(out, "direct-slots: {}", class.direct_slots.len())?;
    for slot in &class.direct_slots {
        writeln!(
            out,
            "  slot:c{}.s{} {} :: {} init-keyword: {}:{} allocation={} mutability={}",
            class.id.0,
            slot.id.0,
            slot.name,
            slot.type_name,
            slot.init_keyword,
            if slot.required_init_keyword {
                " required"
            } else {
                ""
            },
            slot_allocation_label(slot.allocation),
            slot_mutability_label(slot.mutability)
        )?;
    }
    writeln!(out, "effective-slots: {}", class.effective_slots.len())?;
    for slot in &class.effective_slots {
        let source = if slot.source_class == class.id {
            "direct"
        } else {
            "inherited"
        };
        writeln!(
            out,
            "  slot:c{}.s{} {} :: {} source={} from c{}.s{} init-keyword: {}:{} allocation={} mutability={}",
            class.id.0,
            slot.id.0,
            slot.name,
            slot.type_name,
            source,
            slot.source_class.0,
            slot.source_slot.0,
            slot.init_keyword,
            if slot.required_init_keyword {
                " required"
            } else {
                ""
            },
            slot_allocation_label(slot.allocation),
            slot_mutability_label(slot.mutability)
        )?;
    }
    Ok(())
}

fn object_index(object: &EnvironmentObject, prefix: &str) -> Option<usize> {
    object.id.as_str().strip_prefix(prefix)?.parse().ok()
}

fn slot_allocation_label(allocation: sem::SlotAllocation) -> &'static str {
    match allocation {
        sem::SlotAllocation::Instance => "instance",
    }
}

fn slot_mutability_label(mutability: sem::SlotMutability) -> &'static str {
    match mutability {
        sem::SlotMutability::Mutable => "mutable",
        sem::SlotMutability::Constant => "constant",
    }
}

fn format_form_object(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let form = &entry.form;
    let defined = form_defined_objects(entry, artifacts);
    let grammar = form_grammar_context(entry, artifacts);
    let parse_view = artifacts.parse_view_for_span(form.span);
    let mut out = String::new();
    let _ = writeln!(out, "object {}", form.id());
    let _ = writeln!(out, "kind: repl-source-form");
    let _ = writeln!(out, "category: editor-state");
    let _ = writeln!(out, "origin: {}", entry.origin.as_str());
    let _ = writeln!(out, "form-kind: {}", form.kind);
    let _ = writeln!(out, "name: {}", form.name);
    let _ = writeln!(out, "lines: {}..{}", form.line_start, form.line_end);
    let _ = writeln!(out, "span: {}..{}", form.span.start, form.span.end);
    let doc_comments = form_doc_comments(entry, artifacts);
    let _ = writeln!(out, "documentation:");
    write_documentation_lines(&mut out, &doc_comments, "  ");
    let _ = writeln!(out, "defines:");
    if defined.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for object in &defined {
            let primary = if form_primary_object(entry, object) {
                " primary"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  {} {}{}",
                object.id.as_str(),
                object.presentation.title,
                primary
            );
        }
    }
    let _ = writeln!(out, "grammar:");
    write_form_grammar_context(&mut out, &grammar, "  ");
    let _ = writeln!(
        out,
        "parse: {} nodes, {} tokens, {} rule applications, {} diagnostics",
        parse_view.nodes.len(),
        parse_view.tokens.len(),
        parse_view.rule_applications.len(),
        parse_view.diagnostics.len()
    );
    let _ = writeln!(out, "affordances:");
    for affordance in FORM_AFFORDANCES {
        let _ = writeln!(out, "  {affordance}");
    }
    let _ = writeln!(out, "inspection:");
    let _ = writeln!(
        out,
        "  accepted {} {} `{}` in the REPL source buffer",
        entry.origin.as_str(),
        form.kind,
        form.name
    );
    out
}

fn format_form_defined_objects(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "defined objects {}", entry.form.id());
    let defined = form_defined_objects(entry, artifacts);
    if defined.is_empty() {
        let _ = writeln!(out, "<none>");
    } else {
        for object in defined {
            let primary = if form_primary_object(entry, &object) {
                " primary"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{} {}{}",
                object.id.as_str(),
                object.presentation.title,
                primary
            );
        }
    }
    out
}

fn format_form_documentation(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "documentation {}", entry.form.id());
    let doc_comments = form_doc_comments(entry, artifacts);
    write_documentation_lines(&mut out, &doc_comments, "");
    out
}

fn write_documentation_lines(out: &mut String, lines: &[String], indent: &str) {
    if lines.is_empty() {
        let _ = writeln!(out, "{indent}<none>");
    } else {
        for line in lines {
            let _ = writeln!(out, "{indent}{line}");
        }
    }
}

fn form_doc_comments(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> Vec<String> {
    artifacts
        .source_forms()
        .into_iter()
        .find(|form| form.id == entry.form.id())
        .map(|form| form.doc_comments)
        .unwrap_or_default()
}

fn format_form_grammar(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "grammar {}", entry.form.id());
    write_form_grammar_context(&mut out, &form_grammar_context(entry, artifacts), "");
    out
}

fn format_form_parse(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let view = artifacts.parse_view_for_span(entry.form.span);
    let mut out = String::new();
    let _ = writeln!(out, "parse {}", entry.form.id());
    let _ = writeln!(out, "span: {}..{}", view.span.start, view.span.end);
    write_form_cst_view(&mut out, entry, artifacts, "cst:", "  ");
    write_form_token_view(&mut out, entry, artifacts, "tokens:", "  ");
    write_form_rule_application_view(&mut out, entry, artifacts, "rule-applications:", "  ");
    write_form_diagnostic_view(&mut out, entry, artifacts, "diagnostics:", "  ");
    out
}

fn format_form_cst(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "cst {}", entry.form.id());
    write_form_cst_view(&mut out, entry, artifacts, "nodes:", "");
    out
}

fn format_form_tokens(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "tokens {}", entry.form.id());
    write_form_token_view(&mut out, entry, artifacts, "tokens:", "");
    out
}

fn format_form_rule_applications(
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "rule-applications {}", entry.form.id());
    write_form_rule_application_view(&mut out, entry, artifacts, "rules:", "");
    out
}

fn format_form_diagnostics(entry: &AcceptedDefinition, artifacts: &SessionArtifacts) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "diagnostics {}", entry.form.id());
    write_form_diagnostic_view(&mut out, entry, artifacts, "diagnostics:", "");
    out
}

fn write_form_cst_view(
    out: &mut String,
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
    header: &str,
    indent: &str,
) {
    let nodes = artifacts.parse_view_for_span(entry.form.span).nodes;
    let _ = writeln!(out, "{indent}{header}");
    if nodes.is_empty() {
        let _ = writeln!(out, "{indent}  <none>");
        return;
    }
    for node in nodes {
        let _ = writeln!(
            out,
            "{indent}  depth={} {:?} {} span={}..{}",
            node.depth, node.kind, node.id, node.span.start, node.span.end
        );
    }
}

fn write_form_token_view(
    out: &mut String,
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
    header: &str,
    indent: &str,
) {
    let tokens = artifacts.parse_view_for_span(entry.form.span).tokens;
    let _ = writeln!(out, "{indent}{header}");
    if tokens.is_empty() {
        let _ = writeln!(out, "{indent}  <none>");
        return;
    }
    for token in tokens {
        let _ = writeln!(
            out,
            "{indent}  {:?} {}..{} `{}`",
            token.kind, token.span.start, token.span.end, token.text
        );
    }
}

fn write_form_rule_application_view(
    out: &mut String,
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
    header: &str,
    indent: &str,
) {
    let applications = artifacts
        .parse_view_for_span(entry.form.span)
        .rule_applications;
    let _ = writeln!(out, "{indent}{header}");
    if applications.is_empty() {
        let _ = writeln!(out, "{indent}  <none>");
        return;
    }
    for application in applications {
        let _ = writeln!(
            out,
            "{indent}  {} rule={} activation={} span={}..{}",
            application.rule_name,
            application.rule,
            application.activation,
            application.span.start,
            application.span.end
        );
    }
}

fn write_form_diagnostic_view(
    out: &mut String,
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
    header: &str,
    indent: &str,
) {
    let diagnostics = artifacts.parse_view_for_span(entry.form.span).diagnostics;
    let _ = writeln!(out, "{indent}{header}");
    if diagnostics.is_empty() {
        let _ = writeln!(out, "{indent}  <none>");
        return;
    }
    for diagnostic in diagnostics {
        let expected = diagnostic
            .expected_fragment
            .as_deref()
            .unwrap_or("<unknown>");
        let unexpected = diagnostic
            .unexpected_fragment
            .as_deref()
            .unwrap_or("<none>");
        let _ = writeln!(
            out,
            "{indent}  d{} {} span={}..{} rule={} context={} unexpected={} expected={}",
            diagnostic.diagnostic_index,
            diagnostic.code,
            diagnostic.span.start,
            diagnostic.span.end,
            diagnostic.rule_name,
            diagnostic.context.as_str(),
            unexpected,
            expected
        );
    }
}

fn write_form_grammar_context(out: &mut String, context: &[FormGrammarContext], indent: &str) {
    if context.is_empty() {
        let _ = writeln!(out, "{indent}<none>");
        return;
    }
    for (index, item) in context.iter().enumerate() {
        if index > 0 {
            let _ = writeln!(out, "{indent}---");
        }
        let _ = writeln!(out, "{indent}environment: {}", item.environment_object);
        let _ = writeln!(out, "{indent}activation: {}", item.activation_object);
        let _ = writeln!(out, "{indent}grammar: {}", item.grammar_object);
        let _ = writeln!(out, "{indent}root-rule: {}", item.root_rule);
        if let Some(rule_object) = &item.effective_rule_object {
            let _ = writeln!(out, "{indent}effective-rule: {rule_object}");
        }
        let _ = writeln!(out, "{indent}span: {}..{}", item.span.start, item.span.end);
    }
}

fn format_form_affordances(entry: &AcceptedDefinition) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "affordances {}", entry.form.id());
    for affordance in FORM_AFFORDANCES {
        let _ = writeln!(out, "{affordance}");
    }
    out
}

fn write_form_source<W>(entry: &AcceptedDefinition, stdout: &mut W) -> Result<()>
where
    W: Write,
{
    writeln!(
        stdout,
        "source {} {} {}",
        entry.form.id(),
        entry.form.kind,
        entry.form.name
    )?;
    for (offset, line) in entry.source.lines().enumerate() {
        writeln!(stdout, "{:>4} | {line}", entry_line_start(entry) + offset)?;
    }
    Ok(())
}

fn entry_line_start(entry: &AcceptedDefinition) -> usize {
    let leading_lines = entry
        .source
        .lines()
        .position(|line| line.trim_start().starts_with("define "))
        .unwrap_or_default();
    entry.form.line_start.saturating_sub(leading_lines)
}

fn form_defined_objects(
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
) -> Vec<EnvironmentObject> {
    let produced = artifacts
        .source_forms()
        .into_iter()
        .find(|form| form.id == entry.form.id())
        .map(|form| form.produced_object_ids)
        .unwrap_or_default();
    let mut objects = artifacts.environment_objects();
    if produced.is_empty() {
        objects.retain(|object| form_defines_object(entry, object));
    } else {
        objects.retain(|object| {
            produced
                .iter()
                .any(|produced_id| produced_id == object.id.as_str())
        });
    }
    objects.sort_by(|left, right| {
        form_primary_object(entry, right)
            .cmp(&form_primary_object(entry, left))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    objects
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FormGrammarContext {
    environment_object: String,
    activation_object: String,
    grammar_object: String,
    root_rule: String,
    effective_rule_object: Option<String>,
    span: syntax::Span,
}

fn form_grammar_context(
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
) -> Vec<FormGrammarContext> {
    let mut contexts = artifacts
        .grammar_parse
        .activations
        .iter()
        .filter(|activation| span_contains(activation.span, entry.form.span))
        .map(|activation| FormGrammarContext {
            environment_object: grammar_frontend::language_environment_object_id(
                &artifacts.grammar_parse.environment,
            ),
            activation_object: grammar_frontend::grammar_activation_object_id(activation),
            grammar_object: artifacts
                .grammar_parse
                .grammars
                .iter()
                .find(|grammar| grammar.id == activation.grammar)
                .map(grammar_frontend::grammar_object_id)
                .unwrap_or_else(|| format!("grammar:{}", activation.grammar_slug)),
            root_rule: activation.root_rule.to_string(),
            effective_rule_object: form_effective_grammar_rule(entry, artifacts, activation),
            span: activation.span,
        })
        .collect::<Vec<_>>();
    contexts.sort_by_key(|context| {
        (
            context.span.end.saturating_sub(context.span.start),
            context.activation_object.clone(),
        )
    });
    contexts
}

fn form_effective_grammar_rule(
    entry: &AcceptedDefinition,
    artifacts: &SessionArtifacts,
    activation: &grammar_frontend::GrammarActivation,
) -> Option<String> {
    let application = form_rule_application(entry, artifacts, activation)?;
    artifacts
        .grammar_parse
        .effective_rules
        .iter()
        .find(|rule| rule.grammar == activation.grammar && rule.id == application.rule)
        .map(grammar_frontend::effective_grammar_rule_object_id)
}

fn form_rule_application<'a>(
    entry: &AcceptedDefinition,
    artifacts: &'a SessionArtifacts,
    activation: &grammar_frontend::GrammarActivation,
) -> Option<&'a grammar_frontend::GrammarRuleApplication> {
    artifacts
        .grammar_parse
        .provenance
        .applications
        .iter()
        .filter(|application| application.activation == activation.id)
        // Prefer the exact grammar application for this top-level form. The
        // source-file rule usually has the same span for whole-file forms, so
        // skip it unless it is the only exact match.
        .find(|application| {
            application.span == entry.form.span && application.rule_name != "source-file"
        })
        .or_else(|| {
            // Fall back to an exact source-file match for parsers that only
            // recorded the root application.
            artifacts
                .grammar_parse
                .provenance
                .applications
                .iter()
                .filter(|application| application.activation == activation.id)
                .find(|application| application.span == entry.form.span)
        })
        .or_else(|| {
            // Staged regions can wrap emitted forms in an outer activation
            // span. In that case use the smallest rule application that still
            // contains the accepted form.
            artifacts
                .grammar_parse
                .provenance
                .applications
                .iter()
                .filter(|application| application.activation == activation.id)
                .filter(|application| application.rule_name != "source-file")
                .filter(|application| span_contains(application.span, entry.form.span))
                .min_by_key(|application| {
                    application.span.end.saturating_sub(application.span.start)
                })
        })
}

fn span_contains(outer: syntax::Span, inner: syntax::Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

fn form_defines_object(entry: &AcceptedDefinition, object: &EnvironmentObject) -> bool {
    if object.origin != EnvironmentOrigin::SourceAuthored {
        return false;
    }
    let Some(span) = object.source.span else {
        return false;
    };
    span_contains(entry.form.span, span)
}

fn form_primary_object(entry: &AcceptedDefinition, object: &EnvironmentObject) -> bool {
    form_primary_kind_matches(entry.form.parsed_kind, object.kind)
}

fn form_primary_kind_matches(kind: Option<TopLevelFormKind>, object: EnvironmentKind) -> bool {
    // Keep this paired with TopLevelFormKind when new top-level definition forms
    // are added. Non-primary source-authored objects from the same form are still
    // reported by span containment.
    matches!(
        (kind, object),
        (Some(TopLevelFormKind::Class), EnvironmentKind::Class)
            | (Some(TopLevelFormKind::Generic), EnvironmentKind::Generic)
            | (Some(TopLevelFormKind::Method), EnvironmentKind::Method)
            | (Some(TopLevelFormKind::Function), EnvironmentKind::Function)
            | (Some(TopLevelFormKind::Constant), EnvironmentKind::Constant)
    )
}

const FORM_AFFORDANCES: &[&str] = &[
    "inspect",
    "show-source",
    "show-defined-objects",
    "show-grammar",
    "show-parse",
    "show-cst",
    "show-tokens",
    "show-rule-applications",
    "show-diagnostics",
    "show-documentation",
    "drop",
];

fn diagnostic_renderer() -> Result<DiagnosticRenderer> {
    if let Ok(path) = env::var("INTERLOKO_DIAGNOSTIC_CATALOG") {
        let source = fs::read_to_string(&path).map_err(|error| ReplError::Io {
            path: path.clone(),
            error,
        })?;
        let catalog = DiagnosticCatalog::from_toml_source(path, &source)?;
        return Ok(DiagnosticRenderer::new(catalog));
    }

    let renderer = match env::var("INTERLOKO_DIAGNOSTIC_LOCALE")
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
    "session:
  :help                         show this help
  :quit                         exit
  :source                       show accepted source with line numbers
  :forms                        list accepted top-level forms
  :run <zero-arg-function>      run a named zero-argument function
  :load <file>                  replace accepted source from a Loko source file
  :write <file>                 write rendered accepted source to a Loko source file
  :reload                       reload the last loaded source file
  :reset                        clear accepted source and cached artifacts
  :clear                        alias for :reset
  :cancel                       cancel a pending multiline definition
  :clear-pending                alias for :cancel
  :pending                      show pending multiline definition

inspection:
  :dump [syntax|ast|sem|mop|eop|lowered|tape|typing|all]
  :mop
  :eop
  :artifacts
  :dispatch [dispatch-site-id]
  :explain <object-id|name>
  :inspect <object-id|name>
  :affordances <object-id|name>
  :show-form <form-id>
  :drop-form <form-id>
  :invoke <object-id> <affordance>"
}

#[derive(Clone, Debug, Default)]
struct InterlokoHighlighter;

impl Highlighter for InterlokoHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        if line.starts_with(':') {
            return highlight_command_input(line);
        }
        highlight_loko_input(line)
    }
}

fn highlight_command_input(line: &str) -> StyledText {
    let mut styled = StyledText::new();
    let split = line.find(char::is_whitespace).unwrap_or(line.len());
    let command = &line[..split];
    let command_style = if COMMANDS.contains(&command) {
        command_highlight_style()
    } else {
        error_highlight_style()
    };
    push_styled(&mut styled, command_style, command);
    push_command_arguments(&mut styled, &line[split..]);
    styled
}

fn push_command_arguments(styled: &mut StyledText, text: &str) {
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((relative_start, _)) = text[cursor..]
            .char_indices()
            .find(|(_, value)| !value.is_whitespace())
        else {
            push_styled(styled, neutral_highlight_style(), &text[cursor..]);
            return;
        };
        let token_start = cursor + relative_start;
        push_styled(
            styled,
            neutral_highlight_style(),
            &text[cursor..token_start],
        );

        let token_end = text[token_start..]
            .char_indices()
            .find(|(_, value)| value.is_whitespace())
            .map_or(text.len(), |(relative_end, _)| token_start + relative_end);
        let token = &text[token_start..token_end];
        let style = if DUMP_STAGES.contains(&token) {
            literal_highlight_style()
        } else if token.contains(':') {
            type_highlight_style()
        } else {
            neutral_highlight_style()
        };
        push_styled(styled, style, token);
        cursor = token_end;
    }
}

fn highlight_loko_input(line: &str) -> StyledText {
    let mut styled = StyledText::new();
    let mut cursor = 0;
    for token in syntax::lex(line) {
        if token.span.start > cursor {
            push_styled(
                &mut styled,
                neutral_highlight_style(),
                &line[cursor..token.span.start],
            );
        }
        push_styled(
            &mut styled,
            style_for_token(token.kind),
            &line[token.span.start..token.span.end],
        );
        cursor = token.span.end;
    }
    if cursor < line.len() {
        push_styled(&mut styled, neutral_highlight_style(), &line[cursor..]);
    }
    styled
}

fn push_styled(styled: &mut StyledText, style: AnsiStyle, text: &str) {
    if !text.is_empty() {
        styled.push((style, text.to_string()));
    }
}

fn style_for_token(kind: TokenKind) -> AnsiStyle {
    match kind {
        TokenKind::Define
        | TokenKind::Function
        | TokenKind::Class
        | TokenKind::Slot
        | TokenKind::Constant
        | TokenKind::Generic
        | TokenKind::Method
        | TokenKind::Let
        | TokenKind::If
        | TokenKind::Then
        | TokenKind::Else
        | TokenKind::Handle
        | TokenKind::With
        | TokenKind::Restart
        | TokenKind::End => keyword_highlight_style(),
        TokenKind::TypeName => type_highlight_style(),
        TokenKind::Number
        | TokenKind::Bool
        | TokenKind::String
        | TokenKind::Symbol
        | TokenKind::Keyword => literal_highlight_style(),
        TokenKind::Comment | TokenKind::DocComment => comment_highlight_style(),
        TokenKind::Error => error_highlight_style(),
        TokenKind::LParen
        | TokenKind::RParen
        | TokenKind::LBracket
        | TokenKind::RBracket
        | TokenKind::VectorStart
        | TokenKind::Comma
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Equal
        | TokenKind::Arrow
        | TokenKind::Less
        | TokenKind::LessEqual
        | TokenKind::Greater
        | TokenKind::GreaterEqual
        | TokenKind::Colon
        | TokenKind::Assign
        | TokenKind::DoubleColon
        | TokenKind::Dot
        | TokenKind::Semicolon => punctuation_highlight_style(),
        TokenKind::Ident | TokenKind::Whitespace => neutral_highlight_style(),
    }
}

fn neutral_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::White)
}

fn keyword_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::LightPurple).bold()
}

fn command_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::LightGreen).bold()
}

fn type_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::LightCyan).bold()
}

fn literal_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::LightYellow)
}

fn punctuation_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::DarkGray)
}

fn comment_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::DarkGray).italic()
}

fn error_highlight_style() -> AnsiStyle {
    AnsiStyle::new().fg(Color::LightRed).bold()
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

    fn value(self, value: &FunctionValue) -> String {
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
    affordances_by_object: Vec<(String, Vec<String>)>,
}

impl CompletionState {
    fn from_repl(repl: &Repl) -> Self {
        let mut object_ids: Vec<String> = repl.source_buffer.form_ids().collect();
        let mut affordances_by_object: Vec<(String, Vec<String>)> = repl
            .source_buffer
            .form_ids()
            .map(|id| {
                (
                    id,
                    FORM_AFFORDANCES
                        .iter()
                        .map(|affordance| (*affordance).to_string())
                        .collect(),
                )
            })
            .collect();
        let mut dispatch_ids = Vec::new();
        if let Some(artifacts) = &repl.last_artifacts {
            for object in artifacts.environment_objects() {
                let id = object.id.as_str().to_string();
                object_ids.push(id.clone());
                affordances_by_object.push((
                    id,
                    object
                        .affordances
                        .names
                        .iter()
                        .map(|affordance| affordance.as_str().to_string())
                        .collect(),
                ));
            }

            dispatch_ids = artifacts
                .dispatch_sites()
                .into_iter()
                .map(|site| site.id)
                .collect();
        }
        object_ids.sort();
        object_ids.dedup();
        Self {
            object_ids,
            dispatch_ids,
            affordances_by_object,
        }
    }

    fn affordances_for(&self, object_id: &str) -> Vec<String> {
        self.affordances_by_object
            .iter()
            .find(|(id, _)| id == object_id)
            .map(|(_, affordances)| affordances.clone())
            .unwrap_or_else(all_invokable_affordance_names)
    }
}

fn refresh_completion_state(state: &Arc<Mutex<CompletionState>>, repl: &Repl) {
    if let Ok(mut state) = state.lock() {
        *state = CompletionState::from_repl(repl);
    }
}

#[derive(Clone, Debug)]
struct InterlokoCompleter {
    state: Arc<Mutex<CompletionState>>,
}

impl InterlokoCompleter {
    fn new(state: Arc<Mutex<CompletionState>>) -> Self {
        Self { state }
    }
}

impl Completer for InterlokoCompleter {
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
                ":explain" | ":inspect" | ":affordances" => self
                    .state
                    .lock()
                    .map(|state| state.object_ids.clone())
                    .unwrap_or_default(),
                ":show-form" | ":drop-form" => self
                    .state
                    .lock()
                    .map(|state| {
                        state
                            .object_ids
                            .iter()
                            .filter(|id| id.starts_with("form:"))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default(),
                ":invoke" => {
                    let preceding: Vec<&str> =
                        before_cursor[..token_start].split_whitespace().collect();
                    self.state
                        .lock()
                        .map(|state| {
                            if preceding.len() <= 1 {
                                state.object_ids.clone()
                            } else {
                                state.affordances_for(preceding[1])
                            }
                        })
                        .unwrap_or_default()
                }
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

struct InterlokoPrompt {
    pending_definition: bool,
}

impl Prompt for InterlokoPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        if self.pending_definition {
            "define".into()
        } else {
            "interloko".into()
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
    DiagnosticCatalog(interloko_diagnostics::DiagnosticCatalogError),
    DiagnosticRender(interloko_diagnostics::DiagnosticRenderError),
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

impl From<interloko_diagnostics::DiagnosticCatalogError> for ReplError {
    fn from(error: interloko_diagnostics::DiagnosticCatalogError) -> Self {
        Self::DiagnosticCatalog(error)
    }
}

impl From<interloko_diagnostics::DiagnosticRenderError> for ReplError {
    fn from(error: interloko_diagnostics::DiagnosticRenderError) -> Self {
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

    fn completion_values(completer: &mut InterlokoCompleter, line: &str) -> Vec<String> {
        completer
            .complete(line, line.len())
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect()
    }

    fn segment_style(styled: &StyledText, text: &str) -> Option<AnsiStyle> {
        styled
            .buffer
            .iter()
            .find(|(_, segment)| segment == text)
            .map(|(style, _)| *style)
    }

    #[test]
    fn completes_commands_and_dump_stages() {
        let state = Arc::new(Mutex::new(CompletionState::default()));
        let mut completer = InterlokoCompleter::new(state);

        assert!(completion_values(&mut completer, ":du").contains(&":dump".to_string()));
        assert!(completion_values(&mut completer, ":dump l").contains(&"lowered".to_string()));
        assert!(completion_values(&mut completer, ":fo").contains(&":forms".to_string()));
        assert!(completion_values(&mut completer, ":ru").contains(&":run".to_string()));
        assert!(completion_values(&mut completer, "not-a-command").is_empty());
    }

    #[test]
    fn form_records_summarize_accepted_definitions() {
        let mut repl = Repl::new(OutputStyle::Plain).expect("create repl");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for line in [
            "define class <circle> (<object>)",
            "  slot radius :: <integer>;",
            "end;",
            "define generic area(shape :: <object>) => <integer>;",
            "define function answer()",
            "  42",
            "end;",
        ] {
            assert!(
                repl.handle_line(line, &mut stdout, &mut stderr)
                    .expect("handle line")
            );
        }

        assert_eq!(stderr, Vec::<u8>::new());
        let forms: Vec<&FormRecord> = repl
            .source_buffer
            .entries()
            .iter()
            .map(|entry| &entry.form)
            .collect();
        assert_eq!(forms.len(), 3);
        assert_eq!(forms[0].kind, "class");
        assert_eq!(forms[0].name, "<circle>");
        assert_eq!(forms[1].kind, "generic");
        assert_eq!(forms[1].name, "area");
        assert_eq!(forms[2].kind, "function");
        assert_eq!(forms[2].name, "answer");
    }

    #[test]
    fn source_buffer_renders_accepted_definitions() {
        let mut buffer = ReplSourceBuffer::default();
        buffer.push_definition("define function first()\n  1\nend;".to_string());
        buffer.push_definition("define function second()\n  2\nend;".to_string());

        assert_eq!(
            buffer.rendered_source(),
            "define function first()\n  1\nend;\n\ndefine function second()\n  2\nend;\n\n"
        );
        assert_eq!(buffer.entries()[0].form.line_start, 1);
        assert_eq!(buffer.entries()[0].form.line_end, 3);
        assert_eq!(buffer.entries()[1].form.line_start, 5);
        assert_eq!(buffer.entries()[1].form.line_end, 7);
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
        let mut completer = InterlokoCompleter::new(state);
        assert!(
            completion_values(&mut completer, ":inspect function:")
                .contains(&"function:f0".to_string())
        );
        assert!(
            completion_values(&mut completer, ":explain function:")
                .contains(&"function:f0".to_string())
        );
        assert!(
            completion_values(&mut completer, ":dispatch dispatch:")
                .contains(&"dispatch:d0".to_string())
        );
        let function_affordances = completion_values(&mut completer, ":invoke function:f0 ");
        assert!(function_affordances.contains(&"run".to_string()));
        assert!(function_affordances.contains(&"dump-sem".to_string()));
        assert!(!function_affordances.contains(&"drop".to_string()));
        let form_affordances = completion_values(&mut completer, ":invoke form:f0 ");
        assert!(form_affordances.contains(&"show-source".to_string()));
        assert!(form_affordances.contains(&"show-documentation".to_string()));
        assert!(form_affordances.contains(&"drop".to_string()));
        assert!(!form_affordances.contains(&"run".to_string()));
    }

    #[test]
    fn ansi_style_is_separate_from_plain_transcript_style() {
        assert_eq!(OutputStyle::Plain.value(&FunctionValue::Integer(42)), "42");
        assert_eq!(
            OutputStyle::Ansi.value(&FunctionValue::Integer(42)),
            "\x1b[36m42\x1b[0m"
        );
    }

    #[test]
    fn highlighter_styles_loko_source_tokens() {
        let highlighted = InterlokoHighlighter.highlight("define class <circle> // note", 0);

        assert_eq!(
            segment_style(&highlighted, "define"),
            Some(keyword_highlight_style())
        );
        assert_eq!(
            segment_style(&highlighted, "<circle>"),
            Some(type_highlight_style())
        );
        assert_eq!(
            segment_style(&highlighted, "// note"),
            Some(comment_highlight_style())
        );
    }

    #[test]
    fn highlighter_styles_repl_commands_and_arguments() {
        let highlighted = InterlokoHighlighter.highlight(":dump lowered", 0);

        assert_eq!(
            segment_style(&highlighted, ":dump"),
            Some(command_highlight_style())
        );
        assert_eq!(
            segment_style(&highlighted, "lowered"),
            Some(literal_highlight_style())
        );
    }
}
