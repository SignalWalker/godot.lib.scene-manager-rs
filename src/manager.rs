use std::{cell::RefCell, ops::Deref, rc::Rc};

use godot::{classes::Node, obj::NewAlloc, prelude::Gd};

mod transition;

mod internal;
use godot_utils::TypedInstanceId;
use internal::*;

use crate::gd_api::SceneManagerNode;

pub mod gd_api;

#[derive(Debug, thiserror::Error)]
pub enum PushSceneError {
    #[error("scene ({0}) already has a parent node")]
    SceneAlreadyHasParent(Gd<Node>),
}

#[derive(Debug, thiserror::Error)]
pub enum InsertSceneError {
    #[error("index {0} out of bounds")]
    IndexOutOfBounds(usize),
    #[error("scene ({0}) already has a parent node")]
    SceneAlreadyHasParent(Gd<Node>),
}

impl From<PushSceneError> for InsertSceneError {
    fn from(value: PushSceneError) -> Self {
        match value {
            PushSceneError::SceneAlreadyHasParent(gd) => Self::SceneAlreadyHasParent(gd),
        }
    }
}

pub struct SceneManager {
    node_id: TypedInstanceId<gd_api::SceneManagerNode>,

    /// The root node of the scene
    root: RefCell<Gd<Node>>,
    /// The node to which scenes will be added (which may be the same node as `root`)
    scene_parent: RefCell<Gd<Node>>,
    scene_stack: RefCell<Vec<StackData>>,
}

impl std::fmt::Debug for SceneManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SceneManager").finish_non_exhaustive()
    }
}

impl SceneManager {
    pub fn new(
        node: &Gd<SceneManagerNode>,
        root: Gd<Node>,
        scene_parent: Option<Gd<Node>>,
    ) -> Self {
        Self {
            node_id: TypedInstanceId::from_gd(node).unwrap(),
            scene_parent: RefCell::new(scene_parent.unwrap_or_else(|| root.clone())),
            root: RefCell::new(root),
            scene_stack: RefCell::new(Vec::new()),
        }
    }

    pub fn node(&self) -> Gd<SceneManagerNode> {
        self.node_id.get().unwrap()
    }

