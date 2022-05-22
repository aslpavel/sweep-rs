use crate::LockExt;
use serde::{de, Deserialize, Deserializer, Serialize};
use std::{
    borrow::Cow,
    cell::RefCell,
    cmp::Ordering,
    collections::HashMap,
    fmt::{self, Debug},
    ops::Deref,
    sync::{Arc, RwLock},
};
use surf_n_term::{Face, Glyph};

/// Previously registered field that is used as base of the field
///
/// Mainly used avoid constant sending of glyphs (icons)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldRef(pub(crate) usize);

pub(crate) type FieldRefs = Arc<RwLock<HashMap<FieldRef, Field<'static>>>>;

/// Single theme-able part of the haystack
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Field<'a> {
    /// Text content on the field
    pub text: Cow<'a, str>,
    /// Flag indicating if the should be used as part of search
    pub active: bool,
    /// Render glyph (if glyphs are disabled text is shown)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<Glyph>,
    /// Face used to override default one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub face: Option<Face>,
    /// Base field value
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub field_ref: Option<FieldRef>,
}

impl<'a> Field<'a> {
    pub(crate) fn resolve(self, refs: &FieldRefs) -> Self {
        let field_ref = match self.field_ref {
            Some(field_ref) => field_ref,
            None => return self,
        };
        let base = match refs.with(|refs| refs.get(&field_ref).cloned()) {
            Some(base) => base,
            None => return self,
        };
        Self {
            text: if self.text.is_empty() {
                base.text
            } else {
                self.text
            },
            active: self.active,
            glyph: self.glyph.or(base.glyph),
            face: self.face.or(base.face),
            field_ref: None,
        }
    }
}

impl<'a> Default for Field<'a> {
    fn default() -> Self {
        Self {
            text: Cow::Borrowed(""),
            active: true,
            glyph: None,
            face: None,
            field_ref: None,
        }
    }
}

impl<'a, 'b: 'a> From<&'b str> for Field<'a> {
    fn from(text: &'b str) -> Self {
        Self {
            text: text.into(),
            active: true,
            glyph: None,
            face: None,
            field_ref: None,
        }
    }
}

impl From<String> for Field<'static> {
    fn from(text: String) -> Self {
        Self {
            text: text.into(),
            active: true,
            glyph: None,
            face: None,
            field_ref: None,
        }
    }
}

impl<'a, 'b: 'a> From<Cow<'b, str>> for Field<'a> {
    fn from(text: Cow<'b, str>) -> Self {
        Self {
            text,
            active: true,
            glyph: None,
            face: None,
            field_ref: None,
        }
    }
}

impl<'de, 'a> Deserialize<'de> for Field<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor<'a> {
            _phantom: &'a (),
        }

        impl<'de, 'a> de::Visitor<'de> for FieldVisitor<'a> {
            type Value = Field<'a>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("string, list of {string | (string, bool) | Field}")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Field {
                    text: v.to_owned().into(),
                    ..Field::default()
                })
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Field {
                    text: v.into(),
                    ..Field::default()
                })
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                Ok(Field {
                    text: seq
                        .next_element()?
                        .ok_or_else(|| de::Error::missing_field("text"))?,
                    active: seq.next_element()?.unwrap_or(true),
                    ..Field::default()
                })
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut text = None;
                let mut active = None;
                let mut glyph = None;
                let mut face = None;
                let mut reference = None;
                while let Some(name) = map.next_key::<Cow<'de, str>>()? {
                    match name.as_ref() {
                        "text" => {
                            text.replace(map.next_value()?);
                        }
                        "active" => {
                            active.replace(map.next_value()?);
                        }
                        "glyph" => {
                            glyph.replace(map.next_value()?);
                        }
                        "face" => {
                            face.replace(map.next_value()?);
                        }
                        "ref" => {
                            reference.replace(map.next_value()?);
                        }
                        _ => {
                            map.next_value::<de::IgnoredAny>()?;
                        }
                    }
                }
                let text = text.unwrap_or_else(|| Cow::Borrowed::<'static>(""));
                let text_not_empty = !text.is_empty();
                Ok(Field {
                    text,
                    active: active.unwrap_or(glyph.is_none() && text_not_empty),
                    glyph,
                    face,
                    field_ref: reference,
                })
            }
        }

        deserializer.deserialize_any(FieldVisitor::<'a> { _phantom: &() })
    }
}

/// Haystack
///
/// Item that can scored against the needle by the scorer.
pub trait Haystack: Debug + Clone + Send + Sync + 'static {
    /// Slice containing all searchable lowercase characters. Characters from
    /// the inactive fields will not be present in this slice.
    fn chars(&self) -> &[char];

    /// Fields
    fn fields(&self) -> Box<dyn Iterator<Item = Field<'_>> + '_>;
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

    fn fields(&self) -> Box<dyn Iterator<Item = Field<'_>> + '_> {
        Box::new(std::iter::once(Field {
            text: self.string.as_str().into(),
            ..Field::default()
        }))
    }
}

