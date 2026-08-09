# cache-ttl Windows Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `cache-ttl` herdr plugin build and run on Windows with full feature parity to linux/macos (including the `sort` action, which talks to herdr's own socket), then document real findings from a live test in this repo's `CLAUDE.md`.

**Architecture:** Every Unix-only surface (`UnixStream`, `libc::flock`, `libc::signal`/`kill`, `libc::time`) gets a `#[cfg(windows)]` counterpart using `windows-sys`, reusing already-debugged techniques from the sibling fork `C:\git-repositories\4-poc\herdr-pc-ram-and-cpu-usage-overlay` wherever they overlap (pid liveness check, local-time formatting, named-pipe path mapping). The manifest gains a windows `[[build]]` entry (`cargo build --release` — no prebuilt Windows binary exists to copy) and `-win`-suffixed launcher variants for every entry that currently spawns `./cache-ttl` by relative path.

**Tech Stack:** Rust 2021, `windows-sys` 0.59 (Windows-only), `libc` 0.2 (Unix-only), herdr plugin manifest (TOML), PowerShell (Windows launchers).

## Global Constraints

- Linux/macos code paths and behavior must not change — every Windows addition is `#[cfg(windows)]`-gated, mirroring `4-poc\herdr-pc-ram-and-cpu-usage-overlay`'s convention of `#[cfg(unix)]` for the current code.
- `windows-sys` pinned to `"0.59"` — the same version the sibling fork already uses, to avoid needless version drift between forks in this workspace.
- herdr rejects duplicate action/pane ids across platform variants even when their `platforms` lists are disjoint (confirmed independently in both `herdr-notes` and the overlay fork) — every Windows action variant added below uses a `-win`-suffixed id, never a reused one.
- herdr cannot spawn a relative-path command on Windows (`CreateProcessW` fails resolving a relative path against herdr's `\\?\`-prefixed working directory) — every Windows manifest entry that runs the plugin's own binary goes through the inline-PowerShell absolute-path launcher pattern already used by every other forked plugin in this environment, never a bare relative command.
- The repo's existing CI gate (`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`) must stay green throughout — do not disable a lint to make a step pass.

---

## File Structure

- `Cargo.toml` — split `libc` to `[target.'cfg(unix)']`, add `windows-sys` under `[target.'cfg(windows)']`.
- `src/main.rs` — `now_unix()` becomes platform-neutral (no `cfg` split needed — this one's a straight simplification).
- `src/state.rs` — `Lock::acquire`/`Drop` gets a `#[cfg(windows)]` counterpart using `LockFileEx`/`UnlockFileEx`.
- `src/herdr.rs` — `socket_send` gets a `#[cfg(windows)]` counterpart that writes to the named pipe `\\.\pipe\<HERDR_SOCKET_PATH>`.
- `src/daemon.rs` — signal registration becomes `#[cfg(unix)]`-only (no Windows replacement); `is_running`'s pid-liveness check and `logf`'s timestamp formatting each get a `#[cfg(windows)]` counterpart.
- `herdr-plugin.toml` — `platforms` gains `"windows"`; existing unix-only entries get an explicit `platforms = ["linux", "macos"]`; new `-win`-suffixed entries cover `[[build]]`, `[[startup]]`, `[[events]]` ×3, `[[actions]]` ×3.
- `CLAUDE.md` (new file) — remotes, build/test notes, and a Gotchas section populated from the live smoke test in Task 4.

---

### Task 1: Platform-neutral `now_unix()` and dependency scaffolding

**Files:**
- Modify: `Cargo.toml:6-9`
- Modify: `src/main.rs:9-11`
- Test: `src/main.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub fn now_unix() -> i64` (signature unchanged — every caller in `daemon.rs`/`handlers.rs` keeps working with no changes there).

- [ ] **Step 1: Write the failing test**

Add to `src/main.rs`, after the existing `now_unix` function:

```rust
#[cfg(test)]
mod tests {
    use super::now_unix;

    #[test]
    fn now_unix_is_a_plausible_unix_timestamp() {
        // 2020-01-01T00:00:00Z .. 2100-01-01T00:00:00Z, sanity-bounding
        // against a platform time source that returned garbage.
        let t = now_unix();
        assert!(t > 1_577_836_800 && t < 4_102_444_800, "got {t}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails to compile against the new expectation**

Run: `cargo test now_unix_is_a_plausible_unix_timestamp`
Expected: PASS already (the current `libc::time`-based implementation is correct) — this step is a baseline check, not a red step. Confirm it passes before changing the implementation, so Step 4's pass means the *replacement* is correct, not that the test is a no-op.

- [ ] **Step 3: Replace the `libc::time` call with a platform-neutral one**

Replace in `src/main.rs`:

```rust
pub fn now_unix() -> i64 {
    unsafe { libc::time(std::ptr::null_mut()) as i64 }
}
```

with:

```rust
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}
```

- [ ] **Step 4: Run the test to verify it still passes**

Run: `cargo test now_unix_is_a_plausible_unix_timestamp`
Expected: PASS

- [ ] **Step 5: Split the `libc` dependency and add `windows-sys` (unused for now)**

Replace in `Cargo.toml`:

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
libc = "0.2"
```

with:

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = [
  "Win32_Foundation",
  "Win32_Storage_FileSystem",
  "Win32_System_Threading",
  "Win32_System_SystemInformation",
  "Win32_System_Pipes",
] }
```

(The feature list is exactly what Tasks 2–3 below need: `LockFileEx`/`UnlockFileEx` and `CreateFileW`/`WriteFile` live in `Win32_Storage_FileSystem`, `OpenProcess`/`GetExitCodeProcess` in `Win32_System_Threading`, `GetLocalTime` in `Win32_System_SystemInformation`, `WaitNamedPipeW` in `Win32_System_Pipes`.)

- [ ] **Step 6: Run the full test suite to confirm nothing else broke**

Run: `cargo build --release && cargo test`
Expected: builds clean; this crate is still Unix-only at this point (Tasks 2–3 remove that restriction), so this is just confirming the dependency split didn't break the existing Unix build. `windows-sys` is declared but unused — no warning, since target-specific deps aren't compiled into a non-Windows build at all.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "refactor: platform-neutral now_unix, split libc/windows-sys deps"
```

