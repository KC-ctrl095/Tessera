// A test companion, not installed: the same native entry used by CLI/app.
fn main() {
    #[cfg(target_os = "linux")]
    if let Some(status) = tessera_core::sandbox::directory_launcher_entry() {
        std::process::exit(status);
    }
    std::process::exit(2);
}
