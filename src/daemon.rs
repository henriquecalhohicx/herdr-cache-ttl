use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{env, fs, io, thread, time::Duration};

use crate::account;
use crate::config::Config;
use crate::herdr;
use crate::state::{State, Timers};

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
    // Bounded exit after prolonged total failure. 3 consecutive failed ticks
    // already trigger the 60s backoff below; that alone never stops the
    // loop, so a permanently unreachable herdr server previously meant an
    // unkillable-by-normal-means daemon spinning forever on both platforms
    // (Windows especially, since a hard kill can't be caught at all — see
    // the pid-file comment above). 10 is chosen so a few consecutive
    // failures caused by a transient blip (server restart, brief network
    // hiccup) don't shut the daemon down — by the 10th failure at least
    // (10 - 3) * 60s = 7 minutes have passed with every tick still failing,
    // which is long past "transient". This mirrors
    // `herdr-pc-ram-and-cpu-usage-overlay`'s daemon shutting down after five
    // consecutive failures, just tuned to this daemon's shorter tick
    // interval and its separate 60s backoff step.
    const MAX_CONSECUTIVE_FAILURES: u32 = 10;

    // session id -> account number. Only hits are cached: a brand-new
    // session's transcript may not exist on disk yet, so a miss is retried
    // on the next tick.
    let mut accounts: HashMap<String, u32> = HashMap::new();

    while RUNNING.load(Ordering::Relaxed) {
        thread::sleep(tick_dur);
        if !RUNNING.load(Ordering::Relaxed) {
            break;
        }

        tick_count += 1;

        // Runs before the empty-timers early exit: the account badge shows
        // on every Claude pane, counting down or not.
        refresh_accounts(&mut accounts);

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

        if clear_working(&st, cfg) {
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
            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                logf(&format!(
                    "giving up after {consecutive_failures} consecutive failed ticks (herdr server likely gone), shutting down: {last_err}"
                ));
                return Ok(());
            }
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

/// Working panes are guaranteed cache-fresh (see `handlers::turn_action_for`,
/// which clears a pane's timer/tokens the moment it starts working) — so
/// this is a catch-all safety net, not the primary mechanism, covering only
/// the case where that transition event was missed (e.g. daemon restarted
/// mid-turn). Panes already cleared (no timer entry) are left alone so the
/// tick loop doesn't spam redundant clear calls every 15s.
fn panes_needing_clear(agents: &[herdr::AgentInfo], timers: &Timers) -> Vec<String> {
    agents
        .iter()
        .filter(|a| a.agent_status == "working" && timers.contains_key(&a.pane_id))
        .map(|a| a.pane_id.clone())
        .collect()
}

fn clear_working(st: &State, cfg: &Config) -> bool {
    let Ok(agents) = herdr::agent_list() else {
        return false;
    };

    let timers = st.read_timers();
    let to_clear = panes_needing_clear(&agents, &timers);
    if to_clear.is_empty() {
        return false;
    }
    for pane_id in &to_clear {
        let _ = st.delete_timer_and_notification(pane_id);
        let _ = herdr::clear_pane_tokens(pane_id, cfg);
    }
    true
}

/// Tags each Claude pane with its account badge (`acct_cN`, see
/// `herdr::set_account_token`). The tokens carry no TTL, so a pane whose
/// tokens already show the right account is skipped — herdr is only called
/// when a pane is new or its session switched account.
fn refresh_accounts(cache: &mut HashMap<String, u32>) {
    let Some(home) = account::home_dir() else {
        return;
    };
    let Ok(agents) = herdr::agent_list() else {
        return;
    };
    for a in &agents {
        let Some(session_id) = a.agent_session.as_ref().map(|s| s.value.as_str()) else {
            continue;
        };
        if a.agent != "claude" || session_id.is_empty() {
            continue;
        }
        let account = match cache.get(session_id) {
            Some(&n) => n,
            None => {
                let Some(n) = account::find_account(&home, session_id) else {
                    continue;
                };
                cache.insert(session_id.to_string(), n);
                n
            }
        };
        if !account_tokens_match(&a.tokens, account) {
            if let Err(e) = herdr::set_account_token(&a.pane_id, account) {
                logf(&format!("account badge for {} failed: {e}", a.pane_id));
            }
        }
    }
}

/// True when `tokens` holds exactly this account's badge and no other's.
fn account_tokens_match(tokens: &HashMap<String, String>, account: u32) -> bool {
    account::accounts().all(|n| {
        let value = tokens.get(&account::token_name(n));
        if n == account {
            value.is_some_and(|v| *v == format!("c{n}"))
        } else {
            value.is_none()
        }
    })
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
///
/// Beyond that: there is no signal handler on Windows (see the comment in
/// `run` above), so a hard kill — which is how herdr always terminates a
/// plugin process on this platform — never runs `PidGuard::drop`, and
/// `daemon.pid` survives with a dead pid in it. Windows recycles pids, so a
/// live-but-unrelated process can later reuse that exact pid, and the
/// `STILL_ACTIVE` check above alone would then report a false positive
/// forever (no daemon ever restarts, since `run` exits early believing one
/// is already up). Guard against that by also checking, via
/// `QueryFullProcessImageNameW`, that the pid actually belongs to a
/// `cache-ttl.exe` — a match makes it genuinely alive, a mismatch (or a
/// failure to even read the image name) makes the pid file self-heal on the
/// next daemon start, since `run` then proceeds instead of exiting.
#[cfg(windows)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    use windows_sys::Win32::Foundation::{CloseHandle, FALSE, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: handle checked before use; code is caller-owned; handle closed
    // exactly once.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid as u32);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let exit_ok = GetExitCodeProcess(h, &mut code);
        if exit_ok == 0 || code != STILL_ACTIVE as u32 {
            CloseHandle(h);
            return false;
        }

        let mut name_buf = [0u16; 260]; // MAX_PATH
        let mut name_len = name_buf.len() as u32;
        let name_ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, name_buf.as_mut_ptr(), &mut name_len);
        CloseHandle(h);
        if name_ok == 0 {
            return false;
        }
        let image_path = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        is_cache_ttl_image(&image_path)
    }
}

