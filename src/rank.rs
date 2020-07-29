use crate::score::{FuzzyScorer, Haystack, ScoreResult, Scorer};
use rayon::prelude::*;
use std::{
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

/// Rank slice of items
///
/// Each item from heystack is converted to `Haystack` item with provided
/// `focus` function, and then resulting vector is scored and sorted based
/// on score.
pub fn rank<S, H, F, FR>(
    scorer: S,
    keep_order: bool,
    niddle: &str,
    haystack: &[H],
    focus: F,
) -> Vec<ScoreResult<FR>>
where
    S: Scorer + Sync + Send,
    H: Sync,
    F: Fn(&H) -> FR + Send + Sync,
    FR: Haystack + Send,
{
    let niddle: Vec<_> = niddle.chars().flat_map(char::to_lowercase).collect();
    let mut result: Vec<_> = haystack
        .into_par_iter()
        .filter_map(move |haystack| scorer.score(&niddle, focus(haystack)))
        .collect();
    if !keep_order {
        result.par_sort_unstable_by(|a, b| {
            a.score.partial_cmp(&b.score).expect("Nan score").reverse()
        });
    }
    result
}

enum RankerCmd<H> {
    HaystackClear,
    HaystackReverse,
    HaystackAppend(Vec<H>),
    Niddle(String),
    Scorer(Arc<dyn Scorer>),
}

/// Ranker result
pub struct RankerResult<H> {
    /// Scored and sorted heystack items
    pub result: Vec<ScoreResult<H>>,
    /// Scorer used during ranking
    pub scorer: Arc<dyn Scorer>,
    /// Time it took to rank items
    pub duration: Duration,
    /// Full size of the haystack
    pub haystack_size: usize,
    /// Value used to distinguish differnt runs of the ranker
    pub generation: usize,
}

impl<H> Default for RankerResult<H> {
    fn default() -> Self {
        Self {
            result: Default::default(),
            scorer: Arc::new(FuzzyScorer::new()),
            duration: Duration::new(0, 0),
            haystack_size: 0,
            generation: 0,
        }
    }
}

/// Asynchronous ranker
#[derive(Clone)]
pub struct Ranker<H> {
    sender: mpsc::Sender<RankerCmd<H>>,
    result: Arc<Mutex<Arc<RankerResult<H>>>>,
}

impl<H> Ranker<H>
where
    H: Clone + Send + Sync + 'static + Haystack,
{
    /// Create new ranker
    ///
    /// It will also spawn worker thread during construction.
    pub fn new<N>(mut scorer: Arc<dyn Scorer>, keep_order: bool, mut notify: N) -> Self
    where
        N: FnMut() -> bool + Send + 'static,
    {
        let result: Arc<Mutex<Arc<RankerResult<H>>>> = Default::default();
        let mut niddle = String::new();
        let mut haystack = Vec::new();
        let mut generation = 0usize;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn({
            let result = result.clone();
            move || {
                loop {
                    // block on first event and process all pending requests in one go
                    let cmd = match receiver.recv() {
                        Ok(cmd) => cmd,
                        Err(_) => return,
                    };
                    let mut haystack_new = Vec::new();
                    let mut haystack_reverse = false;
                    let mut niddle_updated = false; // niddle was updated
                    let mut niddle_prefix = true; // previous niddle is a prefix of the new one
                    let mut scorer_updated = false;
                    for cmd in Some(cmd).into_iter().chain(receiver.try_iter()) {
                        match cmd {
                            RankerCmd::HaystackAppend(haystack) => {
                                haystack_new.extend(haystack);
                            }
                            RankerCmd::HaystackClear => {
                                haystack.clear();
                                haystack_new.clear();
                                scorer_updated = true;
                            }
                            RankerCmd::Niddle(niddle_new) if niddle_new != niddle => {
                                niddle_updated = true;
                                niddle_prefix = niddle_prefix && niddle_new.starts_with(&niddle);
                                niddle = niddle_new;
                            }
                            RankerCmd::Scorer(scorer_new) => {
                                scorer = scorer_new;
                                scorer_updated = true;
                            }
                            RankerCmd::HaystackReverse => {
                                haystack_reverse = !haystack_reverse;
                                scorer_updated = true;
                            }
                            _ => continue,
                        }
                    }
                    haystack.extend(haystack_new.iter().cloned());
                    if haystack_reverse {
                        haystack.reverse();
                    }

                    // rank haystack
                    let start = Instant::now();
                    let result_new = if scorer_updated {
                        // re-rank all data
                        rank(
                            &scorer,
                            keep_order,
                            niddle.as_ref(),
                            haystack.as_ref(),
                            Clone::clone,
                        )
                    } else if !niddle_updated && haystack_new.is_empty() {
                        continue;
                    } else if niddle_updated {
                        if niddle_prefix && haystack_new.is_empty() {
                            // incremental ranking
                            let result_old = result.with(|result| Arc::clone(result));
                            rank(
                                &scorer,
                                keep_order,
                                niddle.as_ref(),
                                result_old.result.as_ref(),
                                |r| r.haystack.clone(),
                            )
                        } else {
                            // re-rank all data
                            rank(
                                &scorer,
                                keep_order,
                                niddle.as_ref(),
                                haystack.as_ref(),
                                Clone::clone,
                            )
                        }
                    } else {
                        // rank only new data
                        let result_add = rank(
                            &scorer,
                            keep_order,
                            niddle.as_ref(),
                            haystack_new.as_ref(),
                            Clone::clone,
                        );
                        let result_old = result.with(|result| Arc::clone(result));
                        let mut result_new =
                            Vec::with_capacity(result_old.result.len() + result_add.len());
                        result_new.extend(result_old.result.iter().cloned());
                        result_new.extend(result_add);
                        if !keep_order {
                            result_new.par_sort_by(|a, b| {
                                a.score.partial_cmp(&b.score).expect("Nan score").reverse()
                            });
                        }
                        result_new
                    };
                    let duration = Instant::now() - start;
                    generation += 1;
                    let result_new = RankerResult {
                        scorer: scorer.clone(),
                        result: result_new,
                        duration,
                        haystack_size: haystack.len(),
                        generation,
                    };
                    result.with(|result| std::mem::replace(result, Arc::new(result_new)));

                    if !notify() {
                        return;
                    }
                }
            }
        });
        Self {
            sender,
            // worker,
            result,
        }
    }

    /// Extend haystack with new entries
    pub fn haystack_extend(&self, haystack: Vec<H>) {
        self.sender
            .send(RankerCmd::HaystackAppend(haystack))
            .expect("failed to send haystack");
    }

    /// Clear haystack
    pub fn haystack_clear(&self) {
        self.sender
            .send(RankerCmd::HaystackClear)
            .expect("failed to clear haystack")
    }

    /// Reverse order of elements in the haystack
    pub fn haystack_reverse(&self) {
        self.sender
            .send(RankerCmd::HaystackReverse)
            .expect("failed to clear haystack")
    }

    /// Set new needle
    pub fn niddle_set(&self, niddle: String) {
        self.sender
            .send(RankerCmd::Niddle(niddle))
            .expect("failed to send niddle");
    }

    /// Set new scorer
    pub fn scorer_set(&self, scorer: Arc<dyn Scorer>) {
        self.sender
            .send(RankerCmd::Scorer(scorer))
            .expect("failed to send scorer");
    }

    /// Get last result
    pub fn result(&self) -> Arc<RankerResult<H>> {
        self.result.with(|result| result.clone())
    }
}

pub trait LockExt {
    type Value;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out;
}

impl<V> LockExt for Mutex<V> {
    type Value = V;

    fn with<Scope, Out>(&self, scope: Scope) -> Out
    where
        Scope: FnOnce(&mut Self::Value) -> Out,
    {
        let mut value = self.lock().expect("lock poisoned");
        scope(&mut *value)
    }
}
