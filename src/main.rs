use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_WORK_MINUTES: u64 = 25;
const DEFAULT_BREAK_MINUTES: u64 = 5;
const MAX_MINUTES: u64 = 24 * 60;
const MAX_SETS: u64 = 100;
const STATE_DIR: &str = env!("CARGO_PKG_NAME");
const LEGACY_STATE_DIR: &str = "pomo";

#[derive(Parser, Debug)]
#[command(version, about = "Ultra-low resource Pomodoro timer (macOS)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start a work session (default: 25 minutes)
    Start {
        /// Session length in minutes
        minutes: Option<u64>,
        /// Number of work sessions (auto alternates work/break)
        #[arg(long)]
        sets: Option<u64>,
        /// Break length in minutes (used with --sets)
        #[arg(long = "break-minutes", requires = "sets")]
        break_minutes: Option<u64>,
    },
    /// Start a break session (default: 5 minutes)
    Break {
        /// Session length in minutes
        minutes: Option<u64>,
    },
    /// Show current timer status
    Status,
    /// Stop the current timer (if running)
    Stop,
    #[command(name = "__run", hide = true)]
    Run {
        #[arg(long, value_enum)]
        mode: Mode,
        #[arg(long)]
        ready_fd: Option<i32>,
        #[arg(long)]
        sets: Option<u64>,
        #[arg(long)]
        set: Option<u64>,
        #[arg(long = "work-minutes")]
        work_minutes: Option<u64>,
        #[arg(long = "break-minutes")]
        break_minutes: Option<u64>,
        #[arg(long)]
        start_ts: i64,
        #[arg(long)]
        end_ts: i64,
        #[arg(long)]
        minutes: u64,
    },
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Work,
    Break,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Work => write!(f, "work"),
            Mode::Break => write!(f, "break"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct State {
    pid: i32,
    mode: Mode,
    start_ts: i64,
    end_ts: i64,
    minutes: u64,
    #[serde(default)]
    cycle: Option<Cycle>,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Cycle {
    set: u64,
    sets: u64,
    work_minutes: u64,
    break_minutes: u64,
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::Start {
            minutes,
            sets,
            break_minutes,
        } => {
            if let Err(e) = start_work(minutes, sets, break_minutes) {
                eprintln!("{e}");
                2
            } else {
                0
            }
        }
        Commands::Break { minutes } => {
            if let Err(e) = start_single_session(Mode::Break, minutes) {
                eprintln!("{e}");
                2
            } else {
                0
            }
        }
        Commands::Status => match status() {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{e}");
                2
            }
        },
        Commands::Stop => match stop() {
            Ok(code) => code,
            Err(e) => {
                eprintln!("{e}");
                2
            }
        },
        Commands::Run {
            mode,
            ready_fd,
            sets,
            set,
            work_minutes,
            break_minutes,
            start_ts,
            end_ts,
            minutes,
        } => {
            if let Err(e) = run_daemon(
                mode,
                ready_fd,
                sets,
                set,
                work_minutes,
                break_minutes,
                start_ts,
                end_ts,
                minutes,
            ) {
                eprintln!("{e}");
                2
            } else {
                0
            }
        }
    };

    std::process::exit(exit_code);
}

fn start_work(minutes: Option<u64>, sets: Option<u64>, break_minutes: Option<u64>) -> Result<(), String> {
    let Some(sets) = sets else {
        return start_single_session(Mode::Work, minutes);
    };

    validate_sets(sets)?;
    if sets <= 1 {
        return start_single_session(Mode::Work, minutes);
    }

    let work_minutes = minutes.unwrap_or(DEFAULT_WORK_MINUTES);
    let break_minutes = break_minutes.unwrap_or(DEFAULT_BREAK_MINUTES);
    validate_minutes(work_minutes)?;
    validate_minutes(break_minutes)?;

    let cycle = Cycle {
        set: 1,
        sets,
        work_minutes,
        break_minutes,
    };
    start_session(Mode::Work, work_minutes, Some(cycle))
}

fn start_single_session(mode: Mode, minutes: Option<u64>) -> Result<(), String> {
    let minutes = minutes.unwrap_or(match mode {
        Mode::Work => DEFAULT_WORK_MINUTES,
        Mode::Break => DEFAULT_BREAK_MINUTES,
    });
    validate_minutes(minutes)?;
    start_session(mode, minutes, None)
}

