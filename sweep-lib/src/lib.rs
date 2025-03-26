#![deny(warnings)]
#![allow(clippy::reversed_empty_ranges)]

mod haystack;
pub use haystack::{
    Haystack, HaystackBasicPreview, HaystackDefaultView, HaystackPreview, HaystackTagged,
};

mod scorer;
pub use scorer::{
    FuzzyScorer, KMPPattern, Positions, Score, ScoreArray, ScoreItem, ScoreIter, Scorer,
    SubstrScorer,
};

mod rank;
pub use rank::{
    ALL_SCORER_BUILDERS, RankedItems, Ranker, RankerThread, ScorerBuilder, fuzzy_scorer,
    scorer_by_name, substr_scorer,
};

mod candidate;
pub use candidate::{Candidate, CandidateContext, Field, FieldRef, FieldSelector, fields_view};

mod sweep;
pub use crate::sweep::{
    PROMPT_DEFAULT_ICON, Sweep, SweepEvent, SweepOptions, WindowId, WindowLayout, WindowLayoutSize,
    sweep,
};

pub mod rpc;

mod widgets;
pub use widgets::{Process, ProcessCommandArg, ProcessCommandBuilder, Theme};

pub mod common;

pub use surf_n_term;
