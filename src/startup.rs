#[cfg(target_os = "windows")]
use std::io;
#[cfg(target_os = "windows")]
use std::path::Path;
#[cfg(target_os = "windows")]
use winreg::{enums::HKEY_CURRENT_USER, RegKey};

#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const VALUE_NAME: &str = "Echo";
#[cfg(target_os = "windows")]
const LEGACY_VALUE_NAME: &str = "11th Echo";

/// Formats a command for the Windows `Run` registry key.
///
/// The executable is always quoted because an installed path commonly contains
/// spaces. Backslashes must not be inserted before those quotes: registry Run
/// values contain a command line, not a Rust/JSON string literal.
#[cfg(target_os = "windows")]
fn startup_command(executable: &Path) -> String {
    format!(r#""{}" --startup"#, executable.display())
}

/// Returns whether Echo is registered to run when the current user signs in.
#[cfg(target_os = "windows")]
pub fn is_enabled() -> io::Result<bool> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = match current_user.open_subkey(RUN_KEY) {
        Ok(key) => key,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    for value_name in [VALUE_NAME, LEGACY_VALUE_NAME] {
        match run_key.get_value::<String, _>(value_name) {
            Ok(_) => return Ok(true),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(false)
}

/// Enables or disables startup for the current user without requiring elevation.
#[cfg(target_os = "windows")]
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = current_user
        .create_subkey(RUN_KEY)
        .map_err(|err| err.to_string())?;

    if enabled {
        let executable = std::env::current_exe().map_err(|err| err.to_string())?;
        let command = startup_command(&executable);
        run_key
            .set_value(VALUE_NAME, &command)
            .map_err(|err| err.to_string())?;
        match run_key.delete_value(LEGACY_VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    } else {
        for value_name in [VALUE_NAME, LEGACY_VALUE_NAME] {
            match run_key.delete_value(value_name) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.to_string()),
            }
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use std::path::Path;

    use super::{startup_command, LEGACY_VALUE_NAME, RUN_KEY, VALUE_NAME};

    #[test]
    fn registry_names_are_stable() {
        assert_eq!(RUN_KEY, r"Software\Microsoft\Windows\CurrentVersion\Run");
        assert_eq!(VALUE_NAME, "Echo");
        assert_eq!(LEGACY_VALUE_NAME, "11th Echo");
    }

    #[test]
    fn startup_command_quotes_paths_with_spaces_without_escaping_the_quotes() {
        assert_eq!(
            startup_command(Path::new(r"C:\Users\Test User\Echo\Echo.exe")),
            r#""C:\Users\Test User\Echo\Echo.exe" --startup"#
        );
    }

    #[test]
    fn startup_command_quotes_paths_without_spaces_too() {
        assert_eq!(
            startup_command(Path::new(r"C:\Apps\echo.exe")),
            r#""C:\Apps\echo.exe" --startup"#
        );
    }
}
