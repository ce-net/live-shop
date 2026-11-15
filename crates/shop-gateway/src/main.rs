//! `shop.gateway`: the one cell with a public listener (HTTP for the browser, the typed wire for Rust clients;
//! both run the same handlers). Cells behind it never see a session id: the gateway turns a session into a chain
//! narrowed to that customer, and a checkout into a chain narrowed to that checkout, its amount, `once`, handed
//! to `shop.orders`.
use shop_cart_api::{ops as cart, QuoteReq, SetLine};
use shop_catalog_api::{ops as catalog, GetReq};
use shop_gateway_api::*;
use shop_inventory_api::{ops as inv, LevelsReq};
use shop_orders_api::{ops as orders, OrderGet, PlaceReq};
use shop_types::*;
use shop_wire::chain::{Caveats, Holder, Identity};
use shop_wire::http::{Request, Response};
use shop_wire::{Client, Replicas, Server, WireError};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Sess {
    customer: CustomerId,
    email: String,
    expires: u64,
    holder: Holder,
}

struct Ctx {
    me: Arc<Holder>,
    tenant: String,
    sessions: Mutex<HashMap<String, Arc<Sess>>>,
    catalog: Client,
    cart: Client,
    inv: Replicas,
    orders: Replicas,
    orders_key: String,
}

fn up(cell: &str, e: impl std::fmt::Debug) -> GwError {
    GwError::Upstream { cell: cell.into(), reason: format!("{e:?}") }
}
fn wire(cell: &str, e: WireError) -> GwError {
    match e {
        WireError::Io(_) | WireError::Timeout => GwError::Unavailable,
        WireError::Denied(d) => GwError::Denied(d),
        e => up(cell, e),
    }
}

