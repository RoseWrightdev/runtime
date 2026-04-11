use std::future::Future;
use std::cell::UnsafeCell;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct Task {
    future: UnsafeCell<RawFuture>,
}

impl Task {
    pub(crate) fn new() -> Self {
        Self { future: UnsafeCell::new(RawFuture {}) }
    }
}

pub(crate) struct RawFuture {}

impl Future for RawFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}
