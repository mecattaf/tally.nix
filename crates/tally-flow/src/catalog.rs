use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{FlowError, SourceLocation};

const CATALOG_SCHEMA_TEXT: &str = include_str!("../schema/catalog.schema.json");
const CATALOG_FALLBACK_LOCATION: SourceLocation = SourceLocation::new(1, 1);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Catalog {
    pub version: u32,
    pub members: Vec<CatalogMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogMember {
    pub id: String,
    pub family: String,
    pub maker: String,
    pub classes: Vec<String>,
    pub adapter: String,
    pub pools: Vec<String>,
    pub launch: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fine_tune: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SelectorOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diversity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSelection {
    pub selector: String,
    pub options: SelectorOptions,
    pub catalog_hash: String,
    pub members: Vec<CatalogMember>,
}

#[must_use]
pub fn catalog_schema() -> Value {
    serde_json::from_str(CATALOG_SCHEMA_TEXT)
        .expect("the embedded flow catalog schema must remain valid JSON")
}

pub fn load_catalog(path: &Path) -> Result<(Catalog, String), FlowError> {
    let bytes = fs::read(path).map_err(|error| {
        FlowError::new(
            "FlowCatalogError",
            "catalog-unreadable",
            format!("cannot read catalog {}: {error}", path.display()),
        )
        .at(CATALOG_FALLBACK_LOCATION)
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        let line = u32::try_from(error.line()).unwrap_or(u32::MAX).max(1);
        let column = u32::try_from(error.column()).unwrap_or(u32::MAX).max(1);
        FlowError::new(
            "FlowCatalogError",
            "catalog-invalid-json",
            format!("catalog {} is not valid JSON: {error}", path.display()),
        )
        .at(SourceLocation::new(line, column))
    })?;
    validate_catalog_value(&value)
        .map_err(|error| error.at_if_missing(CATALOG_FALLBACK_LOCATION))?;
    let catalog: Catalog = serde_json::from_value(value).map_err(|error| {
        FlowError::new(
            "FlowCatalogError",
            "catalog-schema-mismatch",
            format!("catalog {} has an invalid shape: {error}", path.display()),
        )
        .at(CATALOG_FALLBACK_LOCATION)
    })?;
    validate_catalog_semantics(&catalog)
        .map_err(|error| error.at_if_missing(CATALOG_FALLBACK_LOCATION))?;
    Ok((catalog, sha256(&bytes)))
}

pub(crate) fn validate_catalog_value(value: &Value) -> Result<(), FlowError> {
    let schema = catalog_schema();
    let validator = jsonschema::validator_for(&schema).map_err(|error| {
        FlowError::new(
            "FlowCatalogError",
            "catalog-schema-invalid",
            format!("embedded catalog schema is invalid: {error}"),
        )
    })?;
    let errors = validator
        .iter_errors(value)
        .map(|error| format!("{}: {error}", error.instance_path))
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(FlowError::new(
            "FlowCatalogError",
            "catalog-schema-mismatch",
            format!("catalog does not match schema: {}", errors.join("; ")),
        )
        .detail("errors", serde_json::json!(errors)))
    }
}

pub(crate) fn validate_catalog_semantics(catalog: &Catalog) -> Result<(), FlowError> {
    let mut ids = BTreeSet::new();
    for member in &catalog.members {
        if !ids.insert(member.id.as_str()) {
            return Err(FlowError::new(
                "FlowCatalogError",
                "catalog-duplicate-member",
                format!("catalog member id {:?} appears more than once", member.id),
            )
            .detail("memberId", member.id.clone()));
        }
    }
    Ok(())
}

pub fn resolve_members(
    catalog: &Catalog,
    catalog_hash: &str,
    selector: &str,
    options: &SelectorOptions,
) -> Result<CatalogSelection, FlowError> {
    if selector.trim().is_empty() {
        return Err(FlowError::new(
            "FlowSelectorError",
            "selector-invalid",
            "selector class must not be empty",
        ));
    }
    if options.count == Some(0) {
        return Err(FlowError::new(
            "FlowSelectorError",
            "selector-invalid-count",
            "selector count must be positive",
        ));
    }
    if options
        .diversity
        .as_deref()
        .is_some_and(|key| !matches!(key, "family" | "maker"))
    {
        return Err(FlowError::new(
            "FlowSelectorError",
            "selector-invalid-diversity",
            "selector diversity must be \"family\" or \"maker\"",
        )
        .detail("diversity", options.diversity.clone().unwrap_or_default()));
    }

    let matching = catalog
        .members
        .iter()
        .filter(|member| member.classes.iter().any(|class| class == selector))
        .cloned()
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Err(FlowError::new(
            "FlowSelectorError",
            "selector-empty",
            format!("selector class {selector:?} resolves to no catalog members"),
        )
        .detail("selector", selector.to_owned()));
    }

    let requested = options.count.unwrap_or(matching.len());
    if requested > matching.len() {
        return Err(FlowError::new(
            "FlowSelectorError",
            "selector-insufficient-members",
            format!(
                "selector class {selector:?} requested {requested} members but only {} resolve",
                matching.len()
            ),
        )
        .detail("selector", selector.to_owned())
        .detail("requested", requested)
        .detail("available", matching.len()));
    }

    let ordered = if let Some(diversity) = options.diversity.as_deref() {
        round_robin_diversity(matching, diversity)
    } else {
        matching
    };
    Ok(CatalogSelection {
        selector: selector.to_owned(),
        options: options.clone(),
        catalog_hash: catalog_hash.to_owned(),
        members: ordered.into_iter().take(requested).collect(),
    })
}

