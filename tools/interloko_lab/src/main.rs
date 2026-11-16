//! CLI workbench for Interloko compiler sessions.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::Path;

use interloko_compiler::annotation::AnnotationTarget;
use interloko_compiler::grammar_frontend::{GrammarEnvironmentSelection, GrammarFrontendRegistry};
use interloko_compiler::identity::SourceFileId;
use interloko_compiler::lower::CodegenPolicy;
use interloko_compiler::sem::{
    GenericFunctionId, HostClass, HostMetaRegistry, HostSlot, ValueKind,
};
use interloko_compiler::session::{
    AffordanceFacet, AnnotationOutcome, ArtifactFacet, CompilerSession, DocumentationFacet,
    DumpStage, EnvironmentAffordance, EnvironmentKind, EnvironmentObject, EnvironmentObjectId,
    EnvironmentObjectSink, EnvironmentOrigin, EnvironmentStorage, ExecutionTraceDetail,
    HostEnvironmentProvider, IncrementalCompilerSession, InspectionFacet, MutationError,
    MutationOutcome, PresentationFacet, RecomputeStrategy, RuntimeMode, RuntimeResult,
    SessionArtifacts, SessionMutation, SourceFacet,
};
use interloko_compiler::typing;
use interloko_diagnostics::{DiagnosticCatalog, DiagnosticRenderer};

mod report;

type Result<T> = std::result::Result<T, LabError>;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct TraceOptions {
    enabled: bool,
    include_instructions: bool,
}

impl TraceOptions {
    fn runtime_mode(self) -> RuntimeMode {
        if self.enabled {
            RuntimeMode::RunMainTraced {
                detail: if self.include_instructions {
                    ExecutionTraceDetail::Instructions
                } else {
                    ExecutionTraceDetail::Boundaries
                },
            }
        } else {
            RuntimeMode::RunMain
        }
    }
}

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let (args, codegen) = take_codegen_option(args)?;
    let Some(command) = args.first().map(String::as_str) else {
        return Err(LabError::Usage(usage()));
    };
    match command {
        "run" => command_run(&args, codegen),
        "trace" => command_trace(&args, codegen),
        "dump" => command_dump(&args, codegen),
        "mop" => command_mop(&args, codegen),
        "eop" => command_eop(&args, codegen),
        "registry" => command_registry(&args, codegen),
        "extensions" => command_extensions(&args, codegen),
        "dispatch" => command_dispatch(&args, codegen),
        "inspect" => command_inspect(&args, codegen),
        "affordances" => command_affordances(&args, codegen),
        "invoke-affordance" => command_invoke_affordance(&args, codegen),
        "annotate" => command_annotate(&args, codegen),
        "artifacts" => command_artifacts(&args, codegen),
        "report" => command_report(&args, codegen),
        "diff" => command_diff(&args, codegen),
        _ => Err(LabError::Usage(usage())),
    }
}

/// Removes `--codegen <policy>` (or `--codegen=<policy>`) from anywhere in
/// the arguments and returns the policy every compile of this command uses,
/// the compiler's default when the option is absent.
fn take_codegen_option(args: Vec<String>) -> Result<(Vec<String>, CodegenPolicy)> {
    let mut rest = Vec::with_capacity(args.len());
    let mut policy = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        let label = if arg == "--codegen" {
            iter.next().ok_or_else(|| {
                LabError::Usage(format!(
                    "option `--codegen` requires a policy: {}",
                    codegen_policy_labels()
                ))
            })?
        } else if let Some(label) = arg.strip_prefix("--codegen=") {
            label.to_string()
        } else {
            rest.push(arg);
            continue;
        };
        let parsed = CodegenPolicy::from_label(&label).ok_or_else(|| {
            LabError::Usage(format!(
                "unknown codegen policy `{label}`; expected {}",
                codegen_policy_labels()
            ))
        })?;
        if policy.replace(parsed).is_some() {
            return Err(LabError::Usage(
                "option `--codegen` may be given only once".into(),
            ));
        }
    }
    Ok((rest, policy.unwrap_or_default()))
}

fn codegen_policy_labels() -> String {
    CodegenPolicy::ALL
        .iter()
        .map(|policy| policy.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

fn command_run(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, trace) = run_args(args)?;
    let artifacts = compile_path(file, trace.runtime_mode(), codegen)?;
    print_diagnostics(&artifacts.diagnostics)?;
    match &artifacts.runtime_result {
        Some(RuntimeResult::Returned(value)) => {
            println!("{value}");
            if trace.enabled {
                print!("{}", artifacts.dump_execution_trace());
            }
            Ok(())
        }
        Some(result) => Err(LabError::Runtime(result.to_string())),
        None => Err(missing_runtime_error(&artifacts)),
    }
}

fn command_trace(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, trace) = trace_args(args)?;
    let artifacts = compile_path(file, trace.runtime_mode(), codegen)?;
    print_diagnostics(&artifacts.diagnostics)?;
    print!("{}", artifacts.dump_execution_trace());
    Ok(())
}

fn command_dump(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, stage) = dump_args(args)?;
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    print!("{}", workbench.dump_stage(stage));
    Ok(())
}

fn command_mop(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let file = single_file_arg(args)?;
    let artifacts = compile_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&artifacts.diagnostics)?;
    print!("{}", artifacts.dump_stage(DumpStage::Mop));
    Ok(())
}

fn command_eop(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let file = single_file_arg(args)?;
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    print!("{}", workbench.dump_eop());
    Ok(())
}

fn command_registry(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let file = single_file_arg(args)?;
    let artifacts = compile_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&artifacts.diagnostics)?;
    print!("{}", artifacts.dump_grammar_registry());
    Ok(())
}

fn command_extensions(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let file = single_file_arg(args)?;
    let artifacts = compile_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&artifacts.diagnostics)?;
    print!("{}", artifacts.dump_grammar_extensions());
    Ok(())
}

fn command_dispatch(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, dispatch_id) = dispatch_args(args)?;
    let artifacts = compile_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&artifacts.diagnostics)?;
    if let Some(dispatch_id) = dispatch_id {
        print!("{}", dispatch_explanation(&artifacts, dispatch_id)?);
    } else {
        print!("{}", artifacts.dump_dispatch_sites());
    }
    Ok(())
}

fn command_inspect(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, object_id) = object_args(args)?;
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    print!("{}", format_environment_object(&object));
    Ok(())
}

fn command_affordances(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, object_id) = object_args(args)?;
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    print!("{}", format_affordances(&object));
    Ok(())
}

