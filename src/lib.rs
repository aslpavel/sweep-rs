#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

mod scorer;
pub use scorer::{
    Field, FuzzyScorer, Haystack, KMPPattern, ScoreResult, Scorer, StringHaystack, SubstrScorer,
};
mod rank;
pub use rank::{Ranker, RankerResult, ScorerBuilder};
mod candidate;
pub use candidate::{Candidate, FieldSelector};
mod sweep;
pub use crate::sweep::{sweep, Sweep, SweepEvent, SweepOptions, SCORER_NEXT_TAG};
pub mod rpc;

pub trait LockExt {
    type Value;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out;
}

impl<V> LockExt for std::sync::Mutex<V> {
    type Value = V;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out,
    {
        let mut value = self.lock().expect("lock poisoned");
        scope(&mut *value)
    }
}
