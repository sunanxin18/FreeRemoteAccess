use std::path::{Path, PathBuf};

use frd_core::{ProtocolId, TargetSystem};
use frd_platform_api::{
    ConnectionProfileKey, ConnectionProfileStore, PlatformError, SavedConnectionProfile,
};
use serde::{Deserialize, Serialize};

const FORMAT_VERSION: u32 = 1;

pub struct WindowsConnectionProfileStore {
    path: PathBuf,
}

impl WindowsConnectionProfileStore {
    pub fn current_user_default() -> Result<Self, PlatformError> {
        let local_app_data = std::env::var_os("LOCALAPPDATA").ok_or(PlatformError::Unavailable)?;
        Ok(Self::at_path(
            PathBuf::from(local_app_data)
                .join("FreeRemoteDesk")
                .join("connections-v1.json"),
        ))
    }

    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn read_profiles(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(PlatformError::StorageFailed),
        };
        let document: ProfileDocument =
            serde_json::from_slice(&bytes).map_err(|_| PlatformError::InvalidProfile)?;
        if document.version != FORMAT_VERSION {
            return Err(PlatformError::InvalidProfile);
        }
        document
            .profiles
            .into_iter()
            .map(ProfileRecord::into_profile)
            .collect()
    }

    fn write_profiles(&self, profiles: &[SavedConnectionProfile]) -> Result<(), PlatformError> {
        let document = ProfileDocument {
            version: FORMAT_VERSION,
            profiles: profiles.iter().map(ProfileRecord::from_profile).collect(),
        };
        let encoded = serde_json::to_vec(&document).map_err(|_| PlatformError::StorageFailed)?;
        let parent = self.path.parent().ok_or(PlatformError::StorageFailed)?;
        std::fs::create_dir_all(parent).map_err(|_| PlatformError::StorageFailed)?;

        let temporary = temporary_path(&self.path);
        std::fs::write(&temporary, encoded).map_err(|_| PlatformError::StorageFailed)?;
        if std::fs::rename(&temporary, &self.path).is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(PlatformError::StorageFailed);
        }
        Ok(())
    }
}

impl ConnectionProfileStore for WindowsConnectionProfileStore {
    fn list(&self) -> Result<Vec<SavedConnectionProfile>, PlatformError> {
        let mut profiles = self.read_profiles()?;
        SavedConnectionProfile::sort_most_recent(&mut profiles);
        Ok(profiles)
    }

    fn upsert(&self, profile: &SavedConnectionProfile) -> Result<(), PlatformError> {
        let mut profiles = self.read_profiles()?;
        profiles.retain(|existing| existing.key != profile.key);
        profiles.push(profile.clone());
        self.write_profiles(&profiles)
    }

