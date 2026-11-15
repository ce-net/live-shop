//! `shop.pay`: charges in test mode only. It refuses to start when `shop.keys` holds anything under
//! `shop/pay/live/`, and the only provider client compiled in is `shop-pay-sim-api`.
//!
//! - `Charge` needs a chain narrowed to this order and amount with `once`; the chain id is recorded in
//!   `charges_used`, so a second use of the chain is `AlreadyUsed`.
//! - `key` maps to the first request's hash and answer: the same key with the same request returns the stored
//!   payment, with another request it is `Denied`. The row is written (`Pending`) before the provider is called.
//! - `Webhook` checks the HMAC and applies each event id once (`events_seen`).
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use shop_orders_api::{ops as orders, Settled};
use shop_pay_api::*;
use shop_pay_sim_api::{ops as sim, SimEvent, SimReq, SimReverse, SimState};
use shop_store::Store;
use shop_types::*;
use shop_wire::{Caller, Client, Replicas, Server};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
struct Idem {
    hash: String,
    order: String,
}

struct St {
    payments: Store<Payment>,
    idem: Store<Idem>,
    events_seen: Store<u64>,
    charges_used: Store<String>,
}

struct Ctx {
    st: Mutex<St>,
    sim: Client,
    orders: Replicas,
    secret: String,
}

fn req_hash(r: &ChargeReq) -> String {
    use sha2::Digest;
    hex::encode(&sha2::Sha256::digest(serde_json::to_vec(&(&r.order, &r.amount, &r.token)).unwrap())[..12])
}

fn map_state(s: &SimState) -> PayState {
    match s {
        SimState::Captured => PayState::Captured,
        SimState::Pending => PayState::Pending,
        SimState::Declined(x) => PayState::Declined(x.clone()),
        SimState::Reversed => PayState::Refunded,
    }
}

