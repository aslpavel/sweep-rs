use crate::{common::LockExt, FuzzyScorer, Haystack, Positions, Score, Scorer, SubstrScorer};
use crossbeam_channel::{unbounded, Receiver, Sender};
use rayon::prelude::*;
use std::{
    cell::Cell,
    collections::HashMap,
    hash::Hash,
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

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

#[derive(Clone)]
pub struct Ranker {
    sender: Sender<RankerCmd>,
    result: Arc<Mutex<Arc<RankedItems>>>,
}

impl Ranker {
    pub fn new<N>(notify: N) -> Self
    where
        N: Fn(Arc<RankedItems>) -> bool + Send + 'static,
    {
        let (sender, receiver) = unbounded();
        let result: Arc<Mutex<Arc<RankedItems>>> = Default::default();
        std::thread::Builder::new()
            .name("sweep-ranker".to_string())
            .spawn({
                let result = result.clone();
                move || ranker_worker(receiver, result, notify)
            })
            .expect("failed to start sweep-ranker thread");
        Self { sender, result }
    }

    /// Extend haystack with new entries
    pub fn haystack_extend<'a, H>(
        &self,
        ctx: &H::Context,
        haystack: impl IntoIterator<Item = &'a H>,
    ) where
        H: Haystack,
    {
        let haystack = haystack
            .into_iter()
            .map(|haystack| {
                let mut item = String::new();
                haystack.haystack_scope(ctx, |ch| item.push(ch));
                item
            })
            .collect();
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

    /// Whether to keep order of elements or sort by the best score
    ///
    /// `None` will toggle current state, `Some(value)` will set it
    pub fn keep_order(&self, toggle: Option<bool>) {
        self.sender
            .send(RankerCmd::KeepOrder(toggle))
            .expect("failed to send keep_order");
    }

    /// Get last result
    pub fn result(&self) -> Arc<RankedItems> {
        self.result.with(|result| result.clone())
    }

    /// Sets atomic to true once all requests before it has been processed
    pub fn sync(&self) -> Arc<AtomicBool> {
        let synced = Arc::new(AtomicBool::new(false));
        self.sender
            .send(RankerCmd::Sync(synced.clone()))
            .expect("failed to send sync request");
        synced
    }
}

fn ranker_worker<N>(receiver: Receiver<RankerCmd>, result: Arc<Mutex<Arc<RankedItems>>>, notify: N)
where
    N: Fn(Arc<RankedItems>) -> bool,
{
    let mut haystack: Vec<String> = Default::default();
    let mut needle = String::new();
    let mut keep_order = false;

    let mut scorer_builder = fuzzy_scorer();
    let mut scorer = scorer_builder("");

    let mut rank_gen = 0usize;
    let mut haystack_gen = 0usize;
    let mut pool: Pool<Vec<Match>> = Pool::new();
    let mut matches_prev: Arc<Vec<Match>> = Default::default();
    let mut synced: Vec<Arc<AtomicBool>> = Vec::new();

    loop {
        #[derive(Clone, Copy)]
        enum RankAction {
            DoNothing,     // ignore
            Notify,        // only notify
            Offset(usize), // rank items starting from offset
            CurrentMatch,  // rank only current match
            All,           // rank everything
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
                        DoNothing if needle_new == needle => continue,
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
                        DoNothing => Offset(haystack.len()),
                        Offset(offset) => Offset(offset),
                        _ => All,
                    };
                    haystack.extend(haystack_append);
                }
                HaystackClear => {
                    action = All;
                    haystack_gen += 1;
                    haystack.clear();
                }
                KeepOrder(toggle) => {
                    action = All;
                    match toggle {
                        None => keep_order = !keep_order,
                        Some(value) => keep_order = value,
                    }
                }
                Sync(sync) => {
                    action = match action {
                        DoNothing => Notify,
                        _ => action,
                    };
                    synced.push(sync);
                }
            }
        }

        // rank
        let rank_instant = Instant::now();
        matches_prev = match action {
            DoNothing => {
                continue;
            }
            Notify => matches_prev,
            Offset(offset) => {
                let mut matches = pool.alloc();
                matches.clear();
                // score new matches
                matches.extend((offset..haystack.len()).rev().map(Match::new));
                rank(&(), scorer.clone(), &haystack, &mut matches, false);
                // copy previous matches
                matches.extend(matches_prev.iter().rev().cloned());
                matches.reverse();
                // sort matches
                if !keep_order {
                    matches.par_sort_unstable_by(|a, b| b.score.cmp(&a.score));
                }
                pool.promote(matches)
            }
            CurrentMatch => {
                // score previous matches
                let mut matches = pool.alloc();
                matches.clear();
                matches.extend(matches_prev.iter().cloned());
                rank(&(), scorer.clone(), &haystack, &mut matches, !keep_order);
                pool.promote(matches)
            }
            All => {
                // score all haystack elements
                let mut matches = pool.alloc();
                matches.clear();
                matches.extend((0..haystack.len()).map(Match::new));
                rank(&(), scorer.clone(), &haystack, &mut matches, !keep_order);
                pool.promote(matches)
            }
        };
        let rank_elapsed = rank_instant.elapsed();

        // update result
        // TODO: ArcSwap?
        rank_gen += 1;
        result.with_mut(|result| {
            *result = Arc::new(RankedItems {
                haystack_gen,
                matches: matches_prev.clone(),
                scorer: scorer.clone(),
                duration: rank_elapsed,
                rank_gen,
            });
        });

        for sync in synced.drain(..) {
            sync.store(true, Ordering::Release);
        }
        if !notify(result.with(|r| r.clone())) {
            return;
        }
    }
}

