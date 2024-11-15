use crate::{
    common::{byte_view_concat, LockExt},
    scorer::{ScoreArray, ScoreItem},
    FuzzyScorer, Haystack, Scorer, SubstrScorer,
};
use arrow_array::{builder::StringViewBuilder, Array, StringViewArray};
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::{
    collections::{HashMap, VecDeque},
    iter,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::{Duration, Instant},
};

const SCORE_CHUNK_SIZE: usize = 65_536;
pub static ALL_SCORER_BUILDERS: LazyLock<VecDeque<ScorerBuilder>> = LazyLock::new(|| {
    let mut builders = VecDeque::new();
    builders.push_back(fuzzy_scorer());
    builders.push_back(substr_scorer());
    builders
});

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

/// Find scorer by name, returns selected scorer builder
pub fn scorer_by_name(
    scorers: &mut VecDeque<ScorerBuilder>,
    name: Option<&str>,
) -> Option<ScorerBuilder> {
    if scorers.is_empty() {
        return None;
    }
    match name {
        None => {
            scorers.rotate_left(1);
            scorers.iter().next().cloned()
        }
        Some(name) => scorers
            .iter()
            .enumerate()
            .find_map(|(index, scorer)| {
                let index = (scorer("").name() == name).then_some(index)?;
                Some((index, scorer.clone()))
            })
            .map(|(index, scorer)| {
                scorers.swap(0, index);
                scorer
            }),
    }
}

pub struct Ranker {
    id: usize,
    store: Arc<Mutex<Arc<RankedItems>>>,
    ranker_thread: RankerThread,
}

impl Ranker {
    pub fn new(ranker_thread: RankerThread) -> Result<Self, anyhow::Error> {
        let store: Arc<Mutex<Arc<RankedItems>>> = Default::default();
        let id = ranker_thread.next_id.fetch_add(1, Ordering::SeqCst);
        ranker_thread
            .sender
            .send(RankerThreadCmd::Create {
                id,
                store: store.clone(),
            })
            .map_err(|_| anyhow::anyhow!("ranker thread is dead"))?;
        Ok(Self {
            id,
            store,
            ranker_thread,
        })
    }

    fn send(&self, cmd: RankerCmd) {
        self.ranker_thread
            .sender
            .send(RankerThreadCmd::Cmd { id: self.id, cmd })
            .expect("failed to send ranker cmd");
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
        self.send(RankerCmd::HaystackAppend(builder.finish()));
    }

    /// Clear haystack
    pub fn haystack_clear(&self) {
        self.send(RankerCmd::HaystackClear);
    }

    /// Set new needle
    pub fn needle_set(&self, needle: String) {
        self.send(RankerCmd::Needle(needle));
    }

    /// Set new scorer
    pub fn scorer_set(&self, scorer: ScorerBuilder) {
        self.send(RankerCmd::Scorer(scorer));
    }

    /// Whether to keep order of elements or sort by the best score
    pub fn keep_order(&self, toggle: Option<bool>) {
        self.send(RankerCmd::KeepOrder(toggle));
    }

    /// Get last result
    pub fn result(&self) -> Arc<RankedItems> {
        self.store.with(|result| result.clone())
    }

    /// Sets atomic to true once all requests before it has been processed
    pub fn sync(&self) -> Arc<AtomicBool> {
        let synced = Arc::new(AtomicBool::new(false));
        self.send(RankerCmd::Sync(synced.clone()));
        synced
    }

