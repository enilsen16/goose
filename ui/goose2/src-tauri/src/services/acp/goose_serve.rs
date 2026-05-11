use tauri::{Manager, Runtime};
use tauri_plugin_shell::ShellExt;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::services::distro_bundle::DistroBundleState;

use tokio::process::{Child, Command};
use tokio::sync::OnceCell;

const GOOSE_SERVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const GOOSE_SERVE_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);
const PID_REGISTRY_DIR_NAME: &str = "goose2-serve-pids";
const LEGACY_PID_FILE_NAME: &str = "goose2-serve.pid";
const STATE_DIR_ENV: &str = "GOOSE2_SERVE_STATE_DIR";
const LOCALHOST: &str = "127.0.0.1";
const ADDITIONAL_AGENT_SOURCE_ROOTS_ENV: &str = "ADDITIONAL_AGENT_SOURCE_ROOTS";
const BUNDLED_AGENT_ROOT_DIR: &str = "builtin-sources/agents";

// Cleanup is layered because no single mechanism survives every parent
// failure mode: Drop covers graceful exit, RunEvent::ExitRequested covers
// pre-runtime-teardown, the PID registry directory + name-based scan in
// cleanup_stale_serves recover orphans on next launch (Drop can't run after
// SIGKILL), and on Linux PR_SET_PDEATHSIG reaps the child when the parent
// dies. macOS has no PDEATHSIG equivalent, so the registry + scan carry the
// post-crash recovery there.

/// A long-lived `goose serve` process that accepts WebSocket connections.
///
/// Each WebSocket connection to the `/acp` endpoint creates an independent
/// ACP agent inside the server, so a single process can serve any number of
/// concurrent sessions.
pub struct GooseServeProcess {
    port: u16,
    secret_key: String,
    child: Mutex<Option<Child>>,
    pid_entry: PathBuf,
}

/// Tauri-managed wrapper that lazily initializes the singleton serve process.
pub struct GooseServeHandle {
    cell: OnceCell<GooseServeProcess>,
}

impl GooseServeHandle {
    pub fn new() -> Self {
        Self {
            cell: OnceCell::new(),
        }
    }

    pub async fn get(&self, app_handle: tauri::AppHandle) -> Result<&GooseServeProcess, String> {
        self.cell
            .get_or_try_init(|| GooseServeProcess::spawn(app_handle))
            .await
    }

    /// Kill the child if it has been spawned. Safe to call multiple times.
    pub fn shutdown(&self) {
        if let Some(process) = self.cell.get() {
            process.kill();
        }
    }
}

impl Default for GooseServeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl GooseServeProcess {
    /// Return the WebSocket URL for connecting to this server.
    pub fn ws_url(&self) -> String {
        format!("ws://{LOCALHOST}:{}/acp", self.port)
    }

    /// Return the HTTP base URL for authenticated Goose server routes.
    pub fn http_base_url(&self) -> String {
        format!("http://{LOCALHOST}:{}", self.port)
    }

    /// Return the secret key used to authenticate local HTTP requests.
    pub fn secret_key(&self) -> &str {
        &self.secret_key
    }

    /// Kill the child and remove our PID registry entry. Idempotent.
    pub fn kill(&self) {
        match self.child.lock() {
            Ok(mut guard) => {
                if let Some(mut child) = guard.take() {
                    let _ = child.start_kill();
                }
            }
            Err(_) => log::warn!("goose serve child mutex poisoned; child may leak"),
        }
        let _ = std::fs::remove_file(&self.pid_entry);
    }

