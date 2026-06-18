#![allow(deprecated)]

use godot::{
    builtin::{GString, StringName, Variant, VariantType},
    classes::{ProjectSettings, RefCounted},
    meta::ToGodot,
    obj::{Base, Singleton},
    prelude::Dictionary,
    register::{GodotClass, godot_api, info::PropertyHint},
};

struct SettingDef {
    key: &'static str,
    default_value: &'static str,
    ty: VariantType,
    hint: PropertyHint,
    hint_string: &'static str,

    is_advanced: bool,
    is_internal: bool,
}

impl SettingDef {
    fn to_hint(&self, name: &GString) -> Dictionary<GString, Variant> {
        let mut res = Dictionary::new();
        res.set("name", &name.to_variant());
        res.set("value", &GString::from(self.default_value).to_variant());
        res.set("type", &self.ty.to_variant());
        res.set("hint", &self.hint.to_variant());
        res.set("hint_string", &GString::from(self.hint_string).to_variant());
        res
    }
}

const DEFINITIONS: &[SettingDef] = &[SettingDef {
    key: "runtime/root_scene",
    default_value: "",
    ty: VariantType::STRING_NAME,
    hint: PropertyHint::FILE,
    hint_string: "PackedScene",
    is_advanced: false,
    is_internal: false,
}];

/// Initialize settings
pub(crate) fn prepare() {
    let mut settings = ProjectSettings::singleton();
    for def in DEFINITIONS {
        let name = GString::from(&format!("scene_manager/{}", def.key));
        let def_value = StringName::from(def.default_value).to_variant();

        // ensure setting exists
        if !settings.has_setting(&name) {
            settings.set_setting(&name, &def_value);
        }

        // set initial value
        settings.set_initial_value(&name, &def_value);

        // set up hinting
        settings.add_property_info(&def.to_hint(&name));

        // set how visible this is to the user
        settings.set_as_basic(&name, !def.is_advanced);
        settings.set_as_internal(&name, def.is_internal);
    }
}

/// Get the value of a setting, if it exists
pub fn get(path: &str) -> Option<Variant> {
    let full_path = GString::from(&format!("scene_manager/{}", path));
    let settings = ProjectSettings::singleton();
    if settings.has_setting(&full_path) {
        Some(settings.get_setting(&full_path))
    } else {
        None
    }
}

#[deprecated = "use settings::get() instead"]
#[derive(GodotClass)]
#[class(tool, no_init, base=RefCounted)]
pub struct SceneManagerSettings {
    base: Base<RefCounted>,
}

#[godot_api]
impl SceneManagerSettings {
    #[deprecated = "use settings::get() instead"]
    #[func]
    fn get_setting(path: StringName, default: Variant) -> Variant {
        get(&path.to_string()).unwrap_or(default)
    }

    #[deprecated = "use settings::get() instead"]
    #[func]
    fn has_setting(path: StringName) -> bool {
        ProjectSettings::singleton().has_setting(&format!("scene_manager/{}", path))
    }
}
