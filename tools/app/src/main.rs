//! The browser and desktop shell over the scheduler core: the same Elm
//! loop the TUI runs, rendered with Dioxus.

mod bridge;
mod demo;
mod views;

use dioxus::prelude::*;

use crate::{bridge::Session, demo::boot_demo, views::Desk};

const MAIN_CSS: Asset = asset!("/assets/main.css");

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let session = use_resource(boot_demo);
    let booted: Option<Result<Session, String>> = session.read().clone();
    rsx! {
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        header {
            h1 { "bracket-tools" }
            span { class: "mode", "demo world" }
            if let Some(Ok(session)) = booted.clone() {
                button { class: "quiet", onclick: move |_| session.dispatch(bracket_tools_scheduler_core::ui_action::UiAction::Undo), "undo" }
            }
        }
        match booted {
            Some(Ok(session)) => rsx! { Desk { session } },
            Some(Err(error)) => rsx! { div { class: "failed", "the demo world failed to boot: {error}" } },
            None => rsx! { div { class: "loading", "building the demo world…" } },
        }
    }
}
