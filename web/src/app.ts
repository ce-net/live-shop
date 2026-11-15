// shop.web: four pages (products, product, cart, order) over the generated shop.gateway@1 client.
import {
  ShopGatewayClient, httpTransport, localTransport, SCHEMA_HASH,
  type Money, type OrderView, type Result, type Stage, type TestToken, type GwError, type Session,
} from "../../sdk/ts/shop_gateway";
import { sim } from "./sim";

type Mode = "live" | "sim";
const $ = <T extends HTMLElement>(sel: string): T => document.querySelector(sel) as T;
const app = $("#app");
let gw: ShopGatewayClient;
let mode: Mode = "sim";
let session: Session | null = null;

const store = {
  get(k: string): string | null { try { return localStorage.getItem(k); } catch { return null; } },
  set(k: string, v: string): void { try { localStorage.setItem(k, v); } catch { /* private mode */ } },
};

function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c] as string);
}
function money(m: Money): string {
  const v = (m.amount / 100).toLocaleString("sv-SE", { minimumFractionDigits: 0, maximumFractionDigits: 2 });
  return `${v} ${m.currency === "Sek" ? "kr" : "€"}`;
}
function errText(e: GwError): string {
  if (typeof e === "string") return e === "Unavailable" ? "A cell is restarting. Your checkout is kept; try again." : e;
  if ("Denied" in e) return e.Denied;
  if ("Upstream" in e) return `${e.Upstream.cell}: ${e.Upstream.reason}`;
  return `rate limited, retry in ${e.RateLimited.retry_ms} ms`;
}
function unwrap<T>(r: Result<T, GwError>): T {
  if ("Err" in r) throw new Error(errText(r.Err));
  return r.Ok;
}
function stageName(s: Stage): string {
  if (typeof s === "string") return s;
  if ("Failed" in s) {
    const f = s.Failed;
    if (typeof f === "string") return "Failed: timeout";
    if ("OutOfStock" in f) return `Failed: ${f.OutOfStock} is sold out`;
    if ("Declined" in f) return `Failed: ${f.Declined}`;
    return `Failed: ${f.Internal}`;
  }
  return "Unknown";
}
const final = (s: Stage): boolean => s === "Confirmed" || s === "Cancelled" || (typeof s === "object" && "Failed" in s);

async function ensureSession(): Promise<Session> {
  if (session && session.expires * 1000 > Date.now() + 60_000) return session;
  let email = store.get("shop-email");
  if (!email) {
    email = `test-${Math.random().toString(36).slice(2, 8)}@live.shop`;
    store.set("shop-email", email);
  }
  session = unwrap(await gw.SignIn({ email }));
  store.set("shop-session", JSON.stringify(session));
  return session;
}

async function cartCount(): Promise<void> {
  const s = await ensureSession();
  const r = await gw.Cart(s.id);
  const n = "Ok" in r ? r.Ok.lines.reduce((a, l) => a + l.qty, 0) : 0;
  $("#cart-n").textContent = n > 0 ? String(n) : "";
}

function shell(html: string): void {
  app.innerHTML = html;
  window.scrollTo(0, 0);
}

async function pageList(): Promise<void> {
  const page = unwrap(await gw.Products({ tag: null, after: null, limit: 50 }));
  shell(`
    <section class="hero">
      <p class="crumb">umea-outdoor · ${page.items.length} products</p>
      <h1>Gear for the north</h1>
      <p class="lede">Every part of this store is its own cell with a typed contract: catalog, cart, inventory on three replicas, the order saga, test payments, the gateway and this page.</p>
    </section>
    <ul class="grid">${page.items.map((p) => `
      <li><a class="card" href="#/p/${encodeURIComponent(p.sku)}">
        <img src="${esc(p.image)}" alt="" width="320" height="200" loading="lazy">
        <span class="t">${esc(p.title)}</span>
        <span class="price">${money(p.price)}</span>
      </a></li>`).join("")}
    </ul>`);
}

