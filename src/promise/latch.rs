use std::{
    sync::{Arc, atomic::AtomicBool},
    task::Waker,
};

use godot::classes::{
    class_macros::private::virtuals::ZipReader::{Callable, RustCallable, Signal, Variant},
    object::ConnectFlags,
};
use parking_lot::Mutex;

/// A [RustCallable] that stores whether it's been invoked, and can be polled as a [Future].
#[derive(Clone)]
pub struct Latch {
    id: uuid::Uuid,
    invoked: Arc<AtomicBool>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl std::hash::Hash for Latch {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl PartialEq for Latch {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl std::fmt::Display for Latch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Latch<{}>", self.id)
    }
}

impl RustCallable for Latch {
    fn invoke(&mut self, _: &[&Variant]) -> Variant {
        Self::invoke(self);
        Variant::nil()
    }
}

impl Default for Latch {
    fn default() -> Self {
        Self::new()
    }
}

impl Latch {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            invoked: Arc::new(AtomicBool::new(false)),
            waker: Default::default(),
        }
    }

    pub fn new_immediate() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            invoked: Arc::new(AtomicBool::new(true)),
            waker: Default::default(),
        }
    }

    pub fn from_signal(signal: Signal) -> Self {
        let res = Self::new();
        let callable = Callable::from_custom(res.clone());
        signal.connect_flags(&callable, ConnectFlags::ONE_SHOT);
        res
    }

    pub fn invoked(&self) -> bool {
        self.invoked.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn invoke(&self) {
        self.invoked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // TODO :: can this deadlock?
        if let Some(c) = self.waker.lock().take() {
            c.wake();
        }
    }
}

impl std::future::Future for Latch {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if self.invoked.load(std::sync::atomic::Ordering::SeqCst) {
            std::task::Poll::Ready(())
        } else {
            // TODO :: i'm pretty sure there's no situation in which this would deadlock but i
            // haven't fully thought it through
            *self.waker.lock() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}
