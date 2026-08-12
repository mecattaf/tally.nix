//! Durable, rollback-compatible campaign registrations.
//!
//! The authority object is deliberately frozen at schema version 2: it is the
//! exact closed shape understood by the immediately preceding tally release.
//! Host-local settings live beside it so an older binary can keep scanning the
//! `armed` directory without encountering fields it cannot decode.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::nix_store::{GcRootBackend, NixStore};

pub const REGISTRY_SCHEMA_VERSION: u32 = 2;
pub const HOST_TUNING_SCHEMA_VERSION: u32 = 1;
/// Effective host tuning when a stable-v2 authority has no tuning sidecar.
pub const DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS: u64 = 10_000;
const ASSET_MANIFEST_SCHEMA_VERSION: u32 = 1;

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

/// Private ownership metadata for one `(registrationId, armSerial)` asset
/// generation. It is intentionally not an authority extension: an N-1 reader
/// can continue to consume the exact flow and driver paths without learning
/// how the current process keeps them alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignAssetManifestV1 {
    schema_version: u32,
    flow: CampaignAssetV1,
    driver: CampaignAssetV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "camelCase")]
enum CampaignAssetV1 {
    NixStore { output: PathBuf },
    Snapshot { sha256: String, mode: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetRole {
    Flow,
    Driver,
}

impl AssetRole {
    const ALL: [Self; 2] = [Self::Flow, Self::Driver];

    const fn name(self) -> &'static str {
        match self {
            Self::Flow => "flow",
            Self::Driver => "driver",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Flow => 0,
            Self::Driver => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AssetGeneration {
    registration_id: String,
    arm_serial: u64,
}

impl AssetGeneration {
    fn from_registration(registration: &CampaignRegistration) -> Self {
        Self {
            registration_id: registration.registration_id.clone(),
            arm_serial: registration.arm_serial,
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

/// A campaign registry protected by a shared lock.
pub struct CampaignRegistry {
    state_dir: PathBuf,
    lock: File,
    gc_roots: Box<dyn GcRootBackend>,
}

impl CampaignRegistry {
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self, CampaignRegistryError> {
        Self::open_with_gc_backend(state_dir, Box::new(NixStore::default()))
    }

    pub fn open_with_gc_backend(
        state_dir: impl AsRef<Path>,
        gc_roots: Box<dyn GcRootBackend>,
    ) -> Result<Self, CampaignRegistryError> {
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
        fs2::FileExt::lock_shared(&lock).map_err(|source| io_error(&lock_path, source))?;
        Ok(Self {
            state_dir,
            lock,
            gc_roots,
        })
    }

    pub fn registration_path(&self, issue_url: &str) -> PathBuf {
        registration_path(&self.state_dir, issue_url)
    }

    pub fn read_issue(
        &self,
        issue_url: &str,
    ) -> Result<Option<CampaignRegistration>, CampaignRegistryError> {
        let path = self.registration_path(issue_url);
        Ok(self
            .registrations()?
            .into_iter()
            .find_map(|(candidate, registration)| (candidate == path).then_some(registration)))
    }

    pub fn read(&self, path: &Path) -> Result<CampaignRegistration, CampaignRegistryError> {
        let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
        let authority: CampaignRegistrationV2 = serde_json::from_slice(&bytes)
            .map_err(|source| invalid_registration(path, source.to_string()))?;
        validate_authority(path, &authority)?;
        let projection_wait_ms = self.read_host_tuning(&authority.registration_id)?;
        Ok(CampaignRegistration::new(authority, projection_wait_ms))
    }

    pub fn registrations(
        &self,
    ) -> Result<Vec<(PathBuf, CampaignRegistration)>, CampaignRegistryError> {
        let mut registrations = Vec::new();
        let mut live = BTreeSet::new();
        for path in self.authority_paths()? {
            let mut registration = self.read(&path)?;
            if self.ensure_asset_generation(&mut registration)? {
                // Asset adoption changes only the already-frozen flow/driver
                // paths. Publish that authority only after the generation is
                // complete and durable.
                atomic_write_json(&path, registration.authority())?;
            }
            live.insert(AssetGeneration::from_registration(&registration));
            registrations.push((path, registration));
        }
        self.cleanup_orphan_generations(&live)?;
        Ok(registrations)
    }

    pub fn write(
        &self,
        registration: &mut CampaignRegistration,
    ) -> Result<(), CampaignRegistryError> {
        let path = self.registration_path(&registration.issue_url);
        validate_authority(&path, registration.authority())?;
        self.ensure_asset_generation(registration)?;
        validate_authority(&path, registration.authority())?;
        self.write_host_tuning(
            &registration.registration_id,
            registration.projection_wait_ms,
        )?;
        // This rename is the publication point. Everything above can leave
        // only an unreferenced generation; everything below can leave only an
        // extra old generation. Either interruption retains every live asset.
        atomic_write_json(&path, registration.authority())?;
        let live = self.live_generations()?;
        self.cleanup_orphan_generations(&live)
    }

    pub fn remove_issue(&self, issue_url: &str) -> Result<bool, CampaignRegistryError> {
        let path = self.registration_path(issue_url);
        if !path.exists() {
            // Disarm is still a reconciliation entry for every registration
            // that remains live, even when its requested target is absent.
            self.registrations()?;
            return Ok(false);
        }
        // Deletion does not require the target asset to remain readable. This
        // is the recovery path for an irretrievably collected legacy record:
        // authority is decoded and validated, then removed before any owned
        // generation is touched.
        let registration = self.read(&path)?;
        self.remove(&registration)?;
        Ok(true)
    }

    pub fn remove(&self, registration: &CampaignRegistration) -> Result<(), CampaignRegistryError> {
        let path = self.registration_path(&registration.issue_url);
        // Delete authority first. A crash from this point can leak an orphan,
        // but can never leave live authority pointing at removed assets.
        remove_file_if_present(&path)?;
        let sidecar = host_tuning_path(&self.state_dir, &registration.registration_id);
        remove_file_if_present(&sidecar)?;
        self.remove_asset_generation(&AssetGeneration::from_registration(registration))?;
        self.registrations().map(|_| ())
    }

    fn authority_paths(&self) -> Result<Vec<PathBuf>, CampaignRegistryError> {
        let directory = authority_dir(&self.state_dir);
        let mut paths = fs::read_dir(&directory)
            .map_err(|source| io_error(&directory, source))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension() == Some(OsStr::new("json")))
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    fn live_generations(&self) -> Result<BTreeSet<AssetGeneration>, CampaignRegistryError> {
        self.authority_paths()?
            .into_iter()
            .map(|path| {
                self.read(&path)
                    .map(|registration| AssetGeneration::from_registration(&registration))
            })
            .collect()
    }

    fn ensure_asset_generation(
        &self,
        registration: &mut CampaignRegistration,
    ) -> Result<bool, CampaignRegistryError> {
        let generation = AssetGeneration::from_registration(registration);
        let directory = asset_generation_dir(&self.state_dir, &generation);
        let manifest_path = asset_manifest_path(&directory);
        if manifest_path.exists() {
            let manifest = self.read_asset_manifest(&manifest_path)?;
            return self.reconcile_manifest(registration, &directory, &manifest);
        }

        for role in AssetRole::ALL {
            let path = registration_asset(registration, role);
            if path.starts_with(&directory) {
                return Err(CampaignRegistryError::InvalidAssetManifest {
                    path: manifest_path,
                    reason: format!(
                        "{} authority points into its generation but ownership metadata is missing",
                        role.name()
                    ),
                });
            }
        }

        // A retry after a pre-publication interruption may reuse this serial.
        // Its partial directory is not authoritative and is safe to rebuild.
        self.remove_asset_generation(&generation)?;
        let snapshots = directory.join("snapshots");
        let roots = directory.join("roots");
        secure_directory(&snapshots)?;
        secure_directory(&roots)?;

        let sources = AssetRole::ALL
            .into_iter()
            .map(|role| {
                let path = registration_asset(registration, role).to_owned();
                ensure_regular_asset(registration, role, &path)?;
                let output = self.gc_roots.containing_store_output(&path);
                Ok((role, path, output))
            })
            .collect::<Result<Vec<_>, CampaignRegistryError>>()?;
        let mut entries: [Option<CampaignAssetV1>; 2] = [None, None];
        let mut authority_paths: [Option<PathBuf>; 2] = [None, None];

        // All snapshots are durable before either indirect root is made. The
        // roots are themselves created directly at their final paths.
        for (role, source, output) in &sources {
            if output.is_none() {
                let destination = snapshots.join(role.name());
                let (sha256, mode) = snapshot_asset(source, &destination)?;
                entries[role.index()] = Some(CampaignAssetV1::Snapshot { sha256, mode });
                authority_paths[role.index()] = Some(destination);
            }
        }
        for (role, source, output) in &sources {
            if let Some(output) = output {
                let root = roots.join(role.name());
                self.add_gc_root(registration, *role, &root, output)?;
                entries[role.index()] = Some(CampaignAssetV1::NixStore {
                    output: output.clone(),
                });
                authority_paths[role.index()] = Some(source.clone());
            }
        }
        sync_directory(&snapshots)?;
        sync_directory(&roots)?;

        let manifest = CampaignAssetManifestV1 {
            schema_version: ASSET_MANIFEST_SCHEMA_VERSION,
            flow: entries[AssetRole::Flow.index()]
                .take()
                .expect("flow asset was prepared"),
            driver: entries[AssetRole::Driver.index()]
                .take()
                .expect("driver asset was prepared"),
        };
        atomic_write_json(&manifest_path, &manifest)?;
        sync_directory(&directory)?;
        sync_directory(
            directory
                .parent()
                .expect("asset generations always have a registration directory"),
        )?;
        let assets = assets_dir(&self.state_dir);
        sync_directory(&assets)?;
        sync_directory(
            assets
                .parent()
                .expect("campaign assets always have a campaigns directory"),
        )?;
        let changed = registration.flow
            != *authority_paths[AssetRole::Flow.index()]
                .as_ref()
                .expect("flow authority path was prepared")
            || registration.driver
                != *authority_paths[AssetRole::Driver.index()]
                    .as_ref()
                    .expect("driver authority path was prepared");
        registration.flow = authority_paths[AssetRole::Flow.index()]
            .take()
            .expect("flow authority path was prepared");
        registration.driver = authority_paths[AssetRole::Driver.index()]
            .take()
            .expect("driver authority path was prepared");
        Ok(changed)
    }

    fn read_asset_manifest(
        &self,
        path: &Path,
    ) -> Result<CampaignAssetManifestV1, CampaignRegistryError> {
        let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
        let manifest: CampaignAssetManifestV1 =
            serde_json::from_slice(&bytes).map_err(|source| {
                CampaignRegistryError::InvalidAssetManifest {
                    path: path.to_owned(),
                    reason: source.to_string(),
                }
            })?;
        if manifest.schema_version != ASSET_MANIFEST_SCHEMA_VERSION {
            return Err(CampaignRegistryError::InvalidAssetManifest {
                path: path.to_owned(),
                reason: format!(
                    "schemaVersion must equal {ASSET_MANIFEST_SCHEMA_VERSION}, got {}",
                    manifest.schema_version
                ),
            });
        }
        Ok(manifest)
    }

    fn reconcile_manifest(
        &self,
        registration: &mut CampaignRegistration,
        directory: &Path,
        manifest: &CampaignAssetManifestV1,
    ) -> Result<bool, CampaignRegistryError> {
        let mut changed = false;
        secure_directory(&directory.join("snapshots"))?;
        secure_directory(&directory.join("roots"))?;
        for (role, entry) in [
            (AssetRole::Flow, &manifest.flow),
            (AssetRole::Driver, &manifest.driver),
        ] {
            match entry {
                CampaignAssetV1::NixStore { output } => {
                    let path = registration_asset(registration, role).to_owned();
                    if self.gc_roots.containing_store_output(&path).as_ref() != Some(output) {
                        return Err(asset_verification_error(
                            registration,
                            role,
                            &path,
                            "authority no longer belongs to the rooted store output",
                        ));
                    }
                    ensure_regular_asset(registration, role, &path)?;
                    self.add_gc_root(
                        registration,
                        role,
                        &directory.join("roots").join(role.name()),
                        output,
                    )?;
                }
                CampaignAssetV1::Snapshot { sha256, mode } => {
                    let expected = directory.join("snapshots").join(role.name());
                    verify_snapshot(registration, role, &expected, sha256, *mode)?;
                    let current = registration_asset(registration, role);
                    if current != expected {
                        if current.starts_with(directory) {
                            return Err(asset_verification_error(
                                registration,
                                role,
                                current,
                                "authority names an unexpected path inside its asset generation",
                            ));
                        }
                        *registration_asset_mut(registration, role) = expected;
                        changed = true;
                    }
                }
            }
        }
        sync_directory(&directory.join("roots"))?;
        Ok(changed)
    }

    fn add_gc_root(
        &self,
        registration: &CampaignRegistration,
        role: AssetRole,
        link: &Path,
        target: &Path,
    ) -> Result<(), CampaignRegistryError> {
        self.gc_roots
            .add_root(link, target)
            .map_err(|reason| CampaignRegistryError::GcRoot {
                registration_id: registration.registration_id.clone(),
                arm_serial: registration.arm_serial,
                role: role.name(),
                link: link.to_owned(),
                target: target.to_owned(),
                reason,
            })?;
        let parent = link.parent().expect("asset roots always have a parent");
        sync_directory(parent)
    }

    fn cleanup_orphan_generations(
        &self,
        live: &BTreeSet<AssetGeneration>,
    ) -> Result<(), CampaignRegistryError> {
        let assets = assets_dir(&self.state_dir);
        if !assets.exists() {
            return Ok(());
        }
        for registration_entry in
            fs::read_dir(&assets).map_err(|source| io_error(&assets, source))?
        {
            let registration_entry =
                registration_entry.map_err(|source| io_error(&assets, source))?;
            if !registration_entry
                .file_type()
                .map_err(|source| io_error(&registration_entry.path(), source))?
                .is_dir()
            {
                continue;
            }
            let Some(registration_id) = registration_entry.file_name().to_str().map(str::to_owned)
            else {
                continue;
            };
            if uuid::Uuid::parse_str(&registration_id).is_err() {
                continue;
            }
            let registration_directory = registration_entry.path();
            for serial_entry in fs::read_dir(&registration_directory)
                .map_err(|source| io_error(&registration_directory, source))?
            {
                let serial_entry =
                    serial_entry.map_err(|source| io_error(&registration_directory, source))?;
                if !serial_entry
                    .file_type()
                    .map_err(|source| io_error(&serial_entry.path(), source))?
                    .is_dir()
                {
                    continue;
                }
                let Some(arm_serial) = serial_entry
                    .file_name()
                    .to_str()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0)
                else {
                    continue;
                };
                let generation = AssetGeneration {
                    registration_id: registration_id.clone(),
                    arm_serial,
                };
                if !live.contains(&generation) {
                    self.remove_asset_generation(&generation)?;
                }
            }
            if fs::read_dir(&registration_directory)
                .map_err(|source| io_error(&registration_directory, source))?
                .next()
                .is_none()
            {
                fs::remove_dir(&registration_directory)
                    .map_err(|source| io_error(&registration_directory, source))?;
                sync_directory(&assets)?;
            }
        }
        Ok(())
    }

    fn remove_asset_generation(
        &self,
        generation: &AssetGeneration,
    ) -> Result<(), CampaignRegistryError> {
        let directory = asset_generation_dir(&self.state_dir, generation);
        for role in AssetRole::ALL {
            let link = directory.join("roots").join(role.name());
            self.gc_roots.remove_root(&link).map_err(|reason| {
                CampaignRegistryError::GcRootRemoval {
                    registration_id: generation.registration_id.clone(),
                    arm_serial: generation.arm_serial,
                    role: role.name(),
                    link,
                    reason,
                }
            })?;
        }
        if !directory.exists() {
            return Ok(());
        }
        fs::remove_dir_all(&directory).map_err(|source| io_error(&directory, source))?;
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
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
    #[error("invalid campaign asset manifest {path}: {reason}")]
    InvalidAssetManifest { path: PathBuf, reason: String },
    #[error(
        "campaign {role} asset is missing for registration {registration_id} arm {arm_serial}: {path}"
    )]
    MissingAsset {
        registration_id: String,
        arm_serial: u64,
        role: &'static str,
        path: PathBuf,
    },
    #[error(
        "campaign {role} asset verification failed for registration {registration_id} arm {arm_serial} at {path}: {reason}"
    )]
    AssetVerification {
        registration_id: String,
        arm_serial: u64,
        role: &'static str,
        path: PathBuf,
        reason: String,
    },
    #[error(
        "cannot create campaign {role} GC root for registration {registration_id} arm {arm_serial}: {link} -> {target}: {reason}"
    )]
    GcRoot {
        registration_id: String,
        arm_serial: u64,
        role: &'static str,
        link: PathBuf,
        target: PathBuf,
        reason: String,
    },
    #[error(
        "cannot remove campaign {role} GC root for registration {registration_id} arm {arm_serial} at {link}: {reason}"
    )]
    GcRootRemoval {
        registration_id: String,
        arm_serial: u64,
        role: &'static str,
        link: PathBuf,
        reason: String,
    },
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

