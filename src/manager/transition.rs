use std::rc::Rc;

use godot::{
    classes::{AnimationPlayer, Node},
    prelude::Gd,
    task::FallibleSignalFutureError,
};

use crate::idle::{IdleTask, IdleTaskError};

mod scene_transition_inner;
use scene_transition_inner::*;

#[derive(thiserror::Error, Debug)]
pub enum SceneTransitionError<NodeError> {
    #[error("unrecognized scene transition type: {}", 0)]
    UnrecognizedTransitionType(Gd<Node>),
    #[error("scene transition processed outside the main thread")]
    NotMainThread,
    #[error(transparent)]
    Node(NodeError),
}

impl<T, N> From<IdleTaskError<T>> for SceneTransitionError<N> {
    fn from(value: IdleTaskError<T>) -> Self {
        match value {
            IdleTaskError::NotMainThread(_) => Self::NotMainThread,
        }
    }
}

trait TransitionDriver {
    fn start<'future>(
        &'future mut self,
    ) -> impl Future<Output = Result<(), FallibleSignalFutureError>> + 'future;

    fn finish(self) -> impl Future<Output = Result<(), FallibleSignalFutureError>>;
}

struct SceneTransition {
    inner: SceneTransitionInner,
}

impl SceneTransition {
    fn new<N>(transition: Gd<Node>) -> Result<Self, SceneTransitionError<N>> {
        if let Ok(anim) = transition.clone().try_cast::<AnimationPlayer>() {
            Ok(Self {
                inner: SceneTransitionInner::Animation(SceneTransitionAnimation::new(anim)),
            })
        } else {
            Err(SceneTransitionError::UnrecognizedTransitionType(transition))
        }
    }

    fn start(&mut self) -> impl Future<Output = Result<(), FallibleSignalFutureError>> {
        match &mut self.inner {
            SceneTransitionInner::Animation(anim) => anim.start(),
        }
    }

    /// Returns a [Future] that finishes when the scene transition is finished and ready to be
    /// popped from the scene stack.
    fn finish(self) -> impl Future<Output = Result<(), FallibleSignalFutureError>> {
        match self.inner {
            SceneTransitionInner::Animation(anim) => anim.finish(),
        }
    }
}

impl super::SceneManager {
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    pub unsafe fn transition_scene<'result, NodeError>(
        self: Rc<Self>,
        transition_node: Gd<Node>,
        next_scene: impl Future<Output = Result<Gd<Node>, NodeError>> + 'result,
    ) -> Result<
        impl Future<Output = Result<Gd<Node>, SceneTransitionError<NodeError>>> + 'result,
        SceneTransitionError<NodeError>,
    > {
        tracing::debug!(
            %transition_node,
            "transition_scene"
        );
        // construct the transition...
        let mut transition = SceneTransition::new(transition_node.clone())?;

        let old_scene = self.current_scene().map(|s| s.clone());

        // put it on the scene stack...
        unsafe {
            self.push_scene(transition_node);
        }

        // wait for it to finish...
        Ok(async move {
            tracing::trace!("waiting on transition.start()"); // me irl
            // wait for the transition to be ready...
            if let Err(error) = transition.start().await {
                tracing::error!(%error, "scene transition start");
            }

            tracing::trace!("removing old scene");
            // remove the old scene...
            if let Some(old_scene) = old_scene {
                if self
                    .scene_stack
                    .borrow()
                    .iter()
                    .any(|sc| sc.scene == old_scene)
                {
                    let manager = self.clone();
                    tracing::trace!("waiting on IdleTask for old scene to be removed and freed");
                    IdleTask::defer_local(move || {
                        let Some(old_scene) = (unsafe { manager.try_remove_scene(&old_scene) })
                        else {
                            tracing::error!(
                                %old_scene,
                                "old scene removed during scene transition without permission",
                            );
                            return;
                        };
                        old_scene.free();
                    })?
                    .await;
                } else {
                    tracing::error!(
                        %old_scene,
                        "old scene removed during scene transition without permission",
                    );
                }
            }

            tracing::trace!("waiting on new scene to be inserted");
            // swap the scene...
            let next = match next_scene.await {
                Ok(n) => n,
                Err(e) => return Err(SceneTransitionError::Node(e)),
            };
            let manager = self.clone();
            let next_df = next.clone();
            IdleTask::defer_local(move || {
                // NOTE :: have to do this on its own line so the borrow gets dropped
                let index = manager.len().saturating_sub(1);
                unsafe {
                    manager.insert_scene(index, next_df);
                }
            })?
            .await;

            tracing::trace!("waiting on transition.finish()"); // me irl...
            // finish the transition...
            if let Err(error) = transition.finish().await {
                tracing::error!(%error, "scene transition finish");
            };

            tracing::trace!("removing scene transition");
            // remove the transition...
            let manager = self.clone();
            IdleTask::defer_local(move || {
                let Some(transition) = (unsafe { manager.pop_scene() }) else {
                    tracing::error!(
                        "tried to pop scene transition, but there are no scenes on the stack"
                    );
                    return;
                };
                transition.free();
            })?
            .await;

            // we're done yay
            Ok(next)
        })
    }
}
