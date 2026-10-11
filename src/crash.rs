//! Crash reports: a panic in a game writes the panic message and a
//! symbolicated stack trace to a text file in the user data folder.

use std::backtrace::Backtrace;
use std::path::{Path, PathBuf};

/// Folder under [`crate::project::user_data_folder`] that holds crash
/// reports.
pub const CRASH_FOLDER: &str = "crashes";

/// Crash reports kept in [`CRASH_FOLDER`]; writing one more deletes the
/// oldest, so a game that crashes on every start cannot fill the disk.
pub const MAX_CRASH_REPORTS: usize = 20;

/// Installs a panic hook that writes a crash report, prints its path, and
/// then runs the hook that was installed before (which prints the panic as
/// usual). Panics caught with `catch_unwind` also write a report. Installing
/// it twice is harmless but writes two reports per panic.
pub fn install_crash_reporter() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload_as_str()
            .unwrap_or("panic with a non-text payload");
        let location = info.location().map_or_else(
            || "unknown".to_owned(),
            |at| format!("{}:{}:{}", at.file(), at.line(), at.column()),
        );
        let text = crash_report(
            message,
            &location,
            std::thread::current().name().unwrap_or("unnamed"),
            &Backtrace::force_capture(),
        );
        let folder = crate::project::user_data_folder().join(CRASH_FOLDER);
        match write_crash_report(&folder, &text) {
            Ok(path) => eprintln!("crash report written to {}", path.display()),
            Err(error) => eprintln!("could not write a crash report: {error}"),
        }
        previous(info);
    }));
}

/// The text of a crash report: engine version, platform, executable,
/// thread, panic message and location, then the stack trace. Function
/// names come from the executable's symbol table; file and line numbers
/// need debug info (`debug = "line-tables-only"` in the release profile).
#[must_use]
pub fn crash_report(
    message: &str,
    location: &str,
    thread: &str,
    backtrace: &Backtrace,
) -> String {
    let executable = std::env::current_exe()
        .map_or_else(|_| "unknown".to_owned(), |exe| exe.display().to_string());
    format!(
        "RustingEngine crash report\n\
         engine: {}\n\
         platform: {} {}\n\
         executable: {executable}\n\
         thread: {thread}\n\
         panic: {message}\n\
         at: {location}\n\n\
         stack trace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Writes `text` to a new `crash-<unix ms>.txt` in `folder`, creating the
/// folder, and deletes the oldest reports past [`MAX_CRASH_REPORTS`].
///
/// # Errors
/// Returns the I/O error when the folder or file cannot be written.
pub fn write_crash_report(
    folder: &Path,
    text: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    // Two panics in one millisecond (two threads) get separate files.
    let mut path = folder.join(format!("crash-{millis}.txt"));
    let mut n = 1;
    while path.exists() {
        path = folder.join(format!("crash-{millis}-{n}.txt"));
        n += 1;
    }
    std::fs::write(&path, text)?;
    let mut reports: Vec<(std::time::SystemTime, PathBuf)> =
        std::fs::read_dir(folder)?
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("crash-") && name.ends_with(".txt")
            })
            .filter_map(|entry| {
                Some((entry.metadata().ok()?.modified().ok()?, entry.path()))
            })
            .collect();
    reports.sort();
    let extra = reports.len().saturating_sub(MAX_CRASH_REPORTS);
    for (_, old) in reports.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_reports_name_the_panic_and_its_functions_and_stay_bounded() {
        let text = crash_report(
            "index out of bounds",
            "src/main.rs:4:9",
            "main",
            &Backtrace::force_capture(),
        );
        assert!(text.contains("panic: index out of bounds"), "{text}");
        assert!(text.contains("at: src/main.rs:4:9"), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        // Symbolicated: the trace names this test's own function.
        assert!(
            text.contains("crash_reports_name_the_panic_and_its_functions"),
            "{text}"
        );

        let folder = std::env::temp_dir()
            .join(format!("rusting-crash-{}", uuid::Uuid::new_v4()));
        let first = write_crash_report(&folder, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "first");
        std::fs::write(folder.join("save.json"), "{}").unwrap();
        for _ in 0..MAX_CRASH_REPORTS + 5 {
            write_crash_report(&folder, "again").unwrap();
        }
        let names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|entry| {
                entry.unwrap().file_name().to_string_lossy().into_owned()
            })
            .collect();
        assert_eq!(names.len(), MAX_CRASH_REPORTS + 1, "{names:?}");
        assert!(names.contains(&"save.json".to_owned()), "other files stay");
        let _ = std::fs::remove_dir_all(folder);
    }
}