/// Pure suffix check behind the pid-recycling guard above, split out for
/// unit testing without a real process handle. Case-insensitive: Windows
/// image paths are case-preserving but not case-sensitive.
#[cfg(windows)]
fn is_cache_ttl_image(image_path: &str) -> bool {
    image_path.to_ascii_lowercase().ends_with("cache-ttl.exe")
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
    use crate::state::TimerEntry;

    #[test]
    fn is_running_false_for_a_missing_pid_file() {
        let path =
            std::env::temp_dir().join(format!("cache-ttl-no-such-pid-{}", std::process::id()));
        assert!(!is_running(&path));
    }

    // Unix `pid_alive` is a plain `kill(pid, 0)` liveness probe with no
    // image-name check, so testing it against the current (test-harness)
    // process's own pid is valid there.
    #[cfg(unix)]
    #[test]
    fn is_running_true_for_the_current_process() {
        let path = std::env::temp_dir().join(format!("cache-ttl-self-pid-{}", std::process::id()));
        std::fs::write(&path, format!("{}", std::process::id())).unwrap();
        assert!(is_running(&path));
        let _ = std::fs::remove_file(&path);
    }

    // Windows `pid_alive` additionally requires the pid's image name to end
    // in `cache-ttl.exe` (see the pid-recycling comment above `pid_alive`),
    // so the current process — the `cargo test` harness binary, not
    // `cache-ttl.exe` — must correctly read as NOT running even though it is
    // very much alive. This exercises the real `QueryFullProcessImageNameW`
    // path against a real, live process without needing to build/spawn the
    // actual `cache-ttl.exe` binary.
    #[cfg(windows)]
    #[test]
    fn is_running_false_for_the_current_process_wrong_image_name() {
        let path = std::env::temp_dir().join(format!("cache-ttl-self-pid-{}", std::process::id()));
        std::fs::write(&path, format!("{}", std::process::id())).unwrap();
        assert!(!is_running(&path));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(windows)]
    #[test]
    fn is_cache_ttl_image_matches_case_insensitively_and_rejects_others() {
        assert!(is_cache_ttl_image(
            r"C:\plugins\cache-ttl\target\release\cache-ttl.exe"
        ));
        assert!(is_cache_ttl_image(r"C:\Plugins\Cache-Ttl.EXE"));
        assert!(!is_cache_ttl_image(r"C:\Windows\System32\notepad.exe"));
        assert!(!is_cache_ttl_image(""));
    }

    fn agent(pane_id: &str, agent_status: &str) -> herdr::AgentInfo {
        herdr::AgentInfo {
            pane_id: pane_id.to_string(),
            agent: "claude".to_string(),
            agent_status: agent_status.to_string(),
            terminal_title_stripped: String::new(),
            agent_session: None,
            tokens: HashMap::new(),
        }
    }

    #[test]
    fn account_tokens_match_only_the_exact_badge() {
        let tokens = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert!(account_tokens_match(&tokens(&[("acct_c2", "c2")]), 2));
        assert!(account_tokens_match(
            &tokens(&[("acct_c2", "c2"), ("cache_ok", "56m")]),
            2
        ));
        assert!(!account_tokens_match(&tokens(&[]), 2));
        assert!(!account_tokens_match(&tokens(&[("acct_c1", "c1")]), 2));
        assert!(!account_tokens_match(
            &tokens(&[("acct_c1", "c1"), ("acct_c2", "c2")]),
            2
        ));
    }

    fn timer() -> TimerEntry {
        TimerEntry {
            last_turn: 0,
            ttl_seconds: 3600,
        }
    }

    #[test]
    fn selects_only_working_panes_that_still_carry_a_timer() {
        let agents = vec![
            agent("w1:p1", "working"), // tracked + working -> needs clearing
            agent("w1:p2", "idle"),    // tracked but idle -> leave it counting down
            agent("w1:p3", "working"), // working but never had a timer -> nothing to clear
        ];
        let mut timers: Timers = Timers::new();
        timers.insert("w1:p1".into(), timer());
        timers.insert("w1:p2".into(), timer());

        assert_eq!(
            panes_needing_clear(&agents, &timers),
            vec!["w1:p1".to_string()]
        );
    }

    #[test]
    fn no_working_panes_means_nothing_to_clear() {
        let agents = vec![agent("w1:p1", "idle"), agent("w1:p2", "done")];
        let mut timers: Timers = Timers::new();
        timers.insert("w1:p1".into(), timer());

        assert!(panes_needing_clear(&agents, &timers).is_empty());
    }
}