async function pageProduct(sku: string): Promise<void> {
  const v = unwrap(await gw.Product(sku));
  const p = v.product;
  const stockText = v.in_stock === null ? "stock unknown right now" : v.in_stock > 0 ? `${v.in_stock} in stock` : "sold out";
  shell(`
    <a class="back" href="#/">← All products</a>
    <article class="product">
      <img src="${esc(p.image)}" alt="" width="640" height="400">
      <div>
        <p class="crumb">${esc(p.sku)} · ${esc(p.tags.join(", "))}</p>
        <h1>${esc(p.title)}</h1>
        <p>${esc(p.text)}</p>
        <p class="price big">${money(p.price)}</p>
        <p class="stock ${v.in_stock === 0 ? "bad" : ""}">${stockText}</p>
        <button id="add" class="btn" ${v.in_stock === 0 ? "disabled" : ""}>Add to cart</button>
        <p id="msg" class="msg" role="status"></p>
      </div>
    </article>`);
  $("#add").addEventListener("click", async () => {
    const s = await ensureSession();
    const cart = unwrap(await gw.Cart(s.id));
    const have = cart.lines.find((l) => l.sku === p.sku)?.qty ?? 0;
    const r = await gw.CartSet({ session: s.id, sku: p.sku, qty: have + 1 });
    $("#msg").innerHTML = "Ok" in r ? `In your cart: ${have + 1}. <a href="#/cart">Go to cart →</a>` : esc(errText(r.Err));
    void cartCount();
  });
}

async function pageCart(): Promise<void> {
  const s = await ensureSession();
  const c = unwrap(await gw.Cart(s.id));
  if (c.lines.length === 0) {
    shell(`<h1>Your cart</h1><p class="lede">The cart is empty. <a href="#/">Browse products →</a></p>`);
    return;
  }
  shell(`
    <h1>Your cart</h1>
    <table class="lines">
      ${c.lines.map((l) => `<tr><td>${esc(l.title)}</td>
        <td class="qty"><button class="q" data-sku="${esc(l.sku)}" data-q="${l.qty - 1}" aria-label="One less">−</button>
        <span>${l.qty}</span>
        <button class="q" data-sku="${esc(l.sku)}" data-q="${l.qty + 1}" aria-label="One more">+</button></td>
        <td class="num">${money(l.total)}</td></tr>`).join("")}
      <tr class="sum"><td>Total</td><td></td><td class="num">${money(c.total)}</td></tr>
    </table>
    <fieldset class="pay">
      <legend>Test payment</legend>
      <p class="note">No real money and no card: the payment cell runs in test mode only. Pick what the test provider answers.</p>
      <label><input type="radio" name="tok" value="Ok" checked> Approve</label>
      <label><input type="radio" name="tok" value="Declined"> Decline</label>
      <label><input type="radio" name="tok" value="Slow"> Approve with a slow confirmation</label>
    </fieldset>
    <button id="pay" class="btn">Pay ${money(c.total)} (test)</button>
    <p id="msg" class="msg" role="status"></p>`);
  app.querySelectorAll<HTMLButtonElement>("button.q").forEach((b) =>
    b.addEventListener("click", async () => {
      const r = await gw.CartSet({ session: s.id, sku: b.dataset.sku as string, qty: Math.max(0, Number(b.dataset.q)) });
      if ("Err" in r) $("#msg").textContent = errText(r.Err);
      else void route();
    }),
  );
  $("#pay").addEventListener("click", async () => {
    const pick = (document.querySelector('input[name="tok"]:checked') as HTMLInputElement).value;
    const pay: TestToken = pick === "Slow" ? { SlowWebhook: { delay_ms: 1500 } } : pick === "Declined" ? "Declined" : "Ok";
    // one checkout id per press of Pay; every retry reuses it, so a retry can never make a second order
    const checkout = `k-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
    const btn = $<HTMLButtonElement>("#pay");
    btn.disabled = true;
    for (let attempt = 1; attempt <= 6; attempt++) {
      $("#msg").textContent = attempt === 1 ? "Placing the order…" : `Retrying the same checkout (${attempt})…`;
      try {
        const r = await gw.Checkout({ session: s.id, checkout, pay });
        if ("Ok" in r) { location.hash = `#/order/${encodeURIComponent(r.Ok.id)}`; return; }
        if (r.Err !== "Unavailable") { $("#msg").textContent = errText(r.Err); break; }
      } catch { /* network: retry the same checkout */ }
      await new Promise((res) => setTimeout(res, 800 * attempt));
    }
    btn.disabled = false;
  });
}

