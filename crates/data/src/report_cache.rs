//! Bounded, input-keyed memory reports. Failures are never inserted.
use crate::stable_hash::{Fnv64, bars_digest};
use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, Weak},
};
use tr_core::{Bar, Timeframe};
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReportKey {
    pub kind: String,
    pub symbol: String,
    pub tf: Timeframe,
    pub source: String,
    pub closed_n: usize,
    pub first_ts: i64,
    pub last_closed_ts: i64,
    pub inputs: u64,
    pub epoch: u32,
    pub params: u64,
    pub code_version: u32,
    pub day: Option<chrono::NaiveDate>,
}
impl ReportKey {
    pub fn new(kind: &str, symbol: &str, tf: Timeframe, source: &str, bars: &[Bar], params: &str) -> Self {
        let mut h = Fnv64::default();
        h.write_str(params);
        Self {
            kind: kind.into(),
            symbol: symbol.into(),
            tf,
            source: source.into(),
            closed_n: bars.len(),
            first_ts: bars.first().map_or(0, |b| b.ts),
            last_closed_ts: bars.last().map_or(0, |b| b.ts),
            inputs: bars_digest(bars),
            epoch: 0,
            params: h.finish(),
            code_version: 1,
            day: None,
        }
    }
}
type Entry = (ReportKey, Arc<dyn Any + Send + Sync>);
/// The compute lock deduplicates concurrent identical work; callers run this only
/// on spawn_blocking. Memory hits need just the short entries lock.
pub struct ReportCache {
    entries: Mutex<VecDeque<Entry>>,
    compute: Mutex<HashMap<ReportKey, Weak<Mutex<()>>>>,
    capacity: usize,
}
impl Default for ReportCache {
    fn default() -> Self {
        Self::new(256)
    }
}
impl ReportCache {
    pub fn new(capacity: usize) -> Self {
        Self { entries: Mutex::new(VecDeque::new()), compute: Mutex::new(HashMap::new()), capacity }
    }
    pub fn get<T: Any + Send + Sync>(&self, key: &ReportKey) -> Option<Arc<T>> {
        let mut entries = self.entries.lock().unwrap();
        let i = entries.iter().position(|(k, v)| k == key && v.is::<T>())?;
        let entry = entries.remove(i)?;
        let value = entry.1.clone().downcast().ok();
        entries.push_back(entry);
        value
    }
    pub fn get_or_compute<T: Any + Send + Sync, E>(
        &self,
        key: ReportKey,
        f: impl FnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E> {
        if let Some(hit) = self.get(&key) {
            return Ok(hit);
        }
        let slot = {
            let mut pending = self.compute.lock().unwrap();
            pending.retain(|_, v| v.strong_count() > 0);
            let slot = pending.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| Arc::new(Mutex::new(())));
            pending.insert(key.clone(), Arc::downgrade(&slot));
            slot
        };
        let _guard = slot.lock().unwrap();
        if let Some(hit) = self.get(&key) {
            return Ok(hit);
        }
        let value = Arc::new(f()?);
        let mut entries = self.entries.lock().unwrap();
        entries.push_back((key, value.clone()));
        while entries.len() > self.capacity {
            entries.pop_front();
        }
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    fn key() -> ReportKey {
        ReportKey::new("clock", "AAPL", Timeframe::M1, "yahoo", &[], "20")
    }
    #[test]
    fn identical_key_returns_same_arc_and_errors_are_not_cached() {
        let c = ReportCache::new(2);
        let k = key();
        let a = c.get_or_compute(k.clone(), || Ok::<_, ()>(3)).unwrap();
        let b = c.get_or_compute::<_, ()>(k.clone(), || panic!("cache miss")).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let mut bad = k;
        bad.params += 1;
        for _ in 0..2 {
            assert!(c.get_or_compute::<u32, _>(bad.clone(), || Err(())).is_err());
        }
    }
    #[test]
    fn every_identity_field_invalidates() {
        let c = ReportCache::default();
        let k = key();
        c.get_or_compute(k.clone(), || Ok::<_, ()>(1_u32)).unwrap();
        for i in 0..13 {
            let mut next = k.clone();
            match i {
                0 => next.kind.push('x'),
                1 => next.symbol.push('x'),
                2 => next.tf = Timeframe::M5,
                3 => next.source.push('x'),
                4 => next.closed_n += 1,
                5 => next.first_ts += 1,
                6 => next.last_closed_ts += 1,
                7 => next.inputs += 1,
                8 => next.epoch += 1,
                9 => next.params += 1,
                10 => next.code_version += 1,
                11 => next.day = Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
                _ => next.inputs ^= 0xff,
            };
            assert!(c.get::<u32>(&next).is_none());
        }
    }
    #[test]
    fn concurrent_identical_work_computes_once() {
        let c = Arc::new(ReportCache::default());
        let n = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let c = c.clone();
                let n = n.clone();
                scope.spawn(move || {
                    c.get_or_compute(key(), || {
                        n.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, ()>(42)
                    })
                    .unwrap();
                });
            }
        });
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn lru_evicts_oldest_hit() {
        let c = ReportCache::new(2);
        let a = key();
        let mut b = a.clone();
        b.params = 2;
        let mut d = a.clone();
        d.params = 3;
        for k in [&a, &b] {
            c.get_or_compute(k.clone(), || Ok::<_, ()>(1_u32)).unwrap();
        }
        c.get::<u32>(&a).unwrap();
        c.get_or_compute(d, || Ok::<_, ()>(2_u32)).unwrap();
        assert!(c.get::<u32>(&b).is_none());
    }
}