fn command_invoke_affordance(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, object_id, affordance, extra) = invoke_args(args)?;
    invoke_affordance(file, object_id, affordance, extra, codegen)?;
    Ok(())
}

fn command_artifacts(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let file = single_file_arg(args)?;
    let artifacts = compile_path(file, RuntimeMode::CompileOnly, codegen)?;
    print!("{}", artifacts.dump_artifact_graph());
    Ok(())
}

fn command_report(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let (file, output) = report_args(args)?;
    let workbench = compile_workbench_path(
        file,
        RuntimeMode::RunMainTraced {
            detail: ExecutionTraceDetail::Boundaries,
        },
        codegen,
    )?;
    report::write_report(Path::new(output), &workbench).map_err(|error| LabError::Io {
        path: output.into(),
        error,
    })?;
    println!("wrote {output}");
    Ok(())
}

fn command_diff(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    if args.len() != 3 {
        return Err(LabError::Usage(
            "usage: interloko_lab diff <before.loko> <after.loko>".into(),
        ));
    }
    let source_file = SourceFileId::from_path(&args[1]);
    let before = read_path(&args[1])?;
    let after = read_path(&args[2])?;
    let grammar_registry = GrammarFrontendRegistry::builtin();
    let before_grammar =
        parse_grammar_environment(Path::new(&args[1]), &before, &grammar_registry)?;
    let after_grammar = parse_grammar_environment(Path::new(&args[2]), &after, &grammar_registry)?;
    let mut session = IncrementalCompilerSession::with_grammar_registry(grammar_registry);
    session.set_codegen_policy(codegen);
    session.add_source_with_id_and_grammar(source_file, &args[1], &before, before_grammar);
    let _ = session
        .compile(source_file, RuntimeMode::CompileOnly)
        .expect("source was just added");
    session
        .edit_source(source_file, &after)
        .expect("source was just added");
    session
        .set_grammar_environment(source_file, after_grammar)
        .expect("source was just added");
    let update = session
        .compile(source_file, RuntimeMode::CompileOnly)
        .expect("source was just added");
    print!("{}", update.dump_cache_report());
    Ok(())
}

fn compile_path(path: &str, mode: RuntimeMode, codegen: CodegenPolicy) -> Result<SessionArtifacts> {
    Ok(compile_workbench_path(path, mode, codegen)?.artifacts)
}

fn compile_workbench_path(
    path: &str,
    mode: RuntimeMode,
    codegen: CodegenPolicy,
) -> Result<CompiledWorkbench> {
    let source = read_path(path)?;
    let grammar_registry = GrammarFrontendRegistry::builtin();
    let grammar_environment =
        parse_grammar_environment(Path::new(path), &source, &grammar_registry)?;
    let host_registry = parse_host_registry(Path::new(path), &source)?;
    let host_environment = parse_host_environment(Path::new(path), &source)?;
    let mut session = CompilerSession::with_grammar_registry(grammar_registry);
    session.set_codegen_policy(codegen);
    let source_file = session.add_source_with_grammar(path, &source, grammar_environment);
    let artifacts = session
        .compile_with_host_registry(source_file, mode, &host_registry)
        .expect("source was just added");
    Ok(CompiledWorkbench {
        artifacts,
        host_environment,
    })
}

fn read_path(path: &str) -> Result<String> {
    let source = fs::read_to_string(path).map_err(|error| LabError::Io {
        path: path.into(),
        error,
    })?;
    Ok(source)
}

fn single_file_arg(args: &[String]) -> Result<&str> {
    if args.len() != 2 {
        return Err(LabError::Usage(format!(
            "usage: interloko_lab {} <file.loko>",
            args.first().map_or("COMMAND", String::as_str)
        )));
    }
    Ok(&args[1])
}

fn run_args(args: &[String]) -> Result<(&str, TraceOptions)> {
    let (file, trace) = file_with_trace_options(args, false)?;
    if file.is_empty() {
        return Err(LabError::Usage(
            "usage: interloko_lab run [--trace] [--instructions] <file.loko>".into(),
        ));
    }
    Ok((file, trace))
}

fn trace_args(args: &[String]) -> Result<(&str, TraceOptions)> {
    let (file, mut trace) = file_with_trace_options(args, true)?;
    if file.is_empty() {
        return Err(LabError::Usage(
            "usage: interloko_lab trace [--instructions] <file.loko>".into(),
        ));
    }
    trace.enabled = true;
    Ok((file, trace))
}

fn file_with_trace_options(args: &[String], trace_command: bool) -> Result<(&str, TraceOptions)> {
    let mut file = None;
    let mut trace = TraceOptions {
        enabled: trace_command,
        include_instructions: false,
    };
    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--trace" if !trace_command => trace.enabled = true,
            "--instructions" | "--trace-instr" => {
                trace.enabled = true;
                trace.include_instructions = true;
            }
            option if option.starts_with("--") => {
                return Err(LabError::Usage(format!(
                    "unknown trace option {option}; expected --trace, --instructions, or --trace-instr"
                )));
            }
            path => {
                if file.replace(path).is_some() {
                    return Err(LabError::Usage(format!(
                        "usage: interloko_lab {} [--trace] [--instructions] <file.loko>",
                        args.first().map_or("COMMAND", String::as_str)
                    )));
                }
            }
        }
    }
    Ok((file.unwrap_or(""), trace))
}

fn dump_args(args: &[String]) -> Result<(&str, DumpStage)> {
    if args.len() != 4 || args[2] != "--stage" {
        return Err(LabError::Usage(
            "usage: interloko_lab dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|types|all"
                .into(),
        ));
    }
    let stage = match args[3].as_str() {
        "syntax" => DumpStage::Syntax,
        "ast" => DumpStage::Ast,
        "sem" => DumpStage::Sem,
        "mop" => DumpStage::Mop,
        "eop" => DumpStage::Eop,
        "lowered" => DumpStage::Lowered,
        "tape" => DumpStage::Tape,
        "typing" => DumpStage::Typing,
        "types" => DumpStage::Types,
        "all" => DumpStage::All,
        _ => {
            return Err(LabError::Usage(
                "stage must be one of syntax|ast|sem|mop|eop|lowered|tape|typing|types|all".into(),
            ));
        }
    };
    Ok((&args[1], stage))
}

fn object_args(args: &[String]) -> Result<(&str, &str)> {
    if args.len() != 3 {
        return Err(LabError::Usage(format!(
            "usage: interloko_lab {} <file.loko> <object-id>",
            args.first().map_or("COMMAND", String::as_str)
        )));
    }
    Ok((&args[1], &args[2]))
}

