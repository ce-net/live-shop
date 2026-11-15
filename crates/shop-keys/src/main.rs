//! `shop.keys`: the only cell that holds secrets. It keeps the tenant root seed and the named secrets in its own
//! data dir, derives each cell's identity from the root, and signs each cell's grant from `shop-policy`.
//!
//! Slice 1 trusts enrollment from the node-local network it listens on (loopback); slice 2 binds enrollment to
//! the door's node identity.
use shop_keys_api::*;
use shop_wire::chain::{Caveats, Chain, Identity};
use shop_wire::{Args, Gate, Server};
use std::collections::BTreeMap;
use std::sync::Arc;

fn random_hex() -> String {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).unwrap();
    hex::encode(b)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let dir = shop_cell::Directory::load(&args.get("net", "net.json"));
    let data = std::path::PathBuf::from(args.get("data", "data")).join("shop.keys");
    std::fs::create_dir_all(&data).unwrap();
    let seed_path = data.join("root.seed");
    if !seed_path.exists() {
        std::fs::write(&seed_path, random_hex()).unwrap();
    }
    let root = Identity::from_seed(shop_wire::chain::hex_seed(std::fs::read_to_string(&seed_path).unwrap().trim()).expect("root seed"));
    let sec_path = data.join("secrets.json");
    let mut secrets: BTreeMap<String, String> = std::fs::read(&sec_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if !secrets.contains_key(shop_policy::WEBHOOK_SECRET) {
        secrets.insert(shop_policy::WEBHOOK_SECRET.into(), random_hex());
        std::fs::write(&sec_path, serde_json::to_vec_pretty(&secrets).unwrap()).unwrap();
    }
    let tenant = dir.tenant.clone();
    let root = Arc::new(root);
    let secrets = Arc::new(secrets);
    let gate = Gate { root: root.public_hex() };
    let node = args.get("node", "local");
    let addr = dir.one("shop.keys");
    let (r1, r2, r3, t1) = (root.clone(), root.clone(), root.clone(), tenant.clone());
    let (s1, s2) = (secrets.clone(), secrets.clone());
    let server = Server::new::<ShopKeys>(gate, &node, "shop.keys#0")
        .on::<ops::Enroll, _, _>(move |_, r| {
            let root = r1.clone();
            let tenant = t1.clone();
            async move {
                let rights = shop_policy::grant(&r.cell).ok_or_else(|| KeysError::Unknown(r.cell.clone()))?;
                let id = root.derive(&format!("cell:{}", r.cell));
                let chain = Chain::root(&root, &id.public_hex(), rights, Caveats { tenant: tenant.clone(), until: shop_wire::chain::now() + 30 * 86400, ..Default::default() });
                eprintln!("enrolled {} ({})", r.cell, r.instance);
                Ok(Enrolled { seed: hex::encode(id.seed()), chain: serde_json::to_string(&chain).unwrap(), root: root.public_hex(), tenant })
            }
        })
        .on::<ops::Root, _, _>(move |_, ()| {
            let root = r2.clone();
            async move { Ok(root.public_hex()) }
        })
        .on::<ops::PublicKey, _, _>(move |_, r| {
            let root = r3.clone();
            async move {
                shop_policy::grant(&r.cell).ok_or_else(|| KeysError::Unknown(r.cell.clone()))?;
                Ok(root.derive(&format!("cell:{}", r.cell)).public_hex())
            }
        })
        .on::<ops::Secret, _, _>(move |c, r| {
            let s = s1.clone();
            async move {
                let need = format!("shop.keys:secret/{}", r.name);
                if !c.chain.as_ref().is_some_and(|v| v.allows(&need)) {
                    return Err(KeysError::Refused(format!("chain does not grant {need}")));
                }
                s.get(&r.name).cloned().ok_or(KeysError::Unknown(r.name))
            }
        })
        .on::<ops::Names, _, _>(move |_, ()| {
            let s = s2.clone();
            async move { Ok(s.keys().cloned().collect()) }
        });
    eprintln!("shop.keys on {addr}");
    server.serve(shop_cell::listen(&addr).await).await;
}
