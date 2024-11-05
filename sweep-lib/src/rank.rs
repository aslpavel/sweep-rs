use crate::{
    common::{byte_view_concat, LockExt},
    scorer::{ScoreArray, ScoreItem},
    FuzzyScorer, Haystack, Scorer, SubstrScorer,
};
use arrow_array::{builder::StringViewBuilder, Array, StringViewArray};
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::{
    iter,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const SCORE_CHUNK_SIZE: usize = 65_536;

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
        let mut builder = StringViewBuilder::new();
        let mut string_buf = String::new();
        for haystack in haystack {
            string_buf.clear();
            haystack.haystack_scope(ctx, |ch| string_buf.push(ch));
            builder.append_value(&string_buf);
        }
        self.sender
            .send(RankerCmd::HaystackAppend(builder.finish()))
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

enum RankerCmd {
    HaystackClear,
    HaystackAppend(StringViewArray),
    Needle(String),
    Scorer(ScorerBuilder),
    KeepOrder(Option<bool>),
    Sync(Arc<AtomicBool>),
}

fn ranker_worker<N>(receiver: Receiver<RankerCmd>, result: Arc<Mutex<Arc<RankedItems>>>, notify: N)
where
    N: Fn(Arc<RankedItems>) -> bool,
{
    let mut haystack_gen = 0usize;
    let mut haystack: StringViewArray = byte_view_concat([]);
    let mut haystack_appends: Vec<StringViewArray> = Vec::new();

    let mut needle = String::new();

    let mut keep_order = false;

    let mut scorer_builder = fuzzy_scorer();
    let mut scorer = scorer_builder("");
    let mut score = scorer.score(&haystack, Ok(0), !keep_order);

    let mut rank_gen = 0usize;
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
        for cmd in iter::once(cmd).chain(receiver.try_iter()) {
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
                    haystack_appends.push(haystack_append);
                }
                HaystackClear => {
                    action = All;
                    haystack_gen = haystack_gen.wrapping_add(1);
                    haystack_appends.clear();
                    haystack = byte_view_concat([]);
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
        if !haystack_appends.is_empty() {
            haystack = byte_view_concat(iter::once(&haystack).chain(&haystack_appends));
            haystack_appends.clear();
        }

        // rank
        let rank_instant = Instant::now();
        score = match action {
            DoNothing => {
                continue;
            }
            Notify => score,
            Offset(offset) => {
                // score new data
                score.merge(
                    scorer.score_par(
                        &haystack.slice(offset, haystack.len() - offset),
                        Ok(offset as u32),
                        false,
                        SCORE_CHUNK_SIZE,
                    ),
                    !keep_order,
                )
            }
            CurrentMatch => {
                // score current matches
                score.score_par(&scorer, !keep_order, SCORE_CHUNK_SIZE)
            }
            All => {
                // score all haystack elements
                scorer.score_par(&haystack, Ok(0), !keep_order, SCORE_CHUNK_SIZE)
            }
        };
        let rank_elapsed = rank_instant.elapsed();

        // update result
        // TODO: ArcSwap?
        rank_gen = rank_gen.wrapping_add(1);
        result.with_mut(|result| {
            *result = Arc::new(RankedItems {
                haystack_gen,
                score: score.clone(),
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
    score: ScoreArray,
    scorer: Arc<dyn Scorer>,
    duration: Duration,
    haystack_gen: usize,
    rank_gen: usize,
}

impl RankedItems {
    /// Number of matched items
    pub fn len(&self) -> usize {
        self.score.len()
    }

    pub fn is_empty(&self) -> bool {
        self.score.len() == 0
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
    pub fn generation(&self) -> (usize, usize) {
        (self.haystack_gen, self.rank_gen)
    }

    /// Get score result by rank index
    pub fn get(&self, rank_index: usize) -> Option<ScoreItem<'_>> {
        self.score.get(rank_index)
    }

    /// Find match index by haystack index
    pub fn find_match_index(&self, haystack_index: usize) -> Option<usize> {
        self.score
            .iter()
            .enumerate()
            .find_map(|(index, score)| (score.haystack_index == haystack_index).then_some(index))
    }

    /// Iterator over all matched items
    pub fn iter(&self) -> impl Iterator<Item = ScoreItem<'_>> {
        self.score.iter()
    }
}

impl Default for RankedItems {
    fn default() -> Self {
        Self {
            haystack_gen: Default::default(),
            score: Default::default(),
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
        assert_eq!(result.get(0).map(|r| r.haystack_index), Some(4));

        ranker.keep_order(Some(true));
        let result = recv.recv_timeout(timeout)?;
        println!("{:?}", Vec::from_iter(result.iter()));
        assert_eq!(result.len(), 3);
        assert_eq!(result.get(0).map(|r| r.haystack_index), Some(0));

        ranker.haystack_clear();
        let result = recv.recv_timeout(timeout)?;
        assert_eq!(result.len(), 0);

        Ok(())
    }
}
