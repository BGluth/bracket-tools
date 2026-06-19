use std::sync::Arc;

use bracket_tools_cache::storage::Storage;

use crate::{
    gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup, StartGgId},
    provider::GGProviderError,
    session::GgSession,
};

/// A lazy handle to a start.gg tournament.
///
/// Holds only an id and a shared [`GgSession`]; the underlying data is fetched
/// (and memoized) on [`get`](Self::get). Cloning is cheap — an id plus an `Arc`
/// bump — regardless of the storage backend `S`.
pub struct LazyTournament<S: Storage> {
    id: StartGgId,
    session: Arc<GgSession<S>>,
}

impl<S: Storage> LazyTournament<S> {
    pub(crate) fn new(id: StartGgId, session: Arc<GgSession<S>>) -> Self {
        Self { id, session }
    }

    pub fn id(&self) -> StartGgId {
        self.id
    }

    /// Resolves the tournament, hitting the in-memory, sled, then HTTP tiers in
    /// order. Repeated calls (and other handles for this id) share one `Arc`.
    pub async fn get(&self) -> Result<Arc<HydratedGgTournament>, GGProviderError> {
        self.session.tournament_value(self.id).await
    }

    /// Fetches the tournament, then returns one player handle per participant id.
    ///
    /// The handles are lazy: this performs the parent fetch only, not a fetch
    /// per participant.
    pub async fn participants(&self) -> Result<Vec<LazyPlayer<S>>, GGProviderError> {
        let tournament = self.get().await?;
        Ok(tournament.participant_ids.iter().map(|&id| self.session.player(id)).collect())
    }
}

/// A lazy handle to a start.gg set (and its games).
///
/// See [`LazyTournament`] for the handle model.
pub struct LazySet<S: Storage> {
    id: StartGgId,
    session: Arc<GgSession<S>>,
}

impl<S: Storage> LazySet<S> {
    pub(crate) fn new(id: StartGgId, session: Arc<GgSession<S>>) -> Self {
        Self { id, session }
    }

    pub fn id(&self) -> StartGgId {
        self.id
    }

    /// Resolves the set, hitting the in-memory, sled, then HTTP tiers in order.
    pub async fn get(&self) -> Result<Arc<HydratedGgSet>, GGProviderError> {
        self.session.set_value(self.id).await
    }

    /// Fetches the set, then returns a player handle per matchup slot.
    ///
    /// Empty when the set has no matchup (e.g. an unfilled bracket slot).
    pub async fn players(&self) -> Result<Vec<LazyPlayer<S>>, GGProviderError> {
        let set = self.get().await?;
        let player_ids = match &set.matchup {
            Some(Matchup::Singles { left, right }) => vec![left.player_id, right.player_id],
            None => Vec::new(),
        };
        Ok(player_ids.into_iter().map(|id| self.session.player(id)).collect())
    }
}

/// A lazy handle to a start.gg player.
///
/// See [`LazyTournament`] for the handle model.
pub struct LazyPlayer<S: Storage> {
    id: StartGgId,
    session: Arc<GgSession<S>>,
}

impl<S: Storage> LazyPlayer<S> {
    pub(crate) fn new(id: StartGgId, session: Arc<GgSession<S>>) -> Self {
        Self { id, session }
    }

    pub fn id(&self) -> StartGgId {
        self.id
    }

    /// Resolves the player, hitting the in-memory, sled, then HTTP tiers in order.
    pub async fn get(&self) -> Result<Arc<HydratedGgPlayer>, GGProviderError> {
        self.session.player_value(self.id).await
    }
}

// Manual `Clone` impls: deriving would add a spurious `S: Clone` bound, but a
// handle only ever clones an id and an `Arc`, so it stays cloneable for any
// storage backend.
macro_rules! impl_handle_clone {
    ($handle:ident) => {
        impl<S: Storage> Clone for $handle<S> {
            fn clone(&self) -> Self {
                Self {
                    id: self.id,
                    session: Arc::clone(&self.session),
                }
            }
        }
    };
}

impl_handle_clone!(LazyTournament);
impl_handle_clone!(LazySet);
impl_handle_clone!(LazyPlayer);
