//! `shop.cart`: one cart per `(tenant, customer)`. Lines are a map sku to `(qty, hlc)`, merged per line, so a cart
//! edited on two devices or nodes converges. `Freeze` fixes the lines of one checkout while its saga runs and is
//! idempotent on the checkout id; the frozen snapshot is kept, so a retried checkout sees the same total.
use serde::{Deserialize, Serialize};
use shop_cart_api::*;
use shop_catalog_api::{ops as catalog, PriceReq};
use shop_store::{Clock, Lww, Store};
use shop_types::*;
use shop_wire::{Caller, Client, Server};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Serialize, Deserialize, Default)]
struct CartRec {
    lines: BTreeMap<String, Lww<u32>>,
    rev: u64,
    frozen: Option<CheckoutId>,
    /// Snapshots by checkout id.
    snaps: BTreeMap<String, Frozen>,
}

struct St {
    carts: Store<CartRec>,
    clock: Clock,
}

fn key(c: &Caller) -> Result<(String, String), CartError> {
    let t = c.tenant().ok_or(CartError::Unknown)?;
    let cu = c.customer().ok_or(CartError::Invalid("chain names no customer".into()))?;
    Ok((format!("{t}/{cu}"), cu.to_string()))
}

async fn priced(catalog: &Client, owner: &str, rec: &CartRec) -> Result<Cart, CartError> {
    let lines: Vec<Line> = rec.lines.iter().filter(|(_, v)| v.v > 0).map(|(k, v)| Line { sku: Sku(k.clone()), qty: v.v }).collect();
    let p = catalog
        .call::<catalog::Price>(&PriceReq { lines })
        .await
        .map_err(|e| CartError::Upstream(e.to_string()))?
        .map_err(|e| CartError::Upstream(format!("{e:?}")))?;
    Ok(Cart {
        owner: CustomerId(owner.into()),
        lines: p.lines.into_iter().map(|l| CartLine { sku: l.sku, title: l.title, qty: l.qty, unit: l.unit, total: l.total }).collect(),
        total: p.total,
        rev: rec.rev,
        frozen: rec.frozen.clone(),
    })
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.cart").await;
    let st = Arc::new(Mutex::new(St { carts: Store::open(&cell.data.join("carts.jsonl")), clock: Clock::new(&cell.instance()) }));
    let catalog = cell.client("shop.catalog");
    let (s1, s2, s3, s4, s5) = (st.clone(), st.clone(), st.clone(), st.clone(), st.clone());
    let (c1, c2, c3, c4) = (catalog.clone(), catalog.clone(), catalog.clone(), catalog.clone());
    let server = Server::new::<ShopCart>(cell.gate.clone(), &cell.node, &cell.instance())
        .on::<ops::Get, _, _>(move |c, ()| {
            let (s, cat) = (s1.clone(), c1.clone());
            async move {
                let (k, owner) = key(&c)?;
                let rec = s.lock().unwrap().carts.get(&k).cloned().unwrap_or_default();
                priced(&cat, &owner, &rec).await
            }
        })
        .on::<ops::Set, _, _>(move |c, r| {
            let (s, cat) = (s2.clone(), c2.clone());
            async move {
                let (k, owner) = key(&c)?;
                if r.qty > 99 {
                    return Err(CartError::Invalid("at most 99 of one item".into()));
                }
                // the sku must exist; pricing the single line checks it
                cat.call::<catalog::Price>(&PriceReq { lines: vec![Line { sku: r.sku.clone(), qty: 1 }] })
                    .await
                    .map_err(|e| CartError::Upstream(e.to_string()))?
                    .map_err(|_| CartError::Invalid(format!("no product {}", r.sku.0)))?;
                let rec = {
                    let mut g = s.lock().unwrap();
                    let mut rec = g.carts.get(&k).cloned().unwrap_or_default();
                    if let Some(f) = &rec.frozen {
                        return Err(CartError::Frozen(f.clone()));
                    }
                    let at = g.clock.now(now_ms());
                    let mut line = rec.lines.get(&r.sku.0).cloned().unwrap_or(Lww { v: 0, at: Default::default() });
                    line.merge(&Lww { v: r.qty, at });
                    rec.lines.insert(r.sku.0.clone(), line);
                    rec.rev += 1;
                    g.carts.put(&k, rec.clone());
                    rec
                };
                priced(&cat, &owner, &rec).await
            }
        })
        .on::<ops::Freeze, _, _>(move |c, r| {
            let (s, cat) = (s3.clone(), c3.clone());
            async move {
                let (k, owner) = key(&c)?;
                if let Some(want) = c.chain.as_ref().and_then(|v| v.caveats.checkout.clone()) {
                    if want != r.checkout.0 {
                        return Err(CartError::Invalid("chain is for another checkout".into()));
                    }
                }
                let rec = s.lock().unwrap().carts.get(&k).cloned().unwrap_or_default();
                if let Some(f) = rec.snaps.get(&r.checkout.0) {
                    return Ok(f.clone());
                }
                if let Some(f) = &rec.frozen {
                    return Err(CartError::Frozen(f.clone()));
                }
                let cart = priced(&cat, &owner, &rec).await?;
                if cart.lines.is_empty() {
                    return Err(CartError::Invalid("cart is empty".into()));
                }
                let f = Frozen {
                    checkout: r.checkout.clone(),
                    lines: cart.lines.iter().map(|l| PricedLine { sku: l.sku.clone(), title: l.title.clone(), qty: l.qty, unit: l.unit.clone(), total: l.total.clone() }).collect(),
                    total: cart.total,
                };
                let mut g = s.lock().unwrap();
                let mut rec = g.carts.get(&k).cloned().unwrap_or_default();
                if let Some(x) = rec.snaps.get(&r.checkout.0) {
                    return Ok(x.clone());
                }
                if let Some(x) = &rec.frozen {
                    return Err(CartError::Frozen(x.clone()));
                }
                rec.frozen = Some(r.checkout.clone());
                rec.snaps.insert(r.checkout.0.clone(), f.clone());
                rec.rev += 1;
                g.carts.put(&k, rec);
                Ok(f)
            }
        })
        .on::<ops::Clear, _, _>(move |c, r| {
            let s = s4.clone();
            async move {
                let (k, _) = key(&c)?;
                let mut g = s.lock().unwrap();
                let mut rec = g.carts.get(&k).cloned().unwrap_or_default();
                if rec.frozen.as_ref() == Some(&r.checkout) {
                    rec.frozen = None;
                    if r.empty {
                        let at = g.clock.now(now_ms());
                        // set each line to 0 with a newer stamp, so a merge with an older replica keeps it empty
                        for v in rec.lines.values_mut() {
                            v.merge(&Lww { v: 0, at: at.clone() });
                        }
                    }
                    rec.rev += 1;
                    g.carts.put(&k, rec);
                }
                Ok(())
            }
        })
        .on::<ops::Quote, _, _>(move |c, r| {
            let (s, cat) = (s5.clone(), c4.clone());
            async move {
                let (k, owner) = key(&c)?;
                let rec = s.lock().unwrap().carts.get(&k).cloned().unwrap_or_default();
                if let Some(f) = rec.snaps.get(&r.checkout.0) {
                    return Ok(f.clone());
                }
                let cart = priced(&cat, &owner, &rec).await?;
                Ok(Frozen {
                    checkout: r.checkout,
                    lines: cart.lines.into_iter().map(|l| PricedLine { sku: l.sku, title: l.title, qty: l.qty, unit: l.unit, total: l.total }).collect(),
                    total: cart.total,
                })
            }
        });
    eprintln!("{} on {}", cell.instance(), cell.listen());
    server.serve(shop_cell::listen(&cell.listen()).await).await;
}
