use std::rc::Rc;

use godot::{classes::Node, prelude::Gd};

use crate::{
    InsertSceneError,
    idle::{IdleTask, IdleTaskError},
    transition::TransitionDriver,
};

#[derive(thiserror::Error, Debug)]
pub enum SceneTransitionStartError<TargetSceneError> {
    #[error("transition node ({0}) already has a parent")]
    TransitionAlreadyHasParent(Gd<Node>),
    #[error("scene transition processed outside the main thread")]
    NotMainThread,

    #[error("could not load target scene: {0}")]
    TargetScene(#[source] TargetSceneError),
}

impl<T, E> From<IdleTaskError<T>> for SceneTransitionStartError<E> {
    fn from(value: IdleTaskError<T>) -> Self {
        match value {
            IdleTaskError::NotMainThread(_) => Self::NotMainThread,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SceneTransitionFinishError {
    #[error("scene transition processed outside main thread")]
    NotMainThread,
    #[error("scene node ({0}) already has a parent")]
    SceneAlreadyHasParent(Gd<Node>),
}

impl<T> From<IdleTaskError<T>> for SceneTransitionFinishError {
    fn from(value: IdleTaskError<T>) -> Self {
        match value {
            IdleTaskError::NotMainThread(_) => Self::NotMainThread,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SceneTransitionError<TargetSceneError> {
    #[error(transparent)]
    Start(#[from] SceneTransitionStartError<TargetSceneError>),
    #[error(transparent)]
    Finish(#[from] SceneTransitionFinishError),
}

pub type SceneTransitionResult<Driver, Error> =
    Result<OngoingSceneTransition<Driver>, SceneTransitionStartError<Error>>;

pub type TransitionTargetResult<Error> = Result<Gd<Node>, Error>;

impl super::SceneManager {
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    pub unsafe fn transition_scene<'result, Driver, Error>(
        self: Rc<Self>,
        driver: Driver,
        next_scene: impl Future<Output = TransitionTargetResult<Error>> + 'result,
    ) -> Result<
        impl Future<Output = Result<Gd<Node>, SceneTransitionError<Error>>> + 'result,
        SceneTransitionStartError<Error>,
    >
    where
        Driver: TransitionDriver + 'result,
        Driver::Error: std::fmt::Display,
    {
        let transition = unsafe { self.start_scene_transition(driver, next_scene) }?;
        Ok(async move { transition.await?.finish().await.map_err(From::from) })
    }

    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    pub unsafe fn start_scene_transition<'result, Driver, Error>(
        self: Rc<Self>,
        mut driver: Driver,
        next_scene: impl Future<Output = TransitionTargetResult<Error>> + 'result,
    ) -> Result<
        impl Future<Output = SceneTransitionResult<Driver, Error>> + 'result,
        SceneTransitionStartError<Error>,
    >
    where
        Driver: TransitionDriver + 'result,
        Driver::Error: std::fmt::Display,
    {
        let old_scene = self.current_scene().map(|s| s.clone());

        // put it on the scene stack...
        unsafe { self.clone().push_scene(driver.get_transition_root()) }.map_err(
            |err| match err {
                crate::PushSceneError::SceneAlreadyHasParent(gd) => {
                    SceneTransitionStartError::TransitionAlreadyHasParent(gd)
                }
            },
        )?;

        // wait for it to finish...
        Ok(async move {
            // wait for the transition to be ready...
            match driver.start_transition() {
                Err(error) => {
                    tracing::error!(%error, "could not start scene transition driver");
                }
                Ok(start) => {
                    if let Err(error) = start.await {
                        tracing::error!(%error, "scene transition driver failed during start phase");
                    }
                }
            };

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

            // wait for the new scene...
            let next = match next_scene.await {
                Ok(n) => n,
                Err(e) => return Err(SceneTransitionStartError::TargetScene(e)),
            };

            // yield the transition future
            Ok(OngoingSceneTransition::new(self.clone(), driver, next))
        })
    }
}

#[must_use]
pub struct OngoingSceneTransition<Driver: TransitionDriver> {
    manager: Rc<super::SceneManager>,
    driver: Driver,
    target_node: Gd<Node>,
}

impl<Driver: TransitionDriver> OngoingSceneTransition<Driver> {
    const fn new(manager: Rc<super::SceneManager>, driver: Driver, target_node: Gd<Node>) -> Self {
        Self {
            manager,
            driver,
            target_node,
        }
    }

    pub const fn manager(&self) -> &Rc<super::SceneManager> {
        &self.manager
    }

    pub const fn driver(&self) -> &Driver {
        &self.driver
    }

    pub const fn target_node(&self) -> &Gd<Node> {
        &self.target_node
    }

    pub async fn finish(self) -> Result<Gd<Node>, SceneTransitionFinishError>
    where
        Driver::Error: std::fmt::Display,
    {
        let manager = self.manager.clone();
        let next_df = self.target_node.clone();
        let transition_df = self.driver.get_transition_root();
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
                    return Err(SceneTransitionFinishError::SceneAlreadyHasParent(scene));
                }
                Err(InsertSceneError::IndexOutOfBounds(index)) => unreachable!(
                    "the index ({index}) is either one less than the index of a scene already on the stack (the transition scene), or it is the top of the stack, so, either way, it should always be in bounds"
                ),
            };

        // emit scene transition signal...
        self.manager
            .node()
            .signals()
            .scene_transitioning()
            // TODO :: saturating cast
            .emit(
                &self.target_node,
                scene_index.try_into().unwrap_or(u32::MAX),
            );

        // finish the transition...
        let transition_scene = match self.driver.finish_transition().await {
            Err(error) => {
                tracing::error!(%error, "scene transition driver failed during finish phase");
                None
            }
            Ok(scn) => Some(scn),
        };

        // remove the transition...
        let manager = self.manager.clone();
        IdleTask::defer_local(move || {
                if let Some(transition) = transition_scene {
                    let Some(transition) = (unsafe { manager.try_remove_scene(&transition) }) else {
                        tracing::error!(
                            %transition,
                            "tried to remove scene transition, but it wasn't in the stack (was it removed before the finish phase?)"
                        );
                        return;
                    };
                    transition.0.free();
                }
            })?
            .await;

        // emit the push signal, if our new scene is the top scene
        if self
            .manager
            .scene_stack
            .borrow()
            .last()
            .is_some_and(|last| *last.scene() == self.target_node)
        {
            self.manager.node().signals().scene_pushed().emit(
                &self.target_node,
                // TODO :: saturating cast
                self.manager.len().try_into().unwrap_or(u32::MAX),
            );
        }

        // we're done yay
        Ok(self.target_node)
    }
}
