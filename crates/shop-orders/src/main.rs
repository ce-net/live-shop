//! `shop.orders`: the checkout saga. Each step is written to the replicated log before the call it describes;
//! every call is idempotent at the callee, so a new leader that runs a step again changes nothing.
//!
//! | # | step    | call                                   | compensation when a later step fails |
//! |---|---------|----------------------------------------|--------------------------------------|
//! | 0 | open    | `OrderId = f(tenant, checkout)`        | none                                 |
//! | 1 | freeze  | `shop.cart::Freeze` (priced)           | unfreeze                             |
//! | 2 | reserve | `shop.inventory::Reserve`              | `Release`                            |
//! | 3 | charge  | `shop.pay::Charge`, chain narrowed     | `Refund`                             |
//! | 4 | commit  | `shop.inventory::Commit`               | `Refund`, then `Release`             |
//! | 5 | confirm | stage `Confirmed`, `shop.cart::Clear`  | none: retried until done             |
use shop_cart_api::{ops as cart, CartError, ClearReq, FreezeReq};
use shop_inventory_api::{ops as inv, InvError, InvOrderRef, ReserveReq};
use shop_orders_api::*;
use shop_orders_peer_api::{ops as peer, Entry, StepState};
use shop_pay_api::{ops as pay, ChargeReq, PayError, PayState, RefundReq, StatusReq};
use shop_replog::{Config, Log, Machine, ProposeError, Role, WirePeers};
use shop_types::*;
use shop_wire::chain::{Caveats, Chain, Holder};
use shop_wire::{Client, Replicas, Server, WireError};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct Rec {
    tenant: TenantId,
    view: OrderView,
    token: TestToken,
    chain: String,
    steps: BTreeMap<u8, StepState>,
    fail: Option<FailReason>,
}

#[derive(Default)]
struct Book {
    recs: BTreeMap<String, Rec>,
}

const S_FREEZE: u8 = 1;
const S_RESERVE: u8 = 2;
const S_CHARGE: u8 = 3;
const S_COMMIT: u8 = 4;
const S_CONFIRM: u8 = 5;
const S_RELEASE: u8 = 12;
const S_REFUND: u8 = 13;
const S_UNFREEZE: u8 = 14;
const S_REASON: u8 = 99;

impl Machine for Book {
    type Entry = Entry;
    type Out = Option<OrderView>;
    fn apply(&mut self, _idx: u64, e: &Entry) -> Option<OrderView> {
        match e {
            Entry::Open { tenant, view, token, chain } => {
                let r = self.recs.entry(view.id.0.clone()).or_insert_with(|| Rec { tenant: tenant.clone(), view: view.clone(), token: token.clone(), chain: chain.clone(), steps: BTreeMap::new(), fail: None });
                Some(r.view.clone())
            }
            Entry::Step { order, step, state, note } => {
                let r = self.recs.get_mut(&order.0)?;
                r.steps.insert(*step, *state);
                if *step == S_REASON {
                    r.fail = serde_json::from_str(note).ok();
                }
                Some(r.view.clone())
            }
            Entry::Stage { order, stage, lines, total, payment } => {
                let r = self.recs.get_mut(&order.0)?;
                // one-way: a stale write from a deposed leader cannot move an order backwards
                if stage.rank() > r.view.stage.rank() || (stage.rank() == r.view.stage.rank() && !r.view.stage.is_final()) {
                    r.view.stage = stage.clone();
                }
                if !lines.is_empty() {
                    r.view.lines = lines.clone();
                    r.view.total = total.clone();
                }
                if payment.is_some() {
                    r.view.payment = payment.clone();
                }
                Some(r.view.clone())
            }
        }
    }
}

struct Ctx {
    log: Log<Book>,
    me: Arc<Holder>,
    root: String,
    addrs: Vec<String>,
    inv: Replicas,
    cart: Client,
    pay: Client,
    running: Mutex<HashSet<String>>,
    crash: Option<(u8, String)>,
    crash_marker: std::path::PathBuf,
}