fn start_session(mode: Mode, minutes: u64, cycle: Option<Cycle>) -> Result<(), String> {
    let (state_path, legacy_state_path) = state_paths()?;
    stop_existing(&legacy_state_path)?;
    stop_existing(&state_path)?;

    let start_ts = now_unix();
    let end_ts = start_ts
        .checked_add((minutes as i64).saturating_mul(60))
        .ok_or_else(|| "timestamp overflow".to_string())?;

    let (ready_read_fd, ready_write_fd) = create_pipe()?;

    let exe = std::env::current_exe().map_err(|e| format!("failed to resolve current executable: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("__run")
        .arg("--mode")
        .arg(mode.to_string())
        .arg("--ready-fd")
        .arg(ready_read_fd.to_string());

    if let Some(cycle) = cycle {
        cmd.arg("--sets")
            .arg(cycle.sets.to_string())
            .arg("--set")
            .arg(cycle.set.to_string())
            .arg("--work-minutes")
            .arg(cycle.work_minutes.to_string())
            .arg("--break-minutes")
            .arg(cycle.break_minutes.to_string());
    }

    cmd.arg("--start-ts")
        .arg(start_ts.to_string())
        .arg("--end-ts")
        .arg(end_ts.to_string())
        .arg("--minutes")
        .arg(minutes.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    unsafe {
        cmd.pre_exec(move || {
            // Ensure only the parent holds the write-end, so the daemon can block on EOF.
            libc::close(ready_write_fd);
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            unsafe {
                libc::close(ready_read_fd);
                libc::close(ready_write_fd);
            }
            return Err(format!("failed to spawn background process: {e}"));
        }
    };
    let pid = child.id() as i32;

    let state = State {
        pid,
        mode,
        start_ts,
        end_ts,
        minutes,
        cycle,
    };
    if let Err(e) = write_state(&state_path, &state) {
        let _ = send_sigterm(pid);
        unsafe {
            libc::close(ready_read_fd);
            libc::close(ready_write_fd);
        }
        return Err(e);
    }

    unsafe {
        libc::close(ready_read_fd);
        libc::close(ready_write_fd);
    }

    match state.cycle {
        Some(cycle) => println!(
            "started cycle: work {}m / break {}m x{} (pid {})",
            cycle.work_minutes, cycle.break_minutes, cycle.sets, pid
        ),
        None => println!("started {mode} timer for {minutes} minutes (pid {pid})"),
    }
    Ok(())
}

fn status() -> Result<i32, String> {
    let (primary_state_path, legacy_state_path) = state_paths()?;
    let (state_path, state) = match read_state(&primary_state_path)? {
        Some(state) => (primary_state_path, state),
        None => match read_state(&legacy_state_path)? {
            Some(state) => (legacy_state_path, state),
            None => {
                println!("not running");
                return Ok(1);
            }
        },
    };

    if !pid_alive(state.pid)? {
        clear_state(&state_path)?;
        println!("not running");
        return Ok(1);
    }

    let now = now_unix();
    let remaining_secs = (state.end_ts - now).max(0) as u64;

    println!("running");
    println!("mode: {0}", state.mode);
    println!("pid: {0}", state.pid);
    if let Some(cycle) = state.cycle {
        println!("set: {0}/{1}", cycle.set, cycle.sets);
    }
    println!("started_at: {0}", format_local_time(state.start_ts)?);
    println!("ends_at: {0}", format_local_time(state.end_ts)?);
    println!("remaining: {0}", format_duration(remaining_secs));
    Ok(0)
}

fn stop() -> Result<i32, String> {
    let (primary_state_path, legacy_state_path) = state_paths()?;
    let mut stopped = false;

    for state_path in [&primary_state_path, &legacy_state_path] {
        let Some(state) = read_state(state_path)? else {
            continue;
        };

        if pid_alive(state.pid)? {
            send_sigterm(state.pid)?;
            stopped = true;
        }
        clear_state(state_path)?;
    }

    if stopped {
        println!("stopped");
        Ok(0)
    } else {
        println!("not running");
        Ok(1)
    }
}

fn run_daemon(
    mode: Mode,
    ready_fd: Option<i32>,
    sets: Option<u64>,
    set: Option<u64>,
    work_minutes: Option<u64>,
    break_minutes: Option<u64>,
    start_ts: i64,
    end_ts: i64,
    minutes: u64,
) -> Result<(), String> {
    validate_minutes(minutes)?;
    let mut cycle = parse_cycle(sets, set, work_minutes, break_minutes)?;
    let (state_path, _) = state_paths()?;
    let pid = std::process::id() as i32;

    if let Some(fd) = ready_fd {
        wait_for_ready_fd(fd);
    }

    let mut current_mode = mode;
    let mut current_start_ts = start_ts;
    let mut current_end_ts = end_ts;
    let mut current_minutes = minutes;

    loop {
        if !state_matches(
            &state_path,
            pid,
            current_mode,
            current_start_ts,
            current_end_ts,
            current_minutes,
        )? {
            return Ok(());
        }

        let now = now_unix();
        if current_end_ts > now {
            std::thread::sleep(Duration::from_secs((current_end_ts - now) as u64));
        }

        if !state_matches(
            &state_path,
            pid,
            current_mode,
            current_start_ts,
            current_end_ts,
            current_minutes,
        )? {
            return Ok(());
        }

        let finished_mode = current_mode;

        match cycle {
            None => {
                let _ = clear_state(&state_path);
                notify(finished_mode);
                return Ok(());
            }
            Some(mut cfg) => {
                if finished_mode == Mode::Work {
                    if cfg.set >= cfg.sets {
                        let _ = clear_state(&state_path);
                        notify(finished_mode);
                        return Ok(());
                    }

                    let next_mode = Mode::Break;
                    let next_start_ts = now_unix();
                    let next_minutes = cfg.break_minutes;
                    let next_end_ts = next_start_ts
                        .checked_add((next_minutes as i64).saturating_mul(60))
                        .ok_or_else(|| "timestamp overflow".to_string())?;

                    let next_state = State {
                        pid,
                        mode: next_mode,
                        start_ts: next_start_ts,
                        end_ts: next_end_ts,
                        minutes: next_minutes,
                        cycle: Some(cfg),
                    };

                    write_state(&state_path, &next_state)?;
                    current_mode = next_mode;
                    current_start_ts = next_start_ts;
                    current_end_ts = next_end_ts;
                    current_minutes = next_minutes;
                    cycle = Some(cfg);

                    notify(finished_mode);
                    continue;
                }

                // Break finished; advance to next work session.
                if cfg.set >= cfg.sets {
                    let _ = clear_state(&state_path);
                    notify(finished_mode);
                    return Ok(());
                }

                cfg.set = cfg
                    .set
                    .checked_add(1)
                    .ok_or_else(|| "set counter overflow".to_string())?;

                let next_mode = Mode::Work;
                let next_start_ts = now_unix();
                let next_minutes = cfg.work_minutes;
                let next_end_ts = next_start_ts
                    .checked_add((next_minutes as i64).saturating_mul(60))
                    .ok_or_else(|| "timestamp overflow".to_string())?;

                let next_state = State {
                    pid,
                    mode: next_mode,
                    start_ts: next_start_ts,
                    end_ts: next_end_ts,
                    minutes: next_minutes,
                    cycle: Some(cfg),
                };

                write_state(&state_path, &next_state)?;
                current_mode = next_mode;
                current_start_ts = next_start_ts;
                current_end_ts = next_end_ts;
                current_minutes = next_minutes;
                cycle = Some(cfg);

                notify(finished_mode);
            }
        }
    }
}

fn notify(mode: Mode) {
    let (body, beeps) = match mode {
        Mode::Work => ("Work finished. Time for a break.", 2),
        Mode::Break => ("Break finished. Back to work.", 1),
    };
    let script = format!("display notification \"{body}\" with title \"Pomodoro\"");
    let _ = Command::new("osascript").arg("-e").arg(script).status();

    for _ in 0..beeps {
        let _ = Command::new("osascript").arg("-e").arg("beep").status();
    }
}

fn state_path_for_dir(dir_name: &str) -> Result<PathBuf, String> {
    if let Some(base) = std::env::var_os("XDG_DATA_HOME") {
        if !base.as_os_str().is_empty() {
            return Ok(PathBuf::from(base).join(dir_name).join("state.json"));
        }
    }
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join(dir_name)
        .join("state.json"))
}

fn state_paths() -> Result<(PathBuf, PathBuf), String> {
    Ok((
        state_path_for_dir(STATE_DIR)?,
        state_path_for_dir(LEGACY_STATE_DIR)?,
    ))
}

fn read_state(path: &Path) -> Result<Option<State>, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("failed to read state file {path:?}: {e}")),
    };
    match serde_json::from_str::<State>(&contents) {
        Ok(state) => Ok(Some(state)),
        Err(_) => {
            let _ = clear_state(path);
            Ok(None)
        }
    }
}

