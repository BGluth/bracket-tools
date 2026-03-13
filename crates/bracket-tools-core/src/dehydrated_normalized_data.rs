use serde::{Deserialize, Serialize};

use crate::{
    data_types::{Hydratable, NormalizedOrigin},
    hydrated_normalized_data::{
        HydratedNormalizedBracket, HydratedNormalizedGame, HydratedNormalizedPlayer, HydratedNormalizedPlayerGameInfo,
        HydratedNormalizedSet, HydratedNormalizedTournament, NormalizedId,
    },
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DehydratedNormalizedTournament(NormalizedId);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DehydratedNormalizedBracket(NormalizedId);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DehydratedNormalizedSet(NormalizedId);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DehydratedNormalizedGame(NormalizedId);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DehydratedNormalizedPlayerGameInfo(NormalizedId);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DehydratedNormalizedPlayer(NormalizedId);

impl Hydratable for DehydratedNormalizedTournament {
    type Origin = NormalizedOrigin;
    type Hydrated = HydratedNormalizedTournament;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

impl Hydratable for DehydratedNormalizedBracket {
    type Origin = NormalizedOrigin;
    type Hydrated = HydratedNormalizedBracket;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

impl Hydratable for DehydratedNormalizedSet {
    type Origin = NormalizedOrigin;
    type Hydrated = HydratedNormalizedSet;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

impl Hydratable for DehydratedNormalizedGame {
    type Origin = NormalizedOrigin;
    type Hydrated = HydratedNormalizedGame;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

impl Hydratable for DehydratedNormalizedPlayerGameInfo {
    type Origin = NormalizedOrigin;
    type Hydrated = HydratedNormalizedPlayerGameInfo;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

impl Hydratable for DehydratedNormalizedPlayer {
    type Origin = NormalizedOrigin;
    type Hydrated = HydratedNormalizedPlayer;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}
