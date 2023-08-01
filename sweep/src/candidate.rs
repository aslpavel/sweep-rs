use crate::{Haystack, LockExt, Positions};
use anyhow::Error;
use futures::Stream;
use serde::{de, ser::SerializeMap, Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    io::Write,
    str::FromStr,
    sync::{Arc, RwLock},
};
use surf_n_term::{
    view::{BoxConstraint, Layout, Tree, View, ViewContext},
    Cell, Face, Glyph, Size, Surface, SurfaceMut, TerminalSurface, TerminalSurfaceExt,
};
use tokio::io::{AsyncBufReadExt, AsyncRead};

#[derive(Debug, PartialEq, Eq)]
struct CandidateInner {
    // haystack fields
    fields: Vec<Field<'static>>,
    // right aligned fields
    fields_right: Vec<Field<'static>>,
    // right aligned fields offset
    fields_right_offset: usize,
    // extra fields extracted from candidate object during parsing, this
    // can be useful when candidate has some additional data associated with it
    extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    inner: Arc<CandidateInner>,
}

impl Candidate {
    pub fn new(
        fields: Vec<Field<'static>>,
        extra: Option<HashMap<String, Value>>,
        fields_right: Vec<Field<'static>>,
        fields_right_offset: usize,
    ) -> Self {
        Self {
            inner: Arc::new(CandidateInner {
                fields,
                fields_right,
                fields_right_offset,
                extra: extra.unwrap_or_default(),
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
        Self::new(fields, None, Vec::new(), 0)
    }

    /// Read batched stream of candidates from `AsyncRead`
    pub fn from_lines<R>(
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

    fn fields(&self) -> impl Iterator<Item = Field<'_>> {
        self.inner.fields.iter().map(Field::borrow)
    }

    fn chars(&self) -> impl Iterator<Item = char> + '_ {
        self.inner
            .fields
            .iter()
            .filter_map(|f| (f.active && f.glyph.is_none()).then(|| f.text.chars()))
            .flatten()
    }

    fn fields_right(&self) -> impl Iterator<Item = Field<'_>> {
        self.inner.fields_right.iter().map(Field::borrow)
    }

    fn fields_right_offset(&self) -> usize {
        self.inner.fields_right_offset
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
        let inner = &self.inner;
        if inner.extra.is_empty() && inner.fields.len() == 1 && inner.fields[0].active {
            self.to_string().serialize(serializer)
        } else {
            let mut map = serializer.serialize_map(Some(1 + inner.extra.len()))?;
            map.serialize_entry("fields", &inner.fields)?;
            map.serialize_entry("right", &inner.fields_right)?;
            map.serialize_entry("offset", &inner.fields_right_offset)?;
            for (key, value) in inner.extra.iter() {
                map.serialize_entry(key, value)?;
            }
            map.end()
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
                formatter.write_str("string or dict with entry field")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let fields = vec![Field::from(v.to_owned())];
                Ok(Candidate::new(fields, None, Vec::new(), 0))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut fields = None;
                let mut fields_right = None;
                let mut fields_right_offset = 0;
                let mut extra = HashMap::new();
                while let Some(name) = map.next_key::<Cow<'de, str>>()? {
                    match name.as_ref() {
                        "entry" | "fields" => {
                            fields.replace(map.next_value()?);
                        }
                        "right" => {
                            fields_right.replace(map.next_value()?);
                        }
                        "right_offset" | "offset" => {
                            fields_right_offset = map.next_value()?;
                        }
                        _ => {
                            extra.insert(name.into_owned(), map.next_value()?);
                        }
                    }
                }
                let fields = fields.ok_or_else(|| de::Error::missing_field("entry or fields"))?;
                let fields_right = fields_right.unwrap_or_default();
                Ok(Candidate::new(
                    fields,
                    (!extra.is_empty()).then_some(extra),
                    fields_right,
                    fields_right_offset,
                ))
            }
        }

        deserializer.deserialize_any(CandidateVisitor)
    }
}

/// Split string into chunks separated by `sep` char.
///
/// Separated a glued to the beginning of the chunk. Adjacent separators are treated as
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
                    }
                    return None;
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
    fn haystack_scope<S>(&self, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope);
    }

    fn view(
        &self,
        positions: &crate::Positions,
        theme: &crate::Theme,
        refs: FieldRefs,
    ) -> Box<dyn surf_n_term::view::View> {
        Box::new(CandidteView {
            candidate: self.clone(),
            positions: positions.clone(),
            refs,
            face_default: theme.list_text,
            face_highlight: theme.list_highlight,
            face_inactive: theme.list_inactive,
        })
    }
}

struct CandidteView {
    candidate: Candidate,
    positions: Positions,
    refs: FieldRefs,
    face_default: Face,
    face_highlight: Face,
    face_inactive: Face,
}

