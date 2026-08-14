use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::error::{DriverError, Result};

/// Match `Path.resolve(strict=False)`: canonicalize the deepest existing
/// ancestor, then normalize the not-yet-created suffix lexically.
pub(crate) fn resolve(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(DriverError::new(format!(
            "cannot resolve relative path {}",
            path.display()
        )));
    }
    if path.exists() || std::fs::symlink_metadata(path).is_ok() {
        return std::fs::canonicalize(path).map_err(|error| {
            DriverError::new(format!("cannot resolve {}: {error}", path.display()))
        });
    }

    let mut ancestor = path;
    let mut suffix: Vec<OsString> = Vec::new();
    while std::fs::symlink_metadata(ancestor).is_err() {
        let name = ancestor.file_name().ok_or_else(|| {
            DriverError::new(format!(
                "cannot find an existing ancestor of {}",
                path.display()
            ))
        })?;
        suffix.push(name.to_owned());
        ancestor = ancestor.parent().ok_or_else(|| {
            DriverError::new(format!(
                "cannot find an existing ancestor of {}",
                path.display()
            ))
        })?;
    }
    let mut resolved = std::fs::canonicalize(ancestor).map_err(|error| {
        DriverError::new(format!("cannot resolve {}: {error}", ancestor.display()))
    })?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    lexical(&resolved)
}

fn lexical(path: &Path) -> Result<PathBuf> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                output.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    return Err(DriverError::new(format!(
                        "cannot normalize {}",
                        path.display()
                    )));
                }
            }
        }
    }
    Ok(output)
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}
