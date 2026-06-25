use std::{ops::Deref, rc::Rc};

use godot::{
    classes::{
        INode, Node, class_macros::private::virtuals::ZipReader::Variant,
        resource_loader::CacheMode,
    },
    obj::{Base, WithBaseField, WithUserSignals},
    prelude::{GString, Gd, StringName},
    register::{GodotClass, godot_api},
};

use crate::{resource::LoadNodeFromPathError, settings};

enum SceneManagerState {
    Initializing {
        /// The root node of the scene containing the root node of the effective scene tree
        root_scene: Option<Gd<Node>>,
    },
    Ready {
        manager: Rc<super::SceneManager>,
    },
}

impl SceneManagerState {
    fn manager(&self) -> &Rc<super::SceneManager> {
        match self {
            Self::Ready { manager, .. } => manager,
            _ => panic!("called manager() on SceneManagerState during initialization"),
        }
    }

    fn root(&self) -> impl Deref<Target = Gd<Node>> {
        match self {
            Self::Ready { manager, .. } => manager.root.borrow(),
            _ => panic!("called root() on SceneManagerState during initialization"),
        }
    }

    fn scene_parent(&self) -> impl Deref<Target = Gd<Node>> {
        match self {
            Self::Ready { manager, .. } => manager.scene_parent.borrow(),
            _ => panic!("called scene_parent() on SceneManagerState during initialization"),
        }
    }
}

/// A node that manages scene state.
///
/// This is designed to be used as an autoload.
#[derive(GodotClass)]
#[class(base=Node)]
pub struct SceneManagerNode {
    base: Base<Node>,
    state: SceneManagerState,
}

impl SceneManagerNode {
    pub fn manager(&self) -> &Rc<super::SceneManager> {
        self.state.manager()
    }
}

#[godot_api]
impl SceneManagerNode {
    /// Emitted when a new scene is pushed, after it is added to the scene tree.
    ///
    /// The second parameter is the index of the pushed scene within the scene stack.
    ///
    /// This signal is only emitted when a scene is pushed to the **top** of the stack. As in, it will
    /// **not** be emitted if a scene is inserted into the stack below the top.
    #[signal]
    pub fn scene_pushed(scene: Gd<Node>, index: u32);

    /// Emitted when a scene is popped, after it is removed from the scene tree.
    ///
    /// The second parameter is the former index of the popped scene within the scene stack.
    ///
    /// This signal is only emitted when a scene is popped from the **top** of the stack.
    /// (ex. if the scene stack is `[A, B]` and `A` is removed, this signal will **not** be emitted.
    /// However, if the scene stack is `[D, C]` and `C` is removed, this signal **will** be
    /// emitted.)
    #[signal]
    pub fn scene_popped(scene: Gd<Node>, index: u32);

    /// Get the current scene.
    #[func]
    pub fn get_current_scene(&self) -> Option<Gd<Node>> {
        self.state.manager().current_scene().map(|sc| sc.clone())
    }

    /// Get the current scene's parent.
    #[func]
    pub fn get_scene_parent(&self) -> Gd<Node> {
        self.state.scene_parent().clone()
    }

    /// Push a new scene to the top of the stack. Must only be called during idle time.
    #[func]
    pub fn push_scene_immediate(&mut self, scene: Gd<Node>) -> bool {
        if let Err(error) = unsafe { self.state.manager().clone().push_scene(scene.clone()) } {
            tracing::error!(%error, "could not push scene");
            return false;
        };

        true
    }

    /// Push a new scene to the top of the stack during idle time.
    #[func]
    pub fn push_scene(&mut self, scene: Gd<Node>) {
        self.run_deferred(move |mng: &mut Self| {
            mng.push_scene_immediate(scene);
        });
    }

    /// Remove a scene from the stack. Must only be called during idle time.
    #[func]
    #[must_use]
    pub fn remove_scene_immediate(&mut self, scene: Gd<Node>) -> Option<Gd<Node>> {
        unsafe { self.state.manager().clone().try_remove_scene(&scene) }.map(|(scene, _)| scene)
    }

