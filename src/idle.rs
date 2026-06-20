use std::{sync::Arc, task::Waker};

use godot::{
    classes::{
        Engine,
        class_macros::private::virtuals::ZipReader::{Callable, RustCallable, Variant},
    },
    init::is_main_thread,
    obj::Singleton,
};
use parking_lot::Mutex;

#[derive(thiserror::Error, Debug)]
pub enum IdleTaskError<Task> {
    #[error("called defer_local from outside the main thread")]
    NotMainThread(Task),
}

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

    #[must_use]
    pub fn defer_local(task: T) -> Result<Self, IdleTaskError<T>>
    where
        O: 'static,
        T: 'static,
    {
        if !godot::init::is_main_thread() {
            return Err(IdleTaskError::NotMainThread(task));
        }
        tracing::trace!("deferring local task");
        let returned = Self::new(task);
        let mut task = returned.clone();
        Callable::from_fn(task.to_string(), move |_: &[&Variant]| -> () {
            tracing::trace!(%task, "invoking local task");
            task.invoke();
        })
        .call_deferred(&[]);
        Ok(returned)
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