    async fn spawn(app_handle: tauri::AppHandle) -> Result<GooseServeProcess, String> {
        let registry_dir = resolve_pid_registry_dir(&app_handle)?;
        std::fs::create_dir_all(&registry_dir).map_err(|e| {
            format!(
                "Failed to create goose serve state directory {}: {e}",
                registry_dir.display()
            )
        })?;
        cleanup_stale_serves(&registry_dir);

        let port = reserve_free_port()?;
        let secret_key = format!("goose2-{}", uuid::Uuid::new_v4().simple());

        let working_dir = default_serve_working_dir();
        std::fs::create_dir_all(&working_dir).map_err(|e| {
            format!(
                "Failed to create goose serve working directory {}: {e}",
                working_dir.display()
            )
        })?;

        let mut command: Command = get_goose_command(&app_handle)?;
        let binary_display = command.as_std().get_program().to_string_lossy().to_string();

        if let Some(distro_state) = app_handle.try_state::<DistroBundleState>() {
            if let Some(bundle) = distro_state.bundle() {
                if let Some(bin_dir) = &bundle.bin_dir {
                    prepend_path_env(&mut command, bin_dir);
                }
                if let Some(config_path) = &bundle.config_path {
                    append_additional_config_env(&mut command, config_path);
                }
                command.env("GOOSE_DISTRO_DIR", &bundle.root_dir);
            }
        }

        command.arg("serve");
        add_bundled_agent_root_env(&app_handle, &mut command);

        command
            .arg("--host")
            .arg(LOCALHOST)
            .arg("--port")
            .arg(port.to_string())
            .current_dir(&working_dir)
            .env("GOOSE_SERVER__SECRET_KEY", &secret_key)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        #[cfg(target_os = "linux")]
        configure_parent_death_signal(&mut command);

        log::info!(
            "Spawning long-lived goose serve: binary={binary_display} port={port} cwd={}",
            working_dir.display(),
        );

        let mut child = command.spawn().map_err(|error| {
            format!(
                "Failed to spawn goose serve (binary: {binary_display}, cwd: {}): {error}",
                working_dir.display()
            )
        })?;

        let pid_entry = match child.id() {
            Some(pid) => {
                let entry = registry_dir.join(format!("{pid}.pid"));
                if let Err(e) = std::fs::write(&entry, pid.to_string()) {
                    log::warn!("Failed to write goose serve pid registry entry: {e}");
                }
                entry
            }
            None => registry_dir.join("unknown.pid"),
        };

        wait_for_server_ready(port, &mut child).await?;

        log::info!("Goose serve is ready on port {port}");

        Ok(GooseServeProcess {
            port,
            secret_key,
            child: Mutex::new(Some(child)),
            pid_entry,
        })
    }
}

impl Drop for GooseServeProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

fn add_bundled_agent_root_env<R: Runtime>(manager: &impl Manager<R>, command: &mut Command) {
    let resource_dir = match manager.path().resource_dir() {
        Ok(path) => path,
        Err(error) => {
            log::warn!("Failed to resolve Tauri resource dir for bundled sources: {error}");
            return;
        }
    };

    let root = resource_dir.join(BUNDLED_AGENT_ROOT_DIR);
    if !root.is_dir() {
        log::debug!(
            "No bundled source root found at {}; skipping",
            root.display()
        );
        return;
    }

    append_additional_agent_roots_env(command, &root);
}

fn append_additional_agent_roots_env(command: &mut Command, root: &std::path::Path) {
    let existing = std::env::var_os(ADDITIONAL_AGENT_SOURCE_ROOTS_ENV);
    let mut roots: Vec<PathBuf> = existing
        .as_ref()
        .map(std::env::split_paths)
        .map(Iterator::collect)
        .unwrap_or_default();
    roots.push(root.to_path_buf());

    match std::env::join_paths(&roots) {
        Ok(joined) => {
            command.env(ADDITIONAL_AGENT_SOURCE_ROOTS_ENV, joined);
        }
        Err(error) => {
            eprintln!("Failed to set {ADDITIONAL_AGENT_SOURCE_ROOTS_ENV}: {error}");
        }
    }
}

pub fn get_goose_command(app_handle: &tauri::AppHandle) -> Result<Command, String> {
    if let Ok(override_path) = std::env::var("GOOSE_BIN") {
        Ok(Command::new(override_path))
    } else {
        let tauri_command = app_handle
            .shell()
            .sidecar("goose")
            .map_err(|e| format!("could not resolve goose binary: {e}"))?;
        let std_command: std::process::Command = tauri_command.into();
        Ok(std_command.into())
    }
}

