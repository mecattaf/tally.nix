//! The working tree the spec is checked against: what `BELIEVE:<path>`
//! resolves to, and which backticked tokens are already in context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The tree an identity directory is read against, plus the bytes of every
/// file the spec believes.
#[derive(Clone, Debug)]
pub struct Tree {
    root: PathBuf,
    local: Option<PathBuf>,
    believed: BTreeMap<String, String>,
}

impl Tree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            local: None,
            believed: BTreeMap::new(),
        }
    }

    /// The identity directory. A spec that cites `spec.md` or
    /// `contracts/trace.schema.json` names a file beside itself.
    pub fn with_local(mut self, local: impl Into<PathBuf>) -> Self {
        self.local = Some(local.into());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Read a believed file once and keep its bytes for the identifier checks.
    pub fn believe(&mut self, path: &str) -> Option<&str> {
        if !self.believed.contains_key(path) {
            let bytes = self.read(path)?;
            self.believed.insert(path.to_owned(), bytes);
        }
        self.believed.get(path).map(String::as_str)
    }

    /// Whether a path exists in the tree, read from the root or from the
    /// identity directory.
    pub fn exists(&self, path: &str) -> bool {
        self.resolve(path).iter().any(|full| full.exists())
    }

    /// A path's bytes, when it exists and is UTF-8.
    pub fn read(&self, path: &str) -> Option<String> {
        self.resolve(path)
            .into_iter()
            .find_map(|full| std::fs::read_to_string(full).ok())
    }

    /// Whether any believed file's bytes carry `needle`.
    pub fn believed_bytes_carry(&self, needle: &str) -> bool {
        self.believed.values().any(|bytes| bytes.contains(needle))
    }

    /// Tree paths are relative and never escape their base. A trailing
    /// `#anchor` is a pointer into the file, not part of its name; resolving
    /// the anchor itself belongs to the cross-artifact pass.
    fn resolve(&self, path: &str) -> Vec<PathBuf> {
        let trimmed = path
            .trim()
            .split('#')
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with("..") {
            return Vec::new();
        }
        std::iter::once(&self.root)
            .chain(self.local.iter())
            .map(|base| base.join(trimmed))
            .collect()
    }
}

/// The working-tree root an identity directory is read against: the parent of
/// `specs/` when the directory sits under one, and the directory itself
/// otherwise. Fixture corpora are therefore self-contained wherever the lint
/// runs from, and `spec-lint specs/<identity>` resolves against the repo.
pub fn infer_root(directory: &Path) -> PathBuf {
    let parent = directory.parent();
    let under_specs = parent
        .and_then(Path::file_name)
        .is_some_and(|name| name == "specs");
    match (under_specs, parent.and_then(Path::parent)) {
        (true, Some(grandparent)) if !grandparent.as_os_str().is_empty() => grandparent.to_owned(),
        (true, Some(_)) => PathBuf::from("."),
        _ => directory.to_owned(),
    }
}

/// Whether a whitespace-delimited token looks like an identifier rather than an
/// English word: it carries a path, module, dotted, hyphenated, or camelCase
/// separator. Flags (`--mode`, `-L`) are shell surface, not identifiers.
pub fn is_identifier(token: &str) -> bool {
    let token = trim_token(token);
    if token.len() < 2 || token.starts_with('-') {
        return false;
    }
    if token.contains('/') || token.contains('_') || token.contains("::") {
        return true;
    }
    let bytes: Vec<char> = token.chars().collect();
    let joined = |separator: char| {
        bytes.windows(3).any(|window| {
            window[1] == separator
                && window[0].is_ascii_alphanumeric()
                && window[2].is_ascii_alphanumeric()
        })
    };
    if joined('.') || joined('-') {
        return true;
    }
    bytes
        .windows(2)
        .any(|window| window[0].is_ascii_lowercase() && window[1].is_ascii_uppercase())
}

/// Split a backticked span into candidate identifier tokens.
pub fn identifier_tokens(span: &str) -> Vec<String> {
    span.split_whitespace()
        .filter(|token| is_identifier(token))
        .map(|token| trim_token(token).to_owned())
        .collect()
}

fn trim_token(token: &str) -> &str {
    token.trim_matches(|character: char| ",;:()\"'`".contains(character))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{identifier_tokens, infer_root, is_identifier, Tree};

    #[test]
    fn the_root_is_the_parent_of_specs_or_the_directory_itself() {
        assert_eq!(
            infer_root(Path::new("/repo/specs/zeta")),
            PathBuf::from("/repo")
        );
        assert_eq!(infer_root(Path::new("specs/zeta")), PathBuf::from("."));
        assert_eq!(
            infer_root(Path::new("crates/spec-lint/tests/fixtures/golden")),
            PathBuf::from("crates/spec-lint/tests/fixtures/golden")
        );
    }

    #[test]
    fn identifier_tokens_skip_prose_words_and_flags() {
        assert!(is_identifier("retry_with_jitter"));
        assert!(is_identifier("specs/zeta"));
        assert!(is_identifier("expected-defects.json"));
        assert!(is_identifier("specSections"));
        assert!(!is_identifier("check"));
        assert!(!is_identifier("--mode"));
        assert!(!is_identifier("-L"));
        assert_eq!(
            identifier_tokens("spec-lint --mode check over specs/zeta"),
            ["spec-lint", "specs/zeta"]
        );
    }

    #[test]
    fn a_tree_reads_only_inside_its_root() {
        let tree = Tree::new(env!("CARGO_MANIFEST_DIR"));
        assert!(tree.exists("Cargo.toml"));
        assert!(!tree.exists("/etc/hostname"));
        assert!(!tree.exists("../../Cargo.toml"));
        assert!(tree
            .read("Cargo.toml")
            .is_some_and(|bytes| bytes.contains("spec-lint")));
    }

    #[test]
    fn a_path_resolves_beside_the_spec_and_survives_an_anchor() {
        let tree = Tree::new(env!("CARGO_MANIFEST_DIR"))
            .with_local(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden"));
        assert!(tree.exists("evidence/example.md"));
        assert!(tree.exists("spec.md#r1"));
        assert!(!tree.exists("evidence/missing.md"));
    }
}
