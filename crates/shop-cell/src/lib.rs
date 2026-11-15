//! What every cell does at start: read its flags and the network directory (`net.json`, written by `shop-net`),
//! get its identity and grant from `shop.keys`, and build the gate that checks every incoming call.
use serde::{Deserialize, Serialize};
use shop_keys_api::{ops as keys, EnrollReq};
use shop_wire::chain::{Chain, Holder, Identity};
use shop_wire::{Args, Client, Gate, Replicas};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Where every cell instance listens. Index `i` of a replicated cell is replica `i`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Directory {
    pub tenant: String,
    pub cells: BTreeMap<String, Vec<String>>,
}

impl Directory {
    pub fn load(path: &str) -> Directory {
        let b = std::fs::read(path).unwrap_or_else(|e| panic!("network directory {path}: {e}"));
        serde_json::from_slice(&b).expect("network directory is JSON")
    }
    pub fn addrs(&self, cell: &str) -> Vec<String> {
        self.cells.get(cell).cloned().unwrap_or_else(|| panic!("{cell} is not in the network directory"))
    }
    pub fn one(&self, cell: &str) -> String {
        self.addrs(cell)[0].clone()
    }
}

pub struct Cell {
    pub name: String,
    pub node: String,
    pub replica: usize,
    pub data: PathBuf,
    pub dir: Directory,
    pub me: Arc<Holder>,
    pub gate: Gate,
    pub tenant: String,
    pub args: Args,
}

impl Cell {
    pub fn instance(&self) -> String {
        format!("{}#{}@{}", self.name, self.replica, self.node)
    }
    pub fn listen(&self) -> String {
        self.dir.addrs(&self.name)[self.replica].clone()
    }
    pub fn client(&self, cell: &str) -> Client {
        Client::with(self.dir.one(cell), Some(self.me.clone()))
    }
    pub fn replicas(&self, cell: &str) -> Replicas {
        Replicas::new(self.dir.addrs(cell), Some(self.me.clone()))
    }
}

/// Enroll `cell` with `shop.keys` at `keys_addr`, retrying until it answers.
pub async fn enroll(keys_addr: &str, cell: &str, instance: &str) -> (Arc<Holder>, Gate, String) {
    let c = Client::new(keys_addr);
    let mut wait = 50;
    loop {
        match c.call::<keys::Enroll>(&EnrollReq { cell: cell.into(), instance: instance.into() }).await {
            Ok(Ok(e)) => {
                let seed: [u8; 32] = shop_wire::chain::hex_seed(&e.seed).expect("seed from keys");
                let chain: Chain = serde_json::from_str(&e.chain).expect("chain from keys");
                return (Arc::new(Holder { id: Identity::from_seed(seed), chain }), Gate { root: e.root }, e.tenant);
            }
            Ok(Err(e)) => panic!("shop.keys refused {cell}: {e:?}"),
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                wait = (wait * 2).min(1000);
            }
        }
    }
}

pub async fn boot(name: &str) -> Cell {
    let args = Args::parse();
    let dir = Directory::load(&args.get("net", "net.json"));
    let node = args.get("node", "local");
    let replica: usize = args.get("replica", "0").parse().expect("--replica is a number");
    let data = PathBuf::from(args.get("data", "data")).join(name).join(replica.to_string());
    std::fs::create_dir_all(&data).expect("data dir");
    let instance = format!("{name}#{replica}@{node}");
    let (me, gate, tenant) = enroll(&dir.one("shop.keys"), name, &instance).await;
    Cell { name: name.into(), node, replica, data, dir, me, gate, tenant, args }
}

pub async fn listen(addr: &str) -> tokio::net::TcpListener {
    tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| panic!("listen {addr}: {e}"))
}
