use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, fs, io, thread, time::Duration};

use crate::config::Config;
use crate::herdr;
use crate::state::{State, TimerEntry};

static RUNNING: AtomicBool = AtomicBool::new(true);

#[cfg(unix)]
extern "C" fn on_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

struct PidGuard(std::path::PathBuf);

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn run(cfg: &Config) -> io::Result<()> {
    let st = State::new(cfg);
    st.init()?;

    if is_running(&cfg.pid_file) {
        logf("daemon already running, exiting");
        return Ok(());
    }

    fs::write(&cfg.pid_file, format!("{}", std::process::id()))?;
    let _pid = PidGuard(cfg.pid_file.clone());

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

    reapply_sort(cfg);

    let pid = std::process::id();
    let tick = cfg.tick_interval;
    logf(&format!("daemon started, pid={pid}, tick={tick}s"));

    let tick_dur = Duration::from_secs(tick as u64);
    let cleanup_interval = 10;
    let mut tick_count = 0u64;
    let mut consecutive_failures = 0;

    while RUNNING.load(Ordering::Relaxed) {
        thread::sleep(tick_dur);
        if !RUNNING.load(Ordering::Relaxed) {
            break;
        }

        tick_count += 1;

        let mut timers = st.read_timers();
        if timers.is_empty() {
            continue;
        }

        let now = crate::now_unix();

        if tick_count.is_multiple_of(cleanup_interval) {
            prune_stale(&st);
            timers = st.read_timers();
            if timers.is_empty() {
                continue;
            }
        }

        if refresh_working(&st, cfg, now) {
            timers = st.read_timers();
        }

        let mut updated = 0;
        let mut failed = 0;
        let mut last_err = String::new();

        for (pane_id, entry) in &timers {
            let remaining = entry.ttl_seconds - (now - entry.last_turn);

            match herdr::set_pane_tokens(pane_id, remaining, cfg) {
                Ok(()) => updated += 1,
                Err(e) => {
                    failed += 1;
                    last_err = e.to_string();
                }
            }

            check_notifications(&st, cfg, pane_id, remaining);
        }

        if failed > 0 && updated == 0 {
            consecutive_failures += 1;
            if consecutive_failures >= 3 {
                logf(&format!(
                    "all updates failing ({consecutive_failures} consecutive), backing off: {last_err}"
                ));
                thread::sleep(Duration::from_secs(60));
            }
        } else {
            consecutive_failures = 0;
        }
    }

    logf("daemon shutting down");
    Ok(())
}

fn refresh_working(st: &State, cfg: &Config, now: i64) -> bool {
    let Ok(agents) = herdr::agent_list() else {
        return false;
    };

    let timers = st.read_timers();
    let mut changed = false;

    for agent in &agents {
        if agent.agent_status != "working" {
            continue;
        }
        let last = timers.get(&agent.pane_id).map_or(0, |e| e.last_turn);
        if now - last > cfg.tick_interval {
            let _ = st.set_timer(
                &agent.pane_id,
                TimerEntry {
                    last_turn: now,
                    ttl_seconds: cfg.ttl_seconds,
                },
            );
            changed = true;
        }
    }
    changed
}

fn prune_stale(st: &State) {
    let Ok(agents) = herdr::agent_list() else {
        return;
    };
    if agents.is_empty() {
        let _ = st.clear_all_timers();
        return;
    }
    let keep: HashSet<String> = agents.into_iter().map(|a| a.pane_id).collect();
    let _ = st.prune_timers(&keep);
}

fn check_notifications(st: &State, cfg: &Config, pane_id: &str, remaining: i64) {
    if remaining <= 0 {
        return;
    }

    let notified = st.read_notified();
    let mut seen: Vec<i64> = notified.get(pane_id).cloned().unwrap_or_default();

    for &threshold in &cfg.warn_thresholds {
        if remaining <= threshold && !seen.contains(&threshold) {
            let label = herdr::format_remaining(threshold, cfg.seconds_threshold);
            let title = herdr::agent_get_title(pane_id);
            herdr::notify(&format!("Cache expires in {label}"), &title, "request");
            let _ = st.add_notification(pane_id, threshold);
            seen.push(threshold);
        }
    }
}

fn reapply_sort(cfg: &Config) {
    if !cfg.state_dir.join("sort_active").exists() {
        return;
    }
    let Ok(socket) = env::var("HERDR_SOCKET_PATH") else {
        return;
    };
    let msg = r#"{"id":"on","method":"agent.view.set","params":{"source":"plugin:cache-ttl","label":"cache","sort":[{"field":{"token":"cache_sort"},"order":"desc"},{"field":"attention","order":"desc"},{"field":"state_change_seq","order":"desc"}]}}"#;
    if let Err(e) = herdr::socket_send(&socket, msg) {
        logf(&format!("failed to reapply sort view: {e}"));
    }
}

fn is_running(pid_file: &std::path::Path) -> bool {
    let Ok(data) = fs::read_to_string(pid_file) else {
        return false;
    };
    let Ok(pid) = data.trim().parse::<i32>() else {
        return false;
    };
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
/// `herdr-pc-ram-and-cpu-usage-overlay`'s `pid_alive` already uses. `pid <=
/// 0` is rejected up front: 0 is the Windows System Idle Process pid, and a
/// corrupted/zero pid-file entry must not be reported as a live daemon.
#[cfg(windows)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
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
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

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
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: `GetLocalTime` fills the caller-owned `SYSTEMTIME`.
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    (st.wHour as i32, st.wMinute as i32, st.wSecond as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_running_false_for_a_missing_pid_file() {
        let path =
            std::env::temp_dir().join(format!("cache-ttl-no-such-pid-{}", std::process::id()));
        assert!(!is_running(&path));
    }

    #[test]
    fn is_running_true_for_the_current_process() {
        let path = std::env::temp_dir().join(format!("cache-ttl-self-pid-{}", std::process::id()));
        std::fs::write(&path, format!("{}", std::process::id())).unwrap();
        assert!(is_running(&path));
        let _ = std::fs::remove_file(&path);
    }
}
