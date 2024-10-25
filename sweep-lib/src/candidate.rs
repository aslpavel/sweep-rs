use crate::{
    common::{json_from_slice_seed, LockExt, VecDeserializeSeed},
    rpc::{RpcParams, RpcPeer},
    widgets::ProcessOutput,
    Haystack, HaystackPreview, Positions, Process, ProcessCommandArg, ProcessCommandBuilder, Theme,
};
use anyhow::Error;
use futures::Stream;
use serde::{
    de::{self, DeserializeSeed},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    str::FromStr,
    sync::{Arc, RwLock},
};
use surf_n_term::{
    glyph::GlyphDeserializer,
    rasterize::SVG_COLORS,
    view::{
        Align, ArcView, Axis, BoxView, Container, Flex, IntoView, Justify, Margins, Text, View,
        ViewCache, ViewDeserializer,
    },
    CellWrite, Face, FaceDeserializer, Glyph, Size, TerminalWaker, RGBA,
};
use tokio::io::{AsyncBufReadExt, AsyncRead};

#[derive(Debug, PartialEq)]
struct CandidateInner {
    /// Searchable fields shown on left
    target: Vec<Field<'static>>,
    /// Fields with additional information show on the right
    right: Vec<Field<'static>>,
    /// Amount of space reserved for the right fields
    right_offset: usize,
    /// Face used to fill right fields
    right_face: Option<Face>,
    /// Fields to generate preview [Haystack::preview]
    preview: Vec<Field<'static>>,
    /// Preview flex value
    preview_flex: f64,
    /// Preview haystack position (offset in [Position] to preview match)
    preview_haystack_position: usize,
    /// Extra fields extracted from candidate object during parsing, this
    /// can be useful when candidate has some additional data associated with it
    extra: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    inner: Arc<CandidateInner>,
}

impl Candidate {
    /// Create new candidate entry
    ///
    /// ## Arguments:
    ///  - `target`: Searchable fields shown on left
    ///  - `extra`: Extra payload
    ///  - `right`: Fields with additional information show on the right
    ///  - `right_offset`: Amount of space reserved for the right fields
    ///  - `right_face`: Default face for right fields
    ///  - `preview`: Fields to be shown on preview [Haystack::preview]
    ///  - `preview_flex`: Preview view flex value
    pub fn new(
        target: Vec<Field<'static>>,
        extra: Option<HashMap<String, Value>>,
        right: Vec<Field<'static>>,
        right_offset: usize,
        right_face: Option<Face>,
        preview: Vec<Field<'static>>,
        preview_flex: f64,
    ) -> Self {
        let preview_haystack_position = fields_haystack(&target)
            .chain(fields_haystack(&right))
            .count();
        Self {
            inner: Arc::new(CandidateInner {
                target,
                extra: extra.unwrap_or_default(),
                right,
                right_offset,
                right_face,
                preview,
                preview_flex: preview_flex.max(0.0),
                preview_haystack_position,
            }),
        }
    }

    /// Extra data passed with candidate
    pub fn extra(&self) -> &HashMap<String, Value> {
        &self.inner.extra
    }

    /// Construct from string
    pub fn from_string(
        string: String,
        delimiter: char,
        field_selector: Option<&FieldSelector>,
    ) -> Self {
        let mut fields: Vec<Field<'static>> = split_inclusive(delimiter, string.as_ref())
            .map(|field| Field::from(field.to_owned()))
            .collect();
        if let Some(field_selector) = field_selector {
            let fields_len = fields.len();
            fields.iter_mut().enumerate().for_each(|(index, field)| {
                field.active = field_selector.matches(index, fields_len)
            });
        }
        Self::new(fields, None, Vec::new(), 0, None, Vec::new(), 0.0)
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

    /// Searchable fields shown on left
    pub fn target(&self) -> &[Field<'_>] {
        &self.inner.target
    }

    /// Searchable characters
    pub fn haystack(&self) -> impl Iterator<Item = char> + '_ {
        fields_haystack(&self.inner.target)
            .chain(fields_haystack(&self.inner.right))
            .chain(fields_haystack(&self.inner.preview))
    }

    /// Fields with additional information show on the right
    pub fn right(&self) -> &[Field<'_>] {
        &self.inner.right
    }

    /// Amount of space reserved for the right fields
    pub fn right_offset(&self) -> usize {
        self.inner.right_offset
    }

    /// Face used to fill right fields
    pub fn right_face(&self) -> Option<Face> {
        self.inner.right_face
    }

