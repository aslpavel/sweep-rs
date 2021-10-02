use crate::{Field, Haystack};
use anyhow::Error;
use futures::Stream;
use serde::{de, ser::SerializeMap, Deserialize, Serialize};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    io::{BufRead, Read},
    str::FromStr,
    sync::Arc,
};
use tokio::io::{AsyncBufReadExt, AsyncRead};

#[derive(Debug, PartialEq, Eq)]
struct CandidateInner {
    // haystack fields
    fields: Vec<Field<'static>>,
    // searchable characters
    chars: Vec<char>,
    // extra fields extracted from candidate object during parsing, this
    // can be useful when candidate some additional data assocaited with it
    extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    inner: Arc<CandidateInner>,
}

impl Candidate {
    pub fn new(fields: Vec<Field<'static>>, extra: Option<HashMap<String, Value>>) -> Self {
        let chars = fields
            .iter()
            .filter_map(|f| {
                f.active
                    .then(|| f.text.chars().flat_map(char::to_lowercase))
            })
            .flatten()
            .collect();
        Self {
            inner: Arc::new(CandidateInner {
                fields,
                chars,
                extra: extra.unwrap_or_else(HashMap::new),
            }),
        }
    }

    pub fn extra(&self) -> &HashMap<String, Value> {
        &self.inner.extra
    }

    pub fn from_string(
        string: String,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
    ) -> Self {
        let fields = match field_selector {
            None => vec![string.into()],
            Some(field_selector) => {
                let mut fields: Vec<Field<'static>> = split_inclusive(delimiter, string.as_ref())
                    .map(|field| Field::from(field.to_owned()))
                    .collect();
                let fields_len = fields.len();
                fields.iter_mut().enumerate().for_each(|(index, field)| {
                    field.active = field_selector.matches(index, fields_len)
                });
                fields
            }
        };
        Self::new(fields, None)
    }

    /// Load batched stream of candidates from `AsyncRead`
    pub fn load<R>(
        read: R,
        delimiter: char,
        field_selector: Option<FieldSelector>,
    ) -> impl Stream<Item = Result<Vec<Candidate>, Error>>
    where
        R: AsyncRead + Unpin,
    {
        struct State<R> {
            reader: tokio::io::BufReader<R>,
            batch_size: usize,
            delimiter: char,
            field_selector: Option<FieldSelector>,
        }
        let init = State {
            reader: tokio::io::BufReader::new(read),
            batch_size: 10,
            delimiter,
            field_selector,
        };
        futures::stream::try_unfold(init, |mut state| async move {
            let mut batch = Vec::with_capacity(state.batch_size);
            loop {
                let mut line = String::new();
                let line_len = state.reader.read_line(&mut line).await?;
                if line_len == 0 || batch.len() >= state.batch_size {
                    break;
                };
                batch.push(Candidate::from_string(
                    line,
                    state.delimiter,
                    state.field_selector.as_ref(),
                ));
            }
            if batch.is_empty() {
                Ok(None)
            } else {
                Ok(Some((batch, state)))
            }
        })
    }

    pub fn load_with_callback<R, F>(
        reader: R,
        delimiter: char,
        field_selector: Option<FieldSelector>,
        callback: F,
    ) where
        R: Read + Send + 'static,
        F: Fn(Vec<Candidate>) + Send + 'static,
    {
        let mut buf_size = 10;
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(reader);
            let mut lines = reader.lines();
            let mut buf = Vec::with_capacity(buf_size);
            while let Some(Ok(line)) = lines.next() {
                buf.push(Candidate::from_string(
                    line,
                    delimiter,
                    field_selector.as_ref(),
                ));
                if buf.len() >= buf_size {
                    buf_size *= 2;
                    callback(std::mem::replace(&mut buf, Vec::with_capacity(buf_size)));
                }
            }
            callback(buf);
        });
    }
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for field in self.inner.fields.iter() {
            f.write_str(field.text.as_ref())?;
        }
        Ok(())
    }
}