    /// Remove a scene from the stack and free it, during idle time.
    #[func]
    pub fn remove_scene(&mut self, scene: Gd<Node>) {
        self.run_deferred(move |mng: &mut Self| {
            let Some(scene) = mng.remove_scene_immediate(scene.clone()) else {
                tracing::error!(%scene, "tried to remove scene, but it isn't on the scene stack");
                return;
            };
            scene.free();
        });
    }

    /// Pop a scene from the top of the stack. Must only be called during idle time.
    #[func]
    #[must_use]
    pub fn pop_scene_immediate(&mut self) -> Option<Gd<Node>> {
        unsafe { self.state.manager().pop_scene() }
    }

    /// Pop a scene from the top of the stack and free it, during idle time.
    #[func]
    pub fn pop_scene(&mut self) {
        self.run_deferred(move |mng: &mut Self| {
            if let Some(scene) = unsafe { mng.state.manager().pop_scene() } {
                scene.free();
            }
        });
    }

    /// Replace the current scene with a new one and free the old one.
    #[func]
    #[must_use]
    pub fn swap_scene_immediate(&mut self, scene: Gd<Node>) -> Option<Gd<Node>> {
        let old = self.pop_scene_immediate();
        self.push_scene_immediate(scene);
        old
    }

    /// Replace the current scene with a new one and free the old one, during idle time.
    #[func]
    pub fn swap_scene(&mut self, scene: Gd<Node>) {
        self.run_deferred(move |mng: &mut Self| {
            if let Some(old) = mng.swap_scene_immediate(scene) {
                old.free();
            }
        });
    }

    /// Start a scene transition with the given transition scene and a target scene.
    ///
    /// Currently, `transition_node` can be an AnimationPlayer with three animations:
    /// - `transition_start`, which plays as soon as it enters
    /// - `transition_ready`, which is optional and plays after `transition_start`
    /// - `transition_end`, which is optional and plays after `transition_start` or `transition_ready`
    ///
    /// `next_scene` can be any of:
    /// - A path (String or StringName) to a PackedScene resource, which will be loaded in a
    /// separate thread during the transition
    /// - A PackedScene, which will be instantiated
    /// - A Node
    #[func]
    pub fn transition_scene(&mut self, transition_node: Gd<Node>, next_scene: Variant) {
        let node = match crate::resource::load_threaded_something_to_node(
            next_scene,
            CacheMode::REUSE,
            false,
        ) {
            Ok(n) => n,
            Err(error) => {
                tracing::error!(%error, "could not start loading next scene for scene transition");
                return;
            }
        };
        self.run_deferred(move |mng: &mut Self| {
            let task = match unsafe {
                mng.state
                    .manager()
                    .clone()
                    .transition_scene(transition_node, node)
            } {
                Ok(t) => t,
                Err(error) => {
                    tracing::error!(%error, "could not start scene transition");
                    return;
                }
            };

            godot::task::spawn(async move {
                if let Err(error) = task.await {
                    tracing::error!(%error, "could not complete scene transition");
                }
            });
        });
    }

    /// Given an argument, load it to a Node.
    ///
    /// The argument can be any of:
    /// - A path (String or StringName) to a PackedScene resource, which will be loaded and instantiated
    /// - A PackedScene, which will be instantiated
    /// - A Node, which will be returned unchanged
    #[func]
    fn load_to_node(arg: Variant) -> Option<Gd<Node>> {
        crate::resource::load_something_to_node(arg, CacheMode::REUSE).ok()
    }
}

