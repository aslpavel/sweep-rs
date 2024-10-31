use crate::{
    common::{AbortJoinHandle, LockExt},
    FieldSelector, Haystack, HaystackBasicPreview, HaystackDefaultView, HaystackPreview,
    PositionsRef,
};
use anyhow::Context;
use futures::{future, FutureExt, TryFutureExt};
use std::{
    borrow::Cow,
    cmp::max,
    collections::HashMap,
    io::Write,
    ops::Deref,
    process::Stdio,
    str::FromStr,
    sync::{Arc, Mutex, RwLock},
};
use surf_n_term::{
    rasterize::{PathBuilder, StrokeStyle, SVG_COLORS},
    view::{
        Axis, BoxConstraint, BoxView, Container, Flex, IntoView, Justify, Layout, ScrollBarFn,
        ScrollBarPosition, Text, Tree, TreeMut, View, ViewContext, ViewLayout, ViewMutLayout,
    },
    BBox, Cell, CellWrite, Color, Error, Face, FaceAttrs, Glyph, Key, KeyChord, KeyMod, KeyName,
    Position, Size, SurfaceMut, TerminalEvent, TerminalSurface, TerminalSurfaceExt, TerminalWaker,
    RGBA,
};
use tokio::{io::AsyncReadExt, process::Command, sync::mpsc};

#[derive(Debug, Clone)]
pub struct ThemeInner {
    pub fg: RGBA,
    pub bg: RGBA,
    pub accent: RGBA,
    pub cursor: Face,
    pub input: Face,
    pub list_default: Face,
    pub list_selected: Face,
    pub list_selected_indicator: Text,
    pub list_marked_indicator: Text,
    pub list_text: Face,
    pub list_highlight: Face,
    pub list_inactive: Face,
    pub scrollbar: Face,
    pub stats: Face,
    pub label: Face,
    pub separator: Face,
    pub separator_right: Text,
    pub separator_left: Text,
    pub show_preview: bool,
    pub named_colors: Arc<HashMap<String, RGBA>>,
}

#[derive(Clone)]
pub struct Theme {
    inner: Arc<ThemeInner>,
}

