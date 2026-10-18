//! Files that hold secrets, the database (password and session hashes) and the
//! TLS private key: no user of the host but their owner and group may open them.

use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Refuses a file every user of the host may open.
pub fn check(path: &Path, what: &str) -> Result<(), String> {
    let mode = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o007 != 0 {
        return Err(format!(
            "{what} {} is open to every user of the host (mode {:o}); run chmod o-rwx on it",
            path.display(),
            mode & 0o777
        ));
    }
    Ok(())
}

/// Creates the database file for its owner alone when it is absent (SQLite
/// gives its journal files the same mode), and checks it otherwise.
pub fn database(path: &Path) -> Result<(), String> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => check(path, "the database"),
        Err(e) => Err(format!("cannot create {}: {e}", path.display())),
    }
}

/// Writes `contents` to a file only its owner may open.
pub fn write(path: &Path, contents: &str) -> Result<(), String> {
    let error = |e: std::io::Error| format!("cannot write {}: {e}", path.display());
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(error)?;
    // An existing file keeps its mode through open: narrow it before writing.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(error)?;
    std::io::Write::write_all(&mut &file, contents.as_bytes()).map_err(error)
}
