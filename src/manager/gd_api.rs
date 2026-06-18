use std::{ops::Deref, rc::Rc};

use godot::{
    classes::{INode, Node, PackedScene, ResourceLoader},
    obj::{Base, Singleton, WithBaseField},
    prelude::{GString, Gd, StringName},
    register::{GodotClass, godot_api},
};

use crate::settings;

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

#[godot_api]
impl SceneManagerNode {
    /// Get the current scene.
    #[func]
    fn get_current_scene(&self) -> Option<Gd<Node>> {
        self.state.manager().current_scene().map(|sc| sc.clone())
    }

    /// Get the current scene's parent.
    #[func]
    fn get_scene_parent(&self) -> Gd<Node> {
        self.state.scene_parent().clone()
    }

    #[func]
    fn push_scene(&mut self, scene: Gd<Node>) {
        self.run_deferred(move |mng: &mut Self| unsafe {
            mng.state.manager().push_scene(scene);
        });
    }

    #[func]
    fn pop_scene(&mut self) {
        self.run_deferred(move |mng: &mut Self| unsafe {
            if let Some(scene) = mng.state.manager().pop_scene() {
                scene.free();
            }
        });
    }

    #[func]
    fn swap_scene(&mut self, scene: Gd<Node>) {
        self.run_deferred(move |mng: &mut Self| unsafe {
            if let Some(scene) = mng.state.manager().pop_scene() {
                scene.free();
            }
            mng.state.manager().push_scene(scene);
        });
    }

    #[func]
    fn transition_scene(&mut self, transition_node: Gd<Node>, next_scene: Gd<Node>) {
        self.run_deferred(move |mng: &mut Self| {
            let task = match unsafe {
                mng.state
                    .manager()
                    .clone()
                    .transition_scene(transition_node, std::future::ready(next_scene))
            } {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "could not start scene transition");
                    return;
                }
            };
            godot::task::spawn(async move {
                task.await;
            });
        });
    }
}

impl SceneManagerNode {
    fn get_root_scene() -> Option<Gd<Node>> {
        let Some(root_scene_path) = settings::get("runtime/root_scene") else {
            tracing::error!("could not find scene_manager/runtime/root_scene setting");
            return None;
        };
        let path = match root_scene_path.try_to::<StringName>() {
            Ok(p) => p,
            Err(error) => {
                tracing::error!(
                    %error,
                    "could not convert scene_manager/runtime/root_scene to StringName",
                );
                return None;
            }
        };
        if path.is_empty() {
            // TODO :: should we warn about this? this is what happens when you haven't set a root
            // scene, so this is the default state (as long as nothing's broken)
            return None;
        }
        let Some(resource) = ResourceLoader::singleton()
            .load_ex(&GString::from(&path))
            .type_hint("PackedScene")
            .done()
        else {
            tracing::error!(%path, "could not load scene");
            return None;
        };
        let root_scene_packed = match resource.try_cast::<PackedScene>() {
            Ok(r) => r,
            Err(error) => {
                tracing::error!(
                    %path,
                    %error,
                    "could not cast loaded resource to PackedScene",
                );
                return None;
            }
        };
        match root_scene_packed.instantiate() {
            Some(res) => Some(res),
            None => {
                tracing::error!(
                    %path,
                    packed_scene = %root_scene_packed,
                    "could not instantiate PackedScene",
                );
                None
            }
        }
    }
}

#[godot_api]
impl INode for SceneManagerNode {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            state: SceneManagerState::Initializing {
                root_scene: Self::get_root_scene(),
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
                self.base()
                    .get_tree()
                    .get_root()
                    .expect("scene tree should have a root, since we've entered it")
                    .upcast()
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
                manager: Rc::new(super::SceneManager::new(root, scene_parent)),
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
        let mut tree_root = tree
            .get_root()
            .expect("tree should have a root, since we've already entered it");
        // add our current root to the scene (as long as it's not already the root of the scene)
        if self.state.root().instance_id() != tree_root.instance_id() {
            tree_root.add_child(&*self.state.root());
        }

        unsafe {
            self.state.manager().push_scene(tree_current);
        }
    }
}