impl Deref for Theme {
    type Target = ThemeInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::fmt::Debug for Theme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

impl Theme {
    pub fn from_palette(fg: RGBA, bg: RGBA, accent: RGBA) -> Self {
        let theme_is_light = bg.luma() > fg.luma();

        let cursor = {
            let cursor_bg = bg.blend_over(accent.with_alpha(0.5));
            let cursor_fg = cursor_bg.best_contrast(bg, fg);
            Face::new(Some(cursor_fg), Some(cursor_bg), FaceAttrs::EMPTY)
        };
        let input = Face::new(Some(fg), Some(bg), FaceAttrs::EMPTY);
        let list_default = Face::new(Some(fg), Some(bg), FaceAttrs::EMPTY);
        let list_selected = Face::new(
            Some(fg),
            if theme_is_light {
                Some(bg.blend_over(fg.with_alpha(0.12)))
            } else {
                Some(bg.blend_over(fg.with_alpha(0.04)))
            },
            FaceAttrs::EMPTY,
        );
        let indicator_path = PathBuilder::new()
            .move_to((50.0, 50.0))
            .circle(20.0)
            .build();
        let list_marked_indicator = Text::new()
            .with_face(Face::default().with_fg(Some(accent)))
            .with_glyph(Glyph::new(
                indicator_path
                    .stroke(StrokeStyle {
                        width: 5.0,
                        line_join: Default::default(),
                        line_cap: Default::default(),
                    })
                    .clone(),
                Default::default(),
                Some(BBox::new((0.0, 0.0), (100.0, 100.0))),
                Size::new(1, 3),
                " \u{25CB} ".to_owned(), // white circle
                None,
            ));
        let list_selected_indicator = Text::new()
            .with_face(Face::default().with_fg(Some(accent)))
            .with_glyph(Glyph::new(
                indicator_path,
                Default::default(),
                Some(BBox::new((0.0, 0.0), (100.0, 100.0))),
                Size::new(1, 3),
                " \u{25CF} ".to_owned(), // black circle
                None,
            ));
        let list_text = Face::default().with_fg(list_selected.fg);
        let list_highlight = cursor;
        let list_inactive = Face::default().with_fg(Some(bg.blend_over(fg.with_alpha(0.6))));
        let scrollbar = list_default.with_fg(list_default.bg).overlay(&Face::new(
            Some(accent.with_alpha(0.8)),
            Some(accent.with_alpha(0.5)),
            FaceAttrs::EMPTY,
        ));
        let stats = Face::new(
            Some(accent.best_contrast(bg, fg)),
            Some(accent),
            FaceAttrs::EMPTY,
        );
        let label = stats.with_attrs(FaceAttrs::BOLD);
        let separator = Face::new(Some(accent), input.bg, FaceAttrs::EMPTY);
        let separator_right = Text::new().with_fmt(" ", Some(separator)).take();
        let separator_left = Text::new().with_fmt("", Some(separator)).take();
        let mut named_colors = SVG_COLORS.clone();
        named_colors.insert("fg".to_owned(), fg);
        named_colors.insert("bg".to_owned(), bg);
        named_colors.insert("accent".to_owned(), accent);
        named_colors.insert("base".to_owned(), accent);
        let inner = ThemeInner {
            fg,
            bg,
            accent,
            cursor,
            input,
            list_default,
            list_selected,
            list_selected_indicator,
            list_marked_indicator,
            list_text,
            list_highlight,
            list_inactive,
            scrollbar,
            stats,
            label,
            separator,
            separator_right,
            separator_left,
            show_preview: true,
            named_colors: Arc::new(named_colors),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Light gruvbox theme
    pub fn light() -> Self {
        Self::from_palette(
            "#3c3836".parse().unwrap(),
            "#fbf1c7".parse().unwrap(),
            "#8f3f71".parse().unwrap(),
        )
    }

    /// Dark gruvbox theme
    pub fn dark() -> Self {
        Self::from_palette(
            "#ebdbb2".parse().unwrap(),
            "#282828".parse().unwrap(),
            "#d3869b".parse().unwrap(),
        )
    }

    /// Theme used by dumb terminal with only four colors
    pub fn dumb() -> Self {
        let color0 = RGBA::new(0, 0, 0, 255);
        let color1 = RGBA::new(84, 84, 84, 255);
        let color2 = RGBA::new(168, 168, 168, 255);
        let color3 = RGBA::new(255, 255, 255, 255);

        let fg = color3;
        let bg = color0;
        let accent = color2;
        let default = Face::new(Some(fg), Some(bg), FaceAttrs::EMPTY);
        let input = default;
        let cursor = Face::new(Some(color0), Some(color2), FaceAttrs::EMPTY);
        let stats = Face::new(Some(bg), Some(accent), FaceAttrs::EMPTY);
        let list_selected = Face::new(Some(fg), Some(color1), FaceAttrs::EMPTY);
        let list_selected_indicator =
            Text::new().with_fmt(" > ", Some(Face::default().with_fg(Some(accent))));
        let list_marked_indicator =
            Text::new().with_fmt(" * ", Some(Face::default().with_fg(Some(accent))));
        let list_text = Face::default().with_fg(list_selected.fg);
        let mut named_colors = SVG_COLORS.clone();
        named_colors.insert("fg".to_owned(), fg);
        named_colors.insert("bg".to_owned(), bg);
        named_colors.insert("accent".to_owned(), accent);
        named_colors.insert("base".to_owned(), accent);
        let inner = ThemeInner {
            fg,
            bg,
            accent,
            cursor,
            input,
            list_default: default,
            list_selected,
            list_selected_indicator,
            list_marked_indicator,
            list_text,
            list_highlight: cursor,
            list_inactive: Face::default().with_fg(Some(color2)),
            scrollbar: Face::new(Some(color2), Some(color1), FaceAttrs::EMPTY),
            stats,
            label: stats.with_attrs(FaceAttrs::BOLD),
            separator: Face::new(Some(accent), input.bg, FaceAttrs::EMPTY),
            separator_right: Text::new().with_fmt(" ", Some(default)),
            separator_left: Text::new(),
            show_preview: true,
            named_colors: Arc::new(named_colors),
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Parse theme from `SWEEP_THEME` environment variable
    pub fn from_env() -> Self {
        match std::env::var("SWEEP_THEME") {
            Ok(theme_var) if !theme_var.is_empty() => {
                Theme::from_str(&theme_var).unwrap_or(Theme::light())
            }
            _ => Theme::light(),
        }
    }

    pub fn modify(&self, modify: impl FnOnce(&mut ThemeInner)) -> Theme {
        let mut inner = self.inner.deref().clone();
        modify(&mut inner);
        Theme {
            inner: Arc::new(inner),
        }
    }
}

impl FromStr for Theme {
    type Err = Error;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        string.split(',').try_fold(Theme::light(), |theme, attrs| {
            let mut iter = attrs.splitn(2, '=');
            let key = iter.next().unwrap_or_default().trim().to_lowercase();
            let value = iter.next().unwrap_or_default().trim();
            let theme = match key.as_str() {
                "fg" => Theme::from_palette(value.parse()?, theme.bg, theme.accent),
                "bg" => Theme::from_palette(theme.fg, value.parse()?, theme.accent),
                "accent" | "base" => Theme::from_palette(theme.fg, theme.bg, value.parse()?),
                "light" => Theme::light(),
                "dark" => Theme::dark(),
                "dumb" => Theme::dumb(),
                _ => return Err(Error::ParseError("Theme", string.to_string())),
            };
            Ok(theme)
        })
    }
}

/// Action description with default binding
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionDesc {
    /// default binding
    pub chords: Vec<KeyChord>,
    /// action name
    pub name: String,
    /// action description
    pub description: String,
}

impl Haystack for ActionDesc {
    type Context = ();
    type View = Flex<'static>;
    type Preview = HaystackBasicPreview<Container<Text>>;
    type PreviewLarge = ();

    fn haystack_scope<S>(&self, _ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.name.chars().for_each(scope);
    }

    fn view(
        &self,
        ctx: &Self::Context,
        positions: PositionsRef<&[u8]>,
        theme: &Theme,
    ) -> Self::View {
        let mut chords_text = Text::new();
        for chord in self.chords.iter() {
            chords_text
                .by_ref()
                .with_face(Face::default().with_attrs(FaceAttrs::UNDERLINE | FaceAttrs::BOLD))
                .put_fmt(&chord, None)
                .with_face(Face::default())
                .put_fmt(" ", None);
        }
        Flex::row()
            .justify(Justify::SpaceBetween)
            .add_flex_child(1.0, HaystackDefaultView::new(ctx, self, positions, theme))
            .add_child(chords_text)
    }

    fn preview(
        &self,
        _ctx: &Self::Context,
        _positions: PositionsRef<&[u8]>,
        _theme: &Theme,
    ) -> Option<Self::Preview> {
        let desc = Text::new().put_fmt(&self.description, None).take();
        Some(HaystackBasicPreview::new(Container::new(desc), Some(0.6)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InputAction {
    Insert(char),
    CursorForward,
    CursorBackward,
    CursorEnd,
    CursorStart,
    CursorNextWord,
    CursorPrevWord,
    DeleteBackward,
    DeleteForward,
    DeleteEnd,
}

impl InputAction {
    pub fn description(&self) -> ActionDesc {
        use InputAction::*;
        match self {
            Insert(_) => ActionDesc {
                chords: Vec::new(),
                name: "input.insert.char".to_owned(),
                description: "Insert character to the input field".to_owned(),
            },
            CursorForward => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Right,
                    mode: KeyMod::EMPTY,
                }])],
                name: "input.move.forward".to_owned(),
                description: "Move cursor forward in the input field".to_owned(),
            },
            CursorBackward => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Left,
                    mode: KeyMod::EMPTY,
                }])],
                name: "input.move.backward".to_owned(),
                description: "Move cursor backward in the input field".to_owned(),
            },
            CursorEnd => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('e'),
                    mode: KeyMod::CTRL,
                }])],
                name: "input.move.end".to_owned(),
                description: "Move cursor to the end of the input".to_owned(),
            },
            CursorStart => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('a'),
                    mode: KeyMod::CTRL,
                }])],
                name: "input.move.start".to_owned(),
                description: "Move cursor to the start of the input".to_owned(),
            },
            CursorNextWord => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('f'),
                    mode: KeyMod::ALT,
                }])],
                name: "input.move.next_word".to_owned(),
                description: "Move cursor to the end of the current word".to_owned(),
            },
            CursorPrevWord => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('b'),
                    mode: KeyMod::ALT,
                }])],
                name: "input.move.prev_word".to_owned(),
                description: "Move cursor to the start of the word".to_owned(),
            },
            DeleteBackward => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Backspace,
                    mode: KeyMod::EMPTY,
                }])],
                name: "input.delete.backward".to_owned(),
                description: "Delete previous char".to_owned(),
            },
            DeleteForward => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Delete,
                    mode: KeyMod::EMPTY,
                }])],
                name: "input.delete.forward".to_owned(),
                description: "Delete next char".to_owned(),
            },
            DeleteEnd => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Char('k'),
                    mode: KeyMod::CTRL,
                }])],
                name: "input.delete.end".to_owned(),
                description: "Delete all input after cursor".to_owned(),
            },
        }
    }
    pub fn all() -> impl Iterator<Item = InputAction> {
        use InputAction::*;
        [
            CursorForward,
            CursorBackward,
            CursorEnd,
            CursorStart,
            CursorNextWord,
            CursorPrevWord,
            DeleteBackward,
            DeleteForward,
            DeleteEnd,
        ]
        .into_iter()
    }
}

