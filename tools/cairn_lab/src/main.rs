//! CLI workbench for Cairn compiler sessions.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;

use cairn_compiler::identity::SourceFileId;
use cairn_compiler::sem::{HostClass, HostMetaRegistry, HostSlot, ValueKind};
use cairn_compiler::session::{
    CompilerSession, DumpStage, IncrementalCompilerSession, RuntimeMode, RuntimeResult,
};

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
            print_diagnostics(&artifacts.diagnostics);
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
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics);
            print!("{}", artifacts.dump_stage(stage));
            Ok(())
        }
        "mop" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics);
            print!("{}", artifacts.dump_stage(DumpStage::Mop));
            Ok(())
        }
        "eop" => {
            let file = single_file_arg(&args)?;
            let artifacts = compile_path(file, RuntimeMode::CompileOnly)?;
            print_diagnostics(&artifacts.diagnostics);
            print!("{}", artifacts.dump_stage(DumpStage::Eop));
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
                    "usage: cairn_lab diff <before.cairn> <after.cairn>".into(),
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

fn compile_path(
    path: &str,
    mode: RuntimeMode,
) -> Result<cairn_compiler::session::SessionArtifacts> {
    let source = read_path(path)?;
    let registry = parse_host_registry(Path::new(path), &source)?;
    Ok(CompilerSession::compile_source_with_host_registry(
        path, &source, mode, &registry,
    ))
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
            "usage: cairn_lab {} <file.cairn>",
            args.first().map_or("COMMAND", String::as_str)
        )));
    }
    Ok(&args[1])
}

fn dump_args(args: &[String]) -> Result<(&str, DumpStage)> {
    if args.len() != 4 || args[2] != "--stage" {
        return Err(LabError::Usage(
            "usage: cairn_lab dump <file.cairn> --stage syntax|ast|sem|mop|eop|lowered|tape|all"
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

fn print_diagnostics(diagnostics: &[cairn_compiler::diagnostic::Diagnostic]) {
    if diagnostics.is_empty() {
        return;
    }
    eprintln!("diagnostics:");
    for diagnostic in diagnostics {
        eprintln!(
            "  {:?} {}..{} {}",
            diagnostic.severity, diagnostic.span.start, diagnostic.span.end, diagnostic.message
        );
    }
}

fn usage() -> String {
    let binary = env::args()
        .next()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "cairn_lab".into());
    format!(
        "usage:\n  {binary} run <file.cairn>\n  {binary} dump <file.cairn> --stage syntax|ast|sem|mop|eop|lowered|tape|all\n  {binary} mop <file.cairn>\n  {binary} eop <file.cairn>\n  {binary} artifacts <file.cairn>\n  {binary} diff <before.cairn> <after.cairn>"
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
    Runtime(String),
}

impl LabError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::InvalidHeader { .. } => 2,
            Self::Io { .. } | Self::Runtime(_) => 1,
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
            Self::Runtime(message) => write!(f, "runtime failed: {message}"),
        }
    }
}

impl Error for LabError {}
