//! E2E coverage for the plugin management CLI (#268): list shows the bundled
//! plugins with trust and state, enable/disable round-trips through config,
//! info prints capabilities, settings explain prints provenance, and the
//! contributed worker answers a status batch.

use serial_test::serial;

use crate::harness::{require_tmux, TuiTestHarness};

#[test]
#[serial]
fn test_plugin_list_shows_builtins_with_trust_and_state() {
    let h = TuiTestHarness::new("plugin_list");
    let output = h.run_cli(&["plugin", "list"]);
    assert!(
        output.status.success(),
        "aoe plugin list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("aoe.status"),
        "missing aoe.status:\n{stdout}"
    );
    assert!(
        stdout.contains("builtin"),
        "missing builtin trust label:\n{stdout}"
    );
    assert!(
        stdout.contains("enabled"),
        "missing state column:\n{stdout}"
    );
}

#[test]
#[serial]
fn test_plugin_disable_enable_round_trip() {
    let h = TuiTestHarness::new("plugin_toggle");

    let disable = h.run_cli(&["plugin", "disable", "aoe.status"]);
    assert!(
        disable.status.success(),
        "disable failed: {}",
        String::from_utf8_lossy(&disable.stderr)
    );
    let list = h.run_cli(&["plugin", "list"]);
    let stdout = String::from_utf8_lossy(&list.stdout);
    let status_line = stdout
        .lines()
        .find(|l| l.contains("aoe.status"))
        .unwrap_or_else(|| panic!("aoe.status missing from list:\n{stdout}"));
    assert!(
        status_line.contains("disabled"),
        "aoe.status should be disabled:\n{status_line}"
    );

    let enable = h.run_cli(&["plugin", "enable", "aoe.status"]);
    assert!(
        enable.status.success(),
        "enable failed: {}",
        String::from_utf8_lossy(&enable.stderr)
    );
    let list = h.run_cli(&["plugin", "list"]);
    let stdout = String::from_utf8_lossy(&list.stdout);
    let status_line = stdout.lines().find(|l| l.contains("aoe.status")).unwrap();
    assert!(
        status_line.contains("enabled") && !status_line.contains("disabled"),
        "aoe.status should be enabled again:\n{status_line}"
    );
}

#[test]
#[serial]
fn test_plugin_info_prints_capabilities_and_runtime() {
    let h = TuiTestHarness::new("plugin_info");
    let output = h.run_cli(&["plugin", "info", "aoe.status"]);
    assert!(
        output.status.success(),
        "info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("pane-read"),
        "capabilities missing:\n{stdout}"
    );
    assert!(
        stdout.contains("JSON-RPC worker"),
        "runtime line missing:\n{stdout}"
    );
}

