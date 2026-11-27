use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window_prefix: String,
    pub profiles: Vec<Profile>,
    pub active_profile: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub ocr: OcrSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrSettings {
    pub auto_region: bool,
    pub region: [i32; 4],
    pub th_var: f64,
    pub hp_max: i32,
    pub monster_max_damage: i32,
    pub key_var: String,
    pub mana_region: [i32; 4],
    pub mana_th_var: f64,
    pub mana_key_var: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            window_prefix: "Maple".into(),
            profiles: vec![Profile::default()],
            active_profile: 0,
        }
    }
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "預設".into(),
            ocr: OcrSettings::default(),
        }
    }
}
impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            auto_region: true,
            region: [0, 0, 200, 50],
            th_var: 50.0,
            hp_max: 0,
            monster_max_damage: 0,
            key_var: "Delete".into(),
            mana_region: [0, 60, 200, 110],
            mana_th_var: 40.0,
            mana_key_var: "End".into(),
        }
    }
}

pub fn settings_path() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("usersetting.json"))
}

pub fn load(path: &Path) -> io::Result<Settings> {
    if !path.exists() {
        return Ok(Settings::default());
    }
    let text = fs::read_to_string(path)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if value.get("profiles").is_some() {
        let mut settings: Settings = serde_json::from_value(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if settings.profiles.is_empty() {
            settings.profiles.push(Profile::default());
        }
        settings.active_profile = settings.active_profile.min(settings.profiles.len() - 1);
        Ok(settings)
    } else {
        // Migrate the previous single-profile `ocr` object on first load.
        let ocr_value = value
            .get("ocr")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let window_prefix = ocr_value
            .get("pref_var")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Maple")
            .to_owned();
        let ocr: OcrSettings = serde_json::from_value(ocr_value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // Ignore legacy `skills`; the Rust app intentionally has no skill loop.
        Ok(Settings {
            window_prefix,
            profiles: vec![Profile {
                name: "預設".into(),
                ocr,
            }],
            active_profile: 0,
        })
    }
}

pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(settings)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_legacy_ocr_settings_and_ignores_skills() {
        let path = std::env::temp_dir().join(format!("msw-settings-{}.json", std::process::id()));
        fs::write(&path, r#"{"skills":[{"key":"a"}],"ocr":{"pref_var":"Maple","region":[1,2,30,40],"key_var":"shift"}}"#).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.window_prefix, "Maple");
        assert_eq!(loaded.profiles[0].ocr.region, [1, 2, 30, 40]);
        assert_eq!(loaded.profiles[0].ocr.key_var, "shift");
        save(&path, &loaded).unwrap();
        assert_eq!(load(&path).unwrap().profiles[0].ocr.region, [1, 2, 30, 40]);
        let _ = fs::remove_file(path);
    }
}

impl OcrSettings {
    pub fn validate(&self, mana: bool) -> Result<(), String> {
        let (threshold, key, region) = if mana {
            (self.mana_th_var, &self.mana_key_var, self.mana_region)
        } else {
            (self.th_var, &self.key_var, self.region)
        };
        if !threshold.is_finite() || !(0.0..=100.0).contains(&threshold) {
            return Err("補水門檻需介於 0–100%。".into());
        }
        if self.hp_max < 0 || self.monster_max_damage < 0 {
            return Err("角色 HP 上限與怪物最大傷害不可為負數。".into());
        }
        if key.trim().is_empty() {
            return Err("請設定補水按鍵。".into());
        }
        if !self.auto_region && (region[2] <= region[0] || region[3] <= region[1]) {
            return Err("手動擷取區域的右下座標必須大於左上座標。".into());
        }
        Ok(())
    }
}
