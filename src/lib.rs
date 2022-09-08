#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

mod scorer;
pub use scorer::{
    FuzzyScorer, Haystack, KMPPattern, Positions, Score, ScoreResult, Scorer, StringHaystack,
    SubstrScorer,
};
mod rank;
pub use rank::{fuzzy_scorer, substr_scorer, Ranker, RankerResult, ScorerBuilder};
mod candidate;
pub use candidate::{Candidate, Field, FieldRef, FieldSelector};
mod sweep;
pub use crate::sweep::{sweep, Sweep, SweepEvent, SweepOptions, SCORER_NEXT_TAG};
pub mod rpc;
mod widgets;
pub use widgets::Theme;

trait LockExt {
    type Value;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&Self::Value) -> Out;

    fn with_mut<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out;
}

impl<V> LockExt for std::sync::Mutex<V> {
    type Value = V;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&Self::Value) -> Out,
    {
        let value = self.lock().expect("lock poisoned");
        scope(&*value)
    }

    fn with_mut<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out,
    {
        let mut value = self.lock().expect("lock poisoned");
        scope(&mut *value)
    }
}

impl<V> LockExt for std::sync::RwLock<V> {
    type Value = V;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&Self::Value) -> Out,
    {
        let value = self.read().expect("lock poisoned");
        scope(&*value)
    }

    fn with_mut<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out,
    {
        let mut value = self.write().expect("lock poisoned");
        scope(&mut *value)
    }
}
