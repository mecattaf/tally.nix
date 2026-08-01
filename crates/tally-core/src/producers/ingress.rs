use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressClaim {
    pub path: PathBuf,
    pub original_name: String,
    pub ingress_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressOutcome {
    pub file: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_to: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub fn claim_ingress_files(events_dir: &Path) -> Result<Vec<IngressClaim>, ProducerError> {
    create_ingress_dirs(events_dir)?;
    let _ingress_lock = lock_ingress(events_dir)?;
    let processing = events_dir.join("processing");
    let mut claims = existing_claims(&processing)?;
    let mut candidates = std::fs::read_dir(events_dir)
        .map_err(|source| ProducerError::Io {
            path: events_dir.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ProducerError::Io {
            path: events_dir.to_owned(),
            source,
        })?;
    candidates.sort_by_key(std::fs::DirEntry::file_name);
    for entry in candidates {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_ingress_candidate(&name) {
            continue;
        }
        let source_path = entry.path();
        if name.len() > MAX_CLAIMABLE_NAME_BYTES {
            let rejected_base = events_dir
                .join("rejected")
                .join(format!("overlong-{}.json", stable_key(&[&name])));
            rename_unique(&source_path, &rejected_base)?;
            sync_directory(&events_dir.join("rejected"))?;
            sync_directory(events_dir)?;
            continue;
        }
        let metadata =
            std::fs::symlink_metadata(&source_path).map_err(|source| ProducerError::Io {
                path: source_path.clone(),
                source,
            })?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            continue;
        }
        let ingress_id = Uuid::new_v4().to_string();
        let claimed_name = format!("{ingress_id}--{name}");
        let claimed_path = processing.join(&claimed_name);
        match std::fs::rename(&source_path, &claimed_path) {
            Ok(()) => {
                sync_directory(&processing)?;
                sync_directory(events_dir)?;
                claims.push(IngressClaim {
                    path: claimed_path,
                    original_name: name,
                    ingress_id,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ProducerError::Io {
                    path: source_path,
                    source,
                })
            }
        }
    }
    claims.sort_by(|left, right| {
        left.original_name
            .cmp(&right.original_name)
            .then_with(|| left.ingress_id.cmp(&right.ingress_id))
    });
    Ok(claims)
}

pub fn read_ingress_payload(claim: &IngressClaim) -> Result<EnqueuePayload, ProducerError> {
    let bytes = read_bounded_regular(&claim.path, MAX_INGRESS_BYTES)?;
    serde_json::from_slice(&bytes).map_err(ProducerError::Json)
}

pub fn acknowledged_ingress_ids(events_dir: &Path) -> Result<BTreeSet<String>, ProducerError> {
    Ok(read_acknowledged_events(events_dir)?
        .iter()
        .filter_map(|event| event.ingress_id.clone())
        .collect())
}

/// Returns brief files named by producer ingress that has not yet been
/// archived. The caller must hold the producer ingress lock while using this
/// snapshot for retention decisions.
pub(crate) fn pending_ingress_brief_paths(
    events_dir: &Path,
) -> Result<BTreeSet<PathBuf>, ProducerError> {
    let mut paths = Vec::new();
    if events_dir.is_dir() {
        let entries = std::fs::read_dir(events_dir)
            .map_err(|source| ProducerError::Io {
                path: events_dir.to_owned(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| ProducerError::Io {
                path: events_dir.to_owned(),
                source,
            })?;
        for entry in entries {
            let path = entry.path();
            if entry.file_name().to_str().is_some_and(is_ingress_candidate) {
                paths.push(path);
            }
        }
        let processing = events_dir.join("processing");
        if processing.is_dir() {
            paths.extend(
                existing_claims(&processing)?
                    .into_iter()
                    .map(|claim| claim.path),
            );
        }
    }

    let mut briefs = BTreeSet::new();
    for path in paths {
        let bytes = match read_bounded_regular(&path, MAX_INGRESS_BYTES) {
            Ok(bytes) => bytes,
            Err(error @ ProducerError::Io { .. }) => return Err(error),
            Err(_) => continue,
        };
        let Ok(payload) = serde_json::from_slice::<EnqueuePayload>(&bytes) else {
            continue;
        };
        if let Some(path) = payload.brief_path {
            briefs.insert(path);
        }
    }
    Ok(briefs)
}

pub fn archive_ingress_claim(
    events_dir: &Path,
    claim: &IngressClaim,
    accepted: bool,
) -> Result<PathBuf, ProducerError> {
    let destination_dir = events_dir.join(if accepted { "done" } else { "rejected" });
    create_ingress_dirs(events_dir)?;
    let _ingress_lock = lock_ingress(events_dir)?;
    let destination = rename_unique(&claim.path, &destination_dir.join(&claim.original_name))?;
    sync_directory(&destination_dir)?;
    sync_directory(&events_dir.join("processing"))?;
    Ok(destination)
}

pub(super) fn rename_unique(
    source: &Path,
    destination_base: &Path,
) -> Result<PathBuf, ProducerError> {
    let mut destination = destination_base.to_owned();
    let mut suffix = 1_u64;
    while !rename_noreplace(source, &destination)? {
        let file_name = destination_base
            .file_name()
            .ok_or_else(|| {
                ProducerError::InvalidObservation(format!(
                    "archive path {} has no file name",
                    destination_base.display()
                ))
            })?
            .to_string_lossy();
        destination = destination_base.with_file_name(format!("{file_name}.{suffix}"));
        suffix = suffix.checked_add(1).ok_or_else(|| {
            ProducerError::InvalidObservation("ingress archive suffix overflow".to_owned())
        })?;
    }
    Ok(destination)
}

pub(super) fn rename_noreplace(source: &Path, destination: &Path) -> Result<bool, ProducerError> {
    let source_c = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        ProducerError::InvalidObservation(format!(
            "source path {} contains an interior NUL",
            source.display()
        ))
    })?;
    let destination_c = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        ProducerError::InvalidObservation(format!(
            "destination path {} contains an interior NUL",
            destination.display()
        ))
    })?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let source_error = std::io::Error::last_os_error();
    if source_error.raw_os_error() == Some(libc::EEXIST) {
        Ok(false)
    } else {
        Err(ProducerError::Io {
            path: source.to_owned(),
            source: source_error,
        })
    }
}

pub(super) fn is_ingress_candidate(name: &str) -> bool {
    !name.starts_with('.') && name.ends_with(".json") && !name.ends_with(".enqueue.json")
}

pub(super) fn existing_claims(processing: &Path) -> Result<Vec<IngressClaim>, ProducerError> {
    let mut claims = Vec::new();
    let mut entries = std::fs::read_dir(processing)
        .map_err(|source| ProducerError::Io {
            path: processing.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ProducerError::Io {
            path: processing.to_owned(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some((ingress_id, original_name)) = name.split_once("--") else {
            continue;
        };
        if Uuid::parse_str(ingress_id).is_err() || !is_ingress_candidate(original_name) {
            continue;
        }
        claims.push(IngressClaim {
            path: entry.path(),
            original_name: original_name.to_owned(),
            ingress_id: ingress_id.to_owned(),
        });
    }
    Ok(claims)
}

pub(super) fn create_ingress_dirs(events_dir: &Path) -> Result<(), ProducerError> {
    create_dir_durable(events_dir)?;
    for name in ["processing", "done", "rejected"] {
        create_dir_durable(&events_dir.join(name))?;
    }
    Ok(())
}

/// Guards every rename into and out of the ingress directories, including the
/// retention sweep that prunes `done`/`rejected`.
pub const INGRESS_LOCK_FILE_NAME: &str = ".producer-ingress.lock";

pub(super) fn lock_ingress(events_dir: &Path) -> Result<File, ProducerError> {
    create_dir_durable(events_dir)?;
    let path = events_dir.join(INGRESS_LOCK_FILE_NAME);
    let lock = open_private_rw(&path)?;
    lock.lock_exclusive()
        .map_err(|source| ProducerError::Io { path, source })?;
    Ok(lock)
}

pub(super) fn ingress_name_exists(events_dir: &Path, name: &str) -> Result<bool, ProducerError> {
    for directory in ["", "done", "rejected"] {
        let path = if directory.is_empty() {
            events_dir.join(name)
        } else {
            events_dir.join(directory).join(name)
        };
        if path_lexists(&path)? {
            return Ok(true);
        }
    }
    let processing = events_dir.join("processing");
    if processing.exists() {
        for entry in std::fs::read_dir(&processing).map_err(|source| ProducerError::Io {
            path: processing.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| ProducerError::Io {
                path: processing.clone(),
                source,
            })?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|candidate| candidate.ends_with(&format!("--{name}")))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(super) fn path_lexists(path: &Path) -> Result<bool, ProducerError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(ProducerError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

pub(super) fn create_dir_durable(path: &Path) -> Result<(), ProducerError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => {
            return Err(ProducerError::InvalidObservation(format!(
                "{} is not a real directory",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ProducerError::Io {
                path: path.to_owned(),
                source,
            })
        }
    }
    let parent = path.parent().ok_or_else(|| {
        ProducerError::InvalidObservation(format!("directory {} has no parent", path.display()))
    })?;
    create_dir_durable(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path).map_err(|source| ProducerError::Io {
                path: path.to_owned(),
                source,
            })?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(ProducerError::InvalidObservation(format!(
                    "{} is not a real directory",
                    path.display()
                )));
            }
        }
        Err(source) => {
            return Err(ProducerError::Io {
                path: path.to_owned(),
                source,
            })
        }
    }
    sync_directory(parent)
}

pub(super) fn write_new_atomic(path: &Path, bytes: &[u8]) -> Result<bool, ProducerError> {
    let parent = path.parent().ok_or_else(|| {
        ProducerError::InvalidObservation(format!("path {} has no parent", path.display()))
    })?;
    create_dir_durable(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ProducerError::InvalidObservation(format!(
                "path {} has a non-Unicode file name",
                path.display()
            ))
        })?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| ProducerError::Io {
            path: temporary.clone(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.write_all(b"\n").map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    let linked = match std::fs::hard_link(&temporary, path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(source) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(ProducerError::Io {
                path: path.to_owned(),
                source,
            });
        }
    };
    std::fs::remove_file(&temporary).map_err(|source| ProducerError::Io {
        path: temporary,
        source,
    })?;
    sync_directory(parent)?;
    Ok(linked)
}

pub(super) fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), ProducerError> {
    let parent = path.parent().ok_or_else(|| {
        ProducerError::InvalidObservation(format!("path {} has no parent", path.display()))
    })?;
    create_dir_durable(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| ProducerError::Io {
            path: temporary.clone(),
            source,
        })?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n").map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    file.sync_all().map_err(|source| ProducerError::Io {
        path: temporary.clone(),
        source,
    })?;
    std::fs::rename(&temporary, path).map_err(|source| ProducerError::Io {
        path: path.to_owned(),
        source,
    })?;
    sync_directory(parent)
}

pub(super) fn read_reachability_state(path: &Path) -> Result<ReachabilityState, ProducerError> {
    if !path.exists() {
        return Ok(ReachabilityState::default());
    }
    let bytes = read_bounded_regular(path, 64 * 1024)?;
    let state: ReachabilityState = serde_json::from_slice(&bytes)?;
    let candidate_is_coherent = matches!(
        (state.candidate_reachable, state.consecutive),
        (None, 0) | (Some(_), 1..)
    );
    let generation_is_coherent = matches!(
        (state.stable, state.generation % 2),
        (ReachabilityStable::Reachable, 0) | (ReachabilityStable::Lost, 1)
    );
    if !candidate_is_coherent
        || !generation_is_coherent
        || (state.probe_pool.is_none() && state.generation > 0)
        || state.notified_generation > state.generation
    {
        return Err(ProducerError::InvalidObservation(format!(
            "reachability state {} is internally inconsistent",
            path.display()
        )));
    }
    Ok(state)
}

pub(super) fn validate_reachability_binding(
    state: &ReachabilityState,
    path: &Path,
    probe_pool: &str,
) -> Result<(), ProducerError> {
    if state.probe_pool.as_deref() == Some(probe_pool) {
        Ok(())
    } else {
        Err(ProducerError::InvalidObservation(format!(
            "reachability state {} is not bound to configured probePool {probe_pool:?}",
            path.display()
        )))
    }
}

pub(super) fn read_bounded_regular(path: &Path, limit: u64) -> Result<Vec<u8>, ProducerError> {
    let preopen_metadata = std::fs::symlink_metadata(path).map_err(|source| ProducerError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !preopen_metadata.is_file() || preopen_metadata.file_type().is_symlink() {
        return Err(ProducerError::InvalidObservation(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| {
            if source.raw_os_error() == Some(libc::ELOOP) {
                ProducerError::InvalidObservation(format!(
                    "{} is a symlink, not a regular file",
                    path.display()
                ))
            } else {
                ProducerError::Io {
                    path: path.to_owned(),
                    source,
                }
            }
        })?;
    let metadata = file.metadata().map_err(|source| ProducerError::Io {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ProducerError::InvalidObservation(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > limit {
        return Err(ProducerError::InvalidObservation(format!(
            "{} exceeds the {} byte limit",
            path.display(),
            limit
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(ProducerError::InvalidObservation(format!(
            "{} grew beyond the {} byte limit while reading",
            path.display(),
            limit
        )));
    }
    Ok(bytes)
}

pub(super) fn open_private_rw(path: &Path) -> Result<File, ProducerError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })?;
    if !file
        .metadata()
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })?
        .is_file()
    {
        return Err(ProducerError::InvalidObservation(format!(
            "{} is not a regular lock file",
            path.display()
        )));
    }
    Ok(file)
}

pub(super) fn sync_directory(path: &Path) -> Result<(), ProducerError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| ProducerError::Io {
            path: path.to_owned(),
            source,
        })
}
