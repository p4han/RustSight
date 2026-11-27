use crate::backend::{WindowCaptureBackend, WindowId, WindowInfo};
use crate::error::{Result, RustSightError};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub path_or_cargo_dir: String,
    pub args: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub struct TrackedProcess {
    child: Child,
    pid: u32,
    window_id: WindowId,
    #[cfg(windows)]
    _job: Option<WindowsJob>,
}

impl TrackedProcess {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for TrackedProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(windows)]
struct ScopedHandle(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for ScopedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Returns `true` if `target_pid` is in `tracked_pids` or is a descendant of any PID in `tracked_pids`.
pub fn is_pid_or_descendant_tracked(target_pid: u32, tracked_pids: &HashSet<u32>) -> bool {
    if tracked_pids.is_empty() || target_pid == 0 {
        return false;
    }
    if tracked_pids.contains(&target_pid) {
        return true;
    }

    #[cfg(windows)]
    {
        use std::mem::size_of;
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        };

        let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return false;
        };
        let _snap_guard = ScopedHandle(snap);

        let mut parent_map: HashMap<u32, u32> = HashMap::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        unsafe {
            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    parent_map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
        }

        let mut curr = target_pid;
        let mut visited = HashSet::new();
        for _ in 0..32 {
            if !visited.insert(curr) {
                break;
            }
            let Some(&parent) = parent_map.get(&curr) else {
                break;
            };
            if parent == 0 || parent == curr {
                break;
            }
            if tracked_pids.contains(&parent) {
                return true;
            }
            curr = parent;
        }
        false
    }

    #[cfg(not(windows))]
    {
        false
    }
}

/// Resolve the executable file name (e.g., `"notepad.exe"`) for `pid`.
/// Returns `None` if the process cannot be opened or queried.
pub fn process_exe_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }

    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        };
        use windows::core::PWSTR;

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
        let _guard = ScopedHandle(handle);

        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .ok()?;
        }

        let full_path = String::from_utf16_lossy(&buf[..len as usize]);
        Path::new(&full_path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }

    #[cfg(not(windows))]
    {
        let _ = pid;
        None
    }
}

#[cfg(windows)]
struct WindowsJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
unsafe impl Send for WindowsJob {}
#[cfg(windows)]
unsafe impl Sync for WindowsJob {}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn create_kill_on_close_job(child: &Child) -> Option<WindowsJob> {
    use std::ffi::c_void;
    use std::mem::size_of;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_err()
        {
            let _ = windows::Win32::Foundation::CloseHandle(job);
            return None;
        }
        let proc_handle = HANDLE(child.as_raw_handle());
        if AssignProcessToJobObject(job, proc_handle).is_err() {
            let _ = windows::Win32::Foundation::CloseHandle(job);
            return None;
        }
        Some(WindowsJob(job))
    }
}

pub struct PreparedLaunch {
    pub info: WindowInfo,
    pub spec: LaunchSpec,
    process: TrackedProcess,
}

pub struct ProcessManager {
    processes: HashMap<WindowId, TrackedProcess>,
    active_window: Option<WindowId>,
    last_spec: Option<LaunchSpec>,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            active_window: None,
            last_spec: None,
        }
    }

    #[allow(dead_code)]
    pub fn active_window_id(&self) -> Option<WindowId> {
        self.active_window
    }

    pub fn tracked_pids(&self) -> HashSet<u32> {
        self.processes.values().map(|p| p.pid).collect()
    }

    pub fn resolve_window_id(&self, window_id: Option<WindowId>) -> Result<WindowId> {
        match window_id.or(self.active_window) {
            Some(id) => Ok(id),
            None => Err(RustSightError::NoActiveWindow),
        }
    }

    /// Briefly register a completed [`PreparedLaunch`] under the manager lock.
    pub fn register_launched(&mut self, prepared: PreparedLaunch) -> WindowInfo {
        let wid = prepared.info.id;
        self.processes.insert(wid, prepared.process);
        self.active_window = Some(wid);
        self.last_spec = Some(prepared.spec);
        prepared.info
    }

    /// Clone `last_spec` and remove the active process so it can be killed outside the lock
    /// before rebuilding.
    pub fn take_rebuild_target(&mut self) -> Result<(LaunchSpec, Option<TrackedProcess>)> {
        let spec = self
            .last_spec
            .clone()
            .ok_or(RustSightError::NothingToRebuild)?;
        let old_proc = self
            .active_window
            .and_then(|wid| self.processes.remove(&wid));
        if old_proc.is_some() {
            self.active_window = self.processes.keys().next().copied();
        }
        Ok((spec, old_proc))
    }

    pub fn close(&mut self, window_id: Option<WindowId>) -> Result<WindowId> {
        let wid = self.resolve_window_id(window_id)?;
        if let Some(mut proc) = self.processes.remove(&wid) {
            info!("Closing window {} (pid {})", proc.window_id, proc.pid);
            proc.kill();
        }
        if self.active_window == Some(wid) {
            self.active_window = self.processes.keys().next().copied();
        }
        Ok(wid)
    }

    pub fn cleanup_all(&mut self) {
        for (_, mut proc) in self.processes.drain() {
            debug!("Cleaning up pid {}", proc.pid);
            proc.kill();
        }
        self.active_window = None;
    }
}

