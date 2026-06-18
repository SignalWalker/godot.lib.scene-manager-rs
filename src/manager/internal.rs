use godot::{
    classes::{Node, node::ProcessMode},
    prelude::Gd,
};

#[derive(Clone)]
pub(super) struct StackData {
    /// The scene.
    pub scene: Gd<Node>,
    /// The scene's process mode as it was before it got paused by a higher scene
    pub initial_process_mode: ProcessMode,
}

impl StackData {
    pub(super) fn new(scene: Gd<Node>) -> Self {
        Self {
            initial_process_mode: scene.get_process_mode(),
            scene,
        }
    }
}

impl From<Gd<Node>> for StackData {
    #[inline]
    fn from(value: Gd<Node>) -> Self {
        Self::new(value)
    }
}
