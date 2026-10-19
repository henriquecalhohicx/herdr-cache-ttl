use std::path::Path;
use std::{env, fs};

/// Account numbers that get a sidebar token. Claude Code config dirs map
/// `~/.claude` -> 1, `~/.claude2` -> 2, ... (the same `cN` badge the Claude
/// Code statusline shows); anything outside this range gets no badge.
pub fn accounts() -> std::ops::RangeInclusive<u32> {
    1..=9
}

pub fn token_name(account: u32) -> String {
    format!("acct_c{account}")
}

/// `.claude` -> 1, `.claudeN` -> N; any other directory name -> None.
pub fn account_from_dir_name(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix(".claude")?;
    let n = if suffix.is_empty() {
        1
    } else if suffix.bytes().all(|b| b.is_ascii_digit()) {
        suffix.parse().ok()?
    } else {
        return None;
    };
    accounts().contains(&n).then_some(n)
}

/// Session ids are UUIDs; anything else is refused before it is joined into
/// a filesystem path.
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Which Claude Code account owns `session_id`: the config dir whose
/// `projects/<project>/<session_id>.jsonl` transcript exists. herdr reports
/// the session id per pane but not the config dir, and the transcript is the
/// one place the two are tied together on disk.
pub fn find_account(home: &Path, session_id: &str) -> Option<u32> {
    if !is_safe_session_id(session_id) {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    for entry in fs::read_dir(home).ok()?.flatten() {
        let Some(account) = entry.file_name().to_str().and_then(account_from_dir_name) else {
            continue;
        };
        let Ok(projects) = fs::read_dir(entry.path().join("projects")) else {
            continue;
        };
        if projects
            .flatten()
            .any(|p| p.path().join(&file_name).is_file())
        {
            return Some(account);
        }
    }
    None
}

pub fn home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_config_dir_names_to_account_numbers() {
        assert_eq!(account_from_dir_name(".claude"), Some(1));
        assert_eq!(account_from_dir_name(".claude2"), Some(2));
        assert_eq!(account_from_dir_name(".claude3"), Some(3));
        assert_eq!(account_from_dir_name(".claude-code-plus"), None);
        assert_eq!(account_from_dir_name(".claude0"), None);
        assert_eq!(account_from_dir_name(".claude10"), None);
        assert_eq!(account_from_dir_name("claude"), None);
    }

    #[test]
    fn finds_the_account_whose_projects_hold_the_transcript() {
        let home =
            std::env::temp_dir().join(format!("cache-ttl-account-test-{}", std::process::id()));
        let id = "0ff1d4f3-312d-4358-8993-45785a4e9d55";
        let p1 = home.join(".claude").join("projects").join("C--a");
        let p2 = home.join(".claude2").join("projects").join("C--b");
        fs::create_dir_all(&p1).unwrap();
        fs::create_dir_all(&p2).unwrap();
        fs::write(p2.join(format!("{id}.jsonl")), "").unwrap();

        assert_eq!(find_account(&home, id), Some(2));
        assert_eq!(
            find_account(&home, "aaaaaaaa-0000-0000-0000-000000000000"),
            None
        );
        assert_eq!(find_account(&home, "..\\..\\x"), None);

        let _ = fs::remove_dir_all(&home);
    }
}
