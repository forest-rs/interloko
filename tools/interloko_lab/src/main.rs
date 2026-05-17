//! CLI workbench for Interloko compiler sessions.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::Path;

use interloko_compiler::grammar_frontend::{GrammarEnvironmentSelection, GrammarFrontendRegistry};
use interloko_compiler::identity::SourceFileId;
use interloko_compiler::sem::{
    GenericFunctionId, HostClass, HostMetaRegistry, HostSlot, ValueKind,
};
use interloko_compiler::session::{
    AffordanceFacet, ArtifactFacet, CompilerSession, DumpStage, EnvironmentAffordance,
    EnvironmentKind, EnvironmentObject, EnvironmentObjectId, EnvironmentObjectSink,
    EnvironmentOrigin, EnvironmentStorage, HostEnvironmentProvider, IncrementalCompilerSession,
    InspectionFacet, MutationError, MutationOutcome, PresentationFacet, RecomputeStrategy,
    RuntimeMode, RuntimeResult, SessionArtifacts, SessionMutation, SourceFacet,
    execution_trace_mask,
};
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
                mask: execution_trace_mask(self.include_instructions),
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
    let Some(command) = args.first().map(String::as_str) else {
        return Err(LabError::Usage(usage()));
    };
    match command {
        "run" => {
            let (file, trace) = run_args(&args)?;
            let artifacts = compile_path(file, trace.runtime_mode())?;
            print_diagnostics(&artifacts.diagnostics)?;
            match artifacts.runtime_result {
                Some(RuntimeResult::ReturnedI64(value)) => {
                    println!("{value}");
                    if trace.enabled {
                        print!("{}", artifacts.dump_execution_trace());
                    }
                    Ok(())
                }
                Some(result) => Err(LabError::Runtime(format!("{result:?}"))),
                None => Err(LabError::Runtime("runtime was not requested".into())),
            }
        }
        "trace" => {
            let (file, trace) = trace_args(&args)?;
            let artifacts = compile_path(file, trace.runtime_mode())?;
            print_diagnostics(&artifacts.diagnostics)?;
            print!("{}", artifacts.dump_execution_trace());
            Ok(())
        }
        "dump" => {
            let (file, stage) = dump_args(&args)?;
            let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&workbench.artifacts.diagnostics)?;
            print!("{}", workbench.dump_stage(stage));
            Ok(())
        }
        "mop" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics)?;
            print!("{}", artifacts.dump_stage(DumpStage::Mop));
            Ok(())
        }
        "eop" => {
            let file = single_file_arg(&args)?;
            let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&workbench.artifacts.diagnostics)?;
            print!("{}", workbench.dump_eop());
            Ok(())
        }
        "registry" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics)?;
            print!("{}", artifacts.dump_grammar_registry());
            Ok(())
        }
        "extensions" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics)?;
            print!("{}", artifacts.dump_grammar_extensions());
            Ok(())
        }
        "dispatch" => {
            let (file, dispatch_id) = dispatch_args(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics)?;
            if let Some(dispatch_id) = dispatch_id {
                print!("{}", dispatch_explanation(&artifacts, dispatch_id)?);
            } else {
                print!("{}", artifacts.dump_dispatch_sites());
            }
            Ok(())
        }
        "inspect" => {
            let (file, object_id) = object_args(&args)?;
            let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&workbench.artifacts.diagnostics)?;
            let object = workbench.object(object_id)?;
            print!("{}", format_environment_object(&object));
            Ok(())
        }
        "affordances" => {
            let (file, object_id) = object_args(&args)?;
            let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&workbench.artifacts.diagnostics)?;
            let object = workbench.object(object_id)?;
            print!("{}", format_affordances(&object));
            Ok(())
        }
        "invoke-affordance" => {
            let (file, object_id, affordance, extra) = invoke_args(&args)?;
            invoke_affordance(file, object_id, affordance, extra)?;
            Ok(())
        }
        "artifacts" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print!("{}", artifacts.dump_artifact_graph());
            Ok(())
        }
        "report" => {
            let (file, output) = report_args(&args)?;
            let workbench = compile_workbench_path(
                file,
                RuntimeMode::RunMainTraced {
                    mask: execution_trace_mask(false),
                },
            )?;
            report::write_report(Path::new(output), &workbench).map_err(|error| LabError::Io {
                path: output.into(),
                error,
            })?;
            println!("wrote {output}");
            Ok(())
        }
        "diff" => {
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
            let after_grammar =
                parse_grammar_environment(Path::new(&args[2]), &after, &grammar_registry)?;
            let mut session = IncrementalCompilerSession::with_grammar_registry(grammar_registry);
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
        "-h" | "--help" | "help" => Err(LabError::Usage(usage())),
        _ => Err(LabError::Usage(usage())),
    }
}

