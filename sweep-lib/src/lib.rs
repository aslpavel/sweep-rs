#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

mod haystack;
pub use haystack::{Haystack, HaystackBasicPreview, HaystackDefaultView, HaystackPreview};

mod scorer;
pub use scorer::{
    FuzzyScorer, KMPPattern, Positions, Score, ScoreArray, ScoreItem, ScoreIter, Scorer,
    SubstrScorer,
};

mod rank;
pub use rank::{
    fuzzy_scorer, scorer_by_name, substr_scorer, RankedItems, Ranker, RankerThread, ScorerBuilder,
    ALL_SCORER_BUILDERS,
};

mod candidate;
pub use candidate::{fields_view, Candidate, CandidateContext, Field, FieldRef, FieldSelector};

mod sweep;
pub use crate::sweep::{
    sweep, Sweep, SweepEvent, SweepOptions, WindowId, WindowLayout, WindowLayoutSize,
    PROMPT_DEFAULT_ICON,
};

pub mod rpc;

mod widgets;
pub use widgets::{Process, ProcessCommandArg, ProcessCommandBuilder, Theme};

pub mod common;

pub use surf_n_term;