#[test]
#[serial]
fn test_settings_explain_resolves_plugin_default() {
    let h = TuiTestHarness::new("plugin_settings_explain");
    let output = h.run_cli(&["settings", "explain", "aoe.status.custom_agent_rules"]);
    assert!(
        output.status.success(),
        "settings explain failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // No user value set: the manifest default wins and the chain says so.
    assert!(
        stdout.contains("manifest default"),
        "expected manifest-default provenance:\n{stdout}"
    );
    assert!(
        stdout.contains("true"),
        "expected the default value in the output:\n{stdout}"
    );
}

#[test]
#[serial]
fn test_install_and_update_track_tree_hash_not_just_manifest() {
    let h = TuiTestHarness::new("plugin_tree_hash");
    let source = tempfile::tempdir().expect("plugin source dir");
    std::fs::write(
        source.path().join("aoe-plugin.toml"),
        r#"
id = "acme.demo"
name = "Demo"
version = "1.0.0"
api_version = 1
description = "Tree hash e2e fixture."

[[settings]]
key = "verbose"
label = "Verbose"
widget = { kind = "toggle" }
default = false
"#,
    )
    .unwrap();
    std::fs::write(source.path().join("notes.txt"), "v1").unwrap();
    let dir = source.path().to_str().unwrap();

    let install = h.run_cli(&["plugin", "install", dir, "--yes"]);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        String::from_utf8_lossy(&install.stdout).contains("Installed acme.demo v1.0.0"),
        "unexpected install output"
    );
    // --yes must NOT silence the no-OS-sandbox warning: a scripted install
    // still surfaces it (on stderr) so capabilities are never granted blind.
    assert!(
        String::from_utf8_lossy(&install.stderr).contains("not an OS sandbox"),
        "--yes install must still print the sandbox warning:\n{}",
        String::from_utf8_lossy(&install.stderr)
    );

    let same = h.run_cli(&["plugin", "update", "acme.demo", "--yes"]);
    assert!(
        String::from_utf8_lossy(&same.stdout).contains("up to date"),
        "unchanged tree must be up to date:\n{}",
        String::from_utf8_lossy(&same.stdout)
    );

    // Code-only change: the manifest is untouched but the tree differs, so
    // the update must install it (and needs no capability re-prompt).
    std::fs::write(source.path().join("notes.txt"), "v2").unwrap();
    let changed = h.run_cli(&["plugin", "update", "acme.demo", "--yes"]);
    assert!(
        String::from_utf8_lossy(&changed.stdout).contains("Updated acme.demo to v1.0.0"),
        "code-only change must update:\n{}",
        String::from_utf8_lossy(&changed.stdout)
    );

    let uninstall = h.run_cli(&["plugin", "uninstall", "acme.demo"]);
    assert!(
        uninstall.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
}

#[test]
#[serial]
fn test_link_runs_from_source_and_unlink_keeps_it() {
    let h = TuiTestHarness::new("plugin_link");
    let source = tempfile::tempdir().expect("plugin source dir");
    let manifest = source.path().join("aoe-plugin.toml");
    std::fs::write(
        &manifest,
        r#"
id = "acme.linkdemo"
name = "Link Demo"
version = "0.1.0"
api_version = 1
description = "Link e2e fixture."
"#,
    )
    .unwrap();
    let dir = source.path().to_str().unwrap();

    let link = h.run_cli(&["plugin", "link", dir, "--yes"]);
    assert!(
        link.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(
        String::from_utf8_lossy(&link.stdout).contains("Linked acme.linkdemo v0.1.0"),
        "unexpected link output:\n{}",
        String::from_utf8_lossy(&link.stdout)
    );

    // The list marks the source as linked to the live path, not copied.
    let list = h.run_cli(&["plugin", "list"]);
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(
        listed.contains("acme.linkdemo") && listed.contains("linked:"),
        "list must show the linked source:\n{listed}"
    );

    // uninstall must refuse a linked plugin and point at unlink, so the
    // copy-removing path never runs against a live source tree.
    let bad = h.run_cli(&["plugin", "uninstall", "acme.linkdemo"]);
    assert!(
        !bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("unlink"),
        "uninstall on a linked plugin must fail and mention unlink:\n{}",
        String::from_utf8_lossy(&bad.stderr)
    );

    let unlink = h.run_cli(&["plugin", "unlink", "acme.linkdemo"]);
    assert!(
        unlink.status.success(),
        "unlink failed: {}",
        String::from_utf8_lossy(&unlink.stderr)
    );
    // The source tree must survive unlink untouched (no rm-rf through the
    // recorded path).
    assert!(
        manifest.is_file(),
        "unlink must not delete the source directory"
    );
    let gone = h.run_cli(&["plugin", "list"]);
    assert!(
        !String::from_utf8_lossy(&gone.stdout).contains("acme.linkdemo"),
        "unlinked plugin must disappear from the list"
    );
}

#[test]
#[serial]
fn test_core_setting_default_override_applies_and_explains() {
    let h = TuiTestHarness::new("plugin_core_override");
    let source = tempfile::tempdir().expect("plugin source dir");
    std::fs::write(
        source.path().join("aoe-plugin.toml"),
        r#"
id = "acme.coreover"
name = "Core Override Fixture"
version = "1.0.0"
api_version = 1

[[setting_defaults]]
target = "session.strict_hotkeys"
value = true
priority = 40
reason = "fixture flips a core default"
"#,
    )
    .unwrap();
    let install = h.run_cli(&[
        "plugin",
        "install",
        source.path().to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let explain = h.run_cli(&["settings", "explain", "session.strict_hotkeys"]);
    assert!(
        explain.status.success(),
        "explain failed: {}",
        String::from_utf8_lossy(&explain.stderr)
    );
    let stdout = String::from_utf8_lossy(&explain.stdout);
    assert!(
        stdout.contains("default override by plugin acme.coreover"),
        "core override must win and be attributed:\n{stdout}"
    );
    assert!(
        stdout.contains("built-in default"),
        "chain must show the losing built-in default:\n{stdout}"
    );

    // The override stops applying the moment the plugin is disabled.
    h.run_cli(&["plugin", "disable", "acme.coreover"]);
    let explain = h.run_cli(&["settings", "explain", "session.strict_hotkeys"]);
    let stdout = String::from_utf8_lossy(&explain.stdout);
    assert!(
        stdout.contains("resolved from: built-in default"),
        "disabled plugin must not override:\n{stdout}"
    );

    h.run_cli(&["plugin", "uninstall", "acme.coreover"]);
}

#[test]
#[serial]
fn test_outdated_reports_path_source_drift() {
    let h = TuiTestHarness::new("plugin_outdated");
    let source = tempfile::tempdir().expect("plugin source dir");
    std::fs::write(
        source.path().join("aoe-plugin.toml"),
        r#"
id = "acme.outdated"
name = "Outdated Fixture"
version = "1.0.0"
api_version = 1
"#,
    )
    .unwrap();
    let dir = source.path().to_str().unwrap();
    let install = h.run_cli(&["plugin", "install", dir, "--yes"]);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let fresh = h.run_cli(&["plugin", "outdated"]);
    let stdout = String::from_utf8_lossy(&fresh.stdout);
    assert!(
        stdout.contains("acme.outdated") && stdout.contains("up to date"),
        "fresh install must be up to date:\n{stdout}"
    );

    std::fs::write(source.path().join("new-file.txt"), "drift").unwrap();
    let drifted = h.run_cli(&["plugin", "outdated"]);
    let stdout = String::from_utf8_lossy(&drifted.stdout);
    assert!(
        stdout.contains("update available"),
        "source drift must report an available update:\n{stdout}"
    );

    h.run_cli(&["plugin", "uninstall", "acme.outdated"]);
}

/// Regression for the grafted-command dispatch bug: a plugin CLI command is
/// unknown to the core clap derive, which parses it as `command: None`. The
/// dispatcher must claim it and route to the worker instead of falling
/// through to the bare-`aoe` TUI launch ("requires an interactive TTY").
#[cfg(unix)]
#[test]
#[serial]
fn test_grafted_cli_command_dispatches_to_worker() {
    use std::os::unix::fs::PermissionsExt;

    let h = TuiTestHarness::new("plugin_graft_dispatch");
    let source = tempfile::tempdir().expect("plugin source dir");
    std::fs::write(
        source.path().join("aoe-plugin.toml"),
        r#"
id = "acme.cmd"
name = "Command Fixture"
version = "1.0.0"
api_version = 1

[[commands]]
path = ["acmedemo", "ping"]
about = "ping the worker"
rpc_method = "demo.ping"

[runtime]
entrypoint = "worker.sh"
"#,
    )
    .unwrap();
    // Portable ndjson JSON-RPC worker: echo a result with the request's id.
    let worker = source.path().join("worker.sh");
    std::fs::write(
        &worker,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p')\n  [ -z \"$id\" ] && continue\n  printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":\"GRAFTED_OK\"}\\n' \"$id\"\ndone\n",
    )
    .unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o755)).unwrap();

    let install = h.run_cli(&[
        "plugin",
        "install",
        source.path().to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        install.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    let output = h.run_cli(&["acmedemo", "ping"]);
    assert!(
        output.status.success(),
        "grafted command did not dispatch (fell through to TUI?): {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("GRAFTED_OK"),
        "worker result missing from grafted command output:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    h.run_cli(&["plugin", "uninstall", "acme.cmd"]);
}

/// `aoe.web` is a default plugin; disabling it must turn off the serve
/// surface at runtime. The gate bails in the foreground invocation before any
/// daemon spawn, and re-enabling restores it.
#[test]
#[serial]
fn test_serve_refuses_when_web_plugin_disabled() {
    let h = TuiTestHarness::new("plugin_serve_gate");
    let free_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();

    let disable = h.run_cli(&["plugin", "disable", "aoe.web"]);
    assert!(disable.status.success(), "disable aoe.web failed");

    let refused = h.run_cli(&["serve", "--daemon", "--port", &free_port, "--no-auth"]);
    assert!(
        !refused.status.success(),
        "serve must refuse while aoe.web is disabled"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("web dashboard plugin is disabled"),
        "refusal must name the fix:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    let enable = h.run_cli(&["plugin", "enable", "aoe.web"]);
    assert!(enable.status.success(), "enable aoe.web failed");

    let started = h.run_cli(&["serve", "--daemon", "--port", &free_port, "--no-auth"]);
    assert!(
        started.status.success(),
        "serve must start once aoe.web is enabled:\n{}",
        String::from_utf8_lossy(&started.stderr)
    );
    h.run_cli(&["serve", "--stop"]);
}

/// The command palette opens the plugin manager, which lists the bundled
/// plugins with their trust and state. Palette-only (no default chord).
#[test]
#[serial]
fn test_palette_opens_plugin_manager_listing_builtins() {
    require_tmux!();

    let mut h = TuiTestHarness::new("plugin_manager_palette");
    h.spawn_tui();

    h.wait_for(" aoe ");
    h.send_keys("C-k");
    h.wait_for("Commands");
    h.type_text("plugins");
    h.wait_for("Manage plugins");
    h.send_keys("Enter");

    // The browse dialog lists builtins by their manifest name + trust.
    h.wait_for("Agent Status Detection");
    h.assert_screen_contains("builtin");
}

#[test]
#[serial]
fn test_builtin_worker_answers_status_batch() {
    let h = TuiTestHarness::new("plugin_worker_batch");
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"status.detect_batch","params":{"snapshots":[{"session_id":"s1","agent":"codex","pane_text":"Working (esc to interrupt)"}]}}"#;
    let output = h.run_cli_with_stdin(&["__plugin-worker", "--id", "aoe.status"], request);
    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"session_id\":\"s1\"") && stdout.contains("\"status\""),
        "expected a per-snapshot result:\n{stdout}"
    );
}
