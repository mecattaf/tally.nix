use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SHELL_COMMAND_PROVIDER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test/fixtures/shell-command-provider"
);

fn source_path(path: &Path) -> PathBuf {
    let mut source = OsString::from(path.as_os_str());
    source.push(".tally-test-script");
    PathBuf::from(source)
}

pub fn install(path: &Path, body: impl AsRef<[u8]>) {
    std::fs::write(source_path(path), body).unwrap();
    std::os::unix::fs::symlink(SHELL_COMMAND_PROVIDER, path).unwrap();
}
