//! OSC 52: the terminal escape sequence that asks the *terminal emulator* to
//! set the system clipboard.
//!
//! ```text
//! ESC ] 52 ; <selection> ; <base64 of the text> BEL
//! ```
//!
//! This is the only clipboard mechanism that needs no helper binary, which
//! makes it the universal fallback: it works over SSH, inside Docker, inside
//! tmux (with `set -g set-clipboard on`), and on an Android phone with no
//! Termux:API addon. Termux's own terminal (`com.termux`) implements it.
//!
//! Two properties matter for correctness:
//!
//! * OSC 52 is **write-only**. There is no read sequence, so any code path
//!   that must *read* the clipboard cannot use it (that is why Android's get
//!   path is Termux:API or nothing).
//! * Writing the sequence to stdout succeeds even when the terminal silently
//!   ignores it (conhost, Terminal.app, xterm without `allowWindowOps`). So
//!   the boolean this module returns means "sequence written", never
//!   "clipboard updated". Callers that need certainty use a helper binary.

use std::io::Write;

use base64::Engine as _;

use crate::error::{ClipboardError, Result};
use crate::Osc52Target;

/// Largest text we will base64 into one OSC 52 sequence.
///
/// Not a hard protocol limit (the protocol has none) but an octet-stream limit:
/// terminals commonly cap escape sequences at 8 KiB-100 KiB, and a sequence
/// that gets dropped mid-way corrupts the screen. Refusing loudly beats
/// truncating, which would silently put half a document on the clipboard.
pub const MAX_OSC52_BYTES: usize = 8 * 1024;

/// Build the OSC 52 sequence for `text` without writing it.
///
/// Returns [ClipboardError::Unencodable] if `text` contains a raw `ESC`
/// (0x1B) or `BEL` (0x07): either would terminate the OSC sequence early and
/// let the rest of the payload inject terminal control bytes. base64 cannot
/// escape them - that is the whole point of the encoding - so the sequence
/// would be well-formed and *wrong*.
pub fn encode(target: Osc52Target, text: &str) -> Result<String> {
    if text.contains('\u{1b}') {
        return Err(ClipboardError::Unencodable("payload contains ESC (0x1B)"));
    }
    if text.contains('\u{7}') {
        return Err(ClipboardError::Unencodable("payload contains BEL (0x07)"));
    }
    let bytes = text.as_bytes();
    if bytes.len() > MAX_OSC52_BYTES {
        return Err(ClipboardError::Unencodable(
            "payload larger than the 8KiB OSC 52 octet-stream budget",
        ));
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("\x1b]52;{};{}\x07", target.selector(), encoded))
}

/// Encode `text` and write the sequence to `out`, then flush.
///
/// The writer is generic so tests can capture the bytes and so an animated TUI
/// can route it through its own frame buffer; production callers pass
/// [std::io::stdout].
pub fn write<W: Write>(out: &mut W, target: Osc52Target, text: &str) -> Result<()> {
    let sequence = encode(target, text)?;
    out.write_all(sequence.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|err| ClipboardError::Failed(format!("osc52 write: {err}")))
}