async fn wait_for_server_ready(port: u16, child: &mut Child) -> Result<(), String> {
    let deadline = Instant::now() + GOOSE_SERVE_CONNECT_TIMEOUT;
    let addr = format!("{LOCALHOST}:{port}");

    loop {
        match tokio::net::TcpStream::connect(&addr).await {
            Ok(_) => return Ok(()),
            Err(_) => {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|e| format!("Failed to poll goose serve process: {e}"))?
                {
                    return Err(format!(
                        "Goose serve exited before becoming ready: {status}"
                    ));
                }

                if Instant::now() >= deadline {
                    return Err(format!("Timed out waiting for goose serve on port {port}"));
                }

                tokio::time::sleep(GOOSE_SERVE_CONNECT_RETRY_DELAY).await;
            }
        }
    }
}

fn default_serve_working_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn resolve_pid_registry_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Ok(override_dir) = std::env::var(STATE_DIR_ENV) {
        return Ok(PathBuf::from(override_dir).join(PID_REGISTRY_DIR_NAME));
    }
    app_handle
        .path()
        .app_local_data_dir()
        .map(|dir| dir.join(PID_REGISTRY_DIR_NAME))
        .map_err(|e| format!("Failed to resolve app local data dir: {e}"))
}

fn cleanup_stale_serves(registry_dir: &Path) {
    cleanup_stale_serves_with(registry_dir, &running_goose_processes());
}

/// Kill registry-tracked orphans + name-matched untracked orphans using a
/// pre-built snapshot of running goose processes. One `ps -A` per startup
/// instead of one `ps -p` per registry entry (was 42+ subprocess spawns on
/// the dev machine after this cycle of accumulated crashes). Split from
/// `cleanup_stale_serves` so unit tests can inject a controlled snapshot
/// without scanning the real process table on the test machine.
fn cleanup_stale_serves_with(registry_dir: &Path, goose_procs: &HashMap<i32, String>) {
    if let Ok(entries) = std::fs::read_dir(registry_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("pid") {
                continue;
            }
            let pid_opt = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<i32>().ok());
            if let Some(pid) = pid_opt {
                if pid > 1 && goose_procs.contains_key(&pid) {
                    log::info!("Killing tracked orphan goose serve PID {pid}");
                    kill_pid(pid);
                }
            }
            let _ = std::fs::remove_file(&path);
        }
    }

    // Pre-fix orphans aren't in the registry — match by arg shape against the
    // same snapshot to catch them on first run. `host_arg` is built from the
    // LOCALHOST constant so the scan can't silently desync from the spawn
    // invocation if LOCALHOST ever changes.
    let host_arg = format!("--host {LOCALHOST}");
    let self_pid = std::process::id() as i32;
    for (pid, args) in goose_procs {
        if *pid <= 1 || *pid == self_pid {
            continue;
        }
        let has_serve = args.split_whitespace().any(|tok| tok == "serve");
        if !has_serve || !args.contains(&host_arg) {
            continue;
        }
        log::info!("Killing untracked orphan goose serve PID {pid}");
        kill_pid(*pid);
    }

    if let Some(parent) = registry_dir.parent() {
        let _ = std::fs::remove_file(parent.join(LEGACY_PID_FILE_NAME));
    }
}

fn kill_pid(pid: i32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
    }
}