fn valid_test_email(e: &str) -> bool {
    let Some(local) = e.strip_suffix("@live.shop") else { return false };
    local == "test" || local.strip_prefix("test-").is_some_and(|r| !r.is_empty() && r.len() <= 40 && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

impl Ctx {
    fn session(&self, id: &SessionId) -> Result<Arc<Sess>, GwError> {
        let s = self.sessions.lock().unwrap().get(&id.0).cloned().ok_or(GwError::Unauthenticated)?;
        if s.expires < shop_wire::chain::now() {
            self.sessions.lock().unwrap().remove(&id.0);
            return Err(GwError::Unauthenticated);
        }
        Ok(s)
    }
}

#[tokio::main]
async fn main() {
    let cell = shop_cell::boot("shop.gateway").await;
    let orders_key = cell
        .client("shop.keys")
        .call::<shop_keys_api::ops::PublicKey>(&shop_keys_api::PubReq { cell: "shop.orders".into() })
        .await
        .expect("shop.keys answers")
        .expect("orders key");
    let ctx = Arc::new(Ctx {
        me: cell.me.clone(),
        tenant: cell.tenant.clone(),
        sessions: Mutex::new(HashMap::new()),
        catalog: cell.client("shop.catalog"),
        cart: cell.client("shop.cart"),
        inv: cell.replicas("shop.inventory"),
        orders: cell.replicas("shop.orders"),
        orders_key,
    });
    let x: Vec<Arc<Ctx>> = (0..10).map(|_| ctx.clone()).collect();
    let server = Server::new::<ShopGateway>(cell.gate.clone(), &cell.node, &cell.instance())
        .on::<ops::Hello, _, _>({
            let ctx = x[0].clone();
            move |_, _r| {
                let ctx = ctx.clone();
                async move {
                    Ok(ShopInfo { name: "cell shop".into(), tenant: TenantId(ctx.tenant.clone()), address: "Kungsgatan 103, 903 31 Umeå, Sweden".into(), test_mode: true, currency: Currency::Sek })
                }
            }
        })
        .on::<ops::SignIn, _, _>({
            let ctx = x[1].clone();
            move |_, r| {
                let ctx = ctx.clone();
                async move {
                    let email = r.email.trim().to_ascii_lowercase();
                    if !valid_test_email(&email) {
                        return Err(GwError::Denied("slice 1 signs in test customers only: test-<name>@live.shop".into()));
                    }
                    let customer = CustomerId(format!("c-{}", &Identity::from_seed([7u8; 32]).derive(&email).public_hex()[..16]));
                    let key = Identity::generate();
                    let expires = shop_wire::chain::now() + 30 * 60;
                    let chain = ctx
                        .me
                        .chain
                        .narrow(&ctx.me.id, &key.public_hex(), shop_policy::session_rights(), Caveats { tenant: ctx.tenant.clone(), customer: Some(customer.0.clone()), until: expires, ..Default::default() })
                        .map_err(|e| GwError::Denied(e.0))?;
                    let mut b = [0u8; 16];
                    getrandom::getrandom(&mut b).unwrap();
                    let id = SessionId(hex::encode(b));
                    let s = Arc::new(Sess { customer: customer.clone(), email: email.clone(), expires, holder: Holder { id: key, chain } });
                    ctx.sessions.lock().unwrap().insert(id.0.clone(), s);
                    Ok(Session { id, customer, email, expires })
                }
            }
        })
        .on::<ops::SignOut, _, _>({
            let ctx = x[2].clone();
            move |_, id| {
                let ctx = ctx.clone();
                async move {
                    ctx.sessions.lock().unwrap().remove(&id.0);
                    Ok(())
                }
            }
        })
        .on::<ops::Products, _, _>({
            let ctx = x[3].clone();
            move |_, r| {
                let ctx = ctx.clone();
                async move { ctx.catalog.call::<catalog::List>(&r).await.map_err(|e| wire("shop.catalog", e))?.map_err(|e| up("shop.catalog", e)) }
            }
        })
        .on::<ops::Product, _, _>({
            let ctx = x[4].clone();
            move |_, sku| {
                let ctx = ctx.clone();
                async move {
                    let product = match ctx.catalog.call::<catalog::Get>(&GetReq { sku: sku.clone() }).await.map_err(|e| wire("shop.catalog", e))? {
                        Ok(p) => p,
                        Err(shop_catalog_api::CatalogError::NotFound(_)) => return Err(GwError::NotFound),
                        Err(e) => return Err(up("shop.catalog", e)),
                    };
                    // stock is shown when the inventory answers; the page stays browsable when it does not
                    let lv = ctx.inv.call::<inv::Levels>(&LevelsReq { skus: vec![sku] }, shop_inventory_api::redirect, Duration::from_millis(800)).await;
                    let in_stock = match lv {
                        Ok(Ok(l)) => l.first().map(|l| l.on_hand - l.reserved),
                        _ => None,
                    };
                    Ok(ProductView { product, in_stock })
                }
            }
        })
        .on::<ops::Cart, _, _>({
            let ctx = x[5].clone();
            move |_, sid| {
                let ctx = ctx.clone();
                async move {
                    let s = ctx.session(&sid)?;
                    ctx.cart.call_as::<cart::Get>(&s.holder, &()).await.map_err(|e| wire("shop.cart", e))?.map_err(|e| up("shop.cart", e))
                }
            }
        })
        .on::<ops::CartSet, _, _>({
            let ctx = x[6].clone();
            move |_, r| {
                let ctx = ctx.clone();
                async move {
                    let s = ctx.session(&r.session)?;
                    ctx.cart.call_as::<cart::Set>(&s.holder, &SetLine { sku: r.sku, qty: r.qty }).await.map_err(|e| wire("shop.cart", e))?.map_err(|e| up("shop.cart", e))
                }
            }
        })
        .on::<ops::Checkout, _, _>({
            let ctx = x[7].clone();
            move |_, r| {
                let ctx = ctx.clone();
                async move {
                    let s = ctx.session(&r.session)?;
                    if r.checkout.0.len() < 8 || r.checkout.0.len() > 64 {
                        return Err(GwError::Denied("checkout id must be 8 to 64 characters".into()));
                    }
                    let q = ctx.cart.call_as::<cart::Quote>(&s.holder, &QuoteReq { checkout: r.checkout.clone() }).await.map_err(|e| wire("shop.cart", e))?.map_err(|e| up("shop.cart", e))?;
                    if q.lines.is_empty() {
                        return Err(GwError::Denied("the cart is empty".into()));
                    }
                    // the checkout link: this checkout, this amount, once, handed to shop.orders
                    let cav = Caveats {
                        tenant: ctx.tenant.clone(),
                        customer: Some(s.customer.0.clone()),
                        checkout: Some(r.checkout.0.clone()),
                        amount: Some(q.total.caveat()),
                        once: true,
                        until: (shop_wire::chain::now() + 600).min(s.expires),
                        order: None,
                    };
                    let k = s.holder.chain.narrow(&s.holder.id, &ctx.orders_key, shop_policy::checkout_rights(), cav).map_err(|e| GwError::Denied(e.0))?;
                    let req = PlaceReq { checkout: r.checkout, pay: r.pay, chain: serde_json::to_string(&k).unwrap() };
                    ctx.orders
                        .call_as::<orders::Place>(Some(&s.holder), &req, shop_orders_api::redirect, Duration::from_secs(19))
                        .await
                        .map_err(|e| wire("shop.orders", e))?
                        .map_err(|e| up("shop.orders", e))
                }
            }
        })
        .on::<ops::Order, _, _>({
            let ctx = x[8].clone();
            move |_, r| {
                let ctx = ctx.clone();
                async move {
                    let s = ctx.session(&r.session)?;
                    match ctx.orders.call_as::<orders::Get>(Some(&s.holder), &OrderGet { order: r.order }, shop_orders_api::redirect, Duration::from_secs(5)).await.map_err(|e| wire("shop.orders", e))? {
                        Ok(v) => Ok(v),
                        Err(shop_orders_api::OrderError::NotFound) => Err(GwError::NotFound),
                        Err(e) => Err(up("shop.orders", e)),
                    }
                }
            }
        })
        .on::<ops::Orders, _, _>({
            let ctx = x[9].clone();
            move |_, sid| {
                let ctx = ctx.clone();
                async move {
                    let s = ctx.session(&sid)?;
                    let _ = &s.email;
                    ctx.orders.call_as::<orders::List>(Some(&s.holder), &(), shop_orders_api::redirect, Duration::from_secs(5)).await.map_err(|e| wire("shop.orders", e))?.map_err(|e| up("shop.orders", e))
                }
            }
        });
    let server = Arc::new(server);
    let http_addr = cell.dir.one("shop.gateway.http");
    eprintln!("{} on {} (wire) and http://{} (browser)", cell.instance(), cell.listen(), http_addr);
    let hs = server.clone();
    tokio::spawn(shop_wire::http::serve(shop_cell::listen(&http_addr).await, move |r: Request| {
        let s = hs.clone();
        async move { http(&s, r).await }
    }));
    Server::serve_shared(server, shop_cell::listen(&cell.listen()).await).await;
}

fn cors(r: Response) -> Response {
    r.header("access-control-allow-origin", "*")
        .header("access-control-allow-headers", "content-type, x-shop-schema, x-shop-session")
        .header("access-control-allow-methods", "GET, POST, OPTIONS")
}

async fn http(s: &Server, r: Request) -> Response {
    if r.method == "OPTIONS" {
        return cors(Response::new(204, "text/plain", ""));
    }
    if r.method == "GET" && r.path == "/health" {
        let v = s.dispatch(s.api(), 1, "", None, "null".into()).await.unwrap();
        return cors(Response::json(200, &v));
    }
    let Some(op) = r.path.strip_prefix("/rpc/") else { return cors(Response::new(404, "text/plain", "not found")) };
    if r.method != "POST" {
        return cors(Response::new(400, "text/plain", "POST only"));
    }
    let Some(id) = s.op_id(op) else { return cors(Response::new(404, "text/plain", format!("no op {op}"))) };
    let hash = r.headers.get("x-shop-schema").cloned().unwrap_or_default();
    let body = String::from_utf8_lossy(&r.body).to_string();
    match s.dispatch(s.api(), id, &hash, None, body).await {
        Ok(v) => cors(Response::json(200, &v)),
        Err(WireError::Drift { callee, caller }) => cors(Response::new(409, "text/plain", format!("schema drift: gateway {callee}, page {caller}; reload the page"))),
        Err(e) => cors(Response::new(400, "text/plain", e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_emails_only() {
        assert!(super::valid_test_email("test@live.shop"));
        assert!(super::valid_test_email("test-anna-1@live.shop"));
        assert!(!super::valid_test_email("anna@example.com"));
        assert!(!super::valid_test_email("test-@live.shop"));
        assert!(!super::valid_test_email("test-a b@live.shop"));
    }
}
