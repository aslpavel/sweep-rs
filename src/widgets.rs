use std::{cell::Cell as StdCell, collections::VecDeque, io::Write, str::FromStr};
use surf_n_term::{
    common::clamp,
    view::{Axis, BoxConstraint, IntoView, Layout, ScrollBar, Tree, View, ViewContext},
    Cell, Color, Error, Face, FaceAttrs, Key, KeyMod, KeyName, Position, Size, SurfaceMut,
    TerminalEvent, TerminalSurface, TerminalSurfaceExt, RGBA,
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
    pub scrollbar_on: Face,
    pub scrollbar_off: Face,
}

impl Theme {
    pub fn from_palette(fg: RGBA, bg: RGBA, accent: RGBA) -> Self {
        let cursor = {
            let cursor_bg = bg.blend_over(accent.with_alpha(0.8));
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
            Some(bg.blend_over(fg.with_alpha(0.1))),
            FaceAttrs::EMPTY,
        );
        let scrollbar_on = Face::new(None, Some(accent.with_alpha(0.8)), FaceAttrs::EMPTY);
        let scrollbar_off = Face::new(None, Some(accent.with_alpha(0.5)), FaceAttrs::EMPTY);
        Self {
            fg,
            bg,
            accent,
            cursor,
            input,
            list_default,
            list_selected,
            scrollbar_on,
            scrollbar_off,
        }
    }

    pub fn light() -> Self {
        Self::from_palette(
            "#3c3836".parse().unwrap(),
            "#fbf1c7".parse().unwrap(),
            "#8f3f71".parse().unwrap(),
        )
    }

    pub fn dark() -> Self {
        Self::from_palette(
            "#ebdbb2".parse().unwrap(),
            "#282828".parse().unwrap(),
            "#d3869b".parse().unwrap(),
        )
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
                _ => return Err(Error::ParseError("Theme", string.to_string())),
            };
            Ok(theme)
        })
    }
}