    /// Initialize RpcPeer
    pub fn setup(peer: RpcPeer, waker: TerminalWaker, ctx: CandidateContext) {
        // register field
        peer.register("field_register", {
            let ctx = ctx.clone();
            let view_cache: Arc<dyn ViewCache> = Arc::new(ctx.clone());
            let waker = waker.clone();
            move |mut params: RpcParams| {
                let ctx = ctx.clone();
                let waker = waker.clone();
                let view_cache = view_cache.clone();
                async move {
                    let field: Field = {
                        let ctx_inner = ctx.inner.read().expect("lock poisoned");
                        let field_seed = FieldDeserializer {
                            colors: &ctx_inner.named_colors,
                            view_cache: Some(view_cache),
                        };
                        params.take_seed(field_seed, 0, "field")?
                    };
                    let ref_id_opt: Option<i64> = params.take_opt(1, "id")?;
                    let ref_id = ctx.inner.with_mut(move |inner| {
                        let ref_id = ref_id_opt.unwrap_or(inner.field_refs.len() as i64);
                        inner.field_refs.insert(FieldRef(ref_id), field);
                        ref_id
                    });
                    let _ = waker.wake();
                    Ok(ref_id)
                }
            }
        });

        // view register
        peer.register("view_register", {
            let ctx = ctx.clone();
            let view_cache: Arc<dyn ViewCache> = Arc::new(ctx.clone());
            let waker = waker.clone();
            move |mut params: RpcParams| {
                let ctx = ctx.clone();
                let view_cache = view_cache.clone();
                let waker = waker.clone();
                async move {
                    let view = {
                        let ctx_inner = ctx.inner.read().expect("lock poisoned");
                        let seed =
                            ViewDeserializer::new(Some(&ctx_inner.named_colors), Some(view_cache));
                        params.take_seed(&seed, 0, "view")?
                    };
                    let ref_id_opt: Option<i64> = params.take_opt(1, "ref")?;
                    let ref_id = ctx.inner.with_mut(move |inner| {
                        let ref_id = ref_id_opt.unwrap_or(inner.view_cache.len() as i64);
                        inner.view_cache.insert(ref_id, view);
                        ref_id
                    });
                    let _ = waker.wake();
                    Ok(ref_id)
                }
            }
        });

        ctx.peer_set(peer);
    }
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for field in self.inner.target.iter() {
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
        if inner.extra.is_empty()
            && inner.target.len() == 1
            && inner.target[0].active
            && inner.right.is_empty()
            && inner.preview.is_empty()
        {
            self.to_string().serialize(serializer)
        } else {
            let mut map = serializer.serialize_map(Some(1 + inner.extra.len()))?;
            for (key, value) in inner.extra.iter() {
                map.serialize_entry(key, value)?;
            }
            if !inner.target.is_empty() {
                map.serialize_entry("target", &inner.target)?;
            }
            if !inner.right.is_empty() {
                map.serialize_entry("right", &inner.right)?;
            }
            if inner.right_offset != 0 {
                map.serialize_entry("right_offset", &inner.right_offset)?;
            }
            if let Some(face) = inner.right_face {
                map.serialize_entry("right_face", &face)?;
            }
            if !inner.preview.is_empty() {
                map.serialize_entry("preview", &inner.preview)?;
            }
            if inner.preview_flex != 0.0 {
                map.serialize_entry("preview_flex", &inner.preview_flex)?;
            }
            map.end()
        }
    }
}