#[derive(Debug, Default)]
struct InputState {
    offset: usize,
}

pub struct Input {
    /// string before cursor
    before: Vec<char>,
    /// reversed string after cursor
    after: Vec<char>,
    /// theme
    theme: Theme,
    /// Input view state
    view_state: Arc<Mutex<InputState>>,
}

impl Input {
    pub fn new(theme: Theme) -> Self {
        Self {
            before: Default::default(),
            after: Default::default(),
            theme,
            view_state: Default::default(),
        }
    }

    pub fn theme_set(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn apply(&mut self, action: &InputAction) {
        use InputAction::*;
        match action {
            Insert(c) => self.before.push(*c),
            CursorForward => self.before.extend(self.after.pop()),
            CursorBackward => self.after.extend(self.before.pop()),
            CursorEnd => self.before.extend(self.after.drain(..).rev()),
            CursorStart => self.after.extend(self.before.drain(..).rev()),
            CursorNextWord => {
                while let Some(c) = self.after.pop() {
                    if is_word_separator(c) {
                        self.before.push(c);
                    } else {
                        self.after.push(c);
                        break;
                    }
                }
                while let Some(c) = self.after.pop() {
                    if is_word_separator(c) {
                        self.after.push(c);
                        break;
                    } else {
                        self.before.push(c);
                    }
                }
            }
            CursorPrevWord => {
                while let Some(c) = self.before.pop() {
                    if is_word_separator(c) {
                        self.after.push(c);
                    } else {
                        self.before.push(c);
                        break;
                    }
                }
                while let Some(c) = self.before.pop() {
                    if is_word_separator(c) {
                        self.before.push(c);
                        break;
                    } else {
                        self.after.push(c);
                    }
                }
            }
            DeleteEnd => self.after.clear(),
            DeleteBackward => {
                self.before.pop();
            }
            DeleteForward => {
                self.after.pop();
            }
        }
    }

    pub fn handle(&mut self, event: &TerminalEvent) {
        use KeyName::*;
        match event {
            TerminalEvent::Key(Key { name, mode }) => match *mode {
                KeyMod::EMPTY => match name {
                    Char(c) => self.apply(&InputAction::Insert(*c)),
                    Backspace => self.apply(&InputAction::DeleteBackward),
                    Delete => self.apply(&InputAction::DeleteForward),
                    Left => self.apply(&InputAction::CursorBackward),
                    Right => self.apply(&InputAction::CursorForward),
                    _ => {}
                },
                KeyMod::CTRL => match name {
                    KeyName::Char('e') => self.apply(&InputAction::CursorEnd),
                    KeyName::Char('a') => self.apply(&InputAction::CursorStart),
                    KeyName::Char('k') => self.apply(&InputAction::DeleteEnd),
                    _ => {}
                },
                KeyMod::ALT => match name {
                    KeyName::Char('f') => self.apply(&InputAction::CursorNextWord),
                    KeyName::Char('b') => self.apply(&InputAction::CursorPrevWord),
                    _ => {}
                },
                _ => {}
            },
            TerminalEvent::Paste(text) => self.before.extend(text.chars()),
            _ => {}
        }
    }

    pub fn get(&self) -> impl Iterator<Item = char> + '_ {
        self.before.iter().chain(self.after.iter().rev()).copied()
    }

