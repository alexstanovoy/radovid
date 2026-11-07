//! Private immutable result records: readers cannot block publication or acknowledgement.
use super::*;
use serde::de::{SeqAccess, Visitor};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

pub(super) const MAX_PENDING: usize = 262_144;
const MAX_PAYLOAD: usize = MAX_GAME_REPORT_LEN + 4096;
const HEADER: usize = 48;
const MAGIC: &[u8; 8] = b"RDVRES1\0";

#[derive(Default)]
struct Index {
    next: u64,
    orders: std::collections::HashMap<MatchId, u64>,
    ready: std::collections::BTreeSet<(u64, MatchId)>,
}
pub(super) struct Results {
    directory: std::path::PathBuf,
    index: Mutex<Index>,
}

fn invalid(message: impl Into<String>) -> DbError {
    DbError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message.into(),
    ))
}

impl Results {
    pub(super) fn open(database: &Path) -> DbResult<Self> {
        let mut name = database.as_os_str().to_os_string();
        name.push(".pending_results");
        let directory = std::path::PathBuf::from(name);
        match std::fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(DbError::Io(error)),
        }
        let metadata = std::fs::symlink_metadata(&directory).map_err(DbError::Io)?;
        if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid(
                "pending-result directory must be private and cannot be a link",
            ));
        }
        sync_directory(
            directory
                .parent()
                .ok_or_else(|| invalid("pending-result directory has no parent"))?,
        )?;
        let mut index = Index::default();
        let mut count = 0;
        for entry in std::fs::read_dir(&directory).map_err(DbError::Io)? {
            count += 1;
            if count > MAX_PENDING * 2 {
                return Err(invalid("too many pending-result files"));
            }
            let entry = entry.map_err(DbError::Io)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid("invalid pending-result filename"))?;
            let (id, temporary) = record_name(&name)?;
            let (order, finish) = read_record(&entry.path())?;
            if finish.match_id != id {
                return Err(invalid("pending-result filename and match disagree"));
            }
            let target = directory.join(format!("{:020}.result", id.0));
            if temporary {
                if target.exists() {
                    let existing = read_record(&target)?;
                    if existing != (order, finish.clone()) {
                        return Err(invalid("conflicting pending-result temporary record"));
                    }
                } else {
                    std::fs::File::open(entry.path())
                        .and_then(|file| file.sync_all())
                        .map_err(DbError::Io)?;
                    std::fs::hard_link(entry.path(), &target).map_err(DbError::Io)?;
                }
                sync_directory(&directory)?;
                std::fs::remove_file(entry.path()).map_err(DbError::Io)?;
            }
            if index.orders.len() >= MAX_PENDING && !index.orders.contains_key(&id) {
                return Err(invalid("pending-result capacity is full"));
            }
            index.orders.insert(id, order);
            index.ready.insert((order, id));
            index.next = index.next.max(
                order
                    .checked_add(1)
                    .ok_or_else(|| invalid("pending-result order exhausted"))?,
            );
        }
        sync_directory(&directory)?;
        Ok(Self {
            directory,
            index: Mutex::new(index),
        })
    }

    pub(super) fn save(&self, finish: &Finish) -> DbResult<()> {
        validate(finish)?;
        let payload = postcard::to_allocvec(finish).map_err(|error| invalid(error.to_string()))?;
        if payload.len() > MAX_PAYLOAD {
            return Err(invalid("pending result is too large"));
        }
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let target = self
            .directory
            .join(format!("{:020}.result", finish.match_id.0));
        if target.exists() {
            let (order, existing) = read_record(&target)?;
            if &existing != finish {
                return Err(invalid("conflicting result for the same match"));
            }
            std::fs::File::open(&target)
                .and_then(|file| file.sync_all())
                .map_err(DbError::Io)?;
            sync_directory(&self.directory)?;
            index.orders.insert(finish.match_id, order);
            index.ready.insert((order, finish.match_id));
            index.next = index.next.max(
                order
                    .checked_add(1)
                    .ok_or_else(|| invalid("pending-result order exhausted"))?,
            );
            return Ok(());
        }
        let order = if let Some(order) = index.orders.get(&finish.match_id) {
            *order
        } else {
            if index.orders.len() >= MAX_PENDING {
                return Err(invalid("pending-result capacity is full"));
            }
            let order = index.next;
            index.next = order
                .checked_add(1)
                .ok_or_else(|| invalid("pending-result order exhausted"))?;
            index.orders.insert(finish.match_id, order);
            order
        };
        self.publish(finish.match_id, order, &payload, &target)?;
        index.ready.insert((order, finish.match_id));
        Ok(())
    }

    fn publish(&self, id: MatchId, order: u64, payload: &[u8], target: &Path) -> DbResult<()> {
        let temporary = self.directory.join(format!("{:020}.tmp", id.0));
        if let Ok(metadata) = std::fs::symlink_metadata(&temporary)
            && !metadata.file_type().is_file()
        {
            return Err(invalid(
                "pending-result temporary path must be a regular file",
            ));
        }
        if temporary.exists() {
            if let Ok((stored, finish)) = read_record(&temporary) {
                let previous =
                    postcard::to_allocvec(&finish).map_err(|error| invalid(error.to_string()))?;
                if stored != order || finish.match_id != id || previous != payload {
                    return Err(invalid("conflicting pending-result temporary record"));
                }
                std::fs::File::open(&temporary)
                    .and_then(|file| file.sync_all())
                    .map_err(DbError::Io)?;
                std::fs::hard_link(&temporary, target).map_err(DbError::Io)?;
                sync_directory(&self.directory)?;
                std::fs::remove_file(&temporary).map_err(DbError::Io)?;
                return sync_directory(&self.directory);
            }
            // Remove only our uncommitted directory entry; never truncate a possibly linked inode.
            std::fs::remove_file(&temporary).map_err(DbError::Io)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(DbError::Io)?;
        let mut digest = Sha256::new();
        digest.update(order.to_le_bytes());
        digest.update(payload);
        file.write_all(MAGIC)
            .and_then(|_| file.write_all(&order.to_le_bytes()))
            .and_then(|_| file.write_all(&digest.finalize()))
            .and_then(|_| file.write_all(payload))
            .and_then(|_| file.sync_all())
            .map_err(DbError::Io)?;
        drop(file);
        std::fs::hard_link(&temporary, target).map_err(DbError::Io)?;
        sync_directory(&self.directory)?;
        std::fs::remove_file(&temporary).map_err(DbError::Io)?;
        sync_directory(&self.directory)
    }

    pub(super) fn first(&self) -> DbResult<Option<Finish>> {
        let index = self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(&(order, id)) = index.ready.first() else {
            return Ok(None);
        };
        let (stored, finish) = read_record(&self.directory.join(format!("{:020}.result", id.0)))?;
        if stored != order || finish.match_id != id {
            return Err(invalid("pending-result index disagrees with its record"));
        }
        Ok(Some(finish))
    }

    pub(super) fn remove(&self, id: MatchId) -> DbResult<()> {
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A publication can be durable even if its temporary-name cleanup failed.
        for suffix in ["tmp", "result"] {
            match std::fs::remove_file(self.directory.join(format!("{:020}.{suffix}", id.0))) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(DbError::Io(error)),
            }
        }
        sync_directory(&self.directory)?;
        if let Some(order) = index.orders.remove(&id) {
            index.ready.remove(&(order, id));
        }
        Ok(())
    }
}