impl Ctx {
    async fn ask_provider(&self, tenant: &str, order: &OrderId, token: TestToken) -> Payment {
        let k = format!("{tenant}/{}", order.0);
        let p = self.st.lock().unwrap().payments.get(&k).cloned().expect("row written before the provider call");
        let r = self.sim.call::<sim::Authorize>(&SimReq { key: IdemKey(format!("charge:{}", order.0)), amount: p.amount.clone(), token }).await;
        let mut g = self.st.lock().unwrap();
        let mut cur = g.payments.get(&k).cloned().unwrap();
        if let Ok(Ok(a)) = r {
            cur.provider_ref = a.provider_ref;
            let s = map_state(&a.state);
            // a webhook may have moved it on already; never move a payment backwards
            if cur.state == PayState::Pending {
                cur.state = s;
            }
            g.payments.put(&k, cur.clone());
        }
        cur
    }
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.pay").await;
    let keys = cell.client("shop.keys");
    let names = keys.call::<shop_keys_api::ops::Names>(&()).await.expect("shop.keys answers").expect("names");
    if let Some(n) = names.iter().find(|n| n.starts_with(shop_policy::LIVE_PREFIX)) {
        eprintln!("shop.pay refuses to start: shop.keys holds {n}; this cell runs in test mode only");
        std::process::exit(2);
    }
    let secret = keys
        .call::<shop_keys_api::ops::Secret>(&shop_keys_api::SecretReq { name: shop_policy::WEBHOOK_SECRET.into() })
        .await
        .expect("shop.keys answers")
        .expect("webhook secret");
    let d = &cell.data;
    let ctx = Arc::new(Ctx {
        st: Mutex::new(St {
            payments: Store::open(&d.join("payments.jsonl")),
            idem: Store::open(&d.join("idem.jsonl")),
            events_seen: Store::open(&d.join("events_seen.jsonl")),
            charges_used: Store::open(&d.join("charges_used.jsonl")),
        }),
        sim: cell.client("shop.pay.sim"),
        orders: cell.replicas("shop.orders"),
        secret,
    });
    let (x1, x2, x3, x4, x5) = (ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone());
    let server = Server::new::<ShopPay>(cell.gate.clone(), &cell.node, &cell.instance())
        .on::<ops::Charge, _, _>(move |c: Caller, r| {
            let ctx = x1.clone();
            async move {
                let v = c.chain.clone().ok_or(PayError::Denied("no chain".into()))?;
                let cv = &v.caveats;
                if cv.order.as_deref() != Some(r.order.0.as_str()) {
                    return Err(PayError::Denied("charge chain is not narrowed to this order".into()));
                }
                match &cv.amount {
                    Some(a) if *a == r.amount.caveat() => {}
                    Some(a) => return Err(PayError::AmountMismatch(format!("chain allows {} {}, asked {}", a.0, a.1, r.amount))),
                    None => return Err(PayError::Denied("charge chain names no amount".into())),
                }
                if !cv.once {
                    return Err(PayError::Denied("charge chain is not single-use".into()));
                }
                if r.key.0 != format!("charge:{}", r.order.0) {
                    return Err(PayError::Denied("idempotency key is derived from the order".into()));
                }
                let tenant = cv.tenant.clone();
                let k = format!("{tenant}/{}", r.order.0);
                let h = req_hash(&r);
                {
                    let mut g = ctx.st.lock().unwrap();
                    match g.idem.get(&r.key.0).cloned() {
                        Some(i) if i.hash != h => return Err(PayError::Denied("idempotency key reused with another request".into())),
                        Some(_) => {
                            let p = g.payments.get(&k).cloned().unwrap();
                            if p.state != PayState::Pending {
                                return Ok(p);
                            }
                        }
                        None => {
                            if g.charges_used.get(&v.chain_id).is_some() {
                                return Err(PayError::AlreadyUsed);
                            }
                            g.charges_used.put(&v.chain_id, r.order.0.clone());
                            g.idem.put(&r.key.0, Idem { hash: h, order: r.order.0.clone() });
                            g.payments.put(&k, Payment { id: PaymentId(format!("pay-{}", &r.order.0[2..])), order: r.order.clone(), amount: r.amount.clone(), state: PayState::Pending, mode: Mode::Test, provider_ref: String::new(), at: now_ms() });
                        }
                    }
                }
                Ok(ctx.ask_provider(&tenant, &r.order, r.token).await)
            }
        })
        .on::<ops::Refund, _, _>(move |c, r| {
            let ctx = x2.clone();
            async move {
                let tenant = c.tenant().unwrap_or_default().to_string();
                let k = format!("{tenant}/{}", r.order.0);
                let p = ctx.st.lock().unwrap().payments.get(&k).cloned().ok_or(PayError::NotFound)?;
                if p.amount != r.amount {
                    return Err(PayError::AmountMismatch(format!("payment is {}, refund asked {}", p.amount, r.amount)));
                }
                match p.state {
                    PayState::Captured => {}
                    _ => return Ok(p),
                }
                let a = ctx.sim.call::<sim::Reverse>(&SimReverse { provider_ref: p.provider_ref.clone(), key: r.key }).await;
                match a {
                    Ok(Ok(a)) => {
                        let mut g = ctx.st.lock().unwrap();
                        let mut p = g.payments.get(&k).cloned().unwrap();
                        p.state = map_state(&a.state);
                        g.payments.put(&k, p.clone());
                        Ok(p)
                    }
                    Ok(Err(e)) => Err(PayError::Provider(format!("{e:?}"))),
                    Err(e) => Err(PayError::Provider(e.to_string())),
                }
            }
        })
        .on::<ops::Status, _, _>(move |c, r| {
            let ctx = x3.clone();
            async move {
                let tenant = c.tenant().unwrap_or_default().to_string();
                let k = format!("{tenant}/{}", r.order.0);
                let p = ctx.st.lock().unwrap().payments.get(&k).cloned();
                match p {
                    // still pending: ask the provider again with the same key (it answers the stored outcome)
                    Some(p) if p.state == PayState::Pending && now_ms() > p.at + 1500 => {
                        let r = ctx.sim.call::<sim::Authorize>(&SimReq { key: IdemKey(format!("charge:{}", r.order.0)), amount: p.amount.clone(), token: TestToken::Ok }).await;
                        let mut g = ctx.st.lock().unwrap();
                        let mut cur = g.payments.get(&k).cloned().unwrap();
                        if let Ok(Ok(a)) = r {
                            if cur.state == PayState::Pending {
                                cur.state = map_state(&a.state);
                                cur.provider_ref = a.provider_ref;
                                g.payments.put(&k, cur.clone());
                            }
                        }
                        Ok(Some(cur))
                    }
                    p => Ok(p),
                }
            }
        })
        .on::<ops::Webhook, _, _>(move |_, r| {
            let ctx = x4.clone();
            async move {
                let mut m = Hmac::<sha2::Sha256>::new_from_slice(ctx.secret.as_bytes()).unwrap();
                m.update(r.body.as_bytes());
                let sig = hex::decode(&r.sig).map_err(|_| PayError::Denied("bad signature".into()))?;
                m.verify_slice(&sig).map_err(|_| PayError::Denied("webhook signature does not verify".into()))?;
                let ev: SimEvent = serde_json::from_str(&r.body).map_err(|e| PayError::Provider(e.to_string()))?;
                if ev.event != r.event {
                    return Err(PayError::Denied("event id differs from the signed body".into()));
                }
                let settled = {
                    let mut g = ctx.st.lock().unwrap();
                    if g.events_seen.get(&ev.event).is_some() {
                        return Ok(WebhookAck { applied: false });
                    }
                    g.events_seen.put(&ev.event, now_ms());
                    let order = ev.key.0.strip_prefix("charge:").unwrap_or_default().to_string();
                    let found = g.payments.iter().find(|(_, p)| p.order.0 == order).map(|(k, p)| (k.clone(), p.clone()));
                    match found {
                        Some((k, mut p)) => {
                            let s = map_state(&ev.state);
                            let forward = matches!((&p.state, &s), (PayState::Pending, _) | (PayState::Captured, PayState::Refunded));
                            if forward && s != PayState::Pending {
                                p.state = s;
                                p.provider_ref = ev.provider_ref.clone();
                                g.payments.put(&k, p.clone());
                            }
                            Some(p)
                        }
                        None => None,
                    }
                };
                if let Some(p) = settled {
                    let orders = ctx.orders.clone();
                    tokio::spawn(async move {
                        let s = Settled { order: p.order.clone(), payment: p.id.clone(), state: p.state.clone() };
                        let _ = orders.call::<orders::PaymentSettled>(&s, shop_orders_api::redirect, Duration::from_secs(5)).await;
                    });
                }
                Ok(WebhookAck { applied: true })
            }
        })
        .on::<ops::Payments, _, _>(move |_, ()| {
            let ctx = x5.clone();
            async move { Ok(ctx.st.lock().unwrap().payments.iter().map(|(_, p)| p.clone()).collect()) }
        });
    eprintln!("{} on {} (test mode)", cell.instance(), cell.listen());
    server.serve(shop_cell::listen(&cell.listen()).await).await;
}