    fn delete(&self, key: &ConnectionProfileKey) -> Result<(), PlatformError> {
        let mut profiles = self.read_profiles()?;
        profiles.retain(|profile| &profile.key != key);
        self.write_profiles(&profiles)
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    PathBuf::from(temporary)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileDocument {
    version: u32,
    profiles: Vec<ProfileRecord>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileRecord {
    protocol: String,
    address: String,
    port: u16,
    username: String,
    target_system: String,
    last_success_order: u64,
}

impl ProfileRecord {
    fn from_profile(profile: &SavedConnectionProfile) -> Self {
        Self {
            protocol: profile.key.protocol().as_str().to_owned(),
            address: profile.key.address().to_owned(),
            port: profile.key.port(),
            username: profile.key.username().to_owned(),
            target_system: target_system_name(profile.target_system).to_owned(),
            last_success_order: profile.last_success_order,
        }
    }

    fn into_profile(self) -> Result<SavedConnectionProfile, PlatformError> {
        let protocol = ProtocolId::new(self.protocol).ok_or(PlatformError::InvalidProfile)?;
        let key = ConnectionProfileKey::new(protocol, self.address, self.port, self.username)
            .ok_or(PlatformError::InvalidProfile)?;
        let target_system =
            parse_target_system(&self.target_system).ok_or(PlatformError::InvalidProfile)?;
        Ok(SavedConnectionProfile {
            key,
            target_system,
            last_success_order: self.last_success_order,
        })
    }
}

fn target_system_name(target_system: TargetSystem) -> &'static str {
    match target_system {
        TargetSystem::MacOs => "macos",
        TargetSystem::Windows => "windows",
        TargetSystem::Linux => "linux",
        TargetSystem::Custom => "custom",
    }
}

fn parse_target_system(value: &str) -> Option<TargetSystem> {
    match value {
        "macos" => Some(TargetSystem::MacOs),
        "windows" => Some(TargetSystem::Windows),
        "linux" => Some(TargetSystem::Linux),
        "custom" => Some(TargetSystem::Custom),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use frd_core::{ProtocolId, TargetSystem};
    use frd_platform_api::{
        ConnectionProfileKey, ConnectionProfileStore, PlatformError, SavedConnectionProfile,
    };
    use serde_json::Value;
    use tempfile::tempdir;

    use super::WindowsConnectionProfileStore;

    fn test_profile(address: &str, username: &str, order: u64) -> SavedConnectionProfile {
        SavedConnectionProfile {
            key: ConnectionProfileKey::new(ProtocolId::apple_hpss_mvs(), address, 5900, username)
                .expect("test profile key is valid"),
            target_system: TargetSystem::MacOs,
            last_success_order: order,
        }
    }

    fn object_keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .expect("JSON value is an object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn metadata_round_trip_uses_only_the_version_one_schema_keys() {
        let temporary = tempdir().expect("temporary profile directory");
        let path = temporary.path().join("connections-v1.json");
        let store = WindowsConnectionProfileStore::at_path(path.clone());
        let profile = test_profile("sun.local", "alice", 7);

        store.upsert(&profile).expect("metadata is saved");

        let json: Value =
            serde_json::from_slice(&std::fs::read(path).expect("metadata is readable"))
                .expect("metadata is JSON");
        assert_eq!(object_keys(&json), BTreeSet::from(["profiles", "version"]));
        assert_eq!(json["version"], 1);
        let stored_profile = &json["profiles"][0];
        assert_eq!(
            object_keys(stored_profile),
            BTreeSet::from([
                "address",
                "last_success_order",
                "port",
                "protocol",
                "target_system",
                "username",
            ])
        );
        assert_eq!(store.list().expect("metadata is listed"), vec![profile]);
    }

    #[test]
    fn metadata_lists_newest_success_first() {
        let temporary = tempdir().expect("temporary profile directory");
        let store =
            WindowsConnectionProfileStore::at_path(temporary.path().join("connections-v1.json"));
        let older = test_profile("older.local", "alice", 1);
        let newer = test_profile("newer.local", "bob", 2);

        store.upsert(&older).expect("older metadata is saved");
        store.upsert(&newer).expect("newer metadata is saved");

        assert_eq!(
            store.list().expect("metadata is listed"),
            vec![newer, older]
        );
    }

    #[test]
    fn metadata_replaces_an_exact_matching_key() {
        let temporary = tempdir().expect("temporary profile directory");
        let store =
            WindowsConnectionProfileStore::at_path(temporary.path().join("connections-v1.json"));
        let original = test_profile("sun.local", "alice", 1);
        let replacement = test_profile("sun.local", "alice", 9);

        store.upsert(&original).expect("original metadata is saved");
        store
            .upsert(&replacement)
            .expect("replacement metadata is saved");

        assert_eq!(store.list().expect("metadata is listed"), vec![replacement]);
    }

    #[test]
    fn metadata_rejects_malformed_records() {
        let temporary = tempdir().expect("temporary profile directory");
        let path = temporary.path().join("connections-v1.json");
        std::fs::write(
            &path,
            r#"{"version":1,"profiles":[{"protocol":"apple-hpss-mvs","address":"sun.local","port":0,"username":"alice","target_system":"macos","last_success_order":1}]}"#,
        )
        .expect("malformed metadata fixture is written");
        let store = WindowsConnectionProfileStore::at_path(path);

        assert_eq!(store.list(), Err(PlatformError::InvalidProfile));
    }

    #[test]
    fn metadata_deletes_an_exact_matching_key() {
        let temporary = tempdir().expect("temporary profile directory");
        let store =
            WindowsConnectionProfileStore::at_path(temporary.path().join("connections-v1.json"));
        let retained = test_profile("retained.local", "alice", 1);
        let removed = test_profile("removed.local", "bob", 2);

        store.upsert(&retained).expect("retained metadata is saved");
        store.upsert(&removed).expect("removed metadata is saved");
        store.delete(&removed.key).expect("metadata is deleted");

        assert_eq!(store.list().expect("metadata is listed"), vec![retained]);
    }
}
