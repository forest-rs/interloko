//! CLI workbench for Cairn compiler sessions.

use std::env;
use std::error::Error;
use std::fmt::{self, Write as _};
use std::fs;
use std::path::Path;

use interloko_compiler::identity::SourceFileId;
use interloko_compiler::sem::{HostClass, HostMetaRegistry, HostSlot, ValueKind};
use interloko_compiler::session::{
    AffordanceFacet, CompilerSession, DumpStage, EnvironmentAffordance, EnvironmentKind,
    EnvironmentObject, EnvironmentObjectId, EnvironmentObjectSink, EnvironmentOrigin,
    EnvironmentStorage, HostEnvironmentProvider, IncrementalCompilerSession, InspectionFacet,
    PresentationFacet, RuntimeMode, RuntimeResult, SessionArtifacts,
};
use interloko_diagnostics::{DiagnosticCatalog, DiagnosticRenderer};

type Result<T> = std::result::Result<T, LabError>;

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
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::RunMain)?;
            print_diagnostics(&artifacts.diagnostics)?;
            match artifacts.runtime_result {
                Some(RuntimeResult::ReturnedI64(value)) => {
                    println!("{value}");
                    Ok(())
                }
                Some(result) => Err(LabError::Runtime(format!("{result:?}"))),
                None => Err(LabError::Runtime("runtime was not requested".into())),
            }
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
            let (file, object_id, affordance) = invoke_args(&args)?;
            invoke_affordance(file, object_id, affordance)?;
            Ok(())
        }
        "artifacts" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print!("{}", artifacts.dump_artifact_graph());
            Ok(())
        }
        "diff" => {
            if args.len() != 3 {
                return Err(LabError::Usage(
                    "usage: interloko_lab diff <before.cairn> <after.cairn>".into(),
                ));
            }
            let source_file = SourceFileId::from_path(&args[1]);
            let before = read_path(&args[1])?;
            let after = read_path(&args[2])?;
            let mut session = IncrementalCompilerSession::new();
            session.add_source_with_id(source_file, &args[1], &before);
            let _ = session
                .compile(source_file, RuntimeMode::CompileOnly)
                .expect("source was just added");
            let update = session
                .edit_and_compile(source_file, &after, RuntimeMode::CompileOnly)
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
    let registry = parse_host_registry(Path::new(path), &source)?;
    let host_environment = parse_host_environment(Path::new(path), &source)?;
    let artifacts =
        CompilerSession::compile_source_with_host_registry(path, &source, mode, &registry);
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
            "usage: interloko_lab {} <file.cairn>",
            args.first().map_or("COMMAND", String::as_str)
        )));
    }
    Ok(&args[1])
}

fn dump_args(args: &[String]) -> Result<(&str, DumpStage)> {
    if args.len() != 4 || args[2] != "--stage" {
        return Err(LabError::Usage(
            "usage: interloko_lab dump <file.cairn> --stage syntax|ast|sem|mop|eop|lowered|tape|all"
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
            "usage: interloko_lab {} <file.cairn> <object-id>",
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
            "usage: interloko_lab dispatch <file.cairn> [dispatch-site-id]".into(),
        )),
    }
}

fn invoke_args(args: &[String]) -> Result<(&str, &str, EnvironmentAffordance)> {
    if args.len() != 4 {
        return Err(LabError::Usage(
            "usage: interloko_lab invoke-affordance <file.cairn> <object-id> <affordance>".into(),
        ));
    }
    let affordance = parse_affordance(&args[3]).ok_or_else(|| LabError::UnknownAffordance {
        name: args[3].clone(),
    })?;
    Ok((&args[1], &args[2], affordance))
}

fn parse_affordance(input: &str) -> Option<EnvironmentAffordance> {
    all_affordances()
        .iter()
        .copied()
        .find(|affordance| affordance.as_str() == input)
}

fn all_affordances() -> &'static [EnvironmentAffordance] {
    use EnvironmentAffordance as Aff;

    &[
        Aff::Inspect,
        Aff::DumpSyntax,
        Aff::DumpAst,
        Aff::DumpSem,
        Aff::DumpMop,
        Aff::DumpEop,
        Aff::DumpLowered,
        Aff::DumpTape,
        Aff::ShowDispatch,
        Aff::ShowRuntimeLayout,
        Aff::ShowDependencies,
        Aff::ShowAccessor,
        Aff::ShowSlot,
        Aff::Run,
        Aff::EditBody,
        Aff::AddMethod,
        Aff::Artifacts,
        Aff::Diff,
        Aff::OpenSource,
        Aff::DumpArtifacts,
        Aff::ShowSlots,
    ]
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

fn invoke_affordance(file: &str, object_id: &str, affordance: EnvironmentAffordance) -> Result<()> {
    if affordance == EnvironmentAffordance::Run {
        let workbench = compile_workbench_path(file, RuntimeMode::RunMain)?;
        print_diagnostics(&workbench.artifacts.diagnostics)?;
        let object = workbench.object(object_id)?;
        ensure_object_has_affordance(&object, affordance)?;
        return print_runtime_result(&workbench.artifacts);
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
                print!(
                    "{}",
                    mop_section(&workbench.artifacts, "dispatch table:", "runtime layout:")
                );
            }
            Ok(())
        }
        EnvironmentAffordance::ShowRuntimeLayout => {
            print!(
                "{}",
                mop_section(&workbench.artifacts, "runtime layout:", "")
            );
            Ok(())
        }
        EnvironmentAffordance::ShowDependencies => {
            print!("{}", workbench.artifacts.dump_artifact_graph());
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

fn mop_section(artifacts: &SessionArtifacts, start: &str, end: &str) -> String {
    let dump = artifacts.dump_stage(DumpStage::Mop);
    let Some(start_index) = dump.find(start) else {
        return dump;
    };
    let section = &dump[start_index..];
    if end.is_empty() {
        return section.into();
    }
    section
        .find(end)
        .map_or_else(|| section.into(), |end_index| section[..end_index].into())
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
            "host-class name must be a Cairn class name",
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
            "host-eop-class class name must be a Cairn class name",
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
            "host-eop-instance class name must be a Cairn class name",
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
        "usage:\n  {binary} run <file.cairn>\n  {binary} dump <file.cairn> --stage syntax|ast|sem|mop|eop|lowered|tape|all\n  {binary} mop <file.cairn>\n  {binary} eop <file.cairn>\n  {binary} dispatch <file.cairn> [dispatch-site-id]\n  {binary} inspect <file.cairn> <object-id>\n  {binary} affordances <file.cairn> <object-id>\n  {binary} invoke-affordance <file.cairn> <object-id> <affordance>\n  {binary} artifacts <file.cairn>\n  {binary} diff <before.cairn> <after.cairn>\n\ndiagnostics:\n  INTERLOKO_DIAGNOSTIC_LOCALE=en|pseudo\n  INTERLOKO_DIAGNOSTIC_CATALOG=path/to/catalog.mf2.toml"
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
    Runtime(String),
    DiagnosticCatalog(String),
}

impl LabError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_)
            | Self::InvalidHeader { .. }
            | Self::InvalidHostEopHeader { .. }
            | Self::UnknownObject { .. }
            | Self::UnknownDispatchSite { .. }
            | Self::UnknownAffordance { .. }
            | Self::AffordanceUnavailable { .. }
            | Self::UnsupportedAffordance { .. } => 2,
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
            Self::Runtime(message) => write!(f, "runtime failed: {message}"),
            Self::DiagnosticCatalog(message) => write!(f, "diagnostic catalog failed: {message}"),
        }
    }
}

impl Error for LabError {}
