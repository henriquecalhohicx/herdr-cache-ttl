# CLAUDE.md

Guidance for working in this repo (a Rust herdr plugin: prompt-cache TTL
countdown per agent pane).

## Remotes

- `origin` (`henriquecalhohicx/…`) is the personal fork and the **only**
  push target — branch `windows-port` is the working branch.
- `upstream` (`nytafar/…`) is **read-only reference**: fetch works, do not
  push, do not open PRs against it, do not merge `upstream/main` without a
  specific reason (same rationale as the `herdr-pc-ram-and-cpu-usage-overlay`
  fork's `origin`/`fork` split, just with `gh repo fork`'s default remote
  names instead).

## Build / test (Windows dev)

- CI gate (must stay green): `cargo fmt --all --check`, `cargo clippy
  --all-targets -- -D warnings`, `cargo test --all`.
- The Windows `[[build]]` entry runs `cargo build --release` from source
  (there is no prebuilt Windows binary in `dist/`); linux/macos still copy a
  prebuilt artifact from `dist/`, unchanged.

## Cross-platform rules

- Linux/macos behavior stays byte-identical. Windows code is additive
  behind `#[cfg(windows)]`; the prior Unix-only code is now behind
  `#[cfg(unix)]`. `libc` is `[target.'cfg(unix)']`; `windows-sys` is
  `[target.'cfg(windows)']`.

## herdr integration (learned porting this plugin — verified on herdr
0.8.0-preview.2026-08-04-d78e3d3b5126)

- **Socket transport (`sort`/`status` actions):** `HERDR_SOCKET_PATH` is a
  filesystem-style path; the real Windows endpoint is the named pipe
  `\\.\pipe\` + that path. `socket_send` only ever writes one line and
  returns, so the Windows implementation is a plain `CreateFileW` +
  `WriteFile`, not a full duplex transport.
- **Manifest command spawn (Windows):** herdr runs commands with an
  extended-length `\\?\C:\..` cwd; `CreateProcessW` can't resolve a relative
  `./cache-ttl` exe against it. Windows manifest entries go through a
  PowerShell launcher that resolves `HERDR_PLUGIN_ROOT` (stripping `\\?\`)
  and invokes `target\release\cache-ttl.exe` by absolute path.
- herdr rejects duplicate action ids across platform variants even with
  disjoint `platforms` — hence the `-win`-suffixed ids.
- **Live smoke test used an isolated named session, not the real live
  session**, to avoid disturbing the human partner's active work:
  `herdr --session cache-ttl-smoke server` (run headless in the background)
  creates a session whose socket lives at
  `%APPDATA%\herdr\sessions\<name>\herdr.sock` (confirmed via `herdr session
  list`) — same shape as the default session's `%APPDATA%\herdr\herdr.sock`,
  just nested under `sessions\<name>\`. Point the CLI at it per-call with
  `$env:HERDR_SOCKET_PATH = "$env:APPDATA\herdr\sessions\<name>\herdr.sock"`.
  A throwaway pane was created with `herdr workspace create --cwd <path>
  --focus` (no real agent attached — `agent_status` stayed `unknown`
  throughout) purely so `reset-win`/`status-win` had a `pane_id` to attach a
  timer to; that was sufficient to exercise every Windows-specific code path
  (spawn, socket write, state file, PowerShell launcher) without a live
  Claude/Codex turn. Tear down with `herdr session stop <name>`.
- **`cache-ttl.reset-win`** (`herdr plugin action invoke reset-win --plugin
  cache-ttl`) returned `exit_code 0`, stdout `Timer reset for w1:p1\n` exactly
  as the linux/macos variant would — confirmed via `herdr plugin log list
  --plugin cache-ttl` (the CLI's own JSON response only echoes
  `status:"running"`; the captured `stdout`/`exit_code` show up in the log
  list a moment later, once the spawned PowerShell/exe has actually exited).
- **`cache-ttl.status-win`** returned the expected `PANE / TTL / TITLE`
  table with the just-reset pane at full TTL (`59m` — one minute below the
  configured 60m default, i.e. rounded down, not a bug).
- **`cache-ttl.sort-win` reaching herdr over the named pipe — confirmed.**
  Toggling on produced stdout `Cache sort on\n` *and* a matching
  `herdr-server.log` line `method="agent.view.set" outcome="ok"` timestamped
  ~1ms after the plugin process's own log entry — i.e. the `./cache-ttl.exe
  toggle-sort` process itself opened the named pipe and wrote the RPC, it
  didn't just print text. Toggling off produced stdout `Cache sort off\n`
  paired with `method="agent.view.clear" outcome="ok"` (not a second
  `agent.view.set` — the plugin clears the view override rather than setting
  it to an "off" state, which is the more correct API usage anyway).
- **Gotcha — daemon state is per-machine, not per-session.** `config.rs`
  resolves `HERDR_PLUGIN_STATE_DIR` from the env herdr injects, which
  resolved to the fixed path `%LOCALAPPDATA%\herdr\plugins\cache-ttl\`
  (`timers.json`, `notified.json`, `daemon.pid`) regardless of which named
  session's daemon was running. During the isolated-session smoke test,
  `status-win`'s table showed a second, stale pane id from an unrelated
  earlier session alongside the throwaway pane created for this test —
  isolating the *socket* per session does not isolate the *daemon's on-disk
  state*; every `cache-ttl` daemon on the box (default session or any named
  session) reads/writes the same `timers.json`. Not a bug, just something to
  know before trusting `status-win`'s output as scoped to one session.
- **Not exercised:** a real agent turn completing and the sidebar token
  updating visually (Step 1's original literal ask) — the isolated session's
  throwaway pane never had a real agent attached (`agent_status` stayed
  `unknown`), so `pane.report_metadata` calls from the daemon's own tick loop
  logged `outcome="error"` every ~15s in `herdr-server.log` for that pane
  (herdr rejecting metadata pushes to a pane it doesn't consider
  agent-owned). This is expected given no agent was attached, not a
  Windows-specific defect — the same would happen on linux/macos for an
  agent-less pane — but it means the sidebar-rendering half of Step 1 was
  verified structurally (daemon runs, holds a timer, serves `status-win`)
  rather than visually. Closing the throwaway workspace also did not appear
  to fire the `pane.closed`/`pane.exited` event hooks in `plugin log list`
  (no real terminal process ever ran in the pane), so those two Windows
  event twins were not exercised live in this pass — their PowerShell launcher
  is byte-identical in shape to the startup/action launchers that *were*
  confirmed working, so risk is judged low, but it is an honest gap.

## Layout

- `src/state.rs` — timer/notification persistence + cross-process file
  lock (`flock` on unix, `LockFileEx` on windows).
- `src/herdr.rs` — herdr CLI invocations (`Command::new(herdr_bin())`) plus
  the one raw-socket call (`socket_send`, unix domain socket / windows named
  pipe) used only by the `sort` feature.
- `src/daemon.rs` — the long-running countdown loop, pid-file liveness
  check, and log timestamps.
- `src/handlers.rs` — one-shot CLI subcommands invoked by manifest
  actions/events.
- `src/config.rs` — env-var + optional JSON overlay config.
