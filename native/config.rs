use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub id: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}
impl Root {
    pub fn display_name(&self) -> String {
        self.nickname.clone().unwrap_or_else(|| {
            self.path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| self.id.clone())
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub roots: Vec<Root>,
    #[serde(default = "one")]
    pub next_root_id: u64,
}
fn one() -> u64 {
    1
}

pub fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("LOCAL_FILES_DATA_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return path;
    }
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| ".".into()))
            .join("Local Files")
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
            .join("Library/Application Support/Local Files")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                    .join(".local/share")
            })
            .join("local-files")
    }
}
pub fn default_path() -> PathBuf {
    data_dir().join("folders.json")
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut data = Vec::new();
        fs::File::open(path)
            .context("Не удалось открыть настройки папок")?
            .take(65_537)
            .read_to_end(&mut data)?;
        anyhow::ensure!(data.len() <= 65_536, "Configuration exceeds 64 KiB");
        let config: Self = serde_json::from_slice(&data).context("Некорректные настройки папок")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.version == 1, "Unsupported configuration version");
        anyhow::ensure!(self.roots.len() <= 64, "At most 64 roots are supported");
        let mut ids = HashSet::new();
        for root in &self.roots {
            let b = root.id.as_bytes();
            anyhow::ensure!(
                !b.is_empty()
                    && b.len() <= 32
                    && b[0].is_ascii_lowercase()
                    && b.iter().all(|v| v.is_ascii_lowercase()
                        || v.is_ascii_digit()
                        || *v == b'_'
                        || *v == b'-')
                    && root.id != "all"
                    && ids.insert(&root.id),
                "Invalid or duplicate root ID"
            );
            anyhow::ensure!(root.path.is_absolute(), "Folder paths must be absolute");
            if let Some(nickname) = &root.nickname {
                validate_nickname(nickname)?;
                let key = nickname.to_lowercase();
                anyhow::ensure!(
                    self.roots.iter().all(|other| other.id == root.id
                        || (other.id.to_lowercase() != key
                            && other.display_name().to_lowercase() != key)),
                    "Это прозвище уже используется другой папкой"
                );
            }
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let parent = path
            .parent()
            .context("Configuration requires a parent directory")?;
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut temp, self)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .context("Не удалось сохранить настройки")?;
        Ok(())
    }
    pub fn add(&mut self, path: &Path) -> Result<()> {
        let path = fs::canonicalize(path).context("Папка недоступна")?;
        anyhow::ensure!(path.is_dir(), "Выберите папку");
        if self
            .roots
            .iter()
            .any(|r| fs::canonicalize(&r.path).ok().as_ref() == Some(&path))
        {
            bail!("Эта папка уже подключена");
        }
        loop {
            let id = format!("folder_{}", self.next_root_id);
            self.next_root_id = self
                .next_root_id
                .checked_add(1)
                .context("Root ID limit reached")?;
            if !self
                .roots
                .iter()
                .any(|r| r.id == id || r.display_name().eq_ignore_ascii_case(&id))
            {
                self.roots.push(Root {
                    id,
                    path,
                    nickname: None,
                });
                return self.validate();
            }
        }
    }
    pub fn find_root(&self, name: &str) -> Result<&Root> {
        if let Some(root) = self.roots.iter().find(|r| r.id == name) {
            return Ok(root);
        }
        let key = name.to_lowercase();
        let mut matches = self
            .roots
            .iter()
            .filter(|r| r.display_name().to_lowercase() == key);
        let root = matches.next().context("Unknown folder nickname or ID")?;
        anyhow::ensure!(
            matches.next().is_none(),
            "Folder name is ambiguous; use its ID"
        );
        Ok(root)
    }
    pub fn selector(&self, root: &Root) -> String {
        let name = root.display_name();
        if name.eq_ignore_ascii_case("all") || !self.find_root(&name).is_ok_and(|r| r.id == root.id)
        {
            root.id.clone()
        } else {
            name
        }
    }
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn rename(&mut self, id: &str, nickname: &str) -> Result<()> {
        let nickname = nickname.trim();
        validate_nickname(nickname)?;
        // Validate a candidate first; failed edits leave the live configuration intact.
        let mut candidate = self.clone();
        candidate
            .roots
            .iter_mut()
            .find(|r| r.id == id)
            .context("Папка уже отключена")?
            .nickname = Some(nickname.to_owned());
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}