---

### Task 2: Port the Unix-only runtime surfaces (`state.rs`, `herdr.rs`, `daemon.rs`)

This is one task, not three, because Rust requires every unconditional `use std::os::unix::...` in the crate to be `#[cfg(unix)]`-gated before the crate can compile *at all* on a Windows target — there is no way to get an intermediate green `cargo build` after fixing only one of these three files while the others still reference Unix-only std modules unconditionally. The three files are reviewed together; the meaningful gate is the one at the end of this task.

**Files:**
- Modify: `src/state.rs:1-49` (imports + `Lock`)
- Modify: `src/herdr.rs:1-4,107-112` (imports + `socket_send`)
- Modify: `src/daemon.rs:1-38` (imports + signal registration), `src/daemon.rs:184-201` (`is_running`, `logf`)
- Test: `src/herdr.rs` (inline `#[cfg(test)] mod tests`)
- Test: `src/daemon.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `windows-sys` features declared in Task 1.
- Produces: `state::Lock::acquire(path: &Path) -> io::Result<Lock>` (signature unchanged), `herdr::socket_send(socket_path: &str, msg: &str) -> io::Result<()>` (signature unchanged), `daemon::is_running(pid_file: &Path) -> bool` (signature unchanged), `daemon::logf(msg: &str)` (signature unchanged). No caller anywhere in the crate needs to change.

#### 2a. `src/state.rs` — Windows file locking

- [ ] **Step 1: Write the failing test**

Add to `src/state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lock_is_exclusive_within_one_process() {
        let dir = std::env::temp_dir().join(format!("cache-ttl-lock-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".state.lock");

        let first = Lock::acquire(&path).unwrap();
        // A second acquire from the SAME process/thread would deadlock on a
        // blocking lock, so this test only proves acquire+drop round-trips
        // without erroring — cross-process exclusion is verified live in
        // Task 4's smoke test (two `cache-ttl` invocations racing a save).
        drop(first);
        let second = Lock::acquire(&path).unwrap();
        drop(second);

        let _ = fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test lock_is_exclusive_within_one_process`
Expected: on this Windows machine, FAIL to compile — `state.rs` currently imports `std::os::unix::io::AsRawFd` and calls `libc::flock` unconditionally, neither of which exists on Windows.

- [ ] **Step 3: Gate the Unix implementation and add the Windows one**

Replace the top of `src/state.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::config::Config;
```

with:

```rust
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::Config;
```

Replace the `Lock` struct and its `impl`s:

```rust
struct Lock {
    file: fs::File,
}

impl Lock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Lock { file })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
```

with:

```rust
struct Lock {
    file: fs::File,
}

impl Lock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)?;
        lock_exclusive(&file)?;
        Ok(Lock { file })
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unlock(&self.file);
    }
}

