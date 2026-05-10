//! [`Action`] supertrait: anything an app might want to do in response
//! to user input.
//!
//! Consumers define their own enum; the blanket impl makes any
//! sufficiently-derived type usable as `A` in [`crate::Binding`] and
//! [`crate::Event`]. Keeping this generic lets the crate stay free of
//! domain-specific vocabulary — pan/rotate/zoom for jump-cannon,
//! fire/reload/jump for a game, etc.

use serde::{de::DeserializeOwned, Serialize};

pub trait Action:
    Clone + std::fmt::Debug + std::cmp::PartialEq + Eq + std::hash::Hash + Serialize + DeserializeOwned + 'static
{
}

impl<T> Action for T where
    T: Clone
        + std::fmt::Debug
        + std::cmp::PartialEq
        + Eq
        + std::hash::Hash
        + Serialize
        + DeserializeOwned
        + 'static
{
}
