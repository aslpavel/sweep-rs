use anyhow::Error;
use rayon::prelude::*;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, Write},
    path::Path,
    str::FromStr,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};
use surf_n_term::{
    render::{TerminalWritable, TerminalWriter},
    widgets::{Input, List, ListItems, Theme},
    DecMode, Face, FaceAttrs, Key, KeyMod, KeyName, Position, Surface, SurfaceMut, SystemTerminal,
    Terminal, TerminalAction, TerminalCommand, TerminalEvent, TerminalSurfaceExt,
};

fn main() -> Result<(), Error> {
    let debug_face: Face = "bg=#cc241d,fg=#ebdbb2".parse()?;
    let args = Args::new()?;
    let theme = args.theme.clone();

    // size
    let height_u = args.height;
    let height = args.height as i32;

    // initialize terminal
    let mut term = SystemTerminal::new()?;
    // term.duplicate_output("/tmp/sweep.log")?;
    term.execute(TerminalCommand::DecModeSet {
        enable: false,
        mode: DecMode::VisibleCursor,
    })?;

    // find current row offset
    let mut row_offset = 0;
    term.execute(TerminalCommand::CursorGet)?;
    match term.poll(Some(Duration::from_millis(500)))? {
        Some(TerminalEvent::CursorPosition { row, .. }) => {
            row_offset = row;
        }
        _ => (),
    }
    let term_size = term.size()?;
    if height_u > term_size.height {
        row_offset = 0;
    } else if row_offset + height_u > term_size.height {
        let scroll = row_offset + height_u - term_size.height;
        row_offset = term_size.height - height_u;
        term.execute(TerminalCommand::Scroll(scroll as i32))?;
    }

    // initialize ranker
    let waker = term.waker();
    let ranker = Ranker::new(args.keep_order, move || waker.wake().is_ok());
    Candidate::load_stdin(ranker.clone());

    // initialize widgets
    let mut input = Input::new();
    let mut list = List::new(RankerResultThemed::new(
        theme.clone(),
        Arc::new(RankerResult::<Candidate>::default()),
    ));

    // render loop
    let mut result = None;
    term.run_render(|term, event, view| -> Result<_, Error> {
        // handle events
        if let Some(event) = &event {
            match *event {
                TerminalEvent::Key(Key { name, mode }) if mode == KeyMod::CTRL => {
                    if name == KeyName::Char('c') {
                        return Ok(TerminalAction::Quit);
                    } else if name == KeyName::Char('m') {
                        if let Some(candidate) = list.current() {
                            result.replace(candidate.result.haystack);
                            return Ok(TerminalAction::Quit);
                        }
                    }
                }
                TerminalEvent::Resize(term_size) => {
                    if height_u > term_size.height {
                        row_offset = 0;
                    } else if row_offset + height_u > term_size.height {
                        row_offset = term_size.height - height_u;
                    }
                }
                _ => (),
            }
            input.handle(event);
            list.handle(event);
        }
        // restrict view
        let mut view = view.view_owned((row_offset as i32).., 1..-1);

        // update niddle
        ranker.niddle_set(input.get().collect());
        let result = ranker.result();

        // label
        let mut label_view = view.view_mut(0, ..);
        let label_face = Face::new(Some(theme.bg), Some(theme.accent), FaceAttrs::BOLD);
        let mut label = label_view.writer().face(label_face);
        write!(&mut label, " {} ", args.prompt)?;
        let mut label = label.face(label_face.invert());
        write!(&mut label, " ")?;
        let input_start = label.position().1 as i32;

        // stats
        let stats_str = format!(
            " {}/{} {:.2?} ",
            result.result.len(),
            result.haystack_size,
            result.duration,
        );
        let input_stop = -(stats_str.chars().count() as i32 + 1);
        let mut stats_view = view.view_mut(0, input_stop..);
        let stats_face = Face::new(Some(theme.bg), Some(theme.accent), FaceAttrs::EMPTY);
        let mut stats = stats_view.writer().face(stats_face.invert());
        write!(&mut stats, "")?;
        let mut stats = stats.face(stats_face);
        stats.write_all(stats_str.as_ref())?;

        // input
        input.render(&theme, view.view_mut(0, input_start..input_stop))?;

        // list
        if list.items().result.generation != result.generation {
            list.items_set(RankerResultThemed::new(theme.clone(), result));
        }
        list.render(&theme, view.view_mut(1..height, ..))?;

        // debug
        let mut debug = view.view_mut((height + 1)..(height + 3), ..);
        debug.erase(debug_face.bg);
        write!(
            &mut debug.writer().face(debug_face),
            "row:{} event:{:?} term:{:?}",
            row_offset,
            event,
            term.stats(),
        )?;

        Ok(TerminalAction::Wait)
    })?;

    // restore terminal
    term.execute(TerminalCommand::CursorTo(Position {
        row: row_offset,
        col: 0,
    }))?;
    term.poll(Some(Duration::new(0, 0)))?;
    std::mem::drop(term);

    // print result
    if let Some(result) = result {
        println!("{}", result.as_ref());
    }

    Ok(())
}

