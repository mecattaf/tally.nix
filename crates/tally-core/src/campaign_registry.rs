//! Durable, rollback-compatible campaign registrations.
//!
//! The authority object is deliberately frozen at schema version 2: it is the
//! exact closed shape understood by the immediately preceding tally release.
//! Host-local settings live beside it so an older binary can keep scanning the
//! `armed` directory without encountering fields it cannot decode.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

pub const REGISTRY_SCHEMA_VERSION: u32 = 2;
pub const HOST_TUNING_SCHEMA_VERSION: u32 = 1;
/// Effective host tuning when a stable-v2 authority has no tuning sidecar.
pub const DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS: u64 = 10_000;

/// The schema-2 authority record, frozen to the literal N-1 field set.
///
/// Do not add fields to this type. Extensions belong in a separately
/// versioned sidecar and the N/N-1 compatibility tests below must move with
/// any future authority schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignRegistrationV2 {
    pub schema_version: u32,
    pub registration_id: String,
    pub issue_url: String,
    pub repository: String,
    pub issue_number: u64,
    pub armed_at: String,
    pub arm_serial: u64,
    pub approved_graph_digest: String,
    pub authenticated_actor: String,
    pub allowed_actors: Vec<String>,
    pub allow_test_local_forge: bool,
    #[serde(default)]
    pub sub_issue_walk: bool,
    #[serde(default)]
    pub last_observation: Option<String>,
    #[serde(default)]
    pub last_forge_observation: Option<String>,
    pub flow: PathBuf,
    pub driver: PathBuf,
    pub workspace_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignHostTuningV1 {
    schema_version: u32,
    #[serde(default)]
    projection_wait_ms: Option<u64>,
}

impl CampaignHostTuningV1 {
    const fn new(projection_wait_ms: Option<u64>) -> Self {
        Self {
            schema_version: HOST_TUNING_SCHEMA_VERSION,
            projection_wait_ms,
        }
    }
}

/// The current process view: stable authority plus host-local tuning.
///
/// This type intentionally does not implement `Serialize`. Durable writers
/// must choose the authority or sidecar explicitly, preventing a convenient
/// whole-value serialization from putting tuning back into closed schema 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignRegistration {
    authority: CampaignRegistrationV2,
    pub projection_wait_ms: Option<u64>,
}

impl CampaignRegistration {
    pub const fn new(authority: CampaignRegistrationV2, projection_wait_ms: Option<u64>) -> Self {
        Self {
            authority,
            projection_wait_ms,
        }
    }

    pub const fn authority(&self) -> &CampaignRegistrationV2 {
        &self.authority
    }

    pub fn into_authority(self) -> CampaignRegistrationV2 {
        self.authority
    }

    /// Preserve the historical flat `campaign list` presentation without
    /// making that presentation a durable serializer.
    pub fn list_value(&self) -> Result<Value, CampaignRegistryError> {
        let mut value = serde_json::to_value(&self.authority).map_err(|source| {
            CampaignRegistryError::Encode {
                context: "campaign list view",
                source,
            }
        })?;
        value
            .as_object_mut()
            .expect("a registration struct always serializes as an object")
            .insert(
                "projectionWaitMs".to_owned(),
                self.projection_wait_ms.map_or(Value::Null, Value::from),
            );
        Ok(value)
    }
}

impl Deref for CampaignRegistration {
    type Target = CampaignRegistrationV2;

    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}

impl DerefMut for CampaignRegistration {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.authority
    }
}

/// An exclusively locked campaign registry.
///
/// Reads are exclusive too because encountering the one historical polluted-v2
/// shape performs a one-time migration before returning it.
pub struct CampaignRegistry {
    state_dir: PathBuf,
    lock: File,
}

