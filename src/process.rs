/// Check if a process is running by sending signal 0.
pub fn is_running(pid: i32) -> bool {
    // SAFETY: signal 0 doesn't actually send a signal, just checks if process exists.
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// libc FFI for kill(2)
#[cfg(unix)]
mod libc {
    unsafe extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}
