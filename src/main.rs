mod config;
#[cfg(unix)]
mod daemon;
#[cfg(unix)]
mod handlers;
#[cfg(unix)]
mod herdr;
#[cfg(unix)]
mod state;

use std::process;

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64
}

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

#[cfg(unix)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: cache-ttl <command>");
        eprintln!("commands: daemon, on-status-change, on-pane-closed, reset-timer, show-timers, toggle-sort");
        process::exit(1);
    }

    let cfg = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    };

    let result = match args[1].as_str() {
        "daemon" => daemon::run(&cfg),
        "on-status-change" => handlers::status_change(&cfg),
        "on-pane-closed" => handlers::pane_closed(&cfg),
        "reset-timer" => handlers::reset_timer(&cfg),
        "show-timers" => handlers::show_timers(&cfg),
        "toggle-sort" => handlers::toggle_sort(&cfg),
        cmd => {
            eprintln!("unknown command: {cmd}");
            process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("{e}");
        process::exit(1);
    }
}

#[cfg(windows)]
fn main() {
    eprintln!("cache-ttl is not yet supported on Windows");
    process::exit(1);
}
