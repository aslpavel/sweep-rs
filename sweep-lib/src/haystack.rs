use std::sync::Arc;

use either::Either;
use surf_n_term::{
    render::CellKind,
    view::{BoxConstraint, Layout, Text, View, ViewContext, ViewLayout, ViewMutLayout},
    CellWrite, KeyChord, Position, TerminalSurface,
};

use crate::{Positions, Theme};

/// Haystack
///
/// Item that can be scored/ranked/shown by sweep
pub trait Haystack: std::fmt::Debug + Clone + Send + Sync + 'static {
    /// Haystack context passed when generating view and preview (for example
    /// [Candidate](crate::Candidate) reference resolution)
    type Context: Clone + Send + Sync;
    type View: View;
    type Preview: HaystackPreview;
    type PreviewLarge: HaystackPreview + Clone;

    /// Scope function is called with all characters one after another that will
    /// be searchable by [Scorer]
    fn haystack_scope<S>(&self, ctx: &Self::Context, scope: S)
    where
        S: FnMut(char);

    /// Key that can be used to select that item in hotkey mode
    fn hotkey(&self) -> Option<KeyChord> {
        None
    }

    /// Return a view that renders haystack item in a list
    fn view(&self, ctx: &Self::Context, positions: Positions<&[u8]>, theme: &Theme) -> Self::View;

    /// Side preview of the current item
    fn preview(
        &self,
        _ctx: &Self::Context,
        _positions: Positions<&[u8]>,
        _theme: &Theme,
    ) -> Option<Self::Preview> {
        None
    }

    /// Large preview of the current item
    fn preview_large(
        &self,
        _ctx: &Self::Context,
        _positions: Positions<&[u8]>,
        _theme: &Theme,
    ) -> Option<Self::PreviewLarge> {
        None
    }

    // Tag haystack with a value, useful for `quick_select`
    fn tagged<T>(self, tag: T, hotkey: Option<KeyChord>) -> HaystackTagged<Self, T>
    where
        Self: Sized,
    {
        HaystackTagged {
            haystack: self,
            hotkey,
            tag,
        }
    }
}

/// View that is used for preview, and include addition methods to make it more functional
pub trait HaystackPreview: View {
    /// Flex value when use as a child
    fn flex(&self) -> Option<f64> {
        Some(1.0)
    }

    /// Current preview layout
    ///
    /// Size represents full size of the preview
    /// Offset represents vertical and horizontal scroll position
    fn preview_layout(&self) -> Layout {
        Layout::new()
    }

    /// When rendering offset by specified position (used for scrolling)
    ///
    /// Returns updated offset, default implementation is not scrollable hence
    /// it is always returns `Position::origin()`
    fn set_offset(&self, offset: Position) -> Position {
        _ = offset;
        Position::origin()
    }
}

impl HaystackPreview for () {}

impl<L, R> HaystackPreview for Either<L, R>
where
    L: HaystackPreview,
    R: HaystackPreview,
{
    fn flex(&self) -> Option<f64> {
        match self {
            Either::Left(left) => left.flex(),
            Either::Right(right) => right.flex(),
        }
    }

    fn preview_layout(&self) -> Layout {
        match self {
            Either::Left(left) => left.preview_layout(),
            Either::Right(right) => right.preview_layout(),
        }
    }

    fn set_offset(&self, offset: Position) -> Position {
        match self {
            Either::Left(left) => left.set_offset(offset),
            Either::Right(right) => right.set_offset(offset),
        }
    }
}

impl<T: HaystackPreview + ?Sized> HaystackPreview for Arc<T> {
    fn flex(&self) -> Option<f64> {
        (**self).flex()
    }

    fn preview_layout(&self) -> Layout {
        (**self).preview_layout()
    }

    fn set_offset(&self, offset: Position) -> Position {
        (**self).set_offset(offset)
    }
}

#[derive(Clone)]
pub struct HaystackTagged<H, T> {
    pub haystack: H,
    pub hotkey: Option<KeyChord>,
    pub tag: T,
}

impl<H: Haystack, T> std::fmt::Debug for HaystackTagged<H, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HaystackTagged")
            .field("haystack", &self.haystack)
            .field("hotkey", &self.hotkey)
            .finish()
    }
}

impl<H, T> Haystack for HaystackTagged<H, T>
where
    H: Haystack,
    T: Clone + Send + Sync + 'static,
{
    type Context = H::Context;
    type View = H::View;
    type Preview = H::Preview;
    type PreviewLarge = H::PreviewLarge;

    fn haystack_scope<S>(&self, ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.haystack.haystack_scope(ctx, scope);
    }

    fn view(&self, ctx: &Self::Context, positions: Positions<&[u8]>, theme: &Theme) -> Self::View {
        self.haystack.view(ctx, positions, theme)
    }

    fn hotkey(&self) -> Option<KeyChord> {
        self.hotkey.clone().or(self.haystack.hotkey())
    }

    fn preview(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &Theme,
    ) -> Option<Self::Preview> {
        self.haystack.preview(ctx, positions, theme)
    }

    fn preview_large(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &Theme,
    ) -> Option<Self::PreviewLarge> {
        self.haystack.preview_large(ctx, positions, theme)
    }
}

