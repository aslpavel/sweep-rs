use std::{
    future::Future,
    ops::Deref,
    pin::Pin,
    task::{Context, Poll},
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