    pub fn set(&mut self, text: &str) {
        self.before.clear();
        self.after.clear();
        self.before.extend(text.chars());
        self.view_state.with_mut(|st| st.offset = 0);
    }

    fn offset(&self) -> usize {
        self.view_state.with(|st| st.offset)
    }
}

impl<'a> View for &'a Input {
    fn render(
        &self,
        ctx: &ViewContext,
        surf: TerminalSurface<'_>,
        layout: ViewLayout<'_>,
    ) -> Result<(), Error> {
        if layout.size().is_empty() {
            return Ok(());
        }
        let mut surf = layout.apply_to(surf);
        surf.erase(self.theme.input);

        let mut writer = surf.writer(ctx).with_face(self.theme.input);
        for c in self.before[self.offset()..].iter() {
            writer.put_cell(Cell::new_char(self.theme.input, *c));
        }
        let mut iter = self.after.iter().rev();
        writer.put_cell(Cell::new_char(
            self.theme.cursor,
            iter.next().copied().unwrap_or(' '),
        ));
        for c in iter {
            writer.put_cell(Cell::new_char(self.theme.input, *c));
        }

        Ok(())
    }

    fn layout(
        &self,
        _ctx: &ViewContext,
        ct: BoxConstraint,
        mut layout: ViewMutLayout<'_>,
    ) -> Result<(), Error> {
        let size = ct.max().width * ct.max().height;
        if size < 2 {
            return Ok(());
        }
        // fix render offset
        self.view_state.with_mut(|st| {
            if st.offset > self.before.len() {
                st.offset = self.before.len();
            } else if st.offset + size < self.before.len() + 1 {
                st.offset = self.before.len() - size + 1;
            }
        });
        *layout = Layout::new().with_size(ct.max());
        Ok(())
    }
}

fn is_word_separator(c: char) -> bool {
    c.is_ascii_punctuation() || c.is_ascii_whitespace()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ListAction {
    ItemNext,
    ItemPrev,
    PageNext,
    PagePrev,
    Home,
    End,
}

impl ListAction {
    pub fn description(&self) -> ActionDesc {
        use ListAction::*;
        match self {
            ItemNext => ActionDesc {
                chords: vec![
                    KeyChord::from_iter([Key {
                        name: KeyName::Down,
                        mode: KeyMod::EMPTY,
                    }]),
                    KeyChord::from_iter([Key {
                        name: KeyName::Char('n'),
                        mode: KeyMod::CTRL,
                    }]),
                ],
                name: "list.item.next".to_owned(),
                description: "Move to the next item in the list".to_owned(),
            },
            ItemPrev => ActionDesc {
                chords: vec![
                    KeyChord::from_iter([Key {
                        name: KeyName::Up,
                        mode: KeyMod::EMPTY,
                    }]),
                    KeyChord::from_iter([Key {
                        name: KeyName::Char('p'),
                        mode: KeyMod::CTRL,
                    }]),
                ],
                name: "list.item.prev".to_owned(),
                description: "Move to the previous item in the list".to_owned(),
            },
            PageNext => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::PageDown,
                    mode: KeyMod::EMPTY,
                }])],
                name: "list.page.next".to_owned(),
                description: "Move one page down in the list".to_owned(),
            },
            PagePrev => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::PageUp,
                    mode: KeyMod::EMPTY,
                }])],
                name: "list.page.prev".to_owned(),
                description: "Move one page up in the list".to_owned(),
            },
            Home => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::Home,
                    mode: KeyMod::EMPTY,
                }])],
                name: "list.home".to_owned(),
                description: "Move to the beginning of the list".to_owned(),
            },
            End => ActionDesc {
                chords: vec![KeyChord::from_iter([Key {
                    name: KeyName::End,
                    mode: KeyMod::EMPTY,
                }])],
                name: "list.end".to_owned(),
                description: "Move to the end of the list".to_owned(),
            },
        }
    }

    pub fn all() -> impl Iterator<Item = ListAction> {
        use ListAction::*;
        [ItemNext, ItemPrev, PageNext, PagePrev, Home, End].into_iter()
    }
}

pub trait ListItems {
    type Item;
    type ItemView: IntoView;
    type Context: Send + Sync + ?Sized;

    /// Number of items in the list
    fn len(&self) -> usize;

    /// Get entry in the list by it's index
    fn get(&self, index: usize) -> Option<Self::Item>;

    /// Get view for specified item
    fn get_view(
        &self,
        item: Self::Item,
        theme: Theme,
        ctx: &Self::Context,
    ) -> Option<Self::ItemView>;

    /// Whether item is marked (multi-select)
    fn is_marked(&self, item: &Self::Item) -> bool;
}

pub struct List<T> {
    items: T,
    cursor: usize,
    theme: Theme,
    view_state: Arc<Mutex<ListState>>,
}

/// Current state of the list view (it is only updated on layout calculation)
#[derive(Debug, Clone, Copy, Default)]
struct ListState {
    cursor: usize,        // currently pointed item
    offset: usize,        // visible offset (first rendered element offset)
    visible_count: usize, // number of visible elements
}

impl<T: ListItems> List<T> {
    /// Create new List widget
    pub fn new(items: T, theme: Theme) -> Self {
        Self {
            items,
            cursor: 0,
            theme,
            view_state: Default::default(),
        }
    }