pub struct RankedItems {
    haystack_gen: usize,
    matches: Arc<Vec<Match>>,
    scorer: Arc<dyn Scorer>,
    duration: Duration,
    rank_gen: usize,
}

#[derive(Debug, Clone)]
pub struct RankedItem {
    pub score: Score,
    pub positions: Positions,
    pub id: RankedItemId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RankedItemId {
    pub haystack_index: usize,
    haystack_gen: usize,
}

impl RankedItems {
    /// Number of matched items
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Scorer used to score items
    pub fn scorer(&self) -> &Arc<dyn Scorer> {
        &self.scorer
    }

    /// Duration of ranking
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Generation number
    pub fn generation(&self) -> usize {
        self.rank_gen
    }

    /// Get score result by index
    pub fn get(&self, index: usize) -> Option<RankedItem> {
        let matched = self.matches.get(index)?.clone();
        Some(RankedItem {
            score: matched.score.unwrap_or(Score::MIN),
            positions: matched.positions,
            id: RankedItemId {
                haystack_gen: self.haystack_gen,
                haystack_index: matched.haystack_index,
            },
        })
    }

    /// Get haystack index given match index
    pub fn get_haystack_index(&self, index: usize) -> Option<usize> {
        Some(self.matches.get(index)?.haystack_index)
    }

    /// Find match index by haystack index
    pub fn find_match_index(&self, haystack_index: usize) -> Option<usize> {
        self.matches
            .iter()
            .enumerate()
            .find_map(|(index, matched)| {
                (matched.haystack_index == haystack_index).then_some(index)
            })
    }

    /// Iterator over all matched items
    pub fn iter(&self) -> impl Iterator<Item = RankedItem> + '_ {
        (0..self.matches.len()).flat_map(|index| self.get(index))
    }
}

impl Default for RankedItems {
    fn default() -> Self {
        Self {
            haystack_gen: Default::default(),
            matches: Default::default(),
            scorer: fuzzy_scorer()(""),
            duration: Default::default(),
            rank_gen: Default::default(),
        }
    }
}

