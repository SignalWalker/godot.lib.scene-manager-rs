use std::rc::Rc;

use godot::{
    classes::{AnimationPlayer, Node},
    prelude::Gd,
    task::FallibleSignalFutureError,
};

use crate::{
    InsertSceneError,
    idle::{IdleTask, IdleTaskError},
};

mod scene_transition_inner;
use scene_transition_inner::*;

#[derive(thiserror::Error, Debug)]
pub enum SceneTransitionError<NodeError> {
    #[error("transition node ({0}) already has a parent")]
    TransitionAlreadyHasParent(Gd<Node>),
    #[error("scene node ({0}) already has a parent")]
    SceneAlreadyHasParent(Gd<Node>),
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

    fn finish(self) -> impl Future<Output = Result<Gd<Node>, FallibleSignalFutureError>>;

    fn scene(&self) -> Gd<Node>;
}

pub type SceneTransitionResult<Error> = Result<(Gd<Node>, usize), SceneTransitionError<Error>>;

pub type TransitionTargetResult<Error> = Result<Gd<Node>, Error>;

impl super::SceneManager {
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    pub unsafe fn transition_scene<'result, NodeError>(
        self: Rc<Self>,
        transition: Gd<Node>,
        next_scene: impl Future<Output = TransitionTargetResult<NodeError>> + 'result,
    ) -> Result<
        impl Future<Output = SceneTransitionResult<NodeError>> + 'result,
        SceneTransitionError<NodeError>,
    > {
        // construct the transition...
        let trans = match transition.try_cast::<AnimationPlayer>() {
            Ok(anim) => {
                return unsafe {
                    self.transition_scene_inner(SceneTransitionAnimation::new(anim), next_scene)
                };
            }
            Err(trans) => trans,
        };
        // TODO :: support callable-based transition
        Err(SceneTransitionError::UnrecognizedTransitionType(trans))
    }

    unsafe fn transition_scene_inner<'result, Error>(
        self: Rc<Self>,
        mut transition: impl TransitionDriver + 'result,
        next_scene: impl Future<Output = TransitionTargetResult<Error>> + 'result,
    ) -> Result<
        impl Future<Output = SceneTransitionResult<Error>> + 'result,
        SceneTransitionError<Error>,
    > {
        let old_scene = self.current_scene().map(|s| s.clone());

        // put it on the scene stack...
        unsafe { self.clone().push_scene(transition.scene()) }.map_err(|err| match err {
            crate::PushSceneError::SceneAlreadyHasParent(gd) => {
                SceneTransitionError::TransitionAlreadyHasParent(gd)
            }
        })?;

        // wait for it to finish...
        Ok(async move {
            // wait for the transition to be ready...
            if let Err(error) = transition.start().await {
                tracing::error!(%error, "scene transition start");
            }

            // remove the old scene...
            if let Some(old_scene) = old_scene {
                if self
                    .scene_stack
                    .borrow()
                    .iter()
                    .any(|sc| *sc.scene() == old_scene)
                {
                    let manager = self.clone();
                    IdleTask::defer_local(move || {
                        let Some(old_scene) = (unsafe { manager.try_remove_scene(&old_scene) })
                        else {
                            tracing::error!(
                                %old_scene,
                                "old scene removed during scene transition without permission",
                            );
                            return;
                        };
                        old_scene.0.free();
                    })?
                    .await;
                } else {
                    tracing::error!(
                        %old_scene,
                        "old scene removed during scene transition without permission",
                    );
                }
            }

            // swap the scene...
            let next = match next_scene.await {
                Ok(n) => n,
                Err(e) => return Err(SceneTransitionError::Node(e)),
            };
            let manager = self.clone();
            let next_df = next.clone();
            let transition_df = transition.scene();
            let scene_index = match IdleTask::defer_local(move || {
                // insert the new scene below the scene transition
                let index = match manager.index_of(&transition_df) {
                    Some(i) => i.saturating_sub(1),
                    None => {
                        tracing::error!("could not find scene transition node; inserting transition target scene at top of stack");
                        manager.len()
                    },
                };
                unsafe { manager.insert_scene(index, next_df) }.map(|_: ()| index)
            })?
            .await
            {
                Ok(index) => index,
                Err(InsertSceneError::SceneAlreadyHasParent(scene)) => {
                    return Err(SceneTransitionError::SceneAlreadyHasParent(scene));
                }
                Err(InsertSceneError::IndexOutOfBounds(index)) => unreachable!(
                    "the index ({index}) is either one less than the index of a scene already on the stack (the transition scene), or it is the top of the stack, so, either way, it should always be in bounds"
                ),
            };

            // finish the transition...
            let transition_scene = match transition.finish().await {
                Ok(scn) => Some(scn),
                Err(error) => {
                    tracing::error!(%error, "scene transition finish");
                    None
                }
            };

            // remove the transition...
            let manager = self.clone();
            IdleTask::defer_local(move || {
                if let Some(transition) = transition_scene {
                    let Some(transition) = (unsafe { manager.try_remove_scene(&transition) }) else {
                        tracing::error!(
                            %transition,
                            "tried to remove scene transition, but it wasn't in the stack (did it remove itself?)"
                        );
                        return;
                    };
                    transition.0.free();
                }
            })?
            .await;

            // emit the push signal, if our new scene is the top scene
            if self
                .scene_stack
                .borrow()
                .last()
                .is_some_and(|last| *last.scene() == next)
            {
                // TODO :: saturating cast
                self.node()
                    .signals()
                    .scene_pushed()
                    .emit(&next, self.len().try_into().unwrap_or(u32::MAX));
            }

            // we're done yay
            Ok((next, scene_index))
        })
    }
}
