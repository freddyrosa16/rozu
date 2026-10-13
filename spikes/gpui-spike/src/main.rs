//! Throwaway GPUI spike: a window with a list of streamed text lines (simulated tokens).
use gpui::{
    App, Application, Bounds, Context, SharedString, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use std::time::Duration;

struct Stream {
    lines: Vec<SharedString>,
}

impl Render for Stream {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x111318))
            .text_color(rgb(0xe6e6e6))
            .font_family("Menlo")
            .text_sm()
            .p_2()
            .overflow_y_scroll()
            .children(self.lines.iter().cloned().map(|l| div().child(l)))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(420.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |_, cx| {
                cx.new(|cx| {
                    cx.spawn(async move |this, cx| {
                        let mut i = 0u32;
                        loop {
                            cx.background_executor().timer(Duration::from_millis(16)).await;
                            i += 1;
                            let ok = this.update(cx, |s: &mut Stream, cx| {
                                s.lines.push(format!("token #{i}: streamed text line").into());
                                cx.notify();
                            });
                            if ok.is_err() || i > 2000 { break; }
                        }
                    })
                    .detach();
                    Stream { lines: vec!["GPUI spike — streaming…".into()] }
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
