// Generated from crates/shop-gateway-api (shop.gateway@1) by `cargo run -p xtask -- gen`. Do not edit: change the contract and regenerate.
/* eslint-disable */
export const API = "shop.gateway";
export const VERSION = 1;
export const SCHEMA_HASH = "07ccae65a54ea0b7";

/** Every answer: `{ Ok }` or the op's own error type in `{ Err }`. */
export type Result<T, E> = { Ok: T } | { Err: E };

export interface Cart {
  owner: CustomerId;
  lines: CartLine[];
  total: Money;
  rev: number;
  frozen: CheckoutId | null;
}

export interface CartLine {
  sku: Sku;
  title: string;
  qty: number;
  unit: Money;
  total: Money;
}

export interface CartSetReq {
  session: SessionId;
  sku: Sku;
  qty: number;
}

export type CheckoutId = string;

export interface CheckoutReq {
  session: SessionId;
  checkout: CheckoutId;
  pay: TestToken;
}

export type Currency =
  | "Sek"
  | "Eur";

export type CustomerId = string;

export type FailReason =
  | { OutOfStock: Sku }
  | { Declined: string }
  | "Timeout"
  | { Internal: string };

export type GwError =
  | "Unauthenticated"
  | { Denied: string }
  | "NotFound"
  | { RateLimited: { retry_ms: number } }
  | { Upstream: { cell: string; reason: string } }
  | "Unavailable";

export interface HelloReq {
  host: string;
}

export interface ListReq {
  tag: string | null;
  after: Sku | null;
  limit: number;
}

export interface Money {
  amount: number;
  currency: Currency;
}

export type OrderId = string;

export interface OrderReq {
  session: SessionId;
  order: OrderId;
}

export interface OrderView {
  id: OrderId;
  checkout: CheckoutId;
  customer: CustomerId;
  stage: Stage;
  lines: PricedLine[];
  total: Money;
  payment: PaymentId | null;
  at: number;
  test_payment: boolean;
}

export interface Page {
  items: Product[];
  next: Sku | null;
}

export type PaymentId = string;

export interface PricedLine {
  sku: Sku;
  title: string;
  qty: number;
  unit: Money;
  total: Money;
}

export interface Product {
  sku: Sku;
  title: string;
  text: string;
  price: Money;
  image: string;
  tags: string[];
  active: boolean;
  rev: number;
}

export interface ProductView {
  product: Product;
  in_stock: number | null;
}

export interface Session {
  id: SessionId;
  customer: CustomerId;
  email: string;
  expires: number;
}

export type SessionId = string;

export interface ShopInfo {
  name: string;
  tenant: TenantId;
  address: string;
  test_mode: boolean;
  currency: Currency;
}

export interface SignInReq {
  email: string;
}

export type Sku = string;

export type Stage =
  | "Pending"
  | "Reserved"
  | "Charged"
  | "Confirmed"
  | "Releasing"
  | "Refunding"
  | { Failed: FailReason }
  | "Cancelled";

export type TenantId = string;

export type TestToken =
  | "Ok"
  | "Declined"
  | "InsufficientFunds"
  | "Timeout"
  | { SlowWebhook: { delay_ms: number } }
  | "DuplicateWebhook";

/** Op table of shop.gateway@1. */
export interface ShopGatewayOps {
  Hello: { id: 2; right: "shop.gateway:public"; req: HelloReq; ok: ShopInfo; err: GwError };
  SignIn: { id: 3; right: "shop.gateway:public"; req: SignInReq; ok: Session; err: GwError };
  SignOut: { id: 4; right: "shop.gateway:public"; req: SessionId; ok: null; err: GwError };
  Products: { id: 5; right: "shop.gateway:public"; req: ListReq; ok: Page; err: GwError };
  Product: { id: 6; right: "shop.gateway:public"; req: Sku; ok: ProductView; err: GwError };
  Cart: { id: 8; right: "shop.gateway:public"; req: SessionId; ok: Cart; err: GwError };
  CartSet: { id: 9; right: "shop.gateway:public"; req: CartSetReq; ok: Cart; err: GwError };
  Checkout: { id: 10; right: "shop.gateway:public"; req: CheckoutReq; ok: OrderView; err: GwError };
  Order: { id: 11; right: "shop.gateway:public"; req: OrderReq; ok: OrderView; err: GwError };
  Orders: { id: 12; right: "shop.gateway:public"; req: SessionId; ok: OrderView[]; err: GwError };
}

