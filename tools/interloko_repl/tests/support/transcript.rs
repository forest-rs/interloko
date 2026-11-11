//! Reader for scripted session transcripts such as
//! `tests/fixtures/break_loop/break_loop.transcript`.
//!
//! A transcript is a sequence of scenarios, each opened by a
//! `=== <kind>: <title>` line. In a `repl` scenario, `> ` lines are fed to
//! the REPL's stdin. In a `lab` scenario, each `$ interloko_lab <args>`
//! line is one invocation. `! ` lines are expected stderr and any other line
//! is expected stdout. Blank lines and `//` commentary are ignored, so the
//! expected output can be laid out for reading.
//!
//! Shared by the `interloko_repl` and `interloko_lab` acceptance tests
//! through `#[path]`.

/// Which tool a scenario drives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenarioKind {
    /// One `interloko_repl` process fed the scenario's `> ` lines.
    Repl,
    /// One `interloko_lab` process per `$ interloko_lab` line.
    Lab,
}

/// One scripted run of a tool and the output it must produce.
#[derive(Debug, Default)]
pub struct Step {
    /// REPL stdin, one entry per line (empty for lab steps).
    pub input: Vec<String>,
    /// Lab command-line arguments (empty for REPL steps).
    pub args: Vec<String>,
    /// Expected stdout lines, without blank lines.
    pub stdout: Vec<String>,
    /// Expected stderr lines, without blank lines.
    pub stderr: Vec<String>,
}

/// A titled scenario: a REPL scenario has one step, a lab scenario one
/// step per invocation.
#[derive(Debug)]
pub struct Scenario {
    /// Tool the scenario drives.
    pub kind: ScenarioKind,
    /// Title from the `===` line, used in failure messages.
    pub title: String,
    /// Runs in order.
    pub steps: Vec<Step>,
}

/// Parses a transcript into its scenarios.
pub fn parse(text: &str) -> Vec<Scenario> {
    let mut scenarios: Vec<Scenario> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with("//") {
            continue;
        }
        if let Some(header) = line.strip_prefix("=== ") {
            let (kind, title) = header.split_once(':').expect("`=== <kind>: <title>`");
            let kind = match kind {
                "repl" => ScenarioKind::Repl,
                "lab" => ScenarioKind::Lab,
                other => panic!("unknown scenario kind `{other}`"),
            };
            let steps = match kind {
                ScenarioKind::Repl => vec![Step::default()],
                ScenarioKind::Lab => Vec::new(),
            };
            scenarios.push(Scenario {
                kind,
                title: title.trim().to_owned(),
                steps,
            });
            continue;
        }
        let scenario = scenarios
            .last_mut()
            .expect("transcript lines must follow a `===` scenario header");
        if let Some(command) = line.strip_prefix("$ ") {
            assert_eq!(
                scenario.kind,
                ScenarioKind::Lab,
                "`$` outside a lab scenario"
            );
            let mut words = command.split_whitespace();
            assert_eq!(words.next(), Some("interloko_lab"), "{line}");
            scenario.steps.push(Step {
                args: words.map(str::to_owned).collect(),
                ..Step::default()
            });
            continue;
        }
        let step = scenario
            .steps
            .last_mut()
            .expect("expected output must follow a `$` invocation");
        if line == ">" || line.starts_with("> ") {
            assert_eq!(
                scenario.kind,
                ScenarioKind::Repl,
                "`>` outside a repl scenario"
            );
            step.input
                .push(line.get(2..).unwrap_or_default().to_owned());
        } else if let Some(error) = line.strip_prefix("! ") {
            step.stderr.push(error.to_owned());
        } else {
            step.stdout.push(line.to_owned());
        }
    }
    scenarios
}

/// Asserts that `actual` output matches `expected`, ignoring blank lines,
/// and reports the first line that differs.
pub fn assert_output(scenario: &str, stream: &str, expected: &[String], actual: &str) {
    let actual: Vec<&str> = actual
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let Some(index) = (0..expected.len().max(actual.len()))
        .find(|&index| expected.get(index).map(String::as_str) != actual.get(index).copied())
    else {
        return;
    };
    panic!(
        "{stream} differs in scenario `{scenario}` at output line {}\n\
         expected: {:?}\n  actual: {:?}\n\nfull {stream}:\n{}",
        index + 1,
        expected.get(index),
        actual.get(index),
        actual.join("\n"),
    );
}