/// Snapshot of currently-running processes whose executable basename is
/// exactly `goose` (or `goose.exe` on Windows). Returns PID → args. Used by
/// `cleanup_stale_serves` to validate registry entries and to scan for
/// untracked orphans in one pass; strict basename matching avoids hitting
/// unrelated processes whose name happens to start with "goose".
#[cfg(unix)]
fn running_goose_processes() -> HashMap<i32, String> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,comm=,args="])
        .output()
    else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = HashMap::new();
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        let mut parts = trimmed.splitn(3, char::is_whitespace);
        let Some(pid_str) = parts.next() else {
            continue;
        };
        let Some(comm) = parts.next() else {
            continue;
        };
        let Some(args) = parts.next() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue;
        };
        let comm_basename = comm.rsplit('/').next().unwrap_or("");
        if comm_basename == "goose" {
            out.insert(pid, args.to_string());
        }
    }
    out
}

#[cfg(windows)]
fn running_goose_processes() -> HashMap<i32, String> {
    // PID registry already catches every new orphan on Windows, and Windows
    // doesn't reparent orphans to a long-lived launchd-like process, so legacy
    // zombies don't accumulate the way they do on macOS. Worth wiring up via
    // `wmic process get` if that turns out to be wrong.
    HashMap::new()
}

