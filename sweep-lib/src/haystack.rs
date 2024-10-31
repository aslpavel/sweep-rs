use std::sync::Arc;

use either::Either;
use surf_n_term::{
    view::{BoxConstraint, Layout, Text, View, ViewContext, ViewLayout, ViewMutLayout},
    CellWrite, Position, TerminalSurface,
};

use crate::{PositionsRef, Theme};

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

    /// Return a view that renders haystack item in a list
    fn view(
        &self,
        ctx: &Self::Context,
        positions: PositionsRef<&[u8]>,
        theme: &Theme,
    ) -> Self::View;

    /// Side preview of the current item
    fn preview(
        &self,
        _ctx: &Self::Context,
        _positions: PositionsRef<&[u8]>,
        _theme: &Theme,
    ) -> Option<Self::Preview> {
        None
    }

    /// Large preview of the current item
    fn preview_large(
        &self,
        _ctx: &Self::Context,
        _positions: PositionsRef<&[u8]>,
        _theme: &Theme,
    ) -> Option<Self::PreviewLarge> {
        None
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

pub struct HaystackDefaultView {
    text: Text,
}

impl HaystackDefaultView {
    pub fn new<H: Haystack>(
        ctx: &H::Context,
        haystack: &H,
        positions: PositionsRef<&[u8]>,
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

    fn view(
        &self,
        ctx: &Self::Context,
        positions: PositionsRef<&[u8]>,
        theme: &Theme,
    ) -> Self::View {
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

    fn view(
        &self,
        ctx: &Self::Context,
        positions: PositionsRef<&[u8]>,
        theme: &Theme,
    ) -> Self::View {
        HaystackDefaultView::new(ctx, self, positions, theme)
    }

    fn haystack_scope<S>(&self, _ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope)
    }
}