#[derive(Debug)]
pub struct Args {
    pub height: usize,
    pub prompt: String,
    pub theme: Theme,
    pub field_selector: Option<FieldSelector>,
    pub keep_order: bool,
}

impl Args {
    pub fn new() -> Result<Self, Error> {
        use clap::Arg;

        let matches = clap::App::new("sweep")
            .version(env!("CARGO_PKG_VERSION"))
            .about("Sweep is a command line fuzzy finder")
            .author("Pavel Aslanov")
            .arg(
                Arg::with_name("prompt")
                    .short("p")
                    .long("prompt")
                    .takes_value(true)
                    .help("prompt string"),
            )
            .arg(
                Arg::with_name("height")
                    .long("height")
                    .takes_value(true)
                    .help("height occupied by the sweep list"),
            )
            .arg(
                Arg::with_name("theme")
                    .long("theme")
                    .takes_value(true)
                    .help("specify theme as a list of comma sperated attributes"),
            )
            .arg(
                Arg::with_name("field_selector")
                    .long("nth")
                    .takes_value(true)
                    .help("comma-separated list of fields for limiting search scope"),
            )
            .arg(
                Arg::with_name("keep_order")
                    .long("keep-order")
                    .help("keep order (don't use ranking score)"),
            )
            .get_matches();

        let prompt = match matches.value_of("prompt") {
            Some(prompt) => prompt.to_string(),
            None => "INPUT".to_string(),
        };

        let height = matches
            .value_of("height")
            .map(|h| h.parse::<usize>())
            .transpose()?
            .unwrap_or(11);

        let theme = match matches.value_of("theme") {
            Some(theme) => theme.parse()?,
            None => Theme::light(),
        };

        let field_selector = matches
            .value_of("filed_selector")
            .map(|h| h.parse())
            .transpose()?;

        let keep_order = matches.is_present("keep_order");

        Ok(Self {
            prompt,
            height,
            theme,
            field_selector,
            keep_order,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Candidate(Arc<String>);

impl Candidate {
    fn new(string: String) -> Self {
        Self(Arc::new(string))
    }

    pub fn load_file<P: AsRef<Path>>(path: P) -> std::io::Result<Vec<Self>> {
        let file = BufReader::new(File::open(path)?);
        file.lines().map(|l| Ok(Candidate::new(l?))).collect()
    }

    fn load_stdin(ranker: Ranker<Candidate>) {
        let mut buf_size = 10;
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let handle = stdin.lock();
            let mut lines = handle.lines();
            let mut buf = Vec::with_capacity(buf_size);
            while let Some(Ok(line)) = lines.next() {
                buf.push(Candidate::new(line));
                if buf.len() >= buf_size {
                    buf_size *= 2;
                    ranker
                        .haystack_extend(std::mem::replace(&mut buf, Vec::with_capacity(buf_size)));
                }
            }
            ranker.haystack_extend(buf);
        });
    }
}

impl AsRef<str> for Candidate {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

#[derive(Debug, Clone, Copy)]
enum FieldSelect {
    All,
    Single(i32),
    RangeFrom(i32),
    RangeTo(i32),
    Range(i32, i32),
}

impl FieldSelect {
    fn matches(&self, index: usize, size: usize) -> bool {
        let index = index as i32;
        let size = size as i32;
        let resolve = |value: i32| -> i32 {
            if value < 0 {
                size + value
            } else {
                value
            }
        };
        use FieldSelect::*;
        match *self {
            All => return true,
            Single(single) => {
                if resolve(single) == index {
                    return true;
                }
            }
            RangeFrom(start) => {
                if resolve(start) <= index {
                    return true;
                }
            }
            RangeTo(end) => {
                println!("{} {}", end, resolve(end));
                if resolve(end) > index {
                    return true;
                }
            }
            Range(start, end) => {
                if resolve(start) <= index && resolve(end) > index {
                    return true;
                }
            }
        }
        false
    }
}

impl FromStr for FieldSelect {
    type Err = Error;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        if let Ok(single) = string.parse::<i32>() {
            return Ok(FieldSelect::Single(single));
        }
        let mut iter = string.splitn(2, "..");
        let mut value_next = || {
            iter.next()
                .and_then(|value| {
                    let value = value.trim();
                    if value.is_empty() {
                        None
                    } else {
                        Some(value.parse::<i32>())
                    }
                })
                .transpose()
        };
        match (value_next()?, value_next()?) {
            (Some(start), Some(end)) => Ok(FieldSelect::Range(start, end)),
            (Some(start), None) => Ok(FieldSelect::RangeFrom(start)),
            (None, Some(end)) => Ok(FieldSelect::RangeTo(end)),
            (None, None) => Ok(FieldSelect::All),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FieldSelector(Vec<FieldSelect>);

impl FieldSelector {
    pub fn matches(&self, index: usize, size: usize) -> bool {
        for select in self.0.iter() {
            if select.matches(index, size) {
                return true;
            }
        }
        false
    }
}

impl FromStr for FieldSelector {
    type Err = Error;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        let mut selector = Vec::new();
        for select in string.split(',') {
            selector.push(select.trim().parse()?);
        }
        Ok(FieldSelector(selector))
    }
}

#[derive(Debug)]
struct ScoreResultThemed<H> {
    result: ScoreResult<H>,
    face_default: Face,
    face_highlight: Face,
}

impl<H: AsRef<str>> TerminalWritable for ScoreResultThemed<H> {
    fn fmt(&self, writer: &mut TerminalWriter<'_>) -> std::io::Result<()> {
        for (i, c) in self.result.haystack.as_ref().chars().enumerate() {
            if self.result.positions.contains(&i) {
                writer.put_char(c, self.face_highlight);
            } else {
                writer.put_char(c, Face::default().with_fg(self.face_default.fg));
            }
        }
        Ok(())
    }
}

struct RankerResultThemed<H> {
    theme: Theme,
    result: Arc<RankerResult<H>>,
}

impl<H> RankerResultThemed<H> {
    fn new(theme: Theme, result: Arc<RankerResult<H>>) -> Self {
        Self { theme, result }
    }
}

impl<H: Clone + AsRef<str>> ListItems for RankerResultThemed<H> {
    type Item = ScoreResultThemed<H>;
    fn len(&self) -> usize {
        self.result.result.len()
    }
    fn get(&self, index: usize) -> Option<Self::Item> {
        self.result
            .result
            .get(index)
            .map(|result| ScoreResultThemed {
                result: result.clone(),
                face_default: self.theme.list_default,
                face_highlight: self.theme.cursor,
            })
    }
}

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
    FR: AsRef<str> + Send,
{
    let mut result: Vec<_> = haystack
        .into_par_iter()
        .filter_map(move |haystack| scorer.score(niddle, focus(haystack)))
        .collect();
    if !keep_order {
        result.par_sort_unstable_by(|a, b| {
            a.score.partial_cmp(&b.score).expect("Nan score").reverse()
        });
    }
    result
}

enum RankCmd<H> {
    Haystack(Vec<H>),
    Niddle(String),
}

struct RankerResult<H> {
    result: Vec<ScoreResult<H>>,
    duration: Duration,
    haystack_size: usize,
    generation: usize,
}

impl<H> Default for RankerResult<H> {
    fn default() -> Self {
        Self {
            result: Default::default(),
            duration: Duration::new(0, 0),
            haystack_size: 0,
            generation: 0,
        }
    }
}

#[derive(Clone)]
struct Ranker<H> {
    sender: mpsc::Sender<RankCmd<H>>,
    result: Arc<Mutex<Arc<RankerResult<H>>>>,
}

impl<H> Ranker<H>
where
    H: Clone + Send + Sync + 'static + AsRef<str>,
{
    pub fn new<N>(keep_order: bool, mut notify: N) -> Self
    where
        N: FnMut() -> bool + Send + 'static,
    {
        let result: Arc<Mutex<Arc<RankerResult<H>>>> = Default::default();
        let mut niddle = String::new();
        let mut haystack = Vec::new();
        let mut generation = 0usize;
        let scorer = FuzzyScorer::new();
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
                    let mut niddle_updated = false; // niddle was updated
                    let mut niddle_prefix = true; // previous niddle is a prefix of the new one
                    for cmd in Some(cmd).into_iter().chain(receiver.try_iter()) {
                        match cmd {
                            RankCmd::Haystack(haystack) => {
                                haystack_new.extend(haystack);
                            }
                            RankCmd::Niddle(niddle_new) if niddle_new != niddle => {
                                niddle_updated = true;
                                niddle_prefix = niddle_prefix && niddle_new.starts_with(&niddle);
                                niddle = niddle_new;
                            }
                            _ => continue,
                        }
                    }
                    haystack.extend(haystack_new.iter().cloned());

                    // rank haystack
                    let start = Instant::now();
                    let result_new = if !niddle_updated && haystack_new.is_empty() {
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
            .send(RankCmd::Haystack(haystack))
            .expect("failed to send haystack");
    }

    /// Set new needle
    pub fn niddle_set(&self, niddle: String) {
        self.sender
            .send(RankCmd::Niddle(niddle))
            .expect("failed to send niddle");
    }

    /// Get last result
    pub fn result(&self) -> Arc<RankerResult<H>> {
        self.result.with(|result| result.clone())
    }
}

type Score = f32;
const SCORE_MIN: Score = Score::NEG_INFINITY;
const SCORE_MAX: Score = Score::INFINITY;
const SCORE_GAP_LEADING: Score = -0.005;
const SCORE_GAP_TRAILING: Score = -0.005;
const SCORE_GAP_INNER: Score = -0.01;
const SCORE_MATCH_CONSECUTIVE: Score = 1.0;
const SCORE_MATCH_SLASH: Score = 0.9;
const SCORE_MATCH_WORD: Score = 0.8;
const SCORE_MATCH_CAPITAL: Score = 0.7;
const SCORE_MATCH_DOT: Score = 0.6;

pub type Positions = BTreeSet<usize>;

#[derive(Debug, Clone)]
pub struct ScoreResult<H> {
    pub haystack: H,
    // score of this match
    pub score: Score,
    // match positions in the haystack string
    pub positions: Positions,
}

struct ScoreMatrix<'a> {
    data: &'a mut [Score],
    width: usize,
}

impl<'a> ScoreMatrix<'a> {
    fn new<'b: 'a>(width: usize, data: &'b mut [Score]) -> Self {
        Self { data, width }
    }

    fn get(&self, row: usize, col: usize) -> Score {
        self.data[row * self.width + col]
    }

    fn set(&mut self, row: usize, col: usize, val: Score) {
        self.data[row * self.width + col] = val;
    }
}

pub trait Scorer {
    fn score_str(&self, niddle: &str, haystack: &str) -> Option<(Score, Positions)>;
    fn score<H>(&self, niddle: &str, haystack: H) -> Option<ScoreResult<H>>
    where
        H: AsRef<str>,
        Self: Sized,
    {
        let (score, positions) = self.score_str(niddle, haystack.as_ref())?;
        Some(ScoreResult {
            haystack,
            score,
            positions,
        })
    }
}

impl Scorer for Box<dyn Scorer> {
    fn score_str(&self, niddle: &str, haystack: &str) -> Option<(Score, Positions)> {
        (**self).score_str(niddle, haystack)
    }
}

impl Scorer for Arc<dyn Scorer> {
    fn score_str(&self, niddle: &str, haystack: &str) -> Option<(Score, Positions)> {
        (**self).score_str(niddle, haystack)
    }
}

impl<'a, S: Scorer> Scorer for &'a S {
    fn score_str(&self, niddle: &str, haystack: &str) -> Option<(Score, Positions)> {
        (*self).score_str(niddle, haystack)
    }
}

pub struct FuzzyScorer;

impl FuzzyScorer {
    pub fn new() -> Self {
        FuzzyScorer
    }

    fn bonus(haystack: &str, bonus: &mut [Score]) {
        let mut c_prev = '/';
        for (i, c) in haystack.chars().enumerate() {
            bonus[i] = if c.is_ascii_lowercase() || c.is_ascii_digit() {
                match c_prev {
                    '/' => SCORE_MATCH_SLASH,
                    '-' | '_' | ' ' => SCORE_MATCH_WORD,
                    '.' => SCORE_MATCH_DOT,
                    _ => 0.0,
                }
            } else if c.is_ascii_uppercase() {
                match c_prev {
                    '/' => SCORE_MATCH_SLASH,
                    '-' | '_' | ' ' => SCORE_MATCH_WORD,
                    '.' => SCORE_MATCH_DOT,
                    'a'..='z' => SCORE_MATCH_CAPITAL,
                    _ => 0.0,
                }
            } else {
                0.0
            };
            c_prev = c;
        }
    }

    fn subseq(niddle: &str, haystack: &str) -> bool {
        let mut n_iter = niddle.chars().flat_map(char::to_lowercase);
        let mut h_iter = haystack.chars().flat_map(char::to_lowercase);
        let mut n = if let Some(n) = n_iter.next() {
            n
        } else {
            return true;
        };
        while let Some(h) = h_iter.next() {
            if n == h {
                n = if let Some(n_next) = n_iter.next() {
                    n_next
                } else {
                    return true;
                };
            }
        }
        return false;
    }

    // This function is only called when we know that niddle is a sub-string of
    // the haystack string.
    fn score_impl(niddle: &str, haystack: &str) -> (Score, Positions) {
        let n_len = niddle.chars().flat_map(char::to_lowercase).count();
        let h_len = haystack.chars().flat_map(char::to_lowercase).count();

        if n_len == 0 || n_len == h_len {
            // full match
            return (SCORE_MAX, (0..n_len).collect());
        }

        // find scores
        // use single allocation for all data needed for calulating score and positions
        let mut data = vec![0.0; n_len * h_len * 2 + h_len];
        let (bonus_score, matrix_data) = data.split_at_mut(h_len);
        let (d_data, m_data) = matrix_data.split_at_mut(n_len * h_len);
        Self::bonus(haystack, bonus_score);
        let mut d = ScoreMatrix::new(h_len, d_data); // best score ending with niddle[..i]
        let mut m = ScoreMatrix::new(h_len, m_data); // best score for niddle[..i]
        for (i, n_char) in niddle.chars().flat_map(char::to_lowercase).enumerate() {
            let mut prev_score = SCORE_MIN;
            let gap_score = if i == n_len - 1 {
                SCORE_GAP_TRAILING
            } else {
                SCORE_GAP_INNER
            };
            for (j, h_char) in haystack.chars().flat_map(char::to_lowercase).enumerate() {
                if n_char == h_char {
                    let score = if i == 0 {
                        (j as Score) * SCORE_GAP_LEADING + bonus_score[j]
                    } else if j != 0 {
                        let a = m.get(i - 1, j - 1) + bonus_score[j];
                        let b = d.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE;
                        a.max(b)
                    } else {
                        SCORE_MIN
                    };
                    prev_score = score.max(prev_score + gap_score);
                    d.set(i, j, score);
                    m.set(i, j, prev_score);
                } else {
                    prev_score += gap_score;
                    d.set(i, j, SCORE_MIN);
                    m.set(i, j, prev_score);
                }
            }
        }

        // find positions
        let mut match_required = false;
        let mut positions = BTreeSet::new();
        let mut h_iter = (0..h_len).rev();
        for i in (0..n_len).rev() {
            while let Some(j) = h_iter.next() {
                if (match_required || d.get(i, j) == m.get(i, j)) && d.get(i, j) != SCORE_MIN {
                    match_required = i > 0
                        && j > 0
                        && m.get(i, j) == d.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE;
                    positions.insert(j);
                    break;
                }
            }
        }

        (m.get(n_len - 1, h_len - 1), positions)
    }
}

impl Scorer for FuzzyScorer {
    fn score_str(&self, niddle: &str, haystack: &str) -> Option<(Score, Positions)> {
        if Self::subseq(niddle, haystack) {
            Some(Self::score_impl(niddle, haystack))
        } else {
            None
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subseq() {
        let subseq = FuzzyScorer::subseq;
        assert!(subseq("one", "On/e"));
        assert!(subseq("one", "w o ne"));
        assert!(!subseq("one", "net"));
        assert!(subseq("", "one"));
    }

    #[test]
    fn test_fuzzy_scorer() {
        let scorer: Box<dyn Scorer> = Box::new(FuzzyScorer::new());

        let result = scorer.score("one", " on/e two").unwrap();
        assert_eq!(
            result.positions,
            [1, 2, 4].iter().copied().collect::<BTreeSet<_>>()
        );
        assert!((result.score - 2.665).abs() < 0.001);

        assert!(scorer.score("one", "two").is_none());
    }

    #[test]
    fn test_select() -> Result<(), Error> {
        let select = FieldSelect::from_str("..-1")?;
        assert!(!select.matches(3, 3));
        assert!(!select.matches(2, 3));
        assert!(select.matches(1, 3));
        assert!(select.matches(0, 3));

        let select = FieldSelect::from_str("-2..")?;
        assert!(select.matches(2, 3));
        assert!(select.matches(1, 3));
        assert!(!select.matches(0, 3));

        let select = FieldSelect::from_str("-2..-1")?;
        assert!(!select.matches(2, 3));
        assert!(select.matches(1, 3));
        assert!(!select.matches(0, 3));

        let select = FieldSelect::from_str("..")?;
        assert!(select.matches(2, 3));
        assert!(select.matches(1, 3));
        assert!(select.matches(0, 3));

        let selector = FieldSelector::from_str("..1,-1")?;
        assert!(selector.matches(2, 3));
        assert!(!selector.matches(1, 3));
        assert!(selector.matches(0, 3));

        Ok(())
    }
}