/// Split string into chunks separated by `sep` char.
///
/// Separators a glued to the beginning of the chunk. Adjacent separators are treated as
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
    type Context = CandidateContext;

    fn haystack_scope<S>(&self, _ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.haystack().for_each(scope);
    }

    fn view(&self, ctx: &Self::Context, positions: &Positions, theme: &Theme) -> BoxView<'static> {
        // left side
        let mut positions_offset = 0;
        let left = fields_view(
            self.target(),
            positions,
            &mut positions_offset,
            ctx,
            theme.list_text,
            theme.list_highlight,
            theme.list_inactive,
            Axis::Horizontal,
        );

        // right side
        let right = fields_view(
            self.right(),
            positions,
            &mut positions_offset,
            ctx,
            theme.list_text,
            theme.list_highlight,
            theme.list_inactive,
            Axis::Horizontal,
        );

        let mut view = Flex::row()
            .justify(Justify::SpaceBetween)
            .add_flex_child(1.0, left);
        if !self.right().is_empty() {
            let mut right = Container::new(right).with_margins(Margins {
                left: 1,
                right: 1,
                ..Default::default()
            });
            if self.right_offset() > 0 {
                right = right
                    .with_horizontal(Align::Start)
                    .with_width(self.right_offset());
            }
            view.push_child_ext(right, None, self.right_face(), Align::Start);
        }
        view.boxed()
    }

    fn preview(
        &self,
        ctx: &Self::Context,
        positions: &Positions,
        theme: &Theme,
    ) -> Option<HaystackPreview> {
        if self.inner.preview.is_empty() {
            return None;
        }
        let mut positions_offset = self.inner.preview_haystack_position;
        let preview = fields_view(
            &self.inner.preview,
            positions,
            &mut positions_offset,
            ctx,
            theme.list_text,
            theme.list_highlight,
            theme.list_text,
            Axis::Vertical,
        );
        Some(HaystackPreview::new(
            preview.arc(),
            Some(self.inner.preview_flex),
        ))
    }

    fn preview_large(
        &self,
        ctx: &Self::Context,
        _positions: &Positions,
        _theme: &Theme,
    ) -> Option<HaystackPreview> {
        Some(HaystackPreview::new(ctx.preview_get(self)?.arc(), None))
    }
}

/// Extract searchable character from fields list
pub fn fields_haystack<'a>(fields: &'a [Field<'_>]) -> impl Iterator<Item = char> + 'a {
    fields
        .iter()
        .filter(|f| f.active && f.glyph.is_none())
        .flat_map(|f| f.text.chars())
}

/// Convert fields into [View]
#[allow(clippy::too_many_arguments)]
pub fn fields_view(
    fields: &[Field<'_>],
    positions: &Positions,
    positions_offset: &mut usize,
    candidate_context: &CandidateContext,
    face_default: Face,
    face_highlight: Face,
    face_inactive: Face,
    flex_axis: Axis,
) -> impl View {
    let mut flex = Flex::new(flex_axis);
    let mut has_views = false;
    let mut text = Text::new();
    for field in fields {
        text.set_face(face_default);
        let field = candidate_context.field_resolve(field);
        let field_face = field.face.unwrap_or_default();

        if field.active && field.glyph.is_none() && field.view.is_none() {
            // active field non glyph
            let face_highlight = face_highlight.overlay(&field_face);
            let face_default = face_default.overlay(&field_face);
            for c in field.text.chars() {
                if positions.get(*positions_offset) {
                    text.set_face(face_highlight);
                    text.put_char(c);
                } else {
                    text.set_face(face_default);
                    text.put_char(c);
                }
                *positions_offset += 1;
            }
        } else {
            // inactive field or glyph
            match field.glyph {
                Some(glyph) => {
                    text.set_face(face_default.overlay(&field_face));
                    text.put_glyph(glyph.clone());
                }
                None => {
                    text.put_fmt(&field.text, Some(face_inactive.overlay(&field_face)));
                }
            };
            // view
            if let Some(view) = field.view {
                if !text.is_empty() {
                    flex.push_child(text.take());
                }
                flex.push_child(view.boxed());
                has_views = true;
            }
        }
    }
    if has_views {
        if !text.is_empty() {
            flex.push_child(text.take());
        }
        flex.left_view()
    } else {
        text.right_view()
    }
}

struct CandidateContextInner {
    field_refs: HashMap<FieldRef, Field<'static>>,
    view_cache: HashMap<i64, ArcView<'static>>,
    named_colors: Arc<HashMap<String, RGBA>>,
    peer: Option<RpcPeer>,
    preview_process: Option<Process>,
    preview_output: Option<(Candidate, ProcessOutput)>,
}

#[derive(Clone)]
pub struct CandidateContext {
    inner: Arc<RwLock<CandidateContextInner>>,
}

impl CandidateContext {
    pub fn new() -> Self {
        let inner = CandidateContextInner {
            field_refs: HashMap::new(),
            view_cache: HashMap::new(),
            named_colors: Arc::new(SVG_COLORS.clone()),
            peer: None,
            preview_process: None,
            preview_output: None,
        };
        Self {
            inner: Arc::new(RwLock::new(inner)),
        }
    }

    /// Resolve field references
    pub fn field_resolve<'a>(&self, field: &'a Field<'_>) -> Field<'a> {
        if let Some(field_ref) = field.field_ref {
            if !self
                .inner
                .with(|inner| inner.field_refs.contains_key(&field_ref))
            {
                self.field_missing(field_ref)
            }
        }
        self.inner.with(|inner| field.resolve(&inner.field_refs))
    }

    /// Notify peer that field is missing, if connected
    pub fn field_missing(&self, field_ref: FieldRef) {
        if let Some(peer) = self.inner.with(|inner| inner.peer.clone()) {
            let _ = peer.notify_with_value("field_missing", serde_json::json!({"ref": field_ref}));
        }
    }

    /// Update stored named colors from [Theme]
    pub fn update_named_colors(&self, theme: &Theme) {
        let named_colors = theme.named_colors.clone();
        self.inner
            .with_mut(|inner| inner.named_colors = named_colors)
    }

    /// Set rpc peer
    pub fn peer_set(&self, peer: RpcPeer) {
        self.inner.with_mut(|inner| inner.peer.replace(peer));
    }

    /// Set preview command builder
    pub fn preview_set(&self, builder: ProcessCommandBuilder, waker: TerminalWaker) {
        self.inner.with_mut(|inner| {
            inner
                .preview_process
                .replace(Process::new(Some(builder), waker))
        });
    }

    pub(crate) fn preview_get(&self, candidate: &Candidate) -> Option<ProcessOutput> {
        self.inner.with_mut(|inner| match &inner.preview_output {
            Some((candidate_prev, output)) if candidate == candidate_prev => Some(output.clone()),
            _ => {
                let Some(proc) = &inner.preview_process else {
                    return None;
                };
                proc.spawn(candidate.target());
                let output = proc.into_view();
                inner.preview_output = Some((candidate.clone(), output.clone()));
                Some(output)
            }
        })
    }

    /// Parse [Candidate] from JSON bytes
    pub fn candidate_from_json(&self, slice: &[u8]) -> Result<Candidate, Error> {
        Ok(json_from_slice_seed(self, slice)?)
    }

    /// Parse [Candidate] from [Value]
    pub fn candidate_from_value(&self, value: Value) -> Result<Candidate, Error> {
        Ok(self.deserialize(value)?)
    }
}

impl Default for CandidateContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewCache for CandidateContext {
    fn get(&self, uid: i64) -> Option<ArcView<'static>> {
        match self.inner.with(|inner| inner.view_cache.get(&uid).cloned()) {
            None => {
                if let Some(peer) = self.inner.with(|inner| inner.peer.clone()) {
                    let _ = peer.notify_with_value("view_missing", serde_json::json!({"ref": uid}));
                }
                None
            }
            view => view,
        }
    }
}

impl<'de, 'a> DeserializeSeed<'de> for &'a CandidateContext {
    type Value = Candidate;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> DeserializeSeed<'de> for CandidateContext {
    type Value = Candidate;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(&self)
    }
}