enum Halt {
    NotLeader,
}

fn backoff(n: u32) -> Duration {
    Duration::from_millis((50u64 << n.min(5)).min(1000))
}

impl Ctx {
    fn rec(&self, id: &str) -> Option<Rec> {
        self.log.read(|m, _| m.recs.get(id).cloned())
    }

    async fn put(&self, e: Entry) -> Result<(), Halt> {
        loop {
            match self.log.propose(e.clone()).await {
                Ok(_) => return Ok(()),
                Err(ProposeError::NotLeader(_)) => return Err(Halt::NotLeader),
                Err(ProposeError::NoQuorum) => {
                    if self.log.status().role != Role::Leader {
                        return Err(Halt::NotLeader);
                    }
                }
            }
        }
    }

    async fn step(&self, order: &OrderId, step: u8, state: StepState) -> Result<(), Halt> {
        self.put(Entry::Step { order: order.clone(), step, state, note: String::new() }).await?;
        self.maybe_crash(step, state);
        Ok(())
    }

    /// Fault injection for the step-boundary proof: `SHOP_ORDERS_CRASH=<step>:<started|done>` stops the process
    /// once, right after that step record is written.
    fn maybe_crash(&self, step: u8, state: StepState) {
        if let Some((s, w)) = &self.crash {
            let when = match state {
                StepState::Started => "started",
                _ => "done",
            };
            if *s == step && w == when && !self.crash_marker.exists() {
                let _ = std::fs::write(&self.crash_marker, b"crashed");
                eprintln!("fault injection: stopping after step {step} {when}");
                std::process::exit(86);
            }
        }
    }

    async fn stage(&self, r: &Rec, stage: Stage) -> Result<(), Halt> {
        self.put(Entry::Stage { order: r.view.id.clone(), stage, lines: vec![], total: r.view.total.clone(), payment: None }).await
    }

    async fn fail(&self, r: &Rec, why: FailReason, next: Stage) -> Result<(), Halt> {
        self.put(Entry::Step { order: r.view.id.clone(), step: S_REASON, state: StepState::Failed, note: serde_json::to_string(&why).unwrap() }).await?;
        self.stage(r, next).await
    }

    fn checkout_holder(&self, r: &Rec) -> Holder {
        Holder { id: self.me.id.clone(), chain: serde_json::from_str(&r.chain).expect("stored chain") }
    }

    /// The charge link: the customer's checkout chain narrowed to this order, this amount, once. Deterministic,
    /// so a resumed saga presents the same chain id to `shop.pay`.
    fn charge_holder(&self, r: &Rec) -> Holder {
        let h = self.checkout_holder(r);
        let leaf = h.chain.leaf().caveats.clone();
        let c = Caveats { order: Some(r.view.id.0.clone()), amount: Some(r.view.total.caveat()), once: true, ..leaf };
        let chain = h.chain.narrow(&self.me.id, &self.me.id.public_hex(), shop_policy::charge_rights(), c).expect("narrowing the checkout chain");
        Holder { id: self.me.id.clone(), chain }
    }

    /// Run the saga of `id` until it is final and its follow-ups are done, or this replica stops leading.
    async fn drive(self: Arc<Self>, id: String) {
        let _ = self.drive_inner(&id).await;
        self.running.lock().unwrap().remove(&id);
    }

