use std::sync::Arc;

use either::Either;
use surf_n_term::{
    view::{BoxConstraint, Text, View, ViewContext, ViewLayout, ViewMutLayout},
    CellWrite, TerminalSurface,
};

use crate::{Positions, Theme};

/// Haystack
///
/// Item that can be scored/ranked/shown by sweep
pub trait Haystack: std::fmt::Debug + Clone + Send + Sync + 'static {
    /// Haystack context passed when generating view and preview (for example
    /// [Candidate](crate::Candidate) reference resolution)
    type Context: Clone + Send + Sync;
    type View: View + Send + Sync;
    type Preview: HaystackPreview;
    type PreviewLarge: HaystackPreview;

    /// Scope function is called with all characters one after another that will
    /// be searchable by [Scorer]
    fn haystack_scope<S>(&self, ctx: &Self::Context, scope: S)
    where
        S: FnMut(char);

    /// Return a view that renders haystack item in a list
    fn view(&self, ctx: &Self::Context, positions: &Positions, theme: &Theme) -> Self::View;

    /// Side preview of the current item
    fn preview(
        &self,
        _ctx: &Self::Context,
        _positions: &Positions,
        _theme: &Theme,
    ) -> Option<Self::Preview> {
        None
    }

    /// Large preview of the current item
    fn preview_large(
        &self,
        _ctx: &Self::Context,
        _positions: &Positions,
        _theme: &Theme,
    ) -> Option<Self::PreviewLarge> {
        None
    }
}

pub trait HaystackPreview: View {
    fn flex(&self) -> Option<f64>;
}

impl HaystackPreview for () {
    fn flex(&self) -> Option<f64> {
        None
    }
}

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
}

impl HaystackPreview for Arc<dyn HaystackPreview> {
    fn flex(&self) -> Option<f64> {
        (**self).flex()
    }
}

pub struct HaystackDefaultView {
    text: Text,
}

impl HaystackDefaultView {
    pub fn new<H: Haystack>(
        ctx: &H::Context,
        haystack: &H,
        positions: &Positions,
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

    fn view(&self, ctx: &Self::Context, positions: &Positions, theme: &Theme) -> Self::View {
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

    fn view(&self, ctx: &Self::Context, positions: &Positions, theme: &Theme) -> Self::View {
        HaystackDefaultView::new(ctx, self, positions, theme)
    }

    fn haystack_scope<S>(&self, _ctx: &Self::Context, scope: S)
    where
        S: FnMut(char),
    {
        self.chars().for_each(scope)
    }
}
