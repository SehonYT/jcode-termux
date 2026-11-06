//! The non-Android backend: Linux/BSD clipboard helpers plus OSC 52.
//!
//! This is the same mechanism ordering `jcode-tui` grew by hand over several
//! bug reports, moved here so there is one implementation:
//!
//! * Wayland: `wl-copy` / `wl-paste`. `wl-copy` forks a server that owns the
//!   selection, so it is run with [CommandSpec::with_detach] and a still-running
//!   child counts as success.
//! * X11: `xclip` / `xsel` before arboard. arboard's X11 backend owns the
//!   selection through a connection it closes on drop, so the clipboard
//!   silently reverts (upstream issue #684); xclip/xsel fork a background
//!   owner instead, which is what users expect.
//! * Otherwise OSC 52, which covers SSH/Docker/tmux sessions where no display
//!   exists at all.
//!
//! `arboard` itself stays with the caller: on Windows and macOS it is
//! authoritative (see issue #497 - OSC 52 is silently ignored by conhost and
//! Terminal.app, so writing the sequence first would report a false success).
//! `jcode-tui` calls arboard before this crate on those platforms.

use base64::Engine as _;

use crate::error::{ClipboardError, Result};
use crate::runner::{CommandRunner, CommandSpec, RunOutput};
use crate::{osc52, ClipboardConfig};

