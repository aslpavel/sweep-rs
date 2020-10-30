#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

pub mod scorer;
pub use scorer::{FuzzyScorer, Haystack, ScoreResult, Scorer, StringHaystack, SubstrScorer};
mod rank;
pub use rank::{Ranker, RankerResult, ScorerBuilder};
mod candidate;
pub use candidate::{Candidate, FieldSelector};
mod rpc;
pub use rpc::{rpc_encode, rpc_requests, RPCError, RPCErrorKind, RPCRequest, SweepRequest};
mod sweep;
pub use crate::sweep::{sweep, Sweep, SweepEvent, SweepOptions};
