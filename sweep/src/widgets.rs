use crate::{haystack_default_view, FieldRefs, Haystack, HaystackPreview, Positions};
use std::{cell::Cell as StdCell, cmp::max, collections::VecDeque, fmt::Write as _, str::FromStr};
use surf_n_term::{
    common::clamp,
    rasterize::PathBuilder,
    view::{
        Axis, BoxConstraint, Container, Flex, IntoView, Justify, Layout, ScrollBar, Text, Tree,
        View, ViewContext,
    },
    BBox, Cell, Color, Error, Face, FaceAttrs, Glyph, Key, KeyMod, KeyName, Position, Size,
    SurfaceMut, TerminalEvent, TerminalSurface, TerminalSurfaceExt, RGBA,
};

#[derive(Clone, Debug)]
pub struct Theme {
    pub fg: RGBA,
    pub bg: RGBA,
    pub accent: RGBA,
    pub cursor: Face,
    pub input: Face,
    pub list_default: Face,
    pub list_selected: Face,
    pub list_selected_indicator: Text,
    pub list_text: Face,
    pub list_highlight: Face,
    pub list_inactive: Face,
    pub scrollbar_on: Face,
    pub scrollbar_off: Face,
    pub stats: Face,
    pub label: Face,
    pub separator: Face,
    pub separator_right: Text,
    pub separator_left: Text,
    pub show_preview: bool,
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
        let list_default = Face::new(
            Some(bg.blend_over(fg.with_alpha(0.9))),
            Some(bg),
            FaceAttrs::EMPTY,
        );
        let list_selected = Face::new(
            Some(fg),
            if theme_is_light {
                Some(bg.blend_over(fg.with_alpha(0.12)))
            } else {
                Some(bg.blend_over(fg.with_alpha(0.04)))
            },
            FaceAttrs::EMPTY,
        );
        let list_selected_indicator = Text::new()
            .set_face(Face::default().with_fg(Some(accent)))
            .put_glyph(Glyph::new(
                PathBuilder::new()
                    .move_to((50.0, 50.0))
                    .circle(20.0)
                    .build(),
                Default::default(),
                Some(BBox::new((0.0, 0.0), (100.0, 100.0))),
                Size::new(1, 3),
                " ● ".to_owned(),
            ))
            .take();
        let list_text = Face::default().with_fg(list_selected.fg);
        let list_highlight = cursor;
        let list_inactive = Face::default().with_fg(Some(bg.blend_over(fg.with_alpha(0.6))));
        let scrollbar_on = Face::new(None, Some(accent.with_alpha(0.8)), FaceAttrs::EMPTY);
        let scrollbar_off = Face::new(None, Some(accent.with_alpha(0.5)), FaceAttrs::EMPTY);
        let stats = Face::new(
            Some(accent.best_contrast(bg, fg)),
            Some(accent),
            FaceAttrs::EMPTY,
        );
        let label = stats.with_attrs(FaceAttrs::BOLD);
        let separator = Face::new(Some(accent), input.bg, FaceAttrs::EMPTY);
        let separator_right = Text::new().push_str(" ", Some(separator)).take();
        let separator_left = Text::new().push_str("", Some(separator)).take();
        Self {
            fg,
            bg,
            accent,
            cursor,
            input,
            list_default,
            list_selected,
            list_selected_indicator,
            list_text,
            list_highlight,
            list_inactive,
            scrollbar_on,
            scrollbar_off,
            stats,
            label,
            separator,
            separator_right,
            separator_left,
            show_preview: true,
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
        let list_selected_indicator = Text::new()
            .push_str("> ", Some(Face::default().with_fg(Some(accent))))
            .take();
        let list_text = Face::default().with_fg(list_selected.fg);
        Self {
            fg,
            bg,
            accent,
            cursor,
            input,
            list_default: default,
            list_selected,
            list_selected_indicator,
            list_text,
            list_highlight: cursor,
            list_inactive: Face::default().with_fg(Some(color2)),
            scrollbar_on: Face::new(None, Some(color2), FaceAttrs::EMPTY),
            scrollbar_off: Face::new(None, Some(color1), FaceAttrs::EMPTY),
            stats,
            label: stats.with_attrs(FaceAttrs::BOLD),
            separator: Face::new(Some(accent), input.bg, FaceAttrs::EMPTY),
            separator_right: Text::new().push_str(" ", Some(default)).take(),
            separator_left: Text::new(),
            show_preview: true,
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
    pub chords: Vec<Vec<Key>>,
    /// action name
    pub name: String,
    /// action description
    pub description: String,
}

impl Haystack for ActionDesc {
    fn haystack_scope<S>(&self, scope: S)
    where
        S: FnMut(char),
    {
        self.name.chars().for_each(scope);
    }

    fn view(&self, positions: &Positions, theme: &Theme, _refs: FieldRefs) -> Box<dyn View> {
        let mut chords_text = Text::new();
        for chord in self.chords.iter() {
            (|| {
                chords_text
                    .set_face(Face::default().with_attrs(FaceAttrs::UNDERLINE | FaceAttrs::BOLD));
                for (index, key) in chord.iter().enumerate() {
                    if index != 0 {
                        write!(chords_text, " ")?;
                    }
                    write!(chords_text, "{}", key)?;
                }
                chords_text.set_face(Face::default());
                write!(chords_text, " ")?;
                Ok::<_, Error>(())
            })()
            .expect("In memory write failed");
        }
        Flex::row()
            .justify(Justify::SpaceBetween)
            .add_flex_child(1.0, haystack_default_view(self, positions, theme))
            .add_child(chords_text)
            .boxed()
    }

    fn preview(&self, _theme: &Theme, _refs: FieldRefs) -> Option<HaystackPreview> {
        let desc = Text::new().push_str(&self.description, None).take();
        Some(HaystackPreview::new(
            Container::new(desc).boxed(),
            Some(0.6),
        ))
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
                chords: vec![vec![Key {
                    name: KeyName::Right,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.move.forward".to_owned(),
                description: "Move cursor forward in the input field".to_owned(),
            },
            CursorBackward => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Left,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.move.backward".to_owned(),
                description: "Move cursor backward in the input field".to_owned(),
            },
            CursorEnd => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('e'),
                    mode: KeyMod::CTRL,
                }]],
                name: "input.move.end".to_owned(),
                description: "Move cursor to the end of the input".to_owned(),
            },
            CursorStart => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('a'),
                    mode: KeyMod::CTRL,
                }]],
                name: "input.move.start".to_owned(),
                description: "Move cursor to the start of the input".to_owned(),
            },
            CursorNextWord => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('f'),
                    mode: KeyMod::ALT,
                }]],
                name: "input.move.next_word".to_owned(),
                description: "Move cursor to the end of the current word".to_owned(),
            },
            CursorPrevWord => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('b'),
                    mode: KeyMod::ALT,
                }]],
                name: "input.move.prev_word".to_owned(),
                description: "Move cursor to the start of the word".to_owned(),
            },
            DeleteBackward => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Backspace,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.delete.backward".to_owned(),
                description: "Delete previous char".to_owned(),
            },
            DeleteForward => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Delete,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.delete.forward".to_owned(),
                description: "Delete next char".to_owned(),
            },
            DeleteEnd => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Char('k'),
                    mode: KeyMod::CTRL,
                }]],
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

