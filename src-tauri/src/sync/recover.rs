// Fluence sync - safe corrupt-file recovery primitives (Phase E).
//
// Established pattern (settings.rs `load_settings`, account_scope.rs
// `read_json`): on parse failure, preserve the original bytes under a unique
// `json.corrupt[.N].json` sibling (rename, fallback copy+delete), then fail
// closed — never synthesize an empty store over corrupt bytes, and never let
// a later write persist state derived from bytes we could not read.

use std::path::{Path, PathBuf};

/// Why a present file could not be loaded. Returned instead of a default
/// value so write paths fail closed rather than persisting partial state
/// over bytes we could not read.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// Bytes were read but do not parse as `T` (truncated, malformed, empty,
    /// or valid JSON of an incompatible shape). `backup` is `Some` when the
    /// original bytes were preserved aside, `None` when even the backup
    /// failed — in which case the original file is untouched and every later
    /// load retries. Never `Ok`: recovery is not claimed unless it happened.
    Parse {
        detail: String,
        backup: Option<PathBuf>,
    },
    /// The file could not be read at all (permissions, transient I/O). No
    /// bytes were observed, so there is nothing to preserve; callers must
    /// still refuse to write derived state.
    Io(std::io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Parse { detail, backup } => match backup {
                Some(p) => write!(
                    f,
                    "unparseable store ({detail}); bytes preserved at {}",
                    p.display()
                ),
                None => write!(
                    f,
                    "unparseable store ({detail}); backup failed, original left in place"
                ),
            },
            ReadError::Io(e) => write!(f, "store unreadable: {e}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Size ceiling for a LOCAL store file.
///
/// Audit #6a: this used to be 8 MB - the size of `drive::MAX_DOMAIN_BYTES`,
/// which is a WIRE cap on one remote sync envelope, not a property of local
/// state. Reusing it here declared a legitimate, perfectly valid local store
/// "corrupt" and rotated the user's real ledger aside as `*.corrupt.json`
/// (dictionary.json / snippets.json / stats_events.json all grow without bound
/// over a multi-year install - stats is deliberately sharded on the wire
/// precisely because one envelope cannot hold it, while the local ledger keeps
/// every event). Local stores therefore get their own, much larger ceiling.
/// The wire cap is unchanged and still enforced where it belongs: on remote
/// payloads, in `drive::MAX_DOMAIN_BYTES` and the envelope decoder.
pub(crate) const MAX_LOCAL_STORE_JSON_LEN: u64 = 64 * 1024 * 1024;

/// Read a JSON store file, distinguishing missing / parseable / corrupt /
/// unreadable. Size is bounded before reading so a pathological file cannot
/// OOM the process; oversized files are treated as corrupt (bytes preserved).
pub(crate) fn read_json_file<T>(path: &Path) -> Result<Option<T>, ReadError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    if !path.exists() {
        return Ok(None);
    }
    let meta = std::fs::metadata(path).map_err(ReadError::Io)?;
    if meta.len() > MAX_LOCAL_STORE_JSON_LEN {
        let backup = backup_corrupt_file(path).ok();
        return Err(ReadError::Parse {
            detail: format!(
                "store file too large ({} bytes, cap {}); treated as corrupt",
                meta.len(),
                MAX_LOCAL_STORE_JSON_LEN
            ),
            backup,
        });
    }
    let data = std::fs::read(path).map_err(ReadError::Io)?;
    match serde_json::from_slice::<T>(&data) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            log::warn!("Corrupt store file {}: {e:?}", path.display());
            let backup = backup_corrupt_file(path).ok();
            if backup.is_none() {
                log::error!(
                    "Could not preserve corrupt store file {}; leaving it in place",
                    path.display()
                );
            }
            Err(ReadError::Parse {
                detail: e.to_string(),
                backup,
            })
        }
    }
}