/// Unix: `flock(LOCK_EX)` — blocks until the lock is free.
#[cfg(unix)]
fn lock_exclusive(file: &fs::File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: file outlives the call; return value checked.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn unlock(file: &fs::File) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: file outlives the call; failure is a best-effort unlock,
    // matching the Unix Drop's prior behavior of ignoring the return code.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

/// Windows: `LockFileEx` is the direct analog of Unix `flock` — an exclusive
/// advisory lock on an open file handle. `LOCKFILE_EXCLUSIVE_LOCK` with a
/// zeroed `OVERLAPPED` blocks synchronously until the lock is free, matching
/// `flock(LOCK_EX)`'s blocking behavior. The lock range covers the whole
/// representable range (`u32::MAX` for both the high and low byte counts) —
/// this state file is a few KB of JSON, never near that size.
#[cfg(windows)]
fn lock_exclusive(file: &fs::File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: handle is valid for the duration of this call; overlapped is a
    // caller-owned, zeroed OVERLAPPED as required for a synchronous lock.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn unlock(file: &fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    // SAFETY: same handle/overlapped shape as the matching lock call;
    // failure is a best-effort unlock, matching the Unix Drop's behavior.
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    unsafe {
        UnlockFileEx(handle, 0, u32::MAX, u32::MAX, &mut overlapped);
    }
}
```

- [ ] **Step 4: Note the Windows `Win32_System_IO` feature gap**

`OVERLAPPED` lives in `windows_sys::Win32::System::IO`, which is not yet in the feature list added in Task 1. Add it now: edit `Cargo.toml`'s `[target.'cfg(windows)'.dependencies]` features list to include `"Win32_System_IO"`.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test lock_is_exclusive_within_one_process`
Expected: still won't fully pass yet — `herdr.rs` and `daemon.rs` still have ungated Unix-only code blocking the whole-crate build. Continue to 2b/2c before running this gate for real; that's step 7 below.

#### 2b. `src/herdr.rs` — Windows named-pipe write

- [ ] **Step 6: Write the failing test**

Add to `src/herdr.rs`, after `format_remaining`:

```rust
#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::pipe_name_str;

    #[cfg(windows)]
    #[test]
    fn prepends_pipe_prefix_to_a_filesystem_path() {
        assert_eq!(
            pipe_name_str(r"C:\Users\me\AppData\Roaming\herdr\herdr.sock"),
            r"\\.\pipe\C:\Users\me\AppData\Roaming\herdr\herdr.sock",
        );
    }

    #[cfg(windows)]
    #[test]
    fn leaves_an_existing_pipe_path_untouched() {
        assert_eq!(pipe_name_str(r"\\.\pipe\herdr"), r"\\.\pipe\herdr");
        assert_eq!(pipe_name_str(r"\\?\pipe\herdr"), r"\\?\pipe\herdr");
    }
}
```

This mirrors the already-verified test names and cases from
`4-poc\herdr-pc-ram-and-cpu-usage-overlay\src\herdr\transport.rs`'s
`name_tests` module — same mapping rule, same herdr version it was verified
against (0.7.4).

- [ ] **Step 7: Run test to verify it fails**

Run: `cargo test prepends_pipe_prefix_to_a_filesystem_path`
Expected: FAIL to compile — `pipe_name_str` doesn't exist yet.

- [ ] **Step 8: Gate `socket_send` and add the Windows pipe-write path**

Replace the top of `src/herdr.rs`:

```rust
use serde::Deserialize;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use crate::config::Config;
```

with:

```rust
use serde::Deserialize;
use std::io::{self, Write};
use std::process::Command;
use std::sync::OnceLock;
#[cfg(unix)]
use std::time::Duration;

use crate::config::Config;
```

Replace `socket_send`:

```rust
pub fn socket_send(socket_path: &str, msg: &str) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{msg}")?;
    Ok(())
}
```

with:

```rust
#[cfg(unix)]
pub fn socket_send(socket_path: &str, msg: &str) -> io::Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{msg}")?;
    Ok(())
}

/// Windows: herdr injects `HERDR_SOCKET_PATH` as a filesystem-style path
/// (alongside a placeholder file of that name), but the real Windows IPC
/// endpoint is the named pipe `\\.\pipe\<that path>` — opening the bare path
/// lands on the placeholder and fails with a sharing violation (verified
/// independently in the `herdr-pc-ram-and-cpu-usage-overlay` fork against
/// live herdr 0.7.4). `socket_send` only ever writes one line and returns —
/// it never reads a response — so this is a plain `CreateFileW` + one
/// `WriteFile`, with the same `ERROR_PIPE_BUSY` retry the overlay fork's
/// richer transport uses for its connect path.
#[cfg(windows)]
pub fn socket_send(socket_path: &str, msg: &str) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, WriteFile, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    let wide = to_wide(&pipe_name_str(socket_path));
    let handle = loop {
        // SAFETY: wide is NUL-terminated; the returned handle is checked
        // before use and closed exactly once below.
        let h = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE && !h.is_null() {
            break h;
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
            // SAFETY: wide is NUL-terminated.
            if unsafe { WaitNamedPipeW(wide.as_ptr(), 15_000) } == 0 {
                return Err(io::Error::last_os_error());
            }
            continue;
        }
        return Err(err);
    };

    let line = format!("{msg}\n");
    let bytes = line.as_bytes();
    let mut written: u32 = 0;
    // SAFETY: bytes is valid for bytes.len(); written is caller-owned;
    // handle is closed in the guard below regardless of the outcome.
    let ok = unsafe {
        WriteFile(
            handle,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: handle came from CreateFileW above and is closed exactly once.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Pure prefix logic behind the Windows `socket_send`, split out for unit
/// testing without touching a real pipe.
#[cfg(windows)]
fn pipe_name_str(raw: &str) -> String {
    if raw.starts_with(r"\\.\pipe\") || raw.starts_with(r"\\?\pipe\") {
        raw.to_string()
    } else {
        format!(r"\\.\pipe\{raw}")
    }
}
```

- [ ] **Step 9: Add the `Win32_System_Pipes` `CloseHandle` feature dependency check**

`CloseHandle` lives in `Win32_Foundation`, already in the feature list from Task 1. `WaitNamedPipeW` lives in `Win32_System_Pipes`, already added in Task 1. No further `Cargo.toml` change needed here — this step is a checkpoint, not an edit.

#### 2c. `src/daemon.rs` — signal handling, liveness check, log timestamp

- [ ] **Step 10: Write the failing test**

Add to `src/daemon.rs`, after `logf`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_running_false_for_a_missing_pid_file() {
        let path = std::env::temp_dir()
            .join(format!("cache-ttl-no-such-pid-{}", std::process::id()));
        assert!(!is_running(&path));
    }

    #[test]
    fn is_running_true_for_the_current_process() {
        let path = std::env::temp_dir()
            .join(format!("cache-ttl-self-pid-{}", std::process::id()));
        std::fs::write(&path, format!("{}", std::process::id())).unwrap();
        assert!(is_running(&path));
        let _ = std::fs::remove_file(&path);
    }
}
```

- [ ] **Step 11: Run test to verify it fails**

Run: `cargo test is_running_true_for_the_current_process`
Expected: FAIL to compile — `daemon.rs` currently imports `libc` unconditionally for `signal`/`kill`/`time`/`localtime`, none of which exist on Windows.

- [ ] **Step 12: Gate signal registration to Unix only**

Replace in `src/daemon.rs`:

```rust
extern "C" fn on_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}
```

with:

```rust
#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}
```

Replace in `pub fn run`:

```rust
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
```

with:

```rust
    // Windows: no replacement handler is registered here. `herdr pane close`
    // sends no signal on Windows at all (confirmed independently in the
    // `herdr-notes` fork's Gotchas — "kills the process with no signal"), so
    // a graceful-shutdown handler would never fire from herdr's own teardown
    // path in practice; a `Stop-Process`-style hard kill can't be caught
    // either way. The `RUNNING` flag and pid-file guard below don't depend
    // on this handler existing.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