fn write_state(path: &Path, state: &State) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("invalid state path {path:?}"))?;
    fs::create_dir_all(dir).map_err(|e| format!("failed to create state dir {dir:?}: {e}"))?;

    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec(state).map_err(|e| format!("failed to serialize state: {e}"))?;
    fs::write(&tmp, json).map_err(|e| format!("failed to write state file {tmp:?}: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("failed to persist state file {path:?}: {e}"))?;
    Ok(())
}

fn clear_state(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("failed to remove state file {path:?}: {e}")),
    }
}

fn state_matches(
    path: &Path,
    pid: i32,
    mode: Mode,
    start_ts: i64,
    end_ts: i64,
    minutes: u64,
) -> Result<bool, String> {
    let Some(state) = read_state(path)? else {
        return Ok(false);
    };
    Ok(state.pid == pid
        && state.mode == mode
        && state.start_ts == start_ts
        && state.end_ts == end_ts
        && state.minutes == minutes)
}

fn stop_existing(state_path: &Path) -> Result<(), String> {
    let Some(state) = read_state(state_path)? else {
        return Ok(());
    };

    if pid_alive(state.pid)? {
        send_sigterm(state.pid)?;
    }
    clear_state(state_path)?;
    Ok(())
}

fn pid_alive(pid: i32) -> Result<bool, String> {
    if pid <= 0 {
        return Ok(false);
    }
    let res = unsafe { libc::kill(pid, 0) };
    if res == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(false),
        Some(code) if code == libc::EPERM => Ok(true),
        _ => Err(format!("failed to check pid {pid}: {err}")),
    }
}

