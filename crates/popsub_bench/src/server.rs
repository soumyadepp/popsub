//! The broker under test, run as its own process, and readings of a process's CPU time and memory.
//!
//! The broker is a separate process so it doesn't share a Tokio runtime with the load generator,
//! and so its CPU time can be read on its own. PopSub is the usual one; `nats-server` and
//! `mosquitto` can be started the same way to run the same loads against them.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep};

/// How long the server gets to start listening.
const STARTUP_LIMIT: Duration = Duration::from_secs(15);

/// A publish rate limit high enough that the limiter never refuses a benchmark publisher.
const UNLIMITED_PUBLISHES: &str = "1000000000";

/// Where the server keeps topic logs and sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    /// The default backend: logs on disk, written with `buffered` durability.
    File,
    /// Everything in memory.
    Memory,
}

impl Backend {
    /// The backend's name in `storage.backend`.
    ///
    /// Returns:
    ///     `file` or `memory`.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::File => "file",
            Backend::Memory => "memory",
        }
    }
}

/// A broker running as a child process with its own data directory.
///
/// The process is killed and its directory removed when this is dropped.
pub struct ServerProcess {
    child: Child,
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    port: u16,
    dir: TempDir,
}

impl ServerProcess {
    /// Starts `popsub server` on a free local port and waits until it accepts connections.
    ///
    /// The child gets a clean environment, so nothing from the caller's shell changes its
    /// settings. Two settings always differ from the defaults: the connection limit is off and
    /// the publish rate limit is out of reach. The data directory is a fresh temporary folder.
    /// The admin account keeps its default name and password, which the server accepts on
    /// `127.0.0.1`.
    ///
    /// Args:
    ///     binary: Path to the `popsub` executable.
    ///     backend: The storage backend to run with.
    ///     settings: More settings as environment variables, applied last, so they can
    ///         override the backend.
    ///
    /// Returns:
    ///     The running server.
    ///
    /// Raises:
    ///     String: When the binary can't be started, exits early, or isn't listening within
    ///         `STARTUP_LIMIT`.
    pub async fn start(
        binary: &Path,
        backend: Backend,
        settings: &[(&str, &str)],
    ) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("creating a data directory: {e}"))?;
        let port = free_port()?;
        let mut env = vec![
            ("SERVER_HOST".to_string(), "127.0.0.1".to_string()),
            ("SERVER_PORT".to_string(), port.to_string()),
            ("STORAGE_BACKEND".to_string(), backend.as_str().to_string()),
            (
                "STORAGE_PATH".to_string(),
                dir.path().join("data").display().to_string(),
            ),
            ("BROKER__MAX_CONNECTIONS".to_string(), "0".to_string()),
            (
                "LIMITS__PUBLISH_PER_SECOND".to_string(),
                UNLIMITED_PUBLISHES.to_string(),
            ),
            (
                "LIMITS__PUBLISH_BURST".to_string(),
                UNLIMITED_PUBLISHES.to_string(),
            ),
        ];
        for (key, value) in settings {
            env.push(((*key).to_string(), (*value).to_string()));
        }
        Self::launch(binary, vec!["server".to_string()], env, port, dir).await
    }

    /// Starts `nats-server` on a free local port.
    ///
    /// Everything is left at its defaults. With JetStream on, its store is a folder in the
    /// server's temporary directory.
    ///
    /// Args:
    ///     binary: Path to the `nats-server` executable.
    ///     jetstream: Whether to turn JetStream on.
    ///
    /// Returns:
    ///     The running server.
    ///
    /// Raises:
    ///     String: When the binary can't be started or isn't listening in time.
    pub async fn start_nats(binary: &Path, jetstream: bool) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("creating a data directory: {e}"))?;
        let port = free_port()?;
        let mut args = vec![
            "-a".to_string(),
            "127.0.0.1".to_string(),
            "-p".to_string(),
            port.to_string(),
        ];
        if jetstream {
            args.push("-js".to_string());
            args.push("-sd".to_string());
            args.push(dir.path().join("jetstream").display().to_string());
        }
        Self::launch(binary, args, Vec::new(), port, dir).await
    }

    /// Starts `mosquitto` on a free local port with a configuration written for the run.
    ///
    /// Four settings differ from Mosquitto's defaults: anonymous clients are allowed,
    /// persistence is off, the queue of messages waiting for a slow client has no limit (the
    /// default drops QoS 1 messages past 1,000), and Nagle's algorithm is off, as it is for
    /// the other brokers.
    ///
    /// Args:
    ///     binary: Path to the `mosquitto` executable.
    ///
    /// Returns:
    ///     The running broker.
    ///
    /// Raises:
    ///     String: When the configuration can't be written, or the binary can't be started or
    ///         isn't listening in time.
    pub async fn start_mosquitto(binary: &Path) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("creating a data directory: {e}"))?;
        let port = free_port()?;
        let config = dir.path().join("mosquitto.conf");
        std::fs::write(
            &config,
            format!(
                "listener {port} 127.0.0.1\nallow_anonymous true\npersistence false\nmax_queued_messages 0\nset_tcp_nodelay true\n"
            ),
        )
        .map_err(|e| format!("writing the Mosquitto configuration: {e}"))?;
        let args = vec!["-c".to_string(), config.display().to_string()];
        Self::launch(binary, args, Vec::new(), port, dir).await
    }

    /// Starts a broker and waits until it accepts connections.
    ///
    /// Args:
    ///     program: The executable.
    ///     args: Its arguments.
    ///     env: Every environment variable it gets.
    ///     port: The port it was told to listen on.
    ///     dir: Its working directory, removed when the process is dropped.
    ///
    /// Returns:
    ///     The running broker.
    ///
    /// Raises:
    ///     String: When it can't be started, exits early, or isn't listening in time.
    async fn launch(
        program: &Path,
        args: Vec<String>,
        env: Vec<(String, String)>,
        port: u16,
        dir: TempDir,
    ) -> Result<Self, String> {
        let child = spawn(program, &args, &env, dir.path())?;
        let mut server = Self {
            child,
            program: program.to_path_buf(),
            args,
            env,
            port,
            dir,
        };
        server.wait_until_listening().await?;
        Ok(server)
    }

    /// Stops the broker with SIGTERM and starts it again on the same data directory and port.
    ///
    /// Returns:
    ///     How long the new process took, from being started, to accept a connection.
    ///
    /// Raises:
    ///     String: When the broker can't be started again or isn't listening in time.
    pub async fn restart(&mut self) -> Result<Duration, String> {
        let terminated = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .is_ok_and(|status| status.success());
        if !terminated {
            let _ = self.child.kill();
        }
        let deadline = Instant::now() + STARTUP_LIMIT;
        while self.child.try_wait().map_err(|e| e.to_string())?.is_none() {
            if Instant::now() >= deadline {
                let _ = self.child.kill();
            }
            sleep(Duration::from_millis(10)).await;
        }
        let started = Instant::now();
        self.child = spawn(&self.program, &self.args, &self.env, self.dir.path())?;
        self.wait_until_listening().await?;
        Ok(started.elapsed())
    }

    /// Waits until the broker accepts a TCP connection.
    ///
    /// Raises:
    ///     String: When the broker exits first, with its log, or isn't listening within
    ///         `STARTUP_LIMIT`.
    async fn wait_until_listening(&mut self) -> Result<(), String> {
        let address = self.address();
        let deadline = Instant::now() + STARTUP_LIMIT;
        loop {
            if TcpStream::connect(&address).await.is_ok() {
                return Ok(());
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                let log =
                    std::fs::read_to_string(self.dir.path().join("server.log")).unwrap_or_default();
                return Err(format!(
                    "{} exited at startup ({status}):\n{log}",
                    self.program.display()
                ));
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                return Err(format!(
                    "{} wasn't listening on {address} in time",
                    self.program.display()
                ));
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    /// The broker's TCP address.
    ///
    /// Returns:
    ///     `127.0.0.1:<port>`.
    pub fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// The broker's WebSocket URL, for PopSub.
    ///
    /// Returns:
    ///     A `ws://127.0.0.1:<port>` URL.
    pub fn url(&self) -> String {
        format!("ws://{}", self.address())
    }

    /// The broker's process id.
    ///
    /// Returns:
    ///     The child's pid.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ServerProcess {
    /// Kills the broker and waits for it, so its port and data directory are free again.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts a broker process with a clean environment, logging to `server.log` in `dir`.
///
/// Args:
///     program: The executable.
///     args: Its arguments.
///     env: Every environment variable the child gets.
///     dir: The child's working directory, where its log is appended.
///
/// Returns:
///     The child process.
///
/// Raises:
///     String: When the log can't be opened or the program can't be started.
fn spawn(
    program: &Path,
    args: &[String],
    env: &[(String, String)],
    dir: &Path,
) -> Result<Child, String> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("server.log"))
        .map_err(|e| format!("opening the server log: {e}"))?;
    let errors = log.try_clone().map_err(|e| e.to_string())?;
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors))
        .spawn()
        .map_err(|e| format!("starting {}: {e}", program.display()))
}