```

- [ ] **Step 13: Gate `is_running`'s liveness check**

Replace:

```rust
fn is_running(pid_file: &std::path::Path) -> bool {
    let Ok(data) = fs::read_to_string(pid_file) else {
        return false;
    };
    let Ok(pid) = data.trim().parse::<i32>() else {
        return false;
    };
    unsafe { libc::kill(pid, 0) == 0 }
}
```

with:

```rust
fn is_running(pid_file: &std::path::Path) -> bool {
    let Ok(data) = fs::read_to_string(pid_file) else {
        return false;
    };
    let Ok(pid) = data.trim().parse::<i32>() else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    pid_alive(pid)
}

/// Unix: signal 0 performs no delivery, existence check only.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: kill signal 0 is a liveness probe only.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Windows: open the process and confirm it hasn't exited. `OpenProcess`
/// succeeding is not by itself proof of liveness (a pid can be reused, or
/// the handle can briefly outlive process exit), so this also checks
/// `GetExitCodeProcess` returns `STILL_ACTIVE` — the same two-step check
/// `herdr-pc-ram-and-cpu-usage-overlay`'s `pid_alive` already uses.
#[cfg(windows)]
fn pid_alive(pid: i32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: handle checked before use; code is caller-owned; handle closed
    // exactly once.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid as u32);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}
```

- [ ] **Step 14: Gate `logf`'s timestamp formatting**

Replace:

```rust
fn logf(msg: &str) {
    let (h, m, s) = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let tm = libc::localtime(&t);
        ((*tm).tm_hour, (*tm).tm_min, (*tm).tm_sec)
    };
    eprintln!("[{h:02}:{m:02}:{s:02}] {msg}");
}
```

with:

```rust
fn logf(msg: &str) {
    let (h, m, s) = local_time_hms();
    eprintln!("[{h:02}:{m:02}:{s:02}] {msg}");
}

