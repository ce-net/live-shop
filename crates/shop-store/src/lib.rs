//! Records for the cells of slice 1 that run one instance per node: a map persisted as an append-only JSON-lines
//! file (survives a kill; the last complete line wins), plus [`Hlc`] / [`Lww`] for values that merge per field.
//! In slice 2 these records move to `state.store@2`, which has the same last-writer-wins-by-HLC semantics.
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

pub struct Store<V> {
    map: BTreeMap<String, V>,
    file: File,
}

#[derive(Serialize, Deserialize)]
struct Row<V> {
    k: String,
    v: Option<V>,
}

impl<V: Serialize + DeserializeOwned + Clone> Store<V> {
    pub fn open(path: &Path) -> Store<V> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).expect("store dir");
        }
        let mut map = BTreeMap::new();
        if let Ok(f) = File::open(path) {
            for line in BufReader::new(f).lines().map_while(Result::ok) {
                if let Ok(r) = serde_json::from_str::<Row<V>>(&line) {
                    match r.v {
                        Some(v) => map.insert(r.k, v),
                        None => map.remove(&r.k),
                    };
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(path).expect("store file");
        Store { map, file }
    }
    pub fn get(&self, k: &str) -> Option<&V> {
        self.map.get(k)
    }
    pub fn put(&mut self, k: &str, v: V) {
        let mut line = serde_json::to_vec(&Row { k: k.to_string(), v: Some(&v) }).unwrap();
        line.push(b'\n');
        self.file.write_all(&line).expect("store write");
        self.map.insert(k.to_string(), v);
    }
    pub fn remove(&mut self, k: &str) {
        let mut line = serde_json::to_vec(&Row::<V> { k: k.to_string(), v: None }).unwrap();
        line.push(b'\n');
        self.file.write_all(&line).expect("store write");
        self.map.remove(k);
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.map.iter()
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Hybrid logical clock stamp: wall milliseconds, a counter for ties, and the writer's node as the last tie-break.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct Hlc {
    pub ms: u64,
    pub c: u32,
    pub node: String,
}

/// Makes stamps that never go backwards and stay ahead of every stamp it has seen.
pub struct Clock {
    last: Hlc,
}

impl Clock {
    pub fn new(node: &str) -> Clock {
        Clock { last: Hlc { ms: 0, c: 0, node: node.into() } }
    }
    pub fn now(&mut self, wall_ms: u64) -> Hlc {
        if wall_ms > self.last.ms {
            self.last.ms = wall_ms;
            self.last.c = 0;
        } else {
            self.last.c += 1;
        }
        self.last.clone()
    }
    pub fn observe(&mut self, h: &Hlc) {
        if (h.ms, h.c) > (self.last.ms, self.last.c) {
            self.last.ms = h.ms;
            self.last.c = h.c;
        }
    }
}

/// A value with the stamp of its write. Merge keeps the later stamp, so replicas converge in any order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lww<T> {
    pub v: T,
    pub at: Hlc,
}

impl<T: Clone> Lww<T> {
    pub fn merge(&mut self, o: &Lww<T>) {
        if o.at > self.at {
            *self = o.clone();
        }
    }
}

/// Merge two maps of per-key last-writer-wins values (a cart's lines: sku to quantity).
pub fn merge_map<T: Clone>(a: &mut BTreeMap<String, Lww<T>>, b: &BTreeMap<String, Lww<T>>) {
    for (k, v) in b {
        match a.get_mut(k) {
            Some(x) => x.merge(v),
            None => {
                a.insert(k.clone(), v.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_survives_reopen() {
        let p = std::env::temp_dir().join(format!("shop-store-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        {
            let mut s: Store<u32> = Store::open(&p);
            s.put("a", 1);
            s.put("b", 2);
            s.put("a", 3);
            s.remove("b");
        }
        let s: Store<u32> = Store::open(&p);
        assert_eq!(s.get("a"), Some(&3));
        assert_eq!(s.get("b"), None);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn per_line_merge_converges() {
        // two replicas edit one cart: a sets tent=1 then 2, b sets stove=1 and later tent=5
        let mut ca = Clock::new("a");
        let mut cb = Clock::new("b");
        let mut a = BTreeMap::new();
        let mut b = BTreeMap::new();
        a.insert("tent".to_string(), Lww { v: 1u32, at: ca.now(100) });
        a.insert("tent".to_string(), Lww { v: 2u32, at: ca.now(100) });
        b.insert("stove".to_string(), Lww { v: 1u32, at: cb.now(90) });
        b.insert("tent".to_string(), Lww { v: 5u32, at: cb.now(150) });
        let (mut ab, mut ba) = (a.clone(), b.clone());
        merge_map(&mut ab, &b);
        merge_map(&mut ba, &a);
        assert_eq!(ab, ba);
        assert_eq!(ab["tent"].v, 5);
        assert_eq!(ab["stove"].v, 1);
        // equal wall time: the counter, then the node, decides; still the same on both sides
        let mut x = BTreeMap::from([("k".to_string(), Lww { v: 1u32, at: Hlc { ms: 5, c: 0, node: "a".into() } })]);
        let y = BTreeMap::from([("k".to_string(), Lww { v: 2u32, at: Hlc { ms: 5, c: 0, node: "b".into() } })]);
        let mut y2 = y.clone();
        merge_map(&mut x, &y);
        merge_map(&mut y2, &BTreeMap::from([("k".to_string(), Lww { v: 1u32, at: Hlc { ms: 5, c: 0, node: "a".into() } })]));
        assert_eq!(x, y2);
    }
}
