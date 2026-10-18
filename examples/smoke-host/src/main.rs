//! A stand-in game for `scripts/smoke.sh`.
//!
//! It imports `GetFileVersionInfoSizeW` from `version.dll`, so the OS loader maps whatever
//! `version.dll` sits next to it at startup, exactly as a game's imports do. With uniloader
//! copied there under that name, the call must still reach the system DLL, and the loader must
//! have started the plugins by the time the process has run for a moment.
//!
//! Prints `forwarded <size>` and exits 0 when the forwarded call answers, 1 otherwise.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetFileVersionInfoSizeW;

    let exe = std::env::current_exe().unwrap_or_default();
    // Any file with a version resource would do; the system's own version.dll has one.
    let target = std::env::var("SystemRoot")
        .map(|root| format!("{root}\\System32\\version.dll"))
        .unwrap_or_default();
    #[allow(unsafe_code)]
    // SAFETY: a valid wide string and no handle out-parameter.
    let size = unsafe { GetFileVersionInfoSizeW(&HSTRING::from(target.as_str()), None) };
    println!("host {}", exe.display());
    // Long enough for the loader thread to start the plugins and write the log.
    std::thread::sleep(std::time::Duration::from_secs(3));
    if size == 0 {
        println!("not forwarded");
        return std::process::ExitCode::FAILURE;
    }
    println!("forwarded {size}");
    std::process::ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn main() {
    eprintln!("smoke-host only does something on Windows");
}