fn dispatch_args(args: &[String]) -> Result<(&str, Option<&str>)> {
    match args.len() {
        2 => Ok((&args[1], None)),
        3 => Ok((&args[1], Some(args[2].as_str()))),
        _ => Err(LabError::Usage(
            "usage: interloko_lab dispatch <file.loko> [dispatch-site-id]".into(),
        )),
    }
}

fn report_args(args: &[String]) -> Result<(&str, &str)> {
    if args.len() != 4 || args[2] != "--output" {
        return Err(LabError::Usage(
            "usage: interloko_lab report <file.loko> --output <report.html>".into(),
        ));
    }
    Ok((&args[1], &args[3]))
}

fn invoke_args(args: &[String]) -> Result<(&str, &str, EnvironmentAffordance, &[String])> {
    if args.len() < 4 {
        return Err(LabError::Usage(
            "usage: interloko_lab invoke-affordance <file.loko> <object-id> <affordance> [affordance-args...]".into(),
        ));
    }
    let affordance = parse_affordance(&args[3]).ok_or_else(|| LabError::UnknownAffordance {
        name: args[3].clone(),
    })?;
    Ok((&args[1], &args[2], affordance, &args[4..]))
}

fn parse_affordance(input: &str) -> Option<EnvironmentAffordance> {
    EnvironmentAffordance::from_name(input)
}

#[derive(Clone, Debug, Default)]
struct WorkbenchHostEnvironment {
    objects: Vec<EnvironmentObject>,
}

impl WorkbenchHostEnvironment {
    fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    fn add(&mut self, object: EnvironmentObject) {
        self.objects.push(object);
    }
}

impl HostEnvironmentProvider for WorkbenchHostEnvironment {
    fn project_environment(&self, sink: &mut dyn EnvironmentObjectSink) {
        for object in &self.objects {
            sink.emit(object.clone());
        }
    }
}

#[derive(Clone, Debug)]
struct CompiledWorkbench {
    artifacts: SessionArtifacts,
    host_environment: WorkbenchHostEnvironment,
}

impl CompiledWorkbench {
    fn environment_objects(&self) -> Vec<EnvironmentObject> {
        if self.host_environment.is_empty() {
            self.artifacts.environment_objects()
        } else {
            self.artifacts
                .environment_objects_with_host(&self.host_environment)
        }
    }

    fn object(&self, object_id: &str) -> Result<EnvironmentObject> {
        let objects = self.environment_objects();
        objects
            .iter()
            .find(|object| object.id.as_str() == object_id)
            .cloned()
            .ok_or_else(|| LabError::UnknownObject {
                id: object_id.into(),
                available: objects
                    .iter()
                    .map(|object| object.id.as_str().to_string())
                    .collect(),
            })
    }

    fn dump_stage(&self, stage: DumpStage) -> String {
        match stage {
            DumpStage::Eop => self.dump_eop(),
            DumpStage::All => self.dump_all(),
            _ => self.artifacts.dump_stage(stage),
        }
    }

    fn dump_all(&self) -> String {
        let mut out = String::new();
        for stage in [
            DumpStage::Syntax,
            DumpStage::Ast,
            DumpStage::Sem,
            DumpStage::Mop,
            DumpStage::Eop,
            DumpStage::Lowered,
            DumpStage::Tape,
            DumpStage::Typing,
            DumpStage::Types,
        ] {
            let _ = writeln!(out, "== {stage:?} ==");
            out.push_str(&self.dump_stage(stage));
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }

    fn dump_eop(&self) -> String {
        render_eop(&self.artifacts, &self.environment_objects())
    }
}

fn render_eop(artifacts: &SessionArtifacts, objects: &[EnvironmentObject]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "eop {} {}", artifacts.source.id, artifacts.source.name);
    let _ = writeln!(out, "objects:");
    for object in objects {
        let _ = write!(out, "{}", render_eop_object(object));
    }
    out
}

fn render_eop_object(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  object {}", object.id);
    let _ = writeln!(
        out,
        "    kind={} class={} origin={} storage={}",
        object.kind.as_str(),
        object.meta_class,
        object.origin.as_str(),
        object.storage.as_str()
    );
    let _ = writeln!(
        out,
        "    presentation title=\"{}\"",
        object.presentation.title
    );
    let _ = writeln!(out, "    documentation {}", documentation_summary(object));
    let _ = writeln!(out, "    affordances {}", affordance_csv(object));
    if let Some(execution) = &object.execution {
        let _ = writeln!(out, "    execution {}", execution.detail);
    }
    let _ = writeln!(out, "    inspection {}", object.inspection.detail);
    out
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
    write_documentation_lines(&mut out, object, "  ");
    let _ = writeln!(out, "artifacts:");
    if object.artifacts.keys.is_empty() {
        let _ = writeln!(out, "  <none>");
    } else {
        for key in &object.artifacts.keys {
            let _ = writeln!(out, "  {key}");
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

fn format_affordances(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "affordances {}", object.id);
    for affordance in &object.affordances.names {
        let _ = writeln!(out, "{}", affordance.as_str());
    }
    out
}

fn format_documentation(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "documentation {}", object.id);
    write_documentation_lines(&mut out, object, "");
    out
}

fn write_documentation_lines(out: &mut String, object: &EnvironmentObject, indent: &str) {
    if object.documentation.lines.is_empty() {
        let _ = writeln!(out, "{indent}<none>");
    } else {
        for line in &object.documentation.lines {
            let _ = writeln!(out, "{indent}{line}");
        }
    }
}

fn documentation_summary(object: &EnvironmentObject) -> String {
    if object.documentation.lines.is_empty() {
        "[]".into()
    } else {
        format!("[{}]", object.documentation.lines.join(" | "))
    }
}

fn affordance_csv(object: &EnvironmentObject) -> String {
    let mut out = String::new();
    for (index, affordance) in object.affordances.names.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(affordance.as_str());
    }
    out
}

