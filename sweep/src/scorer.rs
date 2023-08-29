use crate::Theme;
use std::{
    cell::RefCell,
    cmp::Ordering,
    fmt::{self, Debug},
    sync::Arc,
};
use surf_n_term::view::{Text, View};

/// Haystack
///
/// Item that can scored against the needle by the scorer.
pub trait Haystack: Debug + Clone + Send + Sync + 'static {
    type Context: Clone + Send;

    /// Scope function is called with all characters one after another that will
    /// be searchable by [Scorer]
    fn haystack_scope<S>(&self, scope: S)
    where
        S: FnMut(char);

    /// Creates haystack view from matched positions and theme
    fn view(&self, _ctx: &Self::Context, positions: &Positions, theme: &Theme) -> Box<dyn View> {
        haystack_default_view(self, positions, theme).boxed()
    }

    /// Large preview of pointed item
    fn preview(
        &self,
        _ctx: &Self::Context,
        _positions: &Positions,
        _theme: &Theme,
    ) -> Option<HaystackPreview> {
        None
    }
}

/// Preview rendered for haystack item
pub struct HaystackPreview {
    /// Preview of the item
    pub(crate) view: Box<dyn View>,
    /// Flex value value of the view see [`surf_n_term::view::Flex`]
    pub(crate) flex: Option<f64>,
}

impl HaystackPreview {
    /// Create haystack preview item
    pub fn new(view: Box<dyn View>, flex: Option<f64>) -> Self {
        Self { view, flex }
    }

    /// Get view
    pub fn view(&self) -> &dyn View {
        &self.view
    }

    /// Get flex value
    pub fn flex(&self) -> Option<f64> {
        self.flex
    }
}

pub fn haystack_default_view(
    haystack: &impl Haystack,
    positions: &Positions,
    theme: &Theme,
) -> Text {
    let mut text = Text::new();
    let mut index = 0;
    haystack.haystack_scope(|char| {
        text.set_face(if positions.get(index) {
            theme.list_highlight
        } else {
            theme.list_text
        });
        text.put_char(char);
        index += 1;
    });
    text
}

impl Haystack for String {
    type Context = ();

    fn haystack_scope<S>(&self, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope)
    }
}

impl Haystack for &'static str {
    type Context = ();

    fn haystack_scope<S>(&self, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope)
    }
}

thread_local! {
    static HAYSTACK: RefCell<Vec<char>> = Default::default();
}

/// Scorer
///
/// Scorer is used to score haystack against the needle stored inside the scorer
pub trait Scorer: Send + Sync + Debug {
    /// Name of the scorer
    fn name(&self) -> &str;

    /// Needle
    fn needle(&self) -> &str;

    /// Actual scorer non generic implementation
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool;

    /// Generic implementation over anything that implements `Haystack` trait.
    fn score<H>(&self, haystack: H) -> Option<ScoreResult<H>>
    where
        H: Haystack,
        Self: Sized,
    {
        HAYSTACK.with(|target| {
            let mut target = target.borrow_mut();
            target.clear();
            haystack.haystack_scope(|char| target.extend(char::to_lowercase(char)));
            let mut score = Score::MIN;
            let mut positions = Positions::new(target.len());
            self.score_ref(target.as_slice(), &mut score, &mut positions)
                .then_some(ScoreResult {
                    haystack,
                    score,
                    positions,
                })
        })
    }
}

/// Result of the scoring
#[derive(Debug, Clone)]
pub struct ScoreResult<H> {
    pub haystack: H,
    // score of this match
    pub score: Score,
    // match positions in the haystack
    pub positions: Positions,
}

impl<'a, S: Scorer> Scorer for &'a S {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn needle(&self) -> &str {
        (**self).needle()
    }
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

impl Scorer for Box<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn needle(&self) -> &str {
        (**self).needle()
    }
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

impl Scorer for Arc<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn needle(&self) -> &str {
        (**self).needle()
    }
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Score(f32);

impl Score {
    pub const MIN: Score = Score(f32::NEG_INFINITY);
    pub const MAX: Score = Score(f32::INFINITY);

