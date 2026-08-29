mod app;
mod config;
mod keymap;
mod prompt;
mod terminal;
mod tree;

use gpui::{App, Application};

use crate::keymap::actions::Quit;

fn main() {
    let (config, config_error) = config::load();

    Application::new().run(move |cx: &mut App| {
        cx.bind_keys(keymap::default::bindings());
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        app::open_oxide_window(config, config_error, None, cx);
    });
}