fn invoke_affordance(
    file: &str,
    object_id: &str,
    affordance: EnvironmentAffordance,
    extra: &[String],
    codegen: CodegenPolicy,
) -> Result<()> {
    if affordance == EnvironmentAffordance::Run {
        if !extra.is_empty() {
            return Err(LabError::Usage(format!(
                "affordance `{}` does not accept extra arguments",
                affordance.as_str()
            )));
        }
        let workbench = compile_workbench_path(file, RuntimeMode::RunMain, codegen)?;
        print_diagnostics(&workbench.artifacts.diagnostics)?;
        let object = workbench.object(object_id)?;
        ensure_object_has_affordance(&object, affordance)?;
        return print_runtime_result(&workbench.artifacts);
    }

    if affordance == EnvironmentAffordance::AddMethod {
        let source_path = parse_add_method_args(extra)?;
        return invoke_add_method(file, object_id, source_path, codegen);
    }

    if matches!(
        affordance,
        EnvironmentAffordance::Annotate | EnvironmentAffordance::PreviewAnnotation
    ) {
        let type_source = parse_annotate_args(affordance, extra)?;
        let dry_run = affordance == EnvironmentAffordance::PreviewAnnotation;
        return invoke_annotate(file, object_id, &type_source, dry_run, codegen);
    }

    if matches!(
        affordance,
        EnvironmentAffordance::SetTypingPolicy | EnvironmentAffordance::SetCodegenPolicy
    ) {
        return invoke_set_policy(file, object_id, affordance, extra, codegen);
    }

    if matches!(
        affordance,
        EnvironmentAffordance::PreviewFix | EnvironmentAffordance::ApplyFix
    ) {
        let index = parse_fix_args(affordance, extra)?;
        return invoke_fix(file, object_id, affordance, index, codegen);
    }

    if !extra.is_empty() {
        return Err(LabError::Usage(format!(
            "affordance `{}` does not accept extra arguments",
            affordance.as_str()
        )));
    }

    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    ensure_object_has_affordance(&object, affordance)?;
    match affordance {
        EnvironmentAffordance::DumpSyntax => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Syntax)?;
            Ok(())
        }
        EnvironmentAffordance::DumpAst => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Ast)?;
            Ok(())
        }
        EnvironmentAffordance::DumpSem => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Sem)?;
            Ok(())
        }
        EnvironmentAffordance::DumpMop => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Mop)?;
            Ok(())
        }
        EnvironmentAffordance::DumpEop => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Eop)?;
            Ok(())
        }
        EnvironmentAffordance::DumpLowered => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Lowered)?;
            Ok(())
        }
        EnvironmentAffordance::DumpTape => {
            print_invoked_object_stage(&workbench, object.id.as_str(), DumpStage::Tape)?;
            Ok(())
        }
        EnvironmentAffordance::ShowSlot => print_linked_object(
            &workbench,
            object.id.as_str(),
            "accessor:",
            "slot:",
            affordance,
        ),
        EnvironmentAffordance::ShowAccessor => print_linked_object(
            &workbench,
            object.id.as_str(),
            "slot:",
            "accessor:",
            affordance,
        ),
        EnvironmentAffordance::ShowDispatch => {
            if object.id.as_str().starts_with("dispatch:") {
                print!(
                    "{}",
                    dispatch_explanation(&workbench.artifacts, object.id.as_str())?
                );
            } else {
                print!("{}", workbench.artifacts.dump_mop_dispatch_table());
            }
            Ok(())
        }
        EnvironmentAffordance::ShowRuntimeLayout => {
            print!("{}", workbench.artifacts.dump_mop_runtime_layout());
            Ok(())
        }
        EnvironmentAffordance::ShowDependencies => {
            print!("{}", workbench.artifacts.dump_artifact_graph());
            Ok(())
        }
        EnvironmentAffordance::ShowDocumentation => {
            print!("{}", format_documentation(&object));
            Ok(())
        }
        EnvironmentAffordance::ShowGrammarStack => {
            print!("{}", workbench.artifacts.dump_grammar_stack());
            Ok(())
        }
        EnvironmentAffordance::ShowGrammars => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_environment(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowDirectRules => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_direct_grammar_rules(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowEffectiveRules => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_effective_grammar_rules(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowActivations => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_activations(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowGrammarRule => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_rule(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowExpression => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_expression(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowAction => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_action(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowCaptures => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_action_captures(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowActionOutput => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_action_output(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowParseTrace => {
            print!("{}", workbench.artifacts.dump_parse_trace());
            Ok(())
        }
        EnvironmentAffordance::ShowParserShadow => {
            print!("{}", workbench.artifacts.dump_parser_shadow_report());
            Ok(())
        }
        EnvironmentAffordance::ShowGrammarRegistry => {
            print!("{}", workbench.artifacts.dump_grammar_registry());
            Ok(())
        }
        EnvironmentAffordance::ShowGrammarExtension => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_grammar_extension(object.id.as_str())
                    .unwrap_or_else(|| workbench.artifacts.dump_grammar_extensions())
            );
            Ok(())
        }
        EnvironmentAffordance::ShowTyping => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_typing(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowTypes => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_types(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::ShowDependents => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .explain_contract_dependents(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::PreviewStrict => {
            print!(
                "{}",
                workbench
                    .artifacts
                    .preview_strict(object.id.as_str())
                    .ok_or_else(|| LabError::UnsupportedAffordance {
                        name: affordance.as_str().into(),
                    })?
            );
            Ok(())
        }
        EnvironmentAffordance::Run => unreachable!("run handled before compile-only path"),
        _ => Err(LabError::UnsupportedAffordance {
            name: affordance.as_str().into(),
        }),
    }
}

fn dispatch_explanation(artifacts: &SessionArtifacts, dispatch_id: &str) -> Result<String> {
    artifacts
        .explain_dispatch_site(dispatch_id)
        .ok_or_else(|| LabError::UnknownDispatchSite {
            id: dispatch_id.into(),
            available: artifacts
                .dispatch_sites()
                .iter()
                .map(|site| site.id.clone())
                .collect(),
        })
}

fn print_invoked_object_stage(
    workbench: &CompiledWorkbench,
    object_id: &str,
    stage: DumpStage,
) -> Result<()> {
    let dump = workbench
        .artifacts
        .dump_environment_object_stage(object_id, stage)
        .ok_or_else(|| LabError::UnknownObject {
            id: object_id.into(),
            available: workbench
                .environment_objects()
                .iter()
                .map(|object| object.id.as_str().to_string())
                .collect(),
        })?;
    print!("{dump}");
    Ok(())
}

fn parse_add_method_args(extra: &[String]) -> Result<&str> {
    let mut source = None;
    let mut iter = extra.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--source" => {
                let value = iter.next().ok_or_else(|| {
                    LabError::Usage(
                        "affordance `add-method` flag `--source` requires a path argument".into(),
                    )
                })?;
                if source.replace(value.as_str()).is_some() {
                    return Err(LabError::Usage(
                        "affordance `add-method` accepts only one `--source` argument".into(),
                    ));
                }
            }
            other => {
                return Err(LabError::Usage(format!(
                    "affordance `add-method` got unexpected argument `{other}`; expected `--source <path>`"
                )));
            }
        }
    }
    source.ok_or_else(|| {
        LabError::Usage(
            "affordance `add-method` requires `--source <path>` naming a Loko `define method ... end;` file".into(),
        )
    })
}

fn invoke_add_method(
    file: &str,
    object_id: &str,
    method_source_path: &str,
    codegen: CodegenPolicy,
) -> Result<()> {
    let mut workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    ensure_object_has_affordance(&object, EnvironmentAffordance::AddMethod)?;
    let generic = parse_generic_object_id(object_id).ok_or_else(|| {
        LabError::Usage(format!(
            "affordance `add-method` requires a generic:gN object id, got `{object_id}`"
        ))
    })?;

    let method_source = read_path(method_source_path)?;

    let outcome = workbench
        .artifacts
        .apply_mutation(SessionMutation::AddMethod {
            generic,
            method_source,
        })
        .map_err(|error| mutation_failed("add-method", &error))?;

    print_mutation_outcome(&outcome, workbench.artifacts.mutations().len());
    Ok(())
}

/// The fix index `preview-fix` and `apply-fix` take: none for the first
/// fix, or one number counting from zero.
fn parse_fix_args(affordance: EnvironmentAffordance, extra: &[String]) -> Result<usize> {
    match extra {
        [] => Ok(0),
        [index] => index.parse().map_err(|_| {
            LabError::Usage(format!(
                "affordance `{}` takes an optional fix index, got `{index}`",
                affordance.as_str()
            ))
        }),
        _ => Err(LabError::Usage(format!(
            "affordance `{}` takes an optional fix index",
            affordance.as_str()
        ))),
    }
}

/// `preview-fix [n]` and `apply-fix [n]` on a strict error's
/// `diagnostic:d<k>`: the annotation its `n`th fix writes, previewed or
/// applied as `preview-annotation` and `annotate` would.
fn invoke_fix(
    file: &str,
    object_id: &str,
    affordance: EnvironmentAffordance,
    index: usize,
    codegen: CodegenPolicy,
) -> Result<()> {
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    ensure_object_has_affordance(&object, affordance)?;
    let (target, type_source) = workbench
        .artifacts
        .strict_fix_annotation(object_id, index)
        .ok_or_else(|| {
            LabError::Usage(format!(
                "fix {index} of `{object_id}` is not one an annotation makes"
            ))
        })?;
    println!("fix {index} of {object_id}: annotate {target} {type_source}");
    let dry_run = affordance == EnvironmentAffordance::PreviewFix;
    annotate_target(workbench, target, &type_source, dry_run)
}

fn parse_annotate_args(affordance: EnvironmentAffordance, extra: &[String]) -> Result<String> {
    match extra {
        [flag, type_source] if flag == "--type" => Ok(type_source.clone()),
        _ => Err(LabError::Usage(format!(
            "affordance `{}` requires `--type <type>`, such as `--type '<integer>'`",
            affordance.as_str()
        ))),
    }
}

/// `annotate <file.loko> <target> <type...> [--dry-run]`: the `annotate`
/// affordance, or `preview-annotation` with `--dry-run`, as a command.
fn command_annotate(args: &[String], codegen: CodegenPolicy) -> Result<()> {
    let usage = || {
        LabError::Usage(
            "usage: interloko_lab annotate <file.loko> <binding:...|result:...> <type> [--dry-run]"
                .into(),
        )
    };
    let mut dry_run = false;
    let mut positional = Vec::new();
    for arg in args.iter().skip(1) {
        if arg == "--dry-run" {
            dry_run = true;
        } else {
            positional.push(arg.as_str());
        }
    }
    let [file, target, type_words @ ..] = positional.as_slice() else {
        return Err(usage());
    };
    if type_words.is_empty() {
        return Err(usage());
    }
    invoke_annotate(file, target, &type_words.join(" "), dry_run, codegen)
}

fn invoke_annotate(
    file: &str,
    object_id: &str,
    type_source: &str,
    dry_run: bool,
    codegen: CodegenPolicy,
) -> Result<()> {
    let affordance = if dry_run {
        EnvironmentAffordance::PreviewAnnotation
    } else {
        EnvironmentAffordance::Annotate
    };
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    ensure_object_has_affordance(&object, affordance)?;
    let target = AnnotationTarget::parse(object_id).ok_or_else(|| {
        LabError::Usage(format!(
            "affordance `{}` requires a binding:... or result:... object id, got `{object_id}`",
            affordance.as_str()
        ))
    })?;
    annotate_target(workbench, target, type_source, dry_run)
}

/// Previews or applies annotating `target` with `type_source`, printing
/// the outcome and its typing delta.
fn annotate_target(
    mut workbench: CompiledWorkbench,
    target: AnnotationTarget,
    type_source: &str,
    dry_run: bool,
) -> Result<()> {
    let failed = |error: MutationError| mutation_failed("annotate", &error);
    if dry_run {
        let preview = workbench
            .artifacts
            .preview_annotation(&target, type_source)
            .map_err(failed)?;
        println!(
            "preview annotate {} (not applied)",
            annotation_summary(&preview)
        );
        print!("{}", preview.delta.render());
        return Ok(());
    }
    let outcome = workbench
        .artifacts
        .apply_mutation(SessionMutation::Annotate {
            target,
            type_source: type_source.into(),
        })
        .map_err(failed)?;
    print_mutation_outcome(&outcome, workbench.artifacts.mutations().len());
    if let Some(annotation) = outcome.annotation() {
        print!("{}", annotation.delta.render());
    }
    Ok(())
}

fn annotation_summary(outcome: &AnnotationOutcome) -> String {
    let mut summary = format!(
        "target={} annotation={} previous={}",
        outcome.target,
        outcome.edit.text.trim_start_matches([' ', ':', '=', '>']),
        outcome.previous.as_deref().unwrap_or("none")
    );
    if outcome.annotated != outcome.target {
        let _ = write!(summary, " now={}", outcome.annotated);
    }
    summary
}

/// `set-typing-policy <spec...>` and `set-codegen-policy <policy>` on
/// `typing-config:session`. The lab is stateless, so the new policy lasts
/// for this invocation: it prints the change and the decisions it moves.
fn invoke_set_policy(
    file: &str,
    object_id: &str,
    affordance: EnvironmentAffordance,
    extra: &[String],
    codegen: CodegenPolicy,
) -> Result<()> {
    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly, codegen)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    ensure_object_has_affordance(&object, affordance)?;
    let before = &workbench.artifacts;
    let after = if affordance == EnvironmentAffordance::SetTypingPolicy {
        let config = before
            .typing
            .config
            .apply_spec(&extra.join(" "))
            .map_err(|error| LabError::Usage(format!("set-typing-policy: {error}")))?;
        println!(
            "typing policy {} -> {}",
            typing::config_label(before.typing.config),
            typing::config_label(config)
        );
        before.with_typing_config(config)
    } else {
        let [label] = extra else {
            return Err(LabError::Usage(format!(
                "affordance `set-codegen-policy` requires one policy: {}",
                codegen_policy_labels()
            )));
        };
        let policy = CodegenPolicy::from_label(label).ok_or_else(|| {
            LabError::Usage(format!(
                "unknown codegen policy `{label}`; expected {}",
                codegen_policy_labels()
            ))
        })?;
        println!(
            "codegen policy {} -> {}",
            before.codegen_policy().as_str(),
            policy.as_str()
        );
        before.with_codegen_policy(policy)
    };
    print!("{}", before.typing_delta(&after).render());
    print_diagnostics(
        &after
            .diagnostics
            .iter()
            .filter(|diagnostic| !before.diagnostics.contains(diagnostic))
            .cloned()
            .collect::<Vec<_>>(),
    )
}

fn parse_generic_object_id(id: &str) -> Option<GenericFunctionId> {
    let suffix = id.strip_prefix("generic:g")?;
    let n: u32 = suffix.parse().ok()?;
    Some(GenericFunctionId(n))
}

fn print_mutation_outcome(outcome: &MutationOutcome, log_size: usize) {
    let strategy = match outcome.recompute_strategy {
        RecomputeStrategy::FullCandidateRebuild => "full-candidate-rebuild",
        _ => "unknown-strategy",
    };
    let effect = match (outcome.inserted_method(), outcome.annotation()) {
        (Some(method), _) => format!(
            "add-method mutation_id={} inserted_method=m{}",
            outcome.mutation_id, method.0
        ),
        (None, Some(annotation)) => format!(
            "annotate mutation_id={} {}",
            outcome.mutation_id,
            annotation_summary(annotation)
        ),
        (None, None) => format!("mutation mutation_id={}", outcome.mutation_id),
    };
    println!(
        "applied {effect} affected_artifacts={} strategy={strategy} log_size={log_size}",
        outcome.affected_artifacts.len(),
    );
}

/// A refused mutation as the lab's error, after printing the diagnostics
/// it would have added, with their notes, as a compile prints them: an edit
/// that adds errors, a strict definition's included, is never applied.
fn mutation_failed(request: &'static str, error: &MutationError) -> LabError {
    if let MutationError::ParseFailed { diagnostics }
    | MutationError::SemanticFailed { diagnostics } = error
        && let Err(error) = print_diagnostics(diagnostics)
    {
        return error;
    }
    LabError::MutationFailed {
        request,
        summary: format_mutation_error(error),
    }
}

fn format_mutation_error(error: &MutationError) -> String {
    match error {
        MutationError::ParseFailed { diagnostics } => {
            format!("parse failure ({} new diagnostics)", diagnostics.len())
        }
        MutationError::NotAMethodDefinition => {
            "not a single `define method ... end;` clause".into()
        }
        MutationError::UnknownGeneric { generic } => format!("unknown generic id g{}", generic.0),
        MutationError::GenericNameMismatch {
            generic,
            expected,
            actual,
        } => format!(
            "method names generic `{actual}` but target g{} is `{expected}`",
            generic.0
        ),
        MutationError::SemanticFailed { diagnostics } => format!(
            "semantic failure ({} new diagnostics: {})",
            diagnostics.len(),
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        MutationError::DuplicateSpecializer {
            existing_method, ..
        } => format!(
            "specializer vector duplicates existing method m{}",
            existing_method.0
        ),
        MutationError::RuntimeValidationFailed { error } => {
            format!("runtime layout validation failed: {error:?}")
        }
        MutationError::SealedGeneric { generic } => {
            format!(
                "generic g{} is sealed: its methods are the ones its source defines",
                generic.0
            )
        }
        MutationError::UnknownAnnotationTarget { target } => {
            format!("no binding or result `{target}` in the current source")
        }
        MutationError::InvalidTypeSource { type_source } => {
            format!("`{type_source}` is not a type annotation")
        }
        _ => "unknown mutation failure".into(),
    }
}

fn ensure_object_has_affordance(
    object: &EnvironmentObject,
    affordance: EnvironmentAffordance,
) -> Result<()> {
    if object.affordances.contains(affordance) {
        Ok(())
    } else {
        Err(LabError::AffordanceUnavailable {
            id: object.id.as_str().into(),
            name: affordance.as_str().into(),
        })
    }
}

/// Explains a missing runtime result: compilation errors, already printed
/// as diagnostics with their notes (reasons and fixes), stop `main` from
/// running. Every error blocks, strict typing's included; the report names
/// each one's `diagnostic:d<n>` id, code, span and typing decision.
fn missing_runtime_error(artifacts: &SessionArtifacts) -> LabError {
    match artifacts.check_runnable() {
        Err(error) => LabError::RunBlocked(format!("main did not run: {error}")),
        Ok(()) => LabError::Runtime("runtime was not requested".into()),
    }
}

fn print_runtime_result(artifacts: &SessionArtifacts) -> Result<()> {
    match &artifacts.runtime_result {
        Some(RuntimeResult::Returned(value)) => {
            println!("{value}");
            Ok(())
        }
        Some(result) => Err(LabError::Runtime(result.to_string())),
        None => Err(missing_runtime_error(artifacts)),
    }
}

fn print_linked_object(
    workbench: &CompiledWorkbench,
    object_id: &str,
    from_prefix: &str,
    to_prefix: &str,
    affordance: EnvironmentAffordance,
) -> Result<()> {
    let suffix =
        object_id
            .strip_prefix(from_prefix)
            .ok_or_else(|| LabError::UnsupportedAffordance {
                name: affordance.as_str().into(),
            })?;
    let linked = workbench.object(&format!("{to_prefix}{suffix}"))?;
    print!("{}", format_environment_object(&linked));
    Ok(())
}

fn parse_grammar_environment(
    path: &Path,
    source: &str,
    registry: &GrammarFrontendRegistry,
) -> Result<GrammarEnvironmentSelection> {
    let mut selection = GrammarEnvironmentSelection::base_loko();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// grammar:") {
                selection = parse_grammar_selection(path, index + 1, rest.trim(), registry)?;
            }
            continue;
        }
        break;
    }
    Ok(selection)
}

fn parse_grammar_selection(
    path: &Path,
    line: usize,
    input: &str,
    registry: &GrammarFrontendRegistry,
) -> Result<GrammarEnvironmentSelection> {
    registry
        .environment_selection(input)
        .ok_or_else(|| LabError::InvalidGrammarHeader {
            path: path.display().to_string(),
            line,
            message: grammar_header_message(registry),
        })
}

fn grammar_header_message(registry: &GrammarFrontendRegistry) -> String {
    format!(
        "grammar must be one of {}",
        registry
            .environment_slugs()
            .into_iter()
            .map(|slug| format!("`{slug}`"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn parse_host_registry(path: &Path, source: &str) -> Result<HostMetaRegistry> {
    let mut host_registry = HostMetaRegistry::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// host-class:") {
                host_registry.add_class(parse_host_class(path, index + 1, rest.trim())?);
            }
            continue;
        }
        break;
    }
    Ok(host_registry)
}

fn parse_host_environment(path: &Path, source: &str) -> Result<WorkbenchHostEnvironment> {
    let mut environment = WorkbenchHostEnvironment::default();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// host-eop-class:") {
                environment.add(parse_host_eop_class(path, index + 1, rest.trim())?);
            } else if let Some(rest) = trimmed.strip_prefix("// host-eop-instance:") {
                environment.add(parse_host_eop_instance(path, index + 1, rest.trim())?);
            }
            continue;
        }
        break;
    }
    Ok(environment)
}

