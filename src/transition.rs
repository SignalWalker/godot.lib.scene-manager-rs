use godot::classes::{Node, class_macros::private::virtuals::Xrvrs::Gd};

mod animation_player;

/// Something that can be used to run a transition.
pub trait TransitionDriver {
    type Error;

    /// Start the transition, and return a future that returns once the transition has finished starting.
    ///
    /// For scene transitions, the future should return only once underlying scenes are fully hidden.
    fn start_transition(
        &mut self,
    ) -> Result<impl Future<Output = Result<(), Self::Error>>, Self::Error>;

    /// Start finishing the transition, and return a future that yields the transition node
    /// once the transition has completely finished.
    ///
    /// For scene transitions, the future should return only when the transition should be removed
    /// from the scene tree.
    fn finish_transition(self) -> impl Future<Output = Result<Gd<Node>, Self::Error>>;

    /// The root node of this transition.
    fn get_transition_root(&self) -> Gd<Node>;
}