/// Writes everything the operating system still holds for the disk before
/// a scenario starts, including what an earlier run left.
///
/// A scenario can write several gigabytes. Without this the next one starts
/// while the disk is still taking them, and measures that instead of the
/// broker: the same build then gives results that fall with every scenario.
///
/// `sync` alone isn't enough, because on macOS it returns before the writes
/// finish and freeing a deleted data directory takes disk time too. So where
/// `iostat` reports disk throughput, this also waits, for at most
/// `SETTLE_LIMIT_SECS`, until the disks are writing less than
/// `QUIET_MB_PER_SEC`. Where it doesn't, the `sync` is all there is.
pub fn settle_disk() {
    let _ = Command::new("sync").status();
    for _ in 0..SETTLE_LIMIT_SECS {
        match disk_mb_per_sec() {
            Some(rate) if rate >= QUIET_MB_PER_SEC => {}
            _ => return,
        }
    }
}

/// Disk throughput, in megabytes a second, below which the disk counts as quiet.
const QUIET_MB_PER_SEC: f64 = 20.0;

/// The longest `settle_disk` waits for a quiet disk, in seconds.
const SETTLE_LIMIT_SECS: usize = 60;

/// Measures every disk's throughput over one second with `iostat`.
///
/// Returns:
///     The total megabytes a second, or `None` when `iostat` isn't there or
///     prints something other than the macOS layout (kilobytes per
///     transfer, transfers and megabytes a second for each disk).
fn disk_mb_per_sec() -> Option<f64> {
    let output = Command::new("iostat")
        .args(["-d", "-w", "1", "-c", "2"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let last = text.lines().last()?;
    let columns: Vec<f64> = last
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if columns.is_empty() || !columns.len().is_multiple_of(3) {
        return None;
    }
    Some(columns.iter().skip(2).step_by(3).sum())
}

/// Finds an executable by name on `PATH` and in the usual places a package manager puts
/// server programs.
///
/// Args:
///     name: The program's file name, such as `nats-server`.
///
/// Returns:
///     The first match, or `None` when there isn't one.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            ["/opt/homebrew/sbin", "/usr/local/sbin", "/usr/sbin"]
                .iter()
                .map(PathBuf::from),
        )
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Asks the operating system for a port nothing is listening on.
///
/// Returns:
///     The port. Another process could take it before the server binds it, which shows up as
///     the server failing to start.
///
/// Raises:
///     String: When no local port can be bound.
fn free_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("finding a port: {e}"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|e| e.to_string())
}

