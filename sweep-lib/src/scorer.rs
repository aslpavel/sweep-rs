use crate::{common::string_view_filter, Haystack};
use arrow_array::{
    builder::{BinaryViewBuilder, Float32Builder, StringViewBuilder, UInt32Builder},
    Array, BinaryViewArray, Float32Array, StringViewArray, UInt32Array,
};
use std::{cell::RefCell, cmp::Ordering, fmt, sync::Arc};

thread_local! {
    static HAYSTACK: RefCell<Vec<char>> = Default::default();
}

/// Scorer
///
/// Scorer is used to score haystack against the needle stored inside the scorer
pub trait Scorer: Send + Sync + fmt::Debug {
    /// Name of the scorer
    fn name(&self) -> &str;

    /// Needle
    fn needle(&self) -> &str;

    /// Score haystack item
    ///
    /// Returns true if there was a match, false otherwise
    fn score_ref(
        &self,
        haystack: &[char],
        score: &mut Score,
        positions: PositionsRef<&mut [u8]>,
    ) -> bool;

    /// Generic implementation over anything that implements `Haystack` trait.
    fn score<H>(&self, ctx: &H::Context, haystack: H) -> Option<ScoreResult<H>>
    where
        H: Haystack,
        Self: Sized,
    {
        HAYSTACK.with(|target| {
            let mut target = target.borrow_mut();
            target.clear();
            haystack.haystack_scope(ctx, |char| target.extend(char::to_lowercase(char)));
            let mut score = Score::MIN;
            let mut positions = Positions::new(target.len());
            self.score_ref(target.as_slice(), &mut score, positions.as_mut())
                .then_some(ScoreResult {
                    haystack,
                    score,
                    positions,
                })
        })
    }

    /// Run scorer on an arrow of strings and ids (position in the haystack)
    fn score_arrow(
        &self,
        target: &StringViewArray,
        haystack_id: Option<&[u32]>,
        rank: bool,
    ) -> ScoreArray {
        if let Some(haystack_id) = haystack_id {
            assert_eq!(target.len(), haystack_id.len());
        }

        let mut target_buf: Vec<char> = Vec::new();
        let mut positions_buf: Vec<u8> = Vec::new();

        let mut target_builder = StringViewBuilder::new();
        let mut id_builder = UInt32Builder::new();
        let mut score_builder = Float32Builder::new();
        let mut positions_builder = BinaryViewBuilder::new();

        string_view_filter(target, &mut target_builder, |index, target| {
            target_buf.clear();
            target_buf.extend(target.chars().flat_map(char::to_lowercase));
            let mut score_local = Score::MIN;
            positions_buf.clear();
            positions_buf.resize(positions_data_size(target_buf.len()), 0);
            let positions = PositionsRef::new_data(positions_buf.as_mut());
            if !(self.score_ref(target_buf.as_slice(), &mut score_local, positions)) {
                return false;
            }

            id_builder.append_value(
                haystack_id.map_or_else(|| index as u32, |haystack_id| haystack_id[index]),
            );
            score_builder.append_value(score_local.0);
            positions_builder.append_value(positions_buf.as_slice());

            true
        });

        let score = score_builder.finish();
        let rank = rank.then(|| {
            let mut indices: Vec<_> = (0..score.len() as u32).collect();
            indices.sort_unstable_by_key(|index| Score(-score.value(*index as usize)));
            indices.into()
        });

        ScoreArray {
            target: target_builder.finish(),
            id: id_builder.finish(),
            score,
            positions: positions_builder.finish(),
            rank,
        }
    }
}

#[derive(Clone)]
pub struct ScoreArray {
    target: StringViewArray,
    id: UInt32Array,
    score: Float32Array,
    positions: BinaryViewArray,
    rank: Option<UInt32Array>,
}

impl ScoreArray {
    pub fn len(&self) -> usize {
        self.target.len()
    }