/// Unix: `localtime` fills a caller-owned `tm`.
#[cfg(unix)]
fn local_time_hms() -> (i32, i32, i32) {
    // SAFETY: `localtime` fills the caller-owned `tm`; `t` is a valid time_t.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let tm = libc::localtime(&t);
        ((*tm).tm_hour, (*tm).tm_min, (*tm).tm_sec)
    }
}

/// Windows counterpart, sourced from `GetLocalTime` instead of `localtime` —
/// the exact technique `herdr-pc-ram-and-cpu-usage-overlay`'s
/// `render.rs::local_time_string` already uses.
#[cfg(windows)]
fn local_time_hms() -> (i32, i32, i32) {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    // SAFETY: `GetLocalTime` fills the caller-owned `SYSTEMTIME`.
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    (st.wHour as i32, st.wMinute as i32, st.wSecond as i32)
}
```

- [ ] **Step 15: Run all the new tests to verify they pass**

Run: `cargo test`
Expected: PASS — every test added in 2a/2b/2c, plus the full existing suite.

- [ ] **Step 16: Run the repo's required-green gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean. This is the first point in the plan where the crate actually compiles on this Windows machine — every Unix-only unconditional reference is now gated.

- [ ] **Step 17: Commit**

```bash
git add Cargo.toml src/state.rs src/herdr.rs src/daemon.rs
git commit -m "feat(windows): named-pipe socket write, LockFileEx state lock, process liveness + local-time logging"
```

---

### Task 3: Manifest — windows build + `-win` launcher variants

**Files:**
- Modify: `herdr-plugin.toml` (entire file)

**Interfaces:**
- Consumes: `target\release\cache-ttl.exe`, produced by the `cargo build --release` added below (standard cargo output layout — unlike the Unix `[[build]]`, which copies a prebuilt binary directly into the plugin root, the Windows build compiles from source and leaves the binary under `target\release\`, so the Windows launchers below point there, not at plugin-root `.\cache-ttl`).

- [ ] **Step 1: Add `"windows"` to `platforms` and scope the existing entries to unix**

Replace the top of `herdr-plugin.toml`:

```toml
id = "cache-ttl"
name = "Cache TTL Countdown"
version = "0.3.2"
min_herdr_version = "0.7.5"
description = "Live prompt-cache countdown per agent pane"
platforms = ["linux", "macos"]

# Select the prebuilt binary for this host at install time. No toolchain
# required on the target — dist/ ships one artifact per platform.
[[build]]
command = ["sh", "-c", "cp \"dist/cache-ttl-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)\" cache-ttl && chmod +x cache-ttl"]
```

with:

```toml
id = "cache-ttl"
name = "Cache TTL Countdown"
version = "0.3.2"
min_herdr_version = "0.7.5"
description = "Live prompt-cache countdown per agent pane"
platforms = ["linux", "macos", "windows"]