impl CampaignRegistry {
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, CampaignRegistryError> {
        let state_dir = state_dir.as_ref().to_owned();
        let directory = authority_dir(&state_dir);
        secure_directory(&directory)?;
        let lock_path = directory.join("registry.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|source| io_error(&lock_path, source))?;
        lock.lock_exclusive()
            .map_err(|source| io_error(&lock_path, source))?;
        Ok(Self { state_dir, lock })
    }

    pub fn registration_path(&self, issue_url: &str) -> PathBuf {
        registration_path(&self.state_dir, issue_url)
    }

    pub fn read_issue(
        &self,
        issue_url: &str,
    ) -> Result<Option<CampaignRegistration>, CampaignRegistryError> {
        let path = self.registration_path(issue_url);
        if path.exists() {
            self.read(&path).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn read(&self, path: &Path) -> Result<CampaignRegistration, CampaignRegistryError> {
        let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
        match serde_json::from_slice::<CampaignRegistrationV2>(&bytes) {
            Ok(authority) => {
                validate_authority(path, &authority)?;
                let projection_wait_ms = self.read_host_tuning(&authority.registration_id)?;
                Ok(CampaignRegistration::new(authority, projection_wait_ms))
            }
            Err(strict_error) => self.migrate_polluted_v2(path, &bytes, strict_error),
        }
    }

    pub fn registrations(
        &self,
    ) -> Result<Vec<(PathBuf, CampaignRegistration)>, CampaignRegistryError> {
        let directory = authority_dir(&self.state_dir);
        let mut paths = fs::read_dir(&directory)
            .map_err(|source| io_error(&directory, source))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension() == Some(OsStr::new("json")))
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| self.read(&path).map(|registration| (path, registration)))
            .collect()
    }

    pub fn write(&self, registration: &CampaignRegistration) -> Result<(), CampaignRegistryError> {
        let path = self.registration_path(&registration.issue_url);
        validate_authority(&path, registration.authority())?;
        self.write_host_tuning(
            &registration.registration_id,
            registration.projection_wait_ms,
        )?;
        atomic_write_json(&path, registration.authority())
    }

    pub fn remove_issue(&self, issue_url: &str) -> Result<bool, CampaignRegistryError> {
        let path = self.registration_path(issue_url);
        if !path.exists() {
            return Ok(false);
        }
        let registration = self.read(&path)?;
        self.remove(&registration)?;
        Ok(true)
    }

    pub fn remove(&self, registration: &CampaignRegistration) -> Result<(), CampaignRegistryError> {
        let path = self.registration_path(&registration.issue_url);
        remove_file_if_present(&path)?;
        let sidecar = host_tuning_path(&self.state_dir, &registration.registration_id);
        remove_file_if_present(&sidecar)?;
        Ok(())
    }

    fn read_host_tuning(
        &self,
        registration_id: &str,
    ) -> Result<Option<u64>, CampaignRegistryError> {
        let path = host_tuning_path(&self.state_dir, registration_id);
        if !path.exists() {
            // Sidecar absence is the stable-v2 representation of the
            // historical host default, not an unknown tuning value.
            return Ok(Some(DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS));
        }
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        let tuning: CampaignHostTuningV1 = serde_json::from_slice(&bytes).map_err(|source| {
            CampaignRegistryError::InvalidHostTuning {
                path: path.clone(),
                reason: source.to_string(),
            }
        })?;
        if tuning.schema_version != HOST_TUNING_SCHEMA_VERSION {
            return Err(CampaignRegistryError::InvalidHostTuning {
                path,
                reason: format!(
                    "schemaVersion must equal {HOST_TUNING_SCHEMA_VERSION}, got {}",
                    tuning.schema_version
                ),
            });
        }
        Ok(Some(
            tuning
                .projection_wait_ms
                .unwrap_or(DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS),
        ))
    }

    fn write_host_tuning(
        &self,
        registration_id: &str,
        projection_wait_ms: Option<u64>,
    ) -> Result<(), CampaignRegistryError> {
        let path = host_tuning_path(&self.state_dir, registration_id);
        match projection_wait_ms {
            Some(projection_wait_ms) => {
                atomic_write_json(&path, &CampaignHostTuningV1::new(Some(projection_wait_ms)))
            }
            // Re-arming without an override must also clear a retained
            // override. The reader supplies the effective default.
            None => remove_file_if_present(&path),
        }
    }

    fn migrate_polluted_v2(
        &self,
        path: &Path,
        bytes: &[u8],
        strict_error: serde_json::Error,
    ) -> Result<CampaignRegistration, CampaignRegistryError> {
        // This decoder is intentionally narrow. It removes exactly the one
        // member shipped accidentally in schema 2, then sends the remainder
        // through the same deny-unknown-fields decoder. A future unknown field
        // therefore remains unknown and is rejected rather than silently
        // acquiring migration privileges.
        let mut value: Value = serde_json::from_slice(bytes)
            .map_err(|_| invalid_registration(path, strict_error.to_string()))?;
        let Some(object) = value.as_object_mut() else {
            return Err(invalid_registration(path, strict_error.to_string()));
        };
        let Some(polluted_value) = object.remove("projectionWaitMs") else {
            return Err(invalid_registration(path, strict_error.to_string()));
        };
        let projection_wait_ms = serde_json::from_value::<Option<u64>>(polluted_value)
            .map_err(|error| invalid_registration(path, error.to_string()))?;
        let authority: CampaignRegistrationV2 = serde_json::from_value(value)
            .map_err(|error| invalid_registration(path, error.to_string()))?;
        validate_authority(path, &authority)?;

        let sidecar_path = host_tuning_path(&self.state_dir, &authority.registration_id);
        let projection_wait_ms = if sidecar_path.exists() {
            // A sidecar may already exist when the previous migration reached
            // its safe first publication step but crashed before authority was
            // rewritten. Retain that published tuning.
            self.read_host_tuning(&authority.registration_id)?
        } else {
            self.write_host_tuning(&authority.registration_id, projection_wait_ms)?;
            Some(projection_wait_ms.unwrap_or(DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS))
        };
        atomic_write_json(path, &authority)?;
        Ok(CampaignRegistration::new(authority, projection_wait_ms))
    }
}

impl Drop for CampaignRegistry {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.lock);
    }
}

