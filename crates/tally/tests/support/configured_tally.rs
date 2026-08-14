use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const EMPTY_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/empty-config.json");
const SHELL_COMMAND_PROVIDER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test/fixtures/shell-command-provider"
);

fn source_path(path: &Path) -> PathBuf {
    let mut source = OsString::from(path.as_os_str());
    source.push(".tally-test-script");
    PathBuf::from(source)
}

/// Install an argv-compatible recorder program that makes the tally config
/// explicit before forwarding the executor-owned hidden subcommand.
pub fn install(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        source_path(path),
        format!(
            "#!/bin/sh\nexec '{}' --config '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_tally"),
            EMPTY_CONFIG
        ),
    )
    .unwrap();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => assert!(
            metadata.file_type().is_symlink(),
            "configured tally path is not the installed provider symlink: {}",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            std::os::unix::fs::symlink(SHELL_COMMAND_PROVIDER, path).unwrap();
        }
        Err(error) => panic!("failed to inspect {}: {error}", path.display()),
    }
    path.to_owned()
}