    async fn drive_inner(&self, id: &str) -> Result<(), Halt> {
        let mut tries = 0u32;
        loop {
            if self.log.status().role != Role::Leader {
                return Err(Halt::NotLeader);
            }
            let Some(r) = self.rec(id) else { return Ok(()) };
            let oid = r.view.id.clone();
            match &r.view.stage {
                Stage::Pending => {
                    self.step(&oid, S_FREEZE, StepState::Started).await?;
                    let h = self.checkout_holder(&r);
                    let f = match self.cart.call_as::<cart::Freeze>(&h, &FreezeReq { checkout: r.view.checkout.clone() }).await {
                        Ok(Ok(f)) => f,
                        Ok(Err(CartError::Upstream(_))) | Err(WireError::Io(_) | WireError::Timeout) => {
                            tries += 1;
                            tokio::time::sleep(backoff(tries)).await;
                            continue;
                        }
                        Ok(Err(e)) => {
                            self.step(&oid, S_UNFREEZE, StepState::Done).await?;
                            return self.fail(&r, FailReason::Internal(format!("cart: {e:?}")), Stage::Failed(FailReason::Internal(format!("cart: {e:?}")))).await;
                        }
                        Err(e) => return self.fail(&r, FailReason::Internal(e.to_string()), Stage::Failed(FailReason::Internal(e.to_string()))).await,
                    };
                    if f.total != r.view.total {
                        let why = FailReason::Internal(format!("cart total {} differs from the approved {}", f.total, r.view.total));
                        return self.fail(&r, why.clone(), Stage::Releasing).await;
                    }
                    self.put(Entry::Stage { order: oid.clone(), stage: Stage::Pending, lines: f.lines.clone(), total: f.total.clone(), payment: None }).await?;
                    self.step(&oid, S_FREEZE, StepState::Done).await?;
                    self.step(&oid, S_RESERVE, StepState::Started).await?;
                    let lines: Vec<Line> = f.lines.iter().map(|l| Line { sku: l.sku.clone(), qty: l.qty }).collect();
                    let res = self.inv.call::<inv::Reserve>(&ReserveReq { order: oid.clone(), lines, ttl_s: 900 }, shop_inventory_api::redirect, Duration::from_secs(8)).await;
                    match res {
                        Ok(Ok(_)) => {
                            self.step(&oid, S_RESERVE, StepState::Done).await?;
                            self.stage(&r, Stage::Reserved).await?;
                        }
                        Ok(Err(InvError::OutOfStock { sku, .. })) => {
                            self.step(&oid, S_RESERVE, StepState::Failed).await?;
                            self.fail(&r, FailReason::OutOfStock(sku), Stage::Releasing).await?;
                        }
                        Ok(Err(InvError::Expired(_))) => {
                            self.step(&oid, S_RESERVE, StepState::Failed).await?;
                            self.fail(&r, FailReason::Timeout, Stage::Releasing).await?;
                        }
                        _ => {
                            // no definitive answer: ask again (idempotent on the order), never guess
                            tries += 1;
                            tokio::time::sleep(backoff(tries)).await;
                        }
                    }
                }
                Stage::Reserved => {
                    self.step(&oid, S_CHARGE, StepState::Started).await?;
                    let h = self.charge_holder(&r);
                    let req = ChargeReq { order: oid.clone(), amount: r.view.total.clone(), token: r.token.clone(), key: IdemKey(format!("charge:{}", oid.0)) };
                    let p = match self.pay.call_as::<pay::Charge>(&h, &req).await {
                        Ok(Ok(p)) => Some(p),
                        Ok(Err(PayError::AlreadyUsed)) | Ok(Err(PayError::Provider(_))) | Err(WireError::Io(_) | WireError::Timeout) => None,
                        Ok(Err(e)) => {
                            self.step(&oid, S_CHARGE, StepState::Failed).await?;
                            self.fail(&r, FailReason::Internal(format!("pay: {e:?}")), Stage::Releasing).await?;
                            continue;
                        }
                        Err(e) => {
                            self.fail(&r, FailReason::Internal(e.to_string()), Stage::Releasing).await?;
                            continue;
                        }
                    };
                    // pending (slow webhook) or unknown: ask Status until the payment is decided
                    let mut p = p;
                    let mut n = 0;
                    while p.as_ref().is_none_or(|p| p.state == PayState::Pending) {
                        n += 1;
                        tokio::time::sleep(backoff(n).min(Duration::from_millis(400))).await;
                        if self.log.status().role != Role::Leader {
                            return Err(Halt::NotLeader);
                        }
                        match self.pay.call::<pay::Status>(&StatusReq { order: oid.clone() }).await {
                            Ok(Ok(Some(x))) => p = Some(x),
                            // never charged at all (the call was lost before shop.pay): charge again, same key
                            Ok(Ok(None)) => break,
                            _ => {}
                        }
                    }
                    let Some(p) = p else { continue };
                    if p.state == PayState::Pending {
                        continue;
                    }
                    match &p.state {
                        PayState::Captured => {
                            self.step(&oid, S_CHARGE, StepState::Done).await?;
                            self.put(Entry::Stage { order: oid.clone(), stage: Stage::Charged, lines: vec![], total: r.view.total.clone(), payment: Some(p.id.clone()) }).await?;
                        }
                        PayState::Declined(why) | PayState::Failed(why) => {
                            self.step(&oid, S_CHARGE, StepState::Failed).await?;
                            self.fail(&r, FailReason::Declined(why.clone()), Stage::Releasing).await?;
                        }
                        PayState::Refunded => {
                            self.fail(&r, FailReason::Internal("payment refunded before commit".into()), Stage::Releasing).await?;
                        }
                        PayState::Pending => unreachable!(),
                    }
                }
                Stage::Charged => {
                    self.step(&oid, S_COMMIT, StepState::Started).await?;
                    match self.inv.call::<inv::Commit>(&InvOrderRef { order: oid.clone() }, shop_inventory_api::redirect, Duration::from_secs(8)).await {
                        Ok(Ok(_)) => {
                            self.step(&oid, S_COMMIT, StepState::Done).await?;
                            self.stage(&r, Stage::Confirmed).await?;
                        }
                        Ok(Err(InvError::Unknown(_) | InvError::Expired(_))) => {
                            self.step(&oid, S_COMMIT, StepState::Failed).await?;
                            self.fail(&r, FailReason::Timeout, Stage::Refunding).await?;
                        }
                        _ => {
                            tries += 1;
                            tokio::time::sleep(backoff(tries)).await;
                        }
                    }
                }
                Stage::Refunding => {
                    if r.steps.get(&S_REFUND) != Some(&StepState::Done) {
                        self.step(&oid, S_REFUND, StepState::Started).await?;
                        let req = RefundReq { order: oid.clone(), amount: r.view.total.clone(), key: IdemKey(format!("refund:{}", oid.0)) };
                        match self.pay.call::<pay::Refund>(&req).await {
                            Ok(Ok(p)) if p.state == PayState::Refunded || !matches!(p.state, PayState::Captured | PayState::Pending) => {
                                self.step(&oid, S_REFUND, StepState::Done).await?;
                            }
                            Ok(Err(PayError::NotFound)) => self.step(&oid, S_REFUND, StepState::Done).await?,
                            _ => {
                                tries += 1;
                                tokio::time::sleep(backoff(tries)).await;
                                continue;
                            }
                        }
                    }
                    self.stage(&r, Stage::Releasing).await?;
                }
                Stage::Releasing => {
                    // never release stock for a charge that went through: make sure the payment is not captured
                    if r.steps.get(&S_CHARGE).is_some() && r.steps.get(&S_REFUND) != Some(&StepState::Done) {
                        match self.pay.call::<pay::Status>(&StatusReq { order: oid.clone() }).await {
                            Ok(Ok(Some(p))) if p.state == PayState::Captured => {
                                self.put(Entry::Stage { order: oid.clone(), stage: Stage::Refunding, lines: vec![], total: r.view.total.clone(), payment: Some(p.id) }).await?;
                                // Refunding ranks with Releasing; force it through the machine's same-rank rule
                                continue;
                            }
                            Ok(Ok(Some(p))) if p.state == PayState::Pending => {
                                tokio::time::sleep(Duration::from_millis(300)).await;
                                continue;
                            }
                            Ok(Ok(_)) => {}
                            _ => {
                                tries += 1;
                                tokio::time::sleep(backoff(tries)).await;
                                continue;
                            }
                        }
                    }
                    if r.steps.contains_key(&S_RESERVE) && r.steps.get(&S_RELEASE) != Some(&StepState::Done) {
                        self.step(&oid, S_RELEASE, StepState::Started).await?;
                        match self.inv.call::<inv::Release>(&InvOrderRef { order: oid.clone() }, shop_inventory_api::redirect, Duration::from_secs(8)).await {
                            Ok(Ok(_)) => self.step(&oid, S_RELEASE, StepState::Done).await?,
                            _ => {
                                tries += 1;
                                tokio::time::sleep(backoff(tries)).await;
                                continue;
                            }
                        }
                    }
                    let why = r.fail.clone().unwrap_or(FailReason::Internal("compensated".into()));
                    self.stage(&r, Stage::Failed(why)).await?;
                }
                Stage::Confirmed => {
                    if r.steps.get(&S_CONFIRM) == Some(&StepState::Done) {
                        return Ok(());
                    }
                    let h = self.checkout_holder(&r);
                    match self.cart.call_as::<cart::Clear>(&h, &ClearReq { checkout: r.view.checkout.clone(), empty: true }).await {
                        Ok(_) => self.step(&oid, S_CONFIRM, StepState::Done).await?,
                        Err(_) => {
                            tries += 1;
                            tokio::time::sleep(backoff(tries)).await;
                        }
                    }
                }
                Stage::Failed(_) | Stage::Cancelled => {
                    if r.steps.get(&S_UNFREEZE) == Some(&StepState::Done) || !r.steps.contains_key(&S_FREEZE) {
                        return Ok(());
                    }
                    let h = self.checkout_holder(&r);
                    match self.cart.call_as::<cart::Clear>(&h, &ClearReq { checkout: r.view.checkout.clone(), empty: false }).await {
                        Ok(_) => self.step(&oid, S_UNFREEZE, StepState::Done).await?,
                        Err(_) => {
                            tries += 1;
                            tokio::time::sleep(backoff(tries)).await;
                        }
                    }
                }
            }
        }
    }

