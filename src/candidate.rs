use crate::{Field, Haystack};
use anyhow::{anyhow, Error};
use serde::{de, Deserialize, Serialize};
use serde_json::Value;
use std::{
    borrow::Cow,
    fmt,
    io::{BufRead, BufReader, Read},
    str::FromStr,
    sync::Arc,
};

#[derive(Debug)]
struct CandidateInner {
    fields: Vec<Field<'static>>,
    chars: Vec<char>,
    // base JSON object that was used to constract the candidate, this
    // can be useful when candidate some additional data assocaited with it
    json: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    inner: Arc<CandidateInner>,
}

impl Candidate {
    pub fn new(fields: Vec<Field<'static>>, json: Option<Value>) -> Self {
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
                json,
            }),
        }
    }

    pub fn from_string(
        string: String,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
        json: Option<Value>,
    ) -> Self {
        let fields = match field_selector {
            None => vec![Field {
                text: string.into(),
                active: true,
            }],
            Some(field_selector) => {
                let fields_count = split_inclusive(delimiter, string.as_ref()).count();
                split_inclusive(delimiter, string.as_ref())
                    .enumerate()
                    .map(|(index, field)| {
                        let text = Cow::Owned(field.to_owned());
                        if field_selector.matches(index, fields_count) {
                            Field { text, active: true }
                        } else {
                            Field {
                                text,
                                active: false,
                            }
                        }
                    })
                    .collect()
            }
        };
        Self::new(fields, json)
    }

    pub fn to_json(&self) -> Value {
        self.inner
            .json
            .as_ref()
            .map_or_else(|| Value::String(self.to_string()), |json| json.clone())
    }

    pub fn from_json(
        json: Value,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
    ) -> Result<Self, Error> {
        match &json {
            Value::String(string) => Ok(Self::from_string(
                string.clone(),
                delimiter,
                field_selector,
                Some(json),
            )),
            Value::Object(map) => {
                let entry = map
                    .get("entry")
                    .ok_or_else(|| anyhow!("entry attribute must be present"))?;
                match entry {
                    Value::String(string) => Ok(Self::from_string(
                        string.clone(),
                        delimiter,
                        field_selector,
                        Some(json),
                    )),
                    Value::Array(entry_fields) => {
                        let fields = entry_fields
                            .iter()
                            .map(|field| serde_json::from_value(field.clone()))
                            .collect::<Result<_, _>>()?;
                        Ok(Self::new(fields, Some(json)))
                    }
                    _ => Err(anyhow!("entry attribute must a string or an array")),
                }
            }
            _ => Err(anyhow!("string or object is expected")),
        }
    }

    pub fn load_from_reader<R, F>(
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
            let reader = BufReader::new(reader);
            let mut lines = reader.lines();
            let mut buf = Vec::with_capacity(buf_size);
            while let Some(Ok(line)) = lines.next() {
                buf.push(Candidate::from_string(
                    line,
                    delimiter,
                    field_selector.as_ref(),
                    None,
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
        for (index, field) in self.inner.fields.iter().enumerate() {
            if index != 0 {
                f.write_str(" ")?;
            }
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
        match &self.inner.json {
            Some(json) => json.serialize(serializer),
            None => self.inner.fields.serialize(serializer),
        }
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
                formatter.write_str("string or list of Fields")
            }

            fn visit_str<E>(self, _v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                todo!()
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
    fn test_json_candidate() -> Result<(), Error> {
        // string is parsed as usual string entry
        Candidate::from_json(json!("one two"), ' ', None)?;
        // JSON object must include "entry" field
        Candidate::from_json(json!({"entry": "one"}), ' ', None)?;
        // entry fields might be a list [(<string field>, <bool selected>)]
        Candidate::from_json(
            json!({"entry": ["two", ["three", false], ["four", true]]}),
            ' ',
            None,
        )?;
        Ok(())
    }
}