    /// Get the topmost scene on the stack.
    #[inline]
    pub fn current_scene<'scene>(&'scene self) -> Option<impl Deref<Target = Gd<Node>> + 'scene> {
        std::cell::Ref::filter_map(self.scene_stack.borrow(), |st| {
            st.last().map(StackData::scene)
        })
        .ok()
    }

    /// Get the root of the scene
    #[inline]
    pub fn scene_parent<'parent>(&'parent self) -> impl Deref<Target = Gd<Node>> + 'parent {
        self.scene_parent.borrow()
    }

    #[inline]
    pub fn root<'root>(&'root self) -> impl Deref<Target = Gd<Node>> + 'root {
        self.root.borrow()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.scene_stack.borrow().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.scene_stack.borrow().is_empty()
    }

    pub fn index_of(&self, scene: &Gd<Node>) -> Option<usize> {
        self.scene_stack
            .borrow()
            .iter()
            .position(|scn| scn.scene() == scene)
    }

    /// Push a new scene to the stack.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts where it's safe to modify the scene tree.
    pub unsafe fn push_scene(self: Rc<Self>, new: Gd<Node>) -> Result<(), PushSceneError> {
        if new.get_parent().is_some() {
            return Err(PushSceneError::SceneAlreadyHasParent(new));
        }

        if let Some(old) = self.scene_stack.borrow_mut().last_mut() {
            old.pause();
        }

        // push scene
        let data = self.clone().register_scene(new.clone());
        let index = self.len();
        self.scene_stack.borrow_mut().push(data);
        self.scene_parent.borrow_mut().add_child(&new);
        // emit signal
        // TODO :: use saturating_cast once that's stable
        self.node()
            .signals()
            .scene_pushed()
            .emit(&new, u32::try_from(index).unwrap_or(u32::MAX));

        Ok(())
    }

    #[must_use]
    /// Pop a scene from the stack without removing it from the scene tree.
    unsafe fn pop_scene_without_removing_from_tree(&self) -> Option<Gd<Node>> {
        // NOTE :: you can't just use `if let Some(old) = self.scene_stack.borrow_mut().pop()` here
        // because then the RefMut stays in scope
        let old = self.scene_stack.borrow_mut().pop();
        if let Some(old) = old {
            // unregister the old scene
            let old = self.unregister_scene(old);
            // update the process mode on the next scene (if it exists)...
            if let Some(new) = self.scene_stack.borrow_mut().last_mut() {
                new.unpause();
            }
            Some(old)
        } else {
            None
        }
    }

    /// Pop the current scene.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts where it's safe to modify the scene tree.
    #[must_use]
    pub unsafe fn pop_scene(&self) -> Option<Gd<Node>> {
        // pop the scene...
        if let Some(old) = unsafe { self.pop_scene_without_removing_from_tree() } {
            // remove it from the scene tree...
            self.scene_parent.borrow_mut().remove_child(&old);
            // emit signal
            // TODO :: use saturating_cast once that's stable
            self.node()
                .signals()
                .scene_popped()
                .emit(&old, self.len().try_into().unwrap_or(u32::MAX));
            Some(old)
        } else {
            None
        }
    }

    /// Insert a scene at the given index, shifting all scenes after it to the right.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    pub unsafe fn insert_scene(
        self: Rc<Self>,
        index: usize,
        new: Gd<Node>,
    ) -> Result<(), InsertSceneError> {
        let stack_len = self.scene_stack.borrow().len();
        if index > stack_len {
            return Err(InsertSceneError::IndexOutOfBounds(index));
        }
        if index == stack_len {
            // we're inserting it as the last scene, which is the same as pushing it, so lets defer
            // to `push_scene`
            unsafe {
                return self.push_scene(new).map(|_| ()).map_err(From::from);
            }
        }

        // pause the new scene, to keep it consistent with what would have happened had it been
        // pushed normally
        let mut data = self.clone().register_scene(new.clone());
        data.pause();

        // put it on the stack
        self.scene_stack.borrow_mut().insert(index, data);

        // godot is weirdly obtuse about node indexing
        if index == 0 {
            // TODO :: is this really the best way to do this
            let mut dummy = Node::new_alloc();
            self.scene_parent.borrow_mut().add_child(&dummy);
            self.scene_parent.borrow_mut().move_child(&dummy, 0);
            dummy.replace_by(&new);
            dummy.free();
            Ok(())
        } else {
            let prev_sibling = &mut self.scene_stack.borrow_mut()[index - 1];
            prev_sibling.add_sibling(&new);
            Ok(())
        }
    }

    /// Remove and return the scene at the given index, or `None` if the index is out of bounds.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    #[must_use]
    pub unsafe fn try_remove_scene_at(&self, index: usize) -> Option<Gd<Node>> {
        if index >= self.scene_stack.borrow().len() {
            return None;
        }
        if index == self.scene_stack.borrow().len() - 1 {
            // we're removing the top scene, so we'll defer to `pop_scene`
            unsafe {
                return self.pop_scene();
            }
        }

        // remove the scene from the stack...
        let scene = self.unregister_scene(self.scene_stack.borrow_mut().remove(index));

        // remove it from the tree :>
        self.scene_parent.borrow_mut().remove_child(&scene);

        Some(scene)
    }

    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    #[must_use]
    pub unsafe fn try_remove_scene(&self, scene: &Gd<Node>) -> Option<(Gd<Node>, usize)> {
        let index = self.index_of(scene)?;
        unsafe { self.try_remove_scene_at(index) }.map(|node| (node, index))
    }

    fn scene_exiting_tree(&self, scene: Gd<Node>) {
        let Some(index) = self.index_of(&scene) else {
            tracing::error!(%scene, "received tree_exiting, but the node exiting the tree is not on the stack (the tree_exiting signal should already be disconnected)");
            return;
        };
        if index == self.scene_stack.borrow().len() - 1 {
            // the scene is the current top of the stack, so we'll defer to `pop_scene()`,
            // because that has some extra side effects.
            //
            // we're just dropping it without freeing because it seems not to be under our control anymore
            let _ = unsafe { self.pop_scene_without_removing_from_tree() };
            // emit signal
            // TODO :: use saturating_cast once that's stable
            self.node()
                .signals()
                .scene_popped()
                .emit(&scene, self.len().try_into().unwrap_or(u32::MAX));
        } else {
            // the scene is not the current top of the stack, so we can just remove it.
            let data = self.scene_stack.borrow_mut().remove(index);
            // we're just dropping the scene without freeing because it seems not to be under our control anymore
            let _ = self.unregister_scene(data);
        }
    }
}
