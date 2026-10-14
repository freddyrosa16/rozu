//! Just enough Markdown for the agent's replies: paragraphs, headings, bullet and numbered
//! lists, fenced code, and `code` / **bold** spans.

use std::ops::Range;

#[derive(Debug, PartialEq)]
pub enum Block {
    Para(Inline),
    Heading(Inline),
    /// A list item with its marker (`•` or `3.`).
    Item(String, Inline),
    Code(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub code: bool,
    pub bold: bool,
}

/// Text with the markers removed, and the styled ranges: sorted and never overlapping.
#[derive(Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub spans: Vec<(Range<usize>, Style)>,
}

pub fn parse(src: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    let flush = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            blocks.push(Block::Para(inline(&para.join(" "))));
            para.clear();
        }
    };
    for line in src.lines() {
        let t = line.trim();
        if let Some(lines) = &mut code {
            if t.starts_with("```") {
                blocks.push(Block::Code(lines.join("\n")));
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        if t.starts_with("```") {
            flush(&mut para, &mut blocks);
            code = Some(Vec::new());
        } else if t.is_empty() {
            flush(&mut para, &mut blocks);
        } else if let Some(h) = t.strip_prefix('#') {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Heading(inline(h.trim_start_matches('#').trim())));
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Item("•".into(), inline(rest)));
        } else if let Some((n, rest)) = t.split_once(". ")
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
        {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Item(format!("{n}."), inline(rest)));
        } else {
            para.push(t);
        }
    }
    if let Some(lines) = code {
        blocks.push(Block::Code(lines.join("\n")));
    }
    flush(&mut para, &mut blocks);
    blocks
}

fn inline(src: &str) -> Inline {
    let nested = marks(src);
    let mut cuts: Vec<usize> = nested
        .spans
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut spans: Vec<(Range<usize>, Style)> = Vec::new();
    for w in cuts.windows(2) {
        let mut style = Style::default();
        for (_, s) in nested
            .spans
            .iter()
            .filter(|(r, _)| r.start <= w[0] && w[1] <= r.end)
        {
            style.code |= s.code;
            style.bold |= s.bold;
        }
        match spans.last_mut() {
            Some((r, s)) if r.end == w[0] && *s == style => r.end = w[1],
            _ if style != Style::default() => spans.push((w[0]..w[1], style)),
            _ => {}
        }
    }
    Inline {
        text: nested.text,
        spans,
    }
}

/// `inline` before flattening: one range per marker pair, nested ones included.
fn marks(src: &str) -> Inline {
    let code = Style {
        code: true,
        bold: false,
    };
    let bold = Style {
        code: false,
        bold: true,
    };
    let mut out = Inline::default();
    let mut rest = src;
    while !rest.is_empty() {
        let next = [("`", code), ("**", bold)]
            .into_iter()
            .filter_map(|(m, s)| {
                let start = rest.find(m)?;
                let end = rest[start + m.len()..].find(m)? + start + m.len();
                Some((start, end, m.len(), s))
            })
            .min_by_key(|&(start, ..)| start);
        let Some((start, end, len, span)) = next else {
            out.text.push_str(rest);
            break;
        };
        out.text.push_str(&rest[..start]);
        let inner = marks(&rest[start + len..end]);
        let at = out.text.len();
        out.text.push_str(&inner.text);
        out.spans.push((at..out.text.len(), span));
        out.spans.extend(
            inner
                .spans
                .into_iter()
                .map(|(r, s)| (r.start + at..r.end + at, s)),
        );
        rest = &rest[end + len..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_and_spans() {
        let blocks = parse(
            "Your vault is at **`/Users/f/Dev Labs`**.\nIt has notes.\n\n## Next\n- one `a`\n2. two\n```sh\nls -la\n```",
        );
        let Block::Para(p) = &blocks[0] else {
            panic!("{blocks:?}")
        };
        assert_eq!(p.text, "Your vault is at /Users/f/Dev Labs. It has notes.");
        let both = Style {
            code: true,
            bold: true,
        };
        assert_eq!(p.spans, [(17..34, both)]);
        let mixed = inline("**a `b`** c");
        assert_eq!(mixed.text, "a b c");
        assert_eq!(
            mixed.spans,
            [
                (
                    0..2,
                    Style {
                        code: false,
                        bold: true
                    }
                ),
                (2..3, both)
            ]
        );
        assert_eq!(blocks[1], Block::Heading(inline("Next")));
        assert!(matches!(&blocks[2], Block::Item(m, i) if m == "•" && i.text == "one a"));
        assert!(matches!(&blocks[3], Block::Item(m, _) if m == "2."));
        assert_eq!(blocks[4], Block::Code("ls -la".into()));
    }

    #[test]
    fn unclosed_markers_stay_text() {
        assert_eq!(inline("a `b").text, "a `b");
        assert_eq!(parse("```\nx").last(), Some(&Block::Code("x".into())));
    }
}