fn compile_path(path: &str, mode: RuntimeMode) -> Result<SessionArtifacts> {
    Ok(compile_workbench_path(path, mode)?.artifacts)
}

fn compile_workbench_path(path: &str, mode: RuntimeMode) -> Result<CompiledWorkbench> {
    let source = read_path(path)?;
    let grammar_registry = GrammarFrontendRegistry::builtin();
    let grammar_environment =
        parse_grammar_environment(Path::new(path), &source, &grammar_registry)?;
    let host_registry = parse_host_registry(Path::new(path), &source)?;
    let host_environment = parse_host_environment(Path::new(path), &source)?;
    let mut session = CompilerSession::with_grammar_registry(grammar_registry);
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
            "usage: interloko_lab dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|all"
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
        "all" => DumpStage::All,
        _ => {
            return Err(LabError::Usage(
                "stage must be one of syntax|ast|sem|mop|eop|lowered|tape|all".into(),
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
) -> Result<()> {
    if affordance == EnvironmentAffordance::Run {
        if !extra.is_empty() {
            return Err(LabError::Usage(format!(
                "affordance `{}` does not accept extra arguments",
                affordance.as_str()
            )));
        }
        let workbench = compile_workbench_path(file, RuntimeMode::RunMain)?;
        print_diagnostics(&workbench.artifacts.diagnostics)?;
        let object = workbench.object(object_id)?;
        ensure_object_has_affordance(&object, affordance)?;
        return print_runtime_result(&workbench.artifacts);
    }

    if affordance == EnvironmentAffordance::AddMethod {
        let source_path = parse_add_method_args(extra)?;
        return invoke_add_method(file, object_id, source_path);
    }

    if !extra.is_empty() {
        return Err(LabError::Usage(format!(
            "affordance `{}` does not accept extra arguments",
            affordance.as_str()
        )));
    }

    let workbench = compile_workbench_path(file, RuntimeMode::CompileOnly)?;
    print_diagnostics(&workbench.artifacts.diagnostics)?;
    let object = workbench.object(object_id)?;
    ensure_object_has_affordance(&object, affordance)?;
    match affordance {
        EnvironmentAffordance::DumpSyntax => {
            print!("{}", workbench.dump_stage(DumpStage::Syntax));
            Ok(())
        }
        EnvironmentAffordance::DumpAst => {
            print!("{}", workbench.dump_stage(DumpStage::Ast));
            Ok(())
        }
        EnvironmentAffordance::DumpSem => {
            print!("{}", workbench.dump_stage(DumpStage::Sem));
            Ok(())
        }
        EnvironmentAffordance::DumpMop => {
            print!("{}", workbench.dump_stage(DumpStage::Mop));
            Ok(())
        }
        EnvironmentAffordance::DumpEop => {
            print!("{}", workbench.dump_stage(DumpStage::Eop));
            Ok(())
        }
        EnvironmentAffordance::DumpLowered => {
            print!("{}", workbench.dump_stage(DumpStage::Lowered));
            Ok(())
        }
        EnvironmentAffordance::DumpTape => {
            print!("{}", workbench.dump_stage(DumpStage::Tape));
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
        EnvironmentAffordance::ShowGrammarStack => {
            print!("{}", workbench.artifacts.dump_grammar_stack());
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
        EnvironmentAffordance::ShowParseTrace => {
            print!("{}", workbench.artifacts.dump_parse_trace());
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

fn invoke_add_method(file: &str, object_id: &str, method_source_path: &str) -> Result<()> {
    let mut workbench = compile_workbench_path(file, RuntimeMode::CompileOnly)?;
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
        .map_err(|error| LabError::MutationFailed {
            summary: format_mutation_error(&error),
        })?;

    print_mutation_outcome(&outcome, workbench.artifacts.mutations().len());
    Ok(())
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
    println!(
        "applied add-method mutation_id={} inserted_method=m{} affected_artifacts={} strategy={strategy} log_size={log_size}",
        outcome.mutation_id,
        outcome.inserted_method.0,
        outcome.affected_artifacts.len(),
    );
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
        MutationError::SemanticFailed { diagnostics } => {
            format!("semantic failure ({} new diagnostics)", diagnostics.len())
        }
        MutationError::DuplicateSpecializer {
            existing_method, ..
        } => format!(
            "specializer vector duplicates existing method m{}",
            existing_method.0
        ),
        MutationError::RuntimeValidationFailed { error } => {
            format!("runtime layout validation failed: {error:?}")
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

fn print_runtime_result(artifacts: &SessionArtifacts) -> Result<()> {
    match &artifacts.runtime_result {
        Some(RuntimeResult::ReturnedI64(value)) => {
            println!("{value}");
            Ok(())
        }
        Some(result) => Err(LabError::Runtime(format!("{result:?}"))),
        None => Err(LabError::Runtime("runtime was not requested".into())),
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
        artifacts: ArtifactFacet::new(Vec::new()),
        affordances: AffordanceFacet::new(vec![
            EnvironmentAffordance::Inspect,
            EnvironmentAffordance::DumpEop,
            EnvironmentAffordance::ShowRuntimeLayout,
        ]),
        execution: None,
        inspection: InspectionFacet::new(inspection),
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
        "usage:\n  {binary} run [--trace] [--instructions] <file.loko>\n  {binary} trace [--instructions] <file.loko>\n  {binary} dump <file.loko> --stage syntax|ast|sem|mop|eop|lowered|tape|all\n  {binary} mop <file.loko>\n  {binary} eop <file.loko>\n  {binary} registry <file.loko>\n  {binary} extensions <file.loko>\n  {binary} dispatch <file.loko> [dispatch-site-id]\n  {binary} inspect <file.loko> <object-id>\n  {binary} affordances <file.loko> <object-id>\n  {binary} invoke-affordance <file.loko> <object-id> <affordance> [affordance-args...]\n  {binary} invoke-affordance <file.loko> <generic:gN> add-method --source <method.loko>\n  {binary} artifacts <file.loko>\n  {binary} report <file.loko> --output <report.html>\n  {binary} diff <before.loko> <after.loko>\n\ndiagnostics:\n  INTERLOKO_DIAGNOSTIC_LOCALE=en|pseudo\n  INTERLOKO_DIAGNOSTIC_CATALOG=path/to/catalog.mf2.toml"
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
        summary: String,
    },
    Runtime(String),
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
            Self::Io { .. } | Self::Runtime(_) | Self::DiagnosticCatalog(_) => 1,
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
            Self::MutationFailed { summary } => {
                write!(f, "add-method mutation failed: {summary}")
            }
            Self::Runtime(message) => write!(f, "runtime failed: {message}"),
            Self::DiagnosticCatalog(message) => write!(f, "diagnostic catalog failed: {message}"),
        }
    }
}

impl Error for LabError {}
