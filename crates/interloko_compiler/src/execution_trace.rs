//! Interloko meaning for `execution_tape` execution recordings.
//!
//! Traced runs record with [`execution_tape::recording::Recorder`]. The owned
//! [`Recording`] is the evidence: invocation values, every host call with its
//! guest call stack, optional instruction history, the final return or trap
//! with its stack, capture completeness and storage counts. It also supports
//! checked replay. This module adds no second recorder; it only adds what the
//! VM cannot know:
//!
//! - tape function ids resolve to lowered Interloko functions, their roles
//!   ([`lower::FunctionKind`]) and environment object ids;
//! - instruction locations resolve through the tape span table to source
//!   positions in the compiled file.
//!
//! [`ExecutionTraceView`] borrows a trace together with the artifacts that
//! produced it. When the artifacts did not produce the recorded program, the
//! view stays usable but reports raw function ids and no source positions.

use alloc::string::String;
use alloc::sync::Arc;
use core::fmt::{self, Write as _};

use execution_tape::opcode::Opcode;
use execution_tape::recording::{
    CallStack, HostOutcome, InstructionCapture, InstructionOutcome, InstructionStatus, Outcome,
    RecordLimits, RecordedValue, Recorder, Recording,
};
use execution_tape::trace::{TraceLocation, TraceMask, TraceSink};
use execution_tape::value::{FuncId, FuncRef, UnitId};
use execution_tape::verifier::VerifiedProgram;

use crate::session::EnvironmentObjectId;
use crate::{lower, tape};

/// Maximum attempted instructions retained by
/// [`ExecutionTraceDetail::Instructions`]. Longer runs keep a labelled prefix;
/// boundary evidence is unaffected.
pub const TRACE_INSTRUCTION_LIMIT: usize = 10_000;

/// Maximum caller frames written per stack by [`ExecutionTraceView::render`].
const RENDERED_STACK_FRAMES: usize = 8;

/// Evidence retained by a traced run.
///
/// Both levels capture invocation values, host-call arguments and results,
/// and the final values or trap. Capture budgets are
/// [`RecordLimits::default`]; exhausting one marks the capture incomplete
/// without changing execution.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExecutionTraceDetail {
    /// The invocation, every host call and the final outcome, each with its
    /// guest call stack.
    ///
    /// Every entered activation is counted. Activations that make no host
    /// call and are not on the final stack are not listed individually.
    Boundaries,
    /// [`Self::Boundaries`] plus up to [`TRACE_INSTRUCTION_LIMIT`] attempted
    /// instructions, each with its actual successor and guest call stack.
    Instructions,
}

impl ExecutionTraceDetail {
    fn label(self) -> &'static str {
        match self {
            Self::Boundaries => "boundaries",
            Self::Instructions => "instructions",
        }
    }
}

/// Owned execution evidence for one traced runtime invocation.
///
/// Clones share one immutable [`Recording`].
#[derive(Clone, Debug)]
pub struct ExecutionTrace {
    detail: ExecutionTraceDetail,
    recording: Arc<Recording>,
}

impl ExecutionTrace {
    /// Returns the evidence level requested for this run.
    #[must_use]
    pub fn detail(&self) -> ExecutionTraceDetail {
        self.detail
    }

    /// Borrows the upstream recording, for completeness checks, raw values,
    /// aggregate graphs, storage counts and checked replay.
    #[must_use]
    pub fn recording(&self) -> &Recording {
        &self.recording
    }
}

/// Recorder for one traced invocation of a shared tape program.
pub(crate) struct TraceCapture {
    detail: ExecutionTraceDetail,
    recorder: Recorder,
}

impl TraceCapture {
    /// Creates a recorder for one run of exactly `program`.
    pub(crate) fn new(program: &Arc<VerifiedProgram>, detail: ExecutionTraceDetail) -> Self {
        let recorder = Recorder::new(Arc::clone(program), RecordLimits::default());
        let recorder = match detail {
            ExecutionTraceDetail::Boundaries => recorder,
            ExecutionTraceDetail::Instructions => recorder.with_instructions(InstructionCapture {
                max_steps: TRACE_INSTRUCTION_LIMIT,
                watches: alloc::vec::Vec::new(),
            }),
        };
        Self { detail, recorder }
    }

