//! `shop.pay.sim`: a payment provider for test mode. It never sees a card; its only input is a `TestToken`. It
//! keeps a ledger of captures and reversals, is idempotent on the caller's key, and reports outcomes by calling
//! `shop.pay::Webhook` with an HMAC-signed body (at least once; `Fault` can drop, delay or repeat deliveries).
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use shop_pay_api::{ops as pay, WebhookReq};
use shop_pay_sim_api::*;
use shop_store::Store;
use shop_types::*;
use shop_wire::{Client, Server};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Serialize, Deserialize)]
struct Auth {
    key: IdemKey,
    provider_ref: String,
    amount: Money,
    state: SimState,
    reverse_key: Option<IdemKey>,
}

struct St {
    auths: Store<Auth>,
    ledger: Store<SimRow>,
    fault: SimFault,
    seq: u64,
}

struct Ctx {
    st: Mutex<St>,
    pay: Client,
    secret: String,
    latency_ms: u64,
}

impl Ctx {
    fn webhook(self: &Arc<Self>, a: &Auth) {
        let me = self.clone();
        let ev = SimEvent { event: format!("ev-{}-{}", a.provider_ref, state_tag(&a.state)), provider_ref: a.provider_ref.clone(), key: a.key.clone(), state: a.state.clone(), amount: a.amount.clone() };
        tokio::spawn(async move {
            let (drop, times, delay) = {
                let mut g = me.st.lock().unwrap();
                let f = &mut g.fault;
                let drop = f.drop_next > 0;
                f.drop_next = f.drop_next.saturating_sub(1);
                let rep = f.repeat_next > 0;
                f.repeat_next = f.repeat_next.saturating_sub(1);
                (drop, if rep { 2 } else { 1 }, f.delay_ms as u64)
            };
            tokio::time::sleep(Duration::from_millis(delay + 20)).await;
            if drop {
                // a dropped delivery is sent again later, as a real provider would
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            let body = serde_json::to_string(&ev).unwrap();
            let mut m = Hmac::<sha2::Sha256>::new_from_slice(me.secret.as_bytes()).unwrap();
            m.update(body.as_bytes());
            let req = WebhookReq { event: ev.event.clone(), body, sig: hex::encode(m.finalize().into_bytes()) };
            for _ in 0..times {
                for attempt in 0..200u64 {
                    match me.pay.call::<pay::Webhook>(&req).await {
                        Ok(_) => break,
                        Err(_) => tokio::time::sleep(Duration::from_millis(200 + attempt * 50)).await,
                    }
                }
            }
        });
    }
}

fn state_tag(s: &SimState) -> &'static str {
    match s {
        SimState::Captured => "captured",
        SimState::Pending => "pending",
        SimState::Declined(_) => "declined",
        SimState::Reversed => "reversed",
    }
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.pay.sim").await;
    let secret = cell
        .client("shop.keys")
        .call::<shop_keys_api::ops::Secret>(&shop_keys_api::SecretReq { name: shop_policy::WEBHOOK_SECRET.into() })
        .await
        .expect("shop.keys answers")
        .expect("webhook secret");
    let latency_ms: u64 = cell.args.get("latency-ms", "60").parse().unwrap();
    let ctx = Arc::new(Ctx {
        st: Mutex::new(St { auths: Store::open(&cell.data.join("auths.jsonl")), ledger: Store::open(&cell.data.join("ledger.jsonl")), fault: SimFault { drop_next: 0, repeat_next: 0, delay_ms: 0 }, seq: 0 }),
        pay: cell.client("shop.pay"),
        secret,
        latency_ms,
    });
    {
        let mut g = ctx.st.lock().unwrap();
        g.seq = g.auths.len() as u64;
    }
    let (x1, x2, x3, x4) = (ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone());
    let server = Server::new::<ShopPaySim>(cell.gate.clone(), &cell.node, &cell.instance())
        .on::<ops::Authorize, _, _>(move |_, r| {
            let ctx = x1.clone();
            async move {
                if let Some(a) = ctx.st.lock().unwrap().auths.get(&r.key.0).cloned() {
                    if a.amount != r.amount {
                        return Err(SimError::Mismatch("same key, another amount".into()));
                    }
                    return Ok(SimAuth { provider_ref: a.provider_ref, state: a.state });
                }
                // provider latency: the window in which a saga sits between reserve and commit
                let jitter = (now_ms() % 7) * ctx.latency_ms / 3;
                tokio::time::sleep(Duration::from_millis(ctx.latency_ms + jitter)).await;
                if r.token == TestToken::Timeout {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                }
                let (a, slow, dup) = {
                    let mut g = ctx.st.lock().unwrap();
                    if let Some(a) = g.auths.get(&r.key.0).cloned() {
                        return Ok(SimAuth { provider_ref: a.provider_ref, state: a.state });
                    }
                    g.seq += 1;
                    let provider_ref = format!("sim_{:06}_{}", g.seq, &r.key.0[r.key.0.len().saturating_sub(8)..]);
                    let (state, slow, dup) = match &r.token {
                        TestToken::Ok => (SimState::Captured, None, false),
                        TestToken::DuplicateWebhook => (SimState::Captured, None, true),
                        TestToken::SlowWebhook { delay_ms } => (SimState::Pending, Some(*delay_ms as u64), false),
                        TestToken::Declined => (SimState::Declined("declined by test token".into()), None, false),
                        TestToken::InsufficientFunds => (SimState::Declined("insufficient funds (test)".into()), None, false),
                        TestToken::Timeout => (SimState::Declined("provider timeout (test)".into()), None, false),
                    };
                    let a = Auth { key: r.key.clone(), provider_ref, amount: r.amount.clone(), state, reverse_key: None };
                    if a.state == SimState::Captured {
                        g.ledger.put(&format!("cap/{}", r.key.0), SimRow { provider_ref: a.provider_ref.clone(), key: r.key.clone(), kind: SimKind::Capture, amount: r.amount.clone(), at: now_ms() });
                    }
                    g.auths.put(&r.key.0, a.clone());
                    (a, slow, dup)
                };
                match slow {
                    None => {
                        ctx.webhook(&a);
                        if dup {
                            // the same event id twice: shop.pay must apply it once
                            ctx.webhook(&a);
                        }
                    }
                    Some(ms) => {
                        let ctx2 = ctx.clone();
                        let key = a.key.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            let done = {
                                let mut g = ctx2.st.lock().unwrap();
                                let mut a = g.auths.get(&key.0).cloned().unwrap();
                                a.state = SimState::Captured;
                                g.ledger.put(&format!("cap/{}", key.0), SimRow { provider_ref: a.provider_ref.clone(), key: key.clone(), kind: SimKind::Capture, amount: a.amount.clone(), at: now_ms() });
                                g.auths.put(&key.0, a.clone());
                                a
                            };
                            ctx2.webhook(&done);
                        });
                    }
                }
                Ok(SimAuth { provider_ref: a.provider_ref, state: a.state })
            }
        })
        .on::<ops::Reverse, _, _>(move |_, r| {
            let ctx = x2.clone();
            async move {
                let a = {
                    let mut g = ctx.st.lock().unwrap();
                    let Some(mut a) = g.auths.iter().map(|(_, a)| a.clone()).find(|a| a.provider_ref == r.provider_ref) else {
                        return Err(SimError::Unknown(r.provider_ref));
                    };
                    match &a.state {
                        SimState::Reversed => return Ok(SimAuth { provider_ref: a.provider_ref, state: a.state }),
                        SimState::Captured => {}
                        other => return Err(SimError::Mismatch(format!("cannot reverse a payment in state {other:?}"))),
                    }
                    a.state = SimState::Reversed;
                    a.reverse_key = Some(r.key.clone());
                    g.ledger.put(&format!("rev/{}", a.key.0), SimRow { provider_ref: a.provider_ref.clone(), key: r.key.clone(), kind: SimKind::Reverse, amount: a.amount.clone(), at: now_ms() });
                    g.auths.put(&a.key.0.clone(), a.clone());
                    a
                };
                ctx.webhook(&a);
                Ok(SimAuth { provider_ref: a.provider_ref, state: a.state })
            }
        })
        .on::<ops::Ledger, _, _>(move |_, ()| {
            let ctx = x3.clone();
            async move { Ok(ctx.st.lock().unwrap().ledger.iter().map(|(_, r)| r.clone()).collect()) }
        })
        .on::<ops::Fault, _, _>(move |_, f| {
            let ctx = x4.clone();
            async move {
                ctx.st.lock().unwrap().fault = f;
                Ok(())
            }
        });
    eprintln!("{} on {} (test provider)", cell.instance(), cell.listen());
    server.serve(shop_cell::listen(&cell.listen()).await).await;
}
