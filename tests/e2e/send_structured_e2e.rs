//! `aoe send` against an ACP/structured-view session has no tmux pane to type
//! into, so it must dispatch through the running daemon's prompt endpoint
//! instead of bailing with "no tmux pane".

use std::time::Duration;

use serial_test::parallel;

use crate::harness::{require_node, require_tmux, TuiTestHarness};

#[test]
#[parallel]
fn send_delivers_to_structured_session_via_daemon() {
    require_tmux!();
    require_node!();
    let h = TuiTestHarness::new_acp(
        "send_structured",
        r#"{ "turns": [
            { "updates": [], "stopReason": "end_turn" },
            { "updates": [], "stopReason": "end_turn" }
        ] }"#,
    );
    let (_, session_id) = h.start_structured_session("send_structured");
    h.prompt_until_accepted(&session_id, "warm up", Duration::from_secs(30));

    let out = h.run_cli_ok(&["send", &session_id, "hello via send"]);
    assert!(
        out.contains("message to 'send_structured'"),
        "unexpected send output: {out}"
    );
}
