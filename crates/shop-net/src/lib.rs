//! The deployment of cell shop, in Rust. A [`Network`] names nodes and places cell instances on them; [`plan`]
//! compares that with what actually answers (op 1 of every instance, including its schema hash); [`apply`] starts
//! what is missing or drifted; [`verify`] checks health, schema, replicas, leaders and a smoke call.
use serde::Serialize;
use shop_cell::Directory;
use shop_wire::chain::Holder;
use shop_wire::{schema_hash, Api, Client};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub name: String,
    /// `host` is the machine itself (ops-1); `sim` a simulated node on it with its own data dir and listeners.
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Instance {
    pub cell: &'static str,
    pub binary: &'static str,
    pub node: String,
    pub replica: usize,
    /// Schema hash the instance must answer with: the hash of the contract this build was compiled against.
    pub hash: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Network {
    pub name: String,
    pub tenant: String,
    pub base_port: u16,
    pub nodes: Vec<Node>,
    pub instances: Vec<Instance>,
}

fn inst<A: Api>(binary: &'static str, node: &str, replica: usize) -> Instance {
    Instance { cell: A::NAME, binary, node: node.into(), replica, hash: schema_hash::<A>(), args: vec![] }
}

/// Slice 1: the edge and the single-instance cells on ops-1; inventory and orders as three replicas, one on each
/// of the simulated nodes `sim-a`, `sim-b`, `sim-c`, so stopping a node stops one replica of each.
pub fn shop_slice1(name: &str, base_port: u16) -> Network {
    use shop_catalog_api::ShopCatalog;
    use shop_inventory_api::ShopInventory;
    use shop_orders_api::ShopOrders;
    let host = "ops-1";
    let sims = ["sim-a", "sim-b", "sim-c"];
    let mut instances = vec![
        inst::<shop_keys_api::ShopKeys>("shop-keys", host, 0),
        inst::<ShopCatalog>("shop-catalog", host, 0),
        inst::<shop_cart_api::ShopCart>("shop-cart", host, 0),
        inst::<shop_pay_sim_api::ShopPaySim>("shop-pay-sim", host, 0),
        inst::<shop_pay_api::ShopPay>("shop-pay", host, 0),
    ];
    for (i, n) in sims.iter().enumerate() {
        instances.push(inst::<ShopInventory>("shop-inventory", n, i));
    }
    for (i, n) in sims.iter().enumerate() {
        instances.push(inst::<ShopOrders>("shop-orders", n, i));
    }
    instances.push(inst::<shop_gateway_api::ShopGateway>("shop-gateway", host, 0));
    instances.push(Instance { cell: "shop.web", binary: "shop-web", node: host.into(), replica: 0, hash: String::new(), args: vec![] });
    let mut nodes = vec![Node { name: host.into(), kind: "host" }];
    nodes.extend(sims.iter().map(|s| Node { name: s.to_string(), kind: "sim" }));
    Network { name: name.into(), tenant: shop_types::TEST_TENANT.into(), base_port, nodes, instances }
}

impl Network {
    /// Listen addresses, assigned in placement order from `base_port`. Written to `net.json` for every cell.
    pub fn directory(&self) -> Directory {
        let mut cells: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut port = self.base_port;
        let mut add = |k: String| {
            cells.entry(k).or_default().push(format!("127.0.0.1:{port}"));
            port += 1;
        };
        for i in &self.instances {
            match i.cell {
                "shop.web" => add("shop.web.http".into()),
                c => add(c.to_string()),
            }
            match i.cell {
                "shop.inventory" | "shop.orders" => add(format!("{}.peer", i.cell)),
                "shop.gateway" => add("shop.gateway.http".into()),
                _ => {}
            }
        }
        Directory { tenant: self.tenant.clone(), cells }
    }

    pub fn addr(&self, i: &Instance) -> String {
        let d = self.directory();
        match i.cell {
            "shop.web" => d.one("shop.web.http"),
            c => d.addrs(c)[i.replica].clone(),
        }
    }

    pub fn of_node(&self, node: &str) -> Vec<&Instance> {
        self.instances.iter().filter(|i| i.node == node).collect()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum Action {
    Start { cell: String, node: String, replica: usize },
    Restart { cell: String, node: String, replica: usize, reason: String },
}

/// What `apply` would do: an empty plan means everything placed answers with the expected contract.
pub async fn plan(net: &Network) -> Vec<Action> {
    let mut out = Vec::new();
    for i in &net.instances {
        let api = if i.cell == "shop.web" { None } else { Some(i.cell) };
        let addr = net.addr(i);
        let a = match api {
            None => {
                if tokio::net::TcpStream::connect(&addr).await.is_ok() {
                    continue;
                }
                Action::Start { cell: i.cell.into(), node: i.node.clone(), replica: i.replica }
            }
            Some(api) => match Client::new(addr).health(api).await {
                Ok(h) if h.hash == i.hash && h.api == api => continue,
                Ok(h) => Action::Restart { cell: i.cell.into(), node: i.node.clone(), replica: i.replica, reason: format!("answers {}@{} hash {}, plan wants {}", h.api, h.version, h.hash, i.hash) },
                Err(_) => Action::Start { cell: i.cell.into(), node: i.node.clone(), replica: i.replica },
            },
        };
        out.push(a);
    }
    out
}

/// How instances are started on a node.
pub trait Runner {
    fn start(&mut self, net: &Network, i: &Instance) -> std::io::Result<()>;
    fn stop(&mut self, net: &Network, i: &Instance);
}

/// Processes on this machine, one data dir per (simulated) node. Used for the local sim and the chaos proof.
pub struct Procs {
    pub bin: PathBuf,
    pub data: PathBuf,
    pub web_root: PathBuf,
    pub env: Vec<(String, String)>,
    pub extra: BTreeMap<String, Vec<String>>,
    pub children: BTreeMap<String, Child>,
}

pub fn key(i: &Instance) -> String {
    format!("{}#{}@{}", i.cell, i.replica, i.node)
}

impl Procs {
    pub fn new(bin: PathBuf, data: PathBuf, web_root: PathBuf) -> Procs {
        Procs { bin, data, web_root, env: vec![], extra: BTreeMap::new(), children: BTreeMap::new() }
    }
    fn args(&self, net: &Network, i: &Instance) -> Vec<String> {
        let mut a = vec![
            "--net".into(),
            self.data.join("net.json").display().to_string(),
            "--node".into(),
            i.node.clone(),
            "--replica".into(),
            i.replica.to_string(),
            "--data".into(),
            self.data.join(&i.node).display().to_string(),
        ];
        if i.cell == "shop.web" {
            a.extend(["--root".into(), self.web_root.display().to_string()]);
        }
        a.extend(i.args.iter().cloned());
        if let Some(x) = self.extra.get(i.cell) {
            a.extend(x.iter().cloned());
        }
        let _ = net;
        a
    }
    /// Has a started process exited? Returns its exit code.
    pub fn exited(&mut self, i: &Instance) -> Option<i32> {
        let c = self.children.get_mut(&key(i))?;
        match c.try_wait() {
            Ok(Some(s)) => Some(s.code().unwrap_or(-1)),
            _ => None,
        }
    }
}

impl Runner for Procs {
    fn start(&mut self, net: &Network, i: &Instance) -> std::io::Result<()> {
        std::fs::create_dir_all(self.data.join(&i.node))?;
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.data.join(&i.node).join(format!("{}-{}.log", i.cell, i.replica)))?;
        let mut c = Command::new(self.bin.join(i.binary));
        c.args(self.args(net, i)).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        let child = c.spawn()?;
        std::fs::write(self.data.join(&i.node).join(format!("{}-{}.pid", i.cell, i.replica)), child.id().to_string())?;
        if let Some(mut old) = self.children.insert(key(i), child) {
            let _ = old.kill();
            let _ = old.wait();
        }
        Ok(())
    }
    fn stop(&mut self, _net: &Network, i: &Instance) {
        if let Some(mut c) = self.children.remove(&key(i)) {
            let _ = c.kill();
            let _ = c.wait();
        } else if let Ok(pid) = std::fs::read_to_string(self.data.join(&i.node).join(format!("{}-{}.pid", i.cell, i.replica))) {
            // a process an earlier `shop-net apply` started: its pid file names it
            let _ = Command::new("kill").arg("-KILL").arg(pid.trim()).status();
        }
    }
}

/// systemd --user units on ops-1 (`live-shop-<net>-<cell>-<replica>.service`, restarted on failure).
pub struct Systemd {
    pub procs: Procs,
}

impl Systemd {
    fn unit(net: &Network, i: &Instance) -> String {
        format!("live-shop-{}-{}-{}.service", net.name, i.cell.replace('.', "-"), i.replica)
    }
}

impl Runner for Systemd {
    fn start(&mut self, net: &Network, i: &Instance) -> std::io::Result<()> {
        let dir = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/systemd/user");
        std::fs::create_dir_all(&dir)?;
        std::fs::create_dir_all(self.procs.data.join(&i.node))?;
        let args = self.procs.args(net, i).iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" ");
        let text = format!(
            "[Unit]\nDescription=cell shop {} replica {} on {} ({})\n\n[Service]\nExecStart={} {args}\nRestart=always\nRestartSec=1\n\n[Install]\nWantedBy=default.target\n",
            i.cell,
            i.replica,
            i.node,
            net.name,
            self.procs.bin.join(i.binary).display()
        );
        let u = Self::unit(net, i);
        std::fs::write(dir.join(&u), text)?;
        Command::new("systemctl").args(["--user", "daemon-reload"]).status()?;
        Command::new("systemctl").args(["--user", "enable", "--now", &u]).status()?;
        Command::new("systemctl").args(["--user", "restart", &u]).status()?;
        Ok(())
    }
    fn stop(&mut self, net: &Network, i: &Instance) {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", &Self::unit(net, i)]).status();
    }
}

pub fn write_directory(net: &Network, data: &Path) {
    std::fs::create_dir_all(data).unwrap();
    std::fs::write(data.join("net.json"), serde_json::to_vec_pretty(&net.directory()).unwrap()).unwrap();
}

async fn wait_up(net: &Network, i: &Instance, for_: Duration) -> bool {
    let end = Instant::now() + for_;
    while Instant::now() < end {
        let addr = net.addr(i);
        let ok = if i.cell == "shop.web" { tokio::net::TcpStream::connect(&addr).await.is_ok() } else { Client::new(addr).health(i.cell).await.is_ok_and(|h| h.hash == i.hash) };
        if ok {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Carry out `plan`: `shop.keys` first (every other cell enrolls with it), then the rest, then wait until each
/// answers with its expected contract.
pub async fn apply(net: &Network, data: &Path, runner: &mut dyn Runner) -> Result<Vec<Action>, String> {
    write_directory(net, data);
    let actions = plan(net).await;
    let find = |a: &Action| {
        let (c, n, r) = match a {
            Action::Start { cell, node, replica } | Action::Restart { cell, node, replica, .. } => (cell, node, *replica),
        };
        net.instances.iter().find(|i| i.cell == c && &i.node == n && i.replica == r).unwrap().clone()
    };
    let (keys, rest): (Vec<_>, Vec<_>) = actions.iter().partition(|a| matches!(a, Action::Start { cell, .. } | Action::Restart { cell, .. } if cell == "shop.keys"));
    for a in keys.iter().chain(rest.iter()) {
        let i = find(a);
        if matches!(a, Action::Restart { .. }) {
            runner.stop(net, &i);
        }
        runner.start(net, &i).map_err(|e| format!("start {}: {e}", key(&i)))?;
        if i.cell == "shop.keys" && !wait_up(net, &i, Duration::from_secs(10)).await {
            return Err("shop.keys did not come up".into());
        }
    }
    for a in &actions {
        let i = find(a);
        if !wait_up(net, &i, Duration::from_secs(20)).await {
            return Err(format!("{} did not answer with hash {}", key(&i), i.hash));
        }
    }
    Ok(actions)
}

pub async fn down(net: &Network, runner: &mut dyn Runner) {
    for i in net.instances.iter().rev() {
        runner.stop(net, i);
    }
}

/// The operator's identity (`shop.ops` in `shop-policy`): seeding, verification, the chaos proof.
pub async fn operator(net: &Network) -> Arc<Holder> {
    shop_cell::enroll(&net.directory().one("shop.keys"), "shop.ops", "shop-net").await.0
}

/// Stock for the test tenant. Idempotent: every restock carries a fixed key.
pub async fn seed(net: &Network, op: &Arc<Holder>, tent: u32) -> Result<(), String> {
    use shop_inventory_api::{ops::Restock, RestockReq};
    let inv = shop_wire::Replicas::new(net.directory().addrs("shop.inventory"), Some(op.clone()));
    let stock = [("tent-2p", tent), ("bag-10", 500), ("stove-1", 500), ("lamp-400", 500), ("poles-c", 500), ("socks-w", 500)];
    for (sku, n) in stock {
        let r = RestockReq { sku: shop_types::Sku(sku.into()), add: n, key: shop_types::IdemKey(format!("seed-{sku}-v1")) };
        inv.call::<Restock>(&r, shop_inventory_api::redirect, Duration::from_secs(20)).await.map_err(|e| format!("restock {sku}: {e}"))?.map_err(|e| format!("restock {sku}: {e:?}"))?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Verify {
    pub ok: bool,
    pub checks: Vec<(String, bool, String)>,
}

/// Every instance healthy with its contract's hash; one leader per replicated cell, agreed by a majority; and a
/// smoke call through the gateway.
pub async fn verify(net: &Network) -> Verify {
    let mut checks = Vec::new();
    for i in &net.instances {
        if i.cell == "shop.web" {
            let ok = tokio::net::TcpStream::connect(net.addr(i)).await.is_ok();
            checks.push((key(i), ok, "http listener".into()));
            continue;
        }
        match Client::new(net.addr(i)).health(i.cell).await {
            Ok(h) => checks.push((key(i), h.hash == i.hash, format!("hash {} {}", h.hash, h.role))),
            Err(e) => checks.push((key(i), false, e.to_string())),
        }
    }
    for cell in ["shop.inventory", "shop.orders"] {
        let mut leaders = BTreeMap::new();
        for a in net.directory().addrs(cell) {
            if let Ok(h) = Client::new(a.clone()).health(cell).await {
                if h.role.starts_with("Leader") {
                    let epoch = h.role.split_whitespace().nth(2).unwrap_or("").to_string();
                    leaders.insert(epoch, a);
                }
            }
        }
        checks.push((format!("{cell} leader"), leaders.len() == 1, format!("{leaders:?}")));
    }
    let gw = Client::new(net.directory().one("shop.gateway"));
    let smoke = gw.call::<shop_gateway_api::ops::Products>(&shop_catalog_api::ListReq { tag: None, after: None, limit: 10 }).await;
    let n = smoke.as_ref().ok().and_then(|r| r.as_ref().ok()).map(|p| p.items.len()).unwrap_or(0);
    checks.push(("smoke: gateway Products".into(), n > 0, format!("{n} products")));
    Verify { ok: checks.iter().all(|c| c.1), checks }
}

/// Where the cell binaries are: `SHOP_BIN`, else the nearest directory above the running binary that holds
/// them (`target/<profile>/`; test binaries live deeper, in `deps/` or the build dir).
pub fn bin_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SHOP_BIN") {
        return PathBuf::from(d);
    }
    let exe = std::env::current_exe().unwrap();
    let start = exe.parent().unwrap().to_path_buf();
    start.ancestors().find(|a| a.join("shop-keys").is_file()).map(Path::to_path_buf).unwrap_or(start)
}
