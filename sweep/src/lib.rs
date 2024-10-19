#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

mod scorer;
pub use scorer::{
    haystack_default_view, FuzzyScorer, Haystack, HaystackPreview, KMPPattern, Positions, Score,
    ScoreResult, Scorer, SubstrScorer,
};

mod rank;
pub use rank::{fuzzy_scorer, substr_scorer, RankedItems, Ranker, ScorerBuilder};

mod candidate;
pub use candidate::{fields_view, Candidate, CandidateContext, Field, FieldRef, FieldSelector};

mod sweep;
pub use crate::sweep::{sweep, Sweep, SweepEvent, SweepLayout, SweepOptions, PROMPT_DEFAULT_ICON};

pub mod rpc;

mod widgets;
pub use widgets::{Process, Theme};

pub mod common;

pub use surf_n_term;
