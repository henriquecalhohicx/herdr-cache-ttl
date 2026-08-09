use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::Config;

#[derive(Serialize, Deserialize, Clone)]
pub struct TimerEntry {
    pub last_turn: i64,
    pub ttl_seconds: i64,
}

pub type Timers = HashMap<String, TimerEntry>;
pub type Notified = HashMap<String, Vec<i64>>;

pub struct State {
    lock_path: PathBuf,
    timers_path: PathBuf,
    notified_path: PathBuf,
}

struct Lock {
    file: fs::File,
}

impl Lock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
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

impl State {
    pub fn new(cfg: &Config) -> Self {
        State {
            lock_path: cfg.state_dir.join(".state.lock"),
            timers_path: cfg.timers_file.clone(),
            notified_path: cfg.notified_file.clone(),
        }
    }

    pub fn init(&self) -> io::Result<()> {
        if let Some(dir) = self.timers_path.parent() {
            fs::create_dir_all(dir)?;
        }
        for p in [&self.timers_path, &self.notified_path] {
            if !p.exists() {
                fs::write(p, b"{}")?;
            }
        }
        Ok(())
    }

    fn lock(&self) -> io::Result<Lock> {
        Lock::acquire(&self.lock_path)
    }

    pub fn read_timers(&self) -> Timers {
        read_json(&self.timers_path).unwrap_or_default()
    }

    pub fn read_notified(&self) -> Notified {
        read_json(&self.notified_path).unwrap_or_default()
    }

    pub fn set_timer(&self, pane_id: &str, entry: TimerEntry) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut t: Timers = read_json(&self.timers_path).unwrap_or_default();
        t.insert(pane_id.into(), entry);
        atomic_write_json(&self.timers_path, &t)
    }

    pub fn clear_all_timers(&self) -> io::Result<()> {
        let _lock = self.lock()?;
        atomic_write_json(&self.timers_path, &Timers::new())
    }

    pub fn prune_timers(&self, keep: &HashSet<String>) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut t: Timers = read_json(&self.timers_path).unwrap_or_default();
        t.retain(|id, _| keep.contains(id));
        atomic_write_json(&self.timers_path, &t)
    }

    pub fn add_notification(&self, pane_id: &str, threshold: i64) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut n: Notified = read_json(&self.notified_path).unwrap_or_default();
        n.entry(pane_id.into()).or_default().push(threshold);
        atomic_write_json(&self.notified_path, &n)
    }

    pub fn set_timer_and_clear_notification(
        &self,
        pane_id: &str,
        entry: TimerEntry,
    ) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut t: Timers = read_json(&self.timers_path).unwrap_or_default();
        t.insert(pane_id.into(), entry);
        atomic_write_json(&self.timers_path, &t)?;
        let mut n: Notified = read_json(&self.notified_path).unwrap_or_default();
        n.remove(pane_id);
        atomic_write_json(&self.notified_path, &n)
    }

    pub fn delete_timer_and_notification(&self, pane_id: &str) -> io::Result<()> {
        let _lock = self.lock()?;
        let mut t: Timers = read_json(&self.timers_path).unwrap_or_default();
        t.remove(pane_id);
        atomic_write_json(&self.timers_path, &t)?;
        let mut n: Notified = read_json(&self.notified_path).unwrap_or_default();
        n.remove(pane_id);
        atomic_write_json(&self.notified_path, &n)
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let data =
        serde_json::to_vec(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, &data)?;
    fs::rename(&tmp, path)
}

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
        // blocking lock, so this test only proves the Windows
        // `LockFileEx`/`UnlockFileEx` pair (and the Unix `flock` pair)
        // round-trips without erroring in a single process. Cross-process
        // contention — two `cache-ttl` invocations racing a save — is NOT
        // verified live; that remains a known gap (see CLAUDE.md).
        drop(first);
        let second = Lock::acquire(&path).unwrap();
        drop(second);

        let _ = fs::remove_dir_all(&dir);
    }
}
