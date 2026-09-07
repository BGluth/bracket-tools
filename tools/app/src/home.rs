//! The home page: the tools on offer and whether a token is saved.

use dioxus::prelude::*;

use crate::{settings::use_settings, Route};

#[component]
pub fn Home() -> Element {
    let settings = use_settings();
    let token_text = if settings.token().read().is_some() {
        "A start.gg token is saved; the live tools can use it."
    } else {
        "No start.gg token yet; the live tools need one, the demo does not."
    };
    rsx! {
        main { class: "page",
            p { class: "dim", {token_text} }
            section { class: "cards",
                ToolCard {
                    to: Route::Scheduler {},
                    title: "Scheduler",
                    blurb: "Run the TO desk live against a start.gg tournament.",
                }
                ToolCard {
                    to: Route::SchedulerDemo {},
                    title: "Scheduler demo",
                    blurb: "The same desk over a synthetic tournament on a fast clock.",
                }
            }
        }
    }
}

#[component]
fn ToolCard(to: Route, title: String, blurb: String) -> Element {
    rsx! {
        Link { class: "card", to,
            h3 { {title} }
            p { {blurb} }
        }
    }
}