    pub fn get(&self, index: usize) -> Option<ScoreItem<'_>> {
        if index >= self.target.len() {
            return None;
        }
        let index = self
            .rank
            .as_ref()
            .map_or_else(|| index, |rank| rank.value(index) as usize);
        Some(ScoreItem {
            target: self.target.value(index),
            id: self.id.value(index) as usize,
            score: Score(self.score.value(index)),
            positions: PositionsRef::new_data(self.positions.value(index)),
        })
    }

    pub fn iter(&self) -> ScoreIter<'_> {
        ScoreIter {
            index: 0,
            score: self,
        }
    }
}

impl fmt::Debug for ScoreArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        list.entries(&*self);
        list.finish()
    }
}

pub struct ScoreIter<'a> {
    index: usize,
    score: &'a ScoreArray,
}

impl<'a> Iterator for ScoreIter<'a> {
    type Item = ScoreItem<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.score.get(self.index)?;
        self.index += 1;
        Some(value)
    }
}

impl<'a> IntoIterator for &'a ScoreArray {
    type Item = ScoreItem<'a>;
    type IntoIter = ScoreIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Debug)]
pub struct ScoreItem<'a> {
    pub target: &'a str,
    pub id: usize,
    pub score: Score,
    pub positions: PositionsRef<&'a [u8]>,
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