/// Build (if Cargo project), spawn the process, and wait for its main window without holding
/// any `ProcessManager` lock. If any step fails before returning, the local [`TrackedProcess`]
/// is dropped and automatically kills the spawned child process.
pub fn prepare_launch(
    spec: LaunchSpec,
    capture: &dyn WindowCaptureBackend,
    build_timeout: Duration,
    window_timeout: Duration,
) -> Result<PreparedLaunch> {
    prepare_launch_with(
        spec,
        capture,
        build_timeout,
        window_timeout,
        build_cargo_project,
    )
}

pub fn prepare_launch_with<F>(
    spec: LaunchSpec,
    capture: &dyn WindowCaptureBackend,
    build_timeout: Duration,
    window_timeout: Duration,
    builder: F,
) -> Result<PreparedLaunch>
where
    F: FnOnce(&Path, Duration) -> Result<PathBuf>,
{
    let target = Path::new(&spec.path_or_cargo_dir);
    let manifest = detect_cargo_manifest(target);
    let exe_path = if let Some(ref manifest_path) = manifest {
        info!("Building Cargo project at {}", manifest_path.display());
        builder(manifest_path, build_timeout)?
    } else {
        if !target.exists() {
            return Err(RustSightError::BinaryNotFound(
                spec.path_or_cargo_dir.clone(),
                "Path does not exist".to_string(),
            ));
        }
        target.to_path_buf()
    };

    info!("Spawning {}", exe_path.display());
    let working_dir = if let Some(ref manifest_path) = manifest {
        manifest_path.parent().map(|p| p.to_path_buf())
    } else {
        exe_path.parent().map(|p| p.to_path_buf())
    };

    let mut cmd = Command::new(&exe_path);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    let child = cmd
        .spawn()
        .map_err(|e| RustSightError::LaunchFailed(exe_path.display().to_string(), e.to_string()))?;
    let pid = child.id();

    #[cfg(windows)]
    let job = create_kill_on_close_job(&child);

    // Keep child + job in a local TrackedProcess so Drop cleans it up if anything fails below.
    let mut tracked = TrackedProcess {
        child,
        pid,
        window_id: 0,
        #[cfg(windows)]
        _job: job,
    };

    let timeout_ms = window_timeout.as_millis() as u64;
    let deadline = Instant::now() + window_timeout;
    let mut info = loop {
        if let Ok(Some(status)) = tracked.child.try_wait() {
            return Err(RustSightError::LaunchFailed(
                exe_path.display().to_string(),
                format!("Process exited immediately with status: {status}"),
            ));
        }
        if let Some(win) = capture.find_window_by_pid(pid)? {
            break win;
        }
        if Instant::now() >= deadline {
            return Err(RustSightError::WindowTimeout(timeout_ms, pid));
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    if let (Some(w), Some(h)) = (spec.width, spec.height) {
        if let Err(e) = capture.resize_window(info.id, w, h) {
            warn!("Failed to resize window {}: {e}", info.id);
        } else {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(updated) = capture.get_window_info(info.id) {
                info = updated;
            }
        }
    }

    // Give the GUI a brief moment to render its first frame
    std::thread::sleep(Duration::from_millis(150));

    tracked.window_id = info.id;
    Ok(PreparedLaunch {
        info,
        spec,
        process: tracked,
    })
}

impl Drop for ProcessManager {
    fn drop(&mut self) {
        self.cleanup_all();
    }
}

fn detect_cargo_manifest(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        let candidate = path.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
    } else if path.is_file() && path.file_name().is_some_and(|n| n == "Cargo.toml") {
        return Some(path.to_path_buf());
    }
    None
}

#[derive(Deserialize)]
struct CargoMessage {
    reason: String,
    #[serde(default)]
    executable: Option<String>,
    #[serde(default)]
    target: Option<CargoTarget>,
}

#[derive(Deserialize)]
struct CargoTarget {
    #[serde(default)]
    kind: Vec<String>,
}

fn build_cargo_project(manifest_path: &Path, timeout: Duration) -> Result<PathBuf> {
    use std::io::Read;

    let mut child = Command::new("cargo")
        .arg("build")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--message-format=json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut out) = stdout_pipe {
            let _ = out.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut err) = stderr_pipe {
            let _ = err.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RustSightError::BuildFailed {
                path: manifest_path.display().to_string(),
                code: None,
                output: format!(
                    "`cargo build` timed out after {} seconds",
                    timeout.as_secs()
                ),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let stdout_bytes = stdout_handle.join().unwrap_or_default();
    let stderr_bytes = stderr_handle.join().unwrap_or_default();

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
        return Err(RustSightError::BuildFailed {
            path: manifest_path.display().to_string(),
            code: status.code(),
            output: stderr,
        });
    }

    let stdout = String::from_utf8_lossy(&stdout_bytes);
    let mut found_exe: Option<PathBuf> = None;
    for line in stdout.lines() {
        if let Ok(msg) = serde_json::from_str::<CargoMessage>(line)
            && msg.reason == "compiler-artifact"
            && let Some(exe) = msg.executable
        {
            let is_bin = msg
                .target
                .as_ref()
                .is_some_and(|t| t.kind.iter().any(|k| k == "bin"));
            if is_bin {
                found_exe = Some(PathBuf::from(exe));
            }
        }
    }

    found_exe.ok_or_else(|| {
        RustSightError::BinaryNotFound(
            manifest_path.display().to_string(),
            "Cargo build succeeded, but no binary executable artifact was produced.".to_string(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CapturedFrame, WindowRect};
    use std::sync::{Arc, Mutex};

    struct DummyCapture;
    impl WindowCaptureBackend for DummyCapture {
        fn init(&self) -> Result<()> {
            Ok(())
        }
        fn find_window_by_pid(&self, _pid: u32) -> Result<Option<WindowInfo>> {
            Ok(None)
        }
        fn list_windows(&self) -> Result<Vec<WindowInfo>> {
            Ok(Vec::new())
        }
        fn get_window_info(&self, window_id: WindowId) -> Result<WindowInfo> {
            Ok(WindowInfo {
                id: window_id,
                pid: 0,
                title: String::new(),
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
                dpi_scale: 1.0,
                is_minimized: false,
            })
        }
        fn is_window_alive(&self, _window_id: WindowId) -> bool {
            false
        }
        fn capture_window(&self, _window_id: WindowId) -> Result<CapturedFrame> {
            Ok(CapturedFrame {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
            })
        }
        fn resize_window(&self, _window_id: WindowId, _w: u32, _h: u32) -> Result<()> {
            Ok(())
        }
        fn focus_window(&self, _window_id: WindowId) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn slow_build_does_not_block_process_manager_lock() {
        let pm = Arc::new(Mutex::new(ProcessManager::new()));
        let (build_started_tx, build_started_rx) = std::sync::mpsc::channel();
        let (allow_finish_tx, allow_finish_rx) = std::sync::mpsc::channel();

        let manifest_dir = std::env::current_dir().unwrap();
        let spec = LaunchSpec {
            path_or_cargo_dir: manifest_dir.display().to_string(),
            args: Vec::new(),
            width: None,
            height: None,
        };

        let handle = std::thread::spawn(move || {
            prepare_launch_with(
                spec,
                &DummyCapture,
                Duration::from_secs(5),
                Duration::from_secs(1),
                move |_manifest, _timeout| {
                    build_started_tx.send(()).unwrap();
                    allow_finish_rx.recv().unwrap();
                    Err(RustSightError::BuildFailed {
                        path: "simulated".into(),
                        code: Some(1),
                        output: "simulated failure".into(),
                    })
                },
            )
        });

        build_started_rx.recv().unwrap();

        // While the slow build is running, ProcessManager lock is immediately available.
        let mut guard = pm
            .try_lock()
            .expect("ProcessManager lock should not be held during build");
        assert!(guard.tracked_pids().is_empty());
        assert!(matches!(
            guard.resolve_window_id(None),
            Err(RustSightError::NoActiveWindow)
        ));
        assert!(matches!(guard.close(Some(123)), Ok(123)));
        drop(guard);

        allow_finish_tx.send(()).unwrap();
        let res = handle.join().unwrap();
        assert!(matches!(res, Err(RustSightError::BuildFailed { .. })));
    }

    #[test]
    fn failed_launch_leaves_no_tracked_process_or_active_window() {
        let pm = ProcessManager::new();
        let spec = LaunchSpec {
            path_or_cargo_dir: "nonexistent_rustsight_binary_12345.exe".to_string(),
            args: Vec::new(),
            width: None,
            height: None,
        };
        let res = prepare_launch(
            spec,
            &DummyCapture,
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        assert!(matches!(res, Err(RustSightError::BinaryNotFound(_, _))));
        assert!(pm.tracked_pids().is_empty());
        assert!(pm.active_window_id().is_none());
    }

    #[test]
    fn rebuild_without_previous_launch_returns_nothing_to_rebuild() {
        let mut pm = ProcessManager::new();
        let res = pm.take_rebuild_target();
        assert!(matches!(res, Err(RustSightError::NothingToRebuild)));
    }
}
