//! What the player has chosen, kept between runs, and where the game keeps its files.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How the world is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Look {
    /// Big-pixel textures, flat shading by face, blocky clouds: the long-standing look of block games.
    #[default]
    Classic,
    /// Fine textures, sun shadows, bloom, and ray tracing where the graphics card has it.
    Modern,
}

impl Look {
    pub fn name(self) -> &'static str {
        match self {
            Look::Classic => "classic",
            Look::Modern => "modern",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "classic" => Some(Look::Classic),
            "modern" => Some(Look::Modern),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub name: String,
    /// Chunks around the player that are real, in every direction.
    pub render_distance: i32,
    pub fov: f64,
    pub sensitivity: f64,
    pub view_bob: bool,
    pub far_terrain: bool,
    pub look: Look,
    /// Shadows and reflections from traced rays, on a graphics card that can, in the modern look.
    pub ray_tracing: bool,
    pub volume: f32,
    pub last_server: String,
    /// Whether frames wait for the display. Off, the game draws as fast as the graphics card can.
    pub frame_cap: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            name: "Player".into(),
            // A phone has a fraction of a desktop's memory and power to draw with.
            render_distance: if cfg!(target_os = "android") { 8 } else { 16 },
            fov: 75.0,
            sensitivity: 0.12,
            view_bob: true,
            far_terrain: true,
            look: Look::Classic,
            ray_tracing: false,
            volume: 0.7,
            last_server: "127.0.0.1".into(),
            // A phone's battery is better spent elsewhere than on frames its screen never shows.
            frame_cap: cfg!(target_os = "android"),
        }
    }
}

/// The folder for saves and settings: `ZENITH_HOME` if set, else the user's data folder.
pub fn home() -> PathBuf {
    zenith_mods::default_home()
}

impl Options {
    pub fn load(home: &Path) -> Self {
        std::fs::read(home.join("options.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    pub fn save(&self, home: &Path) {
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::create_dir_all(home);
            let _ = std::fs::write(home.join("options.json"), text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_from_an_older_version_fill_in_the_rest() {
        let o: Options = serde_json::from_str(r#"{"name":"ann"}"#).expect("parses");
        assert_eq!(o.name, "ann");
        assert_eq!(o.render_distance, Options::default().render_distance);
        assert!(!o.ray_tracing, "ray tracing is something to turn on, not something that is on");
        assert_eq!(o.look, Look::Classic);
        let modern: Options = serde_json::from_str(r#"{"look":"modern"}"#).expect("parses");
        assert_eq!(modern.look, Look::Modern);
        assert_eq!(Look::from_name(Look::Modern.name()), Some(Look::Modern));
    }

    #[test]
    fn damaged_options_fall_back_to_the_defaults() {
        let dir = std::env::temp_dir().join(format!("zenith-options-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("options.json"), b"{not json").expect("write");
        assert_eq!(Options::load(&dir), Options::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