#[derive(Debug, Error)]
pub enum CampaignRegistryError {
    #[error("campaign registry I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid campaign registration {path}: {reason}")]
    InvalidRegistration { path: PathBuf, reason: String },
    #[error("invalid campaign host tuning {path}: {reason}")]
    InvalidHostTuning { path: PathBuf, reason: String },
    #[error("cannot encode {context}: {source}")]
    Encode {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

fn authority_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("campaigns/armed")
}

fn host_tuning_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("campaigns/host-tuning")
}

fn registration_path(state_dir: &Path, issue_url: &str) -> PathBuf {
    let digest = Sha256::digest(issue_url.as_bytes());
    authority_dir(state_dir).join(format!("{digest:x}.json"))
}

fn host_tuning_path(state_dir: &Path, registration_id: &str) -> PathBuf {
    host_tuning_dir(state_dir).join(format!("{registration_id}.host-v1.json"))
}

fn secure_directory(path: &Path) -> Result<(), CampaignRegistryError> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error(path, source))
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<(), CampaignRegistryError> {
    let parent = path.parent().ok_or_else(|| CampaignRegistryError::Io {
        path: path.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "registry path has no parent directory",
        ),
    })?;
    secure_directory(parent)?;
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|source| CampaignRegistryError::Encode {
            context: "campaign registry document",
            source,
        })?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("campaign-registration"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        file.write_all(&bytes)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        sync_directory(parent)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn remove_file_if_present(path: &Path) -> Result<(), CampaignRegistryError> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            Ok(())
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}

