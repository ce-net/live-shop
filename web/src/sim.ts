// The read-only page mode: the whole shop.gateway@1 contract implemented in the page, typed by the generated
// `Handlers`. A contract change that this file does not follow fails `tsc` exactly like the live client does.
import type { Cart, CartLine, Handlers, Money, OrderView, Product, Result, Stage } from "../../sdk/ts/shop_gateway";

const sek = (kr: number): Money => ({ amount: kr * 100, currency: "Sek" });
const P = (sku: string, title: string, text: string, kr: number, tags: string[]): Product => ({
  sku, title, text, price: sek(kr), image: `img/${sku}.svg`, tags, active: true, rev: 1,
});

// Same seed as the shop.catalog cell.
const products: Product[] = [
  P("bag-10", "Sleeping bag, -10 °C", "Down sleeping bag, comfort -4 °C, limit -10 °C, packs to 6 litres.", 1899, ["sleep"]),
  P("lamp-400", "Headlamp, 400 lm", "Rechargeable headlamp with red light and a lock against switching on in the pack.", 449, ["light"]),
  P("poles-c", "Trekking poles, carbon", "Three-section carbon poles, 410 g a pair, with winter baskets.", 899, ["walk"]),
  P("socks-w", "Wool hiking socks", "Merino terry socks, two pairs.", 249, ["walk"]),
  P("stove-1", "Canister stove", "Boils a litre in three minutes; folds into its own pot.", 649, ["cook"]),
  P("tent-2p", "Tunnel tent, 2 person", "Four-season tunnel tent for two, 2.4 kg, pitches outer first in rain.", 2499, ["tents"]),
];
const stock = new Map<string, number>(products.map((p) => [p.sku, p.sku === "tent-2p" ? 50 : 500]));
const carts = new Map<string, Map<string, number>>();
const orders = new Map<string, { view: OrderView; started: number; declined: boolean; session: string }>();
const ok = <T>(v: T): Result<T, never> => ({ Ok: v });

function cartOf(session: string): Cart {
  const m = carts.get(session) ?? new Map<string, number>();
  const lines: CartLine[] = [];
  let total = 0;
  for (const [sku, qty] of m) {
    const p = products.find((x) => x.sku === sku);
    if (!p || qty === 0) continue;
    lines.push({ sku, title: p.title, qty, unit: p.price, total: { amount: p.price.amount * qty, currency: "Sek" } });
    total += p.price.amount * qty;
  }
  return { owner: `c-${session.slice(0, 8)}`, lines, total: { amount: total, currency: "Sek" }, rev: 0, frozen: null };
}

// The saga as the live system runs it, on a timer: Pending, Reserved, Charged, then Confirmed or Failed.
function advance(o: { view: OrderView; started: number; declined: boolean }): OrderView {
  const t = Date.now() - o.started;
  let stage: Stage = "Pending";
  if (t > 300) stage = "Reserved";
  if (t > 900) stage = o.declined ? { Failed: { Declined: "declined by test token" } } : "Charged";
  if (t > 1400 && !o.declined) stage = "Confirmed";
  o.view = { ...o.view, stage, payment: t > 900 && !o.declined ? `pay-${o.view.id.slice(2)}` : null };
  return o.view;
}

export const sim: Handlers = {
  Hello: async () => ok({ name: "cell shop", tenant: "umea-outdoor", address: "Kungsgatan 103, 903 31 Umeå, Sweden", test_mode: true, currency: "Sek" }),
  SignIn: async (r) => {
    if (!/^test(-[a-z0-9-]+)?@live\.shop$/.test(r.email)) return { Err: { Denied: "test customers only" } };
    const id = Math.random().toString(16).slice(2) + Math.random().toString(16).slice(2);
    return ok({ id, customer: `c-${id.slice(0, 8)}`, email: r.email, expires: Math.floor(Date.now() / 1000) + 1800 });
  },
  SignOut: async () => ok(null),
  Products: async (r) => ok({ items: products.filter((p) => r.tag === null || p.tags.includes(r.tag)).slice(0, r.limit), next: null }),
  Product: async (sku) => {
    const p = products.find((x) => x.sku === sku);
    return p ? ok({ product: p, in_stock: stock.get(sku) ?? 0 }) : { Err: "NotFound" };
  },
  Cart: async (s) => ok(cartOf(s)),
  CartSet: async (r) => {
    const m = carts.get(r.session) ?? new Map<string, number>();
    m.set(r.sku, r.qty);
    carts.set(r.session, m);
    return ok(cartOf(r.session));
  },
  Checkout: async (r) => {
    const id = `o-${(r.checkout + "0000000000").replace(/[^a-z0-9]/g, "").slice(0, 24)}`;
    const have = orders.get(id);
    if (have) return ok(advance(have));
    const c = cartOf(r.session);
    if (c.lines.length === 0) return { Err: { Denied: "the cart is empty" } };
    const declined = r.pay === "Declined" || r.pay === "InsufficientFunds";
    const view: OrderView = { id, checkout: r.checkout, customer: c.owner, stage: "Pending", lines: c.lines, total: c.total, payment: null, at: Date.now(), test_payment: true };
    orders.set(id, { view, started: Date.now(), declined, session: r.session });
    if (!declined) {
      for (const l of c.lines) stock.set(l.sku, (stock.get(l.sku) ?? 0) - l.qty);
      carts.delete(r.session);
    }
    return ok(view);
  },
  Order: async (r) => {
    const o = orders.get(r.order);
    return o && o.session === r.session ? ok(advance(o)) : { Err: "NotFound" };
  },
  Orders: async (s) => ok([...orders.values()].filter((o) => o.session === s).map(advance)),
};