    pub const fn new(score: f32) -> Score {
        Score(score)
    }
}

impl fmt::Display for Score {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl PartialEq for Score {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        // This is copied from std::f32::total_cmp to avoid nightly requirement
        let mut left = self.0.to_bits() as i32;
        let mut right = other.0.to_bits() as i32;
        left ^= (((left >> 31) as u32) >> 1) as i32;
        right ^= (((right >> 31) as u32) >> 1) as i32;
        left.cmp(&right)
    }
}

const SCORE_GAP_LEADING: f32 = -0.005;
const SCORE_GAP_TRAILING: f32 = -0.005;
const SCORE_GAP_INNER: f32 = -0.01;
const SCORE_MATCH_CONSECUTIVE: f32 = 1.0;
const SCORE_MATCH_SLASH: f32 = 0.9;
const SCORE_MATCH_WORD: f32 = 0.8;
const SCORE_MATCH_CAPITAL: f32 = 0.7;
const SCORE_MATCH_DOT: f32 = 0.6;

/// Sub-string scorer
///
/// This scorer splits needle into words and finds each word as uninterrupted sequence of
/// characters inside the haystack.
#[derive(Debug, Clone)]
pub struct SubstrScorer {
    needle: String,
    words: Vec<KMPPattern<char>>,
}

impl SubstrScorer {
    pub fn new(needle: Vec<char>) -> Self {
        let words = needle
            .split(|c| *c == ' ')
            .filter_map(|word| {
                if word.is_empty() {
                    None
                } else {
                    Some(KMPPattern::new(word.to_vec()))
                }
            })
            .collect();
        Self {
            needle: needle.into_iter().collect(),
            words,
        }
    }
}

impl Scorer for SubstrScorer {
    fn name(&self) -> &str {
        "substr"
    }

    fn needle(&self) -> &str {
        self.needle.as_str()
    }

    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        positions.clear();
        if self.words.is_empty() {
            *score = Score::MAX;
            return true;
        }

        let mut match_start = 0;
        let mut match_end = 0;
        for (i, word) in self.words.iter().enumerate() {
            match_end += match word.search(&haystack[match_end..]) {
                Some(match_start) => match_start,
                None => return false,
            };
            if i == 0 {
                match_start = match_end;
            }
            let word_start = match_end;
            match_end += word.len();
            positions.extend(word_start..match_end);
        }

        let match_start = match_start as f32;
        let match_end = match_end as f32;
        let heystack_len = haystack.len() as f32;
        *score = Score::new(
            (match_start - match_end)
                + (match_end - match_start) / heystack_len
                + (match_start + 1.0).recip()
                + (heystack_len - match_end + 1.0).recip(),
        );
        true
    }
}

/// Knuth-Morris-Pratt pattern
#[derive(Debug, Clone)]
pub struct KMPPattern<T> {
    needle: Vec<T>,
    table: Vec<usize>,
}

impl<T: PartialEq> KMPPattern<T> {
    pub fn new(needle: Vec<T>) -> Self {
        if needle.is_empty() {
            return Self {
                needle,
                table: Vec::new(),
            };
        }
        let mut table = vec![0; needle.len()];
        let mut i = 0;
        for j in 1..needle.len() {
            while i > 0 && needle[i] != needle[j] {
                i = table[i - 1];
            }
            if needle[i] == needle[j] {
                i += 1;
            }
            table[j] = i;
        }
        Self { needle, table }
    }

    pub fn len(&self) -> usize {
        self.needle.len()
    }

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }

    /// Search for the match in the haystack, return start of the match on success
    pub fn search(&self, haystack: impl AsRef<[T]>) -> Option<usize> {
        if self.needle.is_empty() {
            return None;
        }
        let mut n_index = 0;
        for (h_index, h) in haystack.as_ref().iter().enumerate() {
            while n_index > 0 && self.needle[n_index] != *h {
                n_index = self.table[n_index - 1];
            }
            if self.needle[n_index] == *h {
                n_index += 1;
            }
            if n_index == self.needle.len() {
                return Some(h_index + 1 - n_index);
            }
        }
        None
    }
}

/// Fuzzy scorer
///
/// This will match any haystack item as long as the needle is a sub-sequence of the haystack.
#[derive(Clone, Debug)]
pub struct FuzzyScorer {
    needle: Vec<char>,
    needle_str: String,
}

thread_local! {
    static DATA_CELL: RefCell<Vec<f32>> = RefCell::new(Vec::new());
}

impl FuzzyScorer {
    pub fn new(needle: Vec<char>) -> Self {
        let needle_str = needle.iter().cloned().collect();
        Self { needle, needle_str }
    }

    fn bonus(haystack: &[char], bonus: &mut [f32]) {
        let mut c_prev = '/';
        for (i, c) in haystack.iter().enumerate() {
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
            c_prev = *c;
        }
    }

