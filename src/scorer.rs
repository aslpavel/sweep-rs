use std::{
    collections::BTreeSet,
    fmt::{self, Debug},
    ops::Deref,
    sync::Arc,
};

/// Heystack
///
/// Item that can scored against the niddle by the scorer.
pub trait Haystack: Debug + Clone + Send + Sync + 'static {
    /// Slice containing all searchable lowercased characters. Characters from
    /// the inactive fields will not be present in this slice.
    fn chars(&self) -> &[char];

    /// Fields
    ///
    /// Iterator over fields, only Ok items should be scored, and Err items
    /// should be ignored during scoring.
    fn fields(&self) -> Box<dyn Iterator<Item = Result<&str, &str>> + '_>;
}

#[derive(Clone)]
pub struct StringHaystack {
    string: String,
    chars: Vec<char>,
}

impl StringHaystack {
    fn new(string: &str) -> Self {
        let string = string.to_string();
        let chars = string.chars().collect();
        Self { string, chars }
    }
}

impl Deref for StringHaystack {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.string.as_ref()
    }
}

impl Debug for StringHaystack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.string.fmt(f)
    }
}

impl<S: AsRef<str>> From<S> for StringHaystack {
    fn from(string: S) -> Self {
        StringHaystack::new(string.as_ref())
    }
}

impl Haystack for StringHaystack {
    fn chars(&self) -> &[char] {
        self.chars.as_slice()
    }

    fn fields(&self) -> Box<dyn Iterator<Item = Result<&str, &str>> + '_> {
        Box::new(std::iter::once(self.string.as_ref()).map(Ok))
    }
}

/// Scorer
///
/// Scorer is used to score haystack against the niddle stored inside the scorer
pub trait Scorer: Send + Sync + Debug {
    /// Name of the scorer
    fn name(&self) -> &str;

    /// Actual scorer implementation which takes haystack as a dynamic referece.
    fn score_ref(&self, haystack: &[char]) -> Option<(Score, Positions)>;

    /// Generic implementation over anyting that implements `Haystack` trati.
    fn score<H>(&self, haystack: H) -> Option<ScoreResult<H>>
    where
        H: Haystack,
        Self: Sized,
    {
        let (score, positions) = self.score_ref(haystack.chars())?;
        Some(ScoreResult {
            haystack,
            score,
            positions,
        })
    }
}

/// Matched positions in heystack
pub type Positions = BTreeSet<usize>;

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
    fn score_ref(&self, haystack: &[char]) -> Option<(Score, Positions)> {
        (*self).score_ref(haystack)
    }
}

impl Scorer for Box<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn score_ref(&self, haystack: &[char]) -> Option<(Score, Positions)> {
        (**self).score_ref(haystack)
    }
}

impl Scorer for Arc<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn score_ref(&self, haystack: &[char]) -> Option<(Score, Positions)> {
        (**self).score_ref(haystack)
    }
}

pub type Score = f32;
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

/// Sub-string scorer
///
/// This scorer splits needle into words and finds each word as uninterrupted sequence of
/// characters inside the haystack.
#[derive(Debug, Clone)]
pub struct SubstrScorer {
    words: Vec<KMPPattern<char>>,
}

impl SubstrScorer {
    pub fn new(niddle: Vec<char>) -> Self {
        let words = niddle
            .split(|c| *c == ' ')
            .filter_map(|word| {
                if word.is_empty() {
                    None
                } else {
                    Some(KMPPattern::new(word.to_vec()))
                }
            })
            .collect();
        Self { words }
    }
}

impl Scorer for SubstrScorer {
    fn name(&self) -> &str {
        &"substr"
    }

    fn score_ref(&self, haystack: &[char]) -> Option<(Score, Positions)> {
        if self.words.is_empty() {
            return Some((SCORE_MAX, Positions::new()));
        }

        let mut positions = Positions::new();
        let mut match_start = 0;
        let mut match_end = 0;
        for (i, word) in self.words.iter().enumerate() {
            match_end += word.search(&haystack[match_end..])?;
            if i == 0 {
                match_start = match_end;
            }
            let word_start = match_end;
            match_end += word.len();
            positions.extend(word_start..match_end);
        }

        let match_start = match_start as Score;
        let match_end = match_end as Score;
        let heystack_len = haystack.len() as Score;
        let score = (match_start - match_end)
            + (match_end - match_start) / heystack_len
            + (match_start + 1.0).recip()
            + (heystack_len - match_end + 1.0).recip();

        Some((score, positions))
    }
}