/// Action description with default binding
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionDesc<A> {
    /// action
    pub action: A,
    /// default binding
    pub chord: &'static [&'static [Key]],
    /// action name
    pub name: &'static str,
    /// action description
    pub description: &'static str,
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
    pub fn description() -> &'static [ActionDesc<Self>] {
        &[
            ActionDesc {
                action: InputAction::CursorForward,
                chord: &[&[Key {
                    name: KeyName::Right,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.move.forward",
                description: "Move cursor forward in the input field",
            },
            ActionDesc {
                action: InputAction::CursorBackward,
                chord: &[&[Key {
                    name: KeyName::Left,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.move.backward",
                description: "Move cursor backward in the input field",
            },
            ActionDesc {
                action: InputAction::CursorEnd,
                chord: &[&[Key {
                    name: KeyName::Char('e'),
                    mode: KeyMod::CTRL,
                }]],
                name: "input.move.end",
                description: "Move cursor to the end of the input",
            },
            ActionDesc {
                action: InputAction::CursorStart,
                chord: &[&[Key {
                    name: KeyName::Char('a'),
                    mode: KeyMod::CTRL,
                }]],
                name: "input.move.start",
                description: "Move cursor to the start of the input",
            },
            ActionDesc {
                action: InputAction::CursorNextWord,
                chord: &[&[Key {
                    name: KeyName::Char('f'),
                    mode: KeyMod::ALT,
                }]],
                name: "input.move.next_word",
                description: "Move cursor to the end of the current word",
            },
            ActionDesc {
                action: InputAction::CursorPrevWord,
                chord: &[&[Key {
                    name: KeyName::Char('b'),
                    mode: KeyMod::ALT,
                }]],
                name: "input.move.prev_word",
                description: "Move cursor to the start of the word",
            },
            ActionDesc {
                action: InputAction::DeleteBackward,
                chord: &[&[Key {
                    name: KeyName::Backspace,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.delete.backward",
                description: "Delete previous char",
            },
            ActionDesc {
                action: InputAction::DeleteForward,
                chord: &[&[Key {
                    name: KeyName::Delete,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.delete.forward",
                description: "Delete next char",
            },
            ActionDesc {
                action: InputAction::DeleteEnd,
                chord: &[&[Key {
                    name: KeyName::Char('k'),
                    mode: KeyMod::CTRL,
                }]],
                name: "input.delete.end",
                description: "Delete all input after cursor",
            },
        ]
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

    #[allow(unused)]
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
            writer.put(Cell::new(self.theme.input, Some(*c)));
        }
        let mut iter = self.after.iter().rev();
        writer.put(Cell::new(self.theme.cursor, iter.next().copied()));
        for c in iter {
            writer.put(Cell::new(self.theme.input, Some(*c)));
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
}

impl ListAction {
    pub fn description() -> &'static [ActionDesc<Self>] {
        &[
            ActionDesc {
                action: ListAction::ItemNext,
                chord: &[
                    &[Key {
                        name: KeyName::Down,
                        mode: KeyMod::EMPTY,
                    }],
                    &[Key {
                        name: KeyName::Char('n'),
                        mode: KeyMod::CTRL,
                    }],
                ],
                name: "list.item.next",
                description: "Move to the next item in the list",
            },
            ActionDesc {
                action: ListAction::ItemPrev,
                chord: &[
                    &[Key {
                        name: KeyName::Up,
                        mode: KeyMod::EMPTY,
                    }],
                    &[Key {
                        name: KeyName::Char('p'),
                        mode: KeyMod::CTRL,
                    }],
                ],
                name: "list.item.prev",
                description: "Move to the previous item in the list",
            },
            ActionDesc {
                action: ListAction::PageNext,
                chord: &[&[Key {
                    name: KeyName::PageDown,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.page.next",
                description: "Move one page down in the list",
            },
            ActionDesc {
                action: ListAction::PagePrev,
                chord: &[&[Key {
                    name: KeyName::PageUp,
                    mode: KeyMod::EMPTY,
                }]],
                name: "input.page.prev",
                description: "Move one page up in the list",
            },
        ]
    }
}

pub trait ListItems {
    type Item: IntoView;

    /// Number of items in the list
    fn len(&self) -> usize;

    /// Get entry in the list by it's index
    fn get(&self, index: usize) -> Option<Self::Item>;

    /// Check if list is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct List<T> {
    items: T,
    theme: Theme,
    /// visible offset
    offset: StdCell<usize>,
    /// current cursor position
    cursor: usize,
    height_hint: usize,
}

impl<T: ListItems> List<T> {
    pub fn new(items: T, theme: Theme) -> Self {
        Self {
            items,
            theme,
            offset: StdCell::new(0),
            cursor: 0,
            height_hint: 1,
        }
    }

    pub fn items(&self) -> &T {
        &self.items
    }

    pub fn items_set(&mut self, items: T) -> T {
        self.offset = StdCell::new(0);
        self.cursor = 0;
        std::mem::replace(&mut self.items, items)
    }

    pub fn current(&self) -> Option<T::Item> {
        self.items.get(self.cursor)
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
            PageNext => self.cursor += self.height_hint,
            PagePrev => {
                if self.cursor >= self.height_hint {
                    self.cursor -= self.height_hint
                }
            }
        }
        if self.items.len() > 0 {
            self.cursor = clamp(self.cursor, 0, self.items.len() - 1);
        } else {
            self.cursor = 0;
        }
    }

    #[allow(unused)]
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

    fn offset(&self) -> usize {
        self.offset.get()
    }

    fn offset_fix(&self, height: usize) -> usize {
        if self.offset() > self.cursor {
            self.offset.replace(self.cursor);
        } else if height > 0 && self.offset() + height - 1 < self.cursor {
            self.offset.replace(self.cursor - height + 1);
        }
        self.offset.get()
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
                let mut surf = surf.view_mut(row..row + height, ..-1);
                surf.erase(self.theme.list_selected);
                let cursor_face = Face::default().with_fg(Some(self.theme.accent));
                write!(surf.writer().face_set(cursor_face), " ● ")?;
            }

            item_data
                .view
                .render(ctx, &mut surf.as_mut(), item_layout)?;
        }

        Ok(())
    }

    fn layout(&self, ctx: &ViewContext, ct: BoxConstraint) -> Tree<Layout> {
        let height = ct.max().height;
        let width = ct.max().width;
        if height < 1 || width < 5 {
            return Tree::leaf(Layout::new());
        }

        // offset if it is too far from the cursor
        let offset = self.offset_fix(ct.max().height);
        let child_ct = BoxConstraint::new(Size::new(0, width - 4), Size::new(height, width - 4));
        let mut layouts: VecDeque<Tree<Layout>> = VecDeque::new();
        let mut children_height = 0;
        let mut children_removed = 0;
        for index in offset..offset + height {
            let item = match self.items().get(index) {
                None => break,
                Some(item) => item,
            };

            // create view and calculate layout
            let pointed = index == self.cursor;
            let view = item.into_view().boxed();
            let mut layout = view.layout(ctx, child_ct);

            // insert layout
            children_height += layout.size().height;
            layout.set_data(ListItemView { view, pointed });
            layouts.push_back(layout);

            if children_height > height {
                if index > self.cursor {
                    break; // all height is occupied, cursor is rendered
                }
                // we have not reached cursor yet, removing items from the top
                // until we can fit new child
                while children_height > height && !layouts.is_empty() {
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

        // fix offset
        self.offset.set(offset + children_removed);

        // compute offsets
        let mut offset = 0;
        for layout in layouts.iter_mut() {
            layout.set_pos(Position::new(offset, 3));
            offset += layout.size().height;
        }

        // add scroll bar
        let scroll_bar_face = Face::default()
            .with_fg(self.theme.scrollbar_on.bg)
            .with_bg(self.theme.scrollbar_off.bg);
        let scroll_bar = ScrollBar::new(
            Axis::Vertical,
            scroll_bar_face,
            self.items.len(),
            self.cursor,
            layouts.len(),
        );
        let mut scroll_bar_layout =
            scroll_bar.layout(ctx, BoxConstraint::tight(Size::new(height, 1)));
        scroll_bar_layout.set_pos(Position::new(0, width - 1));
        scroll_bar_layout.set_data(ListItemView {
            view: Box::new(scroll_bar),
            pointed: false,
        });
        layouts.push_back(scroll_bar_layout);

        Tree::new(
            Layout::new().with_size(Size::new(height, width)),
            layouts.into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Display;
    use surf_n_term::view::Text;

    struct VecItems<T>(Vec<T>);

    impl<T> ListItems for VecItems<T>
    where
        T: Display + Clone,
    {
        type Item = Text<'static>;

        fn len(&self) -> usize {
            self.0.len()
        }

        fn get(&self, index: usize) -> Option<Self::Item> {
            let value = self.0.get(index)?;
            Some(Text::new(value.to_string()))
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
        let items = VecItems(
            [
                "1. other entry",
                "2. this is the third entry",
                "3. first multi line\n - first\n - second\n - thrid",
                "4. fourth entry",
            ]
            .into_iter()
            .collect(),
        );
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
        let items = VecItems(
            [
                "first",
                "too many line to be shown\n - 1\n - 2\n - 3\n - 4\n - 5\n - 6",
                "last",
            ]
            .into_iter()
            .collect(),
        );
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
        let items = VecItems(
            [
                "first short",
                "second",
                "fist very very long line\nwhich is also multi line that should split",
                "second very very long line that should be split into multiple lines and rendered correctly",
                "last",
            ]
            .into_iter()
            .collect(),
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