pub struct Input {
    /// string before cursor
    before: Vec<char>,
    /// reversed string after cursor
    after: Vec<char>,
    /// visible offset
    offset: StdCell<usize>,
    /// theme
    theme: Theme,
}

impl Input {
    pub fn new(theme: Theme) -> Self {
        Self {
            before: Default::default(),
            after: Default::default(),
            offset: StdCell::new(0),
            theme,
        }
    }

    pub fn theme_set(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn apply(&mut self, action: InputAction) {
        use InputAction::*;
        match action {
            Insert(c) => self.before.push(c),
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
                    Char(c) => self.apply(InputAction::Insert(*c)),
                    Backspace => self.apply(InputAction::DeleteBackward),
                    Delete => self.apply(InputAction::DeleteForward),
                    Left => self.apply(InputAction::CursorBackward),
                    Right => self.apply(InputAction::CursorForward),
                    _ => {}
                },
                KeyMod::CTRL => match name {
                    KeyName::Char('e') => self.apply(InputAction::CursorEnd),
                    KeyName::Char('a') => self.apply(InputAction::CursorStart),
                    KeyName::Char('k') => self.apply(InputAction::DeleteEnd),
                    _ => {}
                },
                KeyMod::ALT => match name {
                    KeyName::Char('f') => self.apply(InputAction::CursorNextWord),
                    KeyName::Char('b') => self.apply(InputAction::CursorPrevWord),
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
        self.offset.set(0);
    }

    fn offset(&self) -> usize {
        self.offset.get()
    }

    fn fix_offset(&self, size: usize) -> usize {
        if self.offset() > self.before.len() {
            self.offset.set(self.before.len());
        } else if self.offset() + size < self.before.len() + 1 {
            self.offset.set(self.before.len() - size + 1);
        }
        self.offset()
    }
}

impl<'a> View for &'a Input {
    fn render<'b>(
        &self,
        _ctx: &ViewContext,
        surf: &'b mut TerminalSurface<'b>,
        layout: &Tree<Layout>,
    ) -> Result<(), Error> {
        if layout.size().is_empty() {
            return Ok(());
        }
        let mut surf = layout.apply_to(surf);
        surf.erase(self.theme.input);

        let mut writer = surf.writer().face(self.theme.input);
        for c in self.before[self.offset()..].iter() {
            writer.put(Cell::new_char(self.theme.input, Some(*c)));
        }
        let mut iter = self.after.iter().rev();
        writer.put(Cell::new_char(self.theme.cursor, iter.next().copied()));
        for c in iter {
            writer.put(Cell::new_char(self.theme.input, Some(*c)));
        }

        Ok(())
    }

    fn layout(&self, _ctx: &ViewContext, ct: BoxConstraint) -> Tree<Layout> {
        let size = ct.max().width * ct.max().height;
        if size < 2 {
            return Tree::leaf(Layout::new());
        }
        self.fix_offset(size);
        Tree::leaf(Layout::new().with_size(ct.max()))
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
                    vec![Key {
                        name: KeyName::Down,
                        mode: KeyMod::EMPTY,
                    }],
                    vec![Key {
                        name: KeyName::Char('n'),
                        mode: KeyMod::CTRL,
                    }],
                ],
                name: "list.item.next".to_owned(),
                description: "Move to the next item in the list".to_owned(),
            },
            ItemPrev => ActionDesc {
                chords: vec![
                    vec![Key {
                        name: KeyName::Up,
                        mode: KeyMod::EMPTY,
                    }],
                    vec![Key {
                        name: KeyName::Char('p'),
                        mode: KeyMod::CTRL,
                    }],
                ],
                name: "list.item.prev".to_owned(),
                description: "Move to the previous item in the list".to_owned(),
            },
            PageNext => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::PageDown,
                    mode: KeyMod::EMPTY,
                }]],
                name: "list.page.next".to_owned(),
                description: "Move one page down in the list".to_owned(),
            },
            PagePrev => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::PageUp,
                    mode: KeyMod::EMPTY,
                }]],
                name: "list.page.prev".to_owned(),
                description: "Move one page up in the list".to_owned(),
            },
            Home => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::Home,
                    mode: KeyMod::EMPTY,
                }]],
                name: "list.home".to_owned(),
                description: "Move to the beginning of the list".to_owned(),
            },
            End => ActionDesc {
                chords: vec![vec![Key {
                    name: KeyName::End,
                    mode: KeyMod::EMPTY,
                }]],
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
    type Item: IntoView;

    /// Number of items in the list
    fn len(&self) -> usize;

    /// Get entry in the list by it's index
    fn get(&self, index: usize, theme: Theme) -> Option<Self::Item>;

    /// Check if list is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct List<T> {
    items: T,
    cursor: usize,
    theme: Theme,
    view_state: StdCell<ListViewState>,
}

