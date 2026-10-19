use serde::Deserialize;
use std::collections::HashMap;
use std::io;
#[cfg(unix)]
use std::io::Write;
use std::process::Command;
use std::sync::OnceLock;
#[cfg(unix)]
use std::time::Duration;

use crate::config::Config;

/// The herdr binary is not always on the plugin process's PATH — a server
/// started from a non-login shell inherits none of the shell profile, so an
/// install under ~/.local/bin is invisible. herdr hands us the absolute path
/// in the environment; use it, and fall back to PATH lookup.
fn herdr_bin() -> &'static str {
    static BIN: OnceLock<String> = OnceLock::new();
    BIN.get_or_init(|| {
        std::env::var("HERDR_BIN_PATH")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "herdr".to_string())
    })
}

#[derive(Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub terminal_title_stripped: String,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
    #[serde(default)]
    pub tokens: HashMap<String, String>,
}

#[derive(Deserialize, Default)]
pub struct AgentSession {
    #[serde(default)]
    pub value: String,
}

/// Account badge tokens live under their own metadata source, reported
/// without a TTL: the account never changes for a session, so they are
/// written once per (pane, session) instead of every tick, and a separate
/// source keeps them independent of the countdown tokens' 45s TTL.
pub const ACCOUNT_SOURCE: &str = "plugin:cache-ttl-account";

/// One token per account (`acct_c1` = `c1`, ...) so the sidebar row can give
/// each its own colour; the other account tokens are cleared in the same call.
pub fn set_account_token(pane_id: &str, account: u32) -> io::Result<()> {
    let name = crate::account::token_name(account);
    let token = format!("{name}=c{account}");
    let others: Vec<String> = crate::account::accounts()
        .filter(|&n| n != account)
        .map(crate::account::token_name)
        .collect();
    let mut args = vec![
        "pane",
        "report-metadata",
        pane_id,
        "--source",
        ACCOUNT_SOURCE,
        "--token",
        &token,
    ];
    for other in &others {
        args.extend(["--clear-token", other]);
    }

    let out = Command::new(herdr_bin()).args(&args).output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!(
            "{} pane report-metadata (account) failed: {}",
            herdr_bin(),
            stderr.trim()
        )));
    }
    Ok(())
}

pub fn set_pane_tokens(pane_id: &str, remaining: i64, cfg: &Config) -> io::Result<()> {
    let label = format_remaining(remaining, cfg.seconds_threshold);
    let sort_val = if remaining <= 0 {
        "000000".to_string()
    } else {
        format!("{:06}", remaining)
    };

    let mut args = vec![
        "pane",
        "report-metadata",
        pane_id,
        "--source",
        cfg.metadata_source,
        "--ttl-ms",
    ];
    let ttl_str = cfg.token_ttl_ms.to_string();
    args.push(&ttl_str);
    args.push("--token");
    let sort_token = format!("cache_sort={sort_val}");
    args.push(&sort_token);

    let tier_token;
    if remaining <= cfg.crit_at {
        tier_token = format!("cache_crit={label}");
        args.extend([
            "--token",
            &tier_token,
            "--clear-token",
            "cache_ok",
            "--clear-token",
            "cache_warn",
        ]);
    } else if remaining <= cfg.warn_at {
        tier_token = format!("cache_warn={label}");
        args.extend([
            "--token",
            &tier_token,
            "--clear-token",
            "cache_ok",
            "--clear-token",
            "cache_crit",
        ]);
    } else {
        tier_token = format!("cache_ok={label}");
        args.extend([
            "--token",
            &tier_token,
            "--clear-token",
            "cache_warn",
            "--clear-token",
            "cache_crit",
        ]);
    }

    let out = Command::new(herdr_bin()).args(&args).output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!(
            "{} pane report-metadata failed: {}",
            herdr_bin(),
            stderr.trim()
        )));
    }
    Ok(())
}

pub fn clear_pane_tokens(pane_id: &str, cfg: &Config) -> io::Result<()> {
    let out = Command::new(herdr_bin())
        .args([
            "pane",
            "report-metadata",
            pane_id,
            "--source",
            cfg.metadata_source,
            "--clear-token",
            "cache_ok",
            "--clear-token",
            "cache_warn",
            "--clear-token",
            "cache_crit",
            "--clear-token",
            "cache_sort",
        ])
        .output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(io::Error::other(format!(
            "{} pane report-metadata (clear) failed: {}",
            herdr_bin(),
            stderr.trim()
        )));
    }
    Ok(())
}

pub fn agent_list() -> io::Result<Vec<AgentInfo>> {
    let out = Command::new(herdr_bin()).args(["agent", "list"]).output()?;
    let resp: AgentListResp = serde_json::from_slice(&out.stdout)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(resp.result.agents)
}

pub fn agent_get_title(pane_id: &str) -> String {
    let Ok(out) = Command::new(herdr_bin())
        .args(["agent", "get", pane_id])
        .output()
    else {
        return pane_id.to_string();
    };
    let Ok(resp) = serde_json::from_slice::<AgentGetResp>(&out.stdout) else {
        return pane_id.to_string();
    };
    let title = &resp.result.agent.terminal_title_stripped;
    if title.is_empty() {
        pane_id.to_string()
    } else {
        title.clone()
    }
}

pub fn notify(title: &str, body: &str, sound: &str) {
    let _ = Command::new(herdr_bin())
        .args([
            "notification",
            "show",
            title,
            "--body",
            body,
            "--sound",
            sound,
        ])
        .output();
}

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
///
/// Known limitation: unlike the Unix path's `set_write_timeout(2s)`, this
/// `WriteFile` has no timeout — a stalled reader on the herdr side could
/// block `reapply_sort` indefinitely at daemon startup. A real timeout needs
/// overlapped I/O, which is a bigger change than this pass should attempt;
/// tracked as a known gap in CLAUDE.md.
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
    } else if written as usize != bytes.len() {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!(
                "partial named-pipe write: wrote {written} of {} bytes",
                bytes.len()
            ),
        ))
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

pub fn format_remaining(remaining: i64, threshold: i64) -> String {
    if remaining <= 0 {
        "0m".into()
    } else if remaining <= threshold {
        format!("{}:{:02}", remaining / 60, remaining % 60)
    } else {
        format!("{}m", remaining / 60)
    }
}

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

#[derive(Deserialize)]
struct AgentListResp {
    result: AgentListResult,
}

#[derive(Deserialize)]
struct AgentListResult {
    #[serde(default)]
    agents: Vec<AgentInfo>,
}

#[derive(Deserialize)]
struct AgentGetResp {
    result: AgentGetResult,
}

#[derive(Deserialize)]
struct AgentGetResult {
    agent: AgentInfo,
}
