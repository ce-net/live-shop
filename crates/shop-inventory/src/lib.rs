//! The inventory state machine. `on_hand` is stock not yet sold, `reserved` the part of it held for running
//! checkouts, `committed` what was sold. A `Reserve` is accepted only when every line has
//! `on_hand - reserved >= qty`; it is idempotent on the order. Invariant: `reserved <= on_hand` for every sku.
use serde::{Deserialize, Serialize};
use shop_inventory_api::{InvError, Level, Reservation};
use shop_inventory_peer_api::Entry;
use shop_types::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Stock {
    pub on_hand: u32,
    pub reserved: u32,
    pub committed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Resv {
    pub lines: Vec<Line>,
    pub expires: u64,
    pub seq: u64,
    pub committed: bool,
    pub released: bool,
}

#[derive(Default)]
pub struct Inv {
    pub stock: BTreeMap<(String, String), Stock>,
    pub resv: BTreeMap<(String, String), Resv>,
    pub restocked: BTreeSet<String>,
    pub as_of: u64,
}

#[derive(Debug, Clone)]
pub enum Out {
    Resv(Reservation),
    Seq(u64),
    Level(Level),
    Err(InvError),
}

impl Inv {
    pub fn level(&self, tenant: &str, sku: &Sku) -> Level {
        let s = self.stock.get(&(tenant.to_string(), sku.0.clone())).cloned().unwrap_or_default();
        Level { sku: sku.clone(), on_hand: s.on_hand, reserved: s.reserved, committed: s.committed, as_of: self.as_of }
    }
    fn view(order: &str, r: &Resv) -> Reservation {
        Reservation { order: OrderId(order.into()), lines: r.lines.clone(), expires: r.expires, seq: r.seq, committed: r.committed }
    }
    /// For the proof: every sku with `reserved > on_hand` (must be empty).
    pub fn violations(&self) -> Vec<String> {
        self.stock.iter().filter(|(_, s)| s.reserved > s.on_hand).map(|(k, s)| format!("{}/{}: reserved {} > on_hand {}", k.0, k.1, s.reserved, s.on_hand)).collect()
    }
    fn release(&mut self, k: &(String, String)) {
        let Some(r) = self.resv.get_mut(k) else { return };
        if r.committed || r.released {
            return;
        }
        r.released = true;
        for l in r.lines.clone() {
            if let Some(s) = self.stock.get_mut(&(k.0.clone(), l.sku.0.clone())) {
                s.reserved -= l.qty;
            }
        }
    }
}

impl shop_replog::Machine for Inv {
    type Entry = Entry;
    type Out = Out;
    fn apply(&mut self, idx: u64, e: &Entry) -> Out {
        self.as_of = idx;
        match e {
            Entry::Reserve { tenant, order, lines, expires } => {
                let k = (tenant.0.clone(), order.0.clone());
                if let Some(r) = self.resv.get(&k) {
                    return if r.released { Out::Err(InvError::Expired(order.clone())) } else { Out::Resv(Inv::view(&order.0, r)) };
                }
                for l in lines {
                    let s = self.stock.get(&(tenant.0.clone(), l.sku.0.clone())).cloned().unwrap_or_default();
                    let need: u32 = lines.iter().filter(|x| x.sku == l.sku).map(|x| x.qty).sum();
                    if s.on_hand - s.reserved < need {
                        return Out::Err(InvError::OutOfStock { sku: l.sku.clone(), available: s.on_hand - s.reserved });
                    }
                }
                for l in lines {
                    self.stock.entry((tenant.0.clone(), l.sku.0.clone())).or_default().reserved += l.qty;
                }
                let r = Resv { lines: lines.clone(), expires: *expires, seq: idx, committed: false, released: false };
                let v = Inv::view(&order.0, &r);
                self.resv.insert(k, r);
                Out::Resv(v)
            }
            Entry::Commit { tenant, order } => {
                let k = (tenant.0.clone(), order.0.clone());
                match self.resv.get(&k).cloned() {
                    None => Out::Err(InvError::Unknown(order.clone())),
                    Some(r) if r.released => Out::Err(InvError::Expired(order.clone())),
                    Some(r) if r.committed => Out::Seq(r.seq),
                    Some(r) => {
                        for l in &r.lines {
                            let s = self.stock.entry((tenant.0.clone(), l.sku.0.clone())).or_default();
                            s.reserved -= l.qty;
                            s.on_hand -= l.qty;
                            s.committed += l.qty;
                        }
                        self.resv.get_mut(&k).unwrap().committed = true;
                        Out::Seq(idx)
                    }
                }
            }
            Entry::Release { tenant, order } => {
                let k = (tenant.0.clone(), order.0.clone());
                if !self.resv.contains_key(&k) {
                    // a tombstone: a Reserve that arrives after its order was compensated is refused
                    self.resv.insert(k, Resv { lines: vec![], expires: 0, seq: idx, committed: false, released: true });
                } else {
                    self.release(&k);
                }
                Out::Seq(idx)
            }
            Entry::Restock { tenant, sku, add, key } => {
                if self.restocked.insert(format!("{}/{}", tenant.0, key.0)) {
                    self.stock.entry((tenant.0.clone(), sku.0.clone())).or_default().on_hand += add;
                }
                Out::Level(self.level(&tenant.0, sku))
            }
            Entry::Expire { now_ms } => {
                let due: Vec<_> = self.resv.iter().filter(|(_, r)| !r.committed && !r.released && r.expires < *now_ms).map(|(k, _)| k.clone()).collect();
                for k in due {
                    self.release(&k);
                }
                Out::Seq(idx)
            }
        }
    }
}
