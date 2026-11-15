//! Scenarios of the chaos proof. Everything is read back through typed ops; nothing reads a cell's files.
use serde::Serialize;
use shop_catalog_api::ListReq;
use shop_gateway_api::{ops as gw, CartSetReq, CheckoutReq, GwError, OrderReq, SignInReq};
use shop_inventory_api::{ops as inv, LevelsReq};
use shop_net::{apply, operator, seed, shop_slice1, Network, Procs, Runner};
use shop_orders_api::{ops as orders, OrderView, Stage, TestToken};
use shop_pay_sim_api::SimKind;
use shop_types::*;
use shop_wire::chain::Holder;
use shop_wire::{Client, Replicas};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// xorshift64*: the whole run is a function of the printed seed.
#[derive(Clone)]
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub what: &'static str,
    pub ok: bool,
    pub numbers: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Buyer {
    pub n: usize,
    pub checkout: String,
    pub token: String,
    pub attempts: u32,
    pub orders_seen: BTreeSet<String>,
    pub answer: Option<OrderView>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub scenario: &'static str,
    pub seed: u64,
    pub unsafe_no_quorum: bool,
    pub buyers: usize,
    pub stock: u32,
    pub timeline: Vec<(u64, String)>,
    pub counts: BTreeMap<String, i64>,
    pub checks: Vec<Check>,
    pub ok: bool,
    pub seconds: f64,
}

pub struct Env {
    pub net: Network,
    pub procs: Arc<Mutex<Procs>>,
    pub op: Arc<Holder>,
    pub data: PathBuf,
}

fn web_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web")
}

/// A fresh network in its own data dir and port range, started and seeded.
pub async fn up(name: &str, base_port: u16, stock: u32, tweak: impl FnOnce(&mut Procs)) -> Result<Env, String> {
    let net = shop_slice1(name, base_port);
    let data = std::env::temp_dir().join("live-shop-proof").join(name);
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    let mut procs = Procs::new(shop_net::bin_dir(), data.clone(), web_root());
    for b in ["shop-keys", "shop-inventory", "shop-orders", "shop-gateway"] {
        if !procs.bin.join(b).exists() {
            return Err(format!("{} not built: run `cargo build --workspace` (same profile) first", procs.bin.join(b).display()));
        }
    }
    tweak(&mut procs);
    if let Err(e) = apply(&net, &data, &mut procs).await {
        shop_net::down(&net, &mut procs).await;
        return Err(e);
    }
    let op = operator(&net).await;
    seed(&net, &op, stock).await?;
    Ok(Env { net, procs: Arc::new(Mutex::new(procs)), op, data })
}

impl Env {
    pub async fn down(&self) {
        let mut p = std::mem::replace(&mut *self.procs.lock().unwrap(), Procs::new(PathBuf::new(), PathBuf::new(), PathBuf::new()));
        shop_net::down(&self.net, &mut p).await;
    }
    pub fn gateway(&self) -> Client {
        Client::new(self.net.directory().one("shop.gateway"))
    }
    pub fn stop_node(&self, node: &str) {
        let mut p = self.procs.lock().unwrap();
        for i in self.net.of_node(node) {
            p.stop(&self.net, i);
        }
    }
    pub fn start_node(&self, node: &str) {
        let mut p = self.procs.lock().unwrap();
        for i in self.net.of_node(node) {
            let _ = p.start(&self.net, i);
        }
    }
    /// The node of the replica that currently leads `cell` (highest epoch among replicas that say Leader).
    pub async fn leader_node(&self, cell: &str) -> Option<String> {
        let mut best: Option<(u64, usize)> = None;
        for (i, a) in self.net.directory().addrs(cell).iter().enumerate() {
            if let Ok(h) = Client::new(a.clone()).health(cell).await {
                let mut w = h.role.split_whitespace();
                if w.next() == Some("Leader") {
                    let epoch: u64 = w.nth(1).and_then(|e| e.parse().ok()).unwrap_or(0);
                    if best.is_none_or(|b| epoch > b.0) {
                        best = Some((epoch, i));
                    }
                }
            }
        }
        let (_, i) = best?;
        self.net.instances.iter().find(|x| x.cell == cell && x.replica == i).map(|x| x.node.clone())
    }
    pub fn inv(&self) -> Replicas {
        Replicas::new(self.net.directory().addrs("shop.inventory"), Some(self.op.clone()))
    }
    pub fn orders(&self) -> Replicas {
        Replicas::new(self.net.directory().addrs("shop.orders"), Some(self.op.clone()))
    }
}

