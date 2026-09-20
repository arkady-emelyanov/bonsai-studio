//! Supervises llama-server as a child process.
//!
//! The server runs out-of-process on purpose. Model loads OOM -- a 100K context
//! on a 12 GiB card sits close enough to the limit that it happens in normal
//! use -- and a CUDA OOM aborts the process it happens in. Out here that is a
//! red status light and a restart button; in-process it would take the settings
//! window down with it, leaving no way to lower the context and try again.

use crate::config::Settings;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Executable name and the variable used to find libraries beside it differ per
/// platform. Windows resolves DLLs from the directory of the exe and from PATH;
/// macOS uses DYLD_LIBRARY_PATH; other unixes use LD_LIBRARY_PATH.
#[cfg(windows)]
const SERVER_BIN: &str = "llama-server.exe";
#[cfg(not(windows))]
const SERVER_BIN: &str = "llama-server";

#[cfg(windows)]
const LIB_PATH_VAR: &str = "PATH";
#[cfg(target_os = "macos")]
const LIB_PATH_VAR: &str = "DYLD_LIBRARY_PATH";
#[cfg(all(unix, not(target_os = "macos")))]
const LIB_PATH_VAR: &str = "LD_LIBRARY_PATH";

#[cfg(windows)]
const PATH_SEP: &str = ";";
#[cfg(not(windows))]
const PATH_SEP: &str = ":";

const LOG_LINES: usize = 400;
/// Model loading is slow (~20s for the 27B) and a cold page cache makes it
/// slower, so the readiness poll has to be patient before calling it a failure.
const READY_TIMEOUT: Duration = Duration::from_secs(600);
const TERM_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Stopped,
    Starting,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub state: State,
    pub message: Option<String>,
    pub pid: Option<u32>,
    pub base_url: Option<String>,
    pub chat_url: Option<String>,
    pub log: Vec<String>,
    /// Settings have moved since this process was started, so what is serving
    /// is no longer what the panel shows. Every setting reaches llama-server as
    /// a command-line flag, so nothing can be applied to a live process.
    pub restart_needed: bool,
}

struct Inner {
    child: Option<Child>,
    state: State,
    message: Option<String>,
    port: u16,
    log: VecDeque<String>,
    /// The flags this process was started with, kept so the UI can say when the
    /// settings have drifted away from what is actually running.
    args: Vec<String>,
    /// Bumped on every start so a poll thread from a previous run cannot write
    /// its verdict over a newer one.
    generation: u64,
}

impl Inner {
    fn push_log(&mut self, line: String) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }
}

