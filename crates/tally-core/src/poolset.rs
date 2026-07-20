use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PoolSetError {
    #[error("pool set must contain at least one pool")]
    Empty,
    #[error("pool set contains duplicate pool {0:?}")]
    Duplicate(String),
    #[error("pool name {0:?} must be non-empty and contain no control characters")]
    InvalidName(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PoolSetRepr {
    One(String),
    Many(Vec<String>),
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match PoolSetRepr::deserialize(deserializer)? {
        PoolSetRepr::One(pool) => vec![pool],
        PoolSetRepr::Many(pools) => pools,
    })
}

pub fn serialize<S>(pools: &[String], serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match pools {
        [pool] => serializer.serialize_str(pool),
        pools => pools.serialize(serializer),
    }
}

pub fn deserialize_optional<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Option::<PoolSetRepr>::deserialize(deserializer)? {
        Some(PoolSetRepr::One(pool)) => Some(vec![pool]),
        Some(PoolSetRepr::Many(pools)) => Some(pools),
        None => None,
    })
}

pub fn serialize_optional<S>(pools: &Option<Vec<String>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match pools {
        Some(pools) => serialize(pools, serializer),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize_encoded_optional<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|value| decoded(&value).map_err(serde::de::Error::custom))
        .transpose()
}

pub fn serialize_encoded_optional<S>(
    pools: &Option<Vec<String>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match pools {
        Some(pools) => {
            serializer.serialize_str(&encoded(pools).map_err(serde::ser::Error::custom)?)
        }
        None => serializer.serialize_none(),
    }
}

pub fn canonicalize(pools: &mut [String]) -> Result<(), PoolSetError> {
    if pools.is_empty() {
        return Err(PoolSetError::Empty);
    }
    let mut seen = HashSet::with_capacity(pools.len());
    for pool in pools.iter() {
        if pool.trim().is_empty() || pool.chars().any(char::is_control) {
            return Err(PoolSetError::InvalidName(pool.clone()));
        }
        if !seen.insert(pool.clone()) {
            return Err(PoolSetError::Duplicate(pool.clone()));
        }
    }
    pools.sort();
    Ok(())
}

pub fn encoded(pools: &[String]) -> Result<String, serde_json::Error> {
    match pools {
        [pool] => Ok(pool.clone()),
        pools => serde_json::to_string(pools),
    }
}

pub fn decoded(value: &str) -> Result<Vec<String>, serde_json::Error> {
    if value.starts_with('[') {
        serde_json::from_str(value)
    } else {
        Ok(vec![value.to_owned()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Surface {
        #[serde(serialize_with = "serialize", deserialize_with = "deserialize")]
        pool: Vec<String>,
    }

    #[test]
    fn legacy_scalar_and_canonical_multi_encoding_round_trip() {
        let scalar: Surface = serde_json::from_str(r#"{"pool":"slot"}"#).unwrap();
        assert_eq!(scalar.pool, ["slot"]);
        assert_eq!(
            serde_json::to_string(&scalar).unwrap(),
            r#"{"pool":"slot"}"#
        );
        assert_eq!(encoded(&scalar.pool).unwrap(), "slot");

        let mut multi = vec!["zeta".to_owned(), "alpha".to_owned()];
        canonicalize(&mut multi).unwrap();
        assert_eq!(multi, ["alpha", "zeta"]);
        assert_eq!(encoded(&multi).unwrap(), r#"["alpha","zeta"]"#);
        assert_eq!(decoded(r#"["alpha","zeta"]"#).unwrap(), multi);
    }

    #[test]
    fn empty_duplicate_and_invalid_names_are_distinct() {
        assert_eq!(canonicalize(&mut Vec::new()), Err(PoolSetError::Empty));
        assert_eq!(
            canonicalize(&mut ["slot".to_owned(), "slot".to_owned()]),
            Err(PoolSetError::Duplicate("slot".to_owned()))
        );
        assert_eq!(
            canonicalize(&mut ["bad\nname".to_owned()]),
            Err(PoolSetError::InvalidName("bad\nname".to_owned()))
        );
    }
}
