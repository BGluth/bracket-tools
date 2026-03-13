pub trait Hydratable {
    type Hydrated;

    fn hydrate(self) -> Self::Hydrated;
}

pub trait Dehydrateable {
    type Dehydrated;

    fn dehydrate(&self) -> Self::Dehydrated;
}

pub trait NormalizableData {
    type NormalizedData;

    fn normalize(&self) -> Self::NormalizedData;
}