#[derive(Debug, thiserror::Error)]
enum GetRootSceneError {
    #[error("could not find scene_manager/runtime/root_scene setting")]
    SettingMissing,
    #[error("could not convert scene_manager/runtime/root_scene to StringName: {0}")]
    SettingIsNotStringName(#[from] godot::meta::error::ConvertError),
    #[error("scene_manager/runtime/root_scene is not set")]
    SettingIsUnset,
    #[error(transparent)]
    Load(#[from] LoadNodeFromPathError),
}

impl SceneManagerNode {
    fn get_root_scene() -> Result<Gd<Node>, GetRootSceneError> {
        let Some(root_scene_path) = settings::get("runtime/root_scene") else {
            return Err(GetRootSceneError::SettingMissing);
        };
        let path = root_scene_path.try_to::<StringName>()?;
        if path.is_empty() {
            // TODO :: should we warn about this? this is what happens when you haven't set a root
            // scene, so this is the default state (as long as nothing's broken)
            return Err(GetRootSceneError::SettingIsUnset);
        }
        crate::resource::load_node_from_path(&GString::from(&path), CacheMode::REUSE)
            .map_err(From::from)
    }
}

#[godot_api]
impl INode for SceneManagerNode {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            state: SceneManagerState::Initializing {
                root_scene: match Self::get_root_scene() {
                    Ok(node) => Some(node),
                    Err(GetRootSceneError::SettingIsUnset) => None,
                    Err(error) => {
                        tracing::error!(%error, "could not load root node; defaulting to scene tree root");
                        None
                    }
                },
            },
        }
    }

    fn enter_tree(&mut self) {
        const GET_PARENT_FN: &str = "get_scene_manager_parent";
        // finish initializing state...
        if let SceneManagerState::Initializing { root_scene } = &mut self.state {
            // either we loaded a scene in init(), or...
            let mut root = root_scene.take().unwrap_or_else(|| {
                // ...we didn't get a root scene during init, so we'll use the current tree root
                self.base().get_tree_or_null().expect("we've just entered the tree, so we should be able to get a reference to it").get_root().upcast()
            });
            // either get the scene parent from `get_scene_manager_parent()` or just use the root itself
            let scene_parent = if root.has_method(GET_PARENT_FN) {
                // root has `get_scene_manager_root`, so we'll use the result from that
                let res = root.call(GET_PARENT_FN, &[]);
                if res.is_nil() {
                    tracing::warn!(
                        %root,
                        "{GET_PARENT_FN}() returned null; using root as scene parent"
                    );
                    None
                } else {
                    match res.try_to::<Gd<Node>>() {
                        Ok(n) => Some(n),
                        Err(error) => {
                            tracing::error!(%error, %root, "could not convert value returned from {GET_PARENT_FN}() to Node; using root as scene parent");
                            None
                        }
                    }
                }
            } else {
                None
            };
            self.state = SceneManagerState::Ready {
                manager: Rc::new(super::SceneManager::new(&self.to_gd(), root, scene_parent)),
            }
        }
    }

    fn ready(&mut self) {
        self.run_deferred(Self::deferred_ready);
    }
}

impl SceneManagerNode {
    /// Deferred for after `ready()`
    ///
    /// # Safety
    ///
    /// Must only be called during idle time.
    fn deferred_ready(&mut self) {
        // replace the tree's current root with ours and make the tree's current scene our current scene
        let tree = self.base().get_tree();
        let tree_current = tree.get_current_scene().expect("tree should have a current scene, since we've already entered it and this should only be called once after ready()");
        tree_current
            .get_parent()
            .expect("the tree's current scene should have a parent, since we've only just started the program and we should be the only ones changing that")
            .remove_child(&tree_current);
        let mut tree_root = tree.get_root();
        // add our current root to the scene (as long as it's not already the root of the scene)
        if self.state.root().instance_id() != tree_root.instance_id() {
            tree_root.add_child(&*self.state.root());
        }

        if let Err(error) = unsafe {
            self.state
                .manager()
                .clone()
                .push_scene(tree_current.clone())
        } {
            tracing::error!(%error, "could not push initial scene");
        }
    }
}