fn sync_directory(path: &Path) -> Result<(), CampaignRegistryError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn validate_authority(
    path: &Path,
    registration: &CampaignRegistrationV2,
) -> Result<(), CampaignRegistryError> {
    let invalid = registration.schema_version != REGISTRY_SCHEMA_VERSION
        || uuid::Uuid::parse_str(&registration.registration_id).is_err()
        || registration.issue_number == 0
        || registration.arm_serial == 0
        || !registration
            .approved_graph_digest
            .strip_prefix("sha256:")
            .is_some_and(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        || !safe_github_login(&registration.authenticated_actor)
        || registration.allowed_actors.is_empty()
        || registration
            .allowed_actors
            .iter()
            .any(|actor| !safe_github_login(actor) || actor != &actor.to_ascii_lowercase())
        || !registration
            .allowed_actors
            .contains(&registration.authenticated_actor)
        || !registration.flow.is_absolute()
        || !registration.driver.is_absolute()
        || !registration.workspace_root.is_absolute();
    if invalid {
        return Err(invalid_registration(
            path,
            "record violates schema v2 invariants; explicitly disarm and re-arm legacy registrations",
        ));
    }
    let Some((repository, issue_number)) = issue_locator(&registration.issue_url) else {
        return Err(invalid_registration(
            path,
            "issueUrl is not a canonical GitHub issue URL",
        ));
    };
    if repository != registration.repository || issue_number != registration.issue_number {
        return Err(invalid_registration(
            path,
            "record has inconsistent locator fields",
        ));
    }
    Ok(())
}

fn issue_locator(value: &str) -> Option<(String, u64)> {
    let remainder = value.strip_prefix("https://github.com/")?;
    if remainder.contains(['?', '#']) || remainder.ends_with('/') {
        return None;
    }
    let parts = remainder.split('/').collect::<Vec<_>>();
    if parts.len() != 4
        || parts[2] != "issues"
        || !safe_repo_part(parts[0])
        || !safe_repo_part(parts[1])
    {
        return None;
    }
    let number = parts[3].parse::<u64>().ok().filter(|number| *number > 0)?;
    Some((format!("{}/{}", parts[0], parts[1]), number))
}

fn safe_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn safe_github_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn io_error(path: &Path, source: std::io::Error) -> CampaignRegistryError {
    CampaignRegistryError::Io {
        path: path.to_owned(),
        source,
    }
}

fn invalid_registration(path: &Path, reason: impl Into<String>) -> CampaignRegistryError {
    CampaignRegistryError::InvalidRegistration {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal copy of the closed reader shipped at commit 84b1bf0. This is a
    /// compatibility oracle, not an alias to the current authority type.
    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    struct NMinusOneCampaignRegistration {
        schema_version: u32,
        registration_id: String,
        issue_url: String,
        repository: String,
        issue_number: u64,
        armed_at: String,
        arm_serial: u64,
        approved_graph_digest: String,
        authenticated_actor: String,
        allowed_actors: Vec<String>,
        allow_test_local_forge: bool,
        #[serde(default)]
        sub_issue_walk: bool,
        #[serde(default)]
        last_observation: Option<String>,
        #[serde(default)]
        last_forge_observation: Option<String>,
        flow: PathBuf,
        driver: PathBuf,
        workspace_root: PathBuf,
    }

    fn authority() -> CampaignRegistrationV2 {
        CampaignRegistrationV2 {
            schema_version: REGISTRY_SCHEMA_VERSION,
            registration_id: "0198a62b-41ee-7000-8000-000000000447".to_owned(),
            issue_url: "https://github.com/mecattaf/tally.nix/issues/447".to_owned(),
            repository: "mecattaf/tally.nix".to_owned(),
            issue_number: 447,
            armed_at: "2026-08-08T10:00:00Z".to_owned(),
            arm_serial: 3,
            approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
            authenticated_actor: "operator".to_owned(),
            allowed_actors: vec!["operator".to_owned(), "reviewer".to_owned()],
            allow_test_local_forge: false,
            sub_issue_walk: true,
            last_observation: Some("observation".to_owned()),
            last_forge_observation: Some("forge-observation".to_owned()),
            flow: PathBuf::from("/nix/store/flow/share/spec-build.js"),
            driver: PathBuf::from("/nix/store/driver/bin/spec-build-driver"),
            workspace_root: PathBuf::from("/var/lib/tally/campaigns"),
        }
    }

    fn authority_bytes(registry: &CampaignRegistry, issue_url: &str) -> Vec<u8> {
        fs::read(registry.registration_path(issue_url)).unwrap()
    }

    fn host_tuning_files(state_dir: &Path) -> Vec<PathBuf> {
        // This is the public on-disk boundary. Keep the literal path here so
        // the test cannot follow an accidental production rename.
        let directory = state_dir.join("campaigns/host-tuning");
        let mut paths = match fs::read_dir(directory) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.is_file())
                .collect::<Vec<_>>(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("cannot inspect host tuning sidecars: {error}"),
        };
        paths.sort();
        paths
    }

    #[test]
    fn current_default_and_override_authority_are_literal_n_minus_one_bytes() {
        for projection_wait_ms in [None, Some(240_000)] {
            let temporary = tempfile::tempdir().unwrap();
            let registry = CampaignRegistry::open(temporary.path()).unwrap();
            let registration = CampaignRegistration::new(authority(), projection_wait_ms);
            registry.write(&registration).unwrap();

            let bytes = authority_bytes(&registry, &registration.issue_url);
            let decoded: NMinusOneCampaignRegistration = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(decoded.issue_url, registration.issue_url);
            assert!(!String::from_utf8(bytes)
                .unwrap()
                .contains("projectionWaitMs"));

            let current = registry
                .read_issue(&registration.issue_url)
                .unwrap()
                .unwrap();
            assert_eq!(
                current.projection_wait_ms,
                Some(projection_wait_ms.unwrap_or(DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS))
            );

            let sidecars = host_tuning_files(temporary.path());
            if let Some(expected) = projection_wait_ms {
                assert_eq!(sidecars.len(), 1, "explicit: {sidecars:?}");
                let tuning: CampaignHostTuningV1 =
                    serde_json::from_slice(&fs::read(&sidecars[0]).unwrap()).unwrap();
                assert_eq!(tuning, CampaignHostTuningV1::new(Some(expected)));
            } else {
                assert!(sidecars.is_empty(), "default: {sidecars:?}");
            }
        }
    }

    #[test]
    fn current_reader_accepts_literal_n_minus_one_bytes_without_changing_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let registry = CampaignRegistry::open(temporary.path()).unwrap();
        let expected = authority();
        let legacy: NMinusOneCampaignRegistration =
            serde_json::from_value(serde_json::to_value(&expected).unwrap()).unwrap();
        let legacy_bytes = serde_json::to_vec_pretty(&legacy).unwrap();
        let path = registry.registration_path(&expected.issue_url);
        fs::write(&path, &legacy_bytes).unwrap();

        let loaded = registry.read(&path).unwrap();
        assert_eq!(loaded.authority(), &expected);
        assert_eq!(
            loaded.projection_wait_ms,
            Some(DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS)
        );
        assert_eq!(fs::read(path).unwrap(), legacy_bytes);
        assert!(host_tuning_files(temporary.path()).is_empty());
    }

    #[test]
    fn polluted_v2_is_migrated_once_to_stable_authority_and_sidecar() {
        let temporary = tempfile::tempdir().unwrap();
        let registry = CampaignRegistry::open(temporary.path()).unwrap();
        let expected = authority();
        let path = registry.registration_path(&expected.issue_url);
        let mut polluted = serde_json::to_value(&expected).unwrap();
        polluted
            .as_object_mut()
            .unwrap()
            .insert("projectionWaitMs".to_owned(), Value::from(240_000));
        fs::write(&path, serde_json::to_vec_pretty(&polluted).unwrap()).unwrap();

        let loaded = registry.read(&path).unwrap();
        assert_eq!(loaded.authority(), &expected);
        assert_eq!(loaded.projection_wait_ms, Some(240_000));
        let stable_bytes = fs::read(&path).unwrap();
        let stable: NMinusOneCampaignRegistration = serde_json::from_slice(&stable_bytes).unwrap();
        assert_eq!(stable.issue_url, expected.issue_url);
        assert!(!String::from_utf8(stable_bytes)
            .unwrap()
            .contains("projectionWaitMs"));
        assert!(host_tuning_path(temporary.path(), &expected.registration_id).is_file());

        let loaded_again = registry.read(&path).unwrap();
        assert_eq!(loaded_again.projection_wait_ms, Some(240_000));
    }

    #[test]
    fn downgrade_rewrite_keeps_sidecar_override_for_reupgrade() {
        let temporary = tempfile::tempdir().unwrap();
        let registry = CampaignRegistry::open(temporary.path()).unwrap();
        let registration = CampaignRegistration::new(authority(), Some(240_000));
        registry.write(&registration).unwrap();
        let path = registry.registration_path(&registration.issue_url);

        let mut old_reader: NMinusOneCampaignRegistration =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        old_reader.last_observation = Some("written-by-n-minus-one".to_owned());
        fs::write(&path, serde_json::to_vec_pretty(&old_reader).unwrap()).unwrap();

        let reupgraded = registry.read(&path).unwrap();
        assert_eq!(reupgraded.projection_wait_ms, Some(240_000));
        assert_eq!(
            reupgraded.last_observation.as_deref(),
            Some("written-by-n-minus-one")
        );
        assert_eq!(
            reupgraded.approved_graph_digest,
            registration.approved_graph_digest
        );
        assert_eq!(reupgraded.flow, registration.flow);
        assert_eq!(reupgraded.driver, registration.driver);
    }

    #[test]
    fn compatibility_decoder_rejects_every_unknown_member_beyond_the_one_pollution() {
        for include_historical_pollution in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let registry = CampaignRegistry::open(temporary.path()).unwrap();
            let expected = authority();
            let path = registry.registration_path(&expected.issue_url);
            let mut value = serde_json::to_value(&expected).unwrap();
            let object = value.as_object_mut().unwrap();
            if include_historical_pollution {
                object.insert("projectionWaitMs".to_owned(), Value::Null);
            }
            object.insert("futureAuthority".to_owned(), Value::Bool(true));
            fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

            assert!(matches!(
                registry.read(&path),
                Err(CampaignRegistryError::InvalidRegistration { .. })
            ));
        }
    }
}
