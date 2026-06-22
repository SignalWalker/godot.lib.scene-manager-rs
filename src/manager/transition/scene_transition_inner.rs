use futures::{FutureExt, TryFutureExt};
use godot::{
    classes::{AnimationPlayer, Node},
    prelude::{Gd, StringName},
};

pub(super) struct SceneTransitionAnimation {
    transition: Gd<AnimationPlayer>,
}

impl SceneTransitionAnimation {
    pub(super) const fn new(transition: Gd<AnimationPlayer>) -> Self {
        Self { transition }
    }
}

impl super::TransitionDriver for SceneTransitionAnimation {
    fn scene(&self) -> Gd<Node> {
        self.transition.clone().upcast()
    }

    fn start<'future>(
        &'future mut self,
    ) -> impl futures::Future<
        Output = std::result::Result<(), godot::task::FallibleSignalFutureError>,
    > + 'future {
        let start_anim = {
            let autoplay = self.transition.get_autoplay().clone();
            if !autoplay.is_empty() {
                // check for autoplay and play that if it exists...
                autoplay
            } else if self
                .transition
                .get_animation_list()
                .contains("transition_start")
            {
                // ...otherwise, play "transition_start"...
                StringName::from("transition_start")
            } else {
                // ...or, failing all that, just play the first animation and output a warning...
                let Some(res) = self
                    .transition
                    .get_animation_list()
                    .get(0)
                    .as_ref()
                    .map(StringName::from)
                else {
                    tracing::error!(transition = %self.transition, "scene transition does not contain any animations");
                    return std::future::ready(Ok(())).boxed_local();
                };
                tracing::warn!(
                    animation = %res,
                    "starting scene transition with AnimationPlayer with neither an autoplay animation nor a `transition_start` animation; using first animation instead"
                );
                res
            }
        };

        let anim_finished = self
            .transition
            .signals()
            .animation_finished()
            .to_fallible_future();
        self.transition.play_ex().name(&start_anim).done();

        async move {
            {
                let (finished_anim,) = anim_finished.await?;
                if finished_anim != start_anim {
                    tracing::error!(
                        "scene transition started animation {}, but the next finished animation was {}",
                        start_anim,
                        finished_anim
                    );
                }
            }

            if self
                .transition
                .get_animation_list()
                .contains("transition_ready")
            {
                self.transition.play_ex().name("transition_ready").done();
            }

            Ok(())
        }.boxed_local()
    }

    fn finish(
        mut self,
    ) -> impl futures::Future<
        Output = std::result::Result<
            godot::prelude::Gd<godot::prelude::Node>,
            godot::task::FallibleSignalFutureError,
        >,
    > {
        // if we have an ending animation...
        if self
            .transition
            .get_animation_list()
            .contains("transition_end")
        {
            let future_res = self.transition.clone().upcast();
            // ...play that and return a future waiting for it to finish
            let res = self
                .transition
                .signals()
                .animation_finished()
                .to_fallible_future()
                .map_ok(move |_| future_res)
                .boxed_local();
            self.transition.play_ex().name("transition_end").done();
            res
        } else {
            // otherwise, we're done :>
            std::future::ready(Ok(self.transition.upcast())).boxed_local()
        }
    }
}