/// Knuth-Morris-Pratt pattern
#[derive(Debug, Clone)]
pub struct KMPPattern<T> {
    niddle: Vec<T>,
    table: Vec<usize>,
}

impl<T: PartialEq> KMPPattern<T> {
    pub fn new(niddle: Vec<T>) -> Self {
        if niddle.is_empty() {
            return Self {
                niddle,
                table: Vec::new(),
            };
        }
        let mut table = vec![0; niddle.len()];
        let mut i = 0;
        for j in 1..niddle.len() {
            while i > 0 && niddle[i] != niddle[j] {
                i = table[i - 1];
            }
            if niddle[i] == niddle[j] {
                i += 1;
            }
            table[j] = i;
        }
        Self { niddle, table }
    }

    pub fn len(&self) -> usize {
        self.niddle.len()
    }

    pub fn is_empty(&self) -> bool {
        self.niddle.is_empty()
    }

    pub fn search(&self, haystack: impl AsRef<[T]>) -> Option<usize> {
        if self.niddle.is_empty() {
            return None;
        }
        let mut n_index = 0;
        for (h_index, h) in haystack.as_ref().iter().enumerate() {
            while n_index > 0 && self.niddle[n_index] != *h {
                n_index = self.table[n_index - 1];
            }
            if self.niddle[n_index] == *h {
                n_index += 1;
            }
            if n_index == self.niddle.len() {
                return Some(h_index + 1 - n_index);
            }
        }
        None
    }
}

/// Fuzzy scorrer
///
/// This will match any haystack item as long as the niddle is a sub-sequence of the heystack.
#[derive(Clone, Debug)]
pub struct FuzzyScorer {
    niddle: Vec<char>,
}

impl FuzzyScorer {
    pub fn new(niddle: Vec<char>) -> Self {
        Self { niddle }
    }

    fn bonus(haystack: &[char], bonus: &mut [Score]) {
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

    fn subseq(niddle: &[char], haystack: &[char]) -> bool {
        let mut n_iter = niddle.iter();
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

    // This function is only called when we know that niddle is a sub-string of
    // the haystack string.
    fn score_impl(niddle: &[char], haystack: &[char]) -> (Score, Positions) {
        let n_len = niddle.len();
        let h_len = haystack.len();

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
        for (i, n_char) in niddle.iter().enumerate() {
            let mut prev_score = SCORE_MIN;
            let gap_score = if i == n_len - 1 {
                SCORE_GAP_TRAILING
            } else {
                SCORE_GAP_INNER
            };
            for (j, h_char) in haystack.iter().enumerate() {
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
                if (match_required || (d.get(i, j) - m.get(i, j)).abs() < Score::EPSILON)
                    && d.get(i, j) != SCORE_MIN
                {
                    match_required = i > 0
                        && j > 0
                        && (m.get(i, j) - (d.get(i - 1, j - 1) + SCORE_MATCH_CONSECUTIVE)).abs()
                            < Score::EPSILON;
                    positions.insert(j);
                    break;
                }
            }
        }

        (m.get(n_len - 1, h_len - 1), positions)
    }
}

impl Scorer for FuzzyScorer {
    fn name(&self) -> &str {
        &"fuzzy"
    }

    fn score_ref(&self, haystack: &[char]) -> Option<(Score, Positions)> {
        if Self::subseq(self.niddle.as_ref(), haystack) {
            Some(Self::score_impl(self.niddle.as_ref(), haystack))
        } else {
            None
        }
    }
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

    #[test]
    fn test_fuzzy_scorer() {
        let niddle: Vec<_> = "one".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(FuzzyScorer::new(niddle));

        let result = scorer.score(StringHaystack::new(" on/e two")).unwrap();
        assert_eq!(
            result.positions,
            [1, 2, 4].iter().copied().collect::<BTreeSet<_>>()
        );
        assert!((result.score - 2.665).abs() < 0.001);

        assert!(scorer.score(StringHaystack::new("two")).is_none());
    }

    #[test]
    fn test_substr_scorer() {
        let niddle: Vec<_> = "one  ababc".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(niddle));
        let score = scorer
            .score(StringHaystack::new(" one babababcd "))
            .unwrap();
        assert_eq!(
            score.positions,
            [1, 2, 3, 8, 9, 10, 11, 12]
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
        );

        let niddle: Vec<_> = "o".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(niddle));
        let score = scorer.score(StringHaystack::new("one")).unwrap();
        assert_eq!(
            score.positions,
            [0].iter().copied().collect::<BTreeSet<_>>()
        );
    }
}
