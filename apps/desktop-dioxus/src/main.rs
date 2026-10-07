// Release builds run without a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use dioxus::desktop::{Config, WindowBuilder};
use dioxus::prelude::*;

const STYLE: &str = r#"
    :root { color-scheme: light dark; font-family: system-ui, sans-serif; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; }
    main { text-align: center; max-width: 32rem; padding: 1rem; }
    p { opacity: .75; }
"#;

fn main() {
    let window = WindowBuilder::new()
        .with_title("Orgtree (Dioxus spike)")
        .with_inner_size(dioxus::desktop::LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(dioxus::desktop::LogicalSize::new(640.0, 480.0));

    dioxus::LaunchBuilder::desktop()
        .with_cfg(Config::new().with_window(window))
        .launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        style { {STYLE} }
        main {
            h1 { "Orgtree" }
            p { "Spike de Dioxus: ventana base. La conexión con el motor llega en los issues #9 y #10." }
        }
    }
}
