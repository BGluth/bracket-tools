//! The tools site: a shell with a home page, the settings page, and one
//! route per tool component.

mod home;
mod settings;

use bracket_tools_scheduler_web::{DemoTool, SchedulerTool};
use dioxus::prelude::*;

use crate::{
    home::Home,
    settings::{provide_settings, use_settings, Settings},
};

const MAIN_CSS: Asset = asset!("/assets/main.css");

fn main() {
    dioxus::launch(App);
}

#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[layout(Shell)]
    #[route("/")]
    Home {},
    #[route("/scheduler")]
    Scheduler {},
    #[route("/scheduler/demo")]
    SchedulerDemo {},
    #[route("/settings")]
    Settings {},
}

#[component]
fn App() -> Element {
    rsx! {
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        Router::<Route> {}
    }
}

/// The header and navigation around every page; owns the site settings.
#[component]
fn Shell() -> Element {
    provide_settings();
    rsx! {
        header {
            h1 { Link { to: Route::Home {}, "bracket-tools" } }
            nav {
                Link { to: Route::Scheduler {}, active_class: "active", "scheduler" }
                Link { to: Route::SchedulerDemo {}, active_class: "active", "demo" }
                Link { to: Route::Settings {}, active_class: "active", "settings" }
            }
        }
        Outlet::<Route> {}
    }
}

#[component]
fn Scheduler() -> Element {
    let settings = use_settings();
    rsx! { SchedulerTool { token: settings.token() } }
}

#[component]
fn SchedulerDemo() -> Element {
    rsx! { DemoTool {} }
}
