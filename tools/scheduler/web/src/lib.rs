//! The scheduler's browser and desktop UI: the same Elm loop the TUI runs,
//! rendered with Dioxus as components the site shell mounts on its routes.

mod bridge;
mod demo;
mod live;
mod persist;
mod views;

pub use demo::DemoTool;
use dioxus::prelude::*;
pub use live::SchedulerTool;

/// The desk stylesheet, linked by each tool component.
const DESK_CSS: Asset = asset!("/assets/desk.css");
