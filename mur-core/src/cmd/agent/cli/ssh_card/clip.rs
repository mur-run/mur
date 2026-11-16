//! Copy text to the terminal's clipboard with OSC 52. murmur owns the
//! terminal, so the escape goes straight to stdout; a terminal that does not
//! allow clipboard writes ignores it, which is why the card also shows the
//! line in full.

use std::io::Write;

use base64::Engine;

/// The OSC 52 sequence that sets the clipboard (`c`) to `text`.
pub fn osc52(text: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    format!("\x1b]52;c;{b64}\x07")
}

pub fn copy(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    out.write_all(osc52(text).as_bytes())?;
    out.flush()
}
