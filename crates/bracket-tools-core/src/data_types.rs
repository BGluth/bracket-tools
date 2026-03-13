use std::{cell::UnsafeCell, fmt::Debug, ops::Deref, rc::Rc};

use serde::{Deserialize, Serialize, Serializer};

pub struct NormalizedOrigin();
impl DataOrigin for NormalizedOrigin {}

pub trait Hydratable {
    type Origin: DataOrigin;
    type Hydrated;

    fn hydrate(self) -> Self::Hydrated;
}

pub trait Dehydrateable {
    type Origin: DataOrigin;
    type Dehydrated;

    fn dehydrate(&self) -> Self::Dehydrated;
}

pub trait NormalizableData {
    type NormalizedData;

    fn normalize(&self) -> Self::NormalizedData;
}

pub trait HydratableNormalized {
    type NormalizedData;

    fn hydrate_to_normalized(self) -> Self::NormalizedData;
}

impl<T, H> HydratableNormalized for T
where
    T: Hydratable<Hydrated = H>,
    H: NormalizableData,
{
    type NormalizedData = H::NormalizedData;

    fn hydrate_to_normalized(self) -> Self::NormalizedData {
        self.hydrate().normalize()
    }
}

pub trait DataOrigin {}

#[derive(Clone, Debug)]
/// A wrapper type that is needed to be able to implicitly hydrate to the inner enum without the user being aware.
pub struct HydratableType<D, H>(Rc<UnsafeCell<HydratableTypeInner<D, H>>>)
where
    D: Clone + Hydratable<Hydrated = H>,
    H: Dehydrateable;

/// Fun deref behavior. Essentially the goal is to tr
impl<D, H> Deref for HydratableType<D, H>
where
    D: Clone + Hydratable<Hydrated = H>,
    H: Dehydrateable,
{
    type Target = H;

    fn deref(&self) -> &Self::Target {
        let inner_ptr = self.0.get();

        // I can't find another way to do this without using an `UnsafeCell`.
        unsafe { (*inner_ptr).get_or_hydrate() }
    }
}

impl<D, H> Serialize for HydratableType<D, H>
where
    D: Clone + Hydratable<Hydrated = H> + Hydratable,
    H: Dehydrateable,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        todo!()
    }
}

impl<'de, D, H> Deserialize<'de> for HydratableType<D, H>
where
    D: Clone + Hydratable<Hydrated = H> + Hydratable,
    H: Dehydrateable,
{
    fn deserialize<De>(deserializer: De) -> Result<Self, De::Error>
    where
        De: serde::Deserializer<'de>,
    {
        todo!()
    }
}

#[derive(Clone, Debug)]
enum HydratableTypeInner<D, H>
where
    D: Hydratable<Hydrated = H> + Hydratable,
    H: Dehydrateable,
{
    Dehydrated(D),
    Hydrated(H),
}

impl<D, H> HydratableTypeInner<D, H>
where
    D: Clone + Hydratable<Hydrated = H> + Hydratable,
    H: Dehydrateable,
{
    fn get_or_hydrate(&mut self) -> &H {
        match self {
            HydratableTypeInner::Dehydrated(d) => {
                *self = Self::Hydrated(d.clone().hydrate());

                // Haha this is bizarre... I can not figure out a better way to make this work though.
                match self {
                    HydratableTypeInner::Dehydrated(_) => unreachable!(),
                    HydratableTypeInner::Hydrated(h) => h,
                }
            }
            HydratableTypeInner::Hydrated(h) => h,
        }
    }
}
