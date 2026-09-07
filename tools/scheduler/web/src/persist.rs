//! Browser-side persistence for the desk: the core's overlay and snapshot
//! documents (and the live form) as JSON strings in one IndexedDB object
//! store, keyed by world. Natively the store is in-memory only.

use std::rc::Rc;

use bracket_tools_scheduler_core::{
    app::{AppState, NoticeLevel},
    conflict::UnixMillis,
    state_doc::{OverlayDoc, SnapshotDoc, OVERLAY_VERSION, SNAPSHOT_VERSION},
    timers::now_millis,
};
use cfg_if::cfg_if;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// A document with the moment it was written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saved<T> {
    pub saved_at: UnixMillis,
    pub doc: T,
}

/// Outcome of loading a saved document.
pub enum Load<T> {
    /// Nothing saved yet.
    None,
    Loaded(Saved<T>),
    /// The saved document was corrupt or from another version; it moved to
    /// the `.bak` key and a fresh session begins.
    Recovered,
}

/// The saved documents of one world and where they live.
pub struct DeskPersistence {
    store: Rc<DeskStore>,
    overlay_key: String,
    snapshot_key: String,
}

impl DeskPersistence {
    pub fn new(store: Rc<DeskStore>, world: &str) -> Self {
        Self {
            store,
            overlay_key: format!("overlay:{world}"),
            snapshot_key: format!("snapshot:{world}"),
        }
    }

    pub async fn load_overlay(&self) -> Result<Load<OverlayDoc>, String> {
        self.load_versioned(&self.overlay_key, |doc: &OverlayDoc| doc.version, OVERLAY_VERSION)
            .await
    }

    pub async fn load_snapshot(&self) -> Result<Load<SnapshotDoc>, String> {
        self.load_versioned(&self.snapshot_key, |doc: &SnapshotDoc| doc.version, SNAPSHOT_VERSION)
            .await
    }

    pub async fn save_overlay(&self, doc: &OverlayDoc) -> Result<(), String> {
        self.store.save(&self.overlay_key, &stamped(doc)).await
    }

    pub async fn save_snapshot(&self, doc: &SnapshotDoc) -> Result<(), String> {
        self.store.save(&self.snapshot_key, &stamped(doc)).await
    }

    /// Drops both documents: the operator wants a fresh world.
    pub async fn forget(&self) -> Result<(), String> {
        self.store.remove(&self.overlay_key).await?;
        self.store.remove(&self.snapshot_key).await
    }

    async fn load_versioned<T: DeserializeOwned>(
        &self,
        key: &str,
        version_of: impl Fn(&T) -> u32,
        expected: u32,
    ) -> Result<Load<T>, String> {
        let Some(raw) = self.store.load_raw(key).await? else {
            return Ok(Load::None);
        };
        match serde_json::from_str::<Saved<T>>(&raw) {
            Ok(saved) if version_of(&saved.doc) == expected => Ok(Load::Loaded(saved)),
            _ => {
                self.store.save_raw(&format!("{key}.bak"), &raw).await?;
                self.store.remove(key).await?;
                Ok(Load::Recovered)
            }
        }
    }
}

/// What the store holds for a world, read ahead of opening its desk.
pub enum SavedState {
    None,
    Found(Box<Saved<OverlayDoc>>),
    Corrupt,
    Unavailable(String),
}

pub async fn peek_saved(persistence: &DeskPersistence) -> SavedState {
    match persistence.load_overlay().await {
        Ok(Load::Loaded(saved)) => SavedState::Found(Box::new(saved)),
        Ok(Load::None) => SavedState::None,
        Ok(Load::Recovered) => SavedState::Corrupt,
        Err(error) => SavedState::Unavailable(error),
    }
}