fn validate_nickname(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty() && name.chars().count() <= 64 && name.trim() == name,
        "Прозвище должно содержать от 1 до 64 символов"
    );
    anyhow::ensure!(
        !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':')),
        "Прозвище не должно содержать переносы строк, /, \\ или :"
    );
    anyhow::ensure!(
        !name.eq_ignore_ascii_case("all"),
        "Прозвище all зарезервировано для поиска"
    );
    Ok(())
}

pub fn initialize(path: &Path) -> Result<()> {
    if path.exists() {
        Config::load(path)?;
        return Ok(());
    }
    Config {
        version: 1,
        // First launch must never grant access based on paths or environment defaults.
        roots: Vec::new(),
        next_root_id: 1,
    }
    .save(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_launch_shares_nothing_and_existing_settings_are_preserved() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("folders.json");
        initialize(&path)?;
        assert!(Config::load(&path)?.roots.is_empty());
        let mut config = Config::load(&path)?;
        config.add(temp.path())?;
        config.rename("folder_1", "Мои документы")?;
        config.save(&path)?;
        let before = fs::read(&path)?;
        initialize(&path)?;
        assert_eq!(fs::read(&path)?, before);
        Ok(())
    }

    #[test]
    fn nicknames_persist_without_changing_document_identity() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let original = temp.path().join("Work (OD)");
        fs::create_dir(&original)?;
        let path = temp.path().join("folders.json");
        let mut config = Config {
            version: 1,
            next_root_id: 2,
            roots: vec![Root {
                id: "folder_1".into(),
                path: original.clone(),
                nickname: None,
            }],
        };
        config.save(&path)?;
        assert_eq!(Config::load(&path)?.roots[0].display_name(), "Work (OD)");
        config.rename("folder_1", "  Работа и документы  ")?;
        config.save(&path)?;
        let loaded = Config::load(&path)?;
        assert_eq!(loaded.roots[0].path, original);
        assert_eq!(loaded.find_root("РАБОТА И ДОКУМЕНТЫ")?.id, "folder_1");
        assert_eq!(
            loaded.find_root("folder_1")?.display_name(),
            "Работа и документы"
        );
        assert_eq!(loaded.next_root_id, 2);
        Ok(())
    }

    #[test]
    fn conflicting_or_invalid_names_cannot_change_existing_config() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut config = Config {
            version: 1,
            next_root_id: 3,
            roots: vec![
                Root {
                    id: "folder_1".into(),
                    path: temp.path().join("Work"),
                    nickname: None,
                },
                Root {
                    id: "folder_2".into(),
                    path: temp.path().join("Archive"),
                    nickname: Some("Архив".into()),
                },
            ],
        };
        for nickname in [
            "",
            " ",
            "ALL",
            "folder_2",
            "Архив",
            "АРХИВ",
            "a/b",
            "a\\b",
            "a:b",
            "a\nb",
            &"а".repeat(65),
        ] {
            let before = serde_json::to_value(&config)?;
            assert!(config.rename("folder_1", nickname).is_err(), "{nickname}");
            assert_eq!(serde_json::to_value(&config)?, before);
        }
        assert!(config.rename("folder_2", "WORK").is_err());
        config.rename("folder_1", &"я".repeat(64))?;
        // Identical basenames in old configurations remain usable via stable IDs.
        config.roots[0].nickname = None;
        config.roots[1].nickname = None;
        config.roots[1].path = temp.path().join("another").join("Work");
        config.validate()?;
        assert!(config.find_root("Work").is_err());
        assert_eq!(config.selector(&config.roots[0]), "folder_1");
        Ok(())
    }
}
