//! Leave room for discovery, ZIP I/O and recovery while retaining session locks.
use super::{Agent, ProtectionReason};

pub fn lock_capacity(agent: Agent, requested: usize) -> Result<usize, ProtectionReason> {
    if agent == Agent::Claude {
        // Claude uses presence records rather than retained native locks.
        return Ok(usize::MAX);
    }
    available(requested).map_err(|error| {
        ProtectionReason::Indeterminate(format!(
            "cannot reserve file descriptors for session locks: {error}"
        ))
    })
}

pub fn capacity_exceeded() -> ProtectionReason {
    ProtectionReason::Indeterminate(
        "not enough file descriptors to retain session locks and safely archive or remove files"
            .to_owned(),
    )
}

#[cfg(unix)]
fn available(requested: usize) -> std::io::Result<usize> {
    const IO_HEADROOM: usize = 128;

    #[cfg(any(target_os = "linux", target_os = "android"))]
    let directory = "/proc/self/fd";
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let directory = "/dev/fd";

    // Includes guards held for other providers, logging and index files. Counting
    // the directory's own descriptor too is intentionally conservative.
    let open =
        std::fs::read_dir(directory)?.try_fold(0usize, |count, entry| entry.map(|_| count + 1))?;
    let requested = open.saturating_add(requested).saturating_add(IO_HEADROOM);
    let requested = u64::try_from(requested).unwrap_or(u64::MAX);
    // Only raises this process's soft limit. The hard limit and system settings
    // stay unchanged; rlimit also respects macOS's kern.maxfilesperproc ceiling.
    let soft = match rlimit::increase_nofile_limit(requested) {
        Ok(soft) => soft,
        Err(error) => {
            tracing::debug!(%error, "could not increase the file descriptor soft limit");
            rlimit::Resource::NOFILE.get()?.0
        }
    };
    let soft = usize::try_from(soft).unwrap_or(usize::MAX);
    // Even under a small hard limit, leave at least half the descriptors for
    // ordinary I/O instead of letting guards consume the final usable slot.
    let headroom = (soft / 2).clamp(16, IO_HEADROOM);
    Ok(soft.saturating_sub(open).saturating_sub(headroom))
}

#[cfg(not(unix))]
fn available(_requested: usize) -> std::io::Result<usize> {
    // Rust's native Windows handles are not subject to RLIMIT_NOFILE or the
    // C stdio descriptor limit. Native lock/open failures remain protective.
    Ok(usize::MAX)
}