    fn spawn(self: &Arc<Self>, id: &str) -> bool {
        if !self.running.lock().unwrap().insert(id.to_string()) {
            return false;
        }
        tokio::spawn(self.clone().drive(id.to_string()));
        true
    }

    /// Continue every order that is not final, or whose follow-ups (cart clear, unfreeze) are not done.
    fn resume(self: &Arc<Self>) -> ResumeReport {
        let open: Vec<(String, bool)> = self.log.read(|m, _| {
            m.recs
                .iter()
                .filter(|(_, r)| !r.view.stage.is_final() || (r.view.stage == Stage::Confirmed && r.steps.get(&S_CONFIRM) != Some(&StepState::Done)) || (matches!(r.view.stage, Stage::Failed(_)) && r.steps.contains_key(&S_FREEZE) && r.steps.get(&S_UNFREEZE) != Some(&StepState::Done)))
                .map(|(k, r)| (k.clone(), matches!(r.view.stage, Stage::Releasing | Stage::Refunding)))
                .collect()
        });
        let mut rep = ResumeReport { resumed: 0, compensated: 0 };
        for (id, comp) in open {
            if self.spawn(&id) {
                rep.resumed += 1;
                if comp {
                    rep.compensated += 1;
                }
            }
        }
        rep
    }

    /// Reads are answered by a ready leader only: a follower that just restarted has applied nothing yet.
    fn read_ok(&self) -> Result<(), OrderError> {
        if self.log.ready() {
            Ok(())
        } else if self.log.status().role == Role::Leader {
            Err(OrderError::Busy)
        } else {
            Err(self.not_leader())
        }
    }