    /// Reference to list items
    pub fn items(&self) -> &T {
        &self.items
    }

    /// Set list items
    pub fn items_set(&mut self, items: T) -> T {
        self.cursor = 0;
        self.view_state = Default::default();
        std::mem::replace(&mut self.items, items)
    }

    /// Currently pointed item
    pub fn current(&self) -> Option<T::Item> {
        self.items.get(self.cursor)
    }

    pub fn view<'a, 'b: 'a>(&'a self, ctx: &'b T::Context) -> ListView<'a, T> {
        ListView {
            list_ctx: ctx,
            list: self,
        }
    }

    /// Current cursor position
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Set cursor position
    pub fn cursor_set(&mut self, cursor: usize) {
        self.cursor = cursor;
        self.view_state.with_mut(|st| st.offset = cursor);
    }

    /// Apply action
    pub fn apply(&mut self, action: &ListAction) {
        use ListAction::*;
        match action {
            ItemNext => self.cursor += 1,
            ItemPrev => {
                if self.cursor > 0 {
                    self.cursor -= 1
                }
            }
            PageNext => {
                let page_size = max(self.visible_count(), 1);
                self.cursor += page_size;
            }
            PagePrev => {
                let page_size = max(self.visible_count(), 1);
                if self.cursor >= page_size {
                    self.cursor -= page_size;
                }
            }
            Home => {
                self.cursor = 0;
                self.view_state.with_mut(|st| st.offset = 0);
            }
            End => {
                self.cursor = self.items.len() - 1;
            }
        }
        if self.items.len() > 0 {
            self.cursor = self.cursor.clamp(0, self.items.len() - 1);
        } else {
            self.cursor = 0;
        }
    }

    /// Get scroll bar widget
    pub fn scroll_bar(&self) -> impl View {
        let state = self.view_state.clone();
        let total = self.items.len();
        ScrollBarFn::new(Axis::Vertical, self.theme.scrollbar, move || {
            let state = state.with(|state| *state);
            ScrollBarPosition::from_counts(total, state.cursor, state.visible_count)
        })
    }

    /// Set theme
    pub fn theme_set(&mut self, theme: Theme) {
        self.theme = theme;
    }

    /// First visible element position
    #[cfg(test)]
    fn offset(&self) -> usize {
        self.view_state.with(|st| st.offset)
    }

    /// Number of visible items
    fn visible_count(&self) -> usize {
        self.view_state.with(|st| st.visible_count)
    }
}

pub struct ListView<'a, T: ListItems> {
    list_ctx: &'a T::Context,
    list: &'a List<T>,
}

struct ListItemView {
    view: BoxView<'static>,
    marked: bool,
    pointed: bool,
}

impl<'a, T> View for ListView<'a, T>
where
    T: ListItems + Send + Sync,
    T::ItemView: 'static,
{
    fn render(
        &self,
        ctx: &ViewContext,
        surf: TerminalSurface<'_>,
        layout: ViewLayout<'_>,
    ) -> Result<(), Error> {
        if layout.size().is_empty() {
            return Ok(());
        }
        let mut surf = layout.apply_to(surf);

        // render items and scroll-bar (last layout in the list)
        surf.erase(self.list.theme.list_default);
        for item_layout in layout.children() {
            let row = item_layout.position().row;
            let height = item_layout.size().height;
            let item_data = item_layout
                .data::<ListItemView>()
                .ok_or(Error::InvalidLayout)?;

            // render cursor
            if item_data.pointed {
                let mut surf = surf.view_mut(row..row + height, ..);
                surf.erase(self.list.theme.list_selected);
                surf.draw_view(ctx, None, &self.list.theme.list_selected_indicator)?;
            } else if item_data.marked {
                let mut surf = surf.view_mut(row..row + height, ..);
                surf.draw_view(ctx, None, &self.list.theme.list_marked_indicator)?;
            }

            item_data
                .view
                .render(ctx, surf.as_mut(), item_layout.view())?;
        }

        Ok(())
    }

    fn layout(
        &self,
        ctx: &ViewContext,
        ct: BoxConstraint,
        mut layout: ViewMutLayout<'_>,
    ) -> Result<(), Error> {
        // indicator (tag on the left side of the highlighted item)
        let indicator_layout =
            self.list
                .theme
                .list_selected_indicator
                .layout_new(ctx, ct, layout.store_mut())?;
        let indicator_width = indicator_layout.size().width;

        let height = ct.max().height;
        let width = ct.max().width;
        if height < 1 || width < indicator_width {
            return Ok(());
        }

        // adjust offset so item pointed by cursor will be visible
        let offset = self
            .list
            .view_state
            .with(|st| st.offset)
            .min(self.list.items.len().saturating_sub(height)); // offset is at least hight from the bottom
        let offset = if offset > self.list.cursor {
            self.list.cursor
        } else if height > 0 && offset + height - 1 < self.list.cursor {
            self.list.cursor - height + 1
        } else {
            offset
        };

        // create view and calculate layout for all visible items
        let child_ct = BoxConstraint::new(
            Size::new(0, width - indicator_width),
            Size::new(height, width - indicator_width),
        );
        let mut children_height = 0;
        let mut children_removed = 0;
        let mut children_count = 0;
        // looping over items starting from offset
        for index in offset..offset + 2 * height {
            let Some(item) = self.list.items.get(index) else {
                break;
            };
            let marked = self.list.items.is_marked(&item);
            let Some(item_view) =
                self.list
                    .items
                    .get_view(item, self.list.theme.clone(), self.list_ctx)
            else {
                break;
            };

            // create view and calculate layout
            let pointed = index == self.list.cursor;
            let view = item_view.into_view().boxed();
            let mut child_layout = layout.push_default();
            children_count += 1;
            view.layout(ctx, child_ct, child_layout.view_mut())?;

            // make sure item height is at least one, otherwise it will result
            // in missing cursor
            let size = child_layout.size();
            child_layout.set_size(Size {
                height: max(size.height, 1),
                ..size
            });

            // insert layout
            children_height += child_layout.size().height;
            child_layout.set_data(ListItemView {
                view,
                pointed,
                marked,
            });

            if children_height > height {
                // cursor is rendered, all height is taken
                if index > self.list.cursor {
                    break;
                }
                // cursor is not rendered, remove children from the top until
                // we have some space available
                while children_height > height {
                    if index == self.list.cursor && children_count == 1 {
                        // do not remove the item if it is pointed by cursor
                        break;
                    }
                    match layout.pop() {
                        Some(layout) => {
                            children_height -= layout.size().height;
                            children_removed += 1;
                            children_count -= 1;
                        }
                        None => break,
                    }
                }
            }
        }

        // update view state
        self.list.view_state.with_mut(|st| {
            *st = ListState {
                cursor: self.list.cursor,
                offset: offset + children_removed,
                visible_count: children_count,
            }
        });

        // compute view offsets
        let mut view_offset = 0;
        let mut child_layout_opt = layout.child_mut();
        while let Some(mut child_layout) = child_layout_opt.take() {
            child_layout.set_position(Position::new(view_offset, indicator_width));
            view_offset += child_layout.size().height;
            child_layout_opt = child_layout.sibling();
        }

        *layout = Layout::new().with_size(Size::new(height, width));
        Ok(())
    }
}

