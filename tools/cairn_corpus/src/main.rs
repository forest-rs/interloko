//! Small-program corpus runner for Cairn compiler sessions.

use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use cairn_compiler::session::{CompilerSession, DumpStage, RuntimeMode, RuntimeResult};

type Result<T> = std::result::Result<T, CorpusError>;

fn main() {
    match run(env::args().skip(1).collect()) {
        Ok(Outcome::Success) => {}
        Ok(Outcome::CheckFailed) => std::process::exit(1),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(error.exit_code());
        }
    }
}

fn run(args: Vec<String>) -> Result<Outcome> {
    let command = args.first().map_or("check", String::as_str);
    match command {
        "check" => check_command(args.get(1).map(String::as_str)),
        "list" => list_command(args.get(1).map(String::as_str)),
        "dump" => dump_command(&args),
        "-h" | "--help" | "help" => Err(CorpusError::Usage(usage())),
        _ => Err(CorpusError::Usage(usage())),
    }
}

fn check_command(root: Option<&str>) -> Result<Outcome> {
    let root = root.map_or_else(default_corpus_root, PathBuf::from);
    let cases = load_cases(&root)?;
    let mut passed = 0_usize;
    let mut failed = 0_usize;

    for case in &cases {
        let failures = check_case(case);
        if failures.is_empty() {
            passed += 1;
            println!("ok {}", case.display_path);
        } else {
            println!("FAILED {}", case.display_path);
            for failure in &failures {
                println!("  {failure}");
            }
            failed += 1;
        }
    }

    println!(
        "corpus: {passed} passed, {} failed, {} total",
        failed,
        cases.len()
    );

    if failed == 0 {
        Ok(Outcome::Success)
    } else {
        Ok(Outcome::CheckFailed)
    }
}

fn list_command(root: Option<&str>) -> Result<Outcome> {
    let root = root.map_or_else(default_corpus_root, PathBuf::from);
    for case in load_cases(&root)? {
        println!("{}", case.display_path);
    }
    Ok(Outcome::Success)
}

fn dump_command(args: &[String]) -> Result<Outcome> {
    if args.len() != 4 || args[2] != "--stage" {
        return Err(CorpusError::Usage(
            "usage: cairn_corpus dump <file.cairn> --stage syntax|ast|sem|lowered|tape|all".into(),
        ));
    }
    let stage = parse_stage(&args[3]).ok_or_else(|| {
        CorpusError::Usage("stage must be one of syntax|ast|sem|lowered|tape|all".into())
    })?;
    let source = read_to_string(Path::new(&args[1]))?;
    let artifacts = CompilerSession::compile_source(&args[1], &source, RuntimeMode::CompileOnly);
    print!("{}", artifacts.dump_stage(stage));
    Ok(Outcome::Success)
}

fn load_cases(root: &Path) -> Result<Vec<Case>> {
    let mut paths = Vec::new();
    collect_cairn_files(root, &mut paths)?;
    paths.sort();

    let mut cases = Vec::with_capacity(paths.len());
    for path in paths {
        let source = read_to_string(&path)?;
        let expectations = parse_expectations(&path, &source)?;
        let display_path = display_path(root, &path);
        cases.push(Case {
            display_path,
            source,
            expectations,
        });
    }
    Ok(cases)
}

