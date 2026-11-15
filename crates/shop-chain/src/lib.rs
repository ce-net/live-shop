//! Capability chains for cell shop.
//!
//! A chain is a list of links. The first link is signed by the tenant root key (held by `shop.keys`); every
//! later link is signed by the holder of the link before it and may only narrow it: fewer rights, the same or
//! more caveats, an earlier expiry. A request carries the chain and a proof signed by the leaf holder over the
//! exact request bytes, so a chain copied off the wire is useless without the holder's key.
//!
//! The caveats are the ones cap v3 names for this shop (`tenant`, `customer`, `checkout`, `order`, `amount`,
//! `once`, `until`). When `ce-gate` verifies cap v3 itself this crate is replaced by it.
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Restrictions a link puts on everything below it. `None` means "not restricted here".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Caveats {
    pub tenant: String,
    pub customer: Option<String>,
    pub checkout: Option<String>,
    pub order: Option<String>,
    /// Amount in the smallest unit and the currency code, for example `(49900, "SEK")`.
    pub amount: Option<(i64, String)>,
    /// The leaf of a chain with `once` may be used for one effect only; the callee records its chain id.
    pub once: bool,
    /// Unix seconds after which the link is void.
    pub until: u64,
}

#[derive(Serialize)]
struct LinkBody<'a> {
    issuer: &'a str,
    holder: &'a str,
    rights: &'a [String],
    caveats: &'a Caveats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Hex Ed25519 public key of the signer.
    pub issuer: String,
    /// Hex Ed25519 public key of the holder: the only key that can use or narrow this link.
    pub holder: String,
    pub rights: Vec<String>,
    pub caveats: Caveats,
    pub sig: String,
}

impl Link {
    fn body_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&LinkBody { issuer: &self.issuer, holder: &self.holder, rights: &self.rights, caveats: &self.caveats })
            .expect("link encodes")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chain(pub Vec<Link>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainError(pub String);
impl std::fmt::Display for ChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ChainError {}
fn err<T>(s: impl Into<String>) -> Result<T, ChainError> {
    Err(ChainError(s.into()))
}

/// An Ed25519 key pair. Cells get theirs from `shop.keys`; sessions make a fresh one.
#[derive(Clone)]
pub struct Identity(SigningKey);

impl Identity {
    pub fn generate() -> Identity {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("os random");
        Identity(SigningKey::from_bytes(&seed))
    }
    pub fn from_seed(seed: [u8; 32]) -> Identity {
        Identity(SigningKey::from_bytes(&seed))
    }
    /// Deterministic child key: `sha256(parent seed || label)`. `shop.keys` derives every cell key from the
    /// tenant root this way, so replicas of one cell share one identity and nothing else has to be stored.
    pub fn derive(&self, label: &str) -> Identity {
        let mut h = Sha256::new();
        h.update(self.0.to_bytes());
        h.update(label.as_bytes());
        Identity::from_seed(h.finalize().into())
    }
    pub fn seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
    pub fn public_hex(&self) -> String {
        hex::encode(self.0.verifying_key().to_bytes())
    }
    pub fn sign(&self, msg: &[u8]) -> String {
        hex::encode(self.0.sign(msg).to_bytes())
    }
}

fn vk(hex_key: &str) -> Result<VerifyingKey, ChainError> {
    let b: [u8; 32] = hex::decode(hex_key).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| ChainError("bad key".into()))?;
    VerifyingKey::from_bytes(&b).map_err(|_| ChainError("bad key".into()))
}

fn check_sig(key_hex: &str, msg: &[u8], sig_hex: &str) -> Result<(), ChainError> {
    let b: [u8; 64] = hex::decode(sig_hex).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| ChainError("bad signature".into()))?;
    vk(key_hex)?.verify(msg, &Signature::from_bytes(&b)).map_err(|_| ChainError("signature does not verify".into()))
}

/// `granted` covers `needed` when equal, or when `granted` is `"<cell>:*"` and `needed` starts with `"<cell>:"`.
pub fn right_covers(granted: &str, needed: &str) -> bool {
    granted == needed || granted.strip_suffix('*').is_some_and(|p| p.ends_with(':') && needed.starts_with(p))
}

/// Is `child` the same as or narrower than `parent`?
pub fn caveats_narrower(child: &Caveats, parent: &Caveats) -> Result<(), ChainError> {
    if child.tenant != parent.tenant {
        return err("tenant widened");
    }
    fn keep<T: PartialEq>(name: &str, c: &Option<T>, p: &Option<T>) -> Result<(), ChainError> {
        match (c, p) {
            (_, None) => Ok(()),
            (Some(c), Some(p)) if c == p => Ok(()),
            _ => err(format!("{name} widened")),
        }
    }
    keep("customer", &child.customer, &parent.customer)?;
    keep("checkout", &child.checkout, &parent.checkout)?;
    keep("order", &child.order, &parent.order)?;
    keep("amount", &child.amount, &parent.amount)?;
    if parent.once && !child.once {
        return err("once dropped");
    }
    if child.until > parent.until {
        return err("expiry widened");
    }
    Ok(())
}

