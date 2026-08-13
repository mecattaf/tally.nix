//! Ordered migrations for the durable row schema.
//!
//! The deployed estate was clean when the historical ladder was retired. The
//! frame remains so a future schema change still has one ordered, versioned
//! migration path; rows older than the current floor are refused with the last
//! pin that can upgrade them.

use super::{RowSeed, CURRENT_ROW_VERSION};

#[derive(Debug, Clone, Copy)]
pub struct RowMigrationStep {
    pub from: u32,
    pub to: u32,
    pub migrate: fn(&RowSeed) -> Result<RowSeed, String>,
}

pub type RowMigration = RowMigrationStep;

pub const LAST_ROW_MIGRATION_PIN: &str = "816ed305aed9ab96309483f2fe9ac39155c56c8e";

pub const ROW_MIGRATIONS: &[RowMigration] = &[];

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
                    "rowVersion {} predates this binary; temporarily pin tally.nix to \
                     {LAST_ROW_MIGRATION_PIN}, start tally once to migrate its durable rows, then \
                     upgrade to this pin",
                    migrated.row_version,
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