fn round_robin_diversity(members: Vec<CatalogMember>, diversity: &str) -> Vec<CatalogMember> {
    let mut group_order = Vec::new();
    let mut groups: BTreeMap<String, VecDeque<CatalogMember>> = BTreeMap::new();
    for member in members {
        let key = match diversity {
            "family" => member.family.clone(),
            "maker" => member.maker.clone(),
            _ => unreachable!("diversity is validated before resolution"),
        };
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
        }
        groups.entry(key).or_default().push_back(member);
    }
    let mut output = Vec::new();
    loop {
        let mut emitted = false;
        for key in &group_order {
            if let Some(member) = groups.get_mut(key).and_then(VecDeque::pop_front) {
                output.push(member);
                emitted = true;
            }
        }
        if !emitted {
            break;
        }
    }
    output
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, family: &str, maker: &str) -> CatalogMember {
        CatalogMember {
            id: id.to_owned(),
            family: family.to_owned(),
            maker: maker.to_owned(),
            classes: vec!["pooled-fast".to_owned()],
            adapter: "pi".to_owned(),
            pools: vec!["worker-gpu".to_owned()],
            launch: serde_json::json!({"model": id}),
            architecture: None,
            fine_tune: None,
            backend: None,
            modality: None,
            role: None,
            status: None,
            evidence: None,
            hosts: Vec::new(),
            base_checkpoint: None,
            supersedes: None,
            superseded_by: None,
            notes: None,
        }
    }

    #[test]
    fn diversity_resolution_round_robins_first_seen_partitions() {
        let catalog = Catalog {
            version: 1,
            members: vec![
                member("a1", "a", "x"),
                member("a2", "a", "y"),
                member("b1", "b", "x"),
                member("c1", "c", "z"),
                member("b2", "b", "y"),
            ],
        };
        let selection = resolve_members(
            &catalog,
            "sha256:catalog",
            "pooled-fast",
            &SelectorOptions {
                count: Some(5),
                diversity: Some("family".to_owned()),
            },
        )
        .unwrap();
        assert_eq!(
            selection
                .members
                .iter()
                .map(|member| member.id.as_str())
                .collect::<Vec<_>>(),
            ["a1", "b1", "c1", "a2", "b2"]
        );
    }

    #[test]
    fn embedded_schema_rejects_missing_normative_fields() {
        let invalid = serde_json::json!({
            "version": 1,
            "members": [{"id": "only-an-id"}]
        });
        let error = validate_catalog_value(&invalid).unwrap_err();
        assert_eq!(error.code, "catalog-schema-mismatch");
    }
}
