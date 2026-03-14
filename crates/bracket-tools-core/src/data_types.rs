pub trait Dehydrateable {
    type Dehydrated;

    fn dehydrate(&self) -> Self::Dehydrated;
}

pub trait Normalizable {
    type NormalizedData;

    fn normalize(&self) -> Self::NormalizedData;
}