/// Current state of the list view (it is only updated on layout calculation)
#[derive(Debug, Clone, Copy, Default)]
struct ListViewState {
    offset: usize,  // visible offset (first rendered element offset)
    visible: usize, // number of visible elements
}

impl<T: ListItems> List<T> {
    pub fn new(items: T, theme: Theme) -> Self {
        Self {
            items,
            cursor: 0,
            theme,
            view_state: StdCell::default(),
        }
    }

    pub fn items(&self) -> &T {
        &self.items
    }

    pub fn items_set(&mut self, items: T) -> T {
        self.cursor = 0;
        self.view_state = StdCell::default();
        std::mem::replace(&mut self.items, items)
    }

    pub fn current(&self) -> Option<T::Item> {
        self.items.get(self.cursor, self.theme.clone())
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn cursor_set(&mut self, cursor: usize) {
        self.cursor = cursor;
        self.view_state.get_mut().offset = cursor;
    }

    pub fn apply(&mut self, action: ListAction) {
        use ListAction::*;
        match action {
            ItemNext => self.cursor += 1,
            ItemPrev => {
                if self.cursor > 0 {
                    self.cursor -= 1
                }
            }
            PageNext => {
                let page_size = max(self.view_state.get().visible, 1);
                self.cursor += page_size;
            }
            PagePrev => {
                let page_size = max(self.view_state.get().visible, 1);
                if self.cursor >= page_size {
                    self.cursor -= page_size;
                }
            }
            Home => {
                self.cursor = 0;
                self.view_state.get_mut().offset = 0;
            }
            End => {
                self.cursor = self.items.len() - 1;
            }
        }
        if self.items.len() > 0 {
            self.cursor = clamp(self.cursor, 0, self.items.len() - 1);
        } else {
            self.cursor = 0;
        }
    }

    pub fn handle(&mut self, event: &TerminalEvent) {
        if let TerminalEvent::Key(Key { name, mode }) = event {
            match *mode {
                KeyMod::EMPTY => match name {
                    KeyName::Down => self.apply(ListAction::ItemNext),
                    KeyName::Up => self.apply(ListAction::ItemPrev),
                    KeyName::PageDown => self.apply(ListAction::PageNext),
                    KeyName::PageUp => self.apply(ListAction::PagePrev),
                    _ => {}
                },
                KeyMod::CTRL => match name {
                    KeyName::Char('n') => self.apply(ListAction::ItemNext),
                    KeyName::Char('p') => self.apply(ListAction::ItemPrev),
                    _ => {}
                },
                _ => {}
            }
        }
    }

    /// First visible element position
    #[cfg(test)]
    fn offset(&self) -> usize {
        self.view_state.get().offset
    }

    pub fn scroll_bar(&self) -> ListScrollBar<'_, T> {
        ListScrollBar { list: self }
    }

