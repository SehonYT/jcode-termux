//! Typed failures. Nothing in this crate panics; every dead end is a variant here.

use std::fmt;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ClipboardError>;

/// Why a clipboard operation could not be completed.
///
/// Variants are deliberately coarse: callers in jcode-tui either report
/// "copied" or "could not reach the clipboard", and a caller that wants detail
/// can render the [fmt::Display] text into a status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    /// The platform has no clipboard read path at all. Windows is the only
    /// such platform here: OSC 52 is write-only by design, so there is nothing
    /// to read back.
    Unsupported(&'static str),
    /// No mechanism is installed/available on this platform, e.g. Android
    /// without the Termux:API addon and without a terminal that consumes
    /// OSC 52. Distinct from [ClipboardError::Failed]: nothing was attempted
    /// and nothing was corrupted, the user simply has no clipboard bridge.
    NoMechanism,
    /// A mechanism ran but did not succeed. The string names the program (or
    /// osc52) plus the reason.
    ///
    /// A Failed from a mechanism *stops* the fallback chain rather than
    /// falling through, so that a real helper which already consumed the
    /// payload is not reported as a success by a weaker mechanism.
    Failed(String),
    /// The operation exceeded its hard timeout and the child was killed.
    Timeout {
        /// Wall-clock budget that was exceeded.
        limit_ms: u64,
        /// The reason text collected so far (may be empty).
        stderr: String,
    },
    /// The text handed to [crate::Clipboard::write_osc52] cannot be represented
    /// in OSC 52: it contains a raw ESC or BEL, which would terminate the
    /// sequence early and inject arbitrary terminal control bytes.
    ///
    /// This is *text*, not an image: OSC 52 is a text mechanism, which is why
    /// there is no "too large" variant here. Getting bytes out on Android is
    /// [crate::Clipboard::set_image]'s job (it refuses too, explicitly).
    Unencodable(&'static str),
    /// The payload was produced by a command but is not valid UTF-8, and the
    /// caller asked for text.
    NotUtf8 {
        /// Program that produced the bytes.
        program: String,
        /// How many bytes came back (for the status line).
        len: usize,
    },
    /// The payload was produced by a command but is empty. Most Android
    /// clipboard reads come back empty when the clipboard holds a non-text
    /// clip, so callers usually want to distinguish this from a hard error.
    Empty {
        /// Program that produced the empty output.
        program: String,
    },
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardError::Unsupported(what) => write!(f, "clipboard unsupported here: {what}"),
            ClipboardError::NoMechanism => write!(
                f,
                "no clipboard bridge available (Android: install the Termux:API addon \
                 and pkg install termux-api; a terminal that implements OSC 52 also works)"
            ),
            ClipboardError::Failed(detail) => write!(f, "clipboard command failed: {detail}"),
            ClipboardError::Timeout { limit_ms, stderr } => {
                write!(f, "clipboard command timed out after {limit_ms}ms")?;
                if !stderr.trim().is_empty() {
                    write!(f, " ({})", stderr.trim())?;
                }
                Ok(())
            }
            ClipboardError::Unencodable(reason) => {
                write!(f, "clipboard payload cannot be OSC 52 encoded: {reason}")
            }
            ClipboardError::NotUtf8 { program, len } => {
                write!(f, "{program} returned {len} bytes that are not valid UTF-8")
            }
            ClipboardError::Empty { program } => {
                write!(f, "{program} returned no data (clipboard is empty or not text)")
            }
        }
    }
}

impl std::error::Error for ClipboardError {}

impl ClipboardError {
    /// A short, human-facing label suitable for a TUI status line.
    ///
    /// Kept separate from [fmt::Display] so a long diagnostic (which names the
    /// exact binary and its stderr) never lands in chrome that must not wrap.
    pub fn short_label(&self) -> String {
        match self {
            ClipboardError::Unsupported(_) => "no clipboard on this platform".to_string(),
            ClipboardError::NoMechanism => "no clipboard bridge found".to_string(),
            ClipboardError::Failed(detail) => {
                format!("clipboard failed: {}", truncate(detail, 48))
            }
            ClipboardError::Timeout { .. } => "clipboard timed out".to_string(),
            ClipboardError::Unencodable(_) => "clipboard payload unencodable".to_string(),
            ClipboardError::NotUtf8 { .. } => "clipboard holds non-text data".to_string(),
            ClipboardError::Empty { .. } => "clipboard is empty".to_string(),
        }
    }
}

/// Truncate on a char boundary (never splits UTF-8, never panics).
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}...")
}