impl Serialize for Candidate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(1 + self.inner.extra.len()))?;
        map.serialize_entry("entry", &self.inner.fields)?;
        for (key, value) in self.inner.extra.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Candidate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct CandidateVisitor;

        impl<'de> de::Visitor<'de> for CandidateVisitor {
            type Value = Candidate;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("string or dict with entry field")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let fields = vec![Field::from(v.to_owned())];
                Ok(Candidate::new(fields, None))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut fields = None;
                let mut extra = HashMap::new();
                while let Some(name) = map.next_key::<Cow<'de, str>>()? {
                    match name.as_ref() {
                        "entry" => {
                            fields.replace(map.next_value()?);
                        }
                        _ => {
                            extra.insert(name.into_owned(), map.next_value()?);
                        }
                    }
                }
                let fields = fields.ok_or_else(|| de::Error::missing_field("entry"))?;
                Ok(Candidate::new(
                    fields,
                    (!extra.is_empty()).then(move || extra),
                ))
            }
        }

        deserializer.deserialize_any(CandidateVisitor)
    }
}

/// Split string into chunks separated by `sep` char.
///
/// Separated a glued to the begining of the chunk. Adjacent separators are treated as
/// one separator.
pub fn split_inclusive(sep: char, string: &str) -> impl Iterator<Item = &'_ str> {
    SplitInclusive {
        indices: string.char_indices(),
        string,
        prev: sep,
        sep,
        start: 0,
    }
}

struct SplitInclusive<'a> {
    indices: std::str::CharIndices<'a>,
    string: &'a str,
    sep: char,
    prev: char,
    start: usize,
}

impl<'a> Iterator for SplitInclusive<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (index, ch) = match self.indices.next() {
                Some(index_char) => index_char,
                None => {
                    let string_len = self.string.len();
                    if self.start != string_len {
                        let chunk = &self.string[self.start..];
                        self.start = string_len;
                        return Some(chunk);
                    } else {
                        return None;
                    }
                }
            };
            let should_split = ch == self.sep && self.prev != self.sep;
            self.prev = ch;
            if should_split {
                let chunk = &self.string[self.start..index];
                self.start = index;
                return Some(chunk);
            }
        }
    }
}

impl Haystack for Candidate {
    fn chars(&self) -> &[char] {
        &self.inner.chars
    }

    fn fields(&self) -> Box<dyn Iterator<Item = Field<'_>> + '_> {
        Box::new(self.inner.fields.iter().map(Clone::clone))
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
pub struct FieldSelector(Arc<[FieldSelect]>);

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
        Ok(FieldSelector(selector.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn test_split_inclusive() {
        let chunks: Vec<_> = split_inclusive(' ', "  one  павел two  ").collect();
        assert_eq!(chunks, vec!["  one", "  павел", " two", "  ",]);
    }

    #[test]
    fn test_serde_candidate() -> Result<(), Error> {
        let mut extra = HashMap::new();
        extra.insert("extra".to_owned(), 127.into());
        let candidate = Candidate::new(
            vec![
                "one".into(),
                Field {
                    text: "two".into(),
                    active: false,
                },
                Field {
                    text: "three".into(),
                    active: false,
                },
            ],
            Some(extra),
        );
        let value = json!({
            "entry": [
                "one",
                ["two", false],
                {"text": "three", "active": false}
            ],
            "extra": 127
        });
        let candidate_string = serde_json::to_string(&candidate)?;
        let value_string = serde_json::to_string(&value)?;
        assert_eq!(candidate, serde_json::from_str(candidate_string.as_str())?);
        assert_eq!(candidate, serde_json::from_str(value_string.as_str())?);
        assert_eq!(candidate, serde_json::from_value(value)?);

        assert_eq!(
            Candidate::new(vec!["four".into()], None),
            serde_json::from_str("\"four\"")?,
        );

        // I'm not sure if we want this at all
        // assert_eq!(
        //     Candidate::new(vec!["five".into()], None),
        //     serde_json::from_value(json!({"entry": "five"}))?,
        // );

        Ok(())
    }
}