/// Rehydrates a found overlay over the fresh `state` (its board wins) and
/// tells the operator what happened; an unavailable store raises the badge.
pub fn apply_saved(state: &mut AppState, saved: SavedState, now: UnixMillis) {
    match saved {
        SavedState::Found(saved) => {
            let Saved { saved_at, doc } = *saved;
            state.apply_overlay(doc, now, true);
            let text = format!("restored the desk state saved {}m ago", age_minutes(saved_at, now));
            state.notice(now, NoticeLevel::Info, text);
        }
        SavedState::Corrupt => {
            let text = "saved desk state was corrupt or from another version; it was set aside and this desk starts fresh";
            state.notice(now, NoticeLevel::Warn, text);
        }
        SavedState::Unavailable(error) => {
            state.persist_failed = true;
            state.notice(
                now,
                NoticeLevel::Error,
                format!("browser storage unavailable, nothing will be saved: {error}"),
            );
        }
        SavedState::None => {}
    }
}

pub fn age_minutes(at: UnixMillis, now: UnixMillis) -> i64 {
    (now - at).max(0) / 60_000
}

fn stamped<T>(doc: &T) -> Saved<&T> {
    Saved {
        saved_at: now_millis(),
        doc,
    }
}

impl DeskStore {
    pub async fn load<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, String> {
        match self.load_raw(key).await? {
            Some(raw) => serde_json::from_str(&raw).map(Some).map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }

    pub async fn save<T: Serialize>(&self, key: &str, value: &T) -> Result<(), String> {
        let raw = serde_json::to_string(value).map_err(|e| e.to_string())?;
        self.save_raw(key, &raw).await
    }
}

cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        use idb::{Database, ObjectStore, TransactionMode};
        use wasm_bindgen::JsValue;

        const DB_NAME: &str = "bracket-tools";
        const STORE: &str = "desk";

        /// One IndexedDB object store of JSON strings keyed by string.
        pub struct DeskStore {
            db: Database,
        }

        impl DeskStore {
            pub async fn open() -> Result<Rc<Self>, String> {
                let db = Database::builder(DB_NAME)
                    .version(1)
                    .add_object_store(ObjectStore::builder(STORE))
                    .build()
                    .await
                    .map_err(text)?;
                Ok(Rc::new(Self { db }))
            }

            pub async fn remove(&self, key: &str) -> Result<(), String> {
                let tx = self.db.transaction(&[STORE], TransactionMode::ReadWrite).map_err(text)?;
                tx.object_store(STORE).map_err(text)?.delete(JsValue::from_str(key)).map_err(text)?.await.map_err(text)?;
                tx.commit().map_err(text)?.await.map_err(text)?;
                Ok(())
            }

            async fn load_raw(&self, key: &str) -> Result<Option<String>, String> {
                let tx = self.db.transaction(&[STORE], TransactionMode::ReadOnly).map_err(text)?;
                let value = tx.object_store(STORE).map_err(text)?.get(JsValue::from_str(key)).map_err(text)?.await.map_err(text)?;
                tx.await.map_err(text)?;
                Ok(value.and_then(|v| v.as_string()))
            }

            async fn save_raw(&self, key: &str, raw: &str) -> Result<(), String> {
                let tx = self.db.transaction(&[STORE], TransactionMode::ReadWrite).map_err(text)?;
                tx.object_store(STORE)
                    .map_err(text)?
                    .put(&JsValue::from_str(raw), Some(&JsValue::from_str(key)))
                    .map_err(text)?
                    .await
                    .map_err(text)?;
                tx.commit().map_err(text)?.await.map_err(text)?;
                Ok(())
            }
        }

        fn text(error: idb::Error) -> String {
            error.to_string()
        }
    } else {
        use std::{cell::RefCell, collections::HashMap};

        /// The desktop shell has no document store yet; this keeps documents
        /// for the life of the process only.
        pub struct DeskStore {
            docs: RefCell<HashMap<String, String>>,
        }

        impl DeskStore {
            pub async fn open() -> Result<Rc<Self>, String> {
                Ok(Rc::new(Self { docs: RefCell::default() }))
            }

            pub async fn remove(&self, key: &str) -> Result<(), String> {
                self.docs.borrow_mut().remove(key);
                Ok(())
            }

            async fn load_raw(&self, key: &str) -> Result<Option<String>, String> {
                Ok(self.docs.borrow().get(key).cloned())
            }

            async fn save_raw(&self, key: &str, raw: &str) -> Result<(), String> {
                self.docs.borrow_mut().insert(key.to_owned(), raw.to_owned());
                Ok(())
            }
        }
    }
}
