//! `shop.inventory` replica `--replica i` of three. Writes go through the leader's quorum log; `Levels` is
//! answered by any replica with its `as_of`.
use shop_inventory::{Inv, Out};
use shop_inventory_api::*;
use shop_inventory_peer_api::{ops as peer, Entry};
use shop_replog::{Config, Log, ProposeError, Role, WirePeers};
use shop_types::*;
use shop_wire::Server;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.inventory").await;
    let addrs = cell.dir.addrs("shop.inventory");
    let peer_addrs = cell.dir.addrs("shop.inventory.peer");
    let peers = Arc::new(WirePeers::<peer::Vote, peer::Append>::new(peer_addrs.clone(), cell.me.clone()));
    let log: Log<Inv> = Log::open(
        Config { me: cell.replica, peers: peer_addrs.clone(), dir: cell.data.join("log"), unsafe_local_ack: cell.args.has("unsafe-local-ack") },
        peers,
    );
    let peer_server = log.serve::<peer::Vote, peer::Append, peer::Tail>(Server::new::<shop_inventory_peer_api::ShopInventoryPeer>(cell.gate.clone(), &cell.node, &cell.instance()));
    let pl = shop_cell::listen(&peer_addrs[cell.replica]).await;
    tokio::spawn(peer_server.serve(pl));

    let addrs = Arc::new(addrs);
    let err = {
        let addrs = addrs.clone();
        move |e: ProposeError| match e {
            ProposeError::NotLeader(l) => InvError::NotLeader { leader: l.map(|i| addrs[i].clone()) },
            ProposeError::NoQuorum => InvError::NoQuorum,
        }
    };
    let prop = {
        let log = log.clone();
        let err = err.clone();
        move |e: Entry| {
            let log = log.clone();
            let err = err.clone();
            async move { log.propose(e).await.map_err(err) }
        }
    };
    // the leader releases expired reservations
    {
        let log = log.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let now = now_ms();
                let due = log.status().role == Role::Leader && log.read(|m, _| m.resv.values().any(|r| !r.committed && !r.released && r.expires < now));
                if due {
                    let _ = log.propose(Entry::Expire { now_ms: now }).await;
                }
            }
        });
    }
    let tenant_of = |c: &shop_wire::Caller| TenantId(c.tenant().unwrap_or_default().to_string());
    let (l1, l2, l3) = (log.clone(), log.clone(), log.clone());
    let (p1, p2, p3, p4) = (prop.clone(), prop.clone(), prop.clone(), prop.clone());
    let node = cell.node.clone();
    let roleaddrs = addrs.clone();
    let rl = log.clone();
    let server = Server::new::<ShopInventory>(cell.gate.clone(), &cell.node, &cell.instance())
        .role(move || {
            let s = rl.status();
            format!("{:?} epoch {} commit {}", s.role, s.term, s.commit)
        })
        .on::<ops::Levels, _, _>(move |c, r| {
            let log = l1.clone();
            async move {
                let t = c.tenant().unwrap_or_default().to_string();
                Ok(log.read(|m, _| r.skus.iter().map(|s| m.level(&t, s)).collect()))
            }
        })
        .on::<ops::Reserve, _, _>(move |c, r| {
            let p = p1.clone();
            async move {
                if r.lines.is_empty() || r.lines.iter().any(|l| l.qty == 0) {
                    return Err(InvError::OutOfStock { sku: Sku(String::new()), available: 0 });
                }
                let e = Entry::Reserve { tenant: tenant_of(&c), order: r.order, lines: r.lines, expires: now_ms() + r.ttl_s as u64 * 1000 };
                match p(e).await? {
                    Out::Resv(v) => Ok(v),
                    Out::Err(e) => Err(e),
                    _ => unreachable!(),
                }
            }
        })
        .on::<ops::Commit, _, _>(move |c, r| {
            let p = p2.clone();
            async move {
                match p(Entry::Commit { tenant: tenant_of(&c), order: r.order }).await? {
                    Out::Seq(s) => Ok(s),
                    Out::Err(e) => Err(e),
                    _ => unreachable!(),
                }
            }
        })
        .on::<ops::Release, _, _>(move |c, r| {
            let p = p3.clone();
            async move {
                match p(Entry::Release { tenant: tenant_of(&c), order: r.order }).await? {
                    Out::Seq(s) => Ok(s),
                    Out::Err(e) => Err(e),
                    _ => unreachable!(),
                }
            }
        })
        .on::<ops::Restock, _, _>(move |c, r| {
            let p = p4.clone();
            async move {
                match p(Entry::Restock { tenant: tenant_of(&c), sku: r.sku, add: r.add, key: r.key }).await? {
                    Out::Level(l) => Ok(l),
                    Out::Err(e) => Err(e),
                    _ => unreachable!(),
                }
            }
        })
        .on::<ops::Role, _, _>(move |_, ()| {
            let log = l2.clone();
            let node = node.clone();
            let addrs = roleaddrs.clone();
            async move {
                let s = log.status();
                Ok(InvRole { role: format!("{:?}", s.role), epoch: s.term, commit: s.commit, last: s.last, leader: s.leader.map(|i| addrs[i].clone()), node })
            }
        })
        .on::<ops::Reservations, _, _>(move |c, ()| {
            let log = l3.clone();
            async move {
                let t = c.tenant().unwrap_or_default().to_string();
                Ok(log.read(|m, _| {
                    m.resv
                        .iter()
                        .filter(|(k, r)| k.0 == t && !r.released)
                        .map(|(k, r)| Reservation { order: OrderId(k.1.clone()), lines: r.lines.clone(), expires: r.expires, seq: r.seq, committed: r.committed })
                        .collect()
                }))
            }
        });
    eprintln!("{} on {} (peer {})", cell.instance(), cell.listen(), peer_addrs[cell.replica]);
    server.serve(shop_cell::listen(&cell.listen()).await).await;
}
