//! Full-stack e2e for #1908: the Stop button must end a turn promptly even
//! when the agent ignores ACP `session/cancel`.
//!
//! opencode 1.15.13 ignored `session/cancel` and kept streaming, so the first
//! Stop click appeared to do nothing and the user had to force-stop or wait
//! the full 10s escalation watchdog. The fix (agent-agnostic): once the user
//! has cancelled and the agent keeps producing new progress, the prompt loop
//! shortens the escalation deadline to the Stop click + 2s and force-restarts
//! the worker, ending the turn as `Stopped { reason: "agent_unresponsive" }`.
//!
//! This proves it end-to-end against a real `aoe serve --daemon`: the shared
//! Node fake-ACP agent is put in `FAKE_ACP_IGNORE_CANCEL` mode so it streams
//! through the cancel like opencode did, and the terminal `agent_unresponsive`
//! event is asserted to land well under the old 10s baseline. The pure
//! deadline logic is unit-tested in `src/acp/acp_client.rs`.
//!
//! Compiled only with the default `serve` feature. Run via:
//!
//! ```sh
//! cargo test --features e2e-tests --test e2e -- acp_stop_ignored_cancel
//! ```
#![cfg(feature = "serve")]

use std::time::{Duration, Instant};

use serial_test::serial;

use crate::harness::{pick_free_port, require_node, require_tmux, wait_for_port, TuiTestHarness};

/// Build a long, steady stream: 40 text chunks spaced 500ms apart (~20s of
/// output). The agent keeps emitting progress right through a mid-turn
/// cancel, which is exactly what arms the post-cancel escalation; the worker
/// is force-restarted long before the script would naturally end.
fn streaming_script() -> String {
    let mut updates = String::new();
    for i in 0..40 {
        if i > 0 {
            updates.push(',');
        }
        updates.push_str(
            r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"tok "}},{"sessionUpdate":"wait_ms","ms":500}"#,
        );
    }
    format!(r#"{{"turns":[{{"updates":[{updates}],"stopReason":"end_turn"}}]}}"#)
}

fn parse_session_id(add_stdout: &str) -> String {
    add_stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("ID:"))
        .map(|rest| rest.trim().to_string())
        .unwrap_or_else(|| panic!("could not find session ID in `aoe add` output:\n{add_stdout}"))
}

/// Retry `aoe acp prompt` until accepted; the POST 404s until the worker is
/// live and handshaked, so a success is the readiness oracle.
fn prompt_until_accepted(h: &TuiTestHarness, session_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let out = h.run_cli(&["acp", "prompt", session_id, "please write a long answer"]);
        if out.status.success() {
            return;
        }
        if Instant::now() >= deadline {
            let ps = h.run_cli(&["acp", "ps", "--json"]);
            panic!(
                "structured view worker never accepted a prompt within {:?}.\n\
                 last prompt stdout: {}\n last prompt stderr: {}\n acp ps: {}",
                timeout,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&ps.stdout),
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Poll `aoe acp history --json` until its output contains `needle`, or panic
/// on timeout. Returns once found.
fn wait_for_history_contains(
    h: &TuiTestHarness,
    session_id: &str,
    needle: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let out = h.run_cli(&["acp", "history", session_id, "--json"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.contains(needle) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "history never contained {:?} within {:?}.\nlast history stdout:\n{}",
                needle, timeout, stdout
            );
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// A single Stop click against a cancel-ignoring agent ends the turn as
/// `agent_unresponsive` in ~2s, not the old 10s watchdog and not requiring a
/// second (force-stop) click.
#[test]
#[serial]
fn stop_escalates_when_agent_ignores_cancel() {
    require_tmux!();
    require_node!();

    let mut h = TuiTestHarness::new_in_tmp("acp_stop_ignored_cancel");

    // Put the fake agent in cancel-ignoring mode BEFORE installing the shim so
    // the knob is baked in (the daemon strips arbitrary env when spawning the
    // worker).
    h.set_acp_ignore_cancel();
    let script_path = h.home_path().join("streaming-script.json");
    std::fs::write(&script_path, streaming_script()).expect("write fake-acp script");
    h.install_acp_shim(&script_path);
    h.stop_daemon_on_drop();

    // A structured view session needs a git repo workspace.
    let project = h.project_path();
    for args in [
        vec!["init", "-q"],
        vec!["commit", "--allow-empty", "-q", "-m", "init"],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&project)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let port = pick_free_port();
    let port_s = port.to_string();
    let start = h.run_cli(&["serve", "--daemon", "--port", &port_s, "--no-auth"]);
    assert!(
        start.status.success(),
        "aoe serve --daemon failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&start.stdout),
        String::from_utf8_lossy(&start.stderr),
    );
    assert!(
        wait_for_port(port, Duration::from_secs(10)),
        "daemon never bound port {}",
        port
    );

    let add = h.run_cli(&[
        "add",
        project.to_str().unwrap(),
        "-t",
        "stop-ignored-cancel",
        "-c",
        "claude",
        "--structured-view",
    ]);
    assert!(
        add.status.success(),
        "aoe add --structured-view failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr),
    );
    let session_id = parse_session_id(&String::from_utf8_lossy(&add.stdout));

    // Kick off the long-streaming turn and wait until the agent is actively
    // producing output, so the cancel lands mid-turn.
    prompt_until_accepted(&h, &session_id, Duration::from_secs(30));
    wait_for_history_contains(&h, &session_id, "tok ", Duration::from_secs(15));

    // One Stop click. The agent ignores it and keeps streaming.
    let cancel = h.run_cli(&["acp", "cancel", &session_id]);
    assert!(
        cancel.status.success(),
        "aoe acp cancel failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&cancel.stdout),
        String::from_utf8_lossy(&cancel.stderr),
    );

    // The turn must terminate as agent_unresponsive well under the old 10s
    // baseline (escalation fires ~2s after the click). An 8s bound proves the
    // fast-escalation path ran, not the full-grace watchdog; without the fix
    // this event would not arrive until ~10s.
    wait_for_history_contains(
        &h,
        &session_id,
        "agent_unresponsive",
        Duration::from_secs(8),
    );
}
