use std::path::{Path, PathBuf};
use std::sync::RwLock;

use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::config::ConfigFile;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub last_connected: Option<u64>,
}

/// Public profile representation.  Passwords never leave the backend.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: Option<String>,
    pub trusted: bool,
    pub has_password: bool,
    pub last_connected: Option<u64>,
}

impl Profile {
    pub fn view(&self) -> ProfileView {
        ProfileView {
            id: self.id.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            fingerprint: self.fingerprint.clone(),
            trusted: self.trusted,
            has_password: self.password.as_ref().is_some_and(|p| !p.is_empty()),
            last_connected: self.last_connected,
        }
    }
}

pub fn new_id() -> String {
    let mut bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct ProfileStore {
    path: PathBuf,
    data: RwLock<ConfigFile>,
}

impl ProfileStore {
    pub fn new(path: PathBuf, data: ConfigFile) -> Self {
        Self {
            path,
            data: RwLock::new(data),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn list(&self) -> Vec<ProfileView> {
        self.data
            .read()
            .expect("profiles lock poisoned")
            .profiles
            .iter()
            .map(Profile::view)
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Profile> {
        self.data
            .read()
            .expect("profiles lock poisoned")
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    pub fn find_by_endpoint(&self, host: &str, port: u16) -> Option<Profile> {
        self.data
            .read()
            .expect("profiles lock poisoned")
            .profiles
            .iter()
            .find(|p| p.host == host && p.port == port)
            .cloned()
    }

    pub fn upsert(&self, mut profile: Profile) -> Result<ProfileView> {
        if profile.id.is_empty() {
            profile.id = new_id();
        }
        if profile.name.trim().is_empty() {
            profile.name = if profile.host.is_empty() {
                "UTM server".to_owned()
            } else {
                profile.host.clone()
            };
        }
        {
            let mut data = self.data.write().expect("profiles lock poisoned");
            if let Some(existing) = data.profiles.iter_mut().find(|p| p.id == profile.id) {
                *existing = profile.clone();
            } else {
                data.profiles.push(profile.clone());
            }
        }
        self.save()?;
        Ok(profile.view())
    }

    pub fn update<F>(&self, id: &str, f: F) -> Result<Option<ProfileView>>
    where
        F: FnOnce(&mut Profile),
    {
        let result = {
            let mut data = self.data.write().expect("profiles lock poisoned");
            match data.profiles.iter_mut().find(|p| p.id == id) {
                Some(profile) => {
                    f(profile);
                    Some(profile.view())
                }
                None => None,
            }
        };
        if result.is_some() {
            self.save()?;
        }
        Ok(result)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let removed = {
            let mut data = self.data.write().expect("profiles lock poisoned");
            let before = data.profiles.len();
            data.profiles.retain(|p| p.id != id);
            data.profiles.len() != before
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn save(&self) -> Result<()> {
        let data = self.data.read().expect("profiles lock poisoned");
        let text = serde_json::to_string_pretty(&*data)
            .map_err(|e| Error::Config(format!("cannot serialize config: {e}")))?;
        drop(data);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Config(format!("cannot create config directory {parent:?}: {e}"))
            })?;
        }
        let tmp = self.path.with_extension("tmp");
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&tmp)
                .map_err(|e| Error::Config(format!("cannot write config {tmp:?}: {e}")))?;
            file.write_all(text.as_bytes())
                .map_err(|e| Error::Config(format!("cannot write config {tmp:?}: {e}")))?;
            file.sync_all().ok();
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| Error::Config(format!("cannot replace config {:?}: {e}", self.path)))?;
        Ok(())
    }
}