fn parse_host_class(path: &Path, line: usize, input: &str) -> Result<HostClass> {
    let mut parts = input.split_whitespace();
    let name = parts
        .next()
        .ok_or_else(|| invalid_header(path, line, "host-class requires a class name"))?;
    if !name.starts_with('<') || !name.ends_with('>') {
        return Err(invalid_header(
            path,
            line,
            "host-class name must be a Loko class name",
        ));
    }
    let mut class = HostClass::new(name);
    for part in parts {
        let Some((slot_name, type_name)) = part.split_once(':') else {
            return Err(invalid_header(
                path,
                line,
                "host-class slots must be `slot:<type>`",
            ));
        };
        let kind = parse_host_slot_kind(path, line, type_name)?;
        class = class.with_slot(HostSlot::new(slot_name, kind, type_name));
    }
    Ok(class)
}

fn parse_host_eop_class(path: &Path, line: usize, input: &str) -> Result<EnvironmentObject> {
    let mut parts = input.split_whitespace();
    let host_id = parts
        .next()
        .ok_or_else(|| invalid_host_eop_header(path, line, "host-eop-class requires an id"))?;
    let class_name = parts.next().ok_or_else(|| {
        invalid_host_eop_header(path, line, "host-eop-class requires a class name")
    })?;
    if !class_name.starts_with('<') || !class_name.ends_with('>') {
        return Err(invalid_host_eop_header(
            path,
            line,
            "host-eop-class class name must be a Loko class name",
        ));
    }
    if parts.next().is_some() {
        return Err(invalid_host_eop_header(
            path,
            line,
            "host-eop-class supports `id <class-name>`",
        ));
    }
    Ok(host_environment_object(
        format!("host-class:{host_id}"),
        "<host-class>",
        format!("host class {class_name}"),
        format!("provider=fixture host-id={host_id} class={class_name}"),
    ))
}