impl<'de, 'a> de::Visitor<'de> for &'a CandidateContext {
    type Value = Candidate;
    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("String or Struct")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let fields = vec![Field::from(v.to_owned())];
        Ok(Candidate::new(
            fields,
            None,
            Vec::new(),
            0,
            None,
            Vec::new(),
            0.0,
        ))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: de::MapAccess<'de>,
    {
        let mut target = None;
        let mut extra = HashMap::new();
        let mut right = None;
        let mut right_offset = 0;
        let mut right_face = None;
        let mut preview = None;
        let mut preview_flex = 0.0;

        let view_cache: Arc<dyn ViewCache> = Arc::new(self.clone());
        let ctx = self.inner.read().map_err(de::Error::custom)?;
        let fields_seed = VecDeserializeSeed(FieldDeserializer {
            colors: &ctx.named_colors,
            view_cache: Some(view_cache),
        });
        let face_seed = FaceDeserializer {
            colors: &ctx.named_colors,
        };
        while let Some(name) = map.next_key::<Cow<'de, str>>()? {
            match name.as_ref() {
                "entry" | "fields" | "target" => {
                    target.replace(map.next_value_seed(fields_seed.clone())?);
                }
                "right" => {
                    right.replace(map.next_value_seed(fields_seed.clone())?);
                }
                "right_offset" | "offset" => {
                    right_offset = map.next_value()?;
                }
                "right_face" => {
                    right_face.replace(map.next_value_seed(face_seed.clone())?);
                }
                "preview" => {
                    preview.replace(map.next_value_seed(fields_seed.clone())?);
                }
                "preview_flex" => {
                    preview_flex = map.next_value()?;
                }
                _ => {
                    extra.insert(name.into_owned(), map.next_value()?);
                }
            }
        }
        Ok(Candidate::new(
            target.ok_or_else(|| de::Error::missing_field("entry or fields"))?,
            (!extra.is_empty()).then_some(extra),
            right.unwrap_or_default(),
            right_offset,
            right_face,
            preview.unwrap_or_default(),
            preview_flex,
        ))
    }
}