/// What a verified chain grants: the leaf's rights and caveats, and the chain id (hash of the leaf link).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    pub chain_id: String,
    pub holder: String,
    pub rights: Vec<String>,
    pub caveats: Caveats,
}

impl Verified {
    pub fn allows(&self, right: &str) -> bool {
        self.rights.iter().any(|g| right_covers(g, right))
    }
}

impl Chain {
    /// The root link: `root` grants `rights` under `caveats` to `holder`.
    pub fn root(root: &Identity, holder: &str, rights: Vec<String>, caveats: Caveats) -> Chain {
        let mut l = Link { issuer: root.public_hex(), holder: holder.to_string(), rights, caveats, sig: String::new() };
        l.sig = root.sign(&l.body_bytes());
        Chain(vec![l])
    }

    pub fn leaf(&self) -> &Link {
        self.0.last().expect("chains are never empty")
    }

    /// Id of the chain: the first 16 bytes of `sha256(leaf link)` in hex. Also the root of the trace id.
    pub fn id(&self) -> String {
        let l = self.leaf();
        let mut h = Sha256::new();
        h.update(l.body_bytes());
        h.update(l.sig.as_bytes());
        hex::encode(&h.finalize()[..16])
    }

    /// Narrow the chain: `me` must hold the leaf. The new link is refused here already if it widens anything.
    pub fn narrow(&self, me: &Identity, holder: &str, rights: Vec<String>, caveats: Caveats) -> Result<Chain, ChainError> {
        let leaf = self.leaf();
        if leaf.holder != me.public_hex() {
            return err("not the holder of this chain");
        }
        for r in &rights {
            if !leaf.rights.iter().any(|g| right_covers(g, r)) {
                return err(format!("right {r} widened"));
            }
        }
        caveats_narrower(&caveats, &leaf.caveats)?;
        let mut l = Link { issuer: me.public_hex(), holder: holder.to_string(), rights, caveats, sig: String::new() };
        l.sig = me.sign(&l.body_bytes());
        let mut v = self.0.clone();
        v.push(l);
        Ok(Chain(v))
    }

    /// Verify every link against `root_hex` (the tenant root public key) at time `now`.
    pub fn verify(&self, root_hex: &str, now: u64) -> Result<Verified, ChainError> {
        let Some(first) = self.0.first() else { return err("empty chain") };
        if first.issuer != root_hex {
            return err("chain does not start at the tenant root");
        }
        let mut prev: Option<&Link> = None;
        for l in &self.0 {
            if let Some(p) = prev {
                if l.issuer != p.holder {
                    return err("link not signed by the holder of the link above");
                }
                for r in &l.rights {
                    if !p.rights.iter().any(|g| right_covers(g, r)) {
                        return err(format!("right {r} widened"));
                    }
                }
                caveats_narrower(&l.caveats, &p.caveats)?;
            }
            check_sig(&l.issuer, &l.body_bytes(), &l.sig)?;
            if l.caveats.until < now {
                return err("expired");
            }
            prev = Some(l);
        }
        let leaf = self.leaf();
        Ok(Verified { chain_id: self.id(), holder: leaf.holder.clone(), rights: leaf.rights.clone(), caveats: leaf.caveats.clone() })
    }
}

/// What a caller attaches to a request: its chain and a signature by the leaf holder over the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub chain: Chain,
    pub ts: u64,
    pub proof: String,
}

fn request_digest(api: &str, op: u16, body: &str, ts: u64) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(api.as_bytes());
    h.update([0]);
    h.update(op.to_le_bytes());
    h.update(ts.to_le_bytes());
    h.update(body.as_bytes());
    h.finalize().to_vec()
}

/// A chain together with the key of its leaf holder: everything a cell needs to make calls.
#[derive(Clone)]
pub struct Holder {
    pub id: Identity,
    pub chain: Chain,
}

impl Holder {
    pub fn auth(&self, api: &str, op: u16, body: &str) -> Auth {
        let ts = now();
        Auth { chain: self.chain.clone(), ts, proof: self.id.sign(&request_digest(api, op, body, ts)) }
    }
}

/// Verify an `Auth` attached to a request `(api, op, body)`: the chain, the holder's proof and its age.
pub fn verify_request(root_hex: &str, auth: &Auth, api: &str, op: u16, body: &str, now: u64) -> Result<Verified, ChainError> {
    let v = auth.chain.verify(root_hex, now)?;
    if auth.ts + 120 < now || auth.ts > now + 120 {
        return err("request proof too old");
    }
    check_sig(&v.holder, &request_digest(api, op, body, auth.ts), &auth.proof).map_err(|_| ChainError("proof not by the chain holder".into()))?;
    Ok(v)
}

