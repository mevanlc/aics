//! Cross-platform clipboard abstraction with Termux support.
//!
//! On Android/Termux (or with the `termux` feature), uses
//! `termux-clipboard-set` and `termux-clipboard-get` commands.
//! On other platforms, uses the `arboard` crate.

use anyhow::Result;

/// Set text to the clipboard.
pub fn set_text(text: &str) -> Result<()> {
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
