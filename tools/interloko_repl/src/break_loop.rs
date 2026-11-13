//! The REPL as the chooser for unhandled conditions.
//!
//! When a run signals a condition no handler accepts, the driver suspends it
//! with every frame live and asks a [`ConditionChooser`]. [`BreakLoop`] is
//! that chooser for the REPL: it shows the condition, the frames and the
//! restarts on offer, then reads commands from the session's own input until
//! one of them chooses. An edited definition is compiled and offered to the
//! driver as a new code unit, so `retry` can call a frame's function again
//! with the fixed code while the other frames finish on the code they run.

use std::fmt::Write as _;
use std::io::Write;

use execution_tape::program::ValueType;
use execution_tape::value::Value;
use interloko_runtime::chooser::{
    Choice, ConditionChooser, FrameSnapshot, OfferedRestart, UnhandledCondition,
};

use crate::{
    LineSource, PendingDefinitionState, Repl, ReplError, Result, pending_definition_state,
};

/// A suspended run's chooser, reading commands from the REPL's input.
pub(crate) struct BreakLoop<'a, W, E> {
    pub(crate) repl: &'a mut Repl,
    pub(crate) source: &'a mut dyn LineSource,
    pub(crate) stdout: &'a mut W,
    pub(crate) stderr: &'a mut E,
    /// Form an edit replaces, set by `:edit`.
    pub(crate) editing: Option<String>,
    /// Whether the last choice installed code the driver has not answered yet.
    pub(crate) installing: bool,
    /// Output or compile errors the trait method could not return.
    pub(crate) error: Option<ReplError>,
}

impl<'a, W: Write, E: Write> BreakLoop<'a, W, E> {
    pub(crate) fn new(
        repl: &'a mut Repl,
        source: &'a mut dyn LineSource,
        stdout: &'a mut W,
        stderr: &'a mut E,
    ) -> Self {
        Self {
            repl,
            source,
            stdout,
            stderr,
            editing: None,
            installing: false,
            error: None,
        }
    }

    fn session(&mut self, condition: &UnhandledCondition<'_>) -> Result<Choice> {
        if self.installing {
            self.installing = false;
            self.report_install(condition)?;
        } else {
            self.announce(condition)?;
        }
        let mut definition = String::new();
        loop {
            let Some(line) = self.source.next_line(true)? else {
                writeln!(
                    self.stderr,
                    "end of input in break [1]; the condition is unhandled"
                )?;
                return Ok(Choice::Unhandled);
            };
            let trimmed = line.trim();
            if !definition.is_empty() || trimmed.starts_with("define ") {
                definition.push_str(&line);
                definition.push('\n');
                if pending_definition_state(&definition) == PendingDefinitionState::Ready {
                    let text = std::mem::take(&mut definition);
                    if let Some(choice) = self.edit(&text)? {
                        self.installing = true;
                        return Ok(choice);
                    }
                }
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            let mut words = trimmed.split_whitespace();
            let command = words.next().unwrap_or_default();
            let args = words.collect::<Vec<_>>();
            match command {
                ":frames" => self.print_frames(condition)?,
                ":restarts" => self.print_restarts(condition)?,
                ":restart" => {
                    if let Some(choice) = self.restart(condition, &args)? {
                        return Ok(choice);
                    }
                }
                ":edit" => match args.as_slice() {
                    [form] if self.repl.source_buffer.form_index(form).is_some() => {
                        self.editing = Some((*form).to_string());
                        writeln!(
                            self.stdout,
                            "editing {form}; enter a replacement definition"
                        )?;
                    }
                    _ => writeln!(self.stderr, "usage: :edit <form-id>")?,
                },
                ":help" => writeln!(self.stdout, "{}", help_text())?,
                ":quit" => return Ok(Choice::Abort),
                ":run" | ":load" | ":reload" | ":reset" | ":clear" | ":drop-form" => writeln!(
                    self.stderr,
                    "error: {command} is not available while break [1] is waiting"
                )?,
                _ if command.starts_with(':') => {
                    let mut nothing = crate::NoInput;
                    self.repl.handle_command_from(
                        trimmed,
                        &mut nothing,
                        &mut *self.stdout,
                        &mut *self.stderr,
                    )?;
                }
                _ => writeln!(
                    self.stderr,
                    "error: break [1] is waiting; choose a restart with :restart"
                )?,
            }
        }
    }

    fn announce(&mut self, condition: &UnhandledCondition<'_>) -> Result<()> {
        writeln!(
            self.stdout,
            "break [1]: no handler for {}; {} frames suspended, nothing unwound",
            class_name(condition),
            condition.frames.len()
        )?;
        self.print_frames(condition)?;
        self.print_restarts(condition)?;
        writeln!(
            self.stdout,
            "choose with :restart <n|name> [args...], or :edit <form-id> to fix a definition"
        )?;
        Ok(())
    }

    fn print_frames(&mut self, condition: &UnhandledCondition<'_>) -> Result<()> {
        writeln!(self.stdout, "frames (innermost first):")?;
        for (index, frame) in condition.frames.iter().enumerate() {
            writeln!(self.stdout, "  frame:{index} {}", frame_line(frame))?;
        }
        Ok(())
    }

    fn print_restarts(&mut self, condition: &UnhandledCondition<'_>) -> Result<()> {
        writeln!(self.stdout, "restarts:")?;
        for (index, restart) in condition.restarts.iter().enumerate() {
            writeln!(self.stdout, "  restart:{index} {}", restart_line(restart))?;
        }
        Ok(())
    }

    fn report_install(&mut self, condition: &UnhandledCondition<'_>) -> Result<()> {
        if let Some(refused) = condition.refused {
            writeln!(
                self.stderr,
                "error: the edit is accepted for new runs but not installed here: {refused}"
            )?;
            return Ok(());
        }
        let superseded = condition
            .frames
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.code.superseded)
            .collect::<Vec<_>>();
        if superseded.is_empty() {
            writeln!(
                self.stdout,
                "installed the edit as a new code unit; no suspended frame runs replaced code"
            )?;
        } else {
            writeln!(
                self.stdout,
                "installed the edit as a new code unit; still running replaced code:"
            )?;
        }
        for (index, frame) in superseded {
            writeln!(self.stdout, "  frame:{index} {}", frame_line(frame))?;
        }
        Ok(())
    }