    /// Returns the event mask the VM must deliver for a complete recording.
    pub(crate) fn mask(&self) -> TraceMask {
        self.recorder.mask()
    }

    /// Borrows the recorder as the VM trace sink.
    pub(crate) fn sink(&mut self) -> &mut dyn TraceSink {
        &mut self.recorder
    }

    /// Freezes the recording.
    pub(crate) fn finish(self) -> ExecutionTrace {
        ExecutionTrace {
            detail: self.detail,
            recording: Arc::new(self.recorder.finish()),
        }
    }
}

/// A position in the compiled source file.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SourcePosition {
    /// Byte offset where the instruction's source span starts.
    pub offset: usize,
    /// One-based line.
    pub line: usize,
    /// One-based column, counted in characters.
    pub column: usize,
}

impl fmt::Display for SourcePosition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.line, self.column)
    }
}

/// A recorded tape function with its Interloko identity, when resolvable.
#[derive(Copy, Clone, Debug)]
pub struct TracedFunction<'a> {
    id: FuncRef,
    lowered: Option<&'a lower::Function>,
}

impl<'a> TracedFunction<'a> {
    /// Returns the `execution_tape` function: its unit and program-local id.
    #[must_use]
    pub fn id(&self) -> FuncRef {
        self.id
    }

    /// Returns the lowered function name, when resolvable.
    #[must_use]
    pub fn name(&self) -> Option<&'a str> {
        self.lowered.map(|function| function.name.as_str())
    }

    /// Returns the Interloko role of this function, when resolvable.
    #[must_use]
    pub fn kind(&self) -> Option<lower::FunctionKind> {
        self.lowered.map(|function| function.kind)
    }

    /// Returns the environment object that inspects this function's source
    /// definition: a function, a method, or the generic function a
    /// dispatcher serves.
    #[must_use]
    pub fn object_id(&self) -> Option<EnvironmentObjectId> {
        match self.kind()? {
            lower::FunctionKind::Function(id) => Some(EnvironmentObjectId::new(alloc::format!(
                "function:f{}",
                id.0
            ))),
            lower::FunctionKind::Method(id) => {
                Some(EnvironmentObjectId::new(alloc::format!("method:m{}", id.0)))
            }
            lower::FunctionKind::Dispatcher(id) => Some(EnvironmentObjectId::new(alloc::format!(
                "generic:g{}",
                id.0
            ))),
            lower::FunctionKind::SlotAccessor { .. }
            | lower::FunctionKind::CallableThunk { .. }
            | lower::FunctionKind::Closure(_)
            | lower::FunctionKind::ExitThunk { .. } => None,
        }
    }
}

impl fmt::Display for TracedFunction<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => formatter.write_str(name),
            None => write!(formatter, "u{}:f{}", self.id.unit.0, self.id.func.0),
        }
    }
}

/// A recorded instruction location with Interloko meaning.
#[derive(Copy, Clone, Debug)]
pub struct TracedLocation<'a> {
    /// Function containing the instruction.
    pub function: TracedFunction<'a>,
    /// Instruction byte offset within the tape function.
    pub pc: u32,
    /// Source position of the instruction's span, when resolvable.
    pub source: Option<SourcePosition>,
}

impl TracedLocation<'_> {
    /// Returns the raw `execution_tape` location.
    #[must_use]
    pub fn raw(&self) -> TraceLocation {
        TraceLocation {
            unit: self.function.id.unit,
            func: self.function.id.func,
            pc: self.pc,
        }
    }
}

impl fmt::Display for TracedLocation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.function, self.pc)?;
        if let Some(source) = self.source {
            write!(formatter, " ({source})")?;
        }
        Ok(())
    }
}

