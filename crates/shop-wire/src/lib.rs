//! The typed-call runtime of cell shop.
//!
//! A contract is written once with [`api!`]. The macro emits the Rust types, one marker type per op and the
//! TypeScript text of every type from the same tokens, so the Rust callers, the generated browser client and the
//! schema hash all come from one source. Every frame carries the hash of the caller's contract; the callee refuses
//! a frame whose hash differs from its own (`WireError::Drift`) instead of misreading it.
//!
//! Every op names the right it needs. The server checks the caller's capability chain (see `shop-chain`) before
//! the handler runs; a handler gets the verified chain as [`Caller`].
pub mod gen;
pub mod http;
mod macros;
pub mod ts;

pub use serde;
pub use serde_json;
pub use shop_chain as chain;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use shop_chain::{Auth, Holder, Verified};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct OpMeta {
    pub id: u16,
    pub name: &'static str,
    pub right: &'static str,
    pub deadline_ms: u64,
}

pub struct OpTs {
    pub id: u16,
    pub name: &'static str,
    pub right: &'static str,
    pub req: String,
    pub ok: String,
    pub err: String,
}

pub trait Api: 'static {
    const NAME: &'static str;
    const VERSION: u32;
    fn ops() -> Vec<OpMeta>;
    fn schema(reg: &mut ts::Registry) -> Vec<OpTs>;
}

pub trait Op: 'static {
    type Api: Api;
    const ID: u16;
    const NAME: &'static str;
    const RIGHT: &'static str;
    const DEADLINE_MS: u64;
    type Req: Serialize + DeserializeOwned + Send + Sync + 'static;
    type Ok: Serialize + DeserializeOwned + Send + 'static;
    type Err: Serialize + DeserializeOwned + Send + 'static;
}

/// The schema text of a contract: every type's TypeScript declaration and every op's line. Its hash is the
/// schema hash; a change to any type reachable from any op (also in another crate) changes it.
pub fn schema_text<A: Api>() -> String {
    let mut reg = ts::Registry::default();
    let ops = A::schema(&mut reg);
    let mut s = format!("{} v{}\n{}", A::NAME, A::VERSION, reg.text());
    for o in ops {
        s.push_str(&format!("{} {}({}) -> {} | {} needs {}\n", o.id, o.name, o.req, o.ok, o.err, o.right));
    }
    s
}

fn fnv64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Hex schema hash of a contract, computed once per process.
pub fn schema_hash<A: Api>() -> String {
    // keyed by type, not by name: two builds of one contract name (old and new) have different hashes
    static CACHE: OnceLock<Mutex<HashMap<std::any::TypeId, String>>> = OnceLock::new();
    let m = CACHE.get_or_init(Default::default);
    let k = std::any::TypeId::of::<A>();
    if let Some(h) = m.lock().unwrap().get(&k) {
        return h.clone();
    }
    let h = format!("{:016x}", fnv64(&schema_text::<A>()));
    m.lock().unwrap().insert(k, h.clone());
    h
}

/// Transport-level failures. Domain failures are the op's own `Err` type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireError {
    /// The caller's chain does not grant the op's right, or does not verify.
    Denied(String),
    /// The caller was built from another contract than the callee.
    Drift { callee: String, caller: String },
    UnknownOp(u16),
    WrongApi(String),
    Bad(String),
    Io(String),
    Timeout,
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for WireError {}

/// Answer of op 1, which every cell serves without a right.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub api: String,
    pub version: u32,
    pub hash: String,
    pub node: String,
    pub instance: String,
    pub pid: u32,
    pub uptime_s: u64,
    /// Free text from the cell, for example `leader epoch 4`.
    pub role: String,
}

#[derive(Serialize, Deserialize)]
struct ReqFrame {
    api: String,
    op: u16,
    hash: String,
    auth: Option<Auth>,
    body: String,
}

#[derive(Serialize, Deserialize)]
enum RespFrame {
    R(serde_json::Value),
    E(WireError),
}

async fn write_frame(s: &mut TcpStream, b: &[u8]) -> std::io::Result<()> {
    s.write_all(&(b.len() as u32).to_be_bytes()).await?;
    s.write_all(b).await?;
    s.flush().await
}

async fn read_frame(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut l = [0u8; 4];
    s.read_exact(&mut l).await?;
    let n = u32::from_be_bytes(l) as usize;
    if n > 16 << 20 {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).await?;
    Ok(b)
}

