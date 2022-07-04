use std::{cmp::max, io::Write, str::FromStr};
use surf_n_term::{
    common::clamp, Blend, Cell, Color, Error, Face, FaceAttrs, Key, KeyMod, KeyName, Size, Surface,
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
    pub scrollbar_on: Face,
    pub scrollbar_off: Face,
}

impl Theme {
    pub fn from_palette(fg: RGBA, bg: RGBA, accent: RGBA) -> Self {
        let cursor = {
            let cursor_bg = bg.blend(accent.with_alpha(0.8), Blend::Over);
            let cursor_fg = cursor_bg.best_contrast(bg, fg);
            Face::new(Some(cursor_fg), Some(cursor_bg), FaceAttrs::EMPTY)
        };
        let input = Face::new(Some(fg), Some(bg), FaceAttrs::EMPTY);
        let list_default = Face::new(
            Some(bg.blend(fg.with_alpha(0.8), Blend::Over)),
            Some(bg),
            FaceAttrs::EMPTY,
        );
        let list_selected = Face::new(
            Some(bg.blend(fg.with_alpha(0.8), Blend::Over)),
            Some(bg.blend(fg.with_alpha(0.1), Blend::Over)),
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

/// Anything that can be show on the terminal
pub trait TerminalDisplay {
    /// Display object by updating terminal surface
    fn display(&self, surf: &mut TerminalSurface<'_>) -> Result<(), Error>;
    /// Return the size of the displayed object given the surface size
    fn size_hint(&self, surf_size: Size) -> Option<Size>;
}

impl<'a, T> TerminalDisplay for &'a T
where
    T: TerminalDisplay + ?Sized,
{
    fn display(&self, surf: &mut TerminalSurface<'_>) -> Result<(), Error> {
        (*self).display(surf)
    }

    fn size_hint(&self, surf_size: Size) -> Option<Size> {
        (*self).size_hint(surf_size)
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
    offset: usize,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub fn new() -> Self {
        Self {
            before: Default::default(),
            after: Default::default(),
            offset: 0,
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
        self.offset = 0;
    }

    pub fn render(
        &mut self,
        theme: &Theme,
        mut surf: impl SurfaceMut<Item = Cell>,
    ) -> Result<(), Error> {
        surf.erase(theme.input);
        let size = surf.width() * surf.height();
        if size < 2 {
            return Ok(());
        } else if self.offset > self.before.len() {
            self.offset = self.before.len();
        } else if self.offset + size < self.before.len() + 1 {
            self.offset = self.before.len() - size + 1;
        }
        let mut writer = surf.writer().face(theme.input);
        for c in self.before[self.offset..].iter() {
            writer.put(Cell::new(theme.input, Some(*c)));
        }
        let mut iter = self.after.iter().rev();
        writer.put(Cell::new(theme.cursor, iter.next().copied()));
        for c in iter {
            writer.put(Cell::new(theme.input, Some(*c)));
        }
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
    type Item: TerminalDisplay;

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
    offset: usize,
    cursor: usize,
    height_hint: usize,
}

impl<T: ListItems> List<T> {
    pub fn new(items: T) -> Self {
        Self {
            items,
            offset: 0,
            cursor: 0,
            height_hint: 1,
        }
    }

    pub fn items(&self) -> &T {
        &self.items
    }

    pub fn items_set(&mut self, items: T) -> T {
        self.offset = 0;
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

    pub fn render(
        &mut self,
        theme: &Theme,
        mut surf: impl SurfaceMut<Item = Cell>,
    ) -> Result<(), Error> {
        surf.erase(theme.list_default);
        if surf.height() < 1 || surf.width() < 5 {
            return Ok(());
        }
        if self.offset > self.cursor {
            self.offset = self.cursor;
        } else if self.offset + surf.height() - 1 < self.cursor {
            self.offset = self.cursor - surf.height() + 1;
        }

        // items
        let size = Size {
            width: surf.width() - 4, // exclude left border and scroll bar
            height: surf.height(),
        };
        let items: Vec<_> = (self.offset..self.offset + surf.height())
            .filter_map(|index| {
                let item = self.items.get(index)?;
                let item_size = match item.size_hint(size) {
                    Some(item_size) => Size {
                        height: max(1, item_size.height),
                        width: item_size.width,
                    },
                    None => Size {
                        height: 1,
                        width: size.width,
                    },
                };
                Some((index, item_size, item))
            })
            .collect();
        // make sure items will fit
        let mut cursor_found = false;
        let mut items_height = 0;
        let mut first = 0;
        for (index, size, _item) in items.iter() {
            items_height += size.height;
            if items_height > surf.height() {
                if cursor_found {
                    break;
                }
                while items_height > surf.height() {
                    items_height -= items[first].1.height;
                    first += 1;
                }
            }
            cursor_found = cursor_found || *index == self.cursor;
        }
        self.height_hint = items.len();
        self.offset += first;
        // render items
        let mut row: usize = 0;
        for (index, item_size, item) in items[first..].iter() {
            let mut item_surf = surf.view_mut(row..row + item_size.height, ..-1);
            row += item_size.height;
            if item_surf.is_empty() {
                break;
            }
            if *index == self.cursor {
                item_surf.erase(theme.list_selected);
                let mut writer = item_surf
                    .writer()
                    .face(theme.list_selected.with_fg(Some(theme.accent)));
                writer.write_all(" ● ".as_ref())?;
            } else {
                let mut writer = item_surf.writer().face(theme.list_default);
                writer.write_all("   ".as_ref())?;
            };
            let mut text_surf = item_surf.view_mut(.., 3..);
            if *index == self.cursor {
                text_surf.erase(theme.list_selected);
                item.display(&mut text_surf)?;
            } else {
                text_surf.erase(theme.list_default);
                item.display(&mut text_surf)?;
            }
        }

        // scroll bar
        let (sb_offset, sb_filled) = if self.items.len() != 0 {
            let sb_filled = clamp(
                surf.height() * items.len() / self.items.len(),
                1,
                surf.height(),
            );
            let sb_offset = (surf.height() - sb_filled) * (self.cursor + 1) / self.items.len();
            (sb_offset, sb_filled + sb_offset)
        } else {
            (0, surf.height())
        };
        let range = 0..surf.height();
        let mut sb = surf.view_mut(.., -1);
        let mut sb_writer = sb.writer();
        for i in range {
            if i < sb_offset || i >= sb_filled {
                sb_writer.put_char(' ', theme.scrollbar_off);
            } else {
                sb_writer.put_char(' ', theme.scrollbar_on);
            }
        }

        Ok(())
    }
}
