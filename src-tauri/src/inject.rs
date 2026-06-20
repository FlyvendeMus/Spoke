//! Text injection into the focused window via enigo (OS-level keyboard sim).
//!
//! `Enigo` is not `Send`, so it must be created and used within a single
//! thread. Construct it inside the call (e.g. from `spawn_blocking`) and let it
//! drop before the closure returns.

use anyhow::{anyhow, Result};
use enigo::{Enigo, Keyboard, Settings};

/// Type `text` into whatever window currently has focus. No-op for empty input.
pub fn inject_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow!("failed to init keyboard simulation: {e}"))?;
    enigo
        .text(text)
        .map_err(|e| anyhow!("failed to inject text: {e}"))?;
    Ok(())
}
