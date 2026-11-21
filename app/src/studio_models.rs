//! Immutable, versioned store of frozen residual models (`root/studio/models/{slug}-v{n}.json`).
//!
//! * A saved file is never rewritten: a second save of the same name becomes `-v{n+1}` with
//!   `parent = -v{n}`. The bytes go to a unique temp file that is then `hard_link`ed to the final name,
//!   which fails if the target exists: the no-overwrite rule holds across processes, not only under
//!   the in-process lock.
//! * Nothing here deletes a file. `list` reports unreadable files as `Corrupt` and leaves them alone.
//! * Functions take a `root` so tests never touch `TR_DATA_DIR` or the user's data; production passes
//!   `worker::data_dir()`.
//! * The JSON holds symbols, numbers, sources and receipts only: no credentials or tokens.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tr_core::Timeframe;
use tr_data::studio_residual::{FrozenFit, RESID_SCHEMA, valid_id, valid_name};

/// Files larger than this are refused as corrupt (a model holds ≤ 5,000 residuals).
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

static SAVE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq)]
pub struct SavedFit {
    pub id: String,
    pub digest: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelListing {
    pub id: String,
    pub symbol: String,
    pub tf: Timeframe,
    pub trained_thru: i64,
    pub saved_at: i64,
    /// `Some` for a file that could not be loaded; it is listed, never deleted.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableKind {
    Missing,
    Corrupt,
    SchemaTooNew,
    DigestMismatch,
    InvalidId,
}
impl UnavailableKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
            Self::SchemaTooNew => "schema too new",
            Self::DigestMismatch => "digest mismatch",
            Self::InvalidId => "invalid id",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelUnavailable {
    pub kind: UnavailableKind,
    pub id: String,
    pub message: String,
}

pub fn models_dir(root: &Path) -> PathBuf {
    root.join("studio").join("models")
}

fn path_of(root: &Path, id: &str) -> PathBuf {
    models_dir(root).join(format!("{id}.json"))
}

/// Highest existing version of `slug`, from the file names.
fn latest_version(dir: &Path, slug: &str) -> u32 {
    let Ok(read) = std::fs::read_dir(dir) else { return 0 };
    read.filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string))
        .filter(|stem| valid_id(stem))
        .filter_map(|stem| {
            let (s, n) = stem.rsplit_once("-v")?;
            (s == slug).then(|| n.parse::<u32>().ok()).flatten()
        })
        .max()
        .unwrap_or(0)
}

/// Saves `fit` as the next version of `fit.name`. Sets `id` / `parent` (outside the content digest),
/// validates, and refuses to overwrite.
pub fn save(root: &Path, mut fit: FrozenFit) -> Result<SavedFit, String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !valid_name(&fit.name) {
        return Err("model name must be 1–40 characters of A–Z a–z 0–9 _ -".into());
    }
    let dir = models_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let latest = latest_version(&dir, &fit.name);
    let next = latest + 1;
    if next > 99_999 {
        return Err("version limit reached for this name".into());
    }
    fit.id = format!("{}-v{next}", fit.name);
    fit.parent = (latest > 0).then(|| format!("{}-v{latest}", fit.name));
    fit.validate()?;
    let bytes = serde_json::to_vec_pretty(&fit).map_err(|e| format!("cannot serialise model: {e}"))?;
    let path = path_of(root, &fit.id);
    write_new(&path, &bytes).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} already exists; saved models are never overwritten", fit.id),
        _ => format!("cannot write {}: {e}", path.display()),
    })?;
    Ok(SavedFit { id: fit.id, digest: fit.content_digest })
}

