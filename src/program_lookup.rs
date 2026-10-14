//! Locate external programs the way an interactive shell would.
//!
//! On Windows, `std::process::Command` only appends `.exe` when searching
//! `PATH` for a bare program name, so npm-style `.cmd` shims (for example
//! `cc-session-cwd.cmd`) are reported as not found even though they run fine
//! from a shell. [`command`] resolves bare names against `PATH` and `PATHEXT`
//! first on Windows and leaves every other platform untouched.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Extensions `CreateProcess` (via `Command`) can launch directly. Other
/// `PATHEXT` entries such as `.VBS` or `.JS` need an interpreter and are
/// skipped.
const LAUNCHABLE_EXTENSIONS: [&str; 4] = ["com", "exe", "bat", "cmd"];
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Build a `Command` for `program`, resolving bare names through `PATH` and
/// `PATHEXT` on Windows.
pub fn command(program: &str) -> Command {
    #[cfg(windows)]
    {
        let path = std::env::var_os("PATH");
        let pathext = std::env::var_os("PATHEXT");
        if let Some(resolved) = resolve_in(program, path.as_deref(), pathext.as_deref()) {
            return Command::new(resolved);
        }
    }
    Command::new(program)
}

/// Resolve a bare program name against the given `PATH` and `PATHEXT` values,
/// returning the first launchable match in `PATH` order.
///
/// Returns `None` for names that contain a directory component or already end
/// in a launchable extension; those are left to `Command`'s own lookup.
pub fn resolve_in(
    program: &str,
    path_var: Option<&OsStr>,
    pathext_var: Option<&OsStr>,
) -> Option<PathBuf> {
    let name = Path::new(program);
    if program.is_empty() || name.components().count() != 1 || program.contains(['/', '\\']) {
        return None;
    }
    let has_launchable_extension = name
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(is_launchable);
    if has_launchable_extension {
        return None;
    }

    let pathext = pathext_var
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_PATHEXT);
    // Lowercased so candidates are spelled the same way on case-sensitive
    // filesystems; Windows matches either case.
    let extensions: Vec<String> = pathext
        .split(';')
        .map(|ext| ext.trim().trim_start_matches('.'))
        .filter(|ext| is_launchable(ext))
        .map(str::to_ascii_lowercase)
        .collect();

    for dir in std::env::split_paths(path_var?) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &extensions {
            let mut file_name = OsString::from(program);
            file_name.push(".");
            file_name.push(ext);
            let candidate = dir.join(file_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn is_launchable(ext: &str) -> bool {
    LAUNCHABLE_EXTENSIONS
        .iter()
        .any(|launchable| ext.eq_ignore_ascii_case(launchable))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn path_of(dirs: &[&Path]) -> OsString {
        std::env::join_paths(dirs).unwrap()
    }

    #[test]
    fn finds_cmd_shim_when_no_exe_exists() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("tool"), "#!/bin/sh\n").unwrap();
        fs::write(dir.path().join("tool.ps1"), "").unwrap();
        fs::write(dir.path().join("tool.cmd"), "@echo off\n").unwrap();
        let path = path_of(&[dir.path()]);

        let resolved = resolve_in(
            "tool",
            Some(&path),
            Some(OsStr::new(".COM;.EXE;.BAT;.CMD;.VBS;.JS")),
        );

        assert_eq!(resolved, Some(dir.path().join("tool.cmd")));
    }

    #[test]
    fn follows_path_order_then_pathext_order() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        fs::write(first.path().join("tool.cmd"), "").unwrap();
        fs::write(first.path().join("tool.bat"), "").unwrap();
        fs::write(second.path().join("tool.exe"), "").unwrap();
        let path = path_of(&[first.path(), second.path()]);

        let resolved = resolve_in("tool", Some(&path), Some(OsStr::new(".EXE;.BAT;.CMD")));

        assert_eq!(resolved, Some(first.path().join("tool.bat")));
    }

    #[test]
    fn skips_extensions_that_need_an_interpreter() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("tool.js"), "").unwrap();
        let path = path_of(&[dir.path()]);

        let resolved = resolve_in("tool", Some(&path), Some(OsStr::new(".JS;.EXE")));

        assert_eq!(resolved, None);
    }

    #[test]
    fn falls_back_to_default_pathext_when_unset() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("tool.cmd"), "").unwrap();
        let path = path_of(&[dir.path()]);

        assert_eq!(
            resolve_in("tool", Some(&path), None),
            Some(dir.path().join("tool.cmd"))
        );
    }

    #[test]
    fn appends_pathext_to_dotted_names_without_launchable_extension() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("tool.v2.cmd"), "").unwrap();
        let path = path_of(&[dir.path()]);

        assert_eq!(
            resolve_in("tool.v2", Some(&path), None),
            Some(dir.path().join("tool.v2.cmd"))
        );
    }

    #[test]
    fn leaves_paths_and_launchable_extensions_alone() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("tool.cmd"), "").unwrap();
        fs::write(dir.path().join("tool.exe.cmd"), "").unwrap();
        let path = path_of(&[dir.path()]);

        assert_eq!(resolve_in("tool.exe", Some(&path), None), None);
        assert_eq!(resolve_in("sub/tool", Some(&path), None), None);
        assert_eq!(resolve_in("sub\\tool", Some(&path), None), None);
        assert_eq!(resolve_in("", Some(&path), None), None);
        assert_eq!(resolve_in("tool", None, None), None);
    }
}
