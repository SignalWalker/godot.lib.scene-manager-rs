use std::rc::Rc;

use godot::{
    classes::{Node, node::ProcessMode},
    meta::AsArg,
    prelude::Gd,
    signal::ConnectHandle,
};

pub(super) struct StackData {
    /// The scene.
    scene: Gd<Node>,
    /// The scene's process mode as it was before it got paused by a higher scene
    initial_process_mode: ProcessMode,
    /// The connect handle for this scene's tree_exit handler
    tree_exit_handle: Option<ConnectHandle>,

    #[cfg(debug_assertions)]
    /// Whether this StackData has been unregistered
    registered: bool,
}

impl StackData {
    fn new(scene: Gd<Node>, tree_exit_handle: ConnectHandle) -> Self {
        Self {
            initial_process_mode: scene.get_process_mode(),
            scene,
            tree_exit_handle: Some(tree_exit_handle),
            #[cfg(debug_assertions)]
            registered: true,
        }
    }

    #[inline]
    pub const fn scene(&self) -> &Gd<Node> {
        &self.scene
    }

    pub(super) fn add_sibling(&mut self, sibling: impl AsArg<Gd<Node>>) {
        self.scene.add_sibling(sibling)
    }

    pub fn pause(&mut self) {
        self.initial_process_mode = self.scene.get_process_mode();
        self.scene.set_process_mode(ProcessMode::DISABLED);
    }

    pub fn unpause(&mut self) {
        self.scene.set_process_mode(self.initial_process_mode);
    }
}

impl Drop for StackData {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        if self.registered {
            tracing::error!(scene = %self.scene, "StackData dropped without being unregistered");
        }
        if let Some(handle) = self.tree_exit_handle.take()
            && handle.is_connected()
        {
            handle.disconnect();
        }
    }
}

impl super::SceneManager {
    #[must_use]
    pub(super) fn register_scene(self: Rc<Self>, scene: Gd<Node>) -> StackData {
        let handle_scene = scene.clone();
        let tree_exit_handle = scene.signals().tree_exiting().connect(move || {
            self.scene_exiting_tree(handle_scene.clone());
        });
        StackData::new(scene, tree_exit_handle)
    }

    #[must_use]
    pub(super) fn unregister_scene(&self, mut data: StackData) -> Gd<Node> {
        #[cfg(debug_assertions)]
        {
            data.registered = false;
        }

        data.unpause();
        data.scene.clone()
    }
}