/// Creates `path` with `bytes`, failing with `AlreadyExists` if it exists, even against another process:
/// the content is fully written and synced to a unique temp file, then `hard_link`ed (atomic, never
/// replaces) and the temp removed.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().unwrap_or(Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
    // A temp-name collision is retried with a fresh counter; it never surfaces as "already exists".
    let mut attempts = 0;
    let tmp = loop {
        let tmp = dir.join(format!(".{stem}.{}.{}.tmp", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(mut f) => {
                let written = f.write_all(bytes).and_then(|()| f.sync_all());
                if let Err(e) = written {
                    drop(f);
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
                break tmp;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(std::io::Error::other(format!("cannot create a unique temporary file in {}", dir.display())));
            }
            Err(e) => return Err(e),
        }
    };
    let result = match std::fs::hard_link(&tmp, path) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists && e.kind() != std::io::ErrorKind::NotFound => {
            // No hard-link support (Unsupported / PermissionDenied / …): create the final path exclusively.
            // `create_new` still never overwrites.
            (|| {
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
                f.write_all(bytes)?;
                f.sync_all()
            })()
        }
        other => other,
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

fn unavailable(kind: UnavailableKind, id: &str, message: impl Into<String>) -> ModelUnavailable {
    ModelUnavailable { kind, id: id.into(), message: message.into() }
}

/// Reads and verifies one model: id shape, size, JSON, schema, digest, then full validation.
pub fn load(root: &Path, id: &str) -> Result<FrozenFit, ModelUnavailable> {
    use UnavailableKind::*;
    if !valid_id(id) {
        return Err(unavailable(InvalidId, id, "ids look like AAPL-v-SPY-v1"));
    }
    let path = path_of(root, id);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(unavailable(Missing, id, "no such saved model"));
        }
        Err(e) => return Err(unavailable(Corrupt, id, format!("cannot read: {e}"))),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Err(unavailable(Corrupt, id, format!("file is {} bytes; the limit is {MAX_FILE_BYTES}", meta.len())));
    }
    let bytes = std::fs::read(&path).map_err(|e| unavailable(Corrupt, id, format!("cannot read: {e}")))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| unavailable(Corrupt, id, format!("not valid JSON: {e}")))?;
    let schema = value.get("schema").and_then(|s| s.as_u64()).unwrap_or(0);
    if schema > u64::from(RESID_SCHEMA) {
        return Err(unavailable(
            SchemaTooNew,
            id,
            format!("schema {schema} was written by a newer build (this build reads {RESID_SCHEMA})"),
        ));
    }
    let fit: FrozenFit =
        serde_json::from_value(value).map_err(|e| unavailable(Corrupt, id, format!("unreadable model: {e}")))?;
    if fit.compute_digest() != fit.content_digest {
        return Err(unavailable(DigestMismatch, id, "the saved content does not match its digest"));
    }
    fit.validate().map_err(|e| unavailable(Corrupt, id, e))?;
    if fit.id != id {
        return Err(unavailable(Corrupt, id, format!("the file holds model {}, not {id}", fit.id)));
    }
    Ok(fit)
}

