use crate::Haystack;
use anyhow::Error;
use serde_json::Value;
use std::{
    fmt,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    str::FromStr,
    sync::Arc,
};

#[derive(Debug)]
struct CandidateInner {
    fields: Vec<Result<String, String>>,
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
    pub fn new(
        string: String,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
        json: Option<Value>,
    ) -> Self {
        let fields = match field_selector {
            None => vec![Ok(string)],
            Some(field_selector) => {
                let fields_count = split_inclusive(delimiter, string.as_ref()).count();
                split_inclusive(delimiter, string.as_ref())
                    .enumerate()
                    .map(|(index, field)| {
                        let field = field.to_owned();
                        if field_selector.matches(index, fields_count) {
                            Ok(field)
                        } else {
                            Err(field)
                        }
                    })
                    .collect()
            }
        };
        let chars = fields
            .iter()
            .filter_map(|f| Some(f.as_ref().ok()?.chars().flat_map(char::to_lowercase)))
            .flatten()
            .collect();
        Self {
            inner: Arc::new(CandidateInner {
                chars,
                fields,
                json,
            }),
        }
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
    ) -> Option<Self> {
        let string = match &json {
            Value::String(string) => string.clone(),
            Value::Object(map) => map.get("string")?.as_str()?.to_string(),
            _ => return None,
        };
        Some(Candidate::new(
            string,
            delimiter,
            field_selector,
            Some(json),
        ))
    }

    #[allow(unused)]
    pub fn load_file<P: AsRef<Path>>(
        path: P,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
    ) -> std::io::Result<Vec<Self>> {
        let file = BufReader::new(File::open(path)?);
        file.lines()
            .map(|l| Ok(Candidate::new(l?, delimiter, field_selector, None)))
            .collect()
    }

    pub fn load_from_reader<R, F>(
        reader: R,
        delimiter: char,
        field_selector: Option<FieldSelector>,
        sync: bool,
        callback: F,
    ) where
        R: Read + Send + Sync + 'static,
        F: Fn(Vec<Candidate>) + Send + Sync + 'static,
    {
        let mut buf_size = 10;
        let handle = std::thread::spawn(move || {
            let reader = BufReader::new(reader);
            let mut lines = reader.lines();
            let mut buf = Vec::with_capacity(buf_size);
            while let Some(Ok(line)) = lines.next() {
                buf.push(Candidate::new(
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
        if sync {
            handle.join().expect("haystack loader has failed");
        }
    }
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, field) in self.inner.fields.iter().enumerate() {
            if index != 0 {
                f.write_str(" ")?;
            }
            match field {
                Ok(field) => f.write_str(field.as_ref())?,
                Err(field) => f.write_str(field.as_ref())?,
            }
        }
        Ok(())
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

    fn fields(&self) -> Box<dyn Iterator<Item = Result<&str, &str>> + '_> {
        let iter = self.inner.fields.iter().map(|field| match field {
            Ok(field) => Ok(field.as_ref()),
            Err(field) => Err(field.as_ref()),
        });
        Box::new(iter)
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
}