#[derive(Clone)]
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                child: None,
                state: State::Stopped,
                message: None,
                port: 0,
                log: VecDeque::new(),
                args: Vec::new(),
                generation: 0,
            })),
        }
    }

    pub fn status(&self) -> ServerStatus {
        let inner = self.inner.lock().unwrap();
        let running = matches!(inner.state, State::Starting | State::Ready);
        ServerStatus {
            state: inner.state,
            message: inner.message.clone(),
            pid: inner.child.as_ref().map(|c| c.id()),
            base_url: running.then(|| format!("http://127.0.0.1:{}/v1", inner.port)),
            chat_url: running.then(|| format!("http://127.0.0.1:{}/", inner.port)),
            log: inner.log.iter().cloned().collect(),
            // Filled in by the caller, which is the only place that can see the
            // current settings to compare against.
            restart_needed: false,
        }
    }

    /// The flags the running process was started with, or None when stopped.
    pub fn running_args(&self) -> Option<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        matches!(inner.state, State::Starting | State::Ready).then(|| inner.args.clone())
    }

    pub fn start(&self, settings: &Settings, binary: PathBuf) -> Result<(), String> {
        {
            let inner = self.inner.lock().unwrap();
            if matches!(inner.state, State::Starting | State::Ready) {
                return Err("The server is already running.".into());
            }
        }

        let args = settings.to_args()?;
        check_port(settings.host(), settings.port)?;
        self.inner.lock().unwrap().args = args.clone();

        let mut cmd = Command::new(&binary);
        cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());

        // The binary is dynamically linked against libllama, libmtmd and the
        // ggml backends that ship beside it.
        if let Some(dir) = binary.parent() {
            let mut path = dir.as_os_str().to_os_string();
            if let Some(existing) = std::env::var_os(LIB_PATH_VAR) {
                path.push(PATH_SEP);
                path.push(existing);
            }
            cmd.env(LIB_PATH_VAR, path);
        }

        // If this app dies, the server must not outlive it holding ~9 GB of VRAM.
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", binary.display()))?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let generation = {
            let mut inner = self.inner.lock().unwrap();
            inner.generation += 1;
            inner.child = Some(child);
            inner.state = State::Starting;
            inner.message = Some("Loading model...".into());
            inner.port = settings.port;
            inner.log.clear();
            inner.push_log(format!("$ {} {}", binary.display(), args.join(" ")));
            inner.generation
        };

        if let Some(out) = stdout {
            self.drain(out);
        }
        if let Some(err) = stderr {
            self.drain(err);
        }
        self.poll_ready(settings.port, generation);
        Ok(())
    }

    /// Forward a child pipe into the ring buffer.
    fn drain<R: std::io::Read + Send + 'static>(&self, pipe: R) {
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                inner.lock().unwrap().push_log(line);
            }
        });
    }

    /// Watch until /health answers, the child exits, or we give up.
    fn poll_ready(&self, port: u16, generation: u64) {
        let inner = Arc::clone(&self.inner);
        let url = format!("http://127.0.0.1:{port}/health");
        std::thread::spawn(move || {
            let deadline = Instant::now() + READY_TIMEOUT;
            loop {
                // A newer start supersedes this thread.
                {
                    let mut guard = inner.lock().unwrap();
                    if guard.generation != generation {
                        return;
                    }
                    // Did it die on us? Surface the tail of the log, which is
                    // where the actual reason lives.
                    if let Some(child) = guard.child.as_mut() {
                        if let Ok(Some(exit)) = child.try_wait() {
                            let tail: Vec<String> =
                                guard.log.iter().rev().take(3).rev().cloned().collect();
                            guard.child = None;
                            guard.state = State::Error;
                            guard.message = Some(format!(
                                "llama-server exited ({exit}). {}",
                                tail.join(" | ")
                            ));
                            return;
                        }
                    } else {
                        return;
                    }
                }

                // 503 "Loading model" until the weights are in; 200 when serving.
                if let Ok(resp) = ureq::get(&url).timeout(Duration::from_secs(5)).call() {
                    if resp.status() == 200 {
                        let mut guard = inner.lock().unwrap();
                        if guard.generation == generation {
                            guard.state = State::Ready;
                            guard.message = None;
                        }
                        return;
                    }
                }

                if Instant::now() >= deadline {
                    let mut guard = inner.lock().unwrap();
                    if guard.generation == generation {
                        guard.state = State::Error;
                        guard.message = Some("Timed out waiting for the server to load.".into());
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    }

    pub fn stop(&self) -> Result<(), String> {
        // Take the child out from under the lock: waiting on it can take
        // seconds, and the UI keeps polling status throughout.
        let child = {
            let mut inner = self.inner.lock().unwrap();
            inner.generation += 1;
            inner.state = State::Stopped;
            inner.message = None;
            inner.child.take()
        };

        let Some(mut child) = child else { return Ok(()) };

        // SIGTERM lets llama-server release the GPU cleanly; SIGKILL is for when
        // it will not. Windows has no graceful equivalent for a non-console
        // child, so it goes straight to terminate rather than idling for the
        // grace period first.
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }

            let deadline = Instant::now() + TERM_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => return Ok(()),
                    Err(e) => return Err(e.to_string()),
                    Ok(None) => {}
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }
}

/// Fail before spawning, with a message that names the likely cause.
fn check_port(host: &str, port: u16) -> Result<(), String> {
    let bind_host = if host == "0.0.0.0" { "0.0.0.0" } else { "127.0.0.1" };
    match std::net::TcpListener::bind((bind_host, port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            let hint = match port {
                // 11435 is the conventional port for a second Ollama instance,
                // which is the one real cost of this default.
                11435 => " A second Ollama instance uses this port by default.",
                11434 => " That is Ollama's default port.",
                8080 => " Port 8080 is used by a lot of development servers.",
                _ => "",
            };
            Err(format!("Port {port} is already in use.{hint} Change it in Settings."))
        }
        Err(e) => Err(format!("Cannot bind port {port}: {e}")),
    }
}

/// Find llama-server: explicit override, then the bundled copy, then a local
/// build, then PATH.
pub fn resolve_binary(settings: &Settings, resource_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    let mut tried: Vec<String> = Vec::new();
    let mut candidates: Vec<PathBuf> = Vec::new();

    if !settings.server_path.trim().is_empty() {
        candidates.push(PathBuf::from(settings.server_path.trim()));
    }
    if let Some(p) = std::env::var_os("BONSAI_SERVER_BIN") {
        candidates.push(PathBuf::from(p));
    }
    if let Some(dir) = resource_dir {
        candidates.push(dir.join("llama").join(SERVER_BIN));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("llama").join(SERVER_BIN));
            candidates.push(dir.join(SERVER_BIN));
        }
    }
    // Development: the checkout this app lives in.
    if let Ok(cwd) = std::env::current_dir() {
        for base in cwd.ancestors().take(5) {
            candidates.push(base.join("src/llama.cpp/build/bin").join(SERVER_BIN));
        }
    }

    for path in candidates {
        if path.is_file() {
            return Ok(path);
        }
        tried.push(path.display().to_string());
    }

    if let Ok(path) = which_in_path(SERVER_BIN) {
        return Ok(path);
    }

    Err(format!(
        "Could not find llama-server. Set its path in Settings. Looked in:\n  {}",
        tried.join("\n  ")
    ))
}

fn which_in_path(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .ok_or(())
}
