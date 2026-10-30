//! What a game leaves behind for a match (its replay): one file per match, named by the
//! ruleset's `artifact` template (an absolute path whose file name holds `{match_id}` once).
//! radovid never reads the bytes' meaning: it hands them in chunks to the seats of the match and
//! prunes old files so the directory stays bounded.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use radovid_proto::{MAX_ARTIFACT_CHUNK, MatchId};

/// A parsed `artifact` template: `<dir>/<prefix>{match_id}<suffix>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    dir: PathBuf,
    prefix: String,
    suffix: String,
}

impl Template {
    pub fn parse(text: &str) -> Result<Template, String> {
        let path = Path::new(text);
        if !path.is_absolute() {
            return Err("artifact must be an absolute path".to_owned());
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            return Err("artifact must name a file".to_owned());
        };
        if dir.to_string_lossy().contains(['{', '}']) {
            return Err("artifact: only the file name may hold {match_id}".to_owned());
        }
        let Some((prefix, suffix)) = name.split_once("{match_id}") else {
            return Err("artifact: the file name must hold {match_id}".to_owned());
        };
        if suffix.contains(['{', '}']) || prefix.contains(['{', '}']) {
            return Err("artifact: {match_id} once and no other placeholder".to_owned());
        }
        Ok(Template {
            dir: dir.to_owned(),
            prefix: prefix.to_owned(),
            suffix: suffix.to_owned(),
        })
    }

    pub fn path(&self, match_id: MatchId) -> PathBuf {
        self.dir
            .join(format!("{}{}{}", self.prefix, match_id.0, self.suffix))
    }

    fn is_artifact(&self, name: &str) -> bool {
        name.strip_prefix(&self.prefix)
            .and_then(|rest| rest.strip_suffix(&self.suffix))
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    }
}

/// Up to `MAX_ARTIFACT_CHUNK` bytes of the match's file from `offset`, and the file's length;
/// `None` when there is no such regular file (a link is none: the game that writes the file
/// could otherwise point it at any file radovid may read) or when it is longer than `max_len`.
pub fn read_chunk(
    template: &Template,
    match_id: MatchId,
    offset: u64,
    max_len: u64,
) -> Option<(u64, Vec<u8>)> {
    use std::os::unix::fs::MetadataExt;
    let path = template.path(match_id);
    let linked = std::fs::symlink_metadata(&path).ok()?;
    let mut file = std::fs::File::open(&path).ok()?;
    let meta = file.metadata().ok()?;
    // The file opened is the one inspected: a link swapped in between is another inode.
    if !linked.is_file()
        || !meta.is_file()
        || (linked.dev(), linked.ino()) != (meta.dev(), meta.ino())
    {
        return None;
    }
    let total = meta.len();
    if total > max_len {
        return None;
    }
    let mut data = Vec::new();
    if offset < total {
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.take(MAX_ARTIFACT_CHUNK as u64)
            .read_to_end(&mut data)
            .ok()?;
    }
    Some((total, data))
}

/// Removes artifacts older than `max_age`, then the oldest ones until the rest fit `cap` bytes.
/// Only files named by the template are touched. Returns how many were removed.
pub fn prune(template: &Template, max_age: Duration, cap: u64) -> usize {
    let Ok(entries) = std::fs::read_dir(&template.dir) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| template.is_artifact(n))
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file()
                .then(|| (meta.modified().unwrap_or(now), meta.len(), e.path()))
        })
        .collect();
    files.sort();
    let mut removed = 0;
    let mut kept: u64 = files.iter().map(|f| f.1).sum();
    for (modified, size, path) in files {
        let old = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if (old || kept > cap) && std::fs::remove_file(&path).is_ok() {
            kept = kept.saturating_sub(size);
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_and_surplus_artifacts_go_and_others_stay() {
        let dir = std::env::temp_dir().join(format!("radovid-prune-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let template =
            Template::parse(&format!("{}/r-{{match_id}}.brp", dir.display())).expect("template");
        for id in 1..=3 {
            std::fs::write(template.path(MatchId(id)), vec![0u8; 100]).expect("file");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::write(dir.join("other.txt"), "kept").expect("other");
        assert_eq!(prune(&template, Duration::from_secs(3600), 250), 1);
        assert!(!template.path(MatchId(1)).exists(), "the oldest went");
        assert!(template.path(MatchId(2)).exists() && dir.join("other.txt").exists());
        assert_eq!(prune(&template, Duration::ZERO, u64::MAX), 2);
        assert!(dir.join("other.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