export type OpName = keyof ShopGatewayOps;

/** An implementation of the whole contract, op by op. */
export type Handlers = { [K in OpName]: (req: ShopGatewayOps[K]["req"]) => Promise<Result<ShopGatewayOps[K]["ok"], ShopGatewayOps[K]["err"]>> };

/** Moves one call. `op` and `id` name the op; the answer is the JSON of `Result<ok, err>`. */
export type Transport = (op: OpName, id: number, req: unknown) => Promise<unknown>;

export class ShopGatewayClient {
  constructor(private readonly t: Transport) {}
  /** needs `shop.gateway:public` */
  Hello(req: HelloReq): Promise<Result<ShopInfo, GwError>> {
    return this.t("Hello", 2, req) as Promise<Result<ShopInfo, GwError>>;
  }
  /** needs `shop.gateway:public` */
  SignIn(req: SignInReq): Promise<Result<Session, GwError>> {
    return this.t("SignIn", 3, req) as Promise<Result<Session, GwError>>;
  }
  /** needs `shop.gateway:public` */
  SignOut(req: SessionId): Promise<Result<null, GwError>> {
    return this.t("SignOut", 4, req) as Promise<Result<null, GwError>>;
  }
  /** needs `shop.gateway:public` */
  Products(req: ListReq): Promise<Result<Page, GwError>> {
    return this.t("Products", 5, req) as Promise<Result<Page, GwError>>;
  }
  /** needs `shop.gateway:public` */
  Product(req: Sku): Promise<Result<ProductView, GwError>> {
    return this.t("Product", 6, req) as Promise<Result<ProductView, GwError>>;
  }
  /** needs `shop.gateway:public` */
  Cart(req: SessionId): Promise<Result<Cart, GwError>> {
    return this.t("Cart", 8, req) as Promise<Result<Cart, GwError>>;
  }
  /** needs `shop.gateway:public` */
  CartSet(req: CartSetReq): Promise<Result<Cart, GwError>> {
    return this.t("CartSet", 9, req) as Promise<Result<Cart, GwError>>;
  }
  /** needs `shop.gateway:public` */
  Checkout(req: CheckoutReq): Promise<Result<OrderView, GwError>> {
    return this.t("Checkout", 10, req) as Promise<Result<OrderView, GwError>>;
  }
  /** needs `shop.gateway:public` */
  Order(req: OrderReq): Promise<Result<OrderView, GwError>> {
    return this.t("Order", 11, req) as Promise<Result<OrderView, GwError>>;
  }
  /** needs `shop.gateway:public` */
  Orders(req: SessionId): Promise<Result<OrderView[], GwError>> {
    return this.t("Orders", 12, req) as Promise<Result<OrderView[], GwError>>;
  }
}

/** Calls the cell over HTTP (`POST <base>/rpc/<Op>`); the schema hash travels with every call. */
export function httpTransport(base: string, session: () => string | null = () => null): Transport {
  return async (op, _id, req) => {
    const headers: Record<string, string> = { "content-type": "application/json", "x-shop-schema": SCHEMA_HASH };
    const s = session();
    if (s) headers["x-shop-session"] = s;
    const r = await fetch(`${base}/rpc/${op}`, { method: "POST", headers, body: JSON.stringify(req) });
    if (!r.ok) throw new Error(`${op}: HTTP ${r.status} ${await r.text()}`);
    return r.json();
  };
}

/** Calls an in-page implementation of the contract. */
export function localTransport(h: Handlers): Transport {
  return (op, _id, req) => (h[op] as (r: unknown) => Promise<unknown>)(req);
}
