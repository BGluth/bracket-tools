//! Core data types that are needed to re-create all of the data scraped. Note that there are still other "intermediate" datatypes that are
//! created (and cached?) for ease-of-use.

use std::fmt::Debug;

use serde::{Deserialize, Serialize};

use crate::{
    data_types::{Dehydrateable, HydratableType, NormalizedOrigin},
    dehydrated_normalized_data::{
        DehydratedNormalizedBracket, DehydratedNormalizedGame, DehydratedNormalizedPlayer, DehydratedNormalizedPlayerGameInfo,
        DehydratedNormalizedSet, DehydratedNormalizedTournament,
    },
};

pub type NormalizedId = u64;

pub type NormalizedTournament = HydratableType<DehydratedNormalizedTournament, HydratedNormalizedTournament>;
pub type NormalizedBracket = HydratableType<DehydratedNormalizedBracket, HydratedNormalizedBracket>;
pub type NormalizedSet = HydratableType<DehydratedNormalizedSet, HydratedNormalizedSet>;
pub type NormalizedGame = HydratableType<DehydratedNormalizedGame, HydratedNormalizedGame>;
pub type NormalizedPlayerGameInfo = HydratableType<DehydratedNormalizedPlayerGameInfo, HydratedNormalizedPlayerGameInfo>;
pub type NormalizedPlayer = HydratableType<DehydratedNormalizedPlayer, HydratedNormalizedPlayer>;

// TODO: No idea how to represent this data. This needs to be game agnostic, so
// this might be a bit tricky...
pub type PlayerGameMetaInfo = u64;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HydratedNormalizedTournament {
    pub t_id: NormalizedId,
    pub name: String,
    pub brackets: Vec<NormalizedBracket>,
    pub admins: Vec<AdminAndPrivilegeLevel>,
}

impl Dehydrateable for HydratedNormalizedTournament {
    type Origin = NormalizedOrigin;
    type Dehydrated = DehydratedNormalizedTournament;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminAndPrivilegeLevel {
    pub p_id: NormalizedId,
    pub p_level: AdminPrivilegeLevel,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum AdminPrivilegeLevel {}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HydratedNormalizedBracket {
    pub b_id: NormalizedId,
    pub b_type: BracketNormalizedType,
    pub sets: Vec<NormalizedSet>,
}

impl Dehydrateable for HydratedNormalizedBracket {
    type Origin = NormalizedOrigin;
    type Dehydrated = DehydratedNormalizedBracket;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum BracketNormalizedType {
    DoubleElim,
    RoundRobin,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HydratedNormalizedSet {
    pub s_id: NormalizedId,
    pub games: Vec<NormalizedGame>,
}

impl Dehydrateable for HydratedNormalizedSet {
    type Origin = NormalizedOrigin;
    type Dehydrated = DehydratedNormalizedSet;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HydratedNormalizedGame {
    pub g_id: NormalizedId,
    pub g_type: GameType,
    pub winning_side: GameWinningSide,
}

impl Dehydrateable for HydratedNormalizedGame {
    type Origin = NormalizedOrigin;
    type Dehydrated = DehydratedNormalizedGame;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

/// Kind of a weird way to do this, but we need a bit to indicate which side one
/// in a game. Might need to make this more complicated down the road for when
/// we support more games.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum GameWinningSide {
    Left = 0,
    Right = 1,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum GameType {
    OneVOne(NormalizedPlayerGameInfo, NormalizedPlayerGameInfo),
    MultiVsMulti(Vec<NormalizedPlayerGameInfo>, Vec<NormalizedPlayerGameInfo>),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HydratedNormalizedPlayerGameInfo {
    pub p_id: NormalizedId,
    pub meta: PlayerGameMetaInfo,
}

impl Dehydrateable for HydratedNormalizedPlayerGameInfo {
    type Origin = NormalizedOrigin;
    type Dehydrated = DehydratedNormalizedPlayerGameInfo;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HydratedNormalizedPlayer {
    pub p_id: NormalizedId,
    pub name: String,
    pub prefix: String,
}

impl Dehydrateable for HydratedNormalizedPlayer {
    type Origin = NormalizedOrigin;
    type Dehydrated = DehydratedNormalizedPlayer;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}
