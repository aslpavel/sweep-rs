use crate::{FuzzyScorer, Haystack, LockExt, Positions, Score, ScoreResult, Scorer, SubstrScorer};
use crossbeam_channel::{unbounded, Receiver, Sender};
use rayon::prelude::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

/// Rank slice of items
///
/// Each item from haystack is converted to `Haystack` item with provided
/// `focus` function, and then resulting vector is scored and sorted based
/// on score.
pub fn rank<S, H, F, FR>(
    scorer: S,
    keep_order: bool,
    haystack: &[H],
    focus: F,
) -> Vec<ScoreResult<FR>>
where
    S: Scorer + Sync + Send,
    H: Sync,
    F: Fn(&H) -> FR + Send + Sync,
    FR: Haystack + Send,
{
    let rank_scope = tracing::debug_span!("ranking", len = %haystack.len());
    let mut result: Vec<_> = rank_scope.in_scope(|| {
        haystack
            .into_par_iter()
            .filter_map(move |haystack| scorer.score(focus(haystack)))
            .collect()
    });
    if !keep_order {
        let _ = tracing::debug_span!("sorting", len = %haystack.len()).enter();
        result.par_sort_unstable_by(|a, b| b.score.cmp(&a.score));
    }
    result
}

/// Function to create scorer with the given needle
pub type ScorerBuilder = Arc<dyn Fn(&str) -> Arc<dyn Scorer> + Send + Sync>;

/// Create case-insensitive fuzzy scorer builder
pub fn fuzzy_scorer() -> ScorerBuilder {
    Arc::new(|needle: &str| {
        let needle: Vec<_> = needle.chars().flat_map(char::to_lowercase).collect();
        Arc::new(FuzzyScorer::new(needle))
    })
}

/// Create case-insensitive substring scorer builder
pub fn substr_scorer() -> ScorerBuilder {
    Arc::new(|needle: &str| {
        let needle: Vec<_> = needle.chars().flat_map(char::to_lowercase).collect();
        Arc::new(SubstrScorer::new(needle))
    })
}

/// Ranker result
pub struct RankerResult<H> {
    /// Scored and sorted haystack items
    pub result: Vec<ScoreResult<H>>,
    /// Scorer used during ranking
    pub scorer: Arc<dyn Scorer>,
    /// Time it took to rank items
    pub duration: Duration,
    /// Full size of the haystack
    pub haystack_size: usize,
    /// Value used to distinguish different runs of the ranker
    pub generation: usize,
}

impl<H> Default for RankerResult<H> {
    fn default() -> Self {
        Self {
            result: Default::default(),
            scorer: Arc::new(FuzzyScorer::new(Vec::new())),
            duration: Duration::new(0, 0),
            haystack_size: 0,
            generation: 0,
        }
    }
}

/// Asynchronous ranker
#[derive(Clone)]
pub struct Ranker<H> {
    sender: Sender<RankerCmd<H>>,
    result: Arc<Mutex<Arc<RankerResult<H>>>>,
}

