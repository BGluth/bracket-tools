//! The live desk over a start.gg token. The live flow (tournament pick,
//! preflight, board) is not built yet; this renders the token gate.

use dioxus::prelude::*;

use crate::DESK_CSS;

/// The scheduler run live against start.gg. `token` is the site's start.gg
/// token; `None` asks for one.
#[component]
pub fn SchedulerTool(token: ReadSignal<Option<String>>) -> Element {
    let text = if token.read().is_some() {
        "Live mode is not built yet. The demo runs without a token."
    } else {
        "The live desk needs a start.gg token. Add one in Settings."
    };
    rsx! {
        document::Link { rel: "stylesheet", href: DESK_CSS }
        div { class: "empty", {text} }
    }
}