impl std::fmt::Debug for RankedItems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RankerResult")
            .field("len", &self.len())
            .field("haystack_gen", &self.haystack_gen)
            .field("scorer", &self.scorer)
            .field("duration", &self.duration)
            .field("rank_gen", &self.rank_gen)
            .finish()
    }
}

impl PartialEq for RankedItem {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for RankedItem {}

impl Hash for RankedItem {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl PartialOrd for RankedItem {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.id.cmp(&other.id))
    }
}

impl Ord for RankedItem {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.id.cmp(&other.id)
    }
}

enum RankerCmd {
    HaystackClear,
    HaystackAppend(Vec<String>),
    Needle(String),
    Scorer(ScorerBuilder),
    KeepOrder(Option<bool>),
    Sync(Arc<AtomicBool>),
}

#[derive(Clone, Debug)]
struct Match {
    /// Score value of the match
    score: Option<Score>,
    /// Matched positions
    positions: Positions,
    /// Index in the haystack
    haystack_index: usize,
}

impl Match {
    fn new(haystack_index: usize) -> Self {
        Self {
            score: None,
            positions: Positions::new(0),
            haystack_index,
        }
    }
}

thread_local! {
    static TARGET: Cell<Vec<char>> = Default::default();
}

fn rank<S, H>(ctx: &H::Context, scorer: S, haystack: &[H], matches: &mut Vec<Match>, sort: bool)
where
    S: Scorer + Clone,
    H: Haystack,
{
    if scorer.needle().is_empty() {
        return;
    }

    // score haystack items
    matches
        .par_iter_mut()
        .for_each_with(scorer, |scorer, item| {
            TARGET.with(|target_cell| {
                let mut target = target_cell.take();
                target.clear();
                haystack[item.haystack_index]
                    .haystack_scope(ctx, |char| target.extend(char::to_lowercase(char)));
                let mut score = Score::MIN;
                let mut positions = Positions::new(target.len());
                if scorer.score_ref(target.as_slice(), &mut score, positions.as_mut()) {
                    item.score = Some(score);
                    item.positions = positions;
                } else {
                    item.score = None;
                }
                target_cell.set(target);
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
        tracing::debug!(pool_size = self.count, "[Pool.alloc]");
        item
    }

    fn promote(&mut self, item: PoolItem<T>) -> Arc<T> {
        self.promoted.insert(item.index, item.item.clone());
        item.item
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;

    #[test]
    fn ranker_test() -> Result<(), Error> {
        let timeout = Duration::from_millis(100);
        let (send, recv) = unbounded();
        let ranker = Ranker::new(move |result| send.send(result).is_ok());

        ranker.haystack_extend(&(), &["one", "two", "tree"]);
        let result = recv.recv_timeout(timeout)?;
        println!("{:?}", Vec::from_iter(result.iter()));
        assert_eq!(result.len(), 3);

        ranker.needle_set("o".to_string());
        let result = recv.recv_timeout(timeout)?;
        println!("{:?}", Vec::from_iter(result.iter()));
        assert_eq!(result.len(), 2);

        ranker.needle_set("oe".to_string());
        let result = recv.recv_timeout(timeout)?;
        println!("{:?}", Vec::from_iter(result.iter()));
        assert_eq!(result.len(), 1);

        ranker.haystack_extend(&(), &["ponee", "oe"]);
        let result = recv.recv_timeout(timeout)?;
        println!("{:?}", Vec::from_iter(result.iter()));
        assert_eq!(result.len(), 3);
        assert_eq!(result.get(0).map(|r| r.id.haystack_index), Some(4));

        ranker.keep_order(Some(true));
        let result = recv.recv_timeout(timeout)?;
        println!("{:?}", Vec::from_iter(result.iter()));
        assert_eq!(result.len(), 3);
        assert_eq!(result.get(0).map(|r| r.id.haystack_index), Some(0));

        ranker.haystack_clear();
        let result = recv.recv_timeout(timeout)?;
        assert_eq!(result.len(), 0);

        Ok(())
    }
}