    pub fn theme_set(&mut self, theme: Theme) {
        self.theme = theme;
    }
}

struct ListItemView {
    view: Box<dyn View>,
    pointed: bool,
}

impl<'a, T> View for &'a List<T>
where
    T: ListItems,
    T::Item: 'static,
{
    fn render<'b>(
        &self,
        ctx: &ViewContext,
        surf: &'b mut TerminalSurface<'b>,
        layout: &Tree<Layout>,
    ) -> Result<(), Error> {
        if layout.size().is_empty() {
            return Ok(());
        }
        let mut surf = layout.apply_to(surf);

        // render items and scroll-bar (last layout in the list)
        surf.erase(self.theme.list_default);
        for item_layout in layout.children.iter() {
            let row = item_layout.pos().row;
            let height = item_layout.size().height;
            let item_data = item_layout
                .data::<ListItemView>()
                .ok_or(Error::InvalidLayout)?;

            // render cursor
            if item_data.pointed {
                let mut surf = surf.view_mut(row..row + height, ..);
                surf.erase(self.theme.list_selected);
                surf.draw_view(ctx, &self.theme.list_selected_indicator)?;
            }

            item_data
                .view
                .render(ctx, &mut surf.as_mut(), item_layout)?;
        }

        Ok(())
    }

    fn layout(&self, ctx: &ViewContext, ct: BoxConstraint) -> Tree<Layout> {
        // indicator (tag on the left side of the highlighted item)
        let indicator_layout = self.theme.list_selected_indicator.layout(ctx, ct);
        let indicator_width = indicator_layout.size().width;

        let height = ct.max().height;
        let width = ct.max().width;
        if height < 1 || width < indicator_width {
            return Tree::leaf(Layout::new());
        }

        // adjust offset so item pointed by cursor will be visible
        let offset = self
            .view_state
            .get()
            .offset
            .min(self.items.len().saturating_sub(height)); // offset is at least hight from the bottom
        let offset = if offset > self.cursor {
            self.cursor
        } else if height > 0 && offset + height - 1 < self.cursor {
            self.cursor - height + 1
        } else {
            offset
        };

        // create view and calculate layout for all visible items
        let child_ct = BoxConstraint::new(
            Size::new(0, width - indicator_width),
            Size::new(height, width - indicator_width),
        );
        let mut layouts: VecDeque<Tree<Layout>> = VecDeque::new();
        let mut children_height = 0;
        let mut children_removed = 0;
        // looping over items starting from offset
        for index in offset..offset + 2 * height {
            let item = match self.items().get(index, self.theme.clone()) {
                None => break,
                Some(item) => item,
            };

            // create view and calculate layout
            let pointed = index == self.cursor;
            let view = item.into_view().boxed();
            let mut layout = view.layout(ctx, child_ct);

            // make sure item height is at least one, otherwise it will result
            // in missing cursor
            let size = layout.size();
            layout.value.set_size(Size {
                height: max(size.height, 1),
                ..size
            });

            // insert layout
            children_height += layout.size().height;
            layout.set_data(ListItemView { view, pointed });
            layouts.push_back(layout);

            if children_height > height {
                // cursor is rendered, all height is taken
                if index > self.cursor {
                    break;
                }
                // cursor is not rendered, remove children from the top until
                // we have some space available
                while children_height > height {
                    if index == self.cursor && layouts.len() == 1 {
                        // do not remove the item if it is pointed by cursor
                        break;
                    }
                    match layouts.pop_front() {
                        Some(layout) => {
                            children_height -= layout.size().height;
                            children_removed += 1;
                        }
                        None => break,
                    }
                }
            }
        }

        // update view state
        self.view_state.set(ListViewState {
            offset: offset + children_removed,
            visible: layouts.len(),
        });

        // compute view offsets
        let mut view_offset = 0;
        for layout in layouts.iter_mut() {
            layout.set_pos(Position::new(view_offset, indicator_width));
            view_offset += layout.size().height;
        }

        Tree::new(
            Layout::new().with_size(Size::new(height, width)),
            layouts.into(),
        )
    }
}

