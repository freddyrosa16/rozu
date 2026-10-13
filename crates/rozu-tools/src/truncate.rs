//! Head + tail truncation for tool output, so one huge result can't flood the context window.

use std::borrow::Cow;

/// Default cap for a single tool result sent back to the model.
pub const MAX_OUTPUT_BYTES: usize = 30 * 1024;

/// Keeps the first and last `max_bytes / 2` bytes of `text` (on UTF-8 boundaries) with a marker
/// saying how much was cut. Returns `text` unchanged when it already fits.
pub fn truncate_head_tail(text: &str, max_bytes: usize) -> Cow<'_, str> {
    if text.len() <= max_bytes {
        return Cow::Borrowed(text);
    }
    let half = max_bytes / 2;
    let head_end = floor_char_boundary(text, half);
    let tail_start = ceil_char_boundary(text, text.len() - half);
    let omitted = tail_start - head_end;
    Cow::Owned(format!(
        "{}\n\n[… {omitted} bytes truncated …]\n\n{}",
        &text[..head_end],
        &text[tail_start..]
    ))
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_untouched() {
        assert!(matches!(
            truncate_head_tail("hello", 10),
            Cow::Borrowed("hello")
        ));
        assert!(matches!(truncate_head_tail("", 0), Cow::Borrowed("")));
    }

    #[test]
    fn long_text_keeps_head_and_tail() {
        let text = format!("{}{}{}", "a".repeat(100), "m".repeat(1000), "z".repeat(100));
        let out = truncate_head_tail(&text, 200);
        assert!(out.starts_with(&"a".repeat(100)), "{out}");
        assert!(out.ends_with(&"z".repeat(100)), "{out}");
        assert!(out.contains("[… 1000 bytes truncated …]"), "{out}");
        assert!(!out.contains('m'));
    }

    #[test]
    fn respects_utf8_boundaries() {
        // 'é' is two bytes, so a 5-byte half lands mid-character.
        let text = "é".repeat(50);
        let out = truncate_head_tail(&text, 11);
        assert!(out.starts_with("éé"));
        assert!(out.ends_with("éé"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn output_is_bounded() {
        let text = "x".repeat(MAX_OUTPUT_BYTES * 4);
        let out = truncate_head_tail(&text, MAX_OUTPUT_BYTES);
        assert!(out.len() < MAX_OUTPUT_BYTES + 64, "{}", out.len());
    }
}
