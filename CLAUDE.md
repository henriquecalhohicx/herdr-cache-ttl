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

## herdr integration (learned porting this plugin — verified on herdr 0.8.0-preview.2026-08-04-d78e3d3b5126)

- **Socket transport (`sort`/`sort-win` actions only):** `show_timers`
  (`status`/`status-win`) never touches the socket — only
  `toggle_sort`/`sort`/`sort-win` and the daemon's `reapply_sort` call
  `socket_send`. `HERDR_SOCKET_PATH` is a filesystem-style path; the real
  Windows endpoint is the named pipe `\\.\pipe\` + that path. `socket_send`
  only ever writes one line and returns, so the Windows implementation is a
  plain `CreateFileW` + `WriteFile`, not a full duplex transport, and has no
  write timeout (see gaps below).
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
- **Not exercised — Windows `[[build]]` via herdr's own install/build flow.**
  The `cargo build --release` `[[build]]` entry has never actually been
  triggered by herdr's plugin-install/build machinery; the binary so far has
  only ever been built by hand (`cargo build --release` run directly). This
  assumes `cargo` is on the herdr server process's PATH (not just the
  interactive shell's) and that no `CARGO_TARGET_DIR` env var is redirecting
  build output away from `target\release` — every Windows launcher above
  hardcodes that exact path.
- **Not exercised — cross-process file-lock contention.** The Windows
  `LockFileEx`/`UnlockFileEx` state lock (`src/state.rs`) has only been
  proven to round-trip acquire+drop within a single process
  (`lock_is_exclusive_within_one_process`); two real `cache-ttl` invocations
  actually racing a save has not been run live on either platform. Known
  gap, not attempted in this pass.
- **Known limitation — Windows `socket_send` has no write timeout.** Unlike
  the Unix path's `set_write_timeout(2s)`, the named-pipe `WriteFile` has no
  timeout, so a stalled herdr reader could block `reapply_sort` indefinitely
  at daemon startup. Fixing this needs overlapped I/O; deferred as
  out-of-scope for a cheap fix.

## Behavior change (2026-08-15) — no countdown while working

Previously, `daemon.rs::refresh_working` reset a working pane's timer to full
`ttl_seconds` on every tick (~15s) for as long as `agent_status` stayed
`"working"`, and `handlers.rs::status_change` reset to full on *any* status
transition. Net effect: any actively-worked pane showed a static, near-full
countdown (e.g. `60m`) the entire time it was working — cosmetically
confusing next to herdr's own `working` status icon, since the two signals
said the same thing twice, one of them looking broken.

Changed to: a `"working"` pane shows **no cache badge at all** (all
`cache_ok`/`cache_warn`/`cache_crit`/`cache_sort` tokens cleared, timer
entry deleted) — `handlers.rs::status_change` now branches on
`turn_action_for(agent_status)`: `"working"` clears, anything else starts a
fresh countdown from that transition. `daemon.rs::refresh_working` was
renamed `clear_working` and repurposed as a safety net (sweeps any
currently-working pane whose tokens/timer weren't already cleared, e.g. if
the daemon restarted mid-turn) rather than the primary mechanism. This
diverges from upstream `nytafar/herdr-cache-ttl`, which does not have this
distinction.

**Gotcha — restarting the daemon after a rebuild.** `herdr plugin disable
<id>` / `enable <id>` does **not** re-run `[[startup]]` on this herdr
version (confirmed empirically: the daemon process stayed dead through a
disable/enable cycle, while one-shot `[[events]]`/`[[actions]]` invocations
kept working fine throughout — those are spawned per-call with full env
regardless of daemon state). `[[startup]]` only appears to fire on
herdr-server start. To pick up a rebuilt binary without restarting the
whole herdr session: stop the old process, then launch the new exe's
`daemon` subcommand directly with its required env vars set by hand —
`HERDR_PLUGIN_STATE_DIR` (`%LOCALAPPDATA%\herdr\plugins\cache-ttl`),
`HERDR_PLUGIN_CONFIG_DIR` (`herdr plugin config-dir cache-ttl`), and
optionally `HERDR_BIN_PATH` (`(Get-Command herdr).Source`) so
`herdr_bin()` doesn't fall back to a bare-name PATH lookup. `daemon.pid`
self-heals on the next successful start (see `pid_alive`'s image-name
check), so the stale pid left behind by a hard `Stop-Process` doesn't need
manual cleanup.

## Feature (2026-10-19) — Claude account badge

The daemon also tags every Claude pane with the Claude Code account it runs
under, as one of `acct_c1`/`acct_c2`/.../`acct_c9` (value `c1`, `c2`, ...),
so the sidebar row can show the same `cN` badge the Claude Code statusline
does, coloured per account. Not in upstream.

- **Resolution:** herdr's `agent list` gives each pane's
  `agent_session.value` (session id) but not its config dir.
  `account::find_account` looks for `<id>.jsonl` under
  `~\.claude*\projects\*\` — `.claude` -> 1, `.claudeN` -> N. Only hits
  are cached (per session id, in-memory); a new session whose transcript
  doesn't exist yet is retried next tick.
- **Reporting:** separate metadata source `plugin:cache-ttl-account`, no
  `--ttl-ms` (a no-TTL token persists — verified live), and only re-sent
  when the pane's current tokens don't already show exactly that account.
  Runs every tick, before the empty-timers early exit, so the badge stays
  while a pane is `working` (when the countdown tokens are cleared).
- **Sidebar:** `%APPDATA%\herdr\config.toml` `[ui.sidebar.agents]` renders
  `$acct_c1`/`$acct_c2`/`$acct_c3` before the countdown, in the statusline's
  colours (green `#00d700`, red `#ff5f5f`, blue `#0087ff`). A 4th+ account
  emits a token but needs its own row entry to show.
- Verified live 2026-10-19: all 10 open Claude panes tagged, matching a
  manual transcript lookup (4 c1, 3 c2, 3 c3).

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
- `src/account.rs` — session id -> Claude Code account number lookup for
  the `acct_cN` badge.