#[cfg(target_os = "linux")]
fn configure_parent_death_signal(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent_pid = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // If the parent died between fork and prctl, fail exec so we don't orphan.
            if libc::getppid() != parent_pid {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

fn prepend_path_env(command: &mut Command, extra_dir: &std::path::Path) {
    let mut paths = vec![extra_dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }

    set_path_list_env(command, "PATH", paths, Some(extra_dir.as_os_str()));
}

fn append_additional_config_env(command: &mut Command, config_path: &std::path::Path) {
    let existing = std::env::var_os("GOOSE_ADDITIONAL_CONFIG_FILES");
    let mut paths: Vec<PathBuf> = existing
        .as_ref()
        .map(std::env::split_paths)
        .map(Iterator::collect)
        .unwrap_or_default();
    paths.push(config_path.to_path_buf());

    if let Ok(joined) = std::env::join_paths(&paths) {
        command.env("GOOSE_ADDITIONAL_CONFIG_FILES", joined);
    } else {
        let mut fallback = existing.unwrap_or_default();
        if !fallback.is_empty() {
            fallback.push(if cfg!(windows) { ";" } else { ":" });
        }
        fallback.push(config_path.as_os_str());
        command.env("GOOSE_ADDITIONAL_CONFIG_FILES", fallback);
    }
}

fn set_path_list_env(
    command: &mut Command,
    key: &str,
    paths: Vec<PathBuf>,
    fallback_prefix: Option<&std::ffi::OsStr>,
) {
    if let Ok(joined) = std::env::join_paths(&paths) {
        command.env(key, joined);
    } else if let Some(prefix) = fallback_prefix {
        let mut fallback = OsString::from(prefix);
        for path in paths.iter().skip(1) {
            fallback.push(if cfg!(windows) { ";" } else { ":" });
            fallback.push(path.as_os_str());
        }
        command.env(key, fallback);
    }
}

fn reserve_free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind((LOCALHOST, 0))
        .map_err(|error| format!("Failed to reserve Goose serve port: {error}"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("Failed to resolve reserved Goose serve port: {error}"))
}

// Tests cover the Drop -> kill chain and cleanup_stale_serves safety
// (no kill of non-goose PIDs, malformed/missing entries, PID 1 guard,
// untracked-orphan scan against a synthetic snapshot). The full
// goose-tauri binary lifecycle isn't exercised: spawning the GUI binary
// needs a renderer to invoke the Tauri command, impractical in
// `cargo test`.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pid_alive(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    async fn wait_until<F: Fn() -> bool>(predicate: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        predicate()
    }

    fn spawn_long_sleep() -> Child {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").kill_on_drop(true);
        cmd.spawn().expect("spawn sleep")
    }

    fn write_pid_entry(dir: &Path, pid: i32) -> PathBuf {
        let entry = dir.join(format!("{pid}.pid"));
        std::fs::write(&entry, pid.to_string()).unwrap();
        entry
    }

    #[tokio::test]
    async fn drop_kills_child_and_removes_registry_entry() {
        let tmp = TempDir::new().unwrap();

        let child = spawn_long_sleep();
        let pid = child.id().expect("child pid") as i32;
        let pid_entry = write_pid_entry(tmp.path(), pid);
        assert!(pid_alive(pid));
        assert!(pid_entry.exists());

        let process = GooseServeProcess {
            port: 0,
            secret_key: "test".to_string(),
            child: Mutex::new(Some(child)),
            pid_entry: pid_entry.clone(),
        };

        drop(process);

        assert!(
            wait_until(|| !pid_alive(pid), Duration::from_secs(2)).await,
            "child should be dead after drop"
        );
        assert!(!pid_entry.exists(), "pid registry entry should be removed");
    }

    #[tokio::test]
    async fn kill_is_idempotent() {
        let tmp = TempDir::new().unwrap();

        let child = spawn_long_sleep();
        let pid = child.id().expect("child pid") as i32;
        let pid_entry = write_pid_entry(tmp.path(), pid);

        let process = GooseServeProcess {
            port: 0,
            secret_key: "test".to_string(),
            child: Mutex::new(Some(child)),
            pid_entry: pid_entry.clone(),
        };

        process.kill();
        process.kill();

        assert!(wait_until(|| !pid_alive(pid), Duration::from_secs(2)).await);
        assert!(!pid_entry.exists());
    }

    #[tokio::test]
    async fn cleanup_stale_serves_does_not_kill_non_goose_pid() {
        let tmp = TempDir::new().unwrap();

        let mut child = spawn_long_sleep();
        let pid = child.id().expect("child pid") as i32;
        let pid_entry = write_pid_entry(tmp.path(), pid);
        assert!(pid_alive(pid));

        // Empty snapshot — pid isn't recognized as goose, must not be killed.
        cleanup_stale_serves_with(tmp.path(), &HashMap::new());

        assert!(pid_alive(pid), "non-goose pid must not be killed");
        assert!(!pid_entry.exists(), "stale registry entry must be removed");

        let _ = child.kill().await;
    }

    #[test]
    fn cleanup_stale_serves_handles_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does-not-exist");
        cleanup_stale_serves_with(&missing, &HashMap::new());
        assert!(!missing.exists());
    }

    #[test]
    fn cleanup_stale_serves_handles_malformed_filename() {
        let tmp = TempDir::new().unwrap();
        let bad = tmp.path().join("not-a-pid.pid");
        std::fs::write(&bad, "ignored").unwrap();
        cleanup_stale_serves_with(tmp.path(), &HashMap::new());
        assert!(!bad.exists(), "malformed entry must be removed");
    }

    #[test]
    fn cleanup_stale_serves_rejects_pid_one() {
        let tmp = TempDir::new().unwrap();
        let pid_entry = write_pid_entry(tmp.path(), 1);
        // Even if the snapshot claimed pid 1 was a goose process, the
        // guard must skip kill_pid(1).
        let mut snap = HashMap::new();
        snap.insert(1i32, String::new());
        cleanup_stale_serves_with(tmp.path(), &snap);
        assert!(!pid_entry.exists(), "guard against PID 1 must remove entry");
    }

    #[test]
    fn cleanup_stale_serves_skips_orphan_scan_self_pid() {
        // Build a snapshot containing the test process pretending to be a
        // goose-serve orphan. The self_pid guard must skip it; if the guard
        // breaks, this test would SIGKILL the test runner.
        let tmp = TempDir::new().unwrap();
        let self_pid = std::process::id() as i32;
        let mut snap = HashMap::new();
        snap.insert(
            self_pid,
            format!("/path/to/goose serve --host {LOCALHOST} --port 0"),
        );
        cleanup_stale_serves_with(tmp.path(), &snap);
    }

    #[test]
    fn pid_registry_entry_round_trip() {
        let tmp = TempDir::new().unwrap();
        let entry = write_pid_entry(tmp.path(), 42);
        let read = std::fs::read_to_string(&entry).unwrap();
        assert_eq!(read.trim(), "42");
    }
}
