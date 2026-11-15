//! `shop.catalog`: products of each tenant, keyed `(tenant, sku)`. Read-mostly, so last-writer-wins is right
//! here; `Put` takes `expect_rev` so two editors cannot lose each other's update.
use shop_catalog_api::*;
use shop_store::Store;
use shop_types::*;
use shop_wire::Server;
use std::sync::{Arc, Mutex};

fn seed(tenant: &str) -> Vec<Product> {
    let p = |sku: &str, title: &str, text: &str, kr: i64, tags: &[&str]| Product {
        sku: Sku(sku.into()),
        title: title.into(),
        text: text.into(),
        price: Money::sek(kr * 100),
        image: format!("img/{sku}.svg"),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        active: true,
        rev: 1,
    };
    let _ = tenant;
    vec![
        p("tent-2p", "Tunnel tent, 2 person", "Four-season tunnel tent for two, 2.4 kg, pitches outer first in rain.", 2499, &["tents"]),
        p("bag-10", "Sleeping bag, -10 °C", "Down sleeping bag, comfort -4 °C, limit -10 °C, packs to 6 litres.", 1899, &["sleep"]),
        p("stove-1", "Canister stove", "Boils a litre in three minutes; folds into its own pot.", 649, &["cook"]),
        p("lamp-400", "Headlamp, 400 lm", "Rechargeable headlamp with red light and a lock against switching on in the pack.", 449, &["light"]),
        p("poles-c", "Trekking poles, carbon", "Three-section carbon poles, 410 g a pair, with winter baskets.", 899, &["walk"]),
        p("socks-w", "Wool hiking socks", "Merino terry socks, two pairs.", 249, &["walk"]),
    ]
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.catalog").await;
    let mut store: Store<Product> = Store::open(&cell.data.join("products.jsonl"));
    if store.is_empty() {
        for p in seed(&cell.tenant) {
            store.put(&format!("{}/{}", cell.tenant, p.sku.0), p);
        }
    }
    let store = Arc::new(Mutex::new(store));
    let (s1, s2, s3, s4) = (store.clone(), store.clone(), store.clone(), store.clone());
    let server = Server::new::<ShopCatalog>(cell.gate.clone(), &cell.node, &cell.instance())
        .on::<ops::Get, _, _>(move |c, r| {
            let s = s1.clone();
            async move {
                let t = c.tenant().unwrap_or_default().to_string();
                s.lock().unwrap().get(&format!("{t}/{}", r.sku.0)).cloned().ok_or(CatalogError::NotFound(r.sku))
            }
        })
        .on::<ops::List, _, _>(move |c, r| {
            let s = s2.clone();
            async move {
                let t = format!("{}/", c.tenant().unwrap_or_default());
                let s = s.lock().unwrap();
                let limit = r.limit.clamp(1, 100) as usize;
                let mut items: Vec<Product> = s
                    .iter()
                    .filter(|(k, p)| k.starts_with(&t) && p.active)
                    .map(|(_, p)| p.clone())
                    .filter(|p| r.tag.as_ref().is_none_or(|tag| p.tags.contains(tag)))
                    .filter(|p| r.after.as_ref().is_none_or(|a| p.sku > *a))
                    .collect();
                items.sort_by(|a, b| a.sku.cmp(&b.sku));
                let next = (items.len() > limit).then(|| items[limit - 1].sku.clone());
                items.truncate(limit);
                Ok(Page { items, next })
            }
        })
        .on::<ops::Price, _, _>(move |c, r| {
            let s = s3.clone();
            async move {
                let t = c.tenant().unwrap_or_default().to_string();
                let s = s.lock().unwrap();
                let mut lines = Vec::new();
                let mut total = Money::sek(0);
                for l in r.lines {
                    let p = s.get(&format!("{t}/{}", l.sku.0)).filter(|p| p.active).ok_or(CatalogError::NotFound(l.sku.clone()))?;
                    let lt = p.price.checked_mul(l.qty).ok_or(CatalogError::Invalid("overflow".into()))?;
                    total = total.checked_add(&lt).ok_or(CatalogError::Invalid("currencies differ".into()))?;
                    lines.push(PricedLine { sku: l.sku, title: p.title.clone(), qty: l.qty, unit: p.price.clone(), total: lt });
                }
                Ok(Priced { lines, total })
            }
        })
        .on::<ops::Put, _, _>(move |c, r| {
            let s = s4.clone();
            async move {
                let t = c.tenant().unwrap_or_default().to_string();
                let k = format!("{t}/{}", r.product.sku.0);
                let mut s = s.lock().unwrap();
                let have = s.get(&k).map(|p| p.rev).unwrap_or(0);
                if have != r.expect_rev {
                    return Err(CatalogError::Stale { have });
                }
                let mut p = r.product;
                p.rev = have + 1;
                let rev = p.rev;
                s.put(&k, p);
                Ok(rev)
            }
        });
    eprintln!("{} on {}", cell.instance(), cell.listen());
    server.serve(shop_cell::listen(&cell.listen()).await).await;
}