/// Widget that can run system command and show its output
pub struct Process {
    spawn_channel: mpsc::Sender<Option<Command>>,
    output: ProcessOutput,
    command_builder: Option<ProcessCommandBuilder>,
    _spawner_handle: AbortJoinHandle<()>,
}

impl Process {
    pub fn new(command_builder: Option<ProcessCommandBuilder>, waker: TerminalWaker) -> Self {
        let (spawn_channel, recv) = mpsc::channel(3);
        let output = ProcessOutput::new();
        let _spawner_handle = tokio::spawn(
            Self::spawner(recv, output.clone(), waker)
                .unwrap_or_else(|error| tracing::error!(?error, "[Process] spawner failed")),
        )
        .into();
        Self {
            spawn_channel,
            output,
            command_builder,
            _spawner_handle,
        }
    }

    /// Spawn new process killing existing process and replacing its output
    pub fn spawn(&self, args: &[impl ProcessCommandArg]) {
        let args = match &self.command_builder {
            None => either::Left(args.iter().map(|arg| Cow::Borrowed(arg.as_command_arg()))),
            Some(command_builder) => either::Right(command_builder.build(args)),
        };
        let mut args = args.into_iter();
        let command = args.next().map(|prog| {
            let mut command = Command::new(prog.as_command_arg());
            for arg in args {
                command.arg(arg.as_command_arg());
            }
            command
        });

        self.spawn_channel
            .try_send(command)
            .unwrap_or_else(|error| {
                tracing::error!(?error, "[Process] faield to spawn");
            });
    }

    async fn spawner(
        mut spawn_channel: mpsc::Receiver<Option<Command>>,
        output: ProcessOutput,
        waker: TerminalWaker,
    ) -> Result<(), Error> {
        let mut command: Option<Command> = None;
        let mut command_running: bool;
        loop {
            let command_fut = match command.take() {
                Some(command) => {
                    command_running = true;
                    output.clear();
                    Self::command_run(command, output.clone(), waker.clone()).right_future()
                }
                None => {
                    command_running = false;
                    future::pending().left_future()
                }
            };
            tokio::select! {
                Some(command_next) = spawn_channel.recv() => {
                    command = command_next;
                    while let Ok(command_next) = spawn_channel.try_recv() {
                        command = command_next;
                    }
                }
                command_res = command_fut, if command_running => {
                    let _ = waker.wake();
                    if let Err(error) = command_res {
                        tracing::error!(?error, "[Process] command faield with error");
                    }
                }
                else => { break; }
            }
        }
        Ok(())
    }

    async fn command_run(
        mut command: Command,
        output: ProcessOutput,
        waker: TerminalWaker,
    ) -> Result<(), Error> {
        let mut child = command
            // TODO: pass `FZF_PREVIEW_(TOP|LEFT|LINES|COLUMNS)` and `LINES|COLUMNS`
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().expect("pipe expected, logic error");
        let mut stdout_buf = [0u8; 4096];
        let mut stdout_done = false;
        let mut stderr = child.stderr.take().expect("pipe expected, logic error");
        let mut stderr_buf = [0u8; 4096];
        let mut stderr_done = false;
        let mut writer = output.tty_writer();

        loop {
            tokio::select! {
                biased;
                stderr_res = stderr.read(&mut stderr_buf), if !stderr_done => {
                    let data = &stderr_buf[..stderr_res?];
                    if data.is_empty() {
                        stderr_done = true;
                        continue;
                    }
                    writer.write_all(data)?;
                }
                stdout_res = stdout.read(&mut stdout_buf), if !stdout_done => {
                    let data = &stdout_buf[..stdout_res?];
                    if data.is_empty() {
                        stdout_done = true;
                        continue;
                    }
                    writer.write_all(data)?;
                }
                else => { break; }
            }
            waker.wake()?;
        }
        child.wait().await?;

        Ok(())
    }
}

