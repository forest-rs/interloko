//! REPL scenarios of the break-loop transcripts in
//! `tests/fixtures/break_loop/`: `hot_swap.transcript`, which passes, and the
//! `break_loop.transcript` acceptance target, whose header explains what it
//! still waits on. The lab scenario of that transcript runs from
//! `interloko_lab`'s tests.

#[allow(
    dead_code,
    reason = "each acceptance test uses part of the shared reader"
)]
#[path = "support/transcript.rs"]
mod transcript;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use transcript::ScenarioKind;

#[test]
fn hot_swap_transcript_repl_scenario() {
    run_repl_transcript("tests/fixtures/break_loop/hot_swap.transcript");
}

#[test]
#[ignore = "acceptance target: break-loop EOP objects, choose-restart and the lab chooser"]
fn break_loop_transcript_repl_scenario() {
    run_repl_transcript("tests/fixtures/break_loop/break_loop.transcript");
}

/// Feeds each REPL scenario of a transcript to one `interloko_repl` process
/// and compares its output.
fn run_repl_transcript(path: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(root.join(path)).expect("read transcript");
    let scenarios = transcript::parse(&text);
    let repl_scenarios: Vec<_> = scenarios
        .iter()
        .filter(|scenario| scenario.kind == ScenarioKind::Repl)
        .collect();
    assert!(!repl_scenarios.is_empty(), "no repl scenario in {path}");

    for scenario in repl_scenarios {
        for step in &scenario.steps {
            let mut child = Command::new(env!("CARGO_BIN_EXE_interloko_repl"))
                .current_dir(root)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn interloko_repl");
            let mut input = step.input.join("\n");
            input.push('\n');
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("write stdin");
            let output = child.wait_with_output().expect("wait for interloko_repl");
            assert!(
                output.status.success(),
                "scenario `{}` exited unsuccessfully\nstderr:\n{}",
                scenario.title,
                String::from_utf8_lossy(&output.stderr)
            );

            let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
            let stderr = String::from_utf8(output.stderr).expect("stderr utf8");
            transcript::assert_output(&scenario.title, "stdout", &step.stdout, &stdout);
            transcript::assert_output(&scenario.title, "stderr", &step.stderr, &stderr);
        }
    }
}
