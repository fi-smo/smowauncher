//! Working-set trimming and memory stats.

use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::GetCurrentProcess;

/// Hands unused pages back to Windows. Cheap; pages fault back in when touched.
pub fn trim() {
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

/// (working set, private bytes) in bytes.
pub fn usage() -> (usize, usize) {
    unsafe {
        let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
        let ok = GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
        if ok.is_err() {
            return (0, 0);
        }
        (c.WorkingSetSize, c.PrivateUsage)
    }
}

pub fn usage_string() -> String {
    let (ws, private) = usage();
    format!("working set {:.1} MB, private {:.1} MB", ws as f64 / 1048576.0, private as f64 / 1048576.0)
}