impl<H> Ranker<H>
where
    H: Clone + Send + Sync + 'static + Haystack,
{
    /// Create new ranker
    ///
    /// It will also spawn worker thread during construction.
    pub fn new<N>(mut scorer_builder: ScorerBuilder, keep_order: bool, mut notify: N) -> Self
    where
        N: FnMut() -> bool + Send + 'static,
    {
        let result: Arc<Mutex<Arc<RankerResult<H>>>> = Default::default();
        let (sender, receiver) = unbounded();
        std::thread::Builder::new()
            .name("sweep-ranker".to_string())
            .spawn({
                let result = result.clone();
                move || {
                    let mut needle = String::new();
                    let mut haystack = Vec::new();
                    let mut generation = 0usize;
                    let mut scorer = scorer_builder("");
                    loop {
                        // block on first event and process all pending requests in one go
                        let cmd = match receiver.recv() {
                            Ok(cmd) => cmd,
                            Err(_) => return,
                        };
                        let mut haystack_new = Vec::new();
                        let mut haystack_reverse = false;
                        let mut needle_updated = false; // needle was updated
                        let mut needle_prefix = true; // previous needle is a prefix of the new one
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
                                RankerCmd::Needle(needle_new) if needle_new != needle => {
                                    needle_updated = true;
                                    needle_prefix =
                                        needle_prefix && needle_new.starts_with(&needle);
                                    needle = needle_new;
                                }
                                RankerCmd::Scorer(scorer_new) => {
                                    scorer_builder = scorer_new;
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
                            scorer = scorer_builder(needle.as_ref());
                            rank(&scorer, keep_order, haystack.as_ref(), Clone::clone)
                        } else if !needle_updated && haystack_new.is_empty() {
                            continue;
                        } else if needle_updated {
                            scorer = scorer_builder(needle.as_ref());
                            if needle_prefix && haystack_new.is_empty() {
                                // incremental ranking
                                let result_old = result.with(|result| Arc::clone(result));
                                rank(&scorer, keep_order, result_old.result.as_ref(), |r| {
                                    r.haystack.clone()
                                })
                            } else {
                                // re-rank all data
                                rank(&scorer, keep_order, haystack.as_ref(), Clone::clone)
                            }
                        } else {
                            // rank only new data
                            let result_add =
                                rank(&scorer, keep_order, haystack_new.as_ref(), Clone::clone);
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
                        result.with_mut(|result| std::mem::replace(result, Arc::new(result_new)));

                        if !notify() {
                            return;
                        }
                    }
                }
            })
            .expect("failed to start sweep-ranker thread");
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
    pub fn needle_set(&self, needle: String) {
        self.sender
            .send(RankerCmd::Needle(needle))
            .expect("failed to send needle");
    }

    /// Set new scorer
    pub fn scorer_set(&self, scorer: ScorerBuilder) {
        self.sender
            .send(RankerCmd::Scorer(scorer))
            .expect("failed to send scorer");
    }

    /// Get last result
    pub fn result(&self) -> Arc<RankerResult<H>> {
        self.result
            .with(|result| tracing::debug_span!("clone result").in_scope(|| result.clone()))
    }
}

pub struct Ranker1<H> {
    sender: Sender<RankerCmd<H>>,
    result: Arc<Mutex<RankerResult1<H>>>,
}

impl<H> Ranker1<H>
where
    H: Haystack,
{
    pub fn new<N>(keep_order: bool, notify: N) -> Self
    where
        N: Fn() -> bool + Send + 'static,
    {
        let (sender, receiver) = unbounded();
        let result: Arc<Mutex<RankerResult1<H>>> = Default::default();
        std::thread::Builder::new()
            .name("sweep-ranker".to_string())
            .spawn({
                let result = result.clone();
                move || ranker_worker(receiver, result, notify, keep_order)
            })
            .expect("failed to start sweep-ranker thread");
        Self { sender, result }
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
    pub fn needle_set(&self, needle: String) {
        self.sender
            .send(RankerCmd::Needle(needle))
            .expect("failed to send needle");
    }

    /// Set new scorer
    pub fn scorer_set(&self, scorer: ScorerBuilder) {
        self.sender
            .send(RankerCmd::Scorer(scorer))
            .expect("failed to send scorer");
    }

    /// Get last result
    pub fn result(&self) -> RankerResult1<H> {
        self.result.with(|result| result.clone())
    }
}

fn ranker_worker<H, N>(
    receiver: Receiver<RankerCmd<H>>,
    result: Arc<Mutex<RankerResult1<H>>>,
    notify: N,
    keep_order: bool,
) where
    H: Haystack,
    N: Fn() -> bool,
{
    let haystack: Arc<RwLock<Vec<H>>> = Default::default();
    let mut needle = String::new();

    let mut scorer_builder = fuzzy_scorer();
    let mut scorer = scorer_builder("");

    let mut generation = 0usize;
    let mut pool: Pool<Vec<Match>> = Pool::new();
    let mut matches_prev: Arc<Vec<Match>> = Default::default();

    loop {
        enum RankAction {
            DoNothing,
            Offset(usize),
            CurrentMatch,
            All,
        }
        use RankAction::*;
        let mut action = DoNothing;

        // block on first event and process all pending requests in one go
        let cmd = match receiver.recv() {
            Ok(cmd) => cmd,
            Err(_) => return,
        };
        for cmd in Some(cmd).into_iter().chain(receiver.try_iter()) {
            use RankerCmd::*;
            match cmd {
                Needle(needle_new) => {
                    action = match action {
                        DoNothing | CurrentMatch if needle_new.starts_with(&needle) => CurrentMatch,
                        _ => All,
                    };
                    needle = needle_new;
                    scorer = scorer_builder(&needle);
                }
                Scorer(scorer_builder_new) => {
                    action = All;
                    scorer_builder = scorer_builder_new;
                    scorer = scorer_builder(&needle);
                }
                HaystackAppend(haystack_append) => {
                    action = match action {
                        DoNothing => Offset(haystack.with(|hs| hs.len())),
                        Offset(offset) => Offset(offset),
                        _ => All,
                    };
                    haystack.with_mut(|hs| hs.extend(haystack_append));
                }
                HaystackReverse => {
                    action = All;
                    haystack.with_mut(|hs| hs.reverse());
                }
                HaystackClear => {
                    action = All;
                    haystack.with_mut(|hs| hs.clear());
                }
            }
        }

        // rank
        let rank_instant = Instant::now();
        let matches = match action {
            DoNothing => continue,
            Offset(offset) => {
                let mut matches = pool.alloc();
                matches.clear();
                // score new matches
                matches.extend((offset..haystack.with(|hs| hs.len())).map(Match::new));
                rank1(scorer.clone(), &haystack, &mut matches, false);
                // copy previous matches
                matches.extend(matches_prev.iter().cloned());
                // sort matches
                if !keep_order {
                    matches.par_sort_unstable_by(|a, b| b.score.cmp(&a.score));
                }
                matches
            }
            CurrentMatch => {
                let mut matches = pool.alloc();
                matches.clear();
                // score previous matches
                matches.extend(matches_prev.iter().cloned());
                rank1(scorer.clone(), &haystack, &mut matches, !keep_order);
                matches
            }
            All => {
                let mut matches = pool.alloc();
                matches.clear();
                // score all haystack elements
                matches.extend((0..haystack.with(|hs| hs.len())).map(Match::new));
                rank1(scorer.clone(), &haystack, &mut matches, !keep_order);
                matches
            }
        };
        let rank_elapsed = rank_instant.elapsed();

        // update result
        generation += 1;
        matches_prev = pool.promote(matches);
        result.with_mut(|result| {
            *result = RankerResult1 {
                haystack: haystack.clone(),
                matches: matches_prev.clone(),
                scorer: scorer.clone(),
                duration: rank_elapsed,
                generation,
            };
        });
        if !notify() {
            return;
        }
    }
}

#[derive(Clone)]
pub struct RankerResult1<H> {
    haystack: Arc<RwLock<Vec<H>>>,
    matches: Arc<Vec<Match>>,
    scorer: Arc<dyn Scorer>,
    duration: Duration,
    generation: usize,
}

impl<H> RankerResult1<H> {
    /// Number of matched items
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Number of all items
    pub fn haystack_len(&self) -> usize {
        self.haystack.with(|hs| hs.len())
    }

    /// Scorer used to score items
    pub fn scorer(&self) -> Arc<dyn Scorer> {
        self.scorer.clone()
    }

    /// Duration of ranking
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Generation number
    pub fn generation(&self) -> usize {
        self.generation
    }

    /// Get score result by index
    pub fn get(&self, index: usize) -> Option<ScoreResult<H>>
    where
        H: Clone,
    {
        let matched = self.matches.get(index)?.clone();
        Some(ScoreResult {
            haystack: self.haystack.with(|hs| hs.get(matched.index).cloned())?,
            score: matched.score.unwrap_or(Score::MIN),
            positions: matched.positions,
        })
    }
}

impl<H> Default for RankerResult1<H> {
    fn default() -> Self {
        Self {
            haystack: Default::default(),
            matches: Default::default(),
            scorer: fuzzy_scorer()(""),
            duration: Default::default(),
            generation: Default::default(),
        }
    }
}

enum RankerCmd<H> {
    HaystackClear,
    HaystackReverse,
    HaystackAppend(Vec<H>),
    Needle(String),
    Scorer(ScorerBuilder),
}

#[derive(Clone, Debug)]
struct Match {
    score: Option<Score>,
    positions: Positions,
    index: usize,
}

impl Match {
    fn new(index: usize) -> Self {
        Self {
            score: None,
            positions: Positions::new(0),
            index,
        }
    }
}

thread_local! {
    static TARGET: RefCell<Vec<char>> = Default::default();
}

fn rank1<S, H>(scorer: S, hastack: &Arc<RwLock<Vec<H>>>, matches: &mut Vec<Match>, sort: bool)
where
    S: Scorer + Clone,
    H: Haystack,
{
    // score haystack items
    hastack.with(|haystack| {
        matches
            .par_iter_mut()
            .for_each_with(scorer, |scorer, item| {
                TARGET.with(|target| {
                    let mut target = target.borrow_mut();
                    target.clear();
                    haystack[item.index]
                        .haystack_scope(|char| target.extend(char::to_lowercase(char)));
                    let mut score = Score::MIN;
                    let mut positions = Positions::new(target.len());
                    if scorer.score_ref(target.as_slice(), &mut score, &mut positions) {
                        item.score = Some(score);
                        item.positions = positions;
                    }
                })
            })
    });

    // filter out items that failed to match
    matches.retain(|item| item.score.is_some());

    // sort items
    if sort {
        matches.par_sort_unstable_by(|a, b| b.score.cmp(&a.score));
    }
}

struct Pool<T> {
    promoted: HashMap<usize, Arc<T>>,
    count: usize,
}

/// Unique reference to pool item
struct PoolItem<T> {
    item: Arc<T>,
    index: usize,
}

impl<T> Deref for PoolItem<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

impl<T> DerefMut for PoolItem<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::get_mut(&mut self.item).expect("pool logic error")
    }
}

impl<T: Default> Pool<T> {
    fn new() -> Self {
        Self {
            promoted: Default::default(),
            count: 0,
        }
    }

    fn alloc(&mut self) -> PoolItem<T> {
        if let Some(index) = self
            .promoted
            .iter_mut()
            .find_map(|(index, item)| Arc::get_mut(item).is_some().then_some(*index))
        {
            return PoolItem {
                item: self.promoted.remove(&index).expect("pool logic error"),
                index,
            };
        }
        let item = PoolItem {
            item: Default::default(),
            index: self.count,
        };
        self.count += 1;
        tracing::info!(pool_size = self.count, "pool item allocated");
        item
    }

    fn promote(&mut self, item: PoolItem<T>) -> Arc<T> {
        self.promoted.insert(item.index, item.item.clone());
        item.item
    }
}
