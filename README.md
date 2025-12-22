# tock

Ultra-lightweight Pomodoro timer for macOS (CLI-only) with near-zero CPU usage while running.

## Install

From crates.io:

```sh
cargo install tock256
```

From this directory:

```sh
cargo install --path .
```

Or build a release binary and copy it into your PATH:

```sh
cargo build --release
cp target/release/tock ~/.local/bin/
```

## Usage

```sh
tock start 25   # start a work session (default: 25)
tock break 5    # start a break session (default: 5)
tock start 25 --sets 4 --break-minutes 5  # run 4 work sessions with breaks between
tock status     # show current timer info (exit 0 if running, 1 otherwise)
tock stop       # stop the current timer (exit 0 if stopped, 1 otherwise)
```

## Roadmap

### Time logging (local first)

- [ ] Log sessions to a local CSV by default (append-only)
  - Path: `$XDG_DATA_HOME/tock/log.csv` (or `~/.local/share/tock/log.csv`)
  - Record both `work` and `break` sessions
  - Capture end reason: `completed` / `stopped` / `replaced_by_new_timer`
- [ ] Ensure logs are written for all termination paths
  - Natural completion (daemon)
  - `tock stop` (foreground)
  - Starting a new timer while one is running (auto-stop existing)
- [ ] Add CLI helpers
  - `tock log path` (print current CSV path)
  - `tock log tail --n 50` (show recent entries)
  - `tock log today|week` (simple summaries)

### Metadata (optional, but high leverage)

- [ ] Add `--task`, `--tags`, `--note` and persist them in state + log output
- [ ] Show task/tags in `tock status`

### Google Sheets sync (optional)

- [ ] OAuth for personal MyDrive, store tokens locally (CSV remains source of truth)
- [ ] `tock sync init` to set sheet ID/name and write header row
- [ ] `tock sync run` to append unsynced rows (offline-friendly with retry queue)
- [ ] Avoid duplicates with a stable `id` column (idempotent sync)

## Notes

- Notifications are sent via `osascript` using `display notification ...` (macOS Notification Center).
  If you don’t see notifications, make sure your terminal app (Terminal/iTerm2/etc.) is allowed to post notifications in
  System Settings → Notifications.
- Low resource design: the background process sleeps until the session deadline (no periodic polling).
- State is stored at `$XDG_DATA_HOME/tock/state.json` if `XDG_DATA_HOME` is set; otherwise at `~/.local/share/tock/state.json`.
