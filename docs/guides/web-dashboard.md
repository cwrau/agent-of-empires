# Web Dashboard

Monitor and drive agent sessions from any browser. The dashboard is an embedded server inside the `aoe` binary; start it with `aoe serve`. Sessions run server-side (a real tmux session, or a persistent worker for structured-view sessions), so they survive browser crashes, network drops, and reconnects.

![The web dashboard on desktop: workspace sidebar, live agent terminal, and diff panel](../assets/web/dashboard.png)

This page covers running the server, access modes, security, and PWA install. The surface itself has its own pages:

- **[Dashboard & workspaces](web/dashboard.md)**: layout, the session wizard, sorting and grouping, triage, settings and profiles, mobile behavior.
- **[Terminal view](web/terminal.md)**: agent and paired terminals, reconnect behavior, read-only mode.
- **[Diff view](diff-view.md#in-the-web-dashboard)**: reviewing changed files and commenting on them.

The dashboard ships in every release binary, so `aoe serve` just works. Building from source needs the `web` Cargo feature and Node; a plain `cargo build` still serves the API, with no dashboard behind it.

## Starting the server

```bash
aoe serve                       # Localhost only (default)
aoe serve --remote              # Remote over HTTPS (Tailscale Funnel, else Cloudflare)
aoe serve --host 0.0.0.0        # LAN/VPN access over HTTP (use a VPN)
aoe serve --daemon              # Run in the background (--stop to stop it)
aoe serve --open                # Open the URL in a browser when ready
aoe serve --remote --read-only  # Monitor without sending keystrokes
```

The server prints a URL carrying an auth token; the token becomes a cookie on first visit. `--open` is suppressed with `--daemon`, `--remote`, and whenever no browser you could see is reachable (SSH without `DISPLAY`, a Linux host with no display server); setting `BROWSER` overrides that check except on macOS.

In `--remote` mode the token rotates every 4 hours, so a URL captured at startup eventually stops working. `aoe url` prints the live one against a running daemon (`--all` for every labeled URL, `--token-only` for scripted login), and `--remote` prints a QR code for phone pairing.

## Remote access

`--remote` is the recommended way to reach the dashboard from a phone. aoe picks a transport in this order.

**1. Tailscale Funnel** (preferred). If `tailscale` is on `PATH` and logged in, aoe runs `tailscale funnel --bg --yes <port>` and serves from your stable `https://<machine>.<tailnet>.ts.net` URL. This is the only option where a PWA installed on your phone keeps working across server restarts. One-time setup (aoe surfaces the fix when a gate is missing): install Tailscale and run `tailscale up`, enable Funnel for your tailnet at [login.tailscale.com/f/funnel](https://login.tailscale.com/f/funnel), then grant the node the `funnel` nodeAttr in your [ACL](https://login.tailscale.com/admin/acls/file), for example `{ "target": ["autogroup:member"], "attr": ["funnel"] }`. If port 443 already carries a non-loopback Funnel service, aoe refuses to start rather than replace it; clear it with `tailscale funnel reset` or pass `--no-tailscale`.

**2. Named Cloudflare tunnel.** A stable hostname on your own domain, and it takes precedence over Tailscale when you pass the flags:

```bash
cloudflared tunnel create my-tunnel
# Add a CNAME: aoe.example.com -> <tunnel-id>.cfargotunnel.com
aoe serve --remote --tunnel-name my-tunnel --tunnel-url aoe.example.com
```

**3. Cloudflare quick tunnel** (fallback, needs `cloudflared` on the host). Zero-config, but the URL rotates on every restart, which breaks an installed PWA. aoe prints a notice when it falls back here.

## Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | 8080 | Port to listen on |
| `--host` | 127.0.0.1 | Bind address; `0.0.0.0` for LAN/VPN access |
| `--auth` | `token` | `token` (URL token), `passphrase` (login wall only), `none` (loopback only unless `--behind-proxy`). `--no-auth` is an alias for `none` |
| `--passphrase` | | Passphrase for the login wall, also read from `AOE_SERVE_PASSPHRASE`. Valid with `--auth=token` and `--auth=passphrase` |
| `--behind-proxy` | off | An external proxy terminates TLS: sets `; Secure` cookies and trusts `X-Forwarded-For` / `cf-connecting-ip` from loopback peers. Spawns no tunnel and requires at least one `--allowed-host` |
| `--allowed-host` | | Extra `Host` the [DNS-rebinding gate](#dns-rebinding) accepts (repeatable) |
| `--allowed-origin` | | Extra browser `Origin` to accept (repeatable, full `scheme://host[:port]`); only needed for a proxy on a nonstandard port |
| `--remote` | off | Expose over an HTTPS tunnel |
| `--tunnel-name` / `--tunnel-url` | | Use a named Cloudflare tunnel and its hostname (with `--remote`) |
| `--no-tailscale` | off | Skip Tailscale auto-detection and use Cloudflare |
| `--read-only` | off | View terminals but send no keystrokes |
| `--daemon` / `--stop` | off | Fork to the background, or stop a running daemon |

`--auth=passphrase` and `--auth=none` on a non-loopback bind require `--behind-proxy`; `--auth=passphrase` requires a passphrase; `--auth=none` with a passphrase is rejected; and `--remote` refuses both reduced modes, since a public tunnel mandates token auth plus a passphrase. The TUI structured view has no passphrase exchange, so keep `--auth=token` on daemons you also drive from the local TUI.

### Behind a reverse proxy

When TLS is terminated upstream (Traefik, nginx, Caddy) and forwarded to a loopback `aoe serve`:

```bash
aoe serve --host 127.0.0.1 --port 42041 \
  --auth=passphrase --passphrase "$AOE_PASSPHRASE" \
  --behind-proxy --allowed-host aoe.example.com
```

The upstream must set `X-Forwarded-For` (or `cf-connecting-ip`); aoe reads the last value as the client IP, and only when the socket peer is loopback, so a misconfigured upstream cannot spoof it. `--behind-proxy` also withdraws the same-host bypass, so a browser on the daemon's own host signs in with the passphrase like any other client. Add `--allowed-origin https://aoe.example.com:8443` when the proxy listens on a nonstandard port. Both flags are replayed across `aoe serve --restart`.

## Security

**The dashboard exposes terminal access.** Anyone who authenticates can send keystrokes to your agent sessions, which run as your user.

- **Token auth** (default): a random 256-bit token generated at startup and stored in `serve.token` in the app dir, passed by URL on first visit, then kept as an `HttpOnly; SameSite=Strict` cookie.
- **Passphrase wall**: an argon2-hashed passphrase gates `/login`, and sessions bind to a per-device secret in `localStorage`, so a leaked cookie alone is not enough. Five failed logins from an IP trigger a 15-minute lockout.
- **Token rotation**: in `--remote` mode the token rotates every 4 hours, with a 5-minute grace for active sessions.
- **Connected devices**: signed-in devices (browser, origin IP, last seen) are listed under Settings > Web Dashboard > Connected Devices, where you can revoke one or sign every device out.
- **Session persistence**: login sessions persist to an owner-only `login_sessions.toml`, so devices stay signed in across a daemon restart. A passphrase change drops them all; `auth.persist_sessions = false` opts out.
- **Step-up elevation**: writes that could plant code for, or widen, the next session spawn need a passphrase confirmation valid for 15 minutes. That covers the `sandbox` and `worktree` sections plus `acp.restrict_agents`, `skills.auto_propagate`, `session.smart_rename_model`, and `session.inherit_host_environment`; the gate is per field, and localhost browsers skip it entirely, since a same-host caller already passes the filesystem trust boundary.
- **Local-only fields**: agent commands and status-hook shell commands map names to arbitrary host commands, so the server rejects any PATCH touching them. Edit those in the TUI on the host.

Responses carry `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, and `Referrer-Policy: no-referrer` (the last keeps tokens out of `Referer`).

### DNS rebinding

`aoe serve` validates `Host` and `Origin` before authentication: an unlisted `Host`, or any `Origin` that is sent and not allowlisted (including the opaque `Origin: null`), gets `403`. A request with no `Origin` is exempt from the origin check. The allowlist is derived automatically from `localhost`, `127.0.0.1`, `::1`, a concrete `--host`, any routable IP literal (an IP is dialed directly and cannot be rebound), and `--remote` tunnel hostnames. The unspecified, link-local, and multicast addresses are excluded and cannot be allowlisted at all.

So a wildcard bind is reachable by its LAN or tailnet **IP** with no extra flag. Only reaching it by a **hostname** (mDNS, MagicDNS, custom DNS) needs `--allowed-host`, because a hostname is what a rebinding attacker controls:

```bash
aoe serve --host 0.0.0.0 --allowed-host my-box.tailnet.ts.net
```

### Safe usage patterns

- **Localhost** (`aoe serve`): same security as the TUI.
- **Remote via tunnel** (`--remote`): HTTPS; recommended for phone access.
- **Over a VPN** (`--host 0.0.0.0` on Tailscale/WireGuard): the VPN encrypts.
- **Behind a reverse proxy** (`--auth=passphrase --behind-proxy`): TLS upstream, passphrase as the only human gate.
- **Read-only** (`--read-only`): monitor without input.

Refused outright: `--auth=none --host 0.0.0.0` without `--behind-proxy`, and `--remote` with either reduced auth mode. Plain `--host 0.0.0.0` on public WiFi is unencrypted HTTP; use a VPN or a tunnel.

## Installing as a PWA

The dashboard installs as a Progressive Web App: Chrome's three-dot menu > "Install Agent of Empires", Safari's File > Add to Dock, or Share > Add to Home Screen on iOS. Install it from a Tailscale Funnel or named-Cloudflare URL, since a home-screen app is bound to its install URL and a quick tunnel's rotates.

Keep the server up with `--daemon`. Reopening the PWA returns to the session you last had open (remembered per device), or the dashboard if that session is gone. Stopping the server exits within about five seconds even with open tabs; live clients get a `1001` close frame and reconnect once a fresh server is up.

## CityHall client mode

`AOE_CITYHALL_MODE=1 aoe serve --host 0.0.0.0` (or `--cityhall`) starts the dashboard as a locked-down end-user client: only the message composer and the structured view are reachable. Terminal and diff panes, project and profile management, plugin lifecycle, and agent/worker controls are hidden in the UI and refused server-side, so a direct API or WebSocket call cannot reach them either. Every mutating route is default-deny unless explicitly allowlisted, session creation is server-derived, and the session list is filtered to the structured sessions the mode creates. Settings are curated down to Theme, a delete-to-trash toggle, MCP servers and Plugins (display only), and Telemetry.

New sessions are created by name only; each spans every configured project and runs the default agent in structured view, so that agent must be ACP-capable and at least one project must be configured. The mode is persisted in `serve.launch`, so it survives `aoe serve --restart` and the post-`aoe update` re-exec.

### The CityHall config bundle

A locked-down client cannot configure itself, so a bundle does it: one TOML document describing a workspace's settings and projects.

```toml
schema_version = 1

[settings.acp]
default_agent = "claude-code"

[[projects]]
name = "cityhall"
remote = "https://github.com/agent-of-empires/cityhall.git"
default_base_branch = "main"
```

Projects are addressed by **git remote**, not path: `apply` clones each remote into `<app_dir>/repos/<name>` and registers it. Settings are a sparse patch, keyed section then field, validated like a `PATCH /api/settings` body.

Produce one with `aoe cityhall export --out cityhall.toml` or from the dashboard's **Settings > CityHall** tab (host-specific and local-only fields are omitted, and no credential is ever exported), and apply it with `aoe cityhall apply cityhall.toml`. Applying is idempotent: an existing checkout is left untouched so uncommitted work survives, an already-registered project is not re-added, and a repo that fails to clone is reported without taking the others down. A bundle sets values; it cannot unset them.

To have a workspace fetch its bundle at startup, set `AOE_CITYHALL_BUNDLE_URL` (and `AOE_CITYHALL_BUNDLE_TOKEN` for the bearer token). The fetch happens before any config is read. On a first boot a fetch failure is fatal, rather than leaving a user in a workspace with no projects; once a bundle has been applied it is cached, and a later failure only warns and serves the cached configuration. A malformed bundle, or one naming an unknown setting, is fatal either way.

A git identity and credential arrive in the same document (`[git]`), which is what makes clone, pull, and push work inside a workspace. `export` never writes that section; the host serving the bundle composes it per user.

### Behind a reverse proxy

When TLS is terminated by an external proxy (Traefik, nginx, Caddy) forwarding to `aoe serve` on loopback (often through an SSH reverse tunnel), use `--behind-proxy` so cookies carry `; Secure` and the rate limiter keys by the real client IP:

```bash
aoe serve \
  --host 127.0.0.1 --port 42041 \
  --auth=passphrase --passphrase "$AOE_PASSPHRASE" \
  --behind-proxy \
  --allowed-host aoe.example.com
```

The upstream must set `X-Forwarded-For` (or `cf-connecting-ip`); aoe reads the last value as the client IP. The trust check fires only when the socket peer is loopback, so a misconfigured upstream that lets requests reach aoe directly cannot spoof the IP.

With `--auth=passphrase --behind-proxy` the passphrase wall applies to loopback callers too. Proxied traffic arrives on a loopback socket, so an upstream that forgets the header would otherwise hand every visitor the same-host bypass. A browser on the same host signs in with the passphrase like any other client, and so does the local TUI and every `aoe acp <verb>` CLI command: they read the daemon's own `serve.passphrase` file to log in via the same `/api/login` handshake and cache the resulting session, the same way they already reuse `serve.token` under `--auth=token`.

`--behind-proxy` requires at least one `--allowed-host <public-hostname>`: aoe cannot infer the hostname your proxy forwards, and the [DNS-rebinding gate](#dns-rebinding) rejects any `Host` it does not recognize. If the proxy listens on a nonstandard port, also pass the exact origin, e.g. `--allowed-origin https://aoe.example.com:8443`. The daemon refuses to start (with an explicit message) if `--behind-proxy` is set without `--allowed-host`.

## Security

**The dashboard exposes terminal access.** Anyone who authenticates can send keystrokes to your agent sessions, which run as your user.

### Authentication

- **Token auth** (`--auth=token`, default): a random 256-bit token, generated on startup and stored at `~/.config/agent-of-empires/serve.token` (Linux) or `~/.agent-of-empires/serve.token` (macOS). Passed via URL on first visit, then kept as an `HttpOnly; SameSite=Strict` cookie.
- **Passphrase wall** (`--auth=passphrase`, or combined with token via `--passphrase`): an argon2-hashed passphrase gates `/login`. Sessions bind to a per-device secret in `localStorage`, so a leaked cookie alone is insufficient.
- **Rate limiting**: 5 failed logins from an IP trigger a 15-minute lockout.
- **Token rotation**: in `--remote` mode the token rotates every 4 hours with a 5-minute grace period for active sessions.
- **Device tracking**: connected devices (the signed-in login sessions, with browser, origin IP, and last seen) are visible in Settings > Web Dashboard > Connected Devices, where you can revoke one device or sign every device out.
- **Session persistence**: login sessions are persisted to an owner-only `login_sessions.toml` in the app dir, so signed-in devices survive an `aoe serve` restart instead of being re-prompted for the passphrase. A passphrase change drops every persisted session; set `auth.persist_sessions = false` to force re-authentication on every restart.
- **Step-up elevation**: a "Confirm passphrase" prompt appears on writes that can plant code for, or widen what is exposed to, the next session spawn. That covers the `sandbox` and `worktree` sections plus individual fields that carry the same risk, currently `acp.restrict_agents`, `skills.auto_propagate`, `session.smart_rename_model`, and `session.inherit_host_environment`; the gate is per field, so the rest of a section saves without it. Confirmation lasts 15 minutes. User-preference writes (theme, sound, notifications, etc.) save without it. Localhost browsers skip the prompt entirely; the same-host caller already passes the filesystem trust boundary. See [Settings & profiles](web/settings.md#step-up-elevation).
- **Local-only fields**: the agent-command surface and status-hook shell commands map names to arbitrary host commands, so the server rejects any PATCH touching them; they are editable only in the TUI on the host.

The server also sets `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, and `Referrer-Policy: no-referrer` (the last prevents token leaks via Referer).

### DNS rebinding

`aoe serve` validates the `Host` and `Origin` of every request before authentication: a request whose `Host` is unlisted, or whose browser `Origin` is present but unlisted, gets `403 Forbidden`. A request with no `Origin` header is exempt from the origin check; any `Origin` that is sent, including the opaque `Origin: null`, is rejected unless allowlisted, and since `--allowed-origin` requires a full `scheme://host[:port]`, `Origin: null` can never be allowlisted. This closes the DNS-rebinding vector: a page that rebinds its hostname to your machine's IP still sends that hostname as `Host`, which is not in the allowlist.

The allowlist is derived automatically:

- `localhost`, `127.0.0.1`, and `::1` are always accepted, plus the value of `--host` when it is a concrete (non-wildcard) address.
- Any routable **IP literal** `Host`/`Origin` (LAN, tailnet `100.x`, ULA, global) is accepted unconditionally: an IP is dialed directly and never DNS-resolved, so it cannot be rebound. The unspecified address (`0.0.0.0` / `::`), link-local (`169.254.0.0/16`, `fe80::/10`), and multicast are excluded from this automatic trust and cannot be allowlisted at all: `--allowed-host` / `--allowed-origin` reject them at startup, since allowlisting one would reopen the hole the gate closes.
- `--remote` tunnels (Cloudflare and Tailscale) inject their public hostname and its `https://` origin, so remote dashboards and the live terminal WebSocket work with no extra flag.
- `--allowed-host` / `--allowed-origin` add operator-declared entries (see below).

A wildcard bind (`--host 0.0.0.0` / `::`) is therefore reachable by its LAN/tailnet **IP** with no extra flag. Only reaching it by a **hostname** (mDNS `.local`, Tailscale MagicDNS name, custom DNS) needs an explicit `--allowed-host`, because a hostname is what a DNS-rebinding attacker controls:

```bash
# Add a NAME only to reach it by hostname:
aoe serve --host 0.0.0.0 --allowed-host my-box.tailnet.ts.net
```

### `--allowed-host` for a reverse proxy or custom tunnel

For a reverse proxy or a manually managed tunnel (anything that is not `--remote`), aoe cannot infer the public hostname, so declare it explicitly. Standard 80/443 origins for each `--allowed-host` are derived automatically; only a proxy on a nonstandard port needs an explicit `--allowed-origin`:

```bash
# Reverse proxy terminating TLS on the standard 443
aoe serve --host 127.0.0.1 --behind-proxy --allowed-host aoe.example.com

# Reverse proxy on a nonstandard port
aoe serve --host 127.0.0.1 --behind-proxy \
  --allowed-host aoe.example.com \
  --allowed-origin https://aoe.example.com:8443
```

Both flags are repeatable and are replayed across `aoe serve --restart`, so a restart preserves the posture.

### Safe usage patterns

- **Localhost** (`aoe serve`): same security as the TUI.
- **Remote via tunnel** (`aoe serve --remote`): encrypted via HTTPS. Recommended for phone access.
- **Over Tailscale/WireGuard** (`aoe serve --host 0.0.0.0`): the VPN encrypts traffic; reach it directly at `http://<tailnet-or-lan-ip>:8080`.
- **Behind a reverse proxy** (`--auth=passphrase --behind-proxy`): TLS terminated upstream; passphrase is the only human gate.
- **Read-only** (`aoe serve --remote --read-only`): monitor without input.

### Dangerous (blocked)

- `aoe serve --host 0.0.0.0` on public WiFi without a VPN: traffic is unencrypted HTTP.
- `aoe serve --auth=none --host 0.0.0.0` (or `--no-auth --host 0.0.0.0`): refuses to start without `--behind-proxy`.
- `aoe serve --auth=none --remote` or `--auth=passphrase --remote`: refuses to start.

## Installing as a PWA

The dashboard installs as a Progressive Web App for an app-like, standalone window:

- **macOS (Chrome)**: three-dot menu > "Install Agent of Empires".
- **macOS (Safari)**: File > Add to Dock.
- **iOS**: Share > Add to Home Screen.
- **Android (Chrome)**: "Add to Home Screen" prompt or install banner.

The PWA needs the server running; use `--daemon` to keep it up (`aoe serve --stop` to stop). For a stable URL that survives restarts, install from a Tailscale Funnel or named-Cloudflare URL, not a quick tunnel.

When you leave the PWA and come back, it reopens to the session you last had open rather than the dashboard. The last session is remembered per device (not synced across devices); if you were on the dashboard when you left, or that session no longer exists, you land on the dashboard.

`Ctrl-C` on a foreground server, or `aoe serve --stop` against a daemon, both exit within ~5 seconds even with open tabs. Live clients receive a `1001` ("going away") close frame and reconnect once a fresh server is running.