impl<'a> IntoView for &'a Process {
    type View = ProcessOutput;

    fn into_view(self) -> Self::View {
        self.output.clone()
    }
}

#[derive(Default)]
struct ProcessOutputInner {
    size: Size,
    cursor: Position,
    cells: Vec<Cell>,
    lines: Vec<usize>,
    offset: Position,
}

impl ProcessOutputInner {
    fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        let mut row = self.offset.row;
        let col = self.offset.col;
        std::iter::from_fn(move || {
            let start = self.cells_offset(row)?;
            let end = self.cells_offset(row + 1).unwrap_or(self.cells.len());
            row += 1;
            let row = self.cells.get(start..end)?;
            Some(row.get(col..).unwrap_or(&[]))
        })
    }

    fn cells_offset(&self, line: usize) -> Option<usize> {
        if line == 0 {
            Some(0)
        } else {
            self.lines.get(line - 1).copied()
        }
    }
}

#[derive(Clone, Default)]
pub struct ProcessOutput {
    face: Face,
    wraps: bool,
    inner: Arc<RwLock<ProcessOutputInner>>,
}

impl ProcessOutput {
    pub fn new() -> Self {
        Self {
            face: Face::default(),
            wraps: false,
            inner: Arc::new(RwLock::new(ProcessOutputInner::default())),
        }
    }

    pub fn offset(&self) -> Position {
        self.inner.with(|inner| inner.offset)
    }

    pub fn set_offset(&self, offset: Position) -> Position {
        self.inner.with_mut(|inner| {
            inner.offset = offset;
            offset
        })
    }

    pub fn clear(&self) {
        self.inner.with_mut(|inner| {
            inner.size = Size::empty();
            inner.cursor = Position::origin();
            inner.cells.clear();
            inner.lines.clear();
            inner.offset = Position::origin();
        })
    }
}

impl CellWrite for ProcessOutput {
    fn face(&self) -> Face {
        self.face
    }

    fn set_face(&mut self, face: Face) -> Face {
        std::mem::replace(&mut self.face, face)
    }

    fn wraps(&self) -> bool {
        self.wraps
    }

    fn set_wraps(&mut self, wraps: bool) -> bool {
        std::mem::replace(&mut self.wraps, wraps)
    }

    fn put_cell(&mut self, cell: Cell) -> bool {
        self.inner.with_mut(|inner| {
            let prev_row = inner.cursor.row;
            cell.layout(
                &ViewContext::dummy(), // we do not care about actual size here, only about new lines
                usize::MAX,
                self.wraps,
                &mut inner.size,
                &mut inner.cursor,
            );
            inner.cells.push(cell);
            if prev_row < inner.cursor.row {
                inner.lines.push(inner.cells.len())
            }
        });
        true
    }
}

impl View for ProcessOutput {
    fn render(
        &self,
        ctx: &ViewContext,
        surf: TerminalSurface<'_>,
        layout: ViewLayout<'_>,
    ) -> Result<(), Error> {
        let mut surf = layout.apply_to(surf);
        let mut writer = surf.writer(ctx).with_wraps(false);
        self.inner.with(|inner| {
            let mut cells = inner.rows().flat_map(|row| row.iter()).cloned();
            loop {
                let Some(cell) = cells.next() else {
                    // no more cells
                    break;
                };
                if !writer.put_cell(cell) {
                    // failed to put cell
                    break;
                }
            }
        });
        Ok(())
    }

    fn layout(
        &self,
        _ctx: &ViewContext,
        ct: BoxConstraint,
        mut layout: ViewMutLayout<'_>,
    ) -> Result<(), Error> {
        *layout = Layout::new().with_size(ct.max());
        Ok(())
    }
}

impl HaystackPreview for ProcessOutput {
    fn flex(&self) -> Option<f64> {
        None
    }

    fn preview_layout(&self) -> Layout {
        self.inner.with(|inner| {
            Layout::new()
                .with_size(inner.size)
                .with_position(inner.offset)
        })
    }

    fn set_offset(&self, offset: Position) -> Position {
        ProcessOutput::set_offset(self, offset)
    }
}

/// Builder that used to create argument list from a pattern and a list of fields
pub struct ProcessCommandBuilder {
    args: Vec<Result<String, Vec<Result<String, FieldSelector>>>>,
}

impl ProcessCommandBuilder {
    pub fn new(pattern: &str) -> Result<Self, anyhow::Error> {
        let mut args = Vec::new();
        for arg in shlex::Shlex::new(pattern) {
            if !arg.contains('{') {
                args.push(Ok(arg));
                continue;
            }
            let mut chunks = Vec::new();
            for chunk in parse_command_pattern(&arg) {
                match chunk {
                    Ok(chunk) => chunks.push(Ok(chunk)),
                    Err(pattern) => chunks.push(Err(FieldSelector::from_str(&pattern)
                        .with_context(|| format!("failed to parse selector: \"{pattern}\""))?)),
                }
            }
            args.push(Err(chunks));
        }
        Ok(Self { args })
    }

    pub fn build<'builder: 'args, 'args, A: ProcessCommandArg + 'args>(
        &'builder self,
        args: &'args [A],
    ) -> impl Iterator<Item = Cow<'builder, str>> + 'args {
        let mut index = 0;
        std::iter::from_fn(move || {
            let arg = self.args.get(index)?;
            index += 1;

            match arg {
                Ok(arg) => Some(Cow::Borrowed(arg.as_ref())),
                Err(chunks) => {
                    let mut arg = String::new();
                    for chunk in chunks {
                        match chunk {
                            Ok(chunk) => arg.push_str(chunk),
                            Err(selector) => {
                                selector
                                    .matches_iter(args.len())
                                    .map(|index| &args[index])
                                    .enumerate()
                                    .for_each(|(index, field)| {
                                        if index != 0 {
                                            arg.push(' ');
                                        }
                                        arg.push_str(field.as_command_arg().trim());
                                    });
                            }
                        }
                    }
                    Some(Cow::Owned(arg))
                }
            }
        })
    }
}