    pub fn ranker_thread(&self) -> &RankerThread {
        &self.ranker_thread
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

#[derive(Clone, Copy)]
enum RankAction {
    DoNothing,     // ignore
    Notify,        // only notify
    Offset(usize), // rank items starting from offset
    CurrentMatch,  // rank only current match
    All,           // rank everything
}

struct RankerState {
    haystack_gen: usize,
    haystack: StringViewArray,
    haystack_appends: Vec<StringViewArray>,
    needle: String,
    keep_order: bool,
    scorer_builder: ScorerBuilder,
    scorer: Arc<dyn Scorer>,
    score: ScoreArray,
    rank_gen: usize,
    synced: Vec<Arc<AtomicBool>>,
    action: RankAction,
    result_store: Arc<Mutex<Arc<RankedItems>>>,
}

impl RankerState {
    fn new(result_store: Arc<Mutex<Arc<RankedItems>>>) -> Self {
        let haystack = byte_view_concat([]);
        let keep_order = false;
        let scorer_builder = fuzzy_scorer();
        let scorer = scorer_builder("");
        let score = scorer.score(&haystack, Ok(0), !keep_order);
        Self {
            haystack_gen: 0,
            haystack: byte_view_concat([]),
            haystack_appends: Default::default(),
            needle: String::new(),
            keep_order,
            scorer_builder,
            scorer,
            score,
            rank_gen: 0,
            synced: Default::default(),
            action: RankAction::DoNothing,
            result_store,
        }
    }

    // process ranker cmd
    fn process(&mut self, cmd: RankerCmd) {
        use RankAction::*;
        use RankerCmd::*;

        match cmd {
            Needle(needle_new) => {
                self.action = match self.action {
                    DoNothing if needle_new == self.needle => return,
                    DoNothing | CurrentMatch if needle_new.starts_with(&self.needle) => {
                        CurrentMatch
                    }
                    _ => All,
                };
                self.needle = needle_new;
                self.scorer = (self.scorer_builder)(&self.needle);
            }
            Scorer(scorer_builder_new) => {
                self.action = All;
                self.scorer_builder = scorer_builder_new;
                self.scorer = (self.scorer_builder)(&self.needle);
            }
            HaystackAppend(haystack_append) => {
                self.action = match self.action {
                    DoNothing => Offset(self.haystack.len()),
                    Offset(offset) => Offset(offset),
                    _ => All,
                };
                self.haystack_appends.push(haystack_append);
            }
            HaystackClear => {
                self.action = All;
                self.haystack_gen = self.haystack_gen.wrapping_add(1);
                self.haystack_appends.clear();
                self.haystack = byte_view_concat([]);
            }
            KeepOrder(toggle) => {
                self.action = All;
                match toggle {
                    None => self.keep_order = !self.keep_order,
                    Some(value) => self.keep_order = value,
                }
            }
            Sync(sync) => {
                self.action = match self.action {
                    DoNothing => Notify,
                    _ => self.action,
                };
                self.synced.push(sync);
            }
        }
    }

    // do actual ranking
    fn rank(&mut self) -> Arc<RankedItems> {
        use RankAction::*;

        // collect haystack
        if !self.haystack_appends.is_empty() {
            self.haystack =
                byte_view_concat(iter::once(&self.haystack).chain(&self.haystack_appends));
            self.haystack_appends.clear();
        }

        // rank
        let rank_instant = Instant::now();
        self.score = match self.action {
            DoNothing => {
                return self.result_store.with(|result_store| result_store.clone());
            }
            Notify => self.score.clone(),
            Offset(offset) => {
                // score new data
                self.score.merge(
                    self.scorer.score_par(
                        &self.haystack.slice(offset, self.haystack.len() - offset),
                        Ok(offset as u32),
                        false,
                        SCORE_CHUNK_SIZE,
                    ),
                    !self.keep_order,
                )
            }
            CurrentMatch => {
                // score current matches
                self.score
                    .score_par(&self.scorer, !self.keep_order, SCORE_CHUNK_SIZE)
            }
            All => {
                // score all haystack elements
                self.scorer
                    .score_par(&self.haystack, Ok(0), !self.keep_order, SCORE_CHUNK_SIZE)
            }
        };
        let rank_elapsed = rank_instant.elapsed();

        // update result
        self.rank_gen = self.rank_gen.wrapping_add(1);
        let result = Arc::new(RankedItems {
            score: self.score.clone(),
            scorer: self.scorer.clone(),
            duration: rank_elapsed,
            haystack_gen: self.haystack_gen,
            rank_gen: self.rank_gen,
        });
        self.result_store.with_mut(|result_store| {
            *result_store = result.clone();
        });

        for sync in self.synced.drain(..) {
            sync.store(true, Ordering::Release);
        }

        result
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

enum RankerThreadCmd {
    Create {
        id: usize,
        store: Arc<Mutex<Arc<RankedItems>>>,
    },
    Cmd {
        id: usize,
        cmd: RankerCmd,
    },
}

#[derive(Clone)]
pub struct RankerThread {
    sender: Sender<RankerThreadCmd>,
    next_id: Arc<AtomicUsize>,
}

impl RankerThread {
    pub fn new<N>(notify: N) -> Self
    where
        N: Fn(usize, Arc<RankedItems>) -> bool + Send + 'static,
    {
        let (sender, receiver) = unbounded();
        std::thread::Builder::new()
            .name("sweep-ranker".to_string())
            .spawn(move || ranker_thread_main(receiver, notify))
            .expect("failed to start sweep-ranker thread");
        Self {
            sender,
            next_id: Default::default(),
        }
    }
}

fn ranker_thread_main<N>(receiver: Receiver<RankerThreadCmd>, notify: N)
where
    N: Fn(usize, Arc<RankedItems>) -> bool,
{
    let mut states: HashMap<usize, RankerState> = Default::default();
    loop {
        // process all pending commands
        let cmd = match receiver.recv() {
            Ok(cmd) => cmd,
            Err(_) => return,
        };
        for cmd in iter::once(cmd).chain(receiver.try_iter()) {
            match cmd {
                RankerThreadCmd::Create { id, store } => {
                    states.insert(id, RankerState::new(store));
                }
                RankerThreadCmd::Cmd { id, cmd } => {
                    let Some(state) = states.get_mut(&id) else {
                        continue;
                    };
                    state.process(cmd);
                }
            }
        }

        // rank
        for (id, state) in states.iter_mut() {
            let result = state.rank();
            // notify
            if !notify(*id, result) {
                return;
            }
        }

        // cleanup
        states.retain(|id, state| {
            let retain = Arc::strong_count(&state.result_store) > 1;
            if !retain {
                tracing::debug!(?id, "[ranker_thread_main] remove state");
            }
            retain
        });
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
        let ranker_thread = RankerThread::new(move |_, result| send.send(result).is_ok());
        let ranker = Ranker::new(ranker_thread)?;

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