fn collect_cairn_files(root: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(root).map_err(|error| CorpusError::Io {
        path: root.display().to_string(),
        error,
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| CorpusError::Io {
            path: root.display().to_string(),
            error,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_cairn_files(&path, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "cairn")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn parse_expectations(path: &Path, source: &str) -> Result<Vec<Expectation>> {
    let mut expectations = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            if let Some(rest) = trimmed.strip_prefix("// expect:") {
                expectations.push(parse_expectation(path, index + 1, rest.trim())?);
            }
            continue;
        }
        break;
    }

    if expectations.is_empty() {
        return Err(CorpusError::InvalidHeader {
            path: path.display().to_string(),
            line: 1,
            message: "missing // expect: header".into(),
        });
    }
    Ok(expectations)
}

fn parse_expectation(path: &Path, line: usize, input: &str) -> Result<Expectation> {
    if let Some(value) = input.strip_prefix("run ") {
        let value = value
            .parse()
            .map_err(|_| invalid_header(path, line, "run expectation must be an integer"))?;
        return Ok(Expectation::Run(value));
    }

    if let Some(value) = input.strip_prefix("diagnostics ") {
        let value = value
            .parse()
            .map_err(|_| invalid_header(path, line, "diagnostics expectation must be a count"))?;
        return Ok(Expectation::Diagnostics(value));
    }

    for prefix in ["diagnostic contains ", "diagnostics contains "] {
        if let Some(text) = input.strip_prefix(prefix) {
            return Ok(Expectation::DiagnosticContains(text.into()));
        }
    }

    if let Some(rest) = input.strip_prefix("dump ") {
        let Some((stage, text)) = rest.split_once(" contains ") else {
            return Err(invalid_header(
                path,
                line,
                "dump expectation must be `dump STAGE contains TEXT`",
            ));
        };
        let stage = parse_stage(stage).ok_or_else(|| {
            invalid_header(
                path,
                line,
                "dump stage must be syntax|ast|sem|lowered|tape|all",
            )
        })?;
        return Ok(Expectation::DumpContains {
            stage,
            text: text.into(),
        });
    }

    Err(invalid_header(path, line, "unknown expectation"))
}

fn check_case(case: &Case) -> Vec<String> {
    let mode = if case
        .expectations
        .iter()
        .any(|expectation| matches!(expectation, Expectation::Run(_)))
    {
        RuntimeMode::RunMain
    } else {
        RuntimeMode::CompileOnly
    };
    let artifacts = CompilerSession::compile_source(&case.display_path, &case.source, mode);
    let mut failures = Vec::new();

    for expectation in &case.expectations {
        match expectation {
            Expectation::Run(expected) => match &artifacts.runtime_result {
                Some(RuntimeResult::ReturnedI64(actual)) if actual == expected => {}
                Some(actual) => {
                    failures.push(format!("expected run result {expected}, got {actual:?}"))
                }
                None => failures.push(format!(
                    "expected run result {expected}, but main did not run"
                )),
            },
            Expectation::Diagnostics(expected) => {
                let actual = artifacts.diagnostics.len();
                if actual != *expected {
                    failures.push(format!("expected {expected} diagnostics, got {actual}"));
                }
            }
            Expectation::DiagnosticContains(text) => {
                if !artifacts
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(text))
                {
                    failures.push(format!("expected diagnostic containing `{text}`"));
                }
            }
            Expectation::DumpContains { stage, text } => {
                let dump = artifacts.dump_stage(*stage);
                if !dump.contains(text) {
                    failures.push(format!("expected {stage:?} dump containing `{text}`"));
                }
            }
        }
    }

    failures
}

fn parse_stage(value: &str) -> Option<DumpStage> {
    match value {
        "syntax" => Some(DumpStage::Syntax),
        "ast" => Some(DumpStage::Ast),
        "sem" => Some(DumpStage::Sem),
        "lowered" => Some(DumpStage::Lowered),
        "tape" => Some(DumpStage::Tape),
        "all" => Some(DumpStage::All),
        _ => None,
    }
}

fn invalid_header(path: &Path, line: usize, message: &str) -> CorpusError {
    CorpusError::InvalidHeader {
        path: path.display().to_string(),
        line,
        message: message.into(),
    }
}

fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|error| CorpusError::Io {
        path: path.display().to_string(),
        error,
    })
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn default_corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("corpus")
}

fn usage() -> String {
    let binary = env::args()
        .next()
        .and_then(|path| {
            Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "cairn_corpus".into());
    format!(
        "usage:\n  {binary} check [corpus-root]\n  {binary} list [corpus-root]\n  {binary} dump <file.cairn> --stage syntax|ast|sem|lowered|tape|all"
    )
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Outcome {
    Success,
    CheckFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Expectation {
    Run(i64),
    Diagnostics(usize),
    DiagnosticContains(String),
    DumpContains { stage: DumpStage, text: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Case {
    display_path: String,
    source: String,
    expectations: Vec<Expectation>,
}

#[derive(Debug)]
enum CorpusError {
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
}

impl CorpusError {
    const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) | Self::InvalidHeader { .. } => 2,
            Self::Io { .. } => 1,
        }
    }
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(f, "{message}"),
            Self::Io { path, error } => write!(f, "failed to read {path}: {error}"),
            Self::InvalidHeader {
                path,
                line,
                message,
            } => {
                write!(f, "{path}:{line}: invalid corpus expectation: {message}")
            }
        }
    }
}

impl Error for CorpusError {}
