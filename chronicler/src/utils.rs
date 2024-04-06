use std::{
    future::Future,
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
};
use sweep::surf_n_term::{
    view::{Align, Container, Flex, IntoView},
    Face,
};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct AbortJoinHandle<T> {
    handle: JoinHandle<T>,
}

impl<T> Drop for AbortJoinHandle<T> {
    fn drop(&mut self) {
        self.handle.abort()
    }
}

impl<T> Future for AbortJoinHandle<T> {
    type Output = <JoinHandle<T> as Future>::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle).poll(cx)
    }
}

impl<T> From<JoinHandle<T>> for AbortJoinHandle<T> {
    fn from(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }
}

impl<T> Deref for AbortJoinHandle<T> {
    type Target = JoinHandle<T>;
    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

pub(crate) struct Table<'a> {
    left_width: usize,
    left_face: Option<Face>,
    right_face: Option<Face>,
    view: Flex<'a>,
}

impl<'a> Table<'a> {
    pub(crate) fn new(
        left_width: usize,
        left_face: Option<Face>,
        right_face: Option<Face>,
    ) -> Self {
        Self {
            left_width,
            left_face,
            right_face,
            view: Flex::column(),
        }
    }

    pub(crate) fn push(&mut self, left: impl IntoView + 'a, right: impl IntoView + 'a) {
        let row = Flex::row()
            .add_child_ext(
                Container::new(left)
                    .with_width(self.left_width)
                    .with_horizontal(Align::Expand),
                None,
                self.left_face,
                Align::Start,
            )
            .add_child_ext(right, None, self.right_face, Align::Start);
        self.view.push_child(row)
    }
}

impl<'a> IntoView for Table<'a> {
    type View = Flex<'a>;

    fn into_view(self) -> Self::View {
        self.view
    }
}