/// Reads how much CPU time a process has used since it started, user and system together.
///
/// On Linux this comes from `/proc/<pid>/stat`, assuming the usual 100 clock ticks a second.
/// Elsewhere it comes from `ps -o cputime=`.
///
/// Args:
///     pid: The process to read.
///
/// Returns:
///     The CPU time in seconds, or `None` when it can't be read.
pub fn cpu_seconds(pid: u32) -> Option<f64> {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        return parse_proc_stat(&stat);
    }
    let output = Command::new("ps")
        .args(["-o", "cputime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_cputime(String::from_utf8_lossy(&output.stdout).trim())
}

/// Reads a process's resident memory.
///
/// Args:
///     pid: The process to read.
///
/// Returns:
///     The resident set size in bytes, or `None` when `ps` can't report it.
pub fn resident_bytes(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let kibibytes: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .ok()?;
    Some(kibibytes * 1024)
}

/// Adds up user and system time from the contents of `/proc/<pid>/stat`.
///
/// The process name sits in parentheses and may contain spaces, so fields are counted from the
/// last closing parenthesis: user time is the 12th field after it and system time the 13th.
///
/// Args:
///     stat: The file's contents.
///
/// Returns:
///     The CPU time in seconds, or `None` when the fields are missing.
pub fn parse_proc_stat(stat: &str) -> Option<f64> {
    let after_name = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    let user: f64 = fields.get(11)?.parse().ok()?;
    let system: f64 = fields.get(12)?.parse().ok()?;
    Some((user + system) / 100.0)
}

/// Parses the `cputime` column of `ps`.
///
/// Accepts `M:SS.cc` (macOS), `HH:MM:SS` (Linux) and a leading `D-` for days.
///
/// Args:
///     text: The column's text, trimmed.
///
/// Returns:
///     The time in seconds, or `None` when the text isn't a time.
pub fn parse_cputime(text: &str) -> Option<f64> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<f64>().ok()?, clock),
        None => (0.0, text),
    };
    let mut seconds = days * 86_400.0;
    let mut unit = 1.0;
    let parts: Vec<&str> = clock.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for part in parts.iter().rev() {
        seconds += part.parse::<f64>().ok()? * unit;
        unit *= 60.0;
    }
    Some(seconds)
}