fn send_sigterm(pid: i32) -> Result<(), String> {
    let res = unsafe { libc::kill(pid, libc::SIGTERM) };
    if res == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::ESRCH => Ok(()),
        _ => Err(format!("failed to stop pid {pid}: {err}")),
    }
}

fn validate_sets(sets: u64) -> Result<(), String> {
    if sets == 0 {
        return Err("sets must be > 0".to_string());
    }
    if sets > MAX_SETS {
        return Err(format!("sets too large (max {MAX_SETS})"));
    }
    Ok(())
}

fn validate_minutes(minutes: u64) -> Result<(), String> {
    if minutes == 0 {
        return Err("minutes must be > 0".to_string());
    }
    if minutes > MAX_MINUTES {
        return Err(format!("minutes too large (max {MAX_MINUTES})"));
    }
    Ok(())
}

fn parse_cycle(
    sets: Option<u64>,
    set: Option<u64>,
    work_minutes: Option<u64>,
    break_minutes: Option<u64>,
) -> Result<Option<Cycle>, String> {
    if sets.is_none() && set.is_none() && work_minutes.is_none() && break_minutes.is_none() {
        return Ok(None);
    }

    let sets = sets.ok_or_else(|| "invalid cycle args: missing --sets".to_string())?;
    let set = set.ok_or_else(|| "invalid cycle args: missing --set".to_string())?;
    let work_minutes =
        work_minutes.ok_or_else(|| "invalid cycle args: missing --work-minutes".to_string())?;
    let break_minutes =
        break_minutes.ok_or_else(|| "invalid cycle args: missing --break-minutes".to_string())?;

    validate_sets(sets)?;
    if sets <= 1 {
        return Ok(None);
    }
    if set == 0 || set > sets {
        return Err("invalid cycle args: --set must be in 1..=sets".to_string());
    }
    validate_minutes(work_minutes)?;
    validate_minutes(break_minutes)?;

    Ok(Some(Cycle {
        set,
        sets,
        work_minutes,
        break_minutes,
    }))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs() as i64
}

fn format_duration(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn format_local_time(ts: i64) -> Result<String, String> {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tm_ptr = unsafe { libc::localtime_r(&t, &mut tm) };
    if tm_ptr.is_null() {
        return Err("failed to convert timestamp to local time".to_string());
    }

    let fmt = CString::new("%Y-%m-%d %H:%M:%S").map_err(|e| e.to_string())?;
    let mut buf = [0i8; 64];
    let len = unsafe { libc::strftime(buf.as_mut_ptr(), buf.len(), fmt.as_ptr(), &tm) };
    if len == 0 {
        return Err("failed to format local time".to_string());
    }
    let cstr = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Ok(cstr.to_string_lossy().into_owned())
}

fn create_pipe() -> Result<(i32, i32), String> {
    let mut fds = [0i32; 2];
    let res = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if res == -1 {
        return Err(format!("failed to create pipe: {}", io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

fn wait_for_ready_fd(fd: i32) {
    let mut buf = [0u8; 1];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), 1) };
        if n == 1 || n == 0 {
            break;
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        break;
    }
    unsafe {
        libc::close(fd);
    }
}
