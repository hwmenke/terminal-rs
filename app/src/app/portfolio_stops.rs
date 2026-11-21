//! Local planning data only; loading never writes.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    accounts: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default)]
    budgets: BTreeMap<String, f64>,
}

pub(super) struct Store {
    pub(super) path: PathBuf,
    document: Document,
    corrupt: bool,
}

impl Store {
    pub(super) fn load(dir: PathBuf) -> Self {
        let path = dir.join("portfolio_stops.json");
        let loaded = fs::read(&path).and_then(|bytes| {
            let doc: Document = serde_json::from_slice(&bytes)?;
            if doc.version != 1
                || doc.accounts.values().flat_map(|m| m.values()).any(|v| !v.is_finite() || *v <= 0.0)
                || doc.budgets.values().any(|v| !v.is_finite() || *v < 0.0)
            {
                return Err(io::Error::other("Invalid planning stops"));
            }
            Ok(doc)
        });
        let corrupt = loaded.as_ref().is_err_and(|e| e.kind() != io::ErrorKind::NotFound);
        Self {
            path,
            document: loaded.unwrap_or(Document { version: 1, accounts: BTreeMap::new(), budgets: BTreeMap::new() }),
            corrupt,
        }
    }

    pub(super) fn get(&self, account: &str, symbol: &str) -> Option<f64> {
        self.document.accounts.get(account)?.get(symbol).copied()
    }

    pub(super) fn budget(&self, account: &str) -> Option<f64> {
        self.document.budgets.get(account).copied()
    }

    pub(super) fn set_budget(&mut self, account: &str, value: Option<f64>) -> io::Result<()> {
        if value.is_some_and(|v| !v.is_finite() || v < 0.0) {
            return Err(io::Error::other("Budget must be finite and nonnegative"));
        }
        let mut next = self.document.clone();
        if let Some(value) = value {
            next.budgets.insert(account.into(), value);
        } else {
            next.budgets.remove(account);
        }
        self.save(next)
    }

    pub(super) fn stops(&self) -> tr_core::portfolio_risk::Stops {
        self.document
            .accounts
            .iter()
            .flat_map(|(a, symbols)| symbols.iter().map(move |(s, v)| ((a.clone(), s.clone()), *v)))
            .collect()
    }

    /// Change exactly one key, committing in-memory state only after a successful save.
    pub(super) fn set(&mut self, account: &str, symbol: &str, value: Option<f64>) -> io::Result<()> {
        if value.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(io::Error::other("Planning stop must be finite and positive"));
        }
        let mut next = self.document.clone();
        let symbols = next.accounts.entry(account.into()).or_default();
        if let Some(value) = value {
            symbols.insert(symbol.into(), value);
        } else {
            symbols.remove(symbol);
        }
        self.save(next)
    }

    fn save(&mut self, next: Document) -> io::Result<()> {
        fs::create_dir_all(self.path.parent().unwrap())?;
        let temp = self.path.with_extension("json.tmp");
        let result = (|| {
            use io::Write;
            let mut file = fs::File::create(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(&next)?)?;
            file.sync_all()?;
            if self.corrupt && self.path.exists() {
                // Never replace an earlier backup.
                let mut backup = self.path.with_extension("json.corrupt");
                let mut index = 0;
                while backup.exists() {
                    index += 1;
                    backup = self.path.with_extension(format!("json.corrupt.{index}"));
                }
                fs::copy(&self.path, backup)?;
            }
            fs::rename(&temp, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result?;
        self.document = next;
        self.corrupt = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_files_load_and_budgets_are_validated() {
        let dir = std::env::temp_dir().join(format!("planning-budget-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("portfolio_stops.json");
        fs::write(&path, br#"{"version":1,"accounts":{"a":{"X":12.0}}}"#).unwrap();
        let mut store = Store::load(dir.clone());
        assert_eq!(store.get("a", "X"), Some(12.0));
        assert_eq!(store.budget("a"), None);
        store.set_budget("a", Some(25.0)).unwrap();
        for value in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(store.set_budget("a", Some(value)).is_err());
        }
        assert_eq!(Store::load(dir.clone()).budget("a"), Some(25.0));
        store.set_budget("b", Some(0.0)).unwrap();
        store.set_budget("a", None).unwrap();
        let store = Store::load(dir.clone());
        assert_eq!(store.budget("a"), None);
        assert_eq!(store.budget("b"), Some(0.0));
        assert_eq!(store.get("a", "X"), Some(12.0));
        fs::write(&path, br#"{"version":1,"accounts":{},"budgets":{"a":-1}}"#).unwrap();
        assert!(Store::load(dir.clone()).corrupt);
        fs::write(&path, br#"{"version":1,"accounts":{},"unknown":{}}"#).unwrap();
        assert!(Store::load(dir.clone()).corrupt);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn persistence_is_isolated_and_load_never_writes() {
        let dir = std::env::temp_dir().join(format!("planning-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut store = Store::load(dir.clone());
        assert!(!dir.exists());
        store.set("a", "X", Some(12.0)).unwrap();
        store.set("b", "X", Some(14.0)).unwrap();
        store.set("a", "absent", Some(9.0)).unwrap();
        let mut store = Store::load(dir.clone());
        assert_eq!(store.get("a", "X"), Some(12.0));
        store.set("a", "X", None).unwrap();
        assert_eq!(store.get("b", "X"), Some(14.0));
        assert_eq!(store.get("a", "absent"), Some(9.0));
        fs::write(&store.path, b"broken").unwrap();
        let mut store = Store::load(dir.clone());
        assert!(store.stops().is_empty());
        assert_eq!(fs::read(&store.path).unwrap(), b"broken");
        assert!(!store.path.with_extension("json.corrupt").exists());
        store.set("a", "X", Some(1.0)).unwrap();
        assert_eq!(fs::read(store.path.with_extension("json.corrupt")).unwrap(), b"broken");
        fs::write(dir.join("settings.json"), b"untouched").unwrap();
        let old = fs::read(&store.path).unwrap();
        fs::create_dir(store.path.with_extension("json.tmp")).unwrap();
        assert!(store.set("a", "X", Some(2.0)).is_err());
        assert_eq!(store.get("a", "X"), Some(1.0));
        assert_eq!(fs::read(&store.path).unwrap(), old);
        assert_eq!(fs::read(dir.join("settings.json")).unwrap(), b"untouched");
        fs::remove_dir_all(dir).unwrap();
    }
}