fn parse_host_eop_instance(path: &Path, line: usize, input: &str) -> Result<EnvironmentObject> {
    let mut parts = input.split_whitespace();
    let host_id = parts
        .next()
        .ok_or_else(|| invalid_host_eop_header(path, line, "host-eop-instance requires an id"))?;
    let class_name = parts.next().ok_or_else(|| {
        invalid_host_eop_header(path, line, "host-eop-instance requires a class name")
    })?;
    if !class_name.starts_with('<') || !class_name.ends_with('>') {
        return Err(invalid_host_eop_header(
            path,
            line,
            "host-eop-instance class name must be a Loko class name",
        ));
    }
    let facts = parts.collect::<Vec<_>>().join(" ");
    let inspection = if facts.is_empty() {
        format!("provider=fixture host-id={host_id} class={class_name}")
    } else {
        format!("provider=fixture host-id={host_id} class={class_name} facts={facts}")
    };
    Ok(host_environment_object(
        format!("host-instance:{host_id}"),
        "<host-instance>",
        format!("host instance {host_id} :: {class_name}"),
        inspection,
    ))
}

fn host_environment_object(
    id: String,
    meta_class: &str,
    title: String,
    inspection: String,
) -> EnvironmentObject {
    EnvironmentObject {
        id: EnvironmentObjectId::new(id),
        kind: EnvironmentKind::HostObject,
        meta_class: meta_class.into(),
        origin: EnvironmentOrigin::HostProvided,
        storage: EnvironmentStorage::HostOwned,
        presentation: PresentationFacet::new(title),
        source: SourceFacet::none(),
        documentation: DocumentationFacet::new(Vec::new()),
        artifacts: ArtifactFacet::new(Vec::new()),
        affordances: AffordanceFacet::new(vec![
            EnvironmentAffordance::Inspect,
            EnvironmentAffordance::DumpEop,
            EnvironmentAffordance::ShowRuntimeLayout,
        ]),
        execution: None,
        inspection: InspectionFacet::new(inspection),
        typing: None,
    }
}

