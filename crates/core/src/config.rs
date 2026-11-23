use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// TOML only supports string keys — serialize HashMap<u8,_> as HashMap<String,_>.
fn serialize_u8_map<S, V: Serialize>(map: &HashMap<u8, V>, s: S) -> Result<S::Ok, S::Error>
where S: Serializer {
    let str_map: HashMap<String, &V> = map.iter().map(|(k, v)| (k.to_string(), v)).collect();
    str_map.serialize(s)
}

fn deserialize_u8_map<'de, D, V: Deserialize<'de>>(d: D) -> Result<HashMap<u8, V>, D::Error>
where D: Deserializer<'de> {
    let str_map = HashMap::<String, V>::deserialize(d)?;
    str_map.into_iter()
        .map(|(k, v)| k.parse::<u8>().map(|n| (n, v)).map_err(serde::de::Error::custom))
        .collect()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ButtonConfig {
    #[serde(default)]
    pub icon: Option<PathBuf>,
    #[serde(default)]
    pub label: Option<String>,
    /// Inline Lua or path to a .lua file (resolved relative to config_dir/scripts/)
    #[serde(default)]
    pub on_press: Option<String>,
    #[serde(default)]
    pub on_release: Option<String>,
}

/// Reserved positions (bottom-left / bottom-right) once a profile has more than one page.
pub const PREV_PAGE_POS: u8 = 10;
pub const NEXT_PAGE_POS: u8 = 14;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageNav { Prev, Next }

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Page {
    /// Map of button position → config.
    #[serde(default, serialize_with = "serialize_u8_map", deserialize_with = "deserialize_u8_map")]
    pub buttons: HashMap<u8, ButtonConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default = "default_profile_name")]
    pub name: String,
    /// Wrap from last page to first (and back).
    #[serde(default = "default_true")]
    pub circular: bool,
    #[serde(default)]
    pub pages: Vec<Page>,
    /// Pre-pages profiles stored buttons at top level; migrated into page 1 on load.
    #[serde(default, rename = "buttons", skip_serializing, deserialize_with = "deserialize_u8_map")]
    legacy_buttons: HashMap<u8, ButtonConfig>,
}

fn default_profile_name() -> String {
    "Default".into()
}

fn default_true() -> bool { true }

impl Profile {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut p: Self = toml::from_str(&content)?;
        if p.pages.is_empty() {
            p.pages.push(Page { buttons: std::mem::take(&mut p.legacy_buttons) });
        }
        Ok(p)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn button(&self, page: usize, position: u8) -> Option<&ButtonConfig> {
        self.pages.get(page)?.buttons.get(&position)
    }

    /// Navigation role of a position, if pagination is active.
    pub fn nav_at(&self, position: u8) -> Option<PageNav> {
        if self.pages.len() < 2 { return None; }
        match position {
            PREV_PAGE_POS => Some(PageNav::Prev),
            NEXT_PAGE_POS => Some(PageNav::Next),
            _ => None,
        }
    }

    pub fn step_page(&self, page: usize, nav: PageNav) -> usize {
        let last = self.pages.len().saturating_sub(1);
        match nav {
            PageNav::Next if page < last => page + 1,
            PageNav::Next if self.circular => 0,
            PageNav::Prev if page > 0 => page - 1,
            PageNav::Prev if self.circular => last,
            _ => page.min(last),
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "Default".into(),
            circular: true,
            pages: vec![Page::default()],
            legacy_buttons: HashMap::new(),
        }
    }
}

pub fn default_profile_path() -> PathBuf {
    profiles_dir().join("default.toml")
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("rustdeck")
}

pub fn profiles_dir() -> PathBuf {
    config_dir().join("profiles")
}

pub fn scripts_dir() -> PathBuf {
    config_dir().join("scripts")
}

/// Resolve an `on_press` value: if it ends in ".lua", read the file;
/// otherwise treat it as inline Lua.
pub fn resolve_script(value: &str) -> Result<String> {
    if value.ends_with(".lua") {
        let path = scripts_dir().join(value);
        Ok(std::fs::read_to_string(&path)?)
    } else {
        Ok(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_buttons_become_page_one() {
        let dir = std::env::temp_dir().join("rustdeck-cfg-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.toml");
        std::fs::write(&path, "name = \"Default\"\n[buttons.0]\nlabel = \"A\"\n").unwrap();
        let p = Profile::load(&path).unwrap();
        assert_eq!(p.pages.len(), 1);
        assert_eq!(p.button(0, 0).unwrap().label.as_deref(), Some("A"));
        assert!(p.circular);
        p.save(&path).unwrap();
        let again = Profile::load(&path).unwrap();
        assert_eq!(again.button(0, 0).unwrap().label.as_deref(), Some("A"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("[buttons"));
    }

    #[test]
    fn paging_wraps_only_when_circular() {
        let mut p = Profile::default();
        assert_eq!(p.nav_at(PREV_PAGE_POS), None);
        p.pages.push(Page::default());
        p.pages.push(Page::default());
        assert_eq!(p.nav_at(PREV_PAGE_POS), Some(PageNav::Prev));
        assert_eq!(p.nav_at(NEXT_PAGE_POS), Some(PageNav::Next));
        assert_eq!(p.step_page(2, PageNav::Next), 0);
        assert_eq!(p.step_page(0, PageNav::Prev), 2);
        p.circular = false;
        assert_eq!(p.step_page(2, PageNav::Next), 2);
        assert_eq!(p.step_page(0, PageNav::Prev), 0);
        assert_eq!(p.step_page(1, PageNav::Next), 2);
    }
}