/// Previously registered field that is used as base of the field
///
/// Mainly used avoid constant sending of glyphs (icons)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldRef(pub(crate) i64);

/// Single theme-able part of the haystack
#[derive(Clone, Serialize)]
pub struct Field<'a> {
    /// Text content on the field
    pub text: Cow<'a, str>,
    /// Render glyph (if glyphs are disabled text is shown)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<Glyph>,
    /// [View] representing a field content
    #[serde(skip_serializing)]
    pub view: Option<ArcView<'static>>,
    /// Flag indicating if the should be used as part of search
    pub active: bool,
    /// Face used to override default one
    #[serde(skip_serializing_if = "Option::is_none")]
    pub face: Option<Face>,
    /// Base field value
    #[serde(skip_serializing_if = "Option::is_none", rename = "ref")]
    pub field_ref: Option<FieldRef>,
}

impl<'a> fmt::Debug for Field<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("Field");
        debug_struct
            .field("text", &self.text)
            .field("active", &self.active)
            .field("glyph", &self.glyph)
            .field("face", &self.face)
            .field("field_ref", &self.field_ref);
        if let Some(view) = &self.view {
            debug_struct.field("view", &view.debug(Size::new(20, 10)));
        }
        debug_struct.finish()
    }
}

impl<'a> std::cmp::PartialEq for Field<'a> {
    fn eq(&self, other: &Self) -> bool {
        let eq = self.text == other.text
            && self.active == other.active
            && self.glyph == other.glyph
            && self.face == other.face
            && self.field_ref == other.field_ref;
        if !eq {
            return false;
        }
        match (&self.view, &other.view) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }
    }
}

impl<'a> std::cmp::Eq for Field<'a> {}

impl<'a> Default for Field<'a> {
    fn default() -> Self {
        Self {
            text: Cow::Borrowed(""),
            glyph: None,
            view: None,
            active: true,
            face: None,
            field_ref: None,
        }
    }
}

impl<'a> Field<'a> {
    /// Create text field
    pub fn text(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    /// Create glyph field
    pub fn glyph(glyph: Glyph) -> Self {
        Self {
            glyph: Some(glyph),
            ..Default::default()
        }
    }

    /// Create new field with specified face
    pub fn face(self, face: Face) -> Self {
        Self {
            face: Some(face),
            ..self
        }
    }

    /// Create new field with specified active
    pub fn active(self, active: bool) -> Self {
        Self { active, ..self }
    }

    /// Create new field with specified field reference
    pub fn reference(self, field_ref: FieldRef) -> Self {
        Self {
            field_ref: Some(field_ref),
            ..self
        }
    }

    /// Resolve reference in the field
    pub fn resolve(&'a self, refs: &HashMap<FieldRef, Field<'static>>) -> Field<'a> {
        let Some(field_ref) = self.field_ref else {
            return self.borrow();
        };
        let Some(base) = refs.get(&field_ref).cloned() else {
            return self.borrow();
        };
        Self {
            text: if self.text.is_empty() {
                base.text
            } else {
                Cow::Borrowed(&self.text)
            },
            glyph: self.glyph.clone().or(base.glyph),
            view: self.view.clone().or(base.view),
            active: self.active,
            face: self.face.or(base.face),
            field_ref: None,
        }
    }

    /// Borrow field
    pub fn borrow(&'a self) -> Field<'a> {
        Self {
            text: Cow::Borrowed(&self.text),
            glyph: self.glyph.clone(),
            view: self.view.clone(),
            active: self.active,
            face: self.face,
            field_ref: self.field_ref,
        }
    }
}

impl<'a, 'b: 'a> From<&'b str> for Field<'a> {
    fn from(text: &'b str) -> Self {
        Self::text(text)
    }
}

impl From<String> for Field<'static> {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl<'a, 'b: 'a> From<Cow<'b, str>> for Field<'a> {
    fn from(text: Cow<'b, str>) -> Self {
        Self {
            text,
            ..Default::default()
        }
    }
}