    /// Compiles `definition`, replacing the form named by `:edit` or adding
    /// a new one, and offers the result for installation.
    fn edit(&mut self, definition: &str) -> Result<Option<Choice>> {
        let text = definition.trim_end().to_string();
        let mut candidate = self.repl.source_buffer.clone();
        match self.editing.as_deref() {
            Some(form) => {
                let Some(index) = candidate.form_index(form) else {
                    writeln!(self.stderr, "error: unknown form `{form}`")?;
                    return Ok(None);
                };
                candidate.replace_form(index, &text);
            }
            None => candidate.push_definition(text),
        }
        let artifacts =
            self.repl
                .compile_source(candidate.rendered_source(), &mut *self.stderr, false)?;
        if artifacts.has_errors() {
            self.repl.restore_accepted_cache()?;
            writeln!(
                self.stderr,
                "edit rejected; the definitions and break [1] are unchanged"
            )?;
            return Ok(None);
        }
        let Some(code) = artifacts.code_unit() else {
            writeln!(self.stderr, "edit rejected: no code was emitted")?;
            return Ok(None);
        };
        candidate.rebuild_from_form_spans(&artifacts.syntax.source, &artifacts.top_level_forms());
        self.repl.source_buffer = candidate;
        self.repl.last_artifacts = Some(artifacts);
        self.repl.dirty = true;
        self.editing = None;
        Ok(Some(Choice::Install(code)))
    }

    fn restart(
        &mut self,
        condition: &UnhandledCondition<'_>,
        args: &[&str],
    ) -> Result<Option<Choice>> {
        let Some((which, arguments)) = args.split_first() else {
            writeln!(self.stderr, "usage: :restart <n|name> [args...]")?;
            return Ok(None);
        };
        let which = which.strip_prefix("restart:").unwrap_or(which);
        let restart = match which.parse::<usize>() {
            Ok(index) => condition.restarts.get(index),
            Err(_) => condition
                .restarts
                .iter()
                .find(|restart| restart_name(restart) == which),
        };
        let Some(restart) = restart else {
            writeln!(self.stderr, "error: no restart `{which}` is offered")?;
            return Ok(None);
        };
        let choice = match (restart, arguments) {
            (OfferedRestart::Abort, []) => Some(Choice::Abort),
            (OfferedRestart::UseValue { types }, [literal]) => match parse_value(types, literal) {
                Some(value) => Some(Choice::UseValue(vec![value])),
                None => {
                    writeln!(
                        self.stderr,
                        "error: use-value expects {}, got {literal}; break [1] is still waiting",
                        types_label(types)
                    )?;
                    None
                }
            },
            (OfferedRestart::Retry, [frame]) => {
                let target = frame
                    .strip_prefix("frame:")
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| condition.frames.get(index));
                match target {
                    Some(target) if target.code.is_retryable() => Some(Choice::Retry {
                        frame: target.frame.activation.clone(),
                    }),
                    _ => {
                        writeln!(
                            self.stderr,
                            "error: retry needs a suspended function or method frame, got {frame}"
                        )?;
                        None
                    }
                }
            }
            (
                OfferedRestart::Handler {
                    token, descriptor, ..
                },
                [],
            ) => Some(Choice::Restart {
                token: *token,
                arguments: descriptor
                    .parameters
                    .iter()
                    .map(|_| Value::Obj(condition.condition))
                    .collect(),
            }),
            _ => {
                writeln!(
                    self.stderr,
                    "error: wrong arguments for {}",
                    restart_line(restart)
                )?;
                None
            }
        };
        Ok(choice)
    }
}