const steps: Array<[string, (s: Stage) => boolean]> = [
  ["Order opened", () => true],
  ["Stock reserved", (s) => s !== "Pending"],
  ["Test payment captured", (s) => s === "Charged" || s === "Confirmed"],
  ["Confirmed", (s) => s === "Confirmed"],
];

function renderOrder(o: OrderView): void {
  const failed = typeof o.stage === "object";
  shell(`
    <a class="back" href="#/">← Continue shopping</a>
    <p class="crumb">order ${esc(o.id)} · checkout ${esc(o.checkout)}</p>
    <h1>Your order</h1>
    ${o.test_payment ? `<p class="note">Test payment: no card and no real money were involved.</p>` : ""}
    <ol class="steps">${steps.map(([t, done]) => `<li class="${done(o.stage) && !failed ? "done" : ""}">${t}</li>`).join("")}</ol>
    <p class="stage ${failed ? "bad" : o.stage === "Confirmed" ? "good" : ""}">${esc(stageName(o.stage))}</p>
    <table class="lines">${o.lines.map((l) => `<tr><td>${esc(l.title)}</td><td class="qty">${l.qty}</td><td class="num">${money(l.total)}</td></tr>`).join("")}
      <tr class="sum"><td>Total</td><td></td><td class="num">${money(o.total)}</td></tr></table>
    ${o.payment ? `<p class="note">Payment ${esc(o.payment)} (test mode)</p>` : ""}`);
}

async function pageOrder(id: string): Promise<void> {
  const s = await ensureSession();
  for (;;) {
    const r = await gw.Order({ session: s.id, order: id });
    if ("Err" in r) { shell(`<h1>Order</h1><p class="lede">${esc(errText(r.Err))}</p>`); return; }
    renderOrder(r.Ok);
    void cartCount();
    if (final(r.Ok.stage) || !location.hash.startsWith("#/order/")) return;
    await new Promise((res) => setTimeout(res, 400));
  }
}

async function route(): Promise<void> {
  const h = location.hash || "#/";
  try {
    if (h.startsWith("#/p/")) await pageProduct(decodeURIComponent(h.slice(4)));
    else if (h === "#/cart") await pageCart();
    else if (h.startsWith("#/order/")) await pageOrder(decodeURIComponent(h.slice(8)));
    else await pageList();
  } catch (e) {
    shell(`<h1>Not available right now</h1><p class="lede">${esc(String((e as Error).message ?? e))}</p><p><a href="#/">Back to the products</a></p>`);
  }
}

async function main(): Promise<void> {
  try {
    const cfg = (await (await fetch("config.json", { cache: "no-store" })).json()) as { mode?: string; gateway?: string };
    mode = cfg.mode === "live" ? "live" : "sim";
    gw = new ShopGatewayClient(mode === "live" ? httpTransport(cfg.gateway ?? "") : localTransport(sim));
  } catch {
    gw = new ShopGatewayClient(localTransport(sim));
  }
  const saved = store.get("shop-session");
  if (saved && mode === "live") { try { session = JSON.parse(saved) as Session; } catch { session = null; } }
  const info = unwrap(await gw.Hello({ host: location.host }));
  $("#mode").textContent = mode === "live" ? "live system · test payments" : "in-page contract · read-only";
  $("#mode").title = mode === "live"
    ? `Calls go to shop.gateway@1 (schema ${SCHEMA_HASH}); payments are test mode only.`
    : `This page runs the shop.gateway@1 contract in the browser (schema ${SCHEMA_HASH}); nothing is sent anywhere.`;
  $("#addr").textContent = info.address;
  window.addEventListener("hashchange", () => void route());
  await route();
  void cartCount();
}

void main();