/// A 32-byte seed from its hex form.
pub fn hex_seed(s: &str) -> Option<[u8; 32]> {
    hex::decode(s).ok()?.try_into().ok()
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Identity, Identity, Chain) {
        let root = Identity::generate();
        let gw = Identity::generate();
        let c = Chain::root(
            &root,
            &gw.public_hex(),
            vec!["shop.orders:place".into(), "shop.pay:charge".into(), "shop.cart:own".into()],
            Caveats { tenant: "umea-outdoor".into(), until: now() + 600, ..Default::default() },
        );
        (root, gw, c)
    }

    #[test]
    fn narrow_and_verify() {
        let (root, gw, c) = setup();
        let orders = Identity::generate();
        let k = c
            .narrow(&gw, &orders.public_hex(), vec!["shop.pay:charge".into()], Caveats {
                tenant: "umea-outdoor".into(),
                customer: Some("c1".into()),
                amount: Some((49900, "SEK".into())),
                once: true,
                until: now() + 300,
                ..Default::default()
            })
            .unwrap();
        let v = k.verify(&root.public_hex(), now()).unwrap();
        assert!(v.allows("shop.pay:charge"));
        assert!(!v.allows("shop.orders:place"));
        assert!(v.caveats.once);
        assert_eq!(v.chain_id.len(), 32);
    }

    #[test]
    fn widen_refused() {
        let (root, gw, c) = setup();
        let s = Identity::generate();
        let n = c.narrow(&gw, &s.public_hex(), vec!["shop.cart:own".into()], Caveats { tenant: "umea-outdoor".into(), customer: Some("c1".into()), until: now() + 60, ..Default::default() }).unwrap();
        // a right the parent does not hold
        assert!(n.narrow(&s, &s.public_hex(), vec!["shop.pay:charge".into()], n.leaf().caveats.clone()).is_err());
        // dropping the customer caveat
        assert!(n.narrow(&s, &s.public_hex(), vec!["shop.cart:own".into()], Caveats { tenant: "umea-outdoor".into(), until: now() + 60, ..Default::default() }).is_err());
        // a later expiry
        let mut later = n.leaf().caveats.clone();
        later.until += 1000;
        assert!(n.narrow(&s, &s.public_hex(), vec!["shop.cart:own".into()], later).is_err());
        // a hand-built widening link is refused by verify
        let mut forged = n.clone();
        forged.0[1].rights.push("shop.pay:charge".into());
        assert!(forged.verify(&root.public_hex(), now()).is_err());
    }

    #[test]
    fn wrong_tenant_refused() {
        let (root, gw, c) = setup();
        let s = Identity::generate();
        assert!(c.narrow(&gw, &s.public_hex(), vec!["shop.cart:own".into()], Caveats { tenant: "other".into(), until: now() + 60, ..Default::default() }).is_err());
        let other_root = Identity::generate();
        assert!(c.verify(&other_root.public_hex(), now()).is_err());
        let _ = root;
    }

    #[test]
    fn once_cannot_be_dropped_and_proof_binds_body() {
        let (root, gw, c) = setup();
        let o = Identity::generate();
        let once = c.narrow(&gw, &o.public_hex(), vec!["shop.pay:charge".into()], Caveats { tenant: "umea-outdoor".into(), once: true, until: now() + 60, ..Default::default() }).unwrap();
        assert!(once.narrow(&o, &o.public_hex(), vec!["shop.pay:charge".into()], Caveats { tenant: "umea-outdoor".into(), once: false, until: now() + 60, ..Default::default() }).is_err());
        let h = Holder { id: o.clone(), chain: once };
        let a = h.auth("shop.pay", 2, "{\"amount\":1}");
        assert!(verify_request(&root.public_hex(), &a, "shop.pay", 2, "{\"amount\":1}", now()).is_ok());
        assert!(verify_request(&root.public_hex(), &a, "shop.pay", 2, "{\"amount\":2}", now()).is_err());
        // someone else's key cannot use the chain
        let thief = Holder { id: Identity::generate(), chain: h.chain.clone() };
        let a2 = thief.auth("shop.pay", 2, "{}");
        assert!(verify_request(&root.public_hex(), &a2, "shop.pay", 2, "{}", now()).is_err());
    }

    #[test]
    fn wildcard_rights() {
        assert!(right_covers("shop.cart:*", "shop.cart:own"));
        assert!(!right_covers("shop.cart:*", "shop.catalog:read"));
        assert!(!right_covers("shop.*", "shop.cart:own"));
    }
}