pub struct ListScrollBar<'a, T> {
    list: &'a List<T>,
}

impl<'a, T: ListItems> View for ListScrollBar<'a, T> {
    fn render<'b>(
        &self,
        ctx: &ViewContext,
        surf: &'b mut TerminalSurface<'b>,
        layout: &Tree<Layout>,
    ) -> Result<(), Error> {
        let theme = &self.list.theme;
        let bg = theme.list_default.with_fg(theme.list_default.bg);
        let scroll_bar_face = bg.overlay(
            &Face::default()
                .with_fg(theme.scrollbar_on.bg)
                .with_bg(theme.scrollbar_off.bg),
        );
        let scroll_bar = ScrollBar::new(
            Axis::Vertical,
            scroll_bar_face,
            self.list.items.len(),
            self.list.cursor,
            self.list.view_state.get().visible,
        );
        scroll_bar.render(ctx, surf, layout)
    }

    fn layout(&self, _ctx: &ViewContext, ct: BoxConstraint) -> Tree<Layout> {
        Tree::leaf(Layout::new().with_size(Size::new(ct.max().height, 1)))
    }
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

        fn len(&self) -> usize {
            self.0.len()
        }

        fn get(&self, index: usize, _theme: Theme) -> Option<Self::Item> {
            let value = self.0.get(index)?;
            Some(value.to_string())
        }
    }

    #[test]
    fn test_list_basic() -> Result<(), Error> {
        let mut theme = Theme::light();
        theme.list_selected.bg = Some("#8ec07c".parse()?);

        let items = VecItems((0..60).collect());
        let mut list = List::new(items, theme.clone());

        print!("{:?}", list.into_view().debug(Size::new(8, 50)));
        assert_eq!(list.offset(), 0);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(8, 50)));
        assert_eq!(list.offset(), 0);

        (0..20).for_each(|_| list.apply(ListAction::ItemNext));
        print!("{:?}", list.into_view().debug(Size::new(8, 50)));
        assert_eq!(list.offset(), 14);

        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 17);

        Ok(())
    }

    #[test]
    fn test_list_multiline() -> Result<(), Error> {
        let mut theme = Theme::light();
        theme.list_selected.bg = Some("#8ec07c".parse()?);

        println!("multi-line entry");
        let items = VecItems::new([
            "1. other entry",
            "2. this is the third entry",
            "3. first multi line\n - first\n - second\n - thrid",
            "4. fourth entry",
        ]);
        let mut list = List::new(items, theme);

        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 0);

        (0..2).for_each(|_| list.apply(ListAction::ItemNext));
        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 1);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 2);

        println!("tall multi-line entry");
        let items = VecItems::new([
            "first",
            "too many lines to be shown\n - 1\n - 2\n - 3\n - 4\n - 5\n - 6",
            "last",
        ]);
        list.items_set(items);
        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 0);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
        assert_eq!(list.offset(), 1);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(5, 50)));
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
        print!("{:?}", list.into_view().debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 0);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 0);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 2);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 3);

        list.apply(ListAction::ItemNext);
        print!("{:?}", list.into_view().debug(Size::new(4, 20)));
        assert_eq!(list.offset(), 4);

        Ok(())
    }
}
