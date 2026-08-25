//! Copies text to the clipboard: OSC 52 over SSH, a local clipboard tool otherwise.
//!
//! Mirrors roborev's own TUI, which picks the same way for the same reason: OSC 52 reaches the
//! terminal emulator through an SSH connection without X11 forwarding, while a local session can
//! shell out to whatever clipboard tool is already on `PATH`.

use std::io::Write;
use std::time::Duration;

use crate::error::Result;
use crate::exec;

/// Time budget for a local clipboard tool, which only pipes a few kilobytes and exits immediately.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Copy `text` to the clipboard.
pub fn copy(text: &str) -> Result<()> {
    match is_ssh() {
        true => copy_osc52(text),
        false => copy_local(text),
    }
}

fn is_ssh() -> bool {
    ["SSH_TTY", "SSH_CLIENT", "SSH_CONNECTION"]
        .into_iter()
        .any(|var| std::env::var_os(var).is_some())
}

/// Write an OSC 52 clipboard-set sequence to stderr, which reaches the terminal even though stdout
/// is the alternate screen ratatui owns.
fn copy_osc52(text: &str) -> Result<()> {
    let mut stderr = std::io::stderr();
    write!(stderr, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    stderr.flush()?;
    Ok(())
}

/// The clipboard tool to try, in the order a desktop session is likely to have it: Wayland first
/// when a Wayland session is active, then the X11 tools, matching roborev's TUI.
fn copy_local(text: &str) -> Result<()> {
    let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-in", "-selection", "clipboard"]),
            ("xsel", &["--input", "--clipboard"]),
        ]
    } else {
        &[
            ("xclip", &["-in", "-selection", "clipboard"]),
            ("xsel", &["--input", "--clipboard"]),
            ("wl-copy", &[]),
        ]
    };

    let mut last_err = None;
    for (program, args) in candidates {
        match exec::run_stdin_ok_timed(program, args, text.as_bytes(), None, COMMAND_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.expect("candidates is non-empty"))
}

/// Minimal RFC 4648 base64 encoder, standard alphabet with padding. OSC 52 payloads are small
/// enough that pulling in a crate for this one encode isn't worth it.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[(n >> 18 & 0x3f) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3f) as usize] as char);
        out.push(match chunk.len() {
            1 => '=',
            _ => ALPHABET[(n >> 6 & 0x3f) as usize] as char,
        });
        out.push(match chunk.len() {
            1 | 2 => '=',
            _ => ALPHABET[(n & 0x3f) as usize] as char,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn encodes_without_padding_when_length_is_a_multiple_of_three() {
        assert_eq!(base64_encode(b"Man"), "TWFu");
    }

    #[test]
    fn pads_one_byte_remainder_with_two_equals() {
        assert_eq!(base64_encode(b"M"), "TQ==");
    }

    #[test]
    fn pads_two_byte_remainder_with_one_equals() {
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn empty_input_encodes_to_empty_string() {
        assert_eq!(base64_encode(b""), "");
    }
}