impl<'a> ProcessCommandArg for Field<'a> {
    fn as_command_arg(&self) -> &str {
        &self.text
    }
}

impl<'de> Deserialize<'de> for Field<'static> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        FieldDeserializer {
            colors: &SVG_COLORS,
            view_cache: None,
        }
        .deserialize(deserializer)
    }
}

#[derive(Clone)]
pub struct FieldDeserializer<'a> {
    pub colors: &'a HashMap<String, RGBA>,
    pub view_cache: Option<Arc<dyn ViewCache>>,
}

impl<'de, 'a> DeserializeSeed<'de> for FieldDeserializer<'a> {
    type Value = Field<'static>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de, 'a> de::Visitor<'de> for FieldDeserializer<'a> {
    type Value = Field<'static>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("String, List<String | (String, bool)> or Struct")
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
        let mut view: Option<ArcView<'static>> = None;
        while let Some(name) = map.next_key::<Cow<'de, str>>()? {
            match name.as_ref() {
                "text" => {
                    text.replace(map.next_value()?);
                }
                "active" => {
                    active.replace(map.next_value()?);
                }
                "glyph" => {
                    glyph.replace(map.next_value_seed(&GlyphDeserializer {
                        colors: self.colors,
                    })?);
                }
                "face" => {
                    face.replace(map.next_value_seed(FaceDeserializer {
                        colors: self.colors,
                    })?);
                }
                "ref" => {
                    reference.replace(map.next_value()?);
                }
                "view" => {
                    view.replace(map.next_value_seed(&ViewDeserializer::new(
                        Some(self.colors),
                        self.view_cache.clone(),
                    ))?);
                }
                _ => {
                    map.next_value::<de::IgnoredAny>()?;
                }
            }
        }
        let text = text.unwrap_or(Cow::Borrowed::<'static>(""));
        Ok(Field {
            active: active.unwrap_or(glyph.is_none() && !text.is_empty()),
            text,
            glyph,
            view,
            face,
            field_ref: reference,
        })
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

    pub fn matches_iter(&self, size: usize) -> impl Iterator<Item = usize> + '_ {
        let mut index = 0;
        std::iter::from_fn(move || loop {
            if index >= size {
                return None;
            }
            index += 1;
            if self.matches(index - 1, size) {
                return Some(index - 1);
            }
        })
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
    use surf_n_term::Path;

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

        let selector = FieldSelector::from_str("1..3,-2")?;
        assert_eq!(selector.matches_iter(6).collect::<Vec<_>>(), vec![1, 2, 4]);

        Ok(())
    }

    #[test]
    fn test_split_inclusive() {
        let chunks: Vec<_> = split_inclusive(' ', "  one  павел two  ").collect();
        assert_eq!(chunks, vec!["  one", "  павел", " two", "  ",]);
    }

    #[test]
    fn test_serde_candidate() -> Result<(), Error> {
        let ctx = CandidateContext::new();

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
            String::new(),
            None,
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
            None,
            vec![Field {
                text: "preview".into(),
                ..Field::default()
            }],
            1.0,
        );
        let value = json!({
            "fields": [
                "one",
                ["two", false],
                {"text": "three", "active": false},
                {"text": "", "active": false, "glyph": glyph}
            ],
            "right": [{"face": "bg=#00ff00"}],
            "preview": [{"text": "preview"}],
            "preview_flex": 1.0,
            "offset": 7usize,
            "extra": 127i32
        });
        let candidate_string = serde_json::to_string(&candidate)?;
        let value_string = serde_json::to_string(&value)?;
        // note that glyph uses pointer equality
        println!("1.");
        assert_eq!(
            serde_json::to_value(&candidate).unwrap(),
            serde_json::to_value(ctx.candidate_from_json(candidate_string.as_bytes())?).unwrap()
        );
        println!("2.");
        println!("{}", value_string);
        assert_eq!(
            serde_json::to_value(&candidate).unwrap(),
            serde_json::to_value(ctx.candidate_from_json(value_string.as_bytes())?).unwrap(),
        );
        println!("3.");
        assert_eq!(
            serde_json::to_value(&candidate).unwrap(),
            serde_json::to_value(ctx.candidate_from_value(value)?).unwrap()
        );

        let candidate = Candidate::new(
            vec!["four".into()],
            None,
            Vec::new(),
            0,
            None,
            Vec::new(),
            0.0,
        );
        assert_eq!(
            candidate.inner,
            ctx.candidate_from_json("\"four\"".as_bytes())?.inner
        );
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
