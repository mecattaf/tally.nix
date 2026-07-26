//! Ordered migrations for the durable row schema.
//!
//! Any field a canonicalizer can derive MUST be tolerated absent-on-disk. Every
//! durable-schema change ships as an explicit, versioned, ordered migration
//! with a literal N-1 fixture. An ad-hoc canonicalization fixed-point check is
//! never a migration predicate: each entry admits only its declared delta.

use super::{RowSeed, CURRENT_ROW_VERSION};

#[derive(Debug, Clone, Copy)]
pub struct RowMigration {
    pub from: u32,
    pub to: u32,
    pub migrate: fn(&RowSeed) -> Result<RowSeed, String>,
}

pub const ROW_MIGRATIONS: &[RowMigration] = &[RowMigration {
    from: 1,
    to: 2,
    migrate: migrate_origin_v1_to_v2,
}];

pub fn migrate_to_current(row: &RowSeed) -> Result<RowSeed, String> {
    let mut migrated = row.clone();
    while migrated.row_version != CURRENT_ROW_VERSION {
        if migrated.row_version == 0 || migrated.row_version > CURRENT_ROW_VERSION {
            return Err(format!(
                "rowVersion {} is unsupported; current rowVersion is {CURRENT_ROW_VERSION}",
                migrated.row_version
            ));
        }
        let migration = ROW_MIGRATIONS
            .iter()
            .find(|migration| migration.from == migrated.row_version)
            .ok_or_else(|| {
                format!(
                    "rowVersion {} has no migration to current rowVersion {CURRENT_ROW_VERSION}",
                    migrated.row_version
                )
            })?;
        let next = (migration.migrate)(&migrated)?;
        if migration.to <= migration.from || next.row_version != migration.to {
            return Err(format!(
                "row migration {} -> {} produced rowVersion {}",
                migration.from, migration.to, next.row_version
            ));
        }
        migrated = next;
    }
    Ok(migrated)
}

fn migrate_origin_v1_to_v2(original: &RowSeed) -> Result<RowSeed, String> {
    if original.row_version != 1 {
        return Err(format!(
            "origin migration requires rowVersion 1, got {}",
            original.row_version
        ));
    }
    original.validate().map_err(|error| error.to_string())?;

    let mut canonical = original.clone();
    canonical.row_version = 2;
    canonical
        .canonicalize()
        .map_err(|error| error.to_string())?;

    if original.origin.is_some() {
        return Err(
            "rowVersion 1 origin migration requires the legacy origin field to be absent"
                .to_owned(),
        );
    }

    let mut allowed_delta = original.clone();
    allowed_delta.row_version = 2;
    allowed_delta.origin = canonical.origin.clone();
    allowed_delta.gh_origin = canonical.gh_origin.clone();
    if allowed_delta != canonical {
        return Err(
            "rowVersion 1 differs from canonical rowVersion 2 beyond origin back-fill".to_owned(),
        );
    }

    Ok(canonical)
}
