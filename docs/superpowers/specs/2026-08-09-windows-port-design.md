# Windows port — design

## Context

`cache-ttl` (a.k.a. "Cache TTL Countdown") is a herdr plugin that tracks each
agent pane's prompt-cache countdown and writes it back to herdr as sidebar
tokens. It currently declares `platforms = ["linux", "macos"]` and its
`[[build]]` step copies a prebuilt binary from `dist/` rather than compiling —
`dist/` ships `cache-ttl-linux-x86_64` and `cache-ttl-darwin-arm64` only, no
Windows artifact.

The source (`src/*.rs`) uses four Unix-only surfaces that currently prevent
it from even compiling on Windows:

1. `herdr::socket_send` — connects to herdr's own socket
   (`$HERDR_SOCKET_PATH`) via `std::os::unix::net::UnixStream` to send
   `agent.view.set`/`agent.view.clear` for the `sort` action.
2. `state::Lock` — uses `libc::flock` (via `AsRawFd`) to serialize writes to
   `timers.json`/`notified.json` between the daemon and one-shot CLI
   invocations (`reset-timer`, etc.).
3. `daemon::run` — registers `SIGTERM`/`SIGINT` handlers via `libc::signal`,
   checks daemon liveness via `libc::kill(pid, 0)`, and formats log
   timestamps via `libc::time`/`libc::localtime`.
4. `main::now_unix` — calls `libc::time` unconditionally for TTL math.

This machine already has two precedents for exactly this kind of port,
forked into `C:\git-repositories\4-poc\`:

- `herdr-notes` — a pure-TUI plugin; its port work was about Windows pane
  spawning (relative paths don't resolve against herdr's `\\?\`-prefixed
  cwd) and PowerShell launchers, not Win32 syscalls.
- `herdr-pc-ram-and-cpu-usage-overlay` — the closer analog: it added a
  Windows named-pipe transport, Win32 process metrics, `GetLocalTime`-based
  timestamps, and the same launcher pattern, all via `windows-sys` behind
  `[target.'cfg(windows)']`.

This spec reuses that overlay fork's already-debugged techniques wherever
they overlap, rather than re-deriving them.

## Goals

- `cache-ttl` builds and runs correctly on Windows with full feature parity
  to linux/macos, including the `sort` action (which talks to herdr's own
  socket, not just plugin-local state).
- Linux/macos behavior and code paths are unchanged (`#[cfg(unix)]` /
  `#[cfg(windows)]` split, mirroring the overlay fork's convention).
- Findings from live Windows testing are documented in this repo's
  `CLAUDE.md`, matching the documentation depth of the other two forks.

## Non-goals

- No change to the linux/macos build (dist-copy) or runtime behavior.
- No new abstraction shared across forks — each fork stays a standalone
  crate; only the *technique* is reused, not a shared library.

## Architecture

### 1. Socket write (`herdr.rs`)

`socket_send` is fire-and-forget: it opens the socket, writes one
NDJSON-terminated line, and returns — it never reads a response. On Windows
the actual endpoint is the named pipe `\\.\pipe\` + `$HERDR_SOCKET_PATH`
(opening the bare path hits a placeholder `.sock` file and fails with a
sharing violation — the same finding already recorded in the overlay fork's
`CLAUDE.md`). Because there's no response to read and no connection reuse
needed, a plain `std::fs::File::open` on the pipe path, followed by
`write_all`, is sufficient — Windows exposes an already-listening named pipe
through the ordinary file API for a client that only needs to write. This
needs no new dependency.

```rust
#[cfg(windows)]
pub fn socket_send(socket_path: &str, msg: &str) -> io::Result<()> {
    let pipe_path = format!(r"\\.\pipe\{}", socket_path.trim_start_matches('/'));
    let mut f = std::fs::OpenOptions::new().write(true).open(pipe_path)?;
    writeln!(f, "{msg}")?;
    Ok(())
}
```

(Exact path-mapping from `$HERDR_SOCKET_PATH` to the pipe name needs
confirming against the overlay fork's `herdr/transport.rs::pipe_name` during
implementation — this spec assumes the same mapping applies.)

### 2. State-file locking (`state.rs`)

`libc::flock` has no Windows equivalent, but `LockFileEx` /
`UnlockFileEx` (`Win32::Storage::FileSystem`) is the direct analog — an
exclusive advisory lock on an open file handle, acquired and released around
the same critical section the current `Lock` guard covers. This requires
`windows-sys` as a `[target.'cfg(windows)']` dependency (matching the
overlay fork's `Cargo.toml`). The `Lock`/`PidGuard` structure stays the same
shape; only the acquire/release calls are `#[cfg(unix)]`/`#[cfg(windows)]`
gated.

