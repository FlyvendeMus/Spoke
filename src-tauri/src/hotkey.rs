//! Parse a config hotkey string (e.g. "ctrl+alt+space") into a global shortcut.

use anyhow::{anyhow, Result};
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// Parse "mod+mod+key" into a `Shortcut`. Modifiers: ctrl/control, alt/option,
/// shift, cmd/super/meta/win. The final token is the key.
pub fn parse_shortcut(spec: &str) -> Result<Shortcut> {
    let mut mods = Modifiers::empty();
    let mut key: Option<Code> = None;

    for raw in spec.split('+') {
        let token = raw.trim().to_lowercase();
        if token.is_empty() {
            continue;
        }
        match token.as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "cmd" | "command" | "super" | "meta" | "win" => mods |= Modifiers::SUPER,
            other => {
                key = Some(parse_code(other)?);
            }
        }
    }

    let code = key.ok_or_else(|| anyhow!("hotkey '{spec}' has no key"))?;
    let modifiers = if mods.is_empty() { None } else { Some(mods) };
    Ok(Shortcut::new(modifiers, code))
}

fn parse_code(key: &str) -> Result<Code> {
    let code = match key {
        "space" => Code::Space,
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "esc" | "escape" => Code::Escape,
        "a" => Code::KeyA,
        "b" => Code::KeyB,
        "c" => Code::KeyC,
        "d" => Code::KeyD,
        "e" => Code::KeyE,
        "f" => Code::KeyF,
        "g" => Code::KeyG,
        "h" => Code::KeyH,
        "i" => Code::KeyI,
        "j" => Code::KeyJ,
        "k" => Code::KeyK,
        "l" => Code::KeyL,
        "m" => Code::KeyM,
        "n" => Code::KeyN,
        "o" => Code::KeyO,
        "p" => Code::KeyP,
        "q" => Code::KeyQ,
        "r" => Code::KeyR,
        "s" => Code::KeyS,
        "t" => Code::KeyT,
        "u" => Code::KeyU,
        "v" => Code::KeyV,
        "w" => Code::KeyW,
        "x" => Code::KeyX,
        "y" => Code::KeyY,
        "z" => Code::KeyZ,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        other => return Err(anyhow!("unknown key '{other}'")),
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_hotkey() {
        let s = parse_shortcut("ctrl+alt+space").unwrap();
        assert_eq!(s.key, Code::Space);
        assert_eq!(s.mods, Modifiers::CONTROL | Modifiers::ALT);
    }

    #[test]
    fn parses_single_key() {
        let s = parse_shortcut("f5").unwrap();
        assert_eq!(s.key, Code::F5);
        assert_eq!(s.mods, Modifiers::empty());
    }

    #[test]
    fn case_insensitive_and_trims() {
        let s = parse_shortcut(" CMD + Shift + A ").unwrap();
        assert_eq!(s.key, Code::KeyA);
        assert_eq!(s.mods, Modifiers::SUPER | Modifiers::SHIFT);
    }

    #[test]
    fn errors_without_key() {
        assert!(parse_shortcut("ctrl+alt").is_err());
    }

    #[test]
    fn errors_on_unknown_key() {
        assert!(parse_shortcut("ctrl+banana").is_err());
    }
}
