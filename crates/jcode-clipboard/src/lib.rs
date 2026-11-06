//! Terminal-native clipboard for jcode, with a real Android backend.
//!
//! # Why this crate exists (Termux/armv7 port)
//!
//! The desktop builds use [\`arboard\`](https://docs.rs/arboard). Android cannot:
//! upstream arboard's \`src/platform/mod.rs\` only declares a backend for
//! \`not(any(target_os = "macos", target_os = "android", ...))\`, so on
//! \`target_os = "android"\` its \`platform\` module is empty and \`arboard::Clipboard\`
//! does not exist. Termux has no X11, Wayland, Win32 or NSPasteboard either, so
//! arboard could never work on a phone even if it compiled. This crate is the
//! replacement: it talks to the clipboard mechanisms that *do* exist in a
//! terminal on Android.
//!
//! # Backend selection
//!
//! | Platform | set | get |
//! |---|---|---|
//! | Android | \`termux-clipboard-set\`, then OSC 52 | \`termux-clipboard-get\`, then \`wl-paste\`/\`xclip\`/\`xsel\` if present |
//! | Windows | OSC 52 only (the native path stays with arboard in \`jcode-tui\`) | none |
//! | Everything else | \`wl-copy\`, \`xclip\`, \`xsel\`, then OSC 52 | \`wl-paste\`, \`xclip\`, \`xsel\` |
//!
//! \`/system/bin/service call clipboard ...\` is deliberately **not** used: it is
//! SELinux-blocked for ordinary apps and hangs rather than failing.
//!
//! # Guarantees
//!
//! * Pure Rust, no \`unsafe\`, no C.
//! * Every subprocess runs under a hard timeout ([runner::DEFAULT_TIMEOUT],
//!   2s; configurable via [ClipboardConfig]). Android clipboard helper
//!   binaries routinely block forever when the Termux:API addon is missing or
//!   the user is looking at a permission dialog.
//! * Nothing panics. Every failure is a typed [ClipboardError].
//! * The command runner is injectable ([CommandRunner]), so the selection
//!   logic is tested without Android and without touching a real clipboard.
//!
//! # Example
//!
//! \`\`\`no_run
//! use jcode_clipboard::Clipboard;
//!
//! let clipboard = Clipboard::new();
//! match clipboard.set_text("hello") {
//!     Ok(()) => println!("copied"),
//!     Err(err) => eprintln!("no clipboard: {err}"),
//! }
//! if let Ok(text) = clipboard.get_text() {
//!     println!("clipboard holds {text:?}");
//! }
//! \`\`\`

#![forbid(unsafe_code)]

pub mod error;
pub mod osc52;
pub mod runner;

// Exactly one of these is the *dispatched* backend (see
// `set_text_with`/`get_text_with`). Both are additionally compiled under
// `cfg(test)` so a single `cargo test` run covers both chains: the host
// triple decides which one production uses, and on Termux that host triple is
// itself `target_os = "android"`, which would otherwise leave the desktop
// chain untested exactly where the port is being developed.
#[cfg(any(target_os = "android", test))]
pub mod android;
#[cfg(any(not(target_os = "android"), test))]
pub mod unix;

#[cfg(test)]
mod tests;

pub use error::{ClipboardError, Result};
pub use runner::{CommandRunner, CommandSpec, ProcessRunner, RunOutput, DEFAULT_TIMEOUT};

use std::time::Duration;

/// Where [Clipboard::set_text] wants the payload to end up.
///
/// Most terminals copy OSC 52 writes straight into the *system* clipboard, but
/// some (and tmux's copy buffer, and most terminals over SSH) target their own
/// selection. Android is the reason this exists: two half-working mechanisms
/// are worse than one deliberate target, and a hard-stuck Termux:API call
/// should not silently leave a second copy on some other selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Osc52Target {
    /// The terminal's/system clipboard. This is the OSC 52 \`c\` selector.
    #[default]
    Clipboard,
    /// The X11 primary selection.
    ///
    /// Android/Termux has no primary selection, so this is only ever used by
    /// desktop callers that ask for it explicitly. `cfg(test)` keeps the
    /// variant (and its `p` selector) compilable on an Android host so the
    /// unix-chain tests can cover it from a phone.
    #[cfg(any(not(target_os = "android"), test))]
    Primary,
}

impl Osc52Target {
    /// The single-character OSC 52 selection selector (\`c\` or \`p\`).
    pub fn selector(self) -> char {
        match self {
            Osc52Target::Clipboard => 'c',
            #[cfg(any(not(target_os = "android"), test))]
            Osc52Target::Primary => 'p',
        }
    }
}

/// Tuning for a [Clipboard].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardConfig {
    /// Hard upper bound on any single subprocess call. Defaults to
    /// [runner::DEFAULT_TIMEOUT] (2s).
    pub timeout: Duration,
    /// Where OSC 52 writes should land.
    pub osc52_target: Osc52Target,
    /// Session-scoped "a Termux:API clipboard helper is installed" cache.
    ///
    /// Termux:API is missing on most installs, so probing \`termux-clipboard-set\`
    /// on every copy would pay a failed \`execvp\` per keypress. Set to \`None\`
    /// (the default, and what [Clipboard::new] uses) to probe once per
    /// process; set to \`Some\` to make the decision injectable for tests. Only
    /// consulted on Android.
    pub termux_api_available: Option<bool>,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            timeout: runner::DEFAULT_TIMEOUT,
            osc52_target: Osc52Target::default(),
            termux_api_available: None,
        }
    }
}