/// Every `*.json` with a valid id stem, sorted by id. A file that does not load is listed with its
/// problem; it is never removed.
pub fn list(root: &Path) -> Result<Vec<ModelListing>, String> {
    let dir = models_dir(root);
    let read = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
    };
    let mut ids: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(str::to_string))
        .filter(|stem| valid_id(stem))
        .collect();
    ids.sort();
    Ok(ids
        .into_iter()
        .map(|id| match load(root, &id) {
            Ok(f) => ModelListing {
                id,
                symbol: f.model.symbol,
                tf: f.model.tf,
                trained_thru: f.trained_thru,
                saved_at: f.saved_at,
                problem: None,
            },
            Err(e) => ModelListing {
                id,
                symbol: String::new(),
                tf: Timeframe::D1,
                trained_thru: 0,
                saved_at: 0,
                problem: Some(format!("{}: {}", e.kind.label(), e.message)),
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_data::studio_lab as lab;

    /// A fresh directory under the OS temp dir; never `TR_DATA_DIR`.
    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "tr-resid-test-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn fit(name: &str) -> FrozenFit {
        let (spec, target, rows) = tr_perf::fixtures::lab_model(600);
        let close = target.bars.last().unwrap().ts + 86_400;
        let r = lab::compute(&target, &rows, &spec, Timeframe::D1, true, "benchmark fixture only", close, None, 7);
        tr_data::studio_residual::freeze_lab(&r, Some(name), Some(50), 1_700_000_000).unwrap()
    }

    #[test]
    fn residual_store_roundtrip_and_corrupt_schema() {
        let t = Temp::new("store");
        let saved = save(&t.0, fit("demo")).unwrap();
        assert_eq!(saved.id, "demo-v1");
        let back = load(&t.0, "demo-v1").unwrap();
        assert_eq!(back.id, "demo-v1");
        assert_eq!(back.parent, None);
        assert_eq!(back.content_digest, saved.digest);
        let mut want = fit("demo");
        (want.id, want.parent) = ("demo-v1".into(), None);
        assert_eq!(back, want, "roundtrip gives an equal fit");
        // Versions are immutable: a second save is v2 with parent v1, v1 bytes unchanged.
        let v1_bytes = std::fs::read(path_of(&t.0, "demo-v1")).unwrap();
        let second = save(&t.0, fit("demo")).unwrap();
        assert_eq!(second.id, "demo-v2");
        assert_eq!(std::fs::read(path_of(&t.0, "demo-v1")).unwrap(), v1_bytes);
        assert_eq!(load(&t.0, "demo-v2").unwrap().parent, Some("demo-v1".into()));
        // A different name starts its own chain.
        assert_eq!(save(&t.0, fit("other")).unwrap().id, "other-v1");
        // Missing and invalid ids.
        assert_eq!(load(&t.0, "nope-v1").unwrap_err().kind, UnavailableKind::Missing);
        for bad in ["../x", "a/b-v1", "x", "x-v0", ""] {
            assert_eq!(load(&t.0, bad).unwrap_err().kind, UnavailableKind::InvalidId, "{bad}");
        }
        let text = String::from_utf8(v1_bytes).unwrap();
        // Truncated JSON → Corrupt.
        std::fs::write(path_of(&t.0, "trunc-v1"), &text[..text.len() / 2]).unwrap();
        assert_eq!(load(&t.0, "trunc-v1").unwrap_err().kind, UnavailableKind::Corrupt);
        // schema 2 → SchemaTooNew.
        std::fs::write(path_of(&t.0, "future-v1"), text.replacen("\"schema\": 1", "\"schema\": 2", 1)).unwrap();
        assert_eq!(load(&t.0, "future-v1").unwrap_err().kind, UnavailableKind::SchemaTooNew);
        // An edited coefficient → DigestMismatch.
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["id"] = "edited-v1".into();
        v["model"]["coefficients"][1] = serde_json::json!(123.456);
        std::fs::write(path_of(&t.0, "edited-v1"), serde_json::to_vec(&v).unwrap()).unwrap();
        assert_eq!(load(&t.0, "edited-v1").unwrap_err().kind, UnavailableKind::DigestMismatch);
        // A file under the wrong name is corrupt, not loaded.
        std::fs::write(path_of(&t.0, "alias-v1"), &text).unwrap();
        assert_eq!(load(&t.0, "alias-v1").unwrap_err().kind, UnavailableKind::Corrupt);
        // List reports the corrupt files and deletes nothing.
        let before = std::fs::read_dir(models_dir(&t.0)).unwrap().count();
        let listing = list(&t.0).unwrap();
        assert_eq!(std::fs::read_dir(models_dir(&t.0)).unwrap().count(), before);
        assert_eq!(listing.len(), before);
        let ok: Vec<_> = listing.iter().filter(|l| l.problem.is_none()).map(|l| l.id.as_str()).collect();
        assert_eq!(ok, ["demo-v1", "demo-v2", "other-v1"]);
        let bad = listing.iter().find(|l| l.id == "trunc-v1").unwrap();
        assert!(bad.problem.as_ref().unwrap().starts_with("corrupt"));
        assert_eq!(listing.iter().find(|l| l.id == "demo-v1").unwrap().tf, Timeframe::D1);
        // An empty root lists nothing.
        let empty = Temp::new("empty");
        assert_eq!(list(&empty.0).unwrap(), vec![]);
    }

    #[test]
    fn write_new_never_replaces_an_existing_file() {
        let t = Temp::new("write-new");
        let path = t.0.join("a-v1.json");
        write_new(&path, b"first").unwrap();
        let err = write_new(&path, b"second").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        let left: Vec<_> = std::fs::read_dir(&t.0).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from("a-v1.json")], "no temp files left behind");
    }

    #[test]
    fn save_refuses_bad_names_and_never_overwrites() {
        let t = Temp::new("names");
        let mut f = fit("x");
        f.name = "bad name".into();
        assert!(save(&t.0, f).is_err());
        let mut f = fit("x");
        f.model.coefficients[0] += 1.; // digest no longer matches
        assert!(save(&t.0, f).is_err());
        assert!(list(&t.0).unwrap().is_empty());
        // Concurrent saves of one name get distinct versions.
        let root = t.0.clone();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (root, f) = (root.clone(), fit("race"));
                std::thread::spawn(move || save(&root, f).unwrap().id)
            })
            .collect();
        let mut ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, ["race-v1", "race-v2", "race-v3", "race-v4"]);
    }
}