impl<'a, S: Scorer + ?Sized> Scorer for &'a S {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn needle(&self) -> &str {
        (**self).needle()
    }
    fn score_ref(
        &self,
        haystack: &[char],
        score: &mut Score,
        positions: PositionsRef<&mut [u8]>,
    ) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

impl<T: Scorer + ?Sized> Scorer for Box<T> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn needle(&self) -> &str {
        (**self).needle()
    }
    fn score_ref(
        &self,
        haystack: &[char],
        score: &mut Score,
        positions: PositionsRef<&mut [u8]>,
    ) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

impl<T: Scorer + ?Sized> Scorer for Arc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn needle(&self) -> &str {
        (**self).needle()
    }
    fn score_ref(
        &self,
        haystack: &[char],
        score: &mut Score,
        positions: PositionsRef<&mut [u8]>,
    ) -> bool {
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

    fn score_ref(
        &self,
        haystack: &[char],
        score: &mut Score,
        mut positions: PositionsRef<&mut [u8]>,
    ) -> bool {
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

const SCORE_GAP_LEADING: f32 = -0.005;
const SCORE_GAP_TRAILING: f32 = -0.005;
const SCORE_GAP_INNER: f32 = -0.01;
const SCORE_MATCH_CONSECUTIVE: f32 = 1.0;
const SCORE_MATCH_SLASH: f32 = 0.9;
const SCORE_MATCH_WORD: f32 = 0.8;
const SCORE_MATCH_CAPITAL: f32 = 0.7;
const SCORE_MATCH_DOT: f32 = 0.6;

thread_local! {
    static DATA_CELL: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
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
        mut positions: PositionsRef<&mut [u8]>,
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

        let (score_bonus, matrix_data) = data.split_at_mut(h_len);
        let (score_ends_data, score_best_data) = matrix_data.split_at_mut(n_len * h_len);
        Self::bonus(haystack, score_bonus);
        let mut score_ends = ScoreMatrix::new(h_len, score_ends_data); // best score ending with (needle[..i], haystack[..j])
        let mut score_best = ScoreMatrix::new(h_len, score_best_data); // best score for (needle[..i], haystack[..j])
        for (i, n_char) in needle.iter().enumerate() {
            let mut score_prev = f32::NEG_INFINITY;
            let score_gap = if i == n_len - 1 {
                SCORE_GAP_TRAILING
            } else {
                SCORE_GAP_INNER
            };
            for (j, h_char) in haystack.iter().enumerate() {
                if n_char == h_char {
                    let score = if i == 0 {
                        (j as f32) * SCORE_GAP_LEADING + score_bonus[j]
                    } else if j != 0 {
                        let best = score_best.get(i - 1, j - 1) + score_bonus[j];
                        let ends = score_ends.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE;
                        best.max(ends)
                    } else {
                        f32::NEG_INFINITY
                    };
                    score_prev = score.max(score_prev + score_gap);
                    score_ends.set(i, j, score);
                } else {
                    score_prev += score_gap;
                    score_ends.set(i, j, f32::NEG_INFINITY);
                }
                score_best.set(i, j, score_prev);
            }
        }

        // find positions
        let mut match_required = false;
        let mut j = h_len;
        for i in (0..n_len).rev() {
            while j > 0 {
                j -= 1;
                if (match_required || score_ends.get(i, j) == score_best.get(i, j))
                    && score_ends.get(i, j) != f32::NEG_INFINITY
                {
                    match_required = i > 0
                        && j > 0
                        && (score_best.get(i, j)
                            == (score_ends.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE));
                    positions.set(j);
                    break;
                }
            }
        }
        *score = Score::new(score_best.get(n_len - 1, h_len - 1));

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

    fn score_ref(
        &self,
        haystack: &[char],
        score: &mut Score,
        positions: PositionsRef<&mut [u8]>,
    ) -> bool {
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

    #[inline(always)]
    fn get(&self, row: usize, col: usize) -> f32 {
        self.data[row * self.width + col]
    }

    #[inline(always)]
    fn set(&mut self, row: usize, col: usize, val: f32) {
        self.data[row * self.width + col] = val;
    }
}

// sizeof(u8) = 1 << SHIFT
const POISTIONS_SHIFT: usize = 3;
// given index return `byte_index` and `byte_mask`
fn positions_offset(index: usize) -> (usize, u8) {
    let byte_index = index >> POISTIONS_SHIFT; // index // 8
    let byte_mask = 1u8 << (index - (byte_index << POISTIONS_SHIFT));
    (byte_index, byte_mask)
}
// calculate buffer size give maximum index
fn positions_data_size(size: usize) -> usize {
    if size == 0 {
        0
    } else {
        ((size - 1) >> POISTIONS_SHIFT) + 1
    }
}

/// Position set implemented as bit-set
#[derive(Clone, Hash)]
pub struct PositionsRef<D> {
    data: D,
}

pub type Positions = PositionsRef<smallvec::SmallVec<[u8; 16]>>;

impl Positions {
    pub fn new(size: usize) -> Self {
        let mut chunks = smallvec::SmallVec::new();
        chunks.resize(positions_data_size(size), 0);
        Self { data: chunks }
    }
}

impl<D: AsRef<[u8]>> PositionsRef<D> {
    pub fn new_data(data: D) -> Self {
        Self { data }
    }

    /// check if index is present
    pub fn get(&self, index: usize) -> bool {
        let (index, mask) = positions_offset(index);
        if let Some(chunk) = self.data.as_ref().get(index) {
            chunk & mask != 0
        } else {
            false
        }
    }

    pub fn as_ref(&self) -> PositionsRef<&[u8]> {
        PositionsRef {
            data: self.data.as_ref(),
        }
    }
}

impl<D: AsMut<[u8]>> PositionsRef<D> {
    /// set specified index as selected
    pub fn set(&mut self, index: usize) {
        let (index, mask) = positions_offset(index);
        self.data.as_mut()[index] |= mask;
    }

    pub fn as_mut(&mut self) -> PositionsRef<&mut [u8]> {
        PositionsRef {
            data: self.data.as_mut(),
        }
    }

    /// unset all
    pub fn clear(&mut self) {
        for chunk in self.data.as_mut().iter_mut() {
            *chunk = 0;
        }
    }
}

impl<DL, DR> std::cmp::PartialEq<PositionsRef<DR>> for PositionsRef<DL>
where
    DL: AsRef<[u8]>,
    DR: AsRef<[u8]>,
{
    fn eq(&self, other: &PositionsRef<DR>) -> bool {
        let data_left = self.data.as_ref();
        let data_right = other.data.as_ref();
        let (data_large, data_small) = if data_left.len() >= data_right.len() {
            (data_left, data_right)
        } else {
            (data_right, data_left)
        };
        if &data_large[..data_small.len()] != data_small {
            return false;
        }
        for byte in &data_large[data_small.len()..] {
            if *byte != 0 {
                return false;
            }
        }
        true
    }
}

impl<D: AsRef<[u8]>> std::cmp::Eq for PositionsRef<D> {}

impl<D: AsRef<[u8]>> std::fmt::Debug for PositionsRef<D> {
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

impl<D: AsMut<[u8]>> Extend<usize> for PositionsRef<D> {
    fn extend<T: IntoIterator<Item = usize>>(&mut self, iter: T) {
        for item in iter {
            self.set(item)
        }
    }
}

pub struct PositionsIter<'a> {
    data: &'a [u8],
    index: usize,
}

impl<'a> Iterator for PositionsIter<'a> {
    type Item = bool;

    fn next(&mut self) -> Option<Self::Item> {
        let (index, mask) = positions_offset(self.index);
        if index < self.data.len() {
            self.index += 1;
            Some(self.data[index] & mask != 0)
        } else {
            None
        }
    }
}

impl<'a, D> IntoIterator for &'a PositionsRef<D>
where
    D: AsRef<[u8]>,
{
    type Item = bool;
    type IntoIter = PositionsIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        PositionsIter {
            data: self.data.as_ref(),
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
        assert_eq!(p.data.len(), 38);
        assert!(p.get(1));
        assert!(p.get(15));
        assert!(p.get(67));
        assert!(p.get(300));
    }

    #[test]
    fn test_fuzzy_scorer() {
        let needle: Vec<_> = "one".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(FuzzyScorer::new(needle));

        let result = scorer.score(&(), " on/e two".to_string()).unwrap();
        assert_eq!(result.positions, ps([1, 2, 4]));
        assert!((result.score.0 - 2.665).abs() < 0.001);

        assert!(scorer.score(&(), "two".to_string()).is_none());
    }

    #[test]
    fn test_substr_scorer() {
        let needle: Vec<_> = "one  ababc".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(needle));
        let score = scorer.score(&(), " one babababcd ".to_string()).unwrap();
        assert_eq!(score.positions, ps([1, 2, 3, 8, 9, 10, 11, 12]));

        let needle: Vec<_> = "o".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(needle));
        let score = scorer.score(&(), "one".to_string()).unwrap();
        assert_eq!(score.positions, ps([0]));
    }

    #[test]
    fn test_scorer() {
        let haystack: StringViewArray = [
            "one",
            "two",
            "three",
            "four",
            "five",
            "six",
            "seven",
            "eight",
            "nine",
            "ten",
            "very long string to create buffer",
        ]
        .into_iter()
        .map(Some)
        .collect();

        let scorer = SubstrScorer::new("o".chars().collect());
        let result = scorer.score_arrow(&haystack, None, true);
        assert_eq!(result.len(), 4);
        assert_eq!(
            result.iter().map(|s| s.id).collect::<Vec<_>>(),
            &[0, 1, 3, 10]
        );

        let scorer = SubstrScorer::new("e".chars().collect());
        let result = scorer.score_arrow(&haystack, None, true);
        assert_eq!(result.len(), 8);
        assert_eq!(
            result.iter().map(|s| s.id).collect::<Vec<_>>(),
            &[0, 4, 8, 7, 9, 2, 6, 10]
        );
    }

    #[test]
    fn test_score() {
        assert!(Score::new(1.0) > Score::new(0.9));
        assert!(Score::new(1.0) == Score::new(1.0));
        assert!(Score::MIN < Score::MAX);
    }
}