/// Guest call stack at one recorded boundary, innermost first.
#[derive(Clone, Debug)]
pub struct TracedStack<'a> {
    frames: Option<CallStack<'a>>,
    view: ExecutionTraceView<'a>,
}

impl<'a> TracedStack<'a> {
    /// Returns the number of guest activations, or `None` when the recording
    /// did not retain this stack.
    #[must_use]
    pub fn depth(&self) -> Option<usize> {
        self.frames.as_ref().map(ExactSizeIterator::len)
    }

    /// Iterates the boundary location followed by each suspended caller
    /// instruction. Empty when the stack was not retained.
    pub fn locations(&self) -> impl Iterator<Item = TracedLocation<'a>> + 'a {
        let view = self.view;
        self.frames
            .clone()
            .into_iter()
            .flatten()
            .map(move |location| view.location(location))
    }
}

/// One recorded host call.
#[derive(Debug)]
pub struct TracedHostCall<'a> {
    /// Observation order shared with embedding control commands.
    pub sequence: usize,
    /// Calling guest instruction.
    pub location: TracedLocation<'a>,
    /// Host symbol, when the program names it.
    pub symbol: Option<&'a str>,
    /// Captured arguments.
    pub arguments: &'a [RecordedValue],
    /// Raw host result before VM validation.
    pub outcome: HostOutcome<'a>,
    /// Guest callers, innermost first.
    pub stack: TracedStack<'a>,
}

/// One recorded instruction attempt.
#[derive(Debug)]
pub struct TracedInstruction<'a> {
    /// Zero-based attempt ordinal.
    pub index: usize,
    /// Executed instruction.
    pub location: TracedLocation<'a>,
    /// Raw opcode byte.
    pub opcode: u8,
    /// Actual successor, including call and return transitions.
    pub next: Option<TracedLocation<'a>>,
    /// Outcome of this attempt, separate from the invocation outcome.
    pub outcome: InstructionOutcome,
    /// Guest ancestry at the start of this attempt.
    pub stack: TracedStack<'a>,
}

impl TracedInstruction<'_> {
    /// Returns the opcode mnemonic, or `"?"` for an unknown byte.
    #[must_use]
    pub fn mnemonic(&self) -> &'static str {
        Opcode::from_u8(self.opcode).map_or("?", Opcode::mnemonic)
    }
}

/// The recorded final return or trap.
#[derive(Debug)]
pub struct TracedOutcome<'a> {
    /// Committed values or the original typed trap.
    pub result: Outcome<'a>,
    /// Final return or trapping instruction, when retained.
    pub location: Option<TracedLocation<'a>>,
    /// Guest stack at that instruction, pinned before trap unwinding.
    pub stack: TracedStack<'a>,
}

#[derive(Copy, Clone, Debug)]
struct ProgramMeaning<'a> {
    unit: UnitId,
    lowered: &'a lower::Module,
    functions: &'a [FuncId],
    source: &'a str,
}

/// An [`ExecutionTrace`] read through the Interloko artifacts that produced it.
#[derive(Copy, Clone, Debug)]
pub struct ExecutionTraceView<'a> {
    trace: &'a ExecutionTrace,
    meaning: Option<ProgramMeaning<'a>>,
}

impl<'a> ExecutionTraceView<'a> {
    /// Resolves `trace` against artifacts; meaning is attached only when
    /// `tape` is the exact program of the unit the trace entered, and only
    /// that unit's functions resolve.
    pub(crate) fn new(
        trace: &'a ExecutionTrace,
        lowered: Option<&'a lower::Module>,
        tape: Option<&'a tape::TapeModule>,
        source: &'a str,
    ) -> Self {
        let entry = trace
            .recording
            .invocation()
            .map(|invocation| invocation.entry.unit);
        let meaning = match (lowered, tape, entry) {
            (Some(lowered), Some(tape), Some(unit))
                if core::ptr::eq(trace.recording.program(), Arc::as_ptr(&tape.program)) =>
            {
                Some(ProgramMeaning {
                    unit,
                    lowered,
                    functions: &tape.functions,
                    source,
                })
            }
            _ => None,
        };
        Self { trace, meaning }
    }

