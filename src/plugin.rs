use godot::{
    classes::{EditorPlugin, IEditorPlugin},
    init::is_editor_hint,
    obj::Base,
    register::{GodotClass, godot_api},
};

#[derive(GodotClass)]
#[class(tool, init, base=EditorPlugin)]
pub struct SceneManagerPlugin {
    base: Base<EditorPlugin>,
}

#[godot_api]
impl IEditorPlugin for SceneManagerPlugin {
    fn enter_tree(&mut self) {
        if !is_editor_hint() {
            // we're in the game, not the editor, so we're skipping the rest of this
            return;
        }
        crate::settings::prepare();
    }
}
