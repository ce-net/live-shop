//! Three in-process replicas, 1000 concurrent one-unit reserves against 50 units, the leader cut off twice
//! while they run. A1: exactly 50 reservations succeed and `reserved <= on_hand` on every replica.
//! A6: after the partitions heal the three logs are identical up to the commit index.
use shop_inventory::{Inv, Out};
use shop_inventory_peer_api::Entry;
use shop_replog::{AppendReq, AppendResp, Config, Log, Peers, ProposeError, Role, VoteReq, VoteResp};
use shop_types::*;
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

struct Mem {
    logs: Vec<OnceLock<Log<Inv>>>,
    me: usize,
    down: Arc<Mutex<HashSet<usize>>>,
}

impl Mem {
    fn cut(&self, p: usize) -> bool {
        let d = self.down.lock().unwrap();
        d.contains(&p) || d.contains(&self.me)
    }
}

type Fut<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Option<T>> + Send>>;

struct Net(Arc<Vec<OnceLock<Log<Inv>>>>, usize, Arc<Mutex<HashSet<usize>>>);
impl Peers<Entry> for Net {
    fn vote(&self, p: usize, r: VoteReq) -> Fut<VoteResp> {
        let m = Mem { logs: vec![], me: self.1, down: self.2.clone() };
        let l = self.0[p].get().cloned();
        let cut = m.cut(p);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            if cut { None } else { l.map(|l| l.handle_vote(r)) }
        })
    }
    fn append(&self, p: usize, r: AppendReq<Entry>) -> Fut<AppendResp> {
        let m = Mem { logs: vec![], me: self.1, down: self.2.clone() };
        let l = self.0[p].get().cloned();
        let cut = m.cut(p);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            if cut { None } else { l.map(|l| l.handle_append(r)) }
        })
    }
}

fn leader(logs: &[OnceLock<Log<Inv>>], down: &HashSet<usize>) -> Option<usize> {
    (0..logs.len()).filter(|i| !down.contains(i)).max_by_key(|i| {
        let s = logs[*i].get().unwrap().status();
        if s.role == Role::Leader { s.term + 1 } else { 0 }
    }).filter(|i| logs[*i].get().unwrap().status().role == Role::Leader)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thousand_reserves_fifty_units_leader_cut_twice() {
    let dir = std::env::temp_dir().join(format!("shop-inv-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let logs: Arc<Vec<OnceLock<Log<Inv>>>> = Arc::new((0..3).map(|_| OnceLock::new()).collect());
    let down = Arc::new(Mutex::new(HashSet::new()));
    for i in 0..3 {
        let l = Log::open(Config { me: i, peers: vec![String::new(); 3], dir: dir.join(i.to_string()), unsafe_local_ack: false }, Arc::new(Net(logs.clone(), i, down.clone())));
        let _ = logs[i].set(l);
    }
    let t = TenantId("umea-outdoor".into());
    let sku = Sku("tent-2p".into());
    // wait for a leader and stock the sku
    let restock = Entry::Restock { tenant: t.clone(), sku: sku.clone(), add: 50, key: IdemKey("seed".into()) };
    loop {
        let l = leader(&logs, &down.lock().unwrap());
        if let Some(l) = l {
            if logs[l].get().unwrap().propose(restock.clone()).await.is_ok() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let ok = Arc::new(Mutex::new(HashSet::new()));
    let mut tasks = Vec::new();
    let total: usize = std::env::var("N").ok().and_then(|v| v.parse().ok()).unwrap_or(1000);
    for n in 0..total {
        let (logs, down, ok, t, sku) = (logs.clone(), down.clone(), ok.clone(), t.clone(), sku.clone());
        tasks.push(tokio::spawn(async move {
            let order = OrderId(format!("o-{n}"));
            let e = Entry::Reserve { tenant: t, order: order.clone(), lines: vec![Line { sku, qty: 1 }], expires: u64::MAX };
            loop {
                let l = leader(&logs, &down.lock().unwrap());
                let Some(l) = l else {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    continue;
                };
                match logs[l].get().unwrap().propose(e.clone()).await {
                    Ok(Out::Resv(_)) => {
                        ok.lock().unwrap().insert(order.0);
                        return;
                    }
                    Ok(Out::Err(_)) => return,
                    Ok(_) => unreachable!(),
                    Err(ProposeError::NotLeader(_)) | Err(ProposeError::NoQuorum) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            }
        }));
    }
    // cut the leader off twice while the reserves run
    let mut cuts = 0;
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let l = leader(&logs, &down.lock().unwrap());
        if let Some(l) = l {
            down.lock().unwrap().insert(l);
            cuts += 1;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            down.lock().unwrap().remove(&l);
        }
    }
    for t in tasks {
        tokio::time::timeout(Duration::from_secs(60), t).await.expect("every buyer got an answer").unwrap();
    }
    assert_eq!(cuts, 2);
    // let the healed replicas catch up
    tokio::time::sleep(Duration::from_secs(2)).await;
    let won = ok.lock().unwrap().len();
    assert_eq!(won, 50, "A1: exactly the stock is reserved, never more");
    let mut tails = Vec::new();
    for i in 0..3 {
        let l = logs[i].get().unwrap();
        let (reserved, viol) = l.read(|m, _| (m.level("umea-outdoor", &sku).reserved, m.violations()));
        assert!(viol.is_empty(), "replica {i}: {viol:?}");
        assert_eq!(reserved, 50, "replica {i} applied every committed reserve");
        tails.push(l.handle_tail(shop_replog::TailReq { from: 1 }));
    }
    let commit = tails.iter().map(|t| t.commit).min().unwrap();
    for t in &tails[1..] {
        assert_eq!(&t.entries[..commit as usize], &tails[0].entries[..commit as usize], "A6: logs agree up to the commit index");
    }
    println!("A1 ok: {won} of 1000 reserved, 2 leader cuts; A6 ok: logs identical up to {commit}");
    let _ = std::fs::remove_dir_all(&dir);
}