### 3. Daemon lifecycle (`daemon.rs`)

- **Signal handling** — `SIGTERM`/`SIGINT` registration is dropped on
  Windows, not replaced with `SetConsoleCtrlHandler` or similar. `herdr pane
  close` sends no signal on Windows (confirmed independently in
  `herdr-notes`' Gotchas — "kills the process with no signal"), so a
  graceful-shutdown handler would never fire from herdr's own teardown path
  in practice. The `RUNNING` atomic-flag loop and pid-file "already running"
  guard don't depend on the signal handler existing.
- **Liveness check** (`is_running`) — `libc::kill(pid, 0)` becomes
  `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)` returning a
  valid handle (then `CloseHandle`), via `windows-sys`.
- **Log timestamps** (`logf`) — `libc::time`/`libc::localtime` become
  `GetLocalTime` via `windows-sys`, matching the overlay fork's commit
  `7b63e92` technique directly.

### 4. `now_unix()` (`main.rs`)

Replace the unconditional `libc::time` call with
`std::time::SystemTime::now().duration_since(UNIX_EPOCH)`. This is a
platform-neutral stdlib call — it removes the `libc::time` dependency for
*all* platforms, not just Windows, and needs no `cfg` split.

### Dependency summary

- `Cargo.toml`: `libc` moves to `[target.'cfg(unix)']`; add `windows-sys`
  under `[target.'cfg(windows)']`. Needed feature flags: `Win32_Foundation`,
  `Win32_Storage_FileSystem` (`LockFileEx`/`UnlockFileEx`),
  `Win32_System_Threading` (`OpenProcess`), and whichever feature gates
  `GetLocalTime` (`Win32_System_SystemInformation` in current `windows-sys`
  releases — confirm against the crate version the overlay fork pins,
  `4-poc\herdr-pc-ram-and-cpu-usage-overlay\Cargo.toml`, during
  implementation).

## Manifest & build changes (`herdr-plugin.toml`)

- `platforms` gains `"windows"`.
- New `[[build]]` entry, `platforms = ["windows"]`: `cargo build --release`
  (source-compiled — no Windows artifact exists in `dist/` to copy). The
  existing dist-copy `[[build]]` entry is scoped `platforms = ["linux",
  "macos"]` and otherwise unchanged.
- Every entry that spawns `./cache-ttl ...` by relative path needs a
  `-win`-suffixed Windows twin using the established inline-PowerShell
  launcher (resolve `plugin_root` via `herdr plugin list --json`, strip the
  `\\?\` prefix, invoke `cache-ttl.exe` by absolute path) — the same idiom
  every other forked plugin in this environment already uses:
  - `[[startup]]` (daemon launch)
  - `[[events]]` × 3 (`on-status-change`, `on-pane-closed` ×2)
  - `[[actions]]` × 3 (`reset`, `status`, `sort`) — action ids get `-win`
    suffixes too, since herdr rejects duplicate ids across platform variants
    even when their `platforms` lists are disjoint (a finding already
    recorded in `herdr-notes`' and the overlay fork's Gotchas).

## Testing

1. `cargo build --release`, `cargo test`, `cargo clippy --all-targets -- -D
   warnings` — must all pass, matching the other forks' CI gate.
2. Live smoke test on this machine: `herdr plugin link .`, then:
   - Confirm the daemon starts (`[[startup]]`) and a real agent pane's cache
     token counts down in the sidebar.
   - `herdr plugin action invoke cache-ttl.reset-win` (or equivalent) resets
     a pane's timer.
   - `herdr plugin action invoke cache-ttl.status-win` prints active timers.
   - `herdr plugin action invoke cache-ttl.sort-win` toggles sort and is
     verified via `herdr session snapshot` (or server log) to confirm
     `agent.view.set` actually reached herdr over the named pipe.
3. Findings from the live pass (pipe path mapping, launcher quirks, any
   Win32 API surprises) are written into this repo's `CLAUDE.md` under a
   Gotchas section, matching the depth of `herdr-notes`' and the overlay
   fork's.

## Open questions to resolve during implementation

- The exact `$HERDR_SOCKET_PATH` → pipe-name mapping needs to be confirmed
  against the overlay fork's `transport.rs::pipe_name` rather than assumed;
  if it differs for this herdr version, the design above still holds, only
  the path-formatting line changes.