fn assets_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("campaigns/assets")
}

fn asset_generation_dir(state_dir: &Path, generation: &AssetGeneration) -> PathBuf {
    assets_dir(state_dir)
        .join(&generation.registration_id)
        .join(generation.arm_serial.to_string())
}

fn asset_manifest_path(generation_dir: &Path) -> PathBuf {
    generation_dir.join("assets-v1.json")
}

fn registration_path(state_dir: &Path, issue_url: &str) -> PathBuf {
    let digest = Sha256::digest(issue_url.as_bytes());
    authority_dir(state_dir).join(format!("{digest:x}.json"))
}

fn host_tuning_path(state_dir: &Path, registration_id: &str) -> PathBuf {
    host_tuning_dir(state_dir).join(format!("{registration_id}.host-v1.json"))
}

fn registration_asset(registration: &CampaignRegistration, role: AssetRole) -> &Path {
    match role {
        AssetRole::Flow => &registration.flow,
        AssetRole::Driver => &registration.driver,
    }
}

fn registration_asset_mut(
    registration: &mut CampaignRegistration,
    role: AssetRole,
) -> &mut PathBuf {
    match role {
        AssetRole::Flow => &mut registration.flow,
        AssetRole::Driver => &mut registration.driver,
    }
}

fn ensure_regular_asset(
    registration: &CampaignRegistration,
    role: AssetRole,
    path: &Path,
) -> Result<fs::Metadata, CampaignRegistryError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata),
        Ok(_) => Err(missing_asset_error(registration, role, path)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(missing_asset_error(registration, role, path))
        }
        Err(source) => Err(io_error(path, source)),
    }
}

