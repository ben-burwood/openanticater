//! Anticater control GUI — process bootstrap.
//!
//! For now this is just a basic GPUI window with no real functionality: it
//! stands the tech stack up end to end (workspace → core → UI → a rendered
//! window) so device wiring can be layered on next.

// Keep the console attached in debug builds so logs stay visible; hide it in
// release so launching the app doesn't pop a terminal behind the window.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use gpui::{
    Application, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, WindowOptions, div, rgb,
};

/// The root view. Holds only display state today.
struct AnticaterApp {
    title: SharedString,
}

impl Render for AnticaterApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .justify_center()
            .items_center()
            .gap_2()
            .bg(rgb(0x1e1e26))
            .text_color(rgb(0xf5f5f5))
            .child(div().text_2xl().child(self.title.clone()))
            .child(
                div()
                    .text_color(rgb(0x9aa0b4))
                    .child("Anticater knob control — skeleton"),
            )
    }
}

fn main() {
    println!("starting Anticater control GUI ({:?})", anticater_core::DeviceId::DEFAULT);

    Application::new().run(|cx| {
        cx.open_window(WindowOptions::default(), |_window, cx| {
            cx.new(|_cx| AnticaterApp {
                title: "Open Anticater".into(),
            })
        })
        .expect("failed to open window");

        cx.activate(true);
    });
}