// ---------------------------------------------------------------------------------------------- client

/// A typed client for one address. `holder` signs every request with the caller's chain.
#[derive(Clone)]
pub struct Client {
    pub addr: String,
    holder: Option<Arc<Holder>>,
}

impl Client {
    pub fn new(addr: impl Into<String>) -> Client {
        Client { addr: addr.into(), holder: None }
    }
    pub fn with(addr: impl Into<String>, holder: Option<Arc<Holder>>) -> Client {
        Client { addr: addr.into(), holder }
    }

    /// Call `O` with the op's own deadline.
    pub async fn call<O: Op>(&self, req: &O::Req) -> Result<Result<O::Ok, O::Err>, WireError> {
        self.call_with::<O>(req, self.holder.as_deref(), Duration::from_millis(O::DEADLINE_MS)).await
    }

    /// Call `O` as `holder` (for example a customer session or a narrowed checkout chain).
    pub async fn call_as<O: Op>(&self, holder: &Holder, req: &O::Req) -> Result<Result<O::Ok, O::Err>, WireError> {
        self.call_with::<O>(req, Some(holder), Duration::from_millis(O::DEADLINE_MS)).await
    }

    pub async fn call_with<O: Op>(&self, req: &O::Req, holder: Option<&Holder>, deadline: Duration) -> Result<Result<O::Ok, O::Err>, WireError> {
        let body = serde_json::to_string(req).map_err(|e| WireError::Bad(e.to_string()))?;
        let v = raw_call(&self.addr, <O::Api as Api>::NAME, O::ID, &schema_hash::<O::Api>(), holder, body, deadline).await?;
        serde_json::from_value(v).map_err(|e| WireError::Bad(format!("answer does not decode: {e}")))
    }

    /// Op 1 of any cell. The hash check is skipped so a probe works across versions.
    pub async fn health(&self, api: &str) -> Result<Health, WireError> {
        let v = raw_call(&self.addr, api, 1, "*", None, "null".into(), Duration::from_millis(1500)).await?;
        serde_json::from_value(v).map_err(|e| WireError::Bad(e.to_string()))
    }
}

async fn raw_call(addr: &str, api: &str, op: u16, hash: &str, holder: Option<&Holder>, body: String, deadline: Duration) -> Result<serde_json::Value, WireError> {
    let auth = holder.map(|h| h.auth(api, op, &body));
    let frame = serde_json::to_vec(&ReqFrame { api: api.into(), op, hash: hash.into(), auth, body }).map_err(|e| WireError::Bad(e.to_string()))?;
    let fut = async {
        let mut s = TcpStream::connect(addr).await.map_err(|e| WireError::Io(format!("{addr}: {e}")))?;
        let _ = s.set_nodelay(true);
        write_frame(&mut s, &frame).await.map_err(|e| WireError::Io(e.to_string()))?;
        let b = read_frame(&mut s).await.map_err(|e| WireError::Io(e.to_string()))?;
        match serde_json::from_slice::<RespFrame>(&b).map_err(|e| WireError::Bad(e.to_string()))? {
            RespFrame::R(v) => Ok(v),
            RespFrame::E(e) => Err(e),
        }
    };
    match tokio::time::timeout(deadline, fut).await {
        Ok(r) => r,
        Err(_) => Err(WireError::Timeout),
    }
}

/// Several replicas of one cell. Calls go to the last address that answered; a transport error, or a domain
/// error `retry` says is a redirect (for example `NotLeader`), moves on to the hinted or the next replica until
/// `deadline`.
#[derive(Clone)]
pub struct Replicas {
    pub addrs: Arc<Vec<String>>,
    cur: Arc<Mutex<usize>>,
    holder: Option<Arc<Holder>>,
}

/// What a replica's refusal says about where to go next.
pub enum Redirect {
    /// A final answer: give it to the caller.
    No,
    /// Try another replica (or the one at this address, if known).
    To(Option<String>),
}

impl Replicas {
    pub fn new(addrs: Vec<String>, holder: Option<Arc<Holder>>) -> Replicas {
        Replicas { addrs: Arc::new(addrs), cur: Arc::new(Mutex::new(0)), holder }
    }

    pub async fn call<O: Op>(&self, req: &O::Req, redirect: impl Fn(&O::Err) -> Redirect, deadline: Duration) -> Result<Result<O::Ok, O::Err>, WireError> {
        self.call_as::<O>(self.holder.as_deref(), req, redirect, deadline).await
    }