    fn not_leader(&self) -> OrderError {
        OrderError::NotLeader { leader: self.log.status().leader.map(|i| self.addrs[i].clone()) }
    }

    async fn wait_final(&self, id: &str, for_ms: u64) -> Option<OrderView> {
        let end = now_ms() + for_ms;
        loop {
            let v = self.rec(id).map(|r| r.view);
            if v.as_ref().is_none_or(|v| v.stage.is_final()) || now_ms() > end || self.log.status().role != Role::Leader {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.orders").await;
    let addrs = cell.dir.addrs("shop.orders");
    let peer_addrs = cell.dir.addrs("shop.orders.peer");
    let peers = Arc::new(WirePeers::<peer::Vote, peer::Append>::new(peer_addrs.clone(), cell.me.clone()));
    let log: Log<Book> = Log::open(Config { me: cell.replica, peers: peer_addrs.clone(), dir: cell.data.join("log"), unsafe_local_ack: false }, peers);
    let ps = log.serve::<peer::Vote, peer::Append, peer::Tail>(Server::new::<shop_orders_peer_api::ShopOrdersPeer>(cell.gate.clone(), &cell.node, &cell.instance()));
    tokio::spawn(ps.serve(shop_cell::listen(&peer_addrs[cell.replica]).await));
    let crash = std::env::var("SHOP_ORDERS_CRASH").ok().and_then(|v| {
        let (s, w) = v.split_once(':')?;
        Some((s.parse().ok()?, w.to_string()))
    });
    let ctx = Arc::new(Ctx {
        log: log.clone(),
        me: cell.me.clone(),
        root: cell.gate.root.clone(),
        addrs: addrs.clone(),
        inv: cell.replicas("shop.inventory"),
        cart: cell.client("shop.cart"),
        pay: cell.client("shop.pay"),
        running: Mutex::new(HashSet::new()),
        crash,
        crash_marker: cell.data.join("crash.marker"),
    });
    // a new leader resumes every open saga once its epoch's no-op has committed
    {
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut done_term = 0;
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let s = ctx.log.status();
                if s.role == Role::Leader && s.term != done_term && s.commit >= s.last.min(s.commit.max(1)) {
                    let applied = ctx.log.read(|_, a| a);
                    if applied >= s.last {
                        let r = ctx.resume();
                        done_term = s.term;
                        eprintln!("leader for epoch {}: resumed {} orders ({} compensating)", s.term, r.resumed, r.compensated);
                    }
                }
                if s.role != Role::Leader {
                    done_term = 0;
                }
            }
        });
    }
    let (x1, x2, x3, x4, x5, x6, x7) = (ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone());
    let node = cell.node.clone();
    let rl = log.clone();
    let server = Server::new::<ShopOrders>(cell.gate.clone(), &cell.node, &cell.instance())
        .role(move || {
            let s = rl.status();
            format!("{:?} epoch {} commit {}", s.role, s.term, s.commit)
        })
        .on::<ops::Place, _, _>(move |c, r| {
            let ctx = x1.clone();
            async move {
                ctx.read_ok()?;
                let customer = c.customer().ok_or(OrderError::Denied("session chain names no customer".into()))?.to_string();
                let tenant = TenantId(c.tenant().unwrap_or_default().to_string());
                let chain: Chain = serde_json::from_str(&r.chain).map_err(|e| OrderError::Invalid(e.to_string()))?;
                let v = chain.verify(&ctx.root, shop_wire::chain::now()).map_err(|e| OrderError::Denied(e.0))?;
                let cv = &v.caveats;
                if v.holder != ctx.me.id.public_hex() {
                    return Err(OrderError::Denied("checkout chain is not handed to shop.orders".into()));
                }
                if cv.checkout.as_deref() != Some(r.checkout.0.as_str()) || cv.customer.as_deref() != Some(customer.as_str()) || cv.tenant != tenant.0 || !cv.once {
                    return Err(OrderError::Denied("checkout chain is for another checkout, customer or tenant".into()));
                }
                for need in shop_policy::checkout_rights() {
                    if !v.allows(&need) {
                        return Err(OrderError::Denied(format!("checkout chain lacks {need}")));
                    }
                }
                let Some((amount, cur)) = cv.amount.clone() else { return Err(OrderError::Denied("checkout chain names no amount".into())) };
                let currency = match cur.as_str() {
                    "SEK" => Currency::Sek,
                    "EUR" => Currency::Eur,
                    _ => return Err(OrderError::Invalid(cur)),
                };
                let id = order_id(&tenant, &r.checkout);
                if ctx.rec(&id.0).is_none() {
                    let view = OrderView {
                        id: id.clone(),
                        checkout: r.checkout.clone(),
                        customer: CustomerId(customer),
                        stage: Stage::Pending,
                        lines: vec![],
                        total: Money { amount, currency },
                        payment: None,
                        at: now_ms(),
                        test_payment: true,
                    };
                    ctx.put(Entry::Open { tenant, view, token: r.pay, chain: r.chain }).await.map_err(|_| ctx.not_leader())?;
                }
                let rec = ctx.rec(&id.0).ok_or(OrderError::Busy)?;
                if rec.view.customer.0 != c.customer().unwrap_or_default() {
                    return Err(OrderError::Denied("another customer's order".into()));
                }
                ctx.spawn(&id.0);
                ctx.wait_final(&id.0, 18_000).await.ok_or(OrderError::Busy)
            }
        })
        .on::<ops::Get, _, _>(move |c, r| {
            let ctx = x2.clone();
            async move {
                ctx.read_ok()?;
                let rec = ctx.rec(&r.order.0).ok_or(OrderError::NotFound)?;
                if Some(rec.view.customer.0.as_str()) != c.customer() || Some(rec.tenant.0.as_str()) != c.tenant() {
                    return Err(OrderError::NotFound);
                }
                Ok(rec.view)
            }
        })
        .on::<ops::List, _, _>(move |c, ()| {
            let ctx = x3.clone();
            async move {
                ctx.read_ok()?;
                let cu = c.customer().unwrap_or_default().to_string();
                Ok(ctx.log.read(|m, _| m.recs.values().filter(|r| r.view.customer.0 == cu).map(|r| r.view.clone()).collect()))
            }
        })
        .on::<ops::PaymentSettled, _, _>(move |_, s| {
            let ctx = x4.clone();
            async move {
                if ctx.log.status().role != Role::Leader {
                    return Err(ctx.not_leader());
                }
                // the saga polls Status itself; a settlement only makes sure its driver runs
                if ctx.rec(&s.order.0).is_some_and(|r| !r.view.stage.is_final()) {
                    ctx.spawn(&s.order.0);
                }
                Ok(())
            }
        })
        .on::<ops::Resume, _, _>(move |_, ()| {
            let ctx = x5.clone();
            async move {
                if ctx.log.status().role != Role::Leader {
                    return Err(ctx.not_leader());
                }
                Ok(ctx.resume())
            }
        })
        .on::<ops::All, _, _>(move |c, ()| {
            let ctx = x6.clone();
            async move {
                ctx.read_ok()?;
                let t = c.tenant().unwrap_or_default().to_string();
                Ok(ctx.log.read(|m, _| m.recs.values().filter(|r| r.tenant.0 == t).map(|r| r.view.clone()).collect()))
            }
        })
        .on::<ops::Role, _, _>(move |_, ()| {
            let ctx = x7.clone();
            let node = node.clone();
            async move {
                let s = ctx.log.status();
                Ok(OrdersRole { role: format!("{:?}", s.role), epoch: s.term, commit: s.commit, last: s.last, leader: s.leader.map(|i| ctx.addrs[i].clone()), node })
            }
        });
    eprintln!("{} on {} (peer {})", cell.instance(), cell.listen(), peer_addrs[cell.replica]);
    server.serve(shop_cell::listen(&cell.listen()).await).await;
}
