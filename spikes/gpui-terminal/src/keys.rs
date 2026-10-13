//! Keystroke -> PTY byte mapping. Pure (no GPUI types) so it can be unit tested.

/// Translate a GPUI-style key name (`"enter"`, `"up"`, `"a"`…) plus modifiers into
/// the bytes an xterm-compatible terminal would send. `key_char` is the text the
/// keystroke would have typed (IME/layout aware), if any. `app_cursor` is DECCKM.
pub fn key_to_bytes(
    key: &str,
    key_char: Option<&str>,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let cursor = |c: u8| -> Vec<u8> {
        if app_cursor { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] }
    };
    let named: Option<Vec<u8>> = match key {
        "enter" => Some(b"\r".to_vec()),
        "backspace" => Some(vec![0x7f]),
        "tab" if shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(b"\t".to_vec()),
        "escape" => Some(vec![0x1b]),
        "up" => Some(cursor(b'A')),
        "down" => Some(cursor(b'B')),
        "right" => Some(cursor(b'C')),
        "left" => Some(cursor(b'D')),
        "home" => Some(cursor(b'H')),
        "end" => Some(cursor(b'F')),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        _ => None,
    };
    if let Some(mut bytes) = named {
        if alt {
            bytes.insert(0, 0x1b);
        }
        return Some(bytes);
    }

    if ctrl {
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let byte = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
                '@' | ' ' | '2' => Some(0),
                '[' | '3' => Some(0x1b),
                '\\' | '4' => Some(0x1c),
                ']' | '5' => Some(0x1d),
                '^' | '6' => Some(0x1e),
                '_' | '-' | '7' => Some(0x1f),
                '?' | '8' => Some(0x7f),
                _ => None,
            };
            if let Some(b) = byte {
                return Some(if alt { vec![0x1b, b] } else { vec![b] });
            }
        }
        return None;
    }

    // Printable text. With Option held, macOS gives a composed char (e.g. "ß") in
    // key_char; treat Option as Meta (ESC prefix + base key) like most terminals'
    // "option as meta" setting.
    if alt {
        if key == "space" {
            return Some(b"\x1b ".to_vec());
        }
        if key.chars().count() == 1 {
            let base = if shift { key.to_uppercase() } else { key.to_string() };
            let mut v = vec![0x1b];
            v.extend_from_slice(base.as_bytes());
            return Some(v);
        }
    }
    match key_char {
        Some(s) if !s.is_empty() => Some(s.as_bytes().to_vec()),
        _ if key == "space" => Some(b" ".to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::key_to_bytes;

    fn k(key: &str, ch: Option<&str>) -> Option<Vec<u8>> {
        key_to_bytes(key, ch, false, false, false, false)
    }

    #[test]
    fn printable() {
        assert_eq!(k("a", Some("a")), Some(b"a".to_vec()));
        assert_eq!(k("a", Some("A")), Some(b"A".to_vec()));
        assert_eq!(k("space", Some(" ")), Some(b" ".to_vec()));
        assert_eq!(k("e", Some("é")), Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn named_keys() {
        assert_eq!(k("enter", None), Some(b"\r".to_vec()));
        assert_eq!(k("backspace", None), Some(vec![0x7f]));
        assert_eq!(k("tab", None), Some(b"\t".to_vec()));
        assert_eq!(k("escape", None), Some(vec![0x1b]));
        assert_eq!(key_to_bytes("tab", None, false, false, true, false), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn arrows_respect_app_cursor_mode() {
        assert_eq!(k("up", None), Some(b"\x1b[A".to_vec()));
        assert_eq!(k("left", None), Some(b"\x1b[D".to_vec()));
        assert_eq!(key_to_bytes("up", None, false, false, false, true), Some(b"\x1bOA".to_vec()));
    }

    #[test]
    fn ctrl_letters() {
        assert_eq!(key_to_bytes("c", Some("c"), true, false, false, false), Some(vec![3]));
        assert_eq!(key_to_bytes("a", None, true, false, false, false), Some(vec![1]));
        assert_eq!(key_to_bytes("z", None, true, false, false, false), Some(vec![26]));
        assert_eq!(key_to_bytes("[", None, true, false, false, false), Some(vec![0x1b]));
    }

    #[test]
    fn alt_is_meta() {
        assert_eq!(key_to_bytes("b", Some("∫"), false, true, false, false), Some(b"\x1bb".to_vec()));
        assert_eq!(key_to_bytes("left", None, false, true, false, false), Some(b"\x1b\x1b[D".to_vec()));
    }

    #[test]
    fn unmapped() {
        assert_eq!(k("f13", None), None);
        assert_eq!(key_to_bytes("f1", None, true, false, false, false), None);
    }
}