fn token_name(t: &TestToken) -> String {
    match t {
        TestToken::SlowWebhook { .. } => "SlowWebhook".into(),
        t => format!("{t:?}"),
    }
}

/// One buyer through the gateway: sign in, put one `sku` in the cart, check out, retry the same checkout id on
/// any transport error or timeout, then poll the order until it is final.
pub async fn buy(gwc: Client, n: usize, email: String, checkout: String, sku: &str, token: TestToken, deadline: Instant) -> Buyer {
    let mut b = Buyer { n, checkout: checkout.clone(), token: token_name(&token), attempts: 0, orders_seen: BTreeSet::new(), answer: None };
    let sid = loop {
        if Instant::now() > deadline {
            return b;
        }
        match gwc.call::<gw::SignIn>(&SignInReq { email: email.clone() }).await {
            Ok(Ok(s)) => break s.id,
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    };
    loop {
        if Instant::now() > deadline {
            return b;
        }
        match gwc.call::<gw::CartSet>(&CartSetReq { session: sid.clone(), sku: Sku(sku.into()), qty: 1 }).await {
            Ok(Ok(_)) => break,
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    let mut view: Option<OrderView> = None;
    while view.is_none() && Instant::now() < deadline {
        b.attempts += 1;
        match gwc.call::<gw::Checkout>(&CheckoutReq { session: sid.clone(), checkout: CheckoutId(checkout.clone()), pay: token.clone() }).await {
            Ok(Ok(v)) => {
                b.orders_seen.insert(v.id.0.clone());
                view = Some(v);
            }
            Ok(Err(GwError::Denied(_))) | Ok(Err(GwError::Unauthenticated)) => return b,
            _ => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
    let Some(mut v) = view else { return b };
    while !v.stage.is_final() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(Ok(x)) = gwc.call::<gw::Order>(&OrderReq { session: sid.clone(), order: v.id.clone() }).await {
            b.orders_seen.insert(x.id.0.clone());
            v = x;
        }
    }
    if v.stage.is_final() {
        b.answer = Some(v);
    }
    b
}

fn tent_units(v: &OrderView) -> u32 {
    v.lines.iter().filter(|l| l.sku.0 == "tent-2p").map(|l| l.qty).sum()
}

struct State {
    orders: Vec<OrderView>,
    levels: Vec<Option<shop_inventory_api::Level>>,
    resv: Vec<Vec<shop_inventory_api::Reservation>>,
    tails: Vec<Option<shop_replog::TailResp<shop_inventory_peer_api::Entry>>>,
    ledger: Vec<shop_pay_sim_api::SimRow>,
}

async fn read_back(env: &Env) -> Result<State, String> {
    let d = env.net.directory();
    let orders = env.orders().call::<orders::All>(&(), shop_orders_api::redirect, Duration::from_secs(20)).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
    let mut levels = vec![];
    let mut resv = vec![];
    let mut tails = vec![];
    for (i, a) in d.addrs("shop.inventory").iter().enumerate() {
        let c = Client::with(a.clone(), Some(env.op.clone()));
        levels.push(c.call::<inv::Levels>(&LevelsReq { skus: vec![Sku("tent-2p".into())] }).await.ok().and_then(|r| r.ok()).and_then(|v| v.into_iter().next()));
        resv.push(c.call::<inv::Reservations>(&()).await.ok().and_then(|r| r.ok()).unwrap_or_default());
        let pc = Client::with(d.addrs("shop.inventory.peer")[i].clone(), Some(env.op.clone()));
        tails.push(pc.call::<shop_inventory_peer_api::ops::Tail>(&shop_replog::TailReq { from: 1 }).await.ok().and_then(|r| r.ok()));
    }
    let ledger = Client::with(d.one("shop.pay.sim"), Some(env.op.clone())).call::<shop_pay_sim_api::ops::Ledger>(&()).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
    Ok(State { orders, levels, resv, tails, ledger })
}

/// Wait until the three inventory replicas have applied the same log and no order is left open.
async fn settle(env: &Env, for_: Duration) {
    let end = Instant::now() + for_;
    while Instant::now() < end {
        let mut commits = BTreeSet::new();
        for a in env.net.directory().addrs("shop.inventory") {
            match Client::with(a, Some(env.op.clone())).call::<inv::Role>(&()).await {
                Ok(Ok(r)) => {
                    commits.insert((r.commit, r.last));
                }
                _ => {
                    commits.insert((u64::MAX, 0));
                }
            }
        }
        let open = env.orders().call::<orders::All>(&(), shop_orders_api::redirect, Duration::from_secs(5)).await.ok().and_then(|r| r.ok()).map(|v| v.iter().filter(|o| !o.stage.is_final()).count()).unwrap_or(1);
        if commits.len() == 1 && open == 0 {
            // one more round so followers learn the final commit index
            tokio::time::sleep(Duration::from_millis(300)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn check(id: &'static str, what: &'static str, ok: bool, numbers: serde_json::Value) -> Check {
    Check { id, what, ok, numbers }
}

/// A1-A6 over the read-back state and the buyers' answers.
fn assertions(st: &State, buyers: &[Buyer], stock: u32, tenant: &TenantId) -> Vec<Check> {
    let by_id: BTreeMap<&str, &OrderView> = st.orders.iter().map(|o| (o.id.0.as_str(), o)).collect();
    let confirmed: Vec<&OrderView> = st.orders.iter().filter(|o| o.stage == Stage::Confirmed).collect();
    let units: u32 = confirmed.iter().map(|o| tent_units(o)).sum();
    let per_replica: Vec<serde_json::Value> = st
        .levels
        .iter()
        .map(|l| match l {
            Some(l) => serde_json::json!({ "on_hand": l.on_hand, "consumed": stock - l.on_hand, "committed": l.committed, "reserved": l.reserved }),
            None => serde_json::Value::Null,
        })
        .collect();
    let a1 = units <= stock && st.levels.iter().all(|l| l.as_ref().is_some_and(|l| stock - l.on_hand == units && l.committed == units && l.reserved <= l.on_hand));
    let mut c = vec![check("A1", "no oversell: confirmed units <= stock and equal to stock consumed on every inventory replica", a1, serde_json::json!({ "stock": stock, "confirmed_units": units, "replicas": per_replica }))];

    let mut per_checkout: BTreeMap<&str, u32> = BTreeMap::new();
    for o in &st.orders {
        *per_checkout.entry(o.checkout.0.as_str()).or_default() += 1;
    }
    let dup_checkout = per_checkout.values().filter(|n| **n > 1).count();
    let multi_seen = buyers.iter().filter(|b| b.orders_seen.len() > 1).count();
    let wrong_id = st.orders.iter().filter(|o| order_id(tenant, &o.checkout) != o.id).count();
    c.push(check("A2", "no double order: every checkout id maps to at most one order", dup_checkout == 0 && multi_seen == 0 && wrong_id == 0, serde_json::json!({ "orders": st.orders.len(), "checkouts_with_2+_orders": dup_checkout, "buyers_seeing_2+_order_ids": multi_seen, "ids_not_derived_from_checkout": wrong_id })));

    let unanswered = buyers.iter().filter(|b| b.answer.is_none()).count();
    let differs = buyers.iter().filter(|b| b.answer.as_ref().is_some_and(|a| by_id.get(a.id.0.as_str()).is_none_or(|o| o.stage != a.stage))).count();
    c.push(check("A3", "no lost order: every buyer got a final answer, equal to what the order says now", unanswered == 0 && differs == 0, serde_json::json!({ "buyers": buyers.len(), "without_final_answer": unanswered, "answer_differs_from_order": differs })));

    let mut cap: BTreeMap<String, (u32, i64)> = BTreeMap::new();
    let mut rev: BTreeMap<String, i64> = BTreeMap::new();
    for r in &st.ledger {
        let order = r.key.0.trim_start_matches("charge:").trim_start_matches("refund:").to_string();
        match r.kind {
            SimKind::Capture => {
                let e = cap.entry(order).or_default();
                e.0 += 1;
                e.1 += r.amount.amount;
            }
            SimKind::Reverse => *rev.entry(order).or_default() += r.amount.amount,
        }
    }
    // a reversal row carries the refund key; map it back through the provider ref
    let by_ref: BTreeMap<&str, String> = st.ledger.iter().filter(|r| r.kind == SimKind::Capture).map(|r| (r.provider_ref.as_str(), r.key.0.trim_start_matches("charge:").to_string())).collect();
    let mut rev_by_order: BTreeMap<String, i64> = BTreeMap::new();
    for r in st.ledger.iter().filter(|r| r.kind == SimKind::Reverse) {
        if let Some(o) = by_ref.get(r.provider_ref.as_str()) {
            *rev_by_order.entry(o.clone()).or_default() += r.amount.amount;
        }
    }
    let _ = rev;
    let captured: i64 = cap.values().map(|v| v.1).sum();
    let refunded: i64 = rev_by_order.values().sum();
    let confirmed_total: i64 = confirmed.iter().map(|o| o.total.amount).sum();
    let multi_capture = cap.values().filter(|v| v.0 > 1).count();
    c.push(check("A4", "money matches: captured minus refunded equals the totals of confirmed orders; at most one capture per order", captured - refunded == confirmed_total && multi_capture == 0, serde_json::json!({ "captured": captured, "refunded": refunded, "net": captured - refunded, "confirmed_total": confirmed_total, "orders_with_2+_captures": multi_capture })));

    let failed: BTreeSet<&str> = st.orders.iter().filter(|o| matches!(o.stage, Stage::Failed(_) | Stage::Cancelled)).map(|o| o.id.0.as_str()).collect();
    let orphan_resv: usize = st.resv.iter().map(|rs| rs.iter().filter(|r| !r.committed && failed.contains(r.order.0.as_str())).count()).sum();
    let open_resv: usize = st.resv.iter().map(|rs| rs.iter().filter(|r| !r.committed).count()).sum();
    let orphan_cap = cap.keys().filter(|o| by_id.get(o.as_str()).is_none_or(|v| v.stage != Stage::Confirmed) && rev_by_order.get(*o).copied().unwrap_or(0) != cap[*o].1).count();
    c.push(check("A5", "no orphan: no reservation left for a failed order, no capture kept for an order that is not confirmed", orphan_resv == 0 && orphan_cap == 0 && open_resv == 0, serde_json::json!({ "failed_orders": failed.len(), "reservations_of_failed_orders": orphan_resv, "uncommitted_reservations": open_resv, "captures_kept_without_confirmed_order": orphan_cap })));

    let tails: Vec<_> = st.tails.iter().flatten().collect();
    let commit = tails.iter().map(|t| t.commit).min().unwrap_or(0) as usize;
    let agree = tails.len() == 3 && tails.iter().all(|t| t.entries.len() >= commit && t.entries[..commit] == tails[0].entries[..commit]);
    c.push(check("A6", "logs agree: the three inventory logs are identical up to the commit index after heal", agree, serde_json::json!({ "replicas_read": tails.len(), "commit_index": commit, "lengths": tails.iter().map(|t| t.entries.len()).collect::<Vec<_>>() })));
    c
}

pub struct Opts {
    pub buyers: usize,
    pub stock: u32,
    pub unsafe_no_quorum: bool,
    pub outage_s: u64,
    /// Delay between stopping the inventory leader's node and the orders leader's node.
    pub second_stop_ms: u64,
}

/// Scenario `kill-mid-checkout`: while at least 20 sagas hold a reservation, stop every cell of the node with
/// the inventory leader; `second_stop_ms` later stop the orders leader's node if it is another one; heal after
/// the outage.
pub async fn kill_mid_checkout(seed: u64, base_port: u16, o: &Opts) -> Result<Report, String> {
    let t0 = Instant::now();
    let unsafe_ = o.unsafe_no_quorum;
    let env = Arc::new(
        up(&format!("chaos-{seed}{}", if unsafe_ { "-unsafe" } else { "" }), base_port, o.stock, |p| {
            // a provider that answers in 250-830 ms keeps sagas between reserve and commit long enough to hit
            p.extra.insert("shop.pay.sim".into(), vec!["--latency-ms".into(), "250".into()]);
            if unsafe_ {
                p.extra.insert("shop.inventory".into(), vec!["--unsafe-local-ack".into()]);
            }
        })
        .await?,
    );
    let timeline = Arc::new(Mutex::new(vec![(0u64, format!("network up and seeded: tent-2p {} on hand, 5 other skus 500 each", o.stock))]));
    let ms = move || t0.elapsed().as_millis() as u64;
    let mut rng = Rng::new(seed);
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut tasks = Vec::new();
    for n in 0..o.buyers {
        let r = rng.below(100);
        let token = match r {
            0..=79 => TestToken::Ok,
            80..=89 => TestToken::Declined,
            90..=94 => TestToken::SlowWebhook { delay_ms: 1500 },
            _ => TestToken::DuplicateWebhook,
        };
        let g = env.gateway();
        tasks.push(tokio::spawn(buy(g, n, format!("test-b{n}-s{seed}@live.shop"), format!("k-{seed}-{n:04}"), "tent-2p", token, deadline)));
    }
    // the fault
    let jitter = rng.below(60);
    let fenv = env.clone();
    let tl = timeline.clone();
    let outage = o.outage_s;
    let second_ms = o.second_stop_ms;
    let fault = tokio::spawn(async move {
        // mid-checkout: at least 20 units held by sagas between reserve and commit, read on every replica
        // (a follower may lag)
        let wait_end = Instant::now() + Duration::from_secs(60);
        let mut held = 0;
        let addrs = fenv.net.directory().addrs("shop.inventory");
        while Instant::now() < wait_end {
            let mut best = (0u32, 0u32);
            for a in &addrs {
                if let Ok(Ok(l)) = Client::with(a.clone(), Some(fenv.op.clone())).call::<inv::Levels>(&LevelsReq { skus: vec![Sku("tent-2p".into())] }).await {
                    if let Some(l) = l.first() {
                        best = best.max((l.reserved, l.committed));
                    }
                }
            }
            held = best.0;
            if best.0 >= 20 {
                // a seeded moment inside the window, then make sure it is still open
                tokio::time::sleep(Duration::from_millis(jitter)).await;
                if fenv.inv().call::<inv::Levels>(&LevelsReq { skus: vec![Sku("tent-2p".into())] }, shop_inventory_api::redirect, Duration::from_secs(2)).await.is_ok_and(|r| r.is_ok_and(|l| l[0].reserved >= 20)) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let Some(n1) = fenv.leader_node("shop.inventory").await else {
            tl.lock().unwrap().push((ms(), "no inventory leader found; no fault".into()));
            return vec![];
        };
        fenv.stop_node(&n1);
        tl.lock().unwrap().push((ms(), format!("StopNode {n1} (inventory leader) with {held} units reserved by sagas between reserve and commit")));
        let mut stopped = vec![n1.clone()];
        tokio::time::sleep(Duration::from_millis(second_ms)).await;
        match fenv.leader_node("shop.orders").await {
            Some(n2) if n2 != n1 => {
                fenv.stop_node(&n2);
                tl.lock().unwrap().push((ms(), format!("StopNode {n2} (orders leader)")));
                stopped.push(n2);
            }
            Some(n2) => tl.lock().unwrap().push((ms(), format!("orders leader already re-elected on {n2}; it is a running node: second stop skipped (would leave no majority)"))),
            None => tl.lock().unwrap().push((ms(), "orders leader was on the stopped node; second stop skipped".into())),
        }
        tokio::time::sleep(Duration::from_millis((outage * 1000).saturating_sub(second_ms))).await;
        for n in &stopped {
            fenv.start_node(n);
        }
        tl.lock().unwrap().push((ms(), format!("Heal: restarted {}", stopped.join(", "))));
        stopped
    });
    let mut buyers = Vec::new();
    for t in tasks {
        buyers.push(t.await.map_err(|e| e.to_string())?);
    }
    timeline.lock().unwrap().push((ms(), "every buyer has a final answer (or gave up at the deadline)".into()));
    let _ = fault.await;
    settle(&env, Duration::from_secs(30)).await;
    let st = read_back(&env).await;
    env.down().await;
    let st = st?;
    let checks = assertions(&st, &buyers, o.stock, &TenantId(TEST_TENANT.into()));
    let mut counts = BTreeMap::new();
    let mut count = |k: &str, v: i64| {
        counts.insert(k.to_string(), v);
    };
    count("orders", st.orders.len() as i64);
    count("confirmed", st.orders.iter().filter(|o| o.stage == Stage::Confirmed).count() as i64);
    count("failed_out_of_stock", st.orders.iter().filter(|o| matches!(&o.stage, Stage::Failed(shop_orders_api::FailReason::OutOfStock(_)))).count() as i64);
    count("failed_declined", st.orders.iter().filter(|o| matches!(&o.stage, Stage::Failed(shop_orders_api::FailReason::Declined(_)))).count() as i64);
    count("failed_other", st.orders.iter().filter(|o| matches!(&o.stage, Stage::Failed(shop_orders_api::FailReason::Timeout | shop_orders_api::FailReason::Internal(_)))).count() as i64);
    count("checkout_attempts", buyers.iter().map(|b| b.attempts as i64).sum());
    count("buyers_retried", buyers.iter().filter(|b| b.attempts > 1).count() as i64);
    count("ledger_rows", st.ledger.len() as i64);
    let ok = checks.iter().all(|c| c.ok);
    let tl = timeline.lock().unwrap().clone();
    Ok(Report { scenario: "kill-mid-checkout", seed, unsafe_no_quorum: unsafe_, buyers: buyers.len(), stock: o.stock, timeline: tl, counts, checks, ok, seconds: t0.elapsed().as_secs_f64() })
}

#[derive(Debug, Serialize)]
pub struct StepRow {
    pub step: u8,
    pub when: String,
    pub token: String,
    pub crashes: u32,
    pub stage: String,
    pub legal: bool,
    pub detail: String,
}

/// Fault injection at every saga step boundary: every orders replica stops itself once right after writing
/// that step record; a supervisor restarts it. The order must end in one of the two legal final states.
pub async fn step_faults(base_port: u16) -> Result<Vec<StepRow>, String> {
    let mut rows = Vec::new();
    let cases: Vec<(u8, &str, TestToken)> = vec![
        (1, "started", TestToken::Ok),
        (1, "done", TestToken::Ok),
        (2, "started", TestToken::Ok),
        (2, "done", TestToken::Ok),
        (3, "started", TestToken::Ok),
        (3, "done", TestToken::Ok),
        (4, "started", TestToken::Ok),
        (4, "done", TestToken::Ok),
        (5, "done", TestToken::Ok),
        (3, "started", TestToken::Declined),
        (12, "started", TestToken::Declined),
        (12, "done", TestToken::Declined),
        (3, "done", TestToken::SlowWebhook { delay_ms: 1200 }),
    ];
    for (k, (step, when, token)) in cases.into_iter().enumerate() {
        let name = format!("steps-{step}-{when}-{}", token_name(&token));
        let env = up(&name, base_port, 5, |p| p.env.push(("SHOP_ORDERS_CRASH".into(), format!("{step}:{when}")))).await?;
        let _ = k;
        // supervisor: restart an orders replica that stopped itself, like systemd's Restart=always
        let sup_env = env.procs.clone();
        let net = env.net.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let crashes = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (s2, c2) = (stop.clone(), crashes.clone());
        let sup = tokio::spawn(async move {
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
                let mut p = sup_env.lock().unwrap();
                for i in net.instances.iter().filter(|i| i.cell == "shop.orders") {
                    if p.exited(i).is_some() {
                        c2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = p.start(&net, i);
                    }
                }
            }
        });
        let b = buy(env.gateway(), 0, "test-steps@live.shop".into(), format!("k-steps-{step}-{when}"), "tent-2p", token.clone(), Instant::now() + Duration::from_secs(60)).await;
        settle(&env, Duration::from_secs(15)).await;
        let st = read_back(&env).await;
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = sup.await;
        env.down().await;
        let st = st?;
        let a = assertions(&st, std::slice::from_ref(&b), 5, &TenantId(TEST_TENANT.into()));
        let stage = b.answer.as_ref().map(|v| format!("{:?}", v.stage)).unwrap_or("no answer".into());
        let legal = a.iter().all(|c| c.ok) && b.answer.is_some();
        let bad: Vec<_> = a.iter().filter(|c| !c.ok).map(|c| format!("{} {}", c.id, c.numbers)).collect();
        rows.push(StepRow { step, when: when.into(), token: token_name(&token), crashes: crashes.load(std::sync::atomic::Ordering::Relaxed), stage, legal, detail: bad.join("; ") });
        eprintln!("{:?}", rows.last().unwrap());
    }
    Ok(rows)
}

/// The typed end-to-end path from a Rust client through the gateway (design item 7).
pub async fn smoke(base_port: u16) -> Result<serde_json::Value, String> {
    let env = up("smoke", base_port, 50, |_| {}).await?;
    let g = env.gateway();
    let r = async {
        let info = g.call::<gw::Hello>(&shop_gateway_api::HelloReq { host: "localhost".into() }).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
        let page = g.call::<gw::Products>(&ListReq { tag: None, after: None, limit: 20 }).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
        let pv = g.call::<gw::Product>(&Sku("tent-2p".into())).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
        let ok = buy(g.clone(), 0, "test-smoke@live.shop".into(), "k-smoke-ok-01".into(), "tent-2p", TestToken::Ok, Instant::now() + Duration::from_secs(30)).await;
        let declined = buy(g.clone(), 1, "test-smoke2@live.shop".into(), "k-smoke-declined".into(), "stove-1", TestToken::Declined, Instant::now() + Duration::from_secs(30)).await;
        let after = g.call::<gw::Product>(&Sku("tent-2p".into())).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
        let unknown = g.call::<gw::Cart>(&SessionId("nope".into())).await.map_err(|e| e.to_string())?;
        Ok::<_, String>(serde_json::json!({
            "hello": info,
            "products": page.items.len(),
            "tent_in_stock_before": pv.in_stock,
            "tent_in_stock_after": after.in_stock,
            "ok_order": ok.answer.as_ref().map(|v| format!("{:?}", v.stage)),
            "declined_order": declined.answer.as_ref().map(|v| format!("{:?}", v.stage)),
            "unknown_session": format!("{unknown:?}"),
        }))
    }
    .await;
    env.down().await;
    r
}

/// The payment rules of design item 5, called directly on `shop.pay` with chains a gateway would make.
pub async fn pay_rules(base_port: u16) -> Result<serde_json::Value, String> {
    use shop_pay_api::{ops as pay, ChargeReq, PayError, PayState, WebhookReq};
    use shop_wire::chain::Caveats;
    let env = up("pay-rules", base_port, 50, |_| {}).await?;
    let d = env.net.directory();
    let r = async {
        // act as the gateway would for one customer's checkout: its grant, narrowed to an order and an amount
        let (gwh, _, tenant) = shop_cell::enroll(&d.one("shop.keys"), "shop.gateway", "pay-rules-test").await;
        let charge_chain = |order: &str, amount: i64| {
            let c = Caveats { tenant: tenant.clone(), customer: Some("c-test".into()), order: Some(order.into()), amount: Some((amount, "SEK".into())), once: true, until: shop_wire::chain::now() + 300, ..Default::default() };
            Holder { id: gwh.id.clone(), chain: gwh.chain.narrow(&gwh.id, &gwh.id.public_hex(), vec!["shop.pay:charge".into()], c).unwrap() }
        };
        let p = Client::new(d.one("shop.pay"));
        let req = |order: &str, amount: i64, token: TestToken| ChargeReq { order: OrderId(order.into()), amount: Money::sek(amount), token, key: IdemKey(format!("charge:{order}")) };
        let h1 = charge_chain("o-rule-1", 100_00);
        let first = p.call_as::<pay::Charge>(&h1, &req("o-rule-1", 100_00, TestToken::Ok)).await.map_err(|e| e.to_string())?;
        let again = p.call_as::<pay::Charge>(&h1, &req("o-rule-1", 100_00, TestToken::Ok)).await.map_err(|e| e.to_string())?;
        let other_req_same_key = p.call_as::<pay::Charge>(&h1, &req("o-rule-1", 100_00, TestToken::Declined)).await.map_err(|e| e.to_string())?;
        let wrong_amount = p.call_as::<pay::Charge>(&h1, &req("o-rule-1", 999_00, TestToken::Ok)).await.map_err(|e| e.to_string())?;
        let wrong_order = p.call_as::<pay::Charge>(&h1, &req("o-rule-x", 100_00, TestToken::Ok)).await.map_err(|e| e.to_string())?;
        let h2 = charge_chain("o-rule-2", 50_00);
        let dup = p.call_as::<pay::Charge>(&h2, &req("o-rule-2", 50_00, TestToken::DuplicateWebhook)).await.map_err(|e| e.to_string())?;
        let forged_req = WebhookReq { event: "ev-x".into(), body: "{}".into(), sig: "00".repeat(32) };
        // without the provider's right the gate refuses; with it, the HMAC check refuses a body it did not sign
        let no_right = p.call::<pay::Webhook>(&forged_req).await;
        let (simh, _, _) = shop_cell::enroll(&d.one("shop.keys"), "shop.pay.sim", "pay-rules-test").await;
        let forged = p.call_as::<pay::Webhook>(&simh, &forged_req).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let ledger = Client::with(d.one("shop.pay.sim"), Some(env.op.clone())).call::<shop_pay_sim_api::ops::Ledger>(&()).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:?}"))?;
        let caps = |o: &str| ledger.iter().filter(|r| r.kind == SimKind::Capture && r.key.0 == format!("charge:{o}")).count();
        let checks = serde_json::json!({
            "same_key_twice_one_capture": first.as_ref().is_ok_and(|p| p.state == PayState::Captured) && again.as_ref().ok() == first.as_ref().ok() && caps("o-rule-1") == 1,
            "same_key_other_request_refused": matches!(other_req_same_key, Err(PayError::Denied(_))),
            "amount_unequal_to_chain_refused": matches!(wrong_amount, Err(PayError::AmountMismatch(_))),
            "order_unequal_to_chain_refused": matches!(wrong_order, Err(PayError::Denied(_))),
            "duplicate_webhook_one_capture": dup.as_ref().is_ok_and(|p| p.state == PayState::Captured) && caps("o-rule-2") == 1,
            "webhook_without_right_refused": matches!(no_right, Err(shop_wire::WireError::Denied(_))),
            "forged_webhook_refused": matches!(forged, Ok(Err(PayError::Denied(_)))),
            "mode_is_test": first.as_ref().is_ok_and(|p| p.mode == shop_pay_api::Mode::Test),
        });
        Ok::<_, String>(checks)
    }
    .await;
    env.down().await;
    r
}
