use std::{sync::Arc, task::Waker};

use godot::classes::class_macros::private::virtuals::ZipReader::{Callable, RustCallable, Variant};
use parking_lot::Mutex;

/// A [RustCallable] that gets executed during idle time, and can be awaited as a [Future].
pub struct IdleTask<Output, Task: FnOnce() -> Output> {
    id: uuid::Uuid,
    task: Arc<Mutex<Option<Task>>>,
    result: Arc<Mutex<Option<Output>>>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl<O, T: FnOnce() -> O> Clone for IdleTask<O, T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            task: self.task.clone(),
            result: self.result.clone(),
            waker: self.waker.clone(),
        }
    }
}

impl<O, T: FnOnce() -> O> std::hash::Hash for IdleTask<O, T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<O, T: FnOnce() -> O> PartialEq for IdleTask<O, T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<O, T: FnOnce() -> O> std::fmt::Display for IdleTask<O, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdleTask<{}>", self.id)
    }
}

impl<O: Send + 'static, T: FnOnce() -> O + Send + 'static> RustCallable for IdleTask<O, T> {
    fn invoke(&mut self, args: &[&Variant]) -> Variant {
        #[cfg(debug_assertions)]
        if !args.is_empty() {
            tracing::warn!(task = %self, params = ?args, "invoked IdleTask with parameters; ignoring");
        }
        Self::invoke(self);
        Variant::nil()
    }
}

impl<O, T: FnOnce() -> O> std::future::Future for IdleTask<O, T> {
    type Output = O;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Some(res) = self.result.try_lock().and_then(|mut lock| lock.take()) {
            std::task::Poll::Ready(res)
        } else {
            // TODO :: i'm pretty sure there's no situation in which this would deadlock but i
            // haven't fully thought it through
            *self.waker.lock() = Some(cx.waker().clone());
            std::task::Poll::Pending
        }
    }
}

impl<O, T: FnOnce() -> O> IdleTask<O, T> {
    fn invoke(&mut self) {
        // TODO :: can this deadlock?
        let Some(task) = self.task.lock().take() else {
            tracing::error!(task = %self, "invoked IdleTask after having already been invoked");
            return;
        };
        let res = task();
        // TODO :: can this deadlock?
        *self.result.lock() = Some(res);
        // TODO :: can this deadlock?
        if let Some(waker) = self.waker.lock().take() {
            waker.wake();
        }
    }

    fn new(task: T) -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            task: Arc::new(Mutex::new(Some(task))),
            result: Default::default(),
            waker: Default::default(),
        }
    }

    /// # Safety
    ///
    /// Must only be called from the main thread.
    #[must_use]
    pub unsafe fn defer_local(task: T) -> Self
    where
        O: 'static,
        T: 'static,
    {
        tracing::trace!("deferring local task");
        let returned = Self::new(task);
        let mut task = returned.clone();
        Callable::from_fn(task.to_string(), move |args: &[&Variant]| -> () {
            tracing::trace!("invoking local task");
            #[cfg(debug_assertions)]
            if !args.is_empty() {
                tracing::warn!(%task, params = ?args, "invoked IdleTask with parameters; ignoring");
            }
            task.invoke();
        })
        .call_deferred(&[]);
        returned
    }

    #[must_use]
    pub fn defer(task: T) -> Self
    where
        O: Send + 'static,
        T: Send + 'static,
    {
        let res = Self::new(task);
        Callable::from_custom(res.clone()).call_deferred(&[]);
        res
    }
}
