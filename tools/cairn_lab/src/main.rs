//! CLI workbench for Cairn compiler sessions.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;

use cairn_compiler::identity::SourceFileId;
use cairn_compiler::session::{CompilerSession, DumpStage, RuntimeMode, RuntimeResult};

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
            let before = compile_path_with_id(&args[1], source_file, RuntimeMode::CompileOnly)?;
            let after = compile_path_with_id(&args[2], source_file, RuntimeMode::CompileOnly)?;
            let diff = CompilerSession::diff(&before, &after);
            print!("{}", diff.dump());
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
    let source = fs::read_to_string(path).map_err(|error| LabError::Io {
        path: path.into(),
        error,
    })?;
    Ok(CompilerSession::compile_source(path, &source, mode))
}

fn compile_path_with_id(
    path: &str,
    source_file: SourceFileId,
    mode: RuntimeMode,
) -> Result<cairn_compiler::session::SessionArtifacts> {
    let source = fs::read_to_string(path).map_err(|error| LabError::Io {
        path: path.into(),
        error,
    })?;
    Ok(CompilerSession::compile_source_with_id(
        source_file,
        path,
        &source,
        mode,
    ))
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
            "usage: cairn_lab dump <file.cairn> --stage syntax|ast|sem|lowered|tape|all".into(),
        ));
    }
    let stage = match args[3].as_str() {
        "syntax" => DumpStage::Syntax,
        "ast" => DumpStage::Ast,
        "sem" => DumpStage::Sem,
        "lowered" => DumpStage::Lowered,
        "tape" => DumpStage::Tape,
        "all" => DumpStage::All,
        _ => {
            return Err(LabError::Usage(
                "stage must be one of syntax|ast|sem|lowered|tape|all".into(),
            ));
        }
    };
    Ok((&args[1], stage))
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
        "usage:\n  {binary} run <file.cairn>\n  {binary} dump <file.cairn> --stage syntax|ast|sem|lowered|tape|all\n  {binary} artifacts <file.cairn>\n  {binary} diff <before.cairn> <after.cairn>"
    )
}

#[derive(Debug)]
enum LabError {
    Usage(String),
    Io { path: String, error: std::io::Error },
    Runtime(String),
}

impl LabError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Io { .. } | Self::Runtime(_) => 1,
        }
    }
}

impl fmt::Display for LabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io { path, error } => write!(f, "failed to read {path}: {error}"),
            Self::Runtime(message) => write!(f, "runtime failed: {message}"),
        }
    }
}

impl Error for LabError {}
