use futures::{FutureExt, TryFutureExt};
use godot::{
    classes::{AnimationPlayer, Node},
    prelude::{Gd, StringName},
    task::FallibleSignalFutureError,
};

#[derive(Debug, thiserror::Error)]
pub enum AnimationTransitionError {
    #[error("scene transition ({0}) contains no animations")]
    NoAnimations(Gd<AnimationPlayer>),
    #[error(transparent)]
    Signal(#[from] FallibleSignalFutureError),
}

impl super::TransitionDriver for Gd<AnimationPlayer> {
    type Error = AnimationTransitionError;

    fn get_transition_root(&self) -> Gd<Node> {
        self.clone().upcast()
    }

    fn start_transition(
        &mut self,
    ) -> Result<impl Future<Output = Result<(), Self::Error>>, Self::Error> {
        let start_anim = {
            let autoplay = self.get_autoplay().clone();
            if !autoplay.is_empty() {
                // check for autoplay and play that if it exists...
                autoplay
            } else if self.get_animation_list().contains("transition_start") {
                // ...otherwise, play "transition_start"...
                StringName::from("transition_start")
            } else {
                // ...or, failing all that, just play the first animation and output a warning...
                let Some(res) = self
                    .get_animation_list()
                    .get(0)
                    .as_ref()
                    .map(StringName::from)
                else {
                    // there are no animations at all, we can't actually start
                    return Err(Self::Error::NoAnimations(self.clone()));
                };
                tracing::warn!(
                    animation = %res,
                    "starting scene transition with AnimationPlayer with neither an autoplay animation nor a `transition_start` animation; using first animation instead"
                );
                res
            }
        };

        let anim_finished = self.signals().animation_finished().to_fallible_future();
        self.play_ex().name(&start_anim).done();

        let mut self_captured = self.clone();
        Ok(async move {
            {
                let (finished_anim,) = anim_finished.await?;

                #[cfg(debug_assertions)]
                if finished_anim != start_anim {
                    tracing::warn!(
                        started_anim = %start_anim,
                        %finished_anim,
                        "transition started with one animation, but received finish signal for a different animation"
                    );
                }
            }

            if self_captured
                .get_animation_list()
                .contains("transition_ready")
            {
                self_captured.play_ex().name("transition_ready").done();
            }

            Ok(())
        })
    }

    fn finish_transition(mut self) -> impl Future<Output = Result<Gd<Node>, Self::Error>> {
        // if we have an ending animation...
        if self.get_animation_list().contains("transition_end") {
            let future_res = self.clone().upcast();
            // ...play that and return a future waiting for it to finish
            let res = self
                .signals()
                .animation_finished()
                .to_fallible_future()
                .map_ok(move |_| future_res)
                .map_err(From::from)
                .boxed_local();
            self.play_ex().name("transition_end").done();
            res
        } else {
            // otherwise, we're done :>
            std::future::ready(Ok(self.upcast())).boxed_local()
        }
    }
}