    pub async fn call_as<O: Op>(&self, holder: Option<&Holder>, req: &O::Req, redirect: impl Fn(&O::Err) -> Redirect, deadline: Duration) -> Result<Result<O::Ok, O::Err>, WireError> {
        let start = Instant::now();
        let mut last = WireError::Timeout;
        let mut tries = 0u32;
        loop {
            let left = deadline.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(last);
            }
            let i = *self.cur.lock().unwrap() % self.addrs.len();
            let c = Client::with(self.addrs[i].clone(), None);
            let per = left.min(Duration::from_millis(O::DEADLINE_MS.min(4000).max(500)));
            match c.call_with::<O>(req, holder, per).await {
                Ok(Ok(v)) => return Ok(Ok(v)),
                Ok(Err(e)) => match redirect(&e) {
                    Redirect::No => return Ok(Err(e)),
                    Redirect::To(hint) => {
                        let j = hint.and_then(|h| self.addrs.iter().position(|a| *a == h)).unwrap_or(i + 1);
                        *self.cur.lock().unwrap() = j;
                        last = WireError::Io("no leader yet".into());
                    }
                },
                Err(e @ (WireError::Denied(_) | WireError::Drift { .. } | WireError::UnknownOp(_) | WireError::WrongApi(_) | WireError::Bad(_))) => return Err(e),
                Err(e) => {
                    *self.cur.lock().unwrap() = i + 1;
                    last = e;
                }
            }
            tries += 1;
            if tries % self.addrs.len() as u32 == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------- server

/// What the handler knows about the caller: the verified chain, or `None` for an anonymous call to a public op.
#[derive(Debug, Clone)]
pub struct Caller {
    pub chain: Option<Verified>,
}

impl Caller {
    pub fn tenant(&self) -> Option<&str> {
        self.chain.as_ref().map(|v| v.caveats.tenant.as_str())
    }
    pub fn customer(&self) -> Option<&str> {
        self.chain.as_ref().and_then(|v| v.caveats.customer.as_deref())
    }
}

/// The rights check every server runs before a handler (the stand-in for `ce-gate` until it verifies cap v3).
#[derive(Clone)]
pub struct Gate {
    /// Hex public key of the tenant root every chain must start at.
    pub root: String,
}

impl Gate {
    pub fn check(&self, api: &str, op: u16, right: &str, auth: Option<&Auth>, body: &str) -> Result<Caller, WireError> {
        let now = shop_chain::now();
        let public = right.ends_with(":public");
        let v = match auth {
            Some(a) => match shop_chain::verify_request(&self.root, a, api, op, body, now) {
                Ok(v) => Some(v),
                Err(e) if !public => return Err(WireError::Denied(e.0)),
                Err(_) => None,
            },
            None if public => None,
            None => return Err(WireError::Denied(format!("{right} needs a capability chain"))),
        };
        if !public && !v.as_ref().is_some_and(|v| v.allows(right)) {
            return Err(WireError::Denied(format!("chain does not grant {right}")));
        }
        Ok(Caller { chain: v })
    }
}

type BoxFut = Pin<Box<dyn Future<Output = serde_json::Value> + Send>>;
type Handler = Arc<dyn Fn(Caller, String) -> Result<BoxFut, WireError> + Send + Sync>;

pub struct Server {
    api: &'static str,
    version: u32,
    hash: String,
    ops: HashMap<u16, (&'static str, &'static str, Handler)>,
    names: HashMap<&'static str, u16>,
    gate: Gate,
    node: String,
    instance: String,
    started: Instant,
    role: Arc<dyn Fn() -> String + Send + Sync>,
}

impl Server {
    pub fn new<A: Api>(gate: Gate, node: &str, instance: &str) -> Server {
        Server {
            api: A::NAME,
            version: A::VERSION,
            hash: schema_hash::<A>(),
            ops: HashMap::new(),
            names: HashMap::new(),
            gate,
            node: node.into(),
            instance: instance.into(),
            started: Instant::now(),
            role: Arc::new(String::new),
        }
    }

    /// The text op 1 reports as `role` (leader, follower, epoch, ...).
    pub fn role(mut self, f: impl Fn() -> String + Send + Sync + 'static) -> Server {
        self.role = Arc::new(f);
        self
    }

    /// Serve op `O` with `f`. The request is decoded and the right checked before `f` runs.
    pub fn on<O, F, Fut>(mut self, f: F) -> Server
    where
        O: Op,
        F: Fn(Caller, O::Req) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O::Ok, O::Err>> + Send + 'static,
    {
        assert_eq!(<O::Api as Api>::NAME, self.api, "op of another contract");
        let f = Arc::new(f);
        let h: Handler = Arc::new(move |caller, body| {
            let req: O::Req = serde_json::from_str(&body).map_err(|e| WireError::Bad(format!("request does not decode: {e}")))?;
            let f = f.clone();
            Ok(Box::pin(async move { serde_json::to_value(f(caller, req).await).expect("answer encodes") }))
        });
        self.ops.insert(O::ID, (O::NAME, O::RIGHT, h));
        self.names.insert(O::NAME, O::ID);
        self
    }

    pub fn op_id(&self, name: &str) -> Option<u16> {
        self.names.get(name).copied()
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub fn api(&self) -> &'static str {
        self.api
    }

    fn health(&self) -> serde_json::Value {
        serde_json::to_value(Health {
            api: self.api.into(),
            version: self.version,
            hash: self.hash.clone(),
            node: self.node.clone(),
            instance: self.instance.clone(),
            pid: std::process::id(),
            uptime_s: self.started.elapsed().as_secs(),
            role: (self.role)(),
        })
        .unwrap()
    }

    /// Run one request. Also used by the gateway's HTTP front, which hands browser calls to the same handlers.
    pub async fn dispatch(&self, api: &str, op: u16, hash: &str, auth: Option<&Auth>, body: String) -> Result<serde_json::Value, WireError> {
        if api != self.api {
            return Err(WireError::WrongApi(format!("this is {}, not {api}", self.api)));
        }
        if op == 1 {
            return Ok(self.health());
        }
        if hash != self.hash {
            return Err(WireError::Drift { callee: self.hash.clone(), caller: hash.into() });
        }
        let Some((_, right, h)) = self.ops.get(&op) else { return Err(WireError::UnknownOp(op)) };
        let caller = self.gate.check(api, op, right, auth, &body)?;
        Ok(h(caller, body)?.await)
    }

    pub async fn serve(self, listener: TcpListener) {
        Server::serve_shared(Arc::new(self), listener).await
    }

    /// Serve a server that is also used elsewhere (the gateway's HTTP front dispatches into the same one).
    pub async fn serve_shared(me: Arc<Server>, listener: TcpListener) {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { continue };
            let me = me.clone();
            tokio::spawn(async move {
                let _ = s.set_nodelay(true);
                let Ok(b) = read_frame(&mut s).await else { return };
                let resp = match serde_json::from_slice::<ReqFrame>(&b) {
                    Ok(f) => match me.dispatch(&f.api, f.op, &f.hash, f.auth.as_ref(), f.body).await {
                        Ok(v) => RespFrame::R(v),
                        Err(e) => RespFrame::E(e),
                    },
                    Err(e) => RespFrame::E(WireError::Bad(e.to_string())),
                };
                let _ = write_frame(&mut s, &serde_json::to_vec(&resp).unwrap()).await;
            });
        }
    }
}

/// Common flags of every cell binary: `--node`, `--listen`, `--data`, `--net`, read as `--k v` or `SHOP_K`.
pub struct Args(HashMap<String, String>);

impl Args {
    pub fn parse() -> Args {
        let mut m = HashMap::new();
        let mut it = std::env::args().skip(1).peekable();
        while let Some(a) = it.next() {
            if let Some(k) = a.strip_prefix("--") {
                if let Some((k, v)) = k.split_once('=') {
                    m.insert(k.to_string(), v.to_string());
                } else if it.peek().is_some_and(|n| !n.starts_with("--")) {
                    m.insert(k.to_string(), it.next().unwrap());
                } else {
                    m.insert(k.to_string(), "true".into());
                }
            }
        }
        Args(m)
    }
    pub fn get(&self, k: &str, default: &str) -> String {
        self.0
            .get(k)
            .cloned()
            .or_else(|| std::env::var(format!("SHOP_{}", k.to_uppercase().replace('-', "_"))).ok())
            .unwrap_or_else(|| default.to_string())
    }
    pub fn has(&self, k: &str) -> bool {
        self.0.contains_key(k)
    }
}