fn parse_host_slot_kind(path: &Path, line: usize, input: &str) -> Result<ValueKind> {
    match input {
        "<object>" => Ok(ValueKind::Any),
        "<integer>" => Ok(ValueKind::I64),
        "<boolean>" => Ok(ValueKind::Bool),
        _ => Err(invalid_header(
            path,
            line,
            "host-class workbench slots currently support <object>, <integer>, or <boolean>",
        )),
    }
}

fn invalid_header(path: &Path, line: usize, message: &str) -> LabError {
    LabError::InvalidHeader {
        path: path.display().to_string(),
        line,
        message: message.into(),
    }
}

fn invalid_host_eop_header(path: &Path, line: usize, message: &str) -> LabError {
    LabError::InvalidHostEopHeader {
        path: path.display().to_string(),
        line,
        message: message.into(),
    }
}

fn diagnostic_renderer() -> Result<DiagnosticRenderer> {
    if let Ok(path) = env::var("INTERLOKO_DIAGNOSTIC_CATALOG") {
        let source = fs::read_to_string(&path).map_err(|error| LabError::Io {
            path: path.clone(),
            error,
        })?;
        let catalog = DiagnosticCatalog::from_toml_source(path, &source)
            .map_err(|error| LabError::DiagnosticCatalog(error.to_string()))?;
        return Ok(DiagnosticRenderer::new(catalog));
    }

    match env::var("INTERLOKO_DIAGNOSTIC_LOCALE")
        .unwrap_or_else(|_| "en".into())
        .as_str()
    {
        "en" => DiagnosticRenderer::english(),
        "pseudo" => DiagnosticRenderer::pseudo(),
        locale => {
            return Err(LabError::DiagnosticCatalog(format!(
                "unsupported diagnostic locale `{locale}`"
            )));
        }
    }
    .map_err(|error| LabError::DiagnosticCatalog(error.to_string()))
}

