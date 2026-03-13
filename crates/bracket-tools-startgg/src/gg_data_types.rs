use bracket_tools_core::data_types::{DataOrigin, Dehydrateable, Hydratable, HydratableType};

pub type StartGgId = u64;

pub type TournamentId = u64;
pub type EventId = u64;
pub type SetId = u64;
pub type GameId = u64;
pub type PlayerId = u64;

pub type GgTournament = HydratableType<DehydratedGgTournament, HydratedGgTournament>;
pub type GgBracket = HydratableType<DehydratedGgBracket, HydratedGgBracket>;
pub type GgSet = HydratableType<DehydratedGgSet, HydratedGgSet>;
pub type GgGame = HydratableType<DehydratedGgGame, HydratedGgGame>;
pub type GgPlayer = HydratableType<DehydratedGgPlayer, HydratedGgPlayer>;

pub struct GgOrigin();
impl DataOrigin for GgOrigin {}

#[derive(Clone, Debug)]
pub struct DehydratedGgTournament(StartGgId);

impl Hydratable for DehydratedGgTournament {
    type Origin = GgOrigin;
    type Hydrated = HydratedGgTournament;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

#[derive(Clone, Debug)]
pub struct DehydratedGgBracket(StartGgId);

impl Hydratable for DehydratedGgBracket {
    type Origin = GgOrigin;
    type Hydrated = HydratedGgBracket;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

#[derive(Clone, Debug)]
pub struct DehydratedGgSet(StartGgId);

impl Hydratable for DehydratedGgSet {
    type Origin = GgOrigin;
    type Hydrated = HydratedGgSet;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

#[derive(Clone, Debug)]
pub struct DehydratedGgGame(StartGgId);

impl Hydratable for DehydratedGgGame {
    type Origin = GgOrigin;
    type Hydrated = HydratedGgGame;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

#[derive(Clone, Debug)]
pub struct DehydratedGgPlayer(StartGgId);

impl Hydratable for DehydratedGgPlayer {
    type Origin = GgOrigin;
    type Hydrated = HydratedGgPlayer;

    fn hydrate(self) -> Self::Hydrated {
        todo!()
    }
}

#[derive(Debug)]
pub struct HydratedGgTournament {
    name: String,
    participants: Vec<GgPlayer>,
}

impl Dehydrateable for HydratedGgTournament {
    type Origin = GgOrigin;
    type Dehydrated = DehydratedGgTournament;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Debug)]
pub struct HydratedGgBracket {}

impl Dehydrateable for HydratedGgBracket {
    type Origin = GgOrigin;
    type Dehydrated = DehydratedGgBracket;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Debug)]
pub struct HydratedGgSet {}

impl Dehydrateable for HydratedGgSet {
    type Origin = GgOrigin;
    type Dehydrated = DehydratedGgSet;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Debug)]
pub struct HydratedGgGame {}

impl Dehydrateable for HydratedGgGame {
    type Origin = GgOrigin;
    type Dehydrated = DehydratedGgGame;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}

#[derive(Debug)]
pub struct HydratedGgPlayer {}

impl Dehydrateable for HydratedGgPlayer {
    type Origin = GgOrigin;
    type Dehydrated = DehydratedGgPlayer;

    fn dehydrate(&self) -> Self::Dehydrated {
        todo!()
    }
}
