//! Memory use of a running session's process tree (build plan, 7.4),
//! only sampled while the "Show memory use" setting is on; and, for a
//! session already open in another process, which app that process runs
//! in, and stopping it.

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use sysinfo::{
    Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, RefreshKind, Signal, System, UpdateKind,
};

/// Samples process memory. Keep one and reuse it.
pub struct MemorySampler {
    system: System,
}

impl Default for MemorySampler {
    fn default() -> Self {
        Self::new()
    }
}

impl MemorySampler {
    pub fn new() -> Self {
        Self {
            system: System::new_with_specifics(RefreshKind::nothing()),
        }
    }

    /// Resident memory in bytes of each root process plus all its
    /// descendants, in the order given. Unknown pids give 0.
    pub fn tree_rss(&mut self, roots: &[u32]) -> Vec<u64> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        let processes = self.system.processes();
        roots
            .iter()
            .map(|root| {
                let root = Pid::from_u32(*root);
                processes
                    .iter()
                    .filter(|(pid, _)| is_descendant(processes, **pid, root))
                    .map(|(_, p)| p.memory())
                    .sum()
            })
            .collect()
    }
}

fn is_descendant(
    processes: &std::collections::HashMap<Pid, sysinfo::Process>,
    mut pid: Pid,
    root: Pid,
) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        match processes.get(&pid).and_then(|p| p.parent()) {
            Some(parent) => pid = parent,
            None => return false,
        }
    }
    false
}

/// What a process runs inside, found by walking up its parents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    /// This app started it (a session process it no longer shows).
    ThisApp,
    /// A macOS app: Terminal, iTerm, Visual Studio Code, ….
    App(String),
    /// Another program with no app around it: `tmux`, `sshd`, ….
    Program(String),
    /// Nothing but the system above it: started in the background, or the
    /// window it ran in was closed and it kept running.
    Detached,
}

/// Shells and runtimes between a `claude` process and what it runs in.
const PASS_THROUGH: &[&str] = &[
    "zsh", "bash", "sh", "dash", "fish", "tcsh", "csh", "ksh", "nu", "login", "node", "bun",
    "claude", "env", "sudo", "su", "script", "nohup", "timeout", "npx", "npm",
];

/// The app or program `pid` runs in. `None`: no such process (it ended).
pub fn host_of(pid: u32) -> Option<Host> {
    let mut system = System::new_with_specifics(RefreshKind::nothing());
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
    );
    let processes = system.processes();
    let me = Pid::from_u32(std::process::id());
    let mut current = processes
        .get(&Pid::from_u32(pid))
        // A zombie has ended (its parent hasn't collected it yet).
        .filter(|p| p.status() != ProcessStatus::Zombie)?;
    let mut program: Option<String> = None;
    for _ in 0..64 {
        let Some(parent) = current.parent() else { break };
        if parent == me {
            return Some(Host::ThisApp);
        }
        let Some(p) = processes.get(&parent) else { break };
        if let Some(app) = p.exe().and_then(app_name) {
            return Some(Host::App(app));
        }
        let name = process_name(p.name(), p.exe());
        if program.is_none() && !is_pass_through(&name) && parent.as_u32() > 1 && name != "launchd" {
            program = Some(name);
        }
        current = p;
    }
    Some(program.map_or(Host::Detached, Host::Program))
}

fn is_pass_through(name: &str) -> bool {
    let name = name.trim_start_matches('-');
    PASS_THROUGH.contains(&name)
}

fn process_name(name: &OsStr, exe: Option<&Path>) -> String {
    exe.and_then(Path::file_name)
        .unwrap_or(name)
        .to_string_lossy()
        .into_owned()
}

/// The outermost `Name.app` in an executable's path:
/// `/Applications/Visual Studio Code.app/Contents/Frameworks/Code
/// Helper.app/…` is "Visual Studio Code".
pub fn app_name(exe: &Path) -> Option<String> {
    exe.components().find_map(|c| {
        let part = c.as_os_str().to_str()?;
        let name = part.strip_suffix(".app")?;
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// Ask `pid` to quit (SIGTERM; as closing its terminal would) and wait up
/// to `timeout` for it to end. Blocking: call on a background thread.
pub fn stop_process(pid: u32, timeout: Duration) -> Result<()> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new_with_specifics(RefreshKind::nothing());
    let refresh = |system: &mut System| {
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing(),
        );
    };
    refresh(&mut system);
    let Some(process) = system.process(pid) else {
        return Ok(());
    };
    if process.kill_with(Signal::Term).is_none() && !process.kill() {
        bail!("could not signal process {pid}");
    }
    let deadline = Instant::now() + timeout;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        refresh(&mut system);
        // Ended, or ended and not yet reaped by its parent.
        if system
            .process(pid)
            .is_none_or(|p| p.status() == ProcessStatus::Zombie)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("process {pid} is still running");
        }
    }
}

/// "123 MB".
pub fn format_bytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    if bytes >= 1024 * MB {
        format!("{:.1} GB", bytes as f64 / (1024 * MB) as f64)
    } else {
        format!("{} MB", bytes.div_ceil(MB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_this_process() {
        let mut sampler = MemorySampler::new();
        let rss = sampler.tree_rss(&[std::process::id(), u32::MAX - 1]);
        assert!(rss[0] > 0);
        assert_eq!(rss[1], 0);
    }

    #[test]
    fn app_names_and_hosts() {
        assert_eq!(
            app_name(Path::new("/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal")),
            Some("Terminal".to_owned())
        );
        assert_eq!(
            app_name(Path::new(
                "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)"
            )),
            Some("Visual Studio Code".to_owned())
        );
        assert_eq!(app_name(Path::new("/usr/bin/tmux")), None);
        assert!(is_pass_through("-zsh") && is_pass_through("node") && !is_pass_through("tmux"));
        assert_eq!(host_of(u32::MAX - 1), None);
    }

    #[test]
    fn stops_a_child_process() -> Result<()> {
        let mut child = std::process::Command::new("sleep").arg("30").spawn()?;
        // A child of this process: the test binary is "this app".
        assert_eq!(host_of(child.id()), Some(Host::ThisApp));
        stop_process(child.id(), Duration::from_secs(5))?;
        // Ended but not yet collected: not running.
        assert_eq!(host_of(child.id()), None);
        assert!(child.wait()?.code().is_none(), "ended by a signal");
        // Gone already: nothing to do.
        stop_process(child.id(), Duration::from_secs(1))?;
        Ok(())
    }

    #[test]
    fn formats() {
        assert_eq!(format_bytes(1), "1 MB");
        assert_eq!(format_bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
