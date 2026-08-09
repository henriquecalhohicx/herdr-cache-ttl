# herdr-cache-ttl

A [Herdr](https://herdr.dev) plugin that tracks prompt-cache TTL per agent pane. Shows a live countdown in the sidebar so you know which sessions are about to lose their cache — no more jumping between servers to check.

## How it works

The plugin watches for agent status transitions (via `pane.agent_status_changed`), stamps the time of each turn, and pushes a color-coded countdown into the sidebar:

- **Green** — plenty of time remaining
- **Amber** — under 10 minutes
- **Red** — under 5 minutes or expired (`0m`)

A background daemon ticks every 15 seconds, updating all tracked panes. Agents that are actively working get their timers refreshed automatically — no status transition needed. Notifications fire at 10m, 5m, and 1m remaining.

## Install

```bash
herdr plugin install nytafar/herdr-cache-ttl --yes
```

On linux/macos, no toolchain is needed on the target: `dist/` ships a prebuilt
binary per platform (macOS arm64, static-musl Linux x86_64) and the
`[[build]]` hook copies the right one into place at install time.

On **Windows**, there is no prebuilt `dist/` artifact — the `[[build]]` hook
instead runs `cargo build --release` at install time, so a Rust toolchain
(`cargo` on the herdr server process's PATH) is required on the target.

For local development, link the working copy instead:

```bash
make build
herdr plugin link /path/to/herdr-cache-ttl
```

Then add the tokens to your sidebar config —
`~/.config/herdr/config.toml` on linux/macos, `%APPDATA%\herdr\config.toml`
on Windows:

```toml
[ui.sidebar.agents]
rows = [
  ["state_icon", "workspace", "tab"],
  ["agent", { token = "$cache_ok", fg = "#4ade80" }, { token = "$cache_warn", fg = "#f59e0b" }, { token = "$cache_crit", fg = "#ef4444" }],
]
```

Reload and restart:

```bash
herdr server reload-config
herdr server stop && herdr
```

The daemon starts automatically via the `[[startup]]` hook on server start.

## Sort by cache TTL

Toggle a cache-first sort mode in the agent panel:

```bash
herdr plugin action invoke sort --plugin cache-ttl       # linux/macos
herdr plugin action invoke sort-win --plugin cache-ttl    # windows
```

When active, the sidebar label shows "cache" and agents are sorted by cache urgency (warmest first), with attention priority as a tiebreaker. Toggle again to clear the view and restore the native sort mode.

## Configuration

Create `config.json` in the plugin config directory (`herdr plugin config-dir cache-ttl`):

```json
{
  "ttl_seconds": 3600,
  "tick_interval": 15,
  "seconds_threshold": 300
}
```

| Key | Default | Description |
|-----|---------|-------------|
| `ttl_seconds` | `3600` | Cache TTL duration (60 min for standard, 300 for overage) |
| `tick_interval` | `15` | Seconds between countdown updates |
| `seconds_threshold` | `300` | Show `m:ss` format below this many seconds |
| `warn_at` | `600` | Amber threshold — seconds remaining |
| `crit_at` | `300` | Red threshold — seconds remaining |

## Actions

Action ids are suffixed `-win` on Windows (herdr rejects duplicate action ids
across platform variants, even with disjoint `platforms`):

| Action | linux/macos id | Windows id |
|--------|-----------------|------------|
| Reset cache timer — reset the countdown for the focused pane | `reset` | `reset-win` |
| Show cache timers — print all active timers as a table | `status` | `status-win` |
| Toggle cache sort — sort the agent panel by cache TTL urgency | `sort` | `sort-win` |

## Building

```bash
make build        # local binary at the plugin root, for `plugin link`
make dist         # release artifact for this host → dist/cache-ttl-<os>-<arch>
make dist-linux   # static musl x86_64 build → dist/cache-ttl-linux-x86_64
```

Requires Rust (cargo). Release artifacts in `dist/` are committed so that
linux/macos installs need no toolchain; refresh them when cutting a version.
Windows has no `dist/` artifact — it always compiles from source via the
`[[build]]` hook (see Install above), so `cargo build --release` (which lands
at `target\release\cache-ttl.exe`) is what `make build`'s Windows equivalent
should produce too.

## Requirements

- Herdr >= 0.7.5
- Rust toolchain — required on Windows targets at install time (the
  `[[build]]` hook compiles from source there); only needed on linux/macos to
  rebuild `dist/` artifacts

## License

MIT
