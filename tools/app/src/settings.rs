//! Site settings: the start.gg token, shared with the tools through context
//! and persisted in the browser's localStorage (memory only on native).

use dioxus::prelude::*;

const TOKEN_KEY: &str = "startgg_token";

/// The settings every page can reach: a copyable handle onto context signals.
#[derive(Clone, Copy)]
pub struct SiteSettings {
    token: Signal<Option<String>>,
}

impl SiteSettings {
    pub fn token(&self) -> ReadSignal<Option<String>> {
        self.token.into()
    }

    fn set_token(&mut self, token: Option<String>) {
        storage_write(TOKEN_KEY, token.as_deref());
        self.token.set(token);
    }
}

/// Loads the settings once and provides them to the subtree.
pub fn provide_settings() -> SiteSettings {
    use_context_provider(|| SiteSettings {
        token: Signal::new(storage_read(TOKEN_KEY)),
    })
}

pub fn use_settings() -> SiteSettings {
    use_context()
}

#[component]
pub fn Settings() -> Element {
    let mut settings = use_settings();
    let mut draft = use_signal(|| settings.token.read().clone().unwrap_or_default());
    let saved = settings.token.read().is_some();
    rsx! {
        main { class: "page",
            h2 { "Settings" }
            label { class: "field",
                "start.gg token"
                input {
                    r#type: "password",
                    placeholder: "paste a personal access token",
                    value: "{draft}",
                    oninput: move |event| draft.set(event.value()),
                }
            }
            div { class: "row",
                button { onclick: move |_| settings.set_token(non_empty(draft())), "save" }
                button {
                    class: "quiet",
                    onclick: move |_| {
                        draft.set(String::new());
                        settings.set_token(None);
                    },
                    "clear"
                }
                span { class: "dim", if saved { "a token is saved in this browser" } else { "no token saved" } }
            }
            p { class: "dim", "The token stays in this browser; only the tools send it, and only to start.gg." }
        }
    }
}

fn non_empty(text: String) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(target_arch = "wasm32")]
fn storage_read(key: &str) -> Option<String> {
    local_storage()?.get_item(key).ok().flatten()
}

#[cfg(target_arch = "wasm32")]
fn storage_write(key: &str, value: Option<&str>) {
    let Some(storage) = local_storage() else { return };
    let _ = match value {
        Some(value) => storage.set_item(key, value),
        None => storage.remove_item(key),
    };
}

#[cfg(target_arch = "wasm32")]
fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(not(target_arch = "wasm32"))]
fn storage_read(_key: &str) -> Option<String> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn storage_write(_key: &str, _value: Option<&str>) {}