/// Clipboard owners tried in order for `set_text`.
const SET_FALLBACKS: &[(&str, &[&str])] = &[
    // Wayland first: it is the only one that works without an X server on a
    // modern desktop, and `wl-copy` fails fast when WAYLAND_DISPLAY is unset.
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// Clipboard readers tried in order for `get_text`.
const GET_FALLBACKS: &[(&str, &[&str])] = &[
    ("wl-paste", &["--no-newline"]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];

/// MIME types tried (in order) when reading an image.
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Copy `text` with the native helpers, falling back to OSC 52 when none is
/// installed.
pub fn set_text<R: CommandRunner>(
    runner: &R,
    config: &ClipboardConfig,
    text: &str,
) -> Result<()> {
    for (program, args) in SET_FALLBACKS {
        if !runner.has_program(program) {
            continue;
        }
        let spec = CommandSpec::new(*program, args)
            .with_stdin(text.as_bytes())
            // wl-copy/xclip deliberately outlive the call to keep owning the
            // selection; killing them would drop the clipboard.
            .with_detach()
            .with_timeout(config.timeout);
        match runner.run(&spec)? {
            RunOutput::Exited { .. } | RunOutput::StillRunning { .. } => return Ok(()),
            // Ran and failed (typically no DISPLAY/WAYLAND_DISPLAY): this is a
            // real failure, so do not report a later mechanism's no-op as
            // success. The mapping keeps a timeout a *timeout*, because the
            // caller words the status line differently for "hung" than for
            // "refused".
            other => return Err(error_for(program, &other, config)), 
        }
    }
    // No helper at all: OSC 52 is the caller's job (it owns stdout), so the
    // caller must fall through to `Clipboard::write_osc52`.
    Err(ClipboardError::NoMechanism)
}

/// Read text with the native helpers.
pub fn get_text<R: CommandRunner>(runner: &R, config: &ClipboardConfig) -> Result<String> {
    for (program, args) in GET_FALLBACKS {
        if !runner.has_program(program) {
            continue;
        }
        let spec = CommandSpec::new(*program, args).with_timeout(config.timeout);
        match runner.run(&spec)? {
            RunOutput::Exited { stdout, .. } => return decode_text(program, stdout),
            other => return Err(error_for(program, &other, config)),
        }
    }
    Err(ClipboardError::NoMechanism)
}

/// Map a non-successful [RunOutput] onto the typed error the caller reports.
///
/// Shared by every helper invocation so a timeout is a [ClipboardError::Timeout]
/// everywhere rather than a [ClipboardError::Failed] with the word "timed out"
/// buried in a string.
fn error_for(program: &str, output: &RunOutput, config: &ClipboardConfig) -> ClipboardError {
    match output {
        RunOutput::TimedOut { stderr, .. } => ClipboardError::Timeout {
            limit_ms: u64::try_from(config.timeout.as_millis()).unwrap_or(u64::MAX),
            stderr: stderr.clone(),
        },
        RunOutput::StillRunning { observed_ms, .. } => ClipboardError::Failed(format!(
            "{program}: still running after {observed_ms}ms (never returned)"
        )),
        other => ClipboardError::Failed(format!("{program}: {}", other.stderr_text())),
    }
}

/// Copy a PNG image with `wl-copy`/`xclip`.
///
/// Refuses non-PNG input explicitly: every one of these helpers takes the MIME
/// type on faith, so a JPEG mislabelled as PNG would land on the clipboard as
/// an unopenable blob.
pub fn set_image<R: CommandRunner>(
    runner: &R,
    config: &ClipboardConfig,
    media_type: &str,
    bytes: &[u8],
) -> Result<()> {
    if media_type != "image/png" {
        return Err(ClipboardError::Unsupported(
            "only image/png is supported by the terminal clipboard helpers",
        ));
    }
    if bytes.is_empty() {
        return Err(ClipboardError::Failed("empty image payload".to_string()));
    }
    if runner.has_program("wl-copy") {
        let spec = CommandSpec::new("wl-copy", &["-t", media_type])
            .with_stdin(bytes.to_vec())
            .with_detach()
            .with_timeout(config.timeout);
        return match runner.run(&spec)? {
            RunOutput::Exited { .. } | RunOutput::StillRunning { .. } => Ok(()),
            other => Err(error_for("wl-copy", &other, config)),
        };
    }
    if runner.has_program("xclip") {
        // xclip can read the image from stdin with an explicit target type.
        let spec = CommandSpec::new("xclip", &["-selection", "clipboard", "-t", media_type, "-i"])
            .with_stdin(bytes.to_vec())
            .with_detach()
            .with_timeout(config.timeout);
        return match runner.run(&spec)? {
            RunOutput::Exited { .. } | RunOutput::StillRunning { .. } => Ok(()),
            other => Err(error_for("xclip", &other, config)),
        };
    }
    Err(ClipboardError::NoMechanism)
}

/// Read an image (`media_type`, bytes) from `wl-paste` or `xclip`.
pub fn get_image<R: CommandRunner>(
    runner: &R,
    config: &ClipboardConfig,
) -> Result<(String, Vec<u8>)> {
    if runner.has_program("wl-paste") {
        // Ask for the raw MIME type first; `wl-paste -l` only lists types, so
        // a wrong type here fails fast rather than returning junk.
        for media_type in IMAGE_TYPES {
            let spec = CommandSpec::new("wl-paste", &["--type", media_type, "--no-newline"])
                .with_timeout(config.timeout);
            match runner.run(&spec)? {
                RunOutput::Exited { stdout, .. } if !stdout.is_empty() => {
                    return Ok(((*media_type).to_string(), stdout));
                }
                // Two distinct "no": the owner does not offer this MIME type
                // (exit non-zero / empty stdout), or wl-paste cannot reach a
                // compositor at all. Both mean "try the next candidate" -
                // aborting here would report "no image" for an image that is
                // sitting on the clipboard as PNG behind a missing WebP.
                _ => continue,
            }
        }
    }
    if runner.has_program("xclip") {
        let spec = CommandSpec::new(
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        )
        .with_timeout(config.timeout);
        match runner.run(&spec)? {
            RunOutput::Exited { stdout, .. } if !stdout.is_empty() => {
                return Ok(("image/png".to_string(), stdout));
            }
            _ => {}
        }
    }
    Err(ClipboardError::NoMechanism)
}

/// Decode an image helper's stdout as PNG, verifying the magic bytes.
///
/// `xclip -t image/png -o` happily returns whatever the owner stored as that
/// target, and image owners lie. A caller that base64s the result into a data:
/// URL would otherwise ship an unopenable attachment.
pub fn png_bytes(bytes: Vec<u8>) -> Result<Vec<u8>> {
    const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.starts_with(PNG_MAGIC) {
        Ok(bytes)
    } else {
        Err(ClipboardError::Failed(format!(
            "clipboard image is not PNG ({} bytes, magic {:?})",
            bytes.len(),
            bytes.iter().take(8).collect::<Vec<_>>()
        )))
    }
}

/// Interpret a finished process's stdout as UTF-8 text.
fn decode_text(program: &str, stdout: Vec<u8>) -> Result<String> {
    if stdout.is_empty() {
        return Err(ClipboardError::Empty {
            program: program.to_string(),
        });
    }
    String::from_utf8(stdout).map_err(|err| ClipboardError::NotUtf8 {
        program: program.to_string(),
        len: err.as_bytes().len(),
    })
}

/// Encode `bytes` as a base64 data payload (helper for image callers).
pub fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// OSC 52 escape for the given target (re-exported for callers that own stdio).
pub fn osc52_sequence<W: std::io::Write>(
    out: &mut W,
    target: crate::Osc52Target,
    text: &str,
) -> Result<()> {
    osc52::write(out, target, text)
}
