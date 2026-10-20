//! Keyboard events for `Input.dispatchKeyEvent`: Chrome needs the Windows
//! virtual key code to act on special keys (Backspace, arrows, Enter…).

use serde_json::{Value, json};

pub const ALT: u32 = 1;
pub const CTRL: u32 = 2;
pub const META: u32 = 4;
pub const SHIFT: u32 = 8;

/// Windows virtual key code for a DOM `code` (preferred) or `key`.
pub fn key_code(key: &str, code: &str) -> u32 {
    if let Some(c) = from_code(code) {
        return c;
    }
    from_key(key).unwrap_or(0)
}

fn from_code(code: &str) -> Option<u32> {
    if let Some(l) = code.strip_prefix("Key")
        && l.len() == 1
    {
        let c = l.as_bytes()[0];
        return c.is_ascii_uppercase().then_some(c as u32);
    }
    if let Some(d) = code.strip_prefix("Digit")
        && d.len() == 1
        && d.as_bytes()[0].is_ascii_digit()
    {
        return Some(d.as_bytes()[0] as u32);
    }
    if let Some(d) = code.strip_prefix("Numpad")
        && d.len() == 1
        && d.as_bytes()[0].is_ascii_digit()
    {
        return Some(96 + (d.as_bytes()[0] - b'0') as u32);
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        return Some(111 + n);
    }
    Some(match code {
        "Semicolon" => 186,
        "Equal" => 187,
        "Comma" => 188,
        "Minus" => 189,
        "Period" => 190,
        "Slash" => 191,
        "Backquote" => 192,
        "BracketLeft" => 219,
        "Backslash" => 220,
        "BracketRight" => 221,
        "Quote" => 222,
        "NumpadMultiply" => 106,
        "NumpadAdd" => 107,
        "NumpadSubtract" => 109,
        "NumpadDecimal" => 110,
        "NumpadDivide" => 111,
        "NumpadEnter" => 13,
        "Space" => 32,
        "ShiftLeft" | "ShiftRight" => 16,
        "ControlLeft" | "ControlRight" => 17,
        "AltLeft" | "AltRight" => 18,
        "MetaLeft" | "MetaRight" | "OSLeft" | "OSRight" => 91,
        "ContextMenu" => 93,
        _ => return from_key(code),
    })
}

fn from_key(key: &str) -> Option<u32> {
    Some(match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Pause" => 19,
        "CapsLock" => 20,
        "Escape" => 27,
        " " => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "PrintScreen" => 44,
        "Insert" => 45,
        "Delete" => 46,
        "Meta" => 91,
        "ContextMenu" => 93,
        "NumLock" => 144,
        "ScrollLock" => 145,
        k if k.chars().count() == 1 => {
            let c = k.chars().next()?;
            if c.is_ascii_alphabetic() {
                c.to_ascii_uppercase() as u32
            } else if c.is_ascii_digit() {
                c as u32
            } else {
                match c {
                    ';' | ':' => 186,
                    '=' | '+' => 187,
                    ',' | '<' => 188,
                    '-' | '_' => 189,
                    '.' | '>' => 190,
                    '/' | '?' => 191,
                    '`' | '~' => 192,
                    '[' | '{' => 219,
                    '\\' | '|' => 220,
                    ']' | '}' => 221,
                    '\'' | '"' => 222,
                    _ => 0,
                }
            }
        }
        k => {
            let n = k.strip_prefix('F')?.parse::<u32>().ok()?;
            if (1..=24).contains(&n) {
                111 + n
            } else {
                return None;
            }
        }
    })
}

/// The text a key types when the client didn't say: printable keys
/// without Ctrl/Meta/Alt, and Enter.
pub fn implied_text(key: &str, modifiers: u32) -> Option<String> {
    if key == "Enter" {
        return Some("\r".into());
    }
    if modifiers & (CTRL | META | ALT) != 0 || key.chars().count() != 1 {
        return None;
    }
    Some(key.to_string())
}

/// `Input.dispatchKeyEvent` parameters.
pub fn event(down: bool, key: &str, code: &str, text: Option<&str>, modifiers: u32) -> Value {
    let vk = key_code(key, code);
    let text = text.filter(|t| !t.is_empty() && down);
    let mut ev = json!({
        "type": match (down, text.is_some()) {
            (false, _) => "keyUp",
            (true, true) => "keyDown",
            (true, false) => "rawKeyDown",
        },
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
        "modifiers": modifiers & 0xf,
    });
    if let Some(t) = text {
        ev["text"] = json!(t);
        ev["unmodifiedText"] = json!(t);
    }
    if code.starts_with("Numpad") {
        ev["location"] = json!(3);
    }
    ev
}