impl<W: Write, E: Write> ConditionChooser for BreakLoop<'_, W, E> {
    fn choose(&mut self, condition: &UnhandledCondition<'_>) -> Choice {
        match self.session(condition) {
            Ok(choice) => choice,
            Err(error) => {
                self.error.get_or_insert(error);
                Choice::Unhandled
            }
        }
    }
}

fn class_name(condition: &UnhandledCondition<'_>) -> String {
    condition
        .runtime
        .layout()
        .class(condition.class)
        .map_or_else(
            || format!("class#{}", condition.class.0),
            |class| class.name.clone(),
        )
}

fn frame_line(frame: &FrameSnapshot) -> String {
    let code = &frame.code;
    let mut line = format!(
        "{}  u{}:f{}",
        code.name.as_deref().unwrap_or("?"),
        code.func.unit.0,
        code.func.func.0
    );
    if code.superseded
        && let Some(current) = code.current
    {
        let _ = write!(
            line,
            "  superseded by u{}:f{}",
            current.unit.0, current.func.0
        );
    }
    line
}

fn restart_name(restart: &OfferedRestart<'_>) -> String {
    match restart {
        OfferedRestart::Handler { descriptor, .. } => descriptor.name.clone(),
        OfferedRestart::UseValue { .. } => "use-value".into(),
        OfferedRestart::Retry => "retry".into(),
        OfferedRestart::Abort => "abort".into(),
    }
}

fn restart_line(restart: &OfferedRestart<'_>) -> String {
    match restart {
        OfferedRestart::Handler { descriptor, .. } => {
            format!("{}()  the handle site's recovery", descriptor.name)
        }
        OfferedRestart::UseValue { types } => format!(
            "use-value({})  the signal returns the value",
            types_label(types)
        ),
        OfferedRestart::Retry => {
            "retry(frame)  call a frame's function again, with current definitions".into()
        }
        OfferedRestart::Abort => "abort()  unwind to top level, running cleanups".into(),
    }
}

fn types_label(types: &[ValueType]) -> String {
    types
        .iter()
        .map(|kind| match kind {
            ValueType::I64 => "<integer>".to_string(),
            ValueType::F64 => "<double-float>".to_string(),
            ValueType::I32 => "<i32>".to_string(),
            ValueType::U32 => "<u32>".to_string(),
            ValueType::F32 => "<single-float>".to_string(),
            ValueType::Bool => "<boolean>".to_string(),
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parses a scalar literal of the one type a signal resumes with.
fn parse_value(types: &[ValueType], literal: &str) -> Option<Value> {
    match types {
        [ValueType::F64] => literal.parse::<f64>().ok().map(Value::F64),
        [ValueType::I64] => literal.parse::<i64>().ok().map(Value::I64),
        [ValueType::I32] => literal
            .strip_suffix("i32")
            .unwrap_or(literal)
            .parse::<i32>()
            .ok()
            .map(Value::I32),
        [ValueType::U32] => literal
            .strip_suffix("u32")
            .unwrap_or(literal)
            .parse::<u32>()
            .ok()
            .map(Value::U32),
        [ValueType::F32] => literal
            .strip_suffix("f32")
            .unwrap_or(literal)
            .parse::<f32>()
            .ok()
            .map(Value::F32),
        [ValueType::Bool] => match literal {
            "#t" => Some(Value::Bool(true)),
            "#f" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

fn help_text() -> &'static str {
    "break loop:
  :frames                       list the suspended frames, innermost first
  :restarts                     list the restarts on offer
  :restart <n|name> [args...]   choose a restart: use-value <literal>, retry frame:<n>,
                                abort, or a handle site's name
  :edit <form-id>               replace a form with the next definition and install it
  define ...                    add or (after :edit) replace a definition and install it
  :quit                         abort the run
other inspection commands work as at top level"
}