impl ClipboardConfig {
    /// A config with the given subprocess timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Self::default()
        }
    }
}

/// The clipboard handle.
///
/// Constructing one does no I/O. The first operation that actually needs a
/// mechanism detects it (availability probe / display check) and remembers the
/// answer for the process lifetime, guarded by a [std::sync::Mutex] that is
/// never held across a spawn.
pub struct Clipboard {
    config: ClipboardConfig,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipboard {
    /// A clipboard with default tuning (2s subprocess timeout, system clipboard).
    pub fn new() -> Self {
        Self::with_config(ClipboardConfig::default())
    }

    /// A clipboard with explicit tuning.
    pub fn with_config(config: ClipboardConfig) -> Self {
        Self { config }
    }

    /// A clipboard with an explicit subprocess timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self::with_config(ClipboardConfig::with_timeout(timeout))
    }

    /// This handle's config.
    pub fn config(&self) -> &ClipboardConfig {
        &self.config
    }

    /// Copy \`text\` to the clipboard.
    ///
    /// Tries the platform chain (see the crate docs) and returns
    /// [ClipboardError::NoMechanism] only when every mechanism is
    /// definitively unavailable. A mechanism that ran but failed
    /// ([ClipboardError::Failed]) *stops* the chain: claiming success from a
    /// later, weaker mechanism after a real helper already read the bytes would
    /// be a lie, and reporting failure is the honest outcome.
    pub fn set_text(&self, text: &str) -> Result<()> {
        self.set_text_with(&runner::ProcessRunner, text)
    }

    /// [Clipboard::set_text] against an injected runner.
    pub fn set_text_with<R: CommandRunner>(&self, runner: &R, text: &str) -> Result<()> {
        #[cfg(target_os = "android")]
        {
            android::set_text(runner, &self.config, text)
        }
        #[cfg(not(target_os = "android"))]
        {
            unix::set_text(runner, &self.config, text)
        }
    }

    /// Read text from the clipboard.
    ///
    /// Returns [ClipboardError::Unsupported] on platforms with no portable
    /// read path (Windows - OSC 52 is write-only by design), and
    /// [ClipboardError::NoMechanism] when the platform has read paths but
    /// none of them is installed/available.
    pub fn get_text(&self) -> Result<String> {
        self.get_text_with(&runner::ProcessRunner)
    }

    /// [Clipboard::get_text] against an injected runner.
    pub fn get_text_with<R: CommandRunner>(&self, runner: &R) -> Result<String> {
        #[cfg(target_os = "android")]
        {
            android::get_text(runner, &self.config)
        }
        #[cfg(not(target_os = "android"))]
        {
            unix::get_text(runner, &self.config)
        }
    }

    /// Copy an image to the clipboard.
    ///
    /// \`media_type\` must be \`image/png\`; PNG is the only format every target
    /// here accepts, and silently transcoding would hide a caller bug.
    pub fn set_image(&self, media_type: &str, bytes: &[u8]) -> Result<()> {
        self.set_image_with(&runner::ProcessRunner, media_type, bytes)
    }

    /// [Clipboard::set_image] against an injected runner.
    pub fn set_image_with<R: CommandRunner>(
        &self,
        runner: &R,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<()> {
        #[cfg(target_os = "android")]
        {
            let _ = (runner, media_type, bytes);
            Err(ClipboardError::Unsupported(
                "images: Android exposes text only over Termux:API and OSC 52",
            ))
        }
        #[cfg(not(target_os = "android"))]
        {
            unix::set_image(runner, &self.config, media_type, bytes)
        }
    }

    /// Read an image from the clipboard as \`(media_type, bytes)\`.
    pub fn get_image(&self) -> Result<(String, Vec<u8>)> {
        self.get_image_with(&runner::ProcessRunner)
    }

    /// [Clipboard::get_image] against an injected runner.
    pub fn get_image_with<R: CommandRunner>(&self, runner: &R) -> Result<(String, Vec<u8>)> {
        #[cfg(target_os = "android")]
        {
            let _ = runner;
            Err(ClipboardError::Unsupported(
                "images: Android exposes text only over Termux:API and OSC 52",
            ))
        }
        #[cfg(not(target_os = "android"))]
        {
            unix::get_image(runner, &self.config)
        }
    }

    /// Write an OSC 52 sequence for \`text\` to stdout.
    ///
    /// Exposed because \`jcode-tui\` already owns an OSC 52 write on several
    /// platforms and should get it from one place (the base64 framing is easy
    /// to get subtly wrong). \`out\` is generic so the caller can pass the real
    /// [std::io::stdout] or a buffer; \`is_tty\` is passed in so a non-terminal
    /// stdout does not get an escape sequence sprayed into a pipe.
    ///
    /// On Android one *failed* Termux:API attempt is recorded here, so the
    /// report says "Termux:API failed, fell back to OSC 52" rather than a bare
    /// success. OSC 52 cannot confirm delivery - the terminal may silently
    /// ignore it - so success means "written", never "delivered".
    pub fn write_osc52<W: std::io::Write>(
        &self,
        out: &mut W,
        is_tty: bool,
        text: &str,
    ) -> Result<()> {
        #[cfg(target_os = "android")]
        if is_tty {
            android::note_osc52_after_failed_termux_api();
        }
        let _ = (is_tty, text);
        osc52::write(out, self.config.osc52_target, text)
    }
}

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clipboard")
            .field("config", &self.config)
            .finish()
    }
}
