use serde::Deserialize;
use std::{env, fs, io};

use crate::config::Config;
use crate::herdr;
use crate::state::{State, TimerEntry};

/// A pane's cache badge is only informative while the agent isn't actively
/// working: `"working"` is guaranteed cache-fresh (any real turn just
/// happened or is about to), so showing a countdown there is noise, not
/// signal. The countdown only means something once the agent stops and the
/// cache actually starts aging.
enum TurnAction {
    Clear,
    StartCountdown,
}

fn turn_action_for(agent_status: &str) -> TurnAction {
    if agent_status == "working" {
        TurnAction::Clear
    } else {
        TurnAction::StartCountdown
    }
}

pub fn status_change(cfg: &Config) -> io::Result<()> {
    let Some(event_json) = nonempty_var("HERDR_PLUGIN_EVENT_JSON") else {
        return Ok(());
    };

    let Ok(event) = serde_json::from_str::<StatusEvent>(&event_json) else {
        return Ok(());
    };
    if event.data.pane_id.is_empty() || event.data.agent_status.is_empty() {
        return Ok(());
    }

    let mut agent = event.data.agent.unwrap_or_default();
    if agent.is_empty() {
        if let Some(ctx_json) = nonempty_var("HERDR_PLUGIN_CONTEXT_JSON") {
            if let Ok(ctx) = serde_json::from_str::<ContextJson>(&ctx_json) {
                agent = ctx.focused_pane_agent.unwrap_or_default();
            }
        }
    }

    if !agent.is_empty() && !cfg.tracked_agents.iter().any(|&a| a == agent) {
        return Ok(());
    }

    let st = State::new(cfg);
    st.init()?;
    match turn_action_for(&event.data.agent_status) {
        TurnAction::Clear => {
            st.delete_timer_and_notification(&event.data.pane_id)?;
            herdr::clear_pane_tokens(&event.data.pane_id, cfg)
        }
        TurnAction::StartCountdown => st.set_timer_and_clear_notification(
            &event.data.pane_id,
            TimerEntry {
                last_turn: crate::now_unix(),
                ttl_seconds: cfg.ttl_seconds,
            },
        ),
    }
}

pub fn pane_closed(cfg: &Config) -> io::Result<()> {
    let Some(event_json) = nonempty_var("HERDR_PLUGIN_EVENT_JSON") else {
        return Ok(());
    };
    let Ok(event) = serde_json::from_str::<PaneEvent>(&event_json) else {
        return Ok(());
    };
    if event.data.pane_id.is_empty() {
        return Ok(());
    }
    State::new(cfg).delete_timer_and_notification(&event.data.pane_id)
}

pub fn reset_timer(cfg: &Config) -> io::Result<()> {
    let pane_id = nonempty_var("HERDR_PANE_ID")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no pane in context"))?;

    let st = State::new(cfg);
    st.init()?;
    st.set_timer_and_clear_notification(
        &pane_id,
        TimerEntry {
            last_turn: crate::now_unix(),
            ttl_seconds: cfg.ttl_seconds,
        },
    )?;
    println!("Timer reset for {pane_id}");
    Ok(())
}

pub fn show_timers(cfg: &Config) -> io::Result<()> {
    let timers = State::new(cfg).read_timers();
    if timers.is_empty() {
        println!("No active timers");
        return Ok(());
    }

    let now = crate::now_unix();
    let mut ids: Vec<&String> = timers.keys().collect();
    ids.sort();

    println!("{:<12}  {:<8}  TITLE", "PANE", "TTL");
    println!("{:<12}  {:<8}  -----", "----", "---");

    for pane_id in ids {
        let entry = &timers[pane_id];
        let remaining = entry.ttl_seconds - (now - entry.last_turn);

        let label = if remaining <= 0 {
            "expired".into()
        } else if remaining <= 300 {
            format!("{}:{:02}", remaining / 60, remaining % 60)
        } else {
            format!("{}m", remaining / 60)
        };

        let title = herdr::agent_get_title(pane_id);
        println!("{:<12}  {:<8}  {}", pane_id, label, title);
    }
    Ok(())
}

pub fn toggle_sort(cfg: &Config) -> io::Result<()> {
    let socket = nonempty_var("HERDR_SOCKET_PATH")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HERDR_SOCKET_PATH not set"))?;

    let sort_file = cfg.state_dir.join("sort_active");

    if sort_file.exists() {
        let msg =
            r#"{"id":"off","method":"agent.view.clear","params":{"source":"plugin:cache-ttl"}}"#;
        herdr::socket_send(&socket, msg)
            .map_err(|e| io::Error::new(e.kind(), format!("failed to clear sort view: {e}")))?;
        let _ = fs::remove_file(&sort_file);
        println!("Cache sort off");
    } else {
        let msg = r#"{"id":"on","method":"agent.view.set","params":{"source":"plugin:cache-ttl","label":"cache","sort":[{"field":{"token":"cache_sort"},"order":"desc"},{"field":"attention","order":"desc"},{"field":"state_change_seq","order":"desc"}]}}"#;
        herdr::socket_send(&socket, msg)
            .map_err(|e| io::Error::new(e.kind(), format!("failed to set sort view: {e}")))?;
        fs::write(&sort_file, [])?;
        println!("Cache sort on");
    }
    Ok(())
}

fn nonempty_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.is_empty())
}

#[derive(Deserialize)]
struct StatusEvent {
    data: StatusData,
}

#[derive(Deserialize)]
struct StatusData {
    #[serde(default)]
    pane_id: String,
    #[serde(default)]
    agent_status: String,
    agent: Option<String>,
}

#[derive(Deserialize)]
struct PaneEvent {
    data: PaneData,
}

#[derive(Deserialize)]
struct PaneData {
    #[serde(default)]
    pane_id: String,
}

#[derive(Deserialize)]
struct ContextJson {
    focused_pane_agent: Option<String>,
}

#[cfg(test)]
mod turn_action_tests {
    use super::*;

    #[test]
    fn working_status_clears_instead_of_starting_a_countdown() {
        assert!(matches!(turn_action_for("working"), TurnAction::Clear));
    }

    #[test]
    fn non_working_statuses_start_a_countdown() {
        assert!(matches!(
            turn_action_for("idle"),
            TurnAction::StartCountdown
        ));
        assert!(matches!(
            turn_action_for("done"),
            TurnAction::StartCountdown
        ));
        assert!(matches!(
            turn_action_for("needs-attention"),
            TurnAction::StartCountdown
        ));
    }
}
