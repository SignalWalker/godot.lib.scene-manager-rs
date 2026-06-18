use std::{cell::RefCell, ops::Deref};

use godot::{
    classes::{Node, node::ProcessMode},
    obj::NewAlloc,
    prelude::Gd,
};

mod transition;

mod internal;
use internal::*;

pub mod gd_api;

pub struct SceneManager {
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
    pub fn new(root: Gd<Node>, scene_parent: Option<Gd<Node>>) -> Self {
        Self {
            scene_parent: RefCell::new(scene_parent.unwrap_or_else(|| root.clone())),
            root: RefCell::new(root),
            scene_stack: RefCell::new(Vec::new()),
        }
    }

    /// Get the topmost scene on the stack.
    #[inline]
    pub fn current_scene<'scene>(&'scene self) -> Option<impl Deref<Target = Gd<Node>> + 'scene> {
        std::cell::Ref::filter_map(self.scene_stack.borrow(), |st| st.last().map(|s| &s.scene)).ok()
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

    /// Push a new scene to the stack.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts where it's safe to modify the scene tree.
    #[tracing::instrument]
    pub unsafe fn push_scene(&self, new: Gd<Node>) {
        tracing::info!(scene = %new, "push_scene");
        if let Some(mut parent) = new.get_parent() {
            tracing::error!(
                %parent,
                "pushing scene that already has a parent; reparenting...",
            );
            parent.remove_child(&new);
        }

        if let Some(old) = self.scene_stack.borrow_mut().last_mut() {
            // store the old scene's process mode and pause it
            old.initial_process_mode = old.scene.get_process_mode();
            old.scene.set_process_mode(ProcessMode::DISABLED);
        }

        self.scene_stack
            .borrow_mut()
            .push(StackData::from(new.clone()));

        self.scene_parent.borrow_mut().add_child(&new);
    }

    /// Pop the current scene.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts where it's safe to modify the scene tree.
    #[must_use]
    #[tracing::instrument]
    pub unsafe fn pop_scene(&self) -> Option<Gd<Node>> {
        tracing::info!("pop_scene");
        // NOTE :: you can't just use `if let Some(old) = self.scene_stack.borrow_mut().pop()` here
        // because then the RefMut stays in scope
        let old = self.scene_stack.borrow_mut().pop();
        if let Some(old) = old {
            // remove it from the scene tree...
            self.scene_parent.borrow_mut().remove_child(&old.scene);
            // update the process mode on the next scene (if it exists)...
            if let Some(new) = self.scene_stack.borrow_mut().last_mut() {
                new.scene.set_process_mode(new.initial_process_mode);
            }
            Some(old.scene)
        } else {
            None
        }
    }

    /// Insert a scene at the given index, shifting all scenes after it to the right.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    #[tracing::instrument]
    pub unsafe fn insert_scene(&self, index: usize, new: Gd<Node>) {
        tracing::info!(index, scene = %new, "insert_scene");
        debug_assert!(index <= self.scene_stack.borrow().len());

        if index == self.scene_stack.borrow().len() {
            // we're inserting it as the last scene, which is the same as pushing it, so lets defer
            // to `push_scene` (since we also want this to emit signals, etc. when we're putting
            // something on the top of the stack)
            unsafe {
                return self.push_scene(new);
            }
        }

        // pause the new scene, to keep it consistent with what would have happened had it been
        // pushed normally
        let mut data = StackData::new(new.clone());
        data.scene.set_process_mode(ProcessMode::DISABLED);

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
        } else {
            let prev_sibling = &mut self.scene_stack.borrow_mut()[index - 1];
            prev_sibling.scene.add_sibling(&new);
        }
    }

    /// Remove and return the scene at the given index, or `None` if the index is out of bounds.
    ///
    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    #[must_use]
    #[tracing::instrument]
    pub unsafe fn try_remove_scene_at(&self, index: usize) -> Option<Gd<Node>> {
        tracing::info!(index, "try_remove_scene_at");
        if index >= self.scene_stack.borrow().len() {
            tracing::error!(
                index,
                len = self.scene_stack.borrow().len(),
                "scene index out of bounds"
            );
            return None;
        } else if index == self.scene_stack.borrow().len() - 1 {
            // we're removing the top scene, so we'll defer to `pop_scene`
            unsafe {
                return self.pop_scene();
            }
        }

        // remove the scene...
        let mut data = self.scene_stack.borrow_mut().remove(index);
        // restore its old process mode (just in case we want that for some reason?)...
        data.scene.set_process_mode(data.initial_process_mode);
        // remove it from the tree :>
        self.scene_parent.borrow_mut().remove_child(&data.scene);

        Some(data.scene)
    }

    /// # Safety
    ///
    /// Must only be called in contexts in which it's safe to mutate the scene tree.
    #[must_use]
    #[tracing::instrument]
    pub unsafe fn try_remove_scene(&self, scene: &Gd<Node>) -> Option<Gd<Node>> {
        tracing::info!(?scene, "try_remove_scene");
        let Some(index) = self
            .scene_stack
            .borrow()
            .iter()
            .position(|sc| sc.scene == *scene)
        else {
            tracing::error!(%scene, "tried to remove scene from the scene stack, but it's not there");
            return None;
        };
        unsafe { self.try_remove_scene_at(index) }
    }
}
