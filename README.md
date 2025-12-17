# pomo

Ultra-lightweight Pomodoro timer for macOS (CLI-only) with near-zero CPU usage while running.

## Install

From this directory:

```sh
cargo install --path .
```

Or build a release binary and copy it into your PATH:

```sh
cargo build --release
cp target/release/pomo ~/.local/bin/
```

## Usage

```sh
pomo start 25   # start a work session (default: 25)
pomo break 5    # start a break session (default: 5)
pomo start 25 --sets 4 --break-minutes 5  # run 4 work sessions with breaks between
pomo status     # show current timer info (exit 0 if running, 1 otherwise)
pomo stop       # stop the current timer (exit 0 if stopped, 1 otherwise)
```

## Notes

- Notifications are sent via `osascript` using `display notification ...` (macOS Notification Center).
  If you don’t see notifications, make sure your terminal app (Terminal/iTerm2/etc.) is allowed to post notifications in
  System Settings → Notifications.
- Low resource design: the background process sleeps until the session deadline (no periodic polling).
- State is stored at `$XDG_DATA_HOME/pomo/state.json` if `XDG_DATA_HOME` is set; otherwise at `~/.local/share/pomo/state.json`.