    /// Returns the underlying trace.
    #[must_use]
    pub fn trace(&self) -> &'a ExecutionTrace {
        self.trace
    }

    /// Returns whether function ids and spans resolve to Interloko artifacts.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.meaning.is_some()
    }

    /// Resolves a unit-qualified tape function.
    #[must_use]
    pub fn function(&self, id: FuncRef) -> TracedFunction<'a> {
        let lowered = self.meaning.and_then(|meaning| {
            if id.unit != meaning.unit {
                return None;
            }
            let index = meaning.functions.iter().position(|func| *func == id.func)?;
            meaning.lowered.functions.get(index)
        });
        TracedFunction { id, lowered }
    }

    /// Resolves a raw instruction location.
    #[must_use]
    pub fn location(&self, location: TraceLocation) -> TracedLocation<'a> {
        let source = self.meaning.and_then(|meaning| {
            if location.unit != meaning.unit {
                return None;
            }
            let offset = self.span_start(location)?;
            source_position(meaning.source, offset)
        });
        TracedLocation {
            function: self.function(FuncRef::new(location.unit, location.func)),
            pc: location.pc,
            source,
        }
    }

    /// Returns the requested entry function, when the invocation was captured.
    #[must_use]
    pub fn entry(&self) -> Option<TracedFunction<'a>> {
        let recording = self.trace.recording.as_ref();
        recording
            .invocation()
            .map(|invocation| self.function(invocation.entry))
    }

    /// Returns the captured entry arguments.
    #[must_use]
    pub fn arguments(&self) -> &'a [RecordedValue] {
        let recording = self.trace.recording.as_ref();
        recording
            .invocation()
            .map_or(&[], |invocation| invocation.args)
    }

    /// Returns how many guest activations were entered and retained.
    #[must_use]
    pub fn activation_count(&self) -> usize {
        self.trace.recording.stats().call_frames
    }

    /// Iterates recorded host calls in execution order.
    pub fn host_calls(&self) -> impl ExactSizeIterator<Item = TracedHostCall<'a>> + 'a {
        let view = *self;
        let recording = self.trace.recording.as_ref();
        recording.host_calls().map(move |call| TracedHostCall {
            sequence: call.sequence,
            location: view.location(call.location),
            symbol: recording.unit(call.location.unit).and_then(|unit| {
                let program = unit.program.program();
                let sig = program.host_sig(call.import)?;
                program.symbol_str(sig.symbol).ok()
            }),
            arguments: call.args,
            outcome: call.outcome,
            stack: TracedStack {
                frames: call.stack,
                view,
            },
        })
    }

    /// Iterates retained instruction attempts in execution order. Empty
    /// unless [`ExecutionTraceDetail::Instructions`] was requested; see
    /// [`Recording::instruction_status`] for truncation.
    pub fn instructions(&self) -> impl ExactSizeIterator<Item = TracedInstruction<'a>> + 'a {
        let view = *self;
        let recording = self.trace.recording.as_ref();
        recording
            .instructions()
            .enumerate()
            .map(move |(index, step)| TracedInstruction {
                index,
                location: view.location(step.location),
                opcode: step.opcode,
                next: step.next.map(|next| view.location(next)),
                outcome: step.outcome,
                stack: TracedStack {
                    frames: step.stack,
                    view,
                },
            })
    }

    /// Returns the final return or trap, when it was retained.
    #[must_use]
    pub fn outcome(&self) -> Option<TracedOutcome<'a>> {
        let recording = self.trace.recording.as_ref();
        Some(TracedOutcome {
            result: recording.outcome()?,
            location: recording
                .outcome_location()
                .map(|location| self.location(location)),
            stack: TracedStack {
                frames: recording.outcome_stack(),
                view: *self,
            },
        })
    }

    /// Renders the trace as stable, line-oriented text.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = self.write_text(&mut out);
        out
    }

    fn write_text(&self, out: &mut String) -> fmt::Result {
        let recording = self.trace.recording.as_ref();
        writeln!(out, "execution trace: {}", self.trace.detail.label())?;
        match recording.status() {
            Ok(()) => writeln!(out, "  capture: complete")?,
            Err(fault) => {
                write!(out, "  capture: {fault}")?;
                if let Some(location) = recording.fault_location() {
                    write!(out, " at {}", self.location(location))?;
                }
                writeln!(out)?;
            }
        }
        match self.entry() {
            Some(entry) => {
                write!(out, "  invocation: {}", self.function_label(entry))?;
                write!(out, " args=")?;
                self.write_values(out, self.arguments())?;
                writeln!(out)?;
            }
            None => writeln!(out, "  invocation: not captured")?,
        }
        writeln!(out, "  activations: {} entered", self.activation_count())?;
        writeln!(out, "  host calls: {}", recording.stats().host_calls)?;
        for call in self.host_calls() {
            write!(
                out,
                "    {:04} host-call {} args=",
                call.sequence,
                call.symbol.unwrap_or("<unnamed>")
            )?;
            self.write_values(out, call.arguments)?;
            match &call.outcome {
                HostOutcome::Returned { values, .. } => {
                    write!(out, " returned ")?;
                    self.write_values(out, values)?;
                }
                HostOutcome::Failed(error) => write!(out, " failed: {error}")?,
                HostOutcome::Threw { tag, payload } => {
                    match recording.tag_name(*tag) {
                        Some(name) => write!(out, " threw {name} ")?,
                        None => write!(out, " threw tag#{} ", tag.0)?,
                    }
                    self.write_values(out, payload)?;
                }
            }
            write!(out, " ")?;
            self.write_stack(out, call.location, &call.stack)?;
            writeln!(out)?;
        }
        if let Some(status) = recording.instruction_status() {
            write!(out, "  instructions: {}", recording.stats().instructions)?;
            match status {
                InstructionStatus::Complete => writeln!(out, " complete")?,
                InstructionStatus::Truncated { limit } => {
                    writeln!(out, " retained, truncated at limit {limit}")?;
                }
                InstructionStatus::Incomplete => writeln!(out, " retained, incomplete")?,
            }
            for step in self.instructions() {
                write!(
                    out,
                    "    {:04} instr {} depth={} {}",
                    step.index,
                    step.location,
                    step.stack
                        .depth()
                        .map_or_else(|| String::from("?"), |depth| alloc::format!("{depth}")),
                    step.mnemonic()
                )?;
                if let Some(next) = step.next {
                    write!(out, " -> {next}")?;
                }
                match step.outcome {
                    InstructionOutcome::Completed => {}
                    InstructionOutcome::Pending => write!(out, " pending")?,
                    InstructionOutcome::Trapped => write!(out, " trapped")?,
                    InstructionOutcome::Caught { depth } => {
                        write!(out, " caught at depth {depth}")?;
                    }
                }
                writeln!(out)?;
            }
        }
        match self.outcome() {
            Some(outcome) => {
                write!(out, "  outcome: ")?;
                match outcome.result {
                    Outcome::Returned(values) => {
                        write!(out, "returned ")?;
                        self.write_values(out, values)?;
                    }
                    Outcome::Trapped(trap) => write!(out, "trapped: {}", trap.trap)?,
                }
                if let Some(location) = outcome.location {
                    write!(out, " ")?;
                    self.write_stack(out, location, &outcome.stack)?;
                }
                writeln!(out)?;
            }
            None => writeln!(out, "  outcome: not captured")?,
        }
        Ok(())
    }

    fn function_label(&self, function: TracedFunction<'_>) -> String {
        match function.object_id() {
            Some(object) => alloc::format!("{function} [{object}]"),
            None => alloc::format!("{function}"),
        }
    }

    fn write_stack(
        &self,
        out: &mut String,
        location: TracedLocation<'_>,
        stack: &TracedStack<'_>,
    ) -> fmt::Result {
        write!(out, "at {location}")?;
        // Consecutive activations suspended at one call site (recursion)
        // render once with a repeat count.
        let mut groups = 0;
        let mut written = 0;
        let mut callers = stack.locations().skip(1).peekable();
        while let Some(caller) = callers.next() {
            if groups == RENDERED_STACK_FRAMES {
                let hidden = stack.depth().unwrap_or(1) - 1 - written;
                write!(out, " from {hidden} more frames")?;
                break;
            }
            let mut repeats = 1;
            while callers.next_if(|next| next.raw() == caller.raw()).is_some() {
                repeats += 1;
            }
            write!(out, " from {caller}")?;
            if repeats > 1 {
                write!(out, " x{repeats}")?;
            }
            groups += 1;
            written += repeats;
        }
        Ok(())
    }

    fn write_values(&self, out: &mut String, values: &[RecordedValue]) -> fmt::Result {
        out.push('(');
        for (index, value) in values.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            self.write_value(out, value)?;
        }
        out.push(')');
        Ok(())
    }

    fn write_value(&self, out: &mut String, value: &RecordedValue) -> fmt::Result {
        match value {
            RecordedValue::Unit => out.push_str("unit"),
            RecordedValue::Bool(true) => out.push_str("#t"),
            RecordedValue::Bool(false) => out.push_str("#f"),
            RecordedValue::I64(value) => write!(out, "{value}")?,
            RecordedValue::U64(value) => write!(out, "{value}u")?,
            RecordedValue::F64(bits) => write!(out, "{:?}", f64::from_bits(*bits))?,
            RecordedValue::Decimal(value) => write!(out, "{value:?}")?,
            RecordedValue::Bytes(bytes) => write!(out, "#bytes[{}]", bytes.len())?,
            RecordedValue::Str(text) => write!(out, "{text:?}")?,
            RecordedValue::Obj(object) => write!(out, "obj#{}", object.handle.0)?,
            RecordedValue::Agg(id) => write!(out, "agg#{}", id.get())?,
            RecordedValue::Func(func) => write!(out, "function {}", self.function(*func))?,
            RecordedValue::Closure { func, env } => {
                write!(
                    out,
                    "closure {} env=agg#{}",
                    self.function(*func),
                    env.get()
                )?;
            }
        }
        Ok(())
    }

    /// Finds the start offset of the source span covering `location`.
    fn span_start(&self, location: TraceLocation) -> Option<usize> {
        let unit = self.trace.recording.unit(location.unit)?;
        let program = unit.program.program();
        let function = program.functions.get(location.func.0 as usize)?;
        let mut pc = 0_u64;
        let mut current = None;
        for entry in function.spans(program).ok()? {
            pc = pc.saturating_add(entry.pc_delta);
            if pc > u64::from(location.pc) {
                break;
            }
            current = Some(entry.span_id);
        }
        tape::span_start(current?)
    }
}

/// Converts a byte offset to a one-based line and character column.
fn source_position(source: &str, offset: usize) -> Option<SourcePosition> {
    let prefix = source.get(..offset)?;
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    Some(SourcePosition {
        offset,
        line: prefix.matches('\n').count() + 1,
        column: prefix[line_start..].chars().count() + 1,
    })
}

#[cfg(test)]
mod tests {
    use super::source_position;

    #[test]
    fn source_positions_count_lines_and_characters() {
        let source = "ab\nçd\n";
        assert_eq!(
            source_position(source, 0).map(|p| (p.line, p.column)),
            Some((1, 1))
        );
        assert_eq!(
            source_position(source, 5).map(|p| (p.line, p.column)),
            Some((2, 2))
        );
        assert!(source_position(source, 4).is_none());
        assert!(source_position(source, 99).is_none());
    }
}
