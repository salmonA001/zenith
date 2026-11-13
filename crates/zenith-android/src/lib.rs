//! The game as an Android app. Android starts a NativeActivity, which loads this library and calls
//! `android_main`; that hands the activity to the game. On any other system this crate is empty.
#![cfg(target_os = "android")]

use zenith_client::app::{AndroidApp, Launch, run_android};

/// Android calls this, on a thread of its own, once the activity exists.
#[allow(unsafe_code)]
// SAFETY: the name is the one android-activity's glue looks for, and nothing else in the program
// defines it.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    logcat::forward_stderr();
    if let Some(home) = app.internal_data_path() {
        // SAFETY: nothing else in the game is running yet, so nothing reads the environment while
        // this writes to it.
        unsafe { std::env::set_var("ZENITH_HOME", home) };
    }
    if let Err(e) = run_android(app, Launch::default()) {
        eprintln!("zenith: {e}");
    }
}

/// Android throws away what a program writes to stderr. The game reports everything there, and a
/// panic does too, so it is piped into the system log instead, where `adb logcat` shows it.
#[allow(unsafe_code)]
mod logcat {
    use std::ffi::{CString, c_char, c_int};
    use std::io::{BufRead, BufReader};
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn dup2(old: c_int, new: c_int) -> c_int;
    }
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    const INFO: c_int = 4;
    const STDERR: c_int = 2;

    pub fn forward_stderr() {
        let Ok((reader, writer)) = std::io::pipe() else { return };
        // SAFETY: both descriptors are open and this process's own; this points stderr at the pipe.
        if unsafe { dup2(writer.as_raw_fd(), STDERR) } < 0 {
            return;
        }
        // Stderr holds the pipe open now; this handle to it is no longer needed.
        drop(writer);
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if let Ok(text) = CString::new(line) {
                    // SAFETY: both strings are null-terminated and outlive the call.
                    unsafe { __android_log_write(INFO, c"zenith".as_ptr(), text.as_ptr()) };
                }
            }
        });
    }
}
