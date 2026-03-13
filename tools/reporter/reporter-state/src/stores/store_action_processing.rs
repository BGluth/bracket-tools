use super::{
    stats::TourneyStoreAction,
    store_utils::{Store, StoreData},
    tourney::TourneyStore,
};

#[derive(Clone, Debug)]
pub enum StoreAction {
    Tourney(TourneyStoreAction),
}

#[derive(Clone, Debug)]
pub struct StoreActionProcessingOutput {
    pub rerender_needed: bool,
}

/// To start with, we're going to keep this fairly monolithic and not worry about decoupling. We're going to try to keep this fairly opaque
/// and keep the interface stable though.
#[derive(Debug)]
pub struct Stores {
    pub tourney_store: StoreData<TourneyStore>,
}

impl Default for Stores {
    fn default() -> Self {
        Self::new()
    }
}

impl Stores {
    pub fn new() -> Self {
        Self {
            tourney_store: TourneyStore::new().into(),
        }
    }

    pub fn process_store_action(&mut self, action: StoreAction) -> StoreActionProcessingOutput {
        match action {
            StoreAction::Tourney(a) => self.tourney_store.update(|d| d.update(a)),
        };

        // For now, always assume that we need to redraw.
        StoreActionProcessingOutput { rerender_needed: true }
    }
}
