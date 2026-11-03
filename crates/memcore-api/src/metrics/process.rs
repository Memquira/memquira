//! Process resident memory for the `memcore_process_resident_memory_bytes` gauge, read
//! at scrape time: `/proc/self/status` on Linux, `GetProcessMemoryInfo` on Windows.

/// Resident set size in bytes, when the platform exposes it.
pub fn resident_memory_bytes() -> Option<u64> {
    imp::resident_memory_bytes()
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn resident_memory_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kib * 1024)
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn resident_memory_bytes() -> Option<u64> {
        let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        // SAFETY: the pseudo handle from GetCurrentProcess needs no closing, and `counters` is a
        // properly sized, writable PROCESS_MEMORY_COUNTERS.
        let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        (ok != 0).then_some(counters.WorkingSetSize as u64)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub fn resident_memory_bytes() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resident_memory_is_reported_on_supported_platforms() {
        if cfg!(any(target_os = "linux", windows)) {
            let bytes = super::resident_memory_bytes().expect("rss");
            assert!(bytes > 1024 * 1024, "{bytes}");
        }
    }
}