    fn subseq(needle: &[char], haystack: &[char]) -> bool {
        let mut n_iter = needle.iter();
        let mut n = if let Some(n) = n_iter.next() {
            n
        } else {
            return true;
        };
        for h in haystack {
            if n == h {
                n = if let Some(n_next) = n_iter.next() {
                    n_next
                } else {
                    return true;
                };
            }
        }
        false
    }

    // This function is only called when we know that needle is a sub-string of
    // the haystack string.
    fn score_impl(
        needle: &[char],
        haystack: &[char],
        score: &mut Score,
        positions: &mut Positions,
    ) -> bool {
        positions.clear();
        let n_len = needle.len();
        let h_len = haystack.len();

        if n_len == 0 || n_len == h_len {
            // full match
            *score = Score::MAX;
            positions.extend(0..n_len);
            return true;
        }

        // find scores
        // use thread local storage for all data needed for calculating score and positions
        let mut data = DATA_CELL.with(|data_cell| data_cell.take());
        data.clear();
        data.resize(n_len * h_len * 2 + h_len, 0.0);

        let (bonus_score, matrix_data) = data.split_at_mut(h_len);
        let (d_data, m_data) = matrix_data.split_at_mut(n_len * h_len);
        Self::bonus(haystack, bonus_score);
        let mut d = ScoreMatrix::new(h_len, d_data); // best score ending with needle[..i]
        let mut m = ScoreMatrix::new(h_len, m_data); // best score for needle[..i]
        for (i, n_char) in needle.iter().enumerate() {
            let mut prev_score = f32::NEG_INFINITY;
            let gap_score = if i == n_len - 1 {
                SCORE_GAP_TRAILING
            } else {
                SCORE_GAP_INNER
            };
            for (j, h_char) in haystack.iter().enumerate() {
                if n_char == h_char {
                    let score = if i == 0 {
                        (j as f32) * SCORE_GAP_LEADING + bonus_score[j]
                    } else if j != 0 {
                        let a = m.get(i - 1, j - 1) + bonus_score[j];
                        let b = d.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE;
                        a.max(b)
                    } else {
                        f32::NEG_INFINITY
                    };
                    prev_score = score.max(prev_score + gap_score);
                    d.set(i, j, score);
                } else {
                    prev_score += gap_score;
                    d.set(i, j, f32::NEG_INFINITY);
                }
                m.set(i, j, prev_score);
            }
        }

        // find positions
        let mut match_required = false;
        let mut j = h_len;
        for i in (0..n_len).rev() {
            while j > 0 {
                j -= 1;
                if (match_required || (d.get(i, j) - m.get(i, j)).abs() < f32::EPSILON)
                    && d.get(i, j) != f32::NEG_INFINITY
                {
                    match_required = i > 0
                        && j > 0
                        && (m.get(i, j) - (d.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE)).abs()
                            < f32::EPSILON;
                    positions.set(j);
                    break;
                }
            }
        }
        *score = Score::new(m.get(n_len - 1, h_len - 1));

        DATA_CELL.with(move |data_cell| data_cell.replace(data));
        true
    }
}

impl Scorer for FuzzyScorer {
    fn name(&self) -> &str {
        "fuzzy"
    }

    fn needle(&self) -> &str {
        &self.needle_str
    }

    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        Self::subseq(self.needle.as_ref(), haystack)
            && Self::score_impl(self.needle.as_ref(), haystack, score, positions)
    }
}

struct ScoreMatrix<'a> {
    data: &'a mut [f32],
    width: usize,
}

impl<'a> ScoreMatrix<'a> {
    fn new<'b: 'a>(width: usize, data: &'b mut [f32]) -> Self {
        Self { data, width }
    }

    fn get(&self, row: usize, col: usize) -> f32 {
        self.data[row * self.width + col]
    }

    fn set(&mut self, row: usize, col: usize, val: f32) {
        self.data[row * self.width + col] = val;
    }
}

/// Position set implemented as bit-set
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Positions {
    chunks: smallvec::SmallVec<[u64; 3]>,
}

impl Positions {
    pub fn new(size: usize) -> Self {
        let chunks_size = if size == 0 { 0 } else { ((size - 1) >> 6) + 1 };
        let mut chunks = smallvec::SmallVec::new();
        chunks.resize(chunks_size, 0);
        Self { chunks }
    }

    /// set specified index as selected
    pub fn set(&mut self, index: usize) {
        let (index, mask) = Self::offset(index);
        self.chunks[index] |= mask;
    }

    /// check if index is present
    pub fn get(&self, index: usize) -> bool {
        let (index, mask) = Self::offset(index);
        if let Some(chunk) = self.chunks.get(index) {
            chunk & mask != 0
        } else {
            false
        }
    }

