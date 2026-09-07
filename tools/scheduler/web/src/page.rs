//! Document-level listeners the desk needs from the page: the keyboard
//! (resolved through the core's keymap, so the terminal's keys work here)
//! and a save flush when the page hides or unloads.

use bracket_tools_scheduler_core::keymap::Key;
use dioxus::prelude::*;
use serde::Deserialize;

use crate::bridge::Session;

/// Runs until the Rust side sends anything, then removes its listeners.
/// Keys typed into fields stay with the field; Enter/Space on a focused
/// button stay a click; browser chords (Ctrl/Alt/Meta, Tab) pass through.
const LISTENERS: &str = r#"
const named = ["Enter", "Escape", "Backspace", "ArrowUp", "ArrowDown", "PageUp", "PageDown"];
const key = (event) => {
  const target = event.target;
  const tag = target && target.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || (target && target.isContentEditable)) return;
  if (tag === "BUTTON" && (event.key === "Enter" || event.key === " ")) return;
  if (event.ctrlKey || event.altKey || event.metaKey || event.key === "Tab") return;
  if (!named.includes(event.key) && event.key.length !== 1) return;
  if (event.key !== "PageUp" && event.key !== "PageDown") event.preventDefault();
  dioxus.send({ kind: "key", key: event.key });
};
const flush = () => dioxus.send({ kind: "flush" });
const hidden = () => { if (document.visibilityState === "hidden") flush(); };
document.addEventListener("keydown", key);
window.addEventListener("pagehide", flush);
document.addEventListener("visibilitychange", hidden);
try {
  await dioxus.recv();
} finally {
  document.removeEventListener("keydown", key);
  window.removeEventListener("pagehide", flush);
  document.removeEventListener("visibilitychange", hidden);
}
"#;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum PageEvent {
    Key { key: String },
    Flush,
}

/// Routes the page's keys and hide/unload events to the session for as
/// long as the calling component lives.
pub fn use_page_events(session: Session) {
    let mut eval = use_hook(|| document::eval(LISTENERS));
    use_future(move || {
        let session = session.clone();
        async move {
            while let Ok(event) = eval.recv::<PageEvent>().await {
                match event {
                    PageEvent::Key { key } => {
                        if let Some(key) = desk_key(&key) {
                            session.send_key(key);
                        }
                    }
                    PageEvent::Flush => session.flush(),
                }
            }
        }
    });
    use_drop(move || {
        let _ = eval.send(());
    });
}

/// A DOM `KeyboardEvent.key` as the desk's key; modifier and function keys
/// are `None` (the keymap's `Other` would dismiss a list modal).
fn desk_key(key: &str) -> Option<Key> {
    Some(match key {
        "Enter" => Key::Enter,
        "Escape" => Key::Esc,
        "Backspace" => Key::Backspace,
        "ArrowUp" => Key::Up,
        "ArrowDown" => Key::Down,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        _ => {
            let mut chars = key.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use bracket_tools_scheduler_core::keymap::Key;

    use super::desk_key;

    #[test]
    fn dom_keys_map_to_desk_keys_or_nothing() {
        assert_eq!(desk_key("Enter"), Some(Key::Enter));
        assert_eq!(desk_key("Escape"), Some(Key::Esc));
        assert_eq!(desk_key("ArrowDown"), Some(Key::Down));
        assert_eq!(desk_key("1"), Some(Key::Char('1')));
        assert_eq!(desk_key("?"), Some(Key::Char('?')));
        assert_eq!(desk_key("Shift"), None);
        assert_eq!(desk_key("F5"), None);
        assert_eq!(desk_key(""), None);
    }
}
