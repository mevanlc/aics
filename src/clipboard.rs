//! Cross-platform clipboard abstraction with SSH OSC 52 and Termux support.
//!
//! When running under SSH, uses OSC 52 sequences so the local terminal can own
//! the clipboard operation.
//! On Android/Termux (or with the `termux` feature), uses
//! `termux-clipboard-set` and `termux-clipboard-get` commands.
//! On other platforms, uses the `arboard` crate.

use std::env;
use std::ffi::OsStr;
use std::io::{self, Write};

use anyhow::{Context, Result};
use base64::Engine;

/// Set text to the clipboard.
pub fn set_text(text: &str) -> Result<()> {
    if should_use_osc_clipboard() {
        return write_osc52_clipboard(io::stdout().lock(), text);
    }

    #[cfg(any(feature = "termux", target_os = "android"))]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut child = Command::new("termux-clipboard-set")
            .stdin(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }

        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("termux-clipboard-set failed")
        }
    }

    #[cfg(all(not(feature = "termux"), not(target_os = "android")))]
    {
        let mut clipboard = arboard::Clipboard::new()?;
        clipboard.set_text(text)?;
        Ok(())
    }
}

fn should_use_osc_clipboard() -> bool {
    ssh_tty_env_value_uses_osc(env::var_os("SSH_TTY").as_deref())
}

fn ssh_tty_env_value_uses_osc(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn write_osc52_clipboard(mut writer: impl Write, text: &str) -> Result<()> {
    writer
        .write_all(osc52_clipboard_sequence(text).as_bytes())
        .context("failed to write OSC 52 clipboard sequence")?;
    writer
        .flush()
        .context("failed to flush OSC 52 clipboard sequence")?;
    Ok(())
}

fn osc52_clipboard_sequence(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    format!("\x1b]52;c;{encoded}\x07")
}

/// Get text from the clipboard.
#[allow(dead_code)]
pub fn get_text() -> Result<String> {
    #[cfg(any(feature = "termux", target_os = "android"))]
    {
        use std::process::Command;

        let output = Command::new("termux-clipboard-get").output()?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            anyhow::bail!("termux-clipboard-get failed")
        }
    }

    #[cfg(all(not(feature = "termux"), not(target_os = "android")))]
    {
        let mut clipboard = arboard::Clipboard::new()?;
        Ok(clipboard.get_text()?)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{osc52_clipboard_sequence, ssh_tty_env_value_uses_osc, write_osc52_clipboard};

    #[test]
    fn ssh_tty_value_selects_osc_clipboard() {
        assert!(ssh_tty_env_value_uses_osc(Some(OsStr::new("/dev/ttys003"))));
    }

    #[test]
    fn missing_or_empty_ssh_tty_does_not_select_osc_clipboard() {
        assert!(!ssh_tty_env_value_uses_osc(None));
        assert!(!ssh_tty_env_value_uses_osc(Some(OsStr::new(""))));
    }

    #[test]
    fn osc52_sequence_base64_encodes_clipboard_text() {
        assert_eq!(
            osc52_clipboard_sequence("session/path"),
            "\x1b]52;c;c2Vzc2lvbi9wYXRo\x07"
        );
    }

    #[test]
    fn osc52_writer_emits_sequence() {
        let mut output = Vec::new();

        write_osc52_clipboard(&mut output, "abc").unwrap();

        assert_eq!(output, b"\x1b]52;c;YWJj\x07");
    }
}
