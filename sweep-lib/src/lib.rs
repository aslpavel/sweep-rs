#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

mod haystack;
pub use haystack::{Haystack, HaystackBasicPreview, HaystackDefaultView, HaystackPreview};

mod scorer;
pub use scorer::{
    FuzzyScorer, KMPPattern, Positions, PositionsRef, Score, ScoreArray, ScoreItem, ScoreIter,
    ScoreResult, Scorer, SubstrScorer,
};

mod rank;
pub use rank::{fuzzy_scorer, substr_scorer, RankedItems, Ranker, ScorerBuilder};

mod candidate;
pub use candidate::{fields_view, Candidate, CandidateContext, Field, FieldRef, FieldSelector};

mod sweep;
pub use crate::sweep::{
    sweep, Sweep, SweepEvent, SweepLayout, SweepLayoutSize, SweepOptions, WindowId,
    PROMPT_DEFAULT_ICON,
};

pub mod rpc;

mod widgets;
pub use widgets::{Process, ProcessCommandArg, ProcessCommandBuilder, Theme};

pub mod common;

pub use surf_n_term;