# Select the prebuilt binary for this host at install time. No toolchain
# required on the target — dist/ ships one artifact per platform.
[[build]]
command = ["sh", "-c", "cp \"dist/cache-ttl-$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)\" cache-ttl && chmod +x cache-ttl"]
platforms = ["linux", "macos"]

# dist/ ships no windows artifact; compile from source instead. Lands at
# target\release\cache-ttl.exe (standard cargo layout), NOT plugin-root
# .\cache-ttl like the unix build above — every windows launcher below
# points at that path explicitly.
[[build]]
command = ["cargo", "build", "--release"]
platforms = ["windows"]
```

- [ ] **Step 2: Scope the existing startup/events/actions to unix and add windows twins**

Replace the remainder of `herdr-plugin.toml`:

```toml
[[startup]]
command = ["./cache-ttl", "daemon"]

[[events]]
on = "pane.agent_status_changed"
command = ["./cache-ttl", "on-status-change"]

[[events]]
on = "pane.closed"
command = ["./cache-ttl", "on-pane-closed"]

[[events]]
on = "pane.exited"
command = ["./cache-ttl", "on-pane-closed"]

[[actions]]
id = "reset"
title = "Reset cache timer"
description = "Reset the cache TTL countdown for the focused pane"
contexts = ["pane"]
command = ["./cache-ttl", "reset-timer"]

[[actions]]
id = "status"
title = "Show cache timers"
description = "Print all active cache TTL timers"
contexts = ["workspace"]
command = ["./cache-ttl", "show-timers"]

[[actions]]
id = "sort"
title = "Toggle cache sort"
description = "Sort agent panel by cache TTL urgency"
contexts = ["workspace"]
command = ["./cache-ttl", "toggle-sort"]
```

with:

```toml
[[startup]]
command = ["./cache-ttl", "daemon"]
platforms = ["linux", "macos"]

[[startup]]
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') daemon"
]
platforms = ["windows"]

[[events]]
on = "pane.agent_status_changed"
command = ["./cache-ttl", "on-status-change"]
platforms = ["linux", "macos"]

[[events]]
on = "pane.agent_status_changed"
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') on-status-change"
]
platforms = ["windows"]

[[events]]
on = "pane.closed"
command = ["./cache-ttl", "on-pane-closed"]
platforms = ["linux", "macos"]

[[events]]
on = "pane.closed"
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') on-pane-closed"
]
platforms = ["windows"]

[[events]]
on = "pane.exited"
command = ["./cache-ttl", "on-pane-closed"]
platforms = ["linux", "macos"]

[[events]]
on = "pane.exited"
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') on-pane-closed"
]
platforms = ["windows"]

[[actions]]
id = "reset"
title = "Reset cache timer"
description = "Reset the cache TTL countdown for the focused pane"
contexts = ["pane"]
command = ["./cache-ttl", "reset-timer"]
platforms = ["linux", "macos"]

[[actions]]
id = "reset-win"
title = "Reset cache timer"
description = "Reset the cache TTL countdown for the focused pane"
contexts = ["pane"]
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') reset-timer"
]
platforms = ["windows"]

[[actions]]
id = "status"
title = "Show cache timers"
description = "Print all active cache TTL timers"
contexts = ["workspace"]
command = ["./cache-ttl", "show-timers"]
platforms = ["linux", "macos"]

[[actions]]
id = "status-win"
title = "Show cache timers"
description = "Print all active cache TTL timers"
contexts = ["workspace"]
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') show-timers"
]
platforms = ["windows"]

[[actions]]
id = "sort"
title = "Toggle cache sort"
description = "Sort agent panel by cache TTL urgency"
contexts = ["workspace"]
command = ["./cache-ttl", "toggle-sort"]
platforms = ["linux", "macos"]