fn snapshot_asset(
    source: &Path,
    destination: &Path,
) -> Result<(String, u32), CampaignRegistryError> {
    let parent = destination
        .parent()
        .expect("campaign snapshot paths always have a parent");
    secure_directory(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        destination
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("asset"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| {
        let source_file =
            File::open(source).map_err(|source_error| io_error(source, source_error))?;
        let source_metadata = source_file
            .metadata()
            .map_err(|source_error| io_error(source, source_error))?;
        if !source_metadata.file_type().is_file() {
            return Err(CampaignRegistryError::Io {
                path: source.to_owned(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "campaign snapshot source is not a regular file",
                ),
            });
        }
        // Registry snapshots are owner-immutable. Read and execute bits are
        // retained exactly, including the driver's executable mode.
        let mode = source_metadata.permissions().mode() & 0o555;
        let mut reader = std::io::BufReader::new(source_file);
        let mut writer = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source_error| io_error(&temporary, source_error))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|source_error| io_error(source, source_error))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
            writer
                .write_all(&buffer[..read])
                .map_err(|source_error| io_error(&temporary, source_error))?;
        }
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
            .map_err(|source_error| io_error(&temporary, source_error))?;
        writer
            .sync_all()
            .map_err(|source_error| io_error(&temporary, source_error))?;
        fs::rename(&temporary, destination)
            .map_err(|source_error| io_error(destination, source_error))?;
        sync_directory(parent)?;
        Ok((format!("sha256:{:x}", digest.finalize()), mode))
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn verify_snapshot(
    registration: &CampaignRegistration,
    role: AssetRole,
    path: &Path,
    expected_sha256: &str,
    expected_mode: u32,
) -> Result<(), CampaignRegistryError> {
    let metadata = ensure_regular_asset(registration, role, path)?;
    let actual_mode = metadata.permissions().mode() & 0o777;
    if actual_mode != expected_mode {
        return Err(asset_verification_error(
            registration,
            role,
            path,
            format!("mode changed from {expected_mode:#05o} to {actual_mode:#05o}"),
        ));
    }
    let actual_sha256 = sha256_file(path)?;
    if actual_sha256 != expected_sha256 {
        return Err(asset_verification_error(
            registration,
            role,
            path,
            format!("content hash changed from {expected_sha256} to {actual_sha256}"),
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, CampaignRegistryError> {
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut reader = std::io::BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn missing_asset_error(
    registration: &CampaignRegistration,
    role: AssetRole,
    path: &Path,
) -> CampaignRegistryError {
    CampaignRegistryError::MissingAsset {
        registration_id: registration.registration_id.clone(),
        arm_serial: registration.arm_serial,
        role: role.name(),
        path: path.to_owned(),
    }
}

fn asset_verification_error(
    registration: &CampaignRegistration,
    role: AssetRole,
    path: &Path,
    reason: impl Into<String>,
) -> CampaignRegistryError {
    CampaignRegistryError::AssetVerification {
        registration_id: registration.registration_id.clone(),
        arm_serial: registration.arm_serial,
        role: role.name(),
        path: path.to_owned(),
        reason: reason.into(),
    }
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    #[derive(Debug, Default)]
    struct FakeStoreState {
        roots: BTreeMap<PathBuf, PathBuf>,
    }

    #[derive(Debug, Clone)]
    struct FakeStoreBackend {
        store_dir: PathBuf,
        state: Rc<RefCell<FakeStoreState>>,
    }

    impl FakeStoreBackend {
        fn new(store_dir: PathBuf) -> Self {
            fs::create_dir_all(&store_dir).unwrap();
            Self {
                store_dir,
                state: Rc::new(RefCell::new(FakeStoreState::default())),
            }
        }

        fn asset(&self, output: &str, relative: &str, contents: &str, mode: u32) -> PathBuf {
            let path = self.store_dir.join(output).join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            path
        }

        fn roots(&self) -> BTreeMap<PathBuf, PathBuf> {
            self.state.borrow().roots.clone()
        }

        fn forget_root(&self, link: &Path) {
            self.state.borrow_mut().roots.remove(link);
        }
    }

    impl GcRootBackend for FakeStoreBackend {
        fn containing_store_output(&self, path: &Path) -> Option<PathBuf> {
            let relative = path.strip_prefix(&self.store_dir).ok()?;
            let mut components = relative.components();
            let std::path::Component::Normal(output) = components.next()? else {
                return None;
            };
            if components.any(|component| !matches!(component, std::path::Component::Normal(_))) {
                return None;
            }
            Some(self.store_dir.join(output))
        }

        fn add_root(&self, link: &Path, target: &Path) -> Result<(), String> {
            self.state
                .borrow_mut()
                .roots
                .insert(link.to_owned(), target.to_owned());
            Ok(())
        }

        fn remove_root(&self, link: &Path) -> Result<(), String> {
            self.state.borrow_mut().roots.remove(link);
            Ok(())
        }

        fn collect_garbage(&self) -> Result<(), String> {
            let retained = self
                .state
                .borrow()
                .roots
                .values()
                .cloned()
                .collect::<BTreeSet<_>>();
            for entry in fs::read_dir(&self.store_dir).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                if entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                    && !retained.contains(&entry.path())
                {
                    fs::remove_dir_all(entry.path()).map_err(|error| error.to_string())?;
                }
            }
            Ok(())
        }
    }

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

    fn authority_with_assets(root: &Path) -> CampaignRegistrationV2 {
        let flow = root.join("source-flow.js");
        let driver = root.join("source-driver");
        fs::write(&flow, "flow-447\n").unwrap();
        fs::write(&driver, "driver-447\n").unwrap();
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
        CampaignRegistrationV2 {
            flow,
            driver,
            ..authority()
        }
    }

    fn registration_with_assets(flow: PathBuf, driver: PathBuf) -> CampaignRegistration {
        let mut value = authority();
        value.registration_id = "0198a62b-41ee-7000-8000-000000000448".to_owned();
        value.issue_url = "https://github.com/mecattaf/tally.nix/issues/448".to_owned();
        value.issue_number = 448;
        value.arm_serial = 1;
        value.flow = flow;
        value.driver = driver;
        CampaignRegistration::new(value, Some(240_000))
    }

    fn local_assets(root: &Path, suffix: &str) -> (PathBuf, PathBuf) {
        let flow = root.join(format!("flow-{suffix}.js"));
        let driver = root.join(format!("driver-{suffix}"));
        fs::write(&flow, format!("flow:{suffix}\n")).unwrap();
        fs::write(&driver, format!("driver:{suffix}\n")).unwrap();
        fs::set_permissions(&flow, fs::Permissions::from_mode(0o640)).unwrap();
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o751)).unwrap();
        (flow, driver)
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
            let mut registration = CampaignRegistration::new(
                authority_with_assets(temporary.path()),
                projection_wait_ms,
            );
            registry.write(&mut registration).unwrap();

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
    fn armed_registration_restart_recovery_preserves_arm_attempt_counter() {
        let temporary = tempfile::tempdir().unwrap();
        let state_dir = temporary.path().join("state");
        let source_dir = temporary.path().join("sources");
        fs::create_dir_all(&source_dir).unwrap();
        let (flow, driver) = local_assets(&source_dir, "restart");
        let expected_arm_attempt = 7;
        let expected_registration_id;
        let expected_issue_url;

        {
            let registry = CampaignRegistry::open(&state_dir).unwrap();
            let mut registration = registration_with_assets(flow, driver);
            registration.arm_serial = expected_arm_attempt;
            expected_registration_id = registration.registration_id.clone();
            expected_issue_url = registration.issue_url.clone();
            registry.write(&mut registration).unwrap();
        }

        let restarted = CampaignRegistry::open(&state_dir).unwrap();
        let registrations = restarted.registrations().unwrap();
        assert_eq!(
            registrations.len(),
            1,
            "restart must not duplicate dispatch authority"
        );
        let recovered = &registrations[0].1;
        assert_eq!(recovered.registration_id, expected_registration_id);
        assert_eq!(recovered.issue_url, expected_issue_url);
        assert_eq!(
            recovered.arm_serial, expected_arm_attempt,
            "the armed registration's attempt counter is durable coordinator state"
        );
        assert_eq!(
            restarted
                .read_issue(&expected_issue_url)
                .unwrap()
                .unwrap()
                .arm_serial,
            expected_arm_attempt,
            "repeated recovery reads must not advance the counter"
        );
    }

    #[test]
    fn downgrade_rewrite_keeps_sidecar_override_for_reupgrade() {
        let temporary = tempfile::tempdir().unwrap();
        let registry = CampaignRegistry::open(temporary.path()).unwrap();
        let mut registration =
            CampaignRegistration::new(authority_with_assets(temporary.path()), Some(240_000));
        registry.write(&mut registration).unwrap();
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
    fn store_assets_keep_exact_subpaths_alive_and_reconcile_both_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let backend = FakeStoreBackend::new(temporary.path().join("fake-store"));
        let flow = backend.asset("flow-output-v1", "share/spec-build.js", "flow:v1\n", 0o444);
        let driver = backend.asset(
            "driver-output-v1",
            "bin/spec-build-driver",
            "driver:v1\n",
            0o555,
        );
        let unrelated = backend.asset("unrelated-output", "bin/tool", "unrooted\n", 0o555);
        let registry = CampaignRegistry::open_with_gc_backend(
            temporary.path().join("state"),
            Box::new(backend.clone()),
        )
        .unwrap();
        let mut registration = registration_with_assets(flow.clone(), driver.clone());
        registry.write(&mut registration).unwrap();

        assert_eq!(registration.flow, flow);
        assert_eq!(registration.driver, driver);
        let authority: NMinusOneCampaignRegistration = serde_json::from_slice(
            &fs::read(registry.registration_path(&registration.issue_url)).unwrap(),
        )
        .unwrap();
        assert_eq!(authority.flow, flow);
        assert_eq!(authority.driver, driver);
        let generation = AssetGeneration::from_registration(&registration);
        let generation_dir = asset_generation_dir(&registry.state_dir, &generation);
        assert_eq!(
            backend.roots(),
            BTreeMap::from([
                (
                    generation_dir.join("roots/driver"),
                    backend.store_dir.join("driver-output-v1"),
                ),
                (
                    generation_dir.join("roots/flow"),
                    backend.store_dir.join("flow-output-v1"),
                ),
            ])
        );

        backend.collect_garbage().unwrap();
        assert!(!unrelated.exists());
        assert_eq!(fs::read_to_string(&flow).unwrap(), "flow:v1\n");
        assert_eq!(fs::read_to_string(&driver).unwrap(), "driver:v1\n");

        // A missing indirect link is repairable while the output still
        // exists; list and poll both enter through this reconciliation.
        backend.forget_root(&generation_dir.join("roots/flow"));
        let registrations = registry.registrations().unwrap();
        assert_eq!(registrations[0].1.flow, flow);
        assert_eq!(registrations[0].1.driver, driver);
        assert_eq!(backend.roots().len(), 2);
    }

    #[test]
    fn omitting_either_store_root_loses_that_asset_under_collection() {
        for missing_role in AssetRole::ALL {
            let temporary = tempfile::tempdir().unwrap();
            let backend = FakeStoreBackend::new(temporary.path().join("fake-store"));
            let flow = backend.asset("flow-output", "share/flow.js", "flow\n", 0o444);
            let driver = backend.asset("driver-output", "bin/driver", "driver\n", 0o555);
            let registry = CampaignRegistry::open_with_gc_backend(
                temporary.path().join("state"),
                Box::new(backend.clone()),
            )
            .unwrap();
            let mut registration = registration_with_assets(flow, driver);
            registry.write(&mut registration).unwrap();
            let generation_dir = asset_generation_dir(
                &registry.state_dir,
                &AssetGeneration::from_registration(&registration),
            );
            backend.forget_root(&generation_dir.join("roots").join(missing_role.name()));
            backend.collect_garbage().unwrap();

            assert!(matches!(
                registry.registrations(),
                Err(CampaignRegistryError::MissingAsset { role, .. }) if role == missing_role.name()
            ));
        }
    }

    #[test]
    fn non_store_assets_are_immutable_verified_snapshots() {
        for removed_role in AssetRole::ALL {
            let temporary = tempfile::tempdir().unwrap();
            let source_dir = temporary.path().join("sources");
            fs::create_dir_all(&source_dir).unwrap();
            let (flow, driver) = local_assets(&source_dir, removed_role.name());
            let registry = CampaignRegistry::open(temporary.path().join("state")).unwrap();
            let mut registration = registration_with_assets(flow.clone(), driver.clone());
            registry.write(&mut registration).unwrap();
            fs::remove_file(flow).unwrap();
            fs::remove_file(driver).unwrap();

            let generation_dir = asset_generation_dir(
                &registry.state_dir,
                &AssetGeneration::from_registration(&registration),
            );
            assert_eq!(registration.flow, generation_dir.join("snapshots/flow"));
            assert_eq!(registration.driver, generation_dir.join("snapshots/driver"));
            assert_eq!(
                fs::read_to_string(&registration.flow).unwrap(),
                format!("flow:{}\n", removed_role.name())
            );
            assert_eq!(
                fs::read_to_string(&registration.driver).unwrap(),
                format!("driver:{}\n", removed_role.name())
            );
            assert_eq!(
                fs::metadata(&registration.flow)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o440
            );
            assert_eq!(
                fs::metadata(&registration.driver)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o551
            );
            registry.registrations().unwrap();

            fs::remove_file(registration_asset(&registration, removed_role)).unwrap();
            assert!(matches!(
                registry.registrations(),
                Err(CampaignRegistryError::MissingAsset { role, .. }) if role == removed_role.name()
            ));
        }
    }

    #[test]
    fn snapshot_mode_and_hash_are_both_verified() {
        let temporary = tempfile::tempdir().unwrap();
        let source_dir = temporary.path().join("sources");
        fs::create_dir_all(&source_dir).unwrap();
        let (flow, driver) = local_assets(&source_dir, "verified");
        let registry = CampaignRegistry::open(temporary.path().join("state")).unwrap();
        let mut registration = registration_with_assets(flow, driver);
        registry.write(&mut registration).unwrap();

        fs::set_permissions(&registration.flow, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            registry.registrations(),
            Err(CampaignRegistryError::AssetVerification { role: "flow", reason, .. })
                if reason.contains("mode changed")
        ));

        fs::write(&registration.flow, "tampered flow\n").unwrap();
        fs::set_permissions(&registration.flow, fs::Permissions::from_mode(0o440)).unwrap();
        assert!(matches!(
            registry.registrations(),
            Err(CampaignRegistryError::AssetVerification { role: "flow", reason, .. })
                if reason.contains("content hash changed")
        ));
    }

    #[test]
    fn rearm_and_both_interruption_windows_reconcile_safely() {
        let temporary = tempfile::tempdir().unwrap();
        let source_dir = temporary.path().join("sources");
        fs::create_dir_all(&source_dir).unwrap();
        let registry = CampaignRegistry::open(temporary.path().join("state")).unwrap();
        let (flow_v1, driver_v1) = local_assets(&source_dir, "v1");
        let mut current = registration_with_assets(flow_v1, driver_v1);
        registry.write(&mut current).unwrap();
        let generation_v1 = AssetGeneration::from_registration(&current);
        let generation_v1_dir = asset_generation_dir(&registry.state_dir, &generation_v1);

        let (flow_v2, driver_v2) = local_assets(&source_dir, "v2");
        let mut next = current.clone();
        next.arm_serial = 2;
        next.flow = flow_v2.clone();
        next.driver = driver_v2.clone();

        // Crash before authority publication: only the unreferenced final
        // generation remains, and the next entry removes it.
        registry.ensure_asset_generation(&mut next).unwrap();
        let generation_v2 = AssetGeneration::from_registration(&next);
        let generation_v2_dir = asset_generation_dir(&registry.state_dir, &generation_v2);
        assert!(generation_v1_dir.is_dir());
        assert!(generation_v2_dir.is_dir());
        let reconciled = registry.registrations().unwrap();
        assert_eq!(reconciled[0].1.arm_serial, 1);
        assert!(generation_v1_dir.is_dir());
        assert!(!generation_v2_dir.exists());

        // Recreate the prepared generation, publish authority, and simulate a
        // crash before old-generation cleanup. The next entry retains v2 and
        // removes only the now-unreferenced v1 generation.
        next.flow = flow_v2;
        next.driver = driver_v2;
        registry.ensure_asset_generation(&mut next).unwrap();
        atomic_write_json(
            &registry.registration_path(&next.issue_url),
            next.authority(),
        )
        .unwrap();
        assert!(generation_v1_dir.is_dir());
        assert!(generation_v2_dir.is_dir());
        let reconciled = registry.registrations().unwrap();
        assert_eq!(reconciled[0].1.arm_serial, 2);
        assert_eq!(
            fs::read_to_string(&reconciled[0].1.flow).unwrap(),
            "flow:v2\n"
        );
        assert_eq!(
            fs::read_to_string(&reconciled[0].1.driver).unwrap(),
            "driver:v2\n"
        );
        assert!(!generation_v1_dir.exists());
        assert!(generation_v2_dir.is_dir());

        // A normal re-arm performs the same final cleanup in the write call.
        let (flow_v3, driver_v3) = local_assets(&source_dir, "v3");
        let mut final_registration = reconciled[0].1.clone();
        final_registration.arm_serial = 3;
        final_registration.flow = flow_v3;
        final_registration.driver = driver_v3;
        registry.write(&mut final_registration).unwrap();
        assert!(!generation_v2_dir.exists());
        assert_eq!(
            fs::read_to_string(&final_registration.flow).unwrap(),
            "flow:v3\n"
        );
        assert_eq!(
            fs::read_to_string(&final_registration.driver).unwrap(),
            "driver:v3\n"
        );
    }

    #[test]
    fn legacy_assets_are_adopted_and_missing_legacy_assets_are_typed() {
        let temporary = tempfile::tempdir().unwrap();
        let source_dir = temporary.path().join("sources");
        fs::create_dir_all(&source_dir).unwrap();
        let registry = CampaignRegistry::open(temporary.path().join("state")).unwrap();
        let (flow, driver) = local_assets(&source_dir, "legacy");
        let legacy = registration_with_assets(flow.clone(), driver.clone());
        let path = registry.registration_path(&legacy.issue_url);
        atomic_write_json(&path, legacy.authority()).unwrap();

        let adopted = registry.registrations().unwrap().remove(0).1;
        assert_ne!(adopted.flow, flow);
        assert_ne!(adopted.driver, driver);
        assert_eq!(fs::read_to_string(&adopted.flow).unwrap(), "flow:legacy\n");
        assert_eq!(
            fs::read_to_string(&adopted.driver).unwrap(),
            "driver:legacy\n"
        );

        registry.remove(&adopted).unwrap();
        let (flow, driver) = local_assets(&source_dir, "collected");
        let missing = registration_with_assets(flow.clone(), driver);
        atomic_write_json(&path, missing.authority()).unwrap();
        fs::remove_file(flow).unwrap();
        assert!(matches!(
            registry.registrations(),
            Err(CampaignRegistryError::MissingAsset { role: "flow", .. })
        ));
        assert!(registry.remove_issue(&missing.issue_url).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn disarm_and_closed_prune_remove_authority_sidecars_assets_and_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let backend = FakeStoreBackend::new(temporary.path().join("fake-store"));
        let registry = CampaignRegistry::open_with_gc_backend(
            temporary.path().join("state"),
            Box::new(backend.clone()),
        )
        .unwrap();

        let flow = backend.asset("flow-disarm", "share/flow.js", "flow-disarm\n", 0o444);
        let driver = backend.asset("driver-disarm", "bin/driver", "driver-disarm\n", 0o555);
        let mut disarmed = registration_with_assets(flow, driver);
        registry.write(&mut disarmed).unwrap();
        let disarmed_generation = asset_generation_dir(
            &registry.state_dir,
            &AssetGeneration::from_registration(&disarmed),
        );
        assert!(registry.remove_issue(&disarmed.issue_url).unwrap());
        assert!(!registry.registration_path(&disarmed.issue_url).exists());
        assert!(!host_tuning_path(&registry.state_dir, &disarmed.registration_id).exists());
        assert!(!disarmed_generation.exists());
        assert!(backend.roots().is_empty());

        let flow = backend.asset("flow-prune", "share/flow.js", "flow-prune\n", 0o444);
        let driver = backend.asset("driver-prune", "bin/driver", "driver-prune\n", 0o555);
        let mut pruned = registration_with_assets(flow, driver);
        pruned.registration_id = "0198a62b-41ee-7000-8000-000000000449".to_owned();
        pruned.issue_url = "https://github.com/mecattaf/tally.nix/issues/449".to_owned();
        pruned.issue_number = 449;
        registry.write(&mut pruned).unwrap();
        let pruned_generation = asset_generation_dir(
            &registry.state_dir,
            &AssetGeneration::from_registration(&pruned),
        );
        // This is the lifecycle operation used by the closed-issue branch in
        // `campaign poll`; it deliberately shares disarm's ordering.
        registry.remove(&pruned).unwrap();
        assert!(!registry.registration_path(&pruned.issue_url).exists());
        assert!(!host_tuning_path(&registry.state_dir, &pruned.registration_id).exists());
        assert!(!pruned_generation.exists());
        assert!(backend.roots().is_empty());
    }
}