    /// unset all
    pub fn clear(&mut self) {
        for chunk in self.chunks.iter_mut() {
            *chunk = 0;
        }
    }

    /// given index return chunk_index and chunk_mask
    fn offset(index: usize) -> (usize, u64) {
        let chunk_index = index >> 6; // index / 64
        let chunk_mask = 1u64 << (index - (chunk_index << 6));
        (chunk_index, chunk_mask)
    }
}

impl std::fmt::Debug for Positions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(
                self.into_iter()
                    .enumerate()
                    .filter_map(|(i, s)| s.then_some(i)),
            )
            .finish()
    }
}

impl Extend<usize> for Positions {
    fn extend<T: IntoIterator<Item = usize>>(&mut self, iter: T) {
        for item in iter {
            self.set(item)
        }
    }
}

pub struct PositionsIter<'a> {
    positions: &'a Positions,
    index: usize,
}

impl<'a> Iterator for PositionsIter<'a> {
    type Item = bool;

    fn next(&mut self) -> Option<Self::Item> {
        let (index, mask) = Positions::offset(self.index);
        if index < self.positions.chunks.len() {
            self.index += 1;
            Some(self.positions.chunks[index] & mask != 0)
        } else {
            None
        }
    }
}

impl<'a> IntoIterator for &'a Positions {
    type Item = bool;
    type IntoIter = PositionsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        PositionsIter {
            positions: self,
            index: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_knuth_morris_pratt() {
        let pattern = KMPPattern::new("acat".bytes().collect());
        assert_eq!(pattern.table, vec![0, 0, 1, 0]);

        let pattern = KMPPattern::new("acacagt".bytes().collect());
        assert_eq!(pattern.table, vec![0, 0, 1, 2, 3, 0, 0]);

        let pattern = KMPPattern::new("abcdabd".bytes().collect());
        assert_eq!(Some(13), pattern.search("abcabcdababcdabcdabde"));

        let pattern = KMPPattern::new("abcabcd".bytes().collect());
        assert_eq!(pattern.table, vec![0, 0, 0, 1, 2, 3, 0]);
    }

    #[test]
    fn test_subseq() {
        let subseq = FuzzyScorer::subseq;
        let one: Vec<_> = "one".chars().collect();
        let net: Vec<_> = "net".chars().collect();
        let one1: Vec<_> = "on/e".chars().collect();
        let wone: Vec<_> = "w o ne".chars().collect();
        assert!(subseq(&one, &one1));
        assert!(subseq(&one, &wone));
        assert!(!subseq(&one, &net));
        assert!(subseq(&[], &one));
    }

    fn ps(items: impl AsRef<[usize]>) -> Positions {
        match items.as_ref().iter().max() {
            None => Positions::new(0),
            Some(max) => {
                let mut positions = Positions::new(max + 1);
                positions.extend(items.as_ref().iter().copied());
                positions
            }
        }
    }

    #[test]
    fn positions() {
        let p = ps([1, 15, 67, 300]);
        assert_eq!(format!("{p:?}"), "[1, 15, 67, 300]".to_string());
        assert_eq!(
            p.into_iter()
                .enumerate()
                .filter_map(|(i, m)| m.then(|| i))
                .collect::<Vec<_>>(),
            vec![1, 15, 67, 300]
        );
        assert_eq!(p.chunks.len(), 5);
        assert!(p.get(1));
        assert!(p.get(15));
        assert!(p.get(67));
        assert!(p.get(300));
    }

    #[test]
    fn test_fuzzy_scorer() {
        let needle: Vec<_> = "one".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(FuzzyScorer::new(needle));

        let result = scorer.score(" on/e two".to_string()).unwrap();
        assert_eq!(result.positions, ps([1, 2, 4]));
        assert!((result.score.0 - 2.665).abs() < 0.001);

        assert!(scorer.score("two".to_string()).is_none());
    }

    #[test]
    fn test_substr_scorer() {
        let needle: Vec<_> = "one  ababc".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(needle));
        let score = scorer.score(" one babababcd ".to_string()).unwrap();
        assert_eq!(score.positions, ps([1, 2, 3, 8, 9, 10, 11, 12]));

        let needle: Vec<_> = "o".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(needle));
        let score = scorer.score("one".to_string()).unwrap();
        assert_eq!(score.positions, ps([0]));
    }

    #[test]
    fn test_score() {
        assert!(Score::new(1.0) > Score::new(0.9));
        assert!(Score::new(1.0) == Score::new(1.0));
        assert!(Score::MIN < Score::MAX);
    }
}