/// Scorer
///
/// Scorer is used to score haystack against the needle stored inside the scorer
pub trait Scorer: Send + Sync + Debug {
    /// Name of the scorer
    fn name(&self) -> &str;

    /// Actual scorer non generic implementation
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool;

    /// Generic implementation over anything that implements `Haystack` trait.
    fn score<H>(&self, haystack: H) -> Option<ScoreResult<H>>
    where
        H: Haystack,
        Self: Sized,
    {
        let mut score = Score::MIN;
        let mut positions = Positions::new();
        self.score_ref(haystack.chars(), &mut score, &mut positions)
            .then(move || ScoreResult {
                haystack,
                score,
                positions,
            })
    }
}

/// Matched positions in haystack
pub type Positions = Vec<usize>;

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
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

impl Scorer for Box<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn score_ref(&self, haystack: &[char], score: &mut Score, positions: &mut Positions) -> bool {
        (**self).score_ref(haystack, score, positions)
    }
}

impl Scorer for Arc<dyn Scorer> {
    fn name(&self) -> &str {
        (**self).name()
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
        Self { words }
    }
}

impl Scorer for SubstrScorer {
    fn name(&self) -> &str {
        "substr"
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
}

thread_local! {
    static DATA_CELL: RefCell<Vec<f32>> = RefCell::new(Vec::new());
}

impl FuzzyScorer {
    pub fn new(needle: Vec<char>) -> Self {
        Self { needle }
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
                    positions.push(j);
                    break;
                }
            }
        }
        positions.reverse();
        *score = Score::new(m.get(n_len - 1, h_len - 1));

        DATA_CELL.with(move |data_cell| data_cell.replace(data));
        true
    }
}

impl Scorer for FuzzyScorer {
    fn name(&self) -> &str {
        "fuzzy"
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;
    use serde_json::json;

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
        let needle: Vec<_> = "one".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(FuzzyScorer::new(needle));

        let result = scorer.score(StringHaystack::new(" on/e two")).unwrap();
        assert_eq!(result.positions, vec![1, 2, 4]);
        assert!((result.score.0 - 2.665).abs() < 0.001);

        assert!(scorer.score(StringHaystack::new("two")).is_none());
    }

    #[test]
    fn test_substr_scorer() {
        let needle: Vec<_> = "one  ababc".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(needle));
        let score = scorer
            .score(StringHaystack::new(" one babababcd "))
            .unwrap();
        assert_eq!(score.positions, vec![1, 2, 3, 8, 9, 10, 11, 12]);

        let needle: Vec<_> = "o".chars().collect();
        let scorer: Box<dyn Scorer> = Box::new(SubstrScorer::new(needle));
        let score = scorer.score(StringHaystack::new("one")).unwrap();
        assert_eq!(score.positions, vec![0]);
    }

    #[test]
    fn test_serde_field() -> Result<(), Error> {
        let mut field = Field {
            text: "field text π".into(),
            ..Field::default()
        };

        let expected = "{\"text\":\"field text π\",\"active\":true}";
        let value: serde_json::Value = serde_json::from_str(expected)?;
        assert_eq!(field, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&field)?);
        assert_eq!(field, serde_json::from_str(expected)?);

        assert_eq!(field, serde_json::from_str("\"field text π\"")?);
        assert_eq!(field, serde_json::from_value(json!("field text π"))?);

        assert_eq!(field, serde_json::from_str("[\"field text π\"]")?);
        assert_eq!(field, serde_json::from_value(json!(["field text π"]))?);

        assert_eq!(field, serde_json::from_str("[\"field text π\", true]")?);
        assert_eq!(
            field,
            serde_json::from_value(json!(["field text π", true]))?
        );

        field.active = false;
        let expected = "{\"text\":\"field text π\",\"active\":false}";
        let value: serde_json::Value = serde_json::from_str(expected)?;
        assert_eq!(field, serde_json::from_value(value)?);
        assert_eq!(expected, serde_json::to_string(&field)?);
        assert_eq!(field, serde_json::from_str(expected)?);

        assert_eq!(field, serde_json::from_str("[\"field text π\", false]")?);
        assert_eq!(
            field,
            serde_json::from_value(json!(["field text π", false]))?
        );

        Ok(())
    }

    #[test]
    fn test_score() {
        assert!(Score::new(1.0) > Score::new(0.9));
        assert!(Score::new(1.0) == Score::new(1.0));
        assert!(Score::MIN < Score::MAX);
    }
}
