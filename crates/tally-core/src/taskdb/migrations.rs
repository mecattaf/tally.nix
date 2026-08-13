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

pub const ROW_MIGRATIONS: &[RowMigration] = &[
    RowMigration {
        from: 1,
        to: 2,
        migrate: migrate_origin_v1_to_v2,
    },
    RowMigration {
        from: 2,
        to: 3,
        migrate: migrate_drv_v2_to_v3,
    },
    RowMigration {
        from: 3,
        to: 4,
        migrate: migrate_job_token_hash_v3_to_v4,
    },
    RowMigration {
        from: 4,
        to: 5,
        migrate: migrate_usage_predecessor_v4_to_v5,
    },
];

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
    if allowed_delta != canonical {
        return Err(
            "rowVersion 1 differs from canonical rowVersion 2 beyond origin back-fill".to_owned(),
        );
    }

    Ok(canonical)
}

fn migrate_drv_v2_to_v3(original: &RowSeed) -> Result<RowSeed, String> {
    if original.row_version != 2 {
        return Err(format!(
            "drv migration requires rowVersion 2, got {}",
            original.row_version
        ));
    }
    original.validate().map_err(|error| error.to_string())?;
    if original.drv.is_some() {
        return Err(
            "rowVersion 2 drv migration requires the new drv field to be absent".to_owned(),
        );
    }

    let mut canonical = original.clone();
    canonical.row_version = 3;
    canonical
        .canonicalize()
        .map_err(|error| error.to_string())?;

    let mut allowed_delta = original.clone();
    allowed_delta.row_version = 3;
    if allowed_delta != canonical {
        return Err(
            "rowVersion 2 differs from canonical rowVersion 3 beyond drv absence".to_owned(),
        );
    }
    Ok(canonical)
}

fn migrate_job_token_hash_v3_to_v4(original: &RowSeed) -> Result<RowSeed, String> {
    if original.row_version != 3 {
        return Err(format!(
            "job token hash migration requires rowVersion 3, got {}",
            original.row_version
        ));
    }
    original.validate().map_err(|error| error.to_string())?;
    if original.job_token_hash.is_some() {
        return Err(
            "rowVersion 3 job token hash migration requires the new jobTokenHash field to be absent"
                .to_owned(),
        );
    }

    let mut canonical = original.clone();
    canonical.row_version = 4;
    canonical
        .canonicalize()
        .map_err(|error| error.to_string())?;

    let mut allowed_delta = original.clone();
    allowed_delta.row_version = 4;
    if allowed_delta != canonical {
        return Err(
            "rowVersion 3 differs from canonical rowVersion 4 beyond jobTokenHash absence"
                .to_owned(),
        );
    }
    Ok(canonical)
}

fn migrate_usage_predecessor_v4_to_v5(original: &RowSeed) -> Result<RowSeed, String> {
    if original.row_version != 4 {
        return Err(format!(
            "usage predecessor migration requires rowVersion 4, got {}",
            original.row_version
        ));
    }
    original.validate().map_err(|error| error.to_string())?;
    if original.usage_predecessor.is_some() || original.usage_accounting.is_some() {
        return Err(
            "rowVersion 4 usage evidence migration requires the new usagePredecessor and usageAccounting fields to be absent"
                .to_owned(),
        );
    }

    let mut canonical = original.clone();
    canonical.row_version = 5;
    canonical
        .canonicalize()
        .map_err(|error| error.to_string())?;

    let mut allowed_delta = original.clone();
    allowed_delta.row_version = 5;
    if allowed_delta != canonical {
        return Err(
            "rowVersion 4 differs from canonical rowVersion 5 beyond usage evidence absence"
                .to_owned(),
        );
    }
    Ok(canonical)
}
