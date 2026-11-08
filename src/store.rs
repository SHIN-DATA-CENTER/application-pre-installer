use crate::model::{Catalog, InstallMethod, Profile, SCHEMA_VERSION, Session, timestamp};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedCatalog {
    pub schema_version: u32,
    pub fetched_at: u64,
    pub catalog: Catalog,
}
impl CachedCatalog {
    pub fn new(catalog: Catalog) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            fetched_at: timestamp(),
            catalog,
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "未対応の一覧キャッシュです"
        );
        self.catalog.validate()?;
        for app in &self.catalog.apps {
            ensure!(
                matches!(&app.method, InstallMethod::Winget { package_id, source } if source == "winget" && app.id == format!("search-{package_id}")),
                "取得一覧に不正な導入方法があります"
            );
        }
        Ok(())
    }
}
pub struct InstanceLock {
    _file: File,
}
impl Store {
    pub fn open(demo: bool) -> Result<Self> {
        let root = if demo {
            std::env::temp_dir().join(format!(
                "application-pre-installer-demo-{}",
                std::process::id()
            ))
        } else {
            directories::BaseDirs::new()
                .context("利用者の保存先を取得できません")?
                .data_local_dir()
                .join("ApplicationPreInstaller")
        };
        fs::create_dir_all(root.join("logs"))?;
        fs::create_dir_all(root.join("cache"))?;
        Ok(Self { root })
    }
    pub fn lock(&self) -> Result<InstanceLock> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("instance.lock"))?;
        file.try_lock_exclusive()
            .context("アプリは既に起動しています")?;
        Ok(InstanceLock { _file: file })
    }
    pub fn save_session(&self, session: &Session) -> Result<()> {
        session.validate()?;
        write_json(
            &self
                .root
                .join("sessions")
                .join(format!("{}.json", session.id)),
            session,
        )?;
        write_json(&self.root.join("session.json"), session)
    }
    pub fn load_session(&self) -> Result<Option<Session>> {
        let path = self.root.join("session.json");
        if !path.exists() {
            return Ok(None);
        }
        let result: Session = read_json(&path)?;
        result.validate()?;
        Ok(Some(result))
    }
    pub fn save_profile(&self, p: &Profile) -> Result<()> {
        write_json(&self.root.join("profile.json"), p)
    }
    pub fn load_catalog(&self) -> Result<Option<CachedCatalog>> {
        let path = self.root.join("cache/winget-catalog.json");
        if !path.exists() {
            return Ok(None);
        }
        let cached: CachedCatalog = read_json(&path)?;
        cached.validate()?;
        Ok(Some(cached))
    }
    pub fn save_catalog(&self, cached: &CachedCatalog) -> Result<()> {
        cached.validate()?;
        write_json(&self.root.join("cache/winget-catalog.json"), cached)
    }
    pub fn load_profile(&self) -> Result<Option<Profile>> {
        let p = self.root.join("profile.json");
        if p.exists() {
            Ok(Some(read_json(&p)?))
        } else {
            Ok(None)
        }
    }
    pub fn log(&self, session: &str, message: &str) -> Result<()> {
        ensure!(
            session
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "ログ名が不正です"
        );
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.join("logs").join(format!("{session}.log")))?;
        writeln!(f, "[{}] {}", crate::model::timestamp(), message)?;
        Ok(())
    }
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        fs::metadata(path)?.len() <= 32 * 1024 * 1024,
        "JSONファイルが大きすぎます"
    );
    let bytes = fs::read(path)?;
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .context("JSONを読み込めません")
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("保存先が不正です")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        f.write_all(&serde_json::to_vec_pretty(value)?)?;
        f.sync_all()?;
        drop(f);
        replace(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("保存できません: {}", path.display()))
}
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let a: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let b: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // Both buffers are NUL terminated and remain alive for the entire call.
    ensure!(
        unsafe {
            MoveFileExW(
                a.as_ptr(),
                b.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0,
        "{}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_replacement_is_readable_and_lock_is_exclusive() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&root).unwrap();
        let s = Store { root: root.clone() };
        let lock = s.lock().unwrap();
        assert!(s.lock().is_err());
        let p = Profile {
            schema_version: 1,
            ..Default::default()
        };
        s.save_profile(&p).unwrap();
        s.save_profile(&p).unwrap();
        assert_eq!(s.load_profile().unwrap().unwrap().schema_version, 1);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
