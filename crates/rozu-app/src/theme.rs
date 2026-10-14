//! The mockup's colors and icons (`docs/design/app/index.html`).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

pub const BG: u32 = 0x181818;
pub const SIDE: u32 = 0x1B1B1B;
pub const RAISE: u32 = 0x262626;
pub const BOX: u32 = 0x1F1F1F;
pub const LINE: u32 = 0x2E2E2E;
pub const INK: u32 = 0xF2F2F2;
pub const INK2: u32 = 0xBDBDBD;
pub const INK3: u32 = 0x7E7E7E;
pub const BAD: u32 = 0xF87171;

const ICONS: &[(&str, &str)] = &[
    ("chat", r#"<path d="M4 4h16l-6.5 8v6l-3 2v-8z"/>"#),
    (
        "search",
        r#"<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>"#,
    ),
    (
        "bot",
        r#"<rect x="4" y="8" width="16" height="12" rx="2"/><path d="M12 8V4M9 14h.01M15 14h.01"/>"#,
    ),
    (
        "blocks",
        r#"<rect x="4" y="10" width="10" height="10" rx="1"/><rect x="15" y="4" width="5" height="5" rx="1"/><path d="M9 10V4h4"/>"#,
    ),
    ("plus", r#"<path d="M12 5v14M5 12h14"/>"#),
    (
        "folder",
        r#"<path d="M3.5 7a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>"#,
    ),
    ("down", r#"<path d="m6 9 6 6 6-6"/>"#),
    ("right", r#"<path d="m9 6 6 6-6 6"/>"#),
    ("up", r#"<path d="M12 19V5M6 11l6-6 6 6"/>"#),
    (
        "stop",
        r#"<rect x="7" y="7" width="10" height="10" rx="1.5" fill="black"/>"#,
    ),
    (
        "gear",
        r#"<circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"/>"#,
    ),
    (
        "plan",
        r#"<circle cx="6" cy="7" r="2"/><circle cx="6" cy="17" r="2"/><path d="M11 7h9M11 17h9"/>"#,
    ),
    (
        "bug",
        r#"<rect x="8" y="7" width="8" height="13" rx="4"/><path d="M12 11v9M8 12H4M20 12h-4M8 17H5M19 17h-3M9 7 7 4M15 7l2-3"/>"#,
    ),
    (
        "test",
        r#"<path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 1.8 3h10.4a2 2 0 0 0 1.8-3l-5-9V3M7.5 15h9"/>"#,
    ),
    ("diff", r#"<path d="M12 4v8M8 8h8M7 19h10"/>"#),
    (
        "trash",
        r#"<path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/>"#,
    ),
    ("check", r#"<path d="m5 12 5 5 9-10"/>"#),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(name, _)| *name == path).map(|(_, body)| {
            Cow::Owned(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"#
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, _: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS.iter().map(|(name, _)| (*name).into()).collect())
    }
}
