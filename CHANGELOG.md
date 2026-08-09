# Changelog

## 0.4.0

- Add Windows support (`platforms = ["linux", "macos", "windows"]`) — compiles
  from source via a `[[build]]` `cargo build --release` hook (no prebuilt
  `dist/` artifact for this platform); every manifest entry gets a
  `-win`-suffixed startup/event/action variant that launches
  `target\release\cache-ttl.exe` through a PowerShell wrapper resolving
  `HERDR_PLUGIN_ROOT`
- Windows socket transport: `socket_send` writes to the named pipe
  `\\.\pipe\<HERDR_SOCKET_PATH>` (`CreateFileW` + `WriteFile`, with an
  `ERROR_PIPE_BUSY` retry) so `sort-win` and daemon-startup sort reapplication
  reach herdr the same way the Unix `UnixStream` path does
  (`toggle_sort`/`sort`/`sort-win` and `reapply_sort` are the only callers)
- Windows state lock: `LockFileEx`/`UnlockFileEx` replace `flock` as the
  cross-process exclusive lock guarding `timers.json`/`notified.json`
- Windows daemon liveness hardening: pid-file liveness now also confirms the
  pid's image name ends in `cache-ttl.exe` via `QueryFullProcessImageNameW`,
  so a recycled Windows pid can no longer be mistaken for a live daemon (a
  stale pid file now self-heals on next start instead of blocking the daemon
  forever); the tick loop also now gives up and exits cleanly after 10
  consecutive failed ticks instead of spinning forever against a permanently
  unreachable herdr server (applies to both platforms)
- Windows named-pipe write now checks `WriteFile`'s byte count and errors on
  a partial write instead of always returning success

## 0.3.2

- Resolve the herdr binary via `HERDR_BIN_PATH` instead of bare `herdr` on
  `PATH` — a server started from a non-login shell (an install under
  `~/.local/bin`, say) left every CLI call failing to exec, so no tokens were
  ever pushed and the sidebar stayed empty on that host
- `set_pane_tokens` now treats a non-zero herdr exit as an error rather than
  success, and the daemon's back-off log includes the underlying error

## 0.3.1

- Ship prebuilt binaries in `dist/` and select one via a `[[build]]` hook —
  installing from GitHub previously left no binary at all, so the daemon and
  every event hook failed with "No such file or directory"
- Remove the pre-rewrite shell implementation (`daemon.sh` and friends), unused
  since 0.3.0
- Deploy script: pass `--yes` to `plugin install` (after the repo arg, which the
  server-side handler requires) and stop swallowing install failures

## 0.3.0

- Rewrite in Rust — 396KB static binary, no jq/bash runtime dependency
- flock(2)-based file locking replaces mkdir hack, eliminating shared-state races
- Fix 5m and 1m notifications never firing (only 10m worked)
- Fix sort toggle targeting wrong socket (HERDR_SOCKET → HERDR_SOCKET_PATH)
- Sort toggle now fails loudly instead of flipping state on API error
- Handle pane.closed event alongside pane.exited
- PID guard prevents duplicate daemons on server reload
- Sort view reapplied automatically after server restart
- Validate user config values (reject non-positive integers)
- Deploy script no longer creates invalid duplicate TOML tables
- Bump min_herdr_version to 0.7.5

## 0.2.0

- Add cache sort mode — toggle via `herdr plugin action invoke sort --plugin cache-ttl` to sort the agent panel by cache TTL urgency (warmest first), using herdr's agent view API
- Fix timers showing 0m for actively working agents — the daemon now refreshes timers for agents in `working` status, not only on status transitions
- Emit `cache_sort` token (zero-padded seconds remaining) for sortable ordering

## 0.1.0

- Initial release
- Live prompt-cache countdown per agent pane
- Color-coded sidebar tokens (green/amber/red)
- Notifications at 10m, 5m, and 1m remaining
- Configurable TTL, tick interval, and thresholds
- Stale pane cleanup and pane close handling