impl View for CandidteView {
    fn render<'a>(
        &self,
        ctx: &ViewContext,
        surf: &'a mut TerminalSurface<'a>,
        layout: &Tree<Layout>,
    ) -> Result<(), surf_n_term::Error> {
        let mut surf = layout.apply_to(surf);

        // space reserved for right fields
        let offset = self.candidate.fields_right_offset();

        // render left side
        let mut index = 0;
        let mut left_view = if offset != 0 {
            surf.view_mut(.., ..-(offset as i32))
        } else {
            surf.view_mut(.., ..)
        };
        let mut writer = left_view.writer();
        for field in self.candidate.fields() {
            let field = field.resolve(&self.refs);
            let face_field = field.face.unwrap_or_default();
            if !field.active || field.glyph.is_some() {
                let face = self.face_inactive.overlay(&face_field);
                writer.face_set(face);
                match field.glyph {
                    Some(glyph) if ctx.has_glyphs() => {
                        writer.put(Cell::new_glyph(face, glyph));
                    }
                    _ => writer.write_all(field.text.as_bytes())?,
                }
                writer.face_set(self.face_default);
            } else {
                let face_highlight = self.face_highlight.overlay(&face_field);
                let face_default = self.face_default.overlay(&face_field);
                for c in field.text.chars() {
                    if self.positions.get(index) {
                        writer.put_char(c, face_highlight);
                    } else {
                        writer.put_char(c, face_default);
                    }
                    index += 1;
                }
            }
        }
        // render right side
        if surf.width() <= offset || offset == 0 {
            return Ok(());
        }
        let mut right_view = surf.view_mut(.., -(offset as i32)..);
        let mut writer = right_view.writer();
        for field in self.candidate.fields_right() {
            let field = field.resolve(&self.refs);
            let face = self.face_inactive.overlay(&field.face.unwrap_or_default());
            writer.face_set(face);
            match field.glyph {
                Some(glyph) if ctx.has_glyphs() => {
                    writer.put(Cell::new_glyph(face, glyph));
                }
                _ => writer.write_all(field.text.as_bytes())?,
            }
            writer.face_set(self.face_default);
        }

        Ok(())
    }

    fn layout(&self, _ctx: &ViewContext, ct: BoxConstraint) -> Tree<Layout> {
        let size = ct.max();
        let offset = self.candidate.fields_right_offset();
        let width = if size.width > offset {
            size.width - offset
        } else {
            size.width
        };
        let mut length = 0;
        for field in self.candidate.fields() {
            for c in field.text.chars() {
                length += match c {
                    '\r' => 0,
                    '\n' => width - length % width,
                    _ => 1,
                }
            }
        }
        let height = length / width + (if length % width != 0 { 1 } else { 0 });
        Tree::leaf(Layout::new().with_size(Size::new(height, size.width)))
    }
}

/// Previously registered field that is used as base of the field
///
/// Mainly used avoid constant sending of glyphs (icons)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldRef(pub(crate) usize);
pub type FieldRefs = Arc<RwLock<HashMap<FieldRef, Field<'static>>>>;

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
    /// resolve reference in the field
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

    pub fn borrow(&'a self) -> Field<'a> {
        Self {
            text: match &self.text {
                Cow::Borrowed(text) => Cow::Borrowed(text),
                Cow::Owned(text) => Cow::Borrowed(text),
            },
            active: self.active,
            glyph: self.glyph.clone(),
            face: self.face,
            field_ref: self.field_ref,
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
                let text = text.unwrap_or(Cow::Borrowed::<'static>(""));
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
        use FieldSelect::*;

        let index = index as i32;
        let size = size as i32;
        let resolve = |value: i32| -> i32 {
            if value < 0 {
                size + value
            } else {
                value
            }
        };

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
    use surf_n_term::{Face, Glyph, Path};

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
        extra.insert("extra".to_owned(), Value::from(127i32));
        let glyph = Glyph::new(
            Path::empty(),
            surf_n_term::FillRule::EvenOdd,
            None,
            surf_n_term::Size {
                height: 1,
                width: 2,
            },
        );
        let face: Face = "bg=#00ff00".parse()?;
        let candidate = Candidate::new(
            vec![
                "one".into(),
                Field {
                    text: "two".into(),
                    active: false,
                    ..Field::default()
                },
                Field {
                    text: "three".into(),
                    active: false,
                    ..Field::default()
                },
                Field {
                    glyph: Some(glyph.clone()),
                    active: false,
                    ..Field::default()
                },
            ],
            Some(extra),
            vec![Field {
                face: Some(face),
                active: false,
                ..Field::default()
            }],
            7,
        );
        let value = json!({
            "fields": [
                "one",
                ["two", false],
                {"text": "three", "active": false},
                {"text": "", "active": false, "glyph": glyph}
            ],
            "right": [{"face": "bg=#00ff00"}],
            "offset": 7usize,
            "extra": 127i32
        });
        let candidate_string = serde_json::to_string(&candidate)?;
        let value_string = serde_json::to_string(&value)?;
        // note that glyph uses pointer equality
        println!("=== mark ===: 1");
        assert_eq!(
            serde_json::to_value(&candidate).unwrap(),
            serde_json::to_value(serde_json::from_str::<Candidate>(
                candidate_string.as_str()
            )?)
            .unwrap()
        );
        println!("=== mark ===: 2");
        println!("{}", value_string);
        assert_eq!(
            serde_json::to_value(&candidate).unwrap(),
            serde_json::to_value(serde_json::from_str::<Candidate>(value_string.as_str())?)
                .unwrap(),
        );
        println!("=== mark ===: 3");
        assert_eq!(
            serde_json::to_value(&candidate).unwrap(),
            serde_json::to_value(serde_json::from_value::<Candidate>(value)?).unwrap()
        );

        let candidate = Candidate::new(vec!["four".into()], None, Vec::new(), 0);
        assert_eq!(candidate, serde_json::from_str("\"four\"")?);
        assert_eq!("\"four\"", serde_json::to_string(&candidate)?);

        Ok(())
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
}