fn print_diagnostics(diagnostics: &[interloko_compiler::diagnostic::Diagnostic]) -> Result<()> {
    if diagnostics.is_empty() {
        return Ok(());
    }
    let renderer = diagnostic_renderer()?;
    eprintln!("diagnostics:");
    for diagnostic in diagnostics {
        eprintln!(
            "  {:?} {} {}..{} {}",
            diagnostic.severity,
            diagnostic.code,
            diagnostic.span.start,
            diagnostic.span.end,
            renderer
                .render(diagnostic)
                .map_err(|error| LabError::DiagnosticCatalog(error.to_string()))?
        );
        // Notes are facts, such as a strict error's reasons and fixes.
        for note in &diagnostic.notes {
            eprintln!("    {}", note.message);
        }
    }
    Ok(())
}

fn usage() -> String {
    let binary = env::args()
        .next()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "interloko_lab".into());
    format!(
        "usage:\n  {binary} [--codegen off|proved] COMMAND ...\n\ncommands:\n  {binary} run [--trace] [--instructions] <file.loko>\n  {binary} trace [--instructions] <file.loko>\n  {binary} dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|typing|types|all\n  {binary} mop <file.loko>\n  {binary} eop <file.loko>\n  {binary} registry <file.loko>\n  {binary} extensions <file.loko>\n  {binary} dispatch <file.loko> [dispatch-site-id]\n  {binary} inspect <file.loko> <object-id>\n  {binary} affordances <file.loko> <object-id>\n  {binary} invoke-affordance <file.loko> <object-id> <affordance> [affordance-args...]\n  {binary} invoke-affordance <file.loko> <generic:gN> add-method --source <method.loko>\n  {binary} invoke-affordance <file.loko> <binding:...|result:...> annotate|preview-annotation --type <type>\n  {binary} invoke-affordance <file.loko> typing-config:session set-typing-policy <preset|check.verdict=severity>...\n  {binary} invoke-affordance <file.loko> typing-config:session set-codegen-policy off|proved\n  {binary} invoke-affordance <file.loko> <function:...|method:...|generic:...|constant:...> preview-strict\n  {binary} invoke-affordance <file.loko> <diagnostic:dN> preview-fix|apply-fix [fix-index]\n  {binary} annotate <file.loko> <binding:...|result:...> <type> [--dry-run]\n  {binary} artifacts <file.loko>\n  {binary} report <file.loko> --output <report.html>\n  {binary} diff <before.loko> <after.loko>\n\noptions:\n  --codegen off|proved   codegen policy for every compile (default proved); may appear anywhere\n\ndiagnostics:\n  INTERLOKO_DIAGNOSTIC_LOCALE=en|pseudo\n  INTERLOKO_DIAGNOSTIC_CATALOG=path/to/catalog.mf2.toml"
    )
}

#[derive(Debug)]
enum LabError {
    Usage(String),
    Io {
        path: String,
        error: std::io::Error,
    },
    InvalidHeader {
        path: String,
        line: usize,
        message: String,
    },
    InvalidHostEopHeader {
        path: String,
        line: usize,
        message: String,
    },
    InvalidGrammarHeader {
        path: String,
        line: usize,
        message: String,
    },
    UnknownObject {
        id: String,
        available: Vec<String>,
    },
    UnknownDispatchSite {
        id: String,
        available: Vec<String>,
    },
    UnknownAffordance {
        name: String,
    },
    AffordanceUnavailable {
        id: String,
        name: String,
    },
    UnsupportedAffordance {
        name: String,
    },
    MutationFailed {
        request: &'static str,
        summary: String,
    },
    Runtime(String),
    /// Compilation errors kept `main` from running.
    RunBlocked(String),
    DiagnosticCatalog(String),
}

impl LabError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_)
            | Self::InvalidHeader { .. }
            | Self::InvalidGrammarHeader { .. }
            | Self::InvalidHostEopHeader { .. }
            | Self::UnknownObject { .. }
            | Self::UnknownDispatchSite { .. }
            | Self::UnknownAffordance { .. }
            | Self::AffordanceUnavailable { .. }
            | Self::UnsupportedAffordance { .. }
            | Self::MutationFailed { .. } => 2,
            Self::Io { .. }
            | Self::Runtime(_)
            | Self::RunBlocked(_)
            | Self::DiagnosticCatalog(_) => 1,
        }
    }
}

impl fmt::Display for LabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io { path, error } => write!(f, "failed to read {path}: {error}"),
            Self::InvalidHeader {
                path,
                line,
                message,
            } => {
                write!(f, "{path}:{line}: invalid host-class header: {message}")
            }
            Self::InvalidHostEopHeader {
                path,
                line,
                message,
            } => {
                write!(f, "{path}:{line}: invalid host-eop header: {message}")
            }
            Self::InvalidGrammarHeader {
                path,
                line,
                message,
            } => {
                write!(f, "{path}:{line}: invalid grammar header: {message}")
            }
            Self::UnknownObject { id, available } => {
                write!(f, "unknown object id `{id}`")?;
                if !available.is_empty() {
                    write!(f, "; available object ids: {}", available.join(", "))?;
                }
                Ok(())
            }
            Self::UnknownDispatchSite { id, available } => {
                write!(f, "unknown dispatch site `{id}`")?;
                if !available.is_empty() {
                    write!(f, "; available dispatch sites: {}", available.join(", "))?;
                }
                Ok(())
            }
            Self::UnknownAffordance { name } => write!(f, "unknown affordance `{name}`"),
            Self::AffordanceUnavailable { id, name } => {
                write!(f, "object `{id}` does not expose affordance `{name}`")
            }
            Self::UnsupportedAffordance { name } => {
                write!(
                    f,
                    "affordance `{name}` is not executable by interloko_lab in this slice"
                )
            }
            Self::MutationFailed { request, summary } => {
                write!(f, "{request} mutation failed: {summary}")
            }
            Self::Runtime(message) => write!(f, "runtime failed: {message}"),
            Self::RunBlocked(message) => f.write_str(message),
            Self::DiagnosticCatalog(message) => write!(f, "diagnostic catalog failed: {message}"),
        }
    }
}

impl Error for LabError {}
