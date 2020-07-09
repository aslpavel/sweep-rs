use std::{collections::BTreeSet, sync::Arc};

/// Heystack
///
/// Item that can scored against the niddle by the scorer.
pub trait Haystack {
    /// Iterator over characters in the heystack. Characters from
    /// inactive fields will not be present in this iterator.
    fn chars(&self) -> Box<dyn Iterator<Item = char> + '_>;

    /// Fields
    ///
    /// Iterator over fields, only Ok items should be scored, and Err items
    /// should be ignored during scoring.
    fn fields(&self) -> Box<dyn Iterator<Item = Result<&str, &str>> + '_>;

    /// Length of the iterator returned by `Self::chars`.
    fn len(&self) -> usize;
}

/// Scorer
///
/// Scorer is used to score haystack against provided niddle.
pub trait Scorer: Send + Sync {
    /// Name of the scorer
    fn name(&self) -> &str;

    /// Actual scorer implementation which takes haystack as a dynamic referece.
    fn score_ref(&self, niddle: &str, haystack: &dyn Haystack) -> Option<(Score, Positions)>;

    /// Generic implementation over anyting that implements `Haystack` trati.
    fn score<H>(&self, niddle: &str, haystack: H) -> Option<ScoreResult<H>>
    where
        H: Haystack,
        Self: Sized,
    {
        let (score, positions) = self.score_ref(niddle, &haystack)?;
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

impl<'a> Haystack for &'a str {
    fn chars(&self) -> Box<dyn Iterator<Item = char> + '_> {
        Box::new(str::chars(&self))
    }

    fn fields(&self) -> Box<dyn Iterator<Item = Result<&str, &str>> + '_> {
        Box::new(std::iter::once(*self).map(Ok))
    }

    fn len(&self) -> usize {
        str::chars(&self).count()
    }
}

impl<'a, S: Scorer> Scorer for &'a S {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn score_ref(&self, niddle: &str, haystack: &dyn Haystack) -> Option<(Score, Positions)> {
        (*self).score_ref(niddle, haystack)
    }
}

impl Scorer for Box<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn score_ref(&self, niddle: &str, haystack: &dyn Haystack) -> Option<(Score, Positions)> {
        (**self).score_ref(niddle, haystack)
    }
}

impl Scorer for Arc<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn score_ref(&self, niddle: &str, haystack: &dyn Haystack) -> Option<(Score, Positions)> {
        (**self).score_ref(niddle, haystack)
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
pub struct SubstrScorer;

impl SubstrScorer {
    pub fn new() -> Self {
        SubstrScorer
    }
}

impl Scorer for SubstrScorer {
    fn name(&self) -> &str {
        &"substr"
    }

    fn score_ref(&self, niddle: &str, haystack: &dyn Haystack) -> Option<(Score, Positions)> {
        if niddle.is_empty() {
            return Some((SCORE_MAX, Positions::new()));
        }

        let haystack: Vec<char> = haystack.chars().flat_map(char::to_lowercase).collect();
        let words: Vec<Vec<char>> = niddle
            .split(' ')
            .filter_map(|word| {
                if word.is_empty() {
                    None
                } else {
                    Some(word.chars().flat_map(char::to_lowercase).collect())
                }
            })
            .collect();

        let mut positions = Positions::new();
        let mut match_start = 0;
        let mut match_end = 0;
        for (i, word) in words.into_iter().enumerate() {
            match_end += KMPPattern::new(word.as_ref()).search(&haystack[match_end..])?;
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
pub struct KMPPattern<'a, T> {
    niddle: &'a [T],
    table: Vec<usize>,
}

impl<'a, T: PartialEq> KMPPattern<'a, T> {
    pub fn new(niddle: &'a [T]) -> Self {
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

    pub fn search(&self, haystack: &[T]) -> Option<usize> {
        let mut n_index = 0;
        for h_index in 0..haystack.len() {
            while n_index > 0 && self.niddle[n_index] != haystack[h_index] {
                n_index = self.table[n_index - 1];
            }
            if self.niddle[n_index] == haystack[h_index] {
                n_index += 1;
            }
            if n_index == self.niddle.len() {
                return Some(h_index - n_index + 1);
            }
        }
        None
    }
}

/// Fuzzy scorrer
///
/// This will match any haystack item as long as the niddle is a sub-sequence of the heystack.
pub struct FuzzyScorer;

impl FuzzyScorer {
    pub fn new() -> Self {
        FuzzyScorer
    }

    fn bonus(haystack: &dyn Haystack, bonus: &mut [Score]) {
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

    fn subseq(niddle: &str, haystack: &dyn Haystack) -> bool {
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
    fn score_impl(niddle: &str, haystack: &dyn Haystack) -> (Score, Positions) {
        let n_len = niddle.chars().flat_map(char::to_lowercase).count();
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
    fn name(&self) -> &str {
        &"fuzzy"
    }

    fn score_ref(&self, niddle: &str, haystack: &dyn Haystack) -> Option<(Score, Positions)> {
        if Self::subseq(niddle, haystack) {
            Some(Self::score_impl(niddle, haystack))
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
        let pattern = KMPPattern::new("acat".as_bytes());
        assert_eq!(pattern.table, vec![0, 0, 1, 0]);

        let pattern = KMPPattern::new("acacagt".as_bytes());
        assert_eq!(pattern.table, vec![0, 0, 1, 2, 3, 0, 0]);

        let pattern = KMPPattern::new("abcdabd".as_bytes());
        assert_eq!(Some(13), pattern.search("abcabcdababcdabcdabde".as_bytes()));

        let pattern = KMPPattern::new("abcabcd".as_bytes());
        assert_eq!(pattern.table, vec![0, 0, 0, 1, 2, 3, 0]);
    }

    #[test]
    fn test_subseq() {
        let subseq = FuzzyScorer::subseq;
        assert!(subseq("one", &"On/e"));
        assert!(subseq("one", &"w o ne"));
        assert!(!subseq("one", &"net"));
        assert!(subseq("", &"one"));
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
    fn test_substr_scorer() {
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new());

        let score = scorer.score("one  ababc", " one babababcd ").unwrap();
        assert_eq!(
            score.positions,
            [1, 2, 3, 8, 9, 10, 11, 12]
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
        );
    }
}
