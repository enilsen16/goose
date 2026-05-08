use tauri::{Manager, Runtime};
use tauri_plugin_shell::ShellExt;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::services::distro_bundle::DistroBundleState;

use tokio::process::{Child, Command};
use tokio::sync::OnceCell;

const GOOSE_SERVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const GOOSE_SERVE_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);
const PID_FILE_NAME: &str = "goose2-serve.pid";
const STATE_DIR_ENV: &str = "GOOSE2_SERVE_STATE_DIR";
const LOCALHOST: &str = "127.0.0.1";
const ADDITIONAL_AGENT_SOURCE_ROOTS_ENV: &str = "ADDITIONAL_AGENT_SOURCE_ROOTS";
const BUNDLED_AGENT_ROOT_DIR: &str = "builtin-sources/agents";

// Four redundant cleanup layers because no single one survives every failure
// mode of the parent: managed-state Drop covers graceful exit, the
// RunEvent::ExitRequested handler runs before Tauri tears down the runtime,
// the PID file recovers from SIGKILL of the parent on next launch, and on
// Linux PR_SET_PDEATHSIG kills the child immediately when the parent dies.
// macOS has no PDEATHSIG equivalent, which is why the PID file is needed.

/// A long-lived `goose serve` process that accepts WebSocket connections.
///
/// Each WebSocket connection to the `/acp` endpoint creates an independent
/// ACP agent inside the server, so a single process can serve any number of
/// concurrent sessions.
pub struct GooseServeProcess {
    port: u16,
    secret_key: String,
    child: Mutex<Option<Child>>,
    pid_file: PathBuf,
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

    /// Kill the child and remove the PID file. Idempotent.
    pub fn kill(&self) {
        match self.child.lock() {
            Ok(mut guard) => {
                if let Some(mut child) = guard.take() {
                    let _ = child.start_kill();
                }
            }
            Err(_) => log::warn!("goose serve child mutex poisoned; child may leak"),
        }
        let _ = std::fs::remove_file(&self.pid_file);
    }

    async fn spawn(app_handle: tauri::AppHandle) -> Result<GooseServeProcess, String> {
        let pid_file = resolve_pid_file(&app_handle)?;
        if let Some(parent) = pid_file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Failed to create goose serve state directory {}: {e}",
                    parent.display()
                )
            })?;
        }
        cleanup_stale_serve(&pid_file);

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

        if let Some(pid) = child.id() {
            if let Err(e) = write_pid_file(&pid_file, pid) {
                log::warn!("Failed to write goose serve pid file: {e}");
            }
        }

        wait_for_server_ready(port, &mut child).await?;

        log::info!("Goose serve is ready on port {port}");

        Ok(GooseServeProcess {
            port,
            secret_key,
            child: Mutex::new(Some(child)),
            pid_file,
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

fn resolve_pid_file(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Ok(override_dir) = std::env::var(STATE_DIR_ENV) {
        return Ok(PathBuf::from(override_dir).join(PID_FILE_NAME));
    }
    app_handle
        .path()
        .app_local_data_dir()
        .map(|dir| dir.join(PID_FILE_NAME))
        .map_err(|e| format!("Failed to resolve app local data dir: {e}"))
}

fn cleanup_stale_serve(pid_file: &Path) {
    let Ok(content) = std::fs::read_to_string(pid_file) else {
        return;
    };
    let Ok(pid) = content.trim().parse::<i32>() else {
        let _ = std::fs::remove_file(pid_file);
        return;
    };

    if pid <= 1 {
        let _ = std::fs::remove_file(pid_file);
        return;
    }

    if !is_goose_process(pid) {
        let _ = std::fs::remove_file(pid_file);
        return;
    }

    log::info!("Cleaning up orphaned goose serve PID {pid} from prior launch");

    // Orphan has no live parent and no graceful-shutdown semantics worth waiting
    // for; SIGKILL/taskkill /F directly so spawn isn't blocked on a grace period.
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

    let _ = std::fs::remove_file(pid_file);
}

/// True if `pid` belongs to a process whose executable basename is exactly
/// `goose` (or `goose.exe` on Windows). Strict matching is intentional —
/// PID reuse can land us on an unrelated process, so a substring match
/// like "goose" matching "goose2_lib" or "goose-mascot-screensaver" would
/// be unsafe.
#[cfg(unix)]
fn is_goose_process(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let comm = String::from_utf8_lossy(&out.stdout);
            let basename = comm.trim().rsplit('/').next().unwrap_or("");
            basename == "goose"
        }
        _ => false,
    }
}