fn record_name(name: &str) -> DbResult<(MatchId, bool)> {
    let (digits, suffix) = name
        .split_once('.')
        .ok_or_else(|| invalid("invalid pending-result filename"))?;
    if digits.len() != 20
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || !matches!(suffix, "result" | "tmp")
    {
        return Err(invalid("unrecognized file in pending-result directory"));
    }
    let id = digits
        .parse::<u64>()
        .map_err(|_| invalid("invalid pending-result match id"))?;
    Ok((MatchId(id), suffix == "tmp"))
}

fn read_record(path: &Path) -> DbResult<(u64, Finish)> {
    let metadata = std::fs::symlink_metadata(path).map_err(DbError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.len() > (HEADER + MAX_PAYLOAD) as u64
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(invalid(
            "pending-result record is not a bounded private regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(DbError::Io)?
        .take((HEADER + MAX_PAYLOAD + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(DbError::Io)?;
    decode_record(&bytes)
}

pub(super) fn decode_record(bytes: &[u8]) -> DbResult<(u64, Finish)> {
    if bytes.len() <= HEADER
        || bytes.len() > HEADER + MAX_PAYLOAD
        || bytes.get(..8) != Some(MAGIC.as_slice())
    {
        return Err(invalid("invalid pending-result record header"));
    }
    let order = u64::from_le_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| invalid("invalid result order"))?,
    );
    let payload = &bytes[HEADER..];
    let mut digest = Sha256::new();
    digest.update(order.to_le_bytes());
    digest.update(payload);
    if digest.finalize().as_slice() != &bytes[16..HEADER] {
        return Err(invalid("pending-result checksum failed"));
    }
    let (finish, rest) =
        postcard::take_from_bytes::<Finish>(payload).map_err(|error| invalid(error.to_string()))?;
    if !rest.is_empty() {
        return Err(invalid("trailing pending-result data"));
    }
    validate(&finish)?;
    Ok((order, finish))
}

fn sync_directory(directory: &Path) -> DbResult<()> {
    std::fs::File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(DbError::Io)
}

fn validate(finish: &Finish) -> DbResult<()> {
    let valid_id = |id: u64| id > 0 && id <= i64::MAX as u64;
    if !valid_id(finish.match_id.0)
        || finish.ruleset.0.is_empty()
        || finish.ruleset.0.len() > 32
        || !finish
            .ruleset
            .0
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || finish.report.len() > MAX_GAME_REPORT_LEN
        || finish.seats.is_empty()
        || finish.seats.len() > MAX_SEATS
    {
        return Err(invalid("invalid pending-result bounds"));
    }
    let mut accounts = std::collections::BTreeSet::new();
    let mut slots = std::collections::BTreeSet::new();
    for seat in &finish.seats {
        if !valid_id(seat.account.0)
            || usize::from(seat.slot) >= MAX_SEATS
            || usize::from(seat.team) >= MAX_SEATS
            || !accounts.insert(seat.account)
            || !slots.insert(seat.slot)
        {
            return Err(invalid("invalid pending-result seats"));
        }
    }
    Ok(())
}

pub(super) fn report<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, MAX_GAME_REPORT_LEN>(deserializer)
}
pub(super) fn seats<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<NewSeat>, D::Error> {
    bounded::<D, NewSeat, MAX_SEATS>(deserializer)
}
fn bounded<'de, D, T, const MAX: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Entries<T, const MAX: usize>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>, const MAX: usize> Visitor<'de> for Entries<T, MAX> {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "at most {MAX} pending-result entries")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            if sequence.size_hint().is_some_and(|size| size > MAX) {
                return Err(serde::de::Error::custom(
                    "pending-result field exceeds limit",
                ));
            }
            let mut entries = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
            while let Some(entry) = sequence.next_element()? {
                if entries.len() == MAX {
                    return Err(serde::de::Error::custom(
                        "pending-result field exceeds limit",
                    ));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(Entries::<T, MAX>(std::marker::PhantomData))
}
