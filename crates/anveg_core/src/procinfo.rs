//! Memory use of a running session's process tree (build plan, 7.4).
//! Only sampled while the "Show memory use" setting is on.

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

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
    fn formats() {
        assert_eq!(format_bytes(1), "1 MB");
        assert_eq!(format_bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
    }
}