impl std::str::FromStr for ProcessCommandBuilder {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ProcessCommandBuilder::new(s)
    }
}

pub trait ProcessCommandArg {
    fn as_command_arg(&self) -> &str;
}

impl<T: AsRef<str>> ProcessCommandArg for T {
    fn as_command_arg(&self) -> &str {
        self.as_ref()
    }
}

/// Parse process command pattern
fn parse_command_pattern(string: &str) -> impl Iterator<Item = Result<String, String>> + '_ {
    let mut chars = string.chars().peekable();
    let mut chunk = String::new();
    let mut is_opened = false;
    let mut is_done = false;
    std::iter::from_fn(move || loop {
        if is_done {
            return None;
        }
        if let Some(char) = chars.next() {
            if !matches!(char, '{' | '}') {
                chunk.push(char);
                continue;
            }
            if matches!(chars.peek(), Some(c) if *c == char) {
                chars.next();
                chunk.push(char);
                continue;
            }
            if (char == '{' && is_opened) || (char == '}' && !is_opened) {
                chunk.push(char);
                continue;
            }
        } else {
            is_done = true;
        }
        let item = std::mem::take(&mut chunk);
        if is_opened {
            is_opened = false;
            return Some(Err(item));
        } else {
            is_opened = true;
            if !item.is_empty() {
                return Some(Ok(item));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Display;

    struct VecItems<T>(Vec<T>);

    impl<T> VecItems<T> {
        fn new(items: impl Into<Vec<T>>) -> Self {
            Self(items.into())
        }
    }

    impl<T> ListItems for VecItems<T>
    where
        T: Display + Clone,
    {
        type Item = String;
        type ItemView = String;
        type Context = ();

        fn len(&self) -> usize {
            self.0.len()
        }

        fn get(&self, index: usize) -> Option<Self::Item> {
            let value = self.0.get(index)?;
            Some(value.to_string())
        }

        fn get_view(&self, item: Self::Item, _theme: Theme, _ctx: &()) -> Option<Self::ItemView> {
            Some(item)
        }

        fn is_marked(&self, _item: &Self::Item) -> bool {
            false
        }
    }

    #[test]
    fn test_list_basic() -> Result<(), Error> {
        let list_selected_bg = Some("#8ec07c".parse()?);
        let theme = Theme::light().modify(|inner| inner.list_selected.bg = list_selected_bg);

        let items = VecItems((0..60).collect());
        let mut list = List::new(items, theme.clone());

        print!("{:?}", list.view(&()).debug(Size::new(8, 50)));
        assert_eq!(list.offset(), 0);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(8, 50)));
        assert_eq!(list.offset(), 0);

        (0..20).for_each(|_| list.apply(&ListAction::ItemNext));
        print!("{:?}", list.view(&()).debug(Size::new(8, 50)));
        assert_eq!(list.offset(), 14);

        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 17);

        Ok(())
    }

    #[test]
    fn test_list_multiline() -> Result<(), Error> {
        let list_selected_bg = Some("#8ec07c".parse()?);
        let theme = Theme::light().modify(|inner| inner.list_selected.bg = list_selected_bg);

        println!("multi-line entry");
        let items = VecItems::new([
            "1. other entry",
            "2. this is the third entry",
            "3. first multi line\n - first\n - second\n - thrid",
            "4. fourth entry",
        ]);
        let mut list = List::new(items, theme);

        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 0);

        (0..2).for_each(|_| list.apply(&ListAction::ItemNext));
        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 1);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 2);

        println!("tall multi-line entry");
        let items = VecItems::new([
            "first",
            "too many lines to be shown\n - 1\n - 2\n - 3\n - 4\n - 5\n - 6",
            "last",
        ]);
        list.items_set(items);
        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 0);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 1);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 2);

        println!("very long line");
        let items = VecItems::new(
            [
                "first short",
                "second",
                "fist very very long line\nwhich is also multi line that should split",
                "second very very long line that should be split into multiple lines and rendered correctly",
                "last",
            ]
        );
        list.items_set(items);
        print!("{:?}", list.view(&()).debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 0);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 0);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 2);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 3);

        list.apply(&ListAction::ItemNext);
        print!("{:?}", list.view(&()).debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 4);

        Ok(())
    }

    #[test]
    fn test_parse_command_pattern() -> Result<(), Error> {
        assert_eq!(
            parse_command_pattern("one {two} {{three}}").collect::<Vec<_>>(),
            vec![
                Ok("one ".to_string()),
                Err("two".to_string()),
                Ok(" {three}".to_string())
            ]
        );

        assert_eq!(
            parse_command_pattern("cat {}").collect::<Vec<_>>(),
            vec![Ok("cat ".to_string()), Err("".to_string())]
        );

        assert_eq!(
            parse_command_pattern("{}").collect::<Vec<_>>(),
            vec![Err("".to_string())]
        );

        Ok(())
    }

    #[test]
    fn test_process_command_builder() -> Result<(), anyhow::Error> {
        let builder = ProcessCommandBuilder::from_str("one '{} {0}' two")?;
        assert_eq!(
            builder.build(&["+", "-"]).collect::<String>(),
            "one+ - +two".to_string()
        );
        Ok(())
    }
}