/// Preserve a corrupt store file under a unique `json.corrupt[.N].json`
/// sibling: rename, falling back to copy+delete (cross-device renames fail).
/// Returns the backup path. On failure the original file is left untouched
/// and an error is returned — the caller must fail closed, never claim
/// recovery, and retry on the next load.
pub(crate) fn backup_corrupt_file(path: &Path) -> Result<PathBuf, std::io::Error> {
    let mut target = path.with_extension("json.corrupt.json");
    let mut counter = 1u32;
    while target.exists() {
        target = path.with_extension(format!("json.corrupt.{counter}.json"));
        counter += 1;
    }
    if std::fs::rename(path, &target).is_ok() {
        log::warn!(
            "Preserved corrupt store file {} as {}",
            path.display(),
            target.display()
        );
        return Ok(target);
    }
    // Fallback for cross-device moves, where rename cannot work. Each outcome
    // is reported honestly: recovery is claimed only when the bytes are
    // verifiably aside AND the original is gone.
    std::fs::copy(path, &target)?;
    if let Err(e) = std::fs::remove_file(path) {
        log::error!(
            "Preserved {} as {} but could not remove the original: {e}; failing closed",
            path.display(),
            target.display()
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!(
                "backup made at {} but original not removed: {e}",
                target.display()
            ),
        ));
    }
    log::warn!(
        "Preserved corrupt store file {} as {} (via copy)",
        path.display(),
        target.display()
    );
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn scratch_path(name: &str) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "fluence-recover-test-{}-{n}-{name}",
            std::process::id()
        ))
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Probe {
        name: String,
        count: u32,
    }

    #[test]
    fn missing_file_returns_none_and_creates_no_backup() {
        let path = scratch_path("missing.json");
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        assert!(matches!(out, Ok(None)), "missing file is Ok(None): {out:?}");
        assert!(
            !path.with_extension("json.corrupt.json").exists(),
            "no backup may be created for a missing file"
        );
    }

    #[test]
    fn valid_json_round_trips() {
        let path = scratch_path("valid.json");
        std::fs::write(&path, r#"{"name":"a","count":3}"#).unwrap();
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        assert_eq!(
            out.unwrap(),
            Some(Probe {
                name: "a".to_string(),
                count: 3
            })
        );
        assert!(
            !path.with_extension("json.corrupt.json").exists(),
            "no backup for parseable files"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn truncated_json_errors_and_preserves_bytes() {
        let path = scratch_path("truncated.json");
        let bytes = b"{\"name\":\"abc\",\"count\":";
        std::fs::write(&path, bytes).unwrap();
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        let backup = match out {
            Err(ReadError::Parse { backup, .. }) => backup,
            other => panic!("truncated JSON must be Err(Parse), got: {other:?}"),
        };
        let backup = backup.expect("backup must succeed here");
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            bytes,
            "backup must hold the exact original bytes"
        );
        assert!(!path.exists(), "corrupt original must be rotated aside");
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn malformed_json_errors_and_preserves_bytes() {
        let path = scratch_path("malformed.json");
        let bytes = b"not json at all {{{";
        std::fs::write(&path, bytes).unwrap();
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        assert!(
            matches!(out, Err(ReadError::Parse { .. })),
            "malformed JSON must be Err(Parse), got: {out:?}"
        );
        let backup = path.with_extension("json.corrupt.json");
        assert_eq!(std::fs::read(&backup).unwrap(), bytes);
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn wrong_shape_json_errors_and_preserves_bytes() {
        // Valid JSON, wrong schema: an array where the struct is expected.
        let path = scratch_path("shape.json");
        let bytes = b"[1, 2, 3]";
        std::fs::write(&path, bytes).unwrap();
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        assert!(
            matches!(out, Err(ReadError::Parse { .. })),
            "schema mismatch must be Err(Parse), got: {out:?}"
        );
        let backup = path.with_extension("json.corrupt.json");
        assert_eq!(std::fs::read(&backup).unwrap(), bytes);
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn empty_file_errors_and_preserves_bytes() {
        // Zero bytes is corrupt, not missing: the file exists.
        let path = scratch_path("empty.json");
        std::fs::write(&path, b"").unwrap();
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        assert!(
            matches!(out, Err(ReadError::Parse { .. })),
            "empty file must be Err(Parse), got: {out:?}"
        );
        let backup = path.with_extension("json.corrupt.json");
        assert!(backup.exists());
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn second_corruption_uniquefies_backup() {
        let path = scratch_path("twice.json");
        std::fs::write(&path, b"{{{bad").unwrap();
        // Occupy the base backup name the way a previous incident would have.
        let first = path.with_extension("json.corrupt.json");
        std::fs::write(&first, b"older incident").unwrap();
        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        assert!(matches!(out, Err(ReadError::Parse { .. })));
        let second = path.with_extension("json.corrupt.1.json");
        assert!(
            second.exists(),
            "second incident must not overwrite the first backup"
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"older incident");
        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
    }

    #[test]
    fn backup_of_missing_source_errors_without_creating_files() {
        let path = scratch_path("nosuch.json");
        let out = backup_corrupt_file(&path);
        assert!(out.is_err(), "nothing to back up must be an error");
        assert!(
            !path.with_extension("json.corrupt.json").exists(),
            "failed backup must not create phantom files"
        );
    }

    /// Audit #6a: the 8 MiB WIRE cap must never be applied to a local store.
    /// A valid store larger than one remote envelope (a long-lived stats
    /// ledger, a large dictionary) used to be declared corrupt, and its bytes
    /// were rotated aside - real user data, quarantined for no reason.
    #[test]
    fn a_valid_local_store_larger_than_the_wire_cap_is_not_corrupt() {
        let wire_cap = crate::sync::drive::MAX_DOMAIN_BYTES as u64;
        assert!(
            MAX_LOCAL_STORE_JSON_LEN > wire_cap,
            "the local cap must be its own, larger bound than the wire cap"
        );
        let path = scratch_path("large.json");
        // Comfortably over the wire cap, far under the local cap.
        let filler = "x".repeat((wire_cap as usize) + 1_048_576);
        let bytes = format!(r#"{{"name":"{filler}","count":7}}"#);
        assert!(
            bytes.len() as u64 > wire_cap,
            "precondition: the fixture is over the wire cap"
        );
        std::fs::write(&path, &bytes).unwrap();

        let out: Result<Option<Probe>, ReadError> = read_json_file(&path);
        match out {
            Ok(Some(probe)) => {
                assert_eq!(probe.count, 7);
                assert_eq!(probe.name.len(), filler.len());
            }
            other => panic!("a valid oversized local store must parse, got: {other:?}"),
        }
        assert!(
            !path.with_extension("json.corrupt.json").exists(),
            "an oversized-but-valid store must never be quarantined as corrupt"
        );
        assert!(path.exists(), "the original file must be left in place");
        let _ = std::fs::remove_file(&path);
    }
}