[[actions]]
id = "sort-win"
title = "Toggle cache sort"
description = "Sort agent panel by cache TTL urgency"
contexts = ["workspace"]
command = [
  "powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
  "$r=$env:HERDR_PLUGIN_ROOT; if($r.StartsWith('\\\\?\\')){$r=$r.Substring(4)}; & (Join-Path $r 'target\\release\\cache-ttl.exe') toggle-sort"
]
platforms = ["windows"]
```

- [ ] **Step 3: Build the Windows binary**

Run: `cargo build --release`
Expected: `target\release\cache-ttl.exe` exists.

- [ ] **Step 4: Link the plugin and verify the manifest is well-formed**

Run: `herdr plugin link .` (from the repo root)
Expected: succeeds with no `duplicate_plugin_action_id` or TOML parse error. If `cache-ttl` is already installed from the GitHub-managed copy (`%APPDATA%\herdr\plugins\github\cache-ttl-...`), uninstall it first: `herdr plugin uninstall cache-ttl`.

- [ ] **Step 5: Verify the linked manifest via `herdr plugin list --json`**

Run: `herdr plugin list --json`
Expected: the `cache-ttl` entry's `platforms` includes `"windows"`, and its `actions` array contains both `reset`/`reset-win`, `status`/`status-win`, `sort`/`sort-win` with distinct ids.

- [ ] **Step 6: Commit**

```bash
git add herdr-plugin.toml
git commit -m "feat(windows): cargo-build entry and -win launcher variants for startup/events/actions"
```

---

### Task 4: Live smoke test, CLAUDE.md, and final gate

**Files:**
- Create: `CLAUDE.md`
- No source changes expected in this task unless the live test surfaces a real bug — if it does, fix it here and fold the fix into this task's commit rather than opening a new task, per this plan's "no placeholders" rule (there is no way to pre-write the fix for a bug not yet found).

**Interfaces:**
- None — this task consumes the finished plugin from Tasks 1–3 and produces documentation.

- [ ] **Step 1: Confirm the daemon starts and updates a real pane's token**

With the plugin linked (Task 3, Step 4) and a herdr session running: open an agent pane, let an agent turn complete, and confirm its cache countdown token appears/updates in the sidebar within one `tick_interval` (15s default). If it doesn't, check `herdr plugin log list --plugin cache-ttl` for the startup/event command's stderr.

- [ ] **Step 2: Verify `reset-win`**

Run: `herdr plugin action invoke cache-ttl.reset-win` with a pane focused.
Expected: stdout `Timer reset for <pane_id>`, and the sidebar token jumps back to the full TTL.

- [ ] **Step 3: Verify `status-win`**

Run: `herdr plugin action invoke cache-ttl.status-win`
Expected: a `PANE / TTL / TITLE` table listing the active timer(s) from Step 1/2.

- [ ] **Step 4: Verify `sort-win` actually reaches herdr over the named pipe**

Run: `herdr plugin action invoke cache-ttl.sort-win`
Expected: stdout `Cache sort on`. Then confirm the agents panel is actually sorted by cache urgency — either visually, or via `herdr session snapshot` / `herdr server` logs showing the `agent.view.set` call with `"source":"plugin:cache-ttl"` landed. Run `cache-ttl.sort-win` again; expected: `Cache sort off`, and the view reverts.

- [ ] **Step 5: Record real findings in `CLAUDE.md`**

Create `CLAUDE.md` at the repo root, matching the structure and depth of `4-poc\herdr-pc-ram-and-cpu-usage-overlay\CLAUDE.md` (Remotes / Build+test / Cross-platform rules / herdr integration / Layout). Seed it with what's already known going in — Steps 1–4 above may add to the Gotchas section if anything genuinely surprising turns up live (a wrong pipe-name mapping, an action-id collision, a launcher quirk):

```markdown
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

## herdr integration (learned porting this plugin — verified on herdr <fill in the version this machine is running: check `herdr --version`>)

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
- <Add anything Steps 1–4 above actually surfaced that isn't already
  covered by the two bullets above — a wrong assumption, a timing quirk, a
  launcher edge case. If nothing new turned up, say so explicitly rather
  than leaving this section looking incomplete: "No further Gotchas beyond
  what's listed above turned up in the live smoke test.">

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
```

- [ ] **Step 6: Fill in the herdr version and any live findings**

Run: `herdr --version`, paste the result into the `<fill in ...>` placeholder from Step 5, and replace the `<Add anything ...>` line with either real findings from Steps 1–4 or the explicit "no further Gotchas" sentence — never leave either placeholder verbatim in the committed file.

- [ ] **Step 7: Final full gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all green — this is the same gate as Task 2 Step 16, re-run after Task 3's manifest-only changes and Task 4's CLAUDE.md addition (neither should have touched Rust source, so this is a confirmation, not expected to find anything new).

- [ ] **Step 8: Commit and push**

```bash
git add CLAUDE.md
git commit -m "docs: CLAUDE.md with remotes, build notes, and live Windows-port findings"
git push -u origin windows-port
```