impl<L, R> Haystack for Either<L, R>
where
    L: Haystack,
    R: Haystack,
{
    type Context = (L::Context, R::Context);
    type View = Either<L::View, R::View>;
    type Preview = Either<L::Preview, R::Preview>;
    type PreviewLarge = Either<L::PreviewLarge, R::PreviewLarge>;

    fn haystack_scope<S>(&self, ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        match self {
            Either::Left(left) => left.haystack_scope(&ctx.0, scope),
            Either::Right(right) => right.haystack_scope(&ctx.1, scope),
        }
    }

    fn view(&self, ctx: &Self::Context, positions: Positions<&[u8]>, theme: &Theme) -> Self::View {
        match self {
            Either::Left(left) => left.view(&ctx.0, positions, theme).left_view(),
            Either::Right(right) => right.view(&ctx.1, positions, theme).right_view(),
        }
    }

    fn hotkey(&self) -> Option<KeyChord> {
        None
    }

    fn preview(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &Theme,
    ) -> Option<Self::Preview> {
        let preview = match self {
            Either::Left(left) => left.preview(&ctx.0, positions, theme)?.left_view(),
            Either::Right(right) => right.preview(&ctx.1, positions, theme)?.right_view(),
        };
        Some(preview)
    }

    fn preview_large(
        &self,
        ctx: &Self::Context,
        positions: Positions<&[u8]>,
        theme: &Theme,
    ) -> Option<Self::PreviewLarge> {
        let preview = match self {
            Either::Left(left) => left.preview_large(&ctx.0, positions, theme)?.left_view(),
            Either::Right(right) => right.preview_large(&ctx.1, positions, theme)?.right_view(),
        };
        Some(preview)
    }
}

pub struct HaystackDefaultView {
    text: Text,
}

impl HaystackDefaultView {
    pub fn new<H: Haystack>(
        ctx: &H::Context,
        haystack: &H,
        positions: Positions<&[u8]>,
        theme: &Theme,
    ) -> Self {
        let mut text = Text::new();
        let mut index = 0;
        haystack.haystack_scope(ctx, |char| {
            text.set_face(if positions.get(index) {
                theme.list_highlight
            } else {
                theme.list_text
            });
            text.put_char(char);
            index += 1;
        });
        Self { text }
    }
}

impl View for HaystackDefaultView {
    fn render(
        &self,
        ctx: &ViewContext,
        surf: TerminalSurface<'_>,
        layout: ViewLayout<'_>,
    ) -> Result<(), surf_n_term::Error> {
        self.text.render(ctx, surf, layout)
    }

    fn layout(
        &self,
        ctx: &ViewContext,
        ct: BoxConstraint,
        layout: ViewMutLayout<'_>,
    ) -> Result<(), surf_n_term::Error> {
        self.text.layout(ctx, ct, layout)
    }
}

pub struct HaystackBasicPreview<V> {
    view: V,
    flex: Option<f64>,
}

impl<V> HaystackBasicPreview<V> {
    pub fn new(view: V, flex: Option<f64>) -> Self {
        Self { view, flex }
    }
}

impl<V> View for HaystackBasicPreview<V>
where
    V: View,
{
    fn render(
        &self,
        ctx: &ViewContext,
        surf: TerminalSurface<'_>,
        layout: ViewLayout<'_>,
    ) -> Result<(), surf_n_term::Error> {
        self.view.render(ctx, surf, layout)
    }

    fn layout(
        &self,
        ctx: &ViewContext,
        ct: BoxConstraint,
        layout: ViewMutLayout<'_>,
    ) -> Result<(), surf_n_term::Error> {
        self.view.layout(ctx, ct, layout)
    }
}

impl<V> HaystackPreview for HaystackBasicPreview<V>
where
    V: View,
{
    fn flex(&self) -> Option<f64> {
        self.flex
    }
}

impl Haystack for String {
    type Context = ();
    type View = HaystackDefaultView;
    type Preview = ();
    type PreviewLarge = ();

    fn view(&self, ctx: &Self::Context, positions: Positions<&[u8]>, theme: &Theme) -> Self::View {
        HaystackDefaultView::new(ctx, self, positions, theme)
    }

    fn haystack_scope<S>(&self, _ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope)
    }
}

impl Haystack for &'static str {
    type Context = ();
    type View = HaystackDefaultView;
    type Preview = ();
    type PreviewLarge = ();

    fn view(&self, ctx: &Self::Context, positions: Positions<&[u8]>, theme: &Theme) -> Self::View {
        HaystackDefaultView::new(ctx, self, positions, theme)
    }

    fn haystack_scope<S>(&self, _ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope)
    }
}

impl Haystack for Text {
    type Context = ();
    type View = Text;
    type Preview = ();
    type PreviewLarge = ();

    fn haystack_scope<S>(&self, _ctx: &Self::Context, mut scope: S)
    where
        S: FnMut(char),
    {
        self.cells().iter().for_each(|cell| {
            if let CellKind::Char(ch) = cell.kind() {
                scope(*ch);
            }
        });
    }

    fn view(&self, _ctx: &Self::Context, positions: Positions<&[u8]>, theme: &Theme) -> Self::View {
        let mut index = 0;
        self.cells()
            .iter()
            .map(|cell| {
                let cell = cell.clone();
                if matches!(cell.kind(), CellKind::Char(..)) {
                    let highligted = positions.get(index);
                    index += 1;
                    if highligted {
                        let face = cell.face().overlay(&theme.list_highlight);
                        cell.with_face(face)
                    } else {
                        cell
                    }
                } else {
                    cell
                }
            })
            .collect()
    }
}