#[cfg(windows)]
fn is_goose_process(pid: i32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let line = String::from_utf8_lossy(&out.stdout);
            // First CSV field is "Image Name", quoted. Match exact basename.
            line.split(',')
                .next()
                .map(|name| {
                    name.trim()
                        .trim_matches('"')
                        .eq_ignore_ascii_case("goose.exe")
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

fn write_pid_file(pid_file: &Path, pid: u32) -> Result<(), String> {
    std::fs::write(pid_file, pid.to_string())
        .map_err(|e| format!("Failed to write pid file {}: {e}", pid_file.display()))
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

// Tests cover the Drop -> kill chain and cleanup_stale_serve safety
// (no kill of non-goose PIDs, malformed/missing files, PID 1 guard).
// The full goose-tauri binary lifecycle isn't exercised: spawning the GUI
// binary needs a renderer to invoke the Tauri command, impractical in
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

    #[tokio::test]
    async fn drop_kills_child_and_removes_pid_file() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("test.pid");

        let child = spawn_long_sleep();
        let pid = child.id().expect("child pid") as i32;
        write_pid_file(&pid_file, pid as u32).unwrap();
        assert!(pid_alive(pid));
        assert!(pid_file.exists());

        let process = GooseServeProcess {
            port: 0,
            secret_key: "test".to_string(),
            child: Mutex::new(Some(child)),
            pid_file: pid_file.clone(),
        };

        drop(process);

        assert!(
            wait_until(|| !pid_alive(pid), Duration::from_secs(2)).await,
            "child should be dead after drop"
        );
        assert!(!pid_file.exists(), "pid file should be removed");
    }

    #[tokio::test]
    async fn kill_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("test.pid");

        let child = spawn_long_sleep();
        let pid = child.id().expect("child pid") as i32;
        write_pid_file(&pid_file, pid as u32).unwrap();

        let process = GooseServeProcess {
            port: 0,
            secret_key: "test".to_string(),
            child: Mutex::new(Some(child)),
            pid_file: pid_file.clone(),
        };

        process.kill();
        process.kill();

        assert!(wait_until(|| !pid_alive(pid), Duration::from_secs(2)).await);
        assert!(!pid_file.exists());
    }

    #[tokio::test]
    async fn cleanup_stale_serve_does_not_kill_non_goose_pid() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("stale.pid");

        let mut child = spawn_long_sleep();
        let pid = child.id().expect("child pid") as i32;
        std::fs::write(&pid_file, pid.to_string()).unwrap();
        assert!(pid_alive(pid));

        cleanup_stale_serve(&pid_file);

        assert!(pid_alive(pid), "non-goose pid must not be killed");
        assert!(!pid_file.exists(), "stale pid file must be removed");

        let _ = child.kill().await;
    }

    #[test]
    fn cleanup_stale_serve_handles_missing_file() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("missing.pid");
        cleanup_stale_serve(&pid_file);
        assert!(!pid_file.exists());
    }

    #[test]
    fn cleanup_stale_serve_handles_malformed_file() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("garbage.pid");
        std::fs::write(&pid_file, "not-a-pid").unwrap();
        cleanup_stale_serve(&pid_file);
        assert!(!pid_file.exists(), "malformed pid file must be removed");
    }

    #[test]
    fn cleanup_stale_serve_rejects_pid_one() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("init.pid");
        std::fs::write(&pid_file, "1").unwrap();
        cleanup_stale_serve(&pid_file);
        assert!(!pid_file.exists(), "guard against PID 1 must remove file");
    }

    #[test]
    fn is_goose_process_negative_for_self() {
        let self_pid = std::process::id() as i32;
        assert!(!is_goose_process(self_pid));
    }

    #[test]
    fn write_pid_file_round_trip() {
        let tmp = TempDir::new().unwrap();
        let pid_file = tmp.path().join("rt.pid");
        write_pid_file(&pid_file, 42).unwrap();
        let read = std::fs::read_to_string(&pid_file).unwrap();
        assert_eq!(read.trim(), "42");
    }
}