/// A key the agent named (`Enter`, `ctrl+a`, `Shift+Tab`, `ArrowDown`, `a`):
/// (key, code, modifiers).
pub fn parse_combo(spec: &str) -> Result<(String, String, u32), String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("name a key, e.g. `Enter`, `Tab`, `Escape`, `ArrowDown` or `Control+a`".into());
    }
    let parts: Vec<&str> = if spec == "+" {
        vec!["+"]
    } else {
        spec.split('+').map(str::trim).collect()
    };
    let (last, mods) = parts.split_last().ok_or("empty key")?;
    let mut modifiers = 0;
    for m in mods {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => CTRL,
            "shift" => SHIFT,
            "alt" | "option" | "opt" => ALT,
            "meta" | "cmd" | "command" | "super" | "win" => META,
            other => return Err(format!("unknown modifier `{other}`")),
        };
    }
    let lower = last.to_ascii_lowercase();
    let key = match lower.as_str() {
        "enter" | "return" => "Enter",
        "tab" => "Tab",
        "esc" | "escape" => "Escape",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "space" | " " => " ",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "insert" => "Insert",
        _ if last.chars().count() == 1 => last,
        _ if lower.starts_with('f')
            && lower[1..]
                .parse::<u32>()
                .is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            return Ok((
                last.to_ascii_uppercase(),
                last.to_ascii_uppercase(),
                modifiers,
            ));
        }
        _ => return Err(format!("unknown key `{last}`")),
    };
    let code = match key {
        " " => "Space".to_string(),
        k if k.chars().count() == 1 => {
            let c = k.chars().next().unwrap_or(' ');
            if c.is_ascii_alphabetic() {
                format!("Key{}", c.to_ascii_uppercase())
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                String::new()
            }
        }
        k => k.to_string(),
    };
    // Shift+letter types the capital.
    let key = if modifiers & SHIFT != 0 && key.len() == 1 {
        key.to_ascii_uppercase()
    } else {
        key.to_string()
    };
    Ok((key, code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn special_keys_get_their_codes_and_text() {
        assert_eq!(key_code("Backspace", "Backspace"), 8);
        assert_eq!(key_code("a", "KeyA"), 65);
        assert_eq!(key_code("A", ""), 65);
        assert_eq!(key_code("!", "Digit1"), 49);
        assert_eq!(key_code("ArrowLeft", "ArrowLeft"), 37);
        assert_eq!(key_code("F5", "F5"), 116);
        assert_eq!(key_code(".", "NumpadDecimal"), 110);
        let e = event(true, "Enter", "Enter", Some("\r"), 0);
        assert_eq!(e["type"], "keyDown");
        assert_eq!(e["windowsVirtualKeyCode"], 13);
        assert_eq!(event(true, "Tab", "Tab", None, SHIFT)["type"], "rawKeyDown");
        assert_eq!(event(false, "a", "KeyA", Some("a"), 0)["type"], "keyUp");
        assert!(
            event(false, "a", "KeyA", Some("a"), 0)
                .get("text")
                .is_none()
        );
        assert_eq!(implied_text("a", 0).as_deref(), Some("a"));
        assert_eq!(implied_text("a", CTRL), None);
        assert_eq!(implied_text("Enter", 0).as_deref(), Some("\r"));
        assert_eq!(implied_text("ArrowUp", 0), None);
    }

    #[test]
    fn agent_key_names_parse() {
        assert_eq!(
            parse_combo("Enter").unwrap(),
            ("Enter".into(), "Enter".into(), 0)
        );
        assert_eq!(
            parse_combo("ctrl+a").unwrap(),
            ("a".into(), "KeyA".into(), CTRL)
        );
        assert_eq!(
            parse_combo("Shift+Tab").unwrap(),
            ("Tab".into(), "Tab".into(), SHIFT)
        );
        assert_eq!(parse_combo("esc").unwrap().0, "Escape");
        assert_eq!(parse_combo("PageDown").unwrap().0, "PageDown");
        assert_eq!(parse_combo("f5").unwrap().0, "F5");
        assert_eq!(parse_combo("+").unwrap().0, "+");
        assert!(parse_combo("Hyper+x").is_err());
        assert!(parse_combo("Banana").is_err());
    }
}
