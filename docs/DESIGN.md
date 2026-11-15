# cell shop: an enterprise store where everything is a cell

Status: design, 2026-11-15. Owner: Leif Rydenfalk, Kungsgatan 103, 903 31 Umeå, Sweden.

cell shop is a live system: a complete e-commerce store (frontend, gateway, catalog, inventory, cart, orders,
payments, customers, search, notifications, receipts) in which every part is a cell with its own typed contract,
its own rights and its own deployment, and in which the deployment itself is typed Rust. Nothing links another
cell's code. A contract change that breaks a caller, including the browser frontend, fails the build.

Payments are **test mode only**. The payment contract has no live mode, no card number field and no live key
slot; section 5.7 says how that is enforced by types. Taking real money is a separate decision for Leif.

## 0. What exists and is reused

| Need | Reused as it is | Where |
|---|---|---|
| Typed contract, client, server | `ce_wire::api!`, `Api`, `Op`, `Server`, `Client` | `repo:rust-stack/crates/ce-wire` |
| Clients in other languages | `ce-wire-gen --lang ts-web\|ts-types\|ts\|py\|swift\|json`, schema hash from rustc | `repo:rust-stack/crates/ce-wire-gen` |
| Rights on every op | `ce-gate` (`needs "<cell>:<right>"`), `cell-check conform` | `repo:rust-stack/crates/ce-gate` |
| Capability chains and policy text | cap v3 (`Grant`, `Caveat`, `Requirement`, `narrow`) and CAL | `repo:ce-net-proto/docs/CAL.md` |
| Records | `#[derive(Record)]`, `#[record(cell, collection, version)]`, key and index fields | `repo:ce-db-rs`, `repo:ce-db-ts` |
| Replicated state | `state.store@2`, last-writer-wins with hybrid logical clocks | `repo:state-store` |
| Placement, leader lease, failover | `ce-replica` (majority-vote single writer, CP) | `repo:replica` |
| Start, stop, logs on a node | `orch@1` (`Up`, `Down`, `Ps`, `Restart`, `Logs`) | `repo:orch` |
| Install, upgrade, config | `cell.install@1` (`Install`, `Upgrade`, `Stop`, `Start`, `ConfigPut`) | `repo:cell-install` |
| Artifacts by hash | `artifact.index@1` (`Publish`, `Resolve`), `cell.build@1` | `repo:artifact.index`, `repo:cell.build` |
| Secrets | `keys` (`Get`, `Put`) | `repo:keys-cell` |
| Tamper-evident log | `audit.log` (`Append`, `Query`, `Verify`, `Trail`) | `repo:audit-log` |
| Search | `search.mesh` (`Local`, `Mesh`), the `ce-search` engine | `repo:ce-search` |
| Browser to cell transport | `wire.ws-door` (the `ts-web` client speaks it) | `repo:wire-ws-door` |
| Macro vocabulary | `net!`, `cap!`, `#[layer]`, `#[verify]`, `import_cell!` | `repo:rust-stack/docs/MACROS.md` |

Two things in that table are designed but not yet built: the macro crates of MACROS.md (`cell-net`, `cell-cap`,
`cell-layer`, `cell-verify`, `consensus.log@1`, `saga.run@1`) and cap v3 verification inside `ce-gate` (MACROS
slice 8). This design uses their vocabulary and says, per place, what slice 1 does until they ship. Slice 1 writes
the small part of `cell-net` it needs in the shape MACROS.md section 9 fixes, so it moves upstream unchanged.

## 1. Rules

1. One cell is one function with one contract crate (`<name>-api`, types only) and one program crate.
2. A caller depends only on the callee's `-api` crate, pinned by commit. It never links the program crate.
3. Every op names the right it needs. A cell holds only the rights listed for it in section 6.
4. Every request carries `Ctx { tenant, trace, chain }`. A cell refuses a request whose chain is not for that tenant.
5. Every write that can be retried carries an idempotency key chosen by the first caller.
6. Secrets are read from `keys` at start and never written to a record, a log or a page.
7. Machines are reached only through typed cell ops (`orch`, `cell.install`, `ce-replica`), never a remote shell.

## 2. Map

```
browser ── shop.web (static bundle, generated TS client)
   │  wire.ws-door
shop.gateway ── session -> cap v3 chain, tenant from host name, routes by contract
   ├── shop.customers      sign-in, addresses
   ├── shop.catalog        products, prices
   ├── shop.search         adapter over search.mesh
   ├── shop.cart           one cart per session or customer
   └── shop.orders         checkout saga
          ├── shop.inventory   x3, leader + quorum log     (reserve, commit, release)
          ├── shop.pay         test-mode payment API        (charge, refund, webhook)
          │      └── shop.pay.sim   the test provider       (calls shop.pay::Webhook)
          ├── shop.receipts    signed receipt, audit.log
          └── shop.notify      mail and push outbox
shop.net  description, plan, apply     shop.rollout  rolling, blue-green, rollback
shop.trace  spans by chain id          shop.chaos    fault injection and the proof
platform: state.store  ce-replica  orch  cell.install  artifact.index  cell.build  keys  audit.log  search.mesh
```

## 3. Type safety end to end

```
crates/<cell>-api/src/lib.rs        ce_wire::api! { types { .. } ops { .. } }      the only source of truth
        │ cargo build                 Rust callers: wrong field, wrong op, wrong right = compile error
        │ ce-wire-gen --lang ts-web   sdk/ts/<api>.ts   (browser client over wire.ws-door)
        │ ce-wire-gen --lang ts-types sdk/ts/<Api>.d.ts (types as serde_json writes them)
        │ ce-wire-gen --lang py|swift sdk/py, sdk/swift (any other language, same hash)
        ▼
shop-web/src/**/*.ts imports only sdk/ts          tsc --noEmit --strict is a build step of shop.web
```

- The browser calls one contract, `shop.gateway@1`. Its request and answer types are the types of the inner
  contracts (`shop-gateway-api` depends on `shop-catalog-api`, `shop-cart-api`, `shop-orders-api`: types only),
  so a field renamed in `Product` changes `sdk/ts/shop_gateway.ts` and every page that reads the old field stops
  compiling.
- `cargo xtask gen` regenerates every client; `cargo xtask check` regenerates into a temp dir and fails on any
  difference from the committed `sdk/`, then runs `tsc --noEmit`. Both run in `cell.build` before `Publish`, so
  an artifact with a stale or broken client is never published and therefore never deployed.
- At run time the schema hash in every frame is compared by the callee; a client built from an older contract is
  refused as drift instead of being misread. `ce-wire-gen freeze` keeps `versions/v<N>.rs` when a contract must
  change while old callers still run (rolling upgrade, section 8.3).
- Records are typed the same way: `#[derive(Record)]` gives the schema; `ce-db-ts` reads it for any typed read
  model the frontend keeps.
- Rights are typed: section 6 is one Rust file (`shop-policy`), built with `cap!` / `cal_file!` when `cell-cap`
  ships and with `ce-net-proto` v3 values directly in slice 1. A grant naming an op that does not exist does not
  compile.
- Proof (slice 1): `cargo xtask break-proof` renames `Product.price` to `Product.unit_price` in a scratch copy,
  runs the build, and passes only when the build fails in `shop-web` with the TypeScript error on that field.

Shared types live in `shop-types` (types only, no logic):

```rust
pub struct TenantId(pub String);   pub struct Sku(pub String);        pub struct CustomerId(pub Ulid);
pub struct OrderId(pub Ulid);      pub struct CheckoutId(pub Ulid);   pub struct PaymentId(pub Ulid);
/// Smallest unit of the currency (1999 = 19.99). Arithmetic is checked; currencies never mix.
pub struct Money { pub amount: i64, pub currency: Currency }          pub enum Currency { Sek, Eur }
pub struct Line { pub sku: Sku, pub qty: u32 }
/// Carried by every request. `chain` is the id (hash of the leaf link) of the cap v3 chain; it is the trace id root.
pub struct Ctx { pub tenant: TenantId, pub trace: TraceId, pub span: u64, pub chain: ChainId }
pub struct Health { pub ok: bool, pub version: String, pub uptime_s: u64 }
```

## 4. Capability model

```
tenant root key (in keys, one per shop)
  └─ shop.gateway@<tenant>        every customer-facing right, caveat tenant == T
       └─ session S               caveats: customer == C, until now+30m, rights for C's own cart and orders
            └─ checkout K         narrowed by the gateway when the customer presses Pay:
                 │                orders::Place where checkout == K, total == M, once, until now+10m
                 └─ charge        narrowed by shop.orders for the payment hop:
                                  pay::Charge where order == O, amount == M, currency == SEK, once, until now+10m
```

- The gateway terminates the browser session (cookie holding an opaque session id) into a chain. Cells behind it
  never see a cookie and never trust an id in a body: they read tenant and customer from the verified chain.
- `shop.orders` has no standing right to charge. It can only narrow the checkout link it was handed, so it can
  charge exactly the amount the customer approved, for that order, one time. `shop.pay` verifies the chain,
  compares `order`, `amount` and `currency` in the caveats with the request, and records the chain id in
  `charges_used`; a second use of the same chain is refused (`AlreadyUsed`) even on another node.
- Standing service rights (orders may reserve stock, receipts may append to audit.log) are grants from the
  tenant root in `shop-policy`, each with `tenant == T` and no customer.
- Until `ce-gate` verifies v3 (MACROS slice 8): `ce-gate` enforces the op right as today, and the three cells that
  need caveats (`shop.gateway`, `shop.orders`, `shop.pay`) verify the v3 chain with `ce-net-proto` in a shared
  `shop-chain` crate. When `layer-auth` ships that crate is deleted and the op attribute carries the condition.

## 5. Cells

Every contract has op 1 `HealthOp(()) -> Health, needs "<cell>:health"`; it is left out below. `C` is `Ctx`.

### 5.1 shop.web (frontend)

Serves the built bundle as a cell; pages follow the ce-net design standard. No business logic.

```rust
api ShopWeb = "shop.web", version 1;
pub struct Asset { pub mime: String, pub etag: String, pub body: Vec<u8>, pub immutable: bool }
pub enum WebError { NotFound, Tenant }
2 => Get(AssetReq { c: C, path: String, if_none_match: Option<String> }) -> Result<Option<Asset>, WebError>, needs "shop.web:read";
3 => Manifest(C) -> BuildInfo { git: String, contracts: Vec<(String, u64)> }, needs "shop.web:read";
```
Rights held: none. Records: none (the bundle is its artifact). `Manifest` lists the schema hash of every
contract the bundle was generated from; rollout verify compares it with the running gateway.

### 5.2 shop.gateway (edge)

The only cell with a public listener. TLS, host name to tenant, session to chain, rate limits, typed routing.

```rust
api ShopGateway = "shop.gateway", version 1;
pub struct Session { pub id: SessionId, pub customer: Option<CustomerId>, pub expires: u64 }
pub enum GwError { Unauthenticated, Denied(String), RateLimited { retry_ms: u32 }, Upstream { cell: String, reason: String }, Unavailable }
2  => Hello(HelloReq { host: String }) -> Result<ShopInfo, GwError>, needs "shop.gateway:public";
3  => SignIn(SignInReq) -> Result<Session, GwError>, needs "shop.gateway:public";
4  => SignOut(SessionId) -> (), needs "shop.gateway:public";
5  => Products(catalog::ListReq) -> Result<catalog::Page, GwError>, needs "shop.gateway:public";
6  => Product(Sku) -> Result<catalog::ProductView, GwError>, needs "shop.gateway:public";
7  => Search(search::Query) -> Result<search::Hits, GwError>, needs "shop.gateway:public";
8  => Cart(()) -> Result<cart::Cart, GwError>, needs "shop.gateway:public";
9  => CartSet(cart::SetLine) -> Result<cart::Cart, GwError>, needs "shop.gateway:public";
10 => Checkout(CheckoutReq { checkout: CheckoutId, pay: pay::TestToken, ship_to: customers::AddressId })
          -> Result<orders::OrderView, GwError>, needs "shop.gateway:public", deadline_ms 20000;
11 => Order(OrderId) -> Result<orders::OrderView, GwError>, needs "shop.gateway:public";
12 => Orders(()) -> Result<Vec<orders::OrderView>, GwError>, needs "shop.gateway:public";
13 => OrderEvents(OrderId) -> stream orders::OrderEvent, needs "shop.gateway:public";
14 => Route(RouteSet { cell: String, color: Color, weight: u8 }) -> Result<RouteTable, GwError>, needs "shop.gateway:route";
```
Rights held: the customer-facing rights of section 6, tenant-bound. Records:
`sessions { #[key] id, tenant, customer, chain, expires }` (lww),
`routes { #[key] cell, blue: Target, green: Option<Target>, weight_green: u8, rev: u64 }` (lww; written only by
`shop.rollout` through `Route`). The gateway is stateless apart from those two and runs on every edge node.

### 5.3 shop.catalog

```rust
api ShopCatalog = "shop.catalog", version 1;
pub struct Product { pub sku: Sku, pub title: String, pub text: String, pub price: Money, pub images: Vec<BlobRef>, pub tags: Vec<String>, pub active: bool, pub rev: u64 }
pub struct ProductView { pub product: Product, pub in_stock: Option<u32> }
pub enum CatalogError { NotFound(Sku), Invalid(String), Stale { have: u64 } }
2 => Get(GetReq { c: C, sku: Sku }) -> Result<Product, CatalogError>, needs "shop.catalog:read";
3 => List(ListReq { c: C, tag: Option<String>, after: Option<Sku>, limit: u16 }) -> Result<Page, CatalogError>, needs "shop.catalog:read";
4 => Price(PriceReq { c: C, lines: Vec<Line> }) -> Result<Priced { lines: Vec<PricedLine>, total: Money }, CatalogError>, needs "shop.catalog:price";
5 => Put(PutReq { c: C, product: Product, expect_rev: u64 }) -> Result<u64, CatalogError>, needs "shop.catalog:write";
6 => Changes(Since { c: C, rev: u64 }) -> stream Product, needs "shop.catalog:read";
```
Rights held: none. Records: `products` (`#[record(cell = "shop.catalog", collection = "products", version = 1)]`,
key `(tenant, sku)`, index `active`), lww, replicas 3. Read-mostly, so last-writer-wins is right here; `expect_rev`
stops a lost update between two editors.

### 5.4 shop.inventory (distributed, no oversell)

Stock is the one place where last-writer-wins is wrong: two nodes each selling the last unit must not both win.
Inventory therefore runs three replicas with one leader and a quorum-acknowledged log.

```rust
api ShopInventory = "shop.inventory", version 1;
pub struct Level { pub sku: Sku, pub on_hand: u32, pub reserved: u32, pub as_of: Seq }
pub struct Reservation { pub order: OrderId, pub lines: Vec<Line>, pub expires: u64, pub seq: Seq }
pub enum InvError { OutOfStock { sku: Sku, available: u32 }, NotLeader { leader: Option<NodeName> }, NoQuorum, Unknown(OrderId), Expired(OrderId) }
2 => Levels(LevelsReq { c: C, skus: Vec<Sku> }) -> Vec<Level>, needs "shop.inventory:read";          // any replica
3 => Reserve(ReserveReq { c: C, order: OrderId, lines: Vec<Line>, ttl_s: u32 }) -> Result<Reservation, InvError>, needs "shop.inventory:reserve";
4 => Commit(OrderRef { c: C, order: OrderId }) -> Result<Seq, InvError>, needs "shop.inventory:reserve";
5 => Release(OrderRef) -> Result<Seq, InvError>, needs "shop.inventory:reserve";
6 => Restock(RestockReq { c: C, sku: Sku, add: u32, key: Ulid }) -> Result<Level, InvError>, needs "shop.inventory:restock";
7 => Role(()) -> RoleInfo { role: Role, epoch: u64, commit: Seq, peers: Vec<PeerState> }, needs "shop.inventory:read";

api ShopInventoryPeer = "shop.inventory.peer", version 1;       // replica to replica only
pub enum Entry { Reserve { order, tenant, lines, expires }, Commit { order }, Release { order }, Restock { tenant, sku, add, key } }
2 => Append(AppendReq { epoch: u64, prev: Seq, entries: Vec<(Seq, Entry)>, commit: Seq }) -> AppendResp { ok: bool, epoch: u64, last: Seq }, needs "shop.inventory.peer:replicate";
3 => Tail(TailReq { from: Seq }) -> TailResp { epoch: u64, entries: Vec<(Seq, u64, Entry)>, commit: Seq }, needs "shop.inventory.peer:replicate";
```

How it stays correct:

- **Leader**: `ce-replica` gives the lease (majority votes, two leaders never overlap in its view). Its leadership
  generation is the `epoch`. Only the leader answers `Reserve`, `Commit`, `Release`, `Restock`; a follower answers
  `NotLeader { leader }` and the caller's client follows it.
- **Log**: the leader appends an entry, sends `Append` to both followers and answers the caller only after a
  majority (2 of 3, itself included) has it on disk. A follower refuses an `Append` whose `epoch` is below the
  highest it has seen, so a leader that lost its lease but has not noticed cannot commit anything: fencing does not
  depend on clocks.
- **New leader**: before serving it calls `Tail` on a majority, adopts the longest log of the highest epoch, and
  re-appends it under its own epoch. An entry acknowledged to a caller is on a majority, so it is in that log.
- **Decision**: the state machine applies entries in order. `Reserve` is accepted only when for every line
  `on_hand - reserved >= qty`. It is idempotent on `order`: the same order asked twice gets the same reservation.
  `Commit` and `Release` are idempotent too. Expired reservations are released by a leader-written `Release`.
- **Invariant** (checked by the chaos proof on every replica): for every sku, `0 <= reserved <= on_hand`, and
  units committed equal units in confirmed orders.
- Without a majority nothing sells (`NoQuorum`); the store stays browsable because `Levels` is served by any
  replica with its `as_of`.

Rights held: `shop.inventory.peer:replicate` (on its own peers only). Records: the log is a local append-only
file per replica (`log/<seq>`), not state.store; `levels` is a derived read model also written to state.store
(lww is fine for a read model written by one leader) so catalog pages can show stock without a call.
When `consensus.log@1` and `layer-consensus` ship (MACROS slice 12) the peer contract and log move there and
this cell keeps only the state machine.

### 5.5 shop.cart

```rust
api ShopCart = "shop.cart", version 1;
pub struct Cart { pub id: CartId, pub lines: Vec<CartLine { sku, qty, title, unit: Money }>, pub total: Money, pub rev: u64 }
pub enum CartError { Unknown, Invalid(String), Frozen(CheckoutId) }
2 => Get(C) -> Result<Cart, CartError>, needs "shop.cart:own";
3 => Set(SetLine { c: C, sku: Sku, qty: u32 }) -> Result<Cart, CartError>, needs "shop.cart:own";      // qty 0 removes
4 => Merge(MergeReq { c: C, from_session: SessionId }) -> Result<Cart, CartError>, needs "shop.cart:own"; // sign-in
5 => Freeze(FreezeReq { c: C, checkout: CheckoutId }) -> Result<Frozen { lines: Vec<Line>, rev: u64 }, CartError>, needs "shop.cart:checkout";
6 => Clear(ClearReq { c: C, checkout: CheckoutId }) -> Result<(), CartError>, needs "shop.cart:checkout";
```
Rights held: `shop.catalog:price`. Records: `carts`, key `(tenant, owner)`, lines stored as a map sku to
`(qty, hlc)` merged per line, so a cart edited on two devices or two nodes converges (lww per line, replicas 3).
`Freeze` is idempotent on `checkout` and makes the lines of a checkout fixed while the saga runs.

### 5.6 shop.orders (saga)

```rust
api ShopOrders = "shop.orders", version 1;
pub enum Stage { Pending, Reserved, Charged, Confirmed, Releasing, Refunding, Failed(FailReason), Cancelled }
pub enum FailReason { OutOfStock(Sku), Declined(String), Timeout, Internal(String) }
pub struct OrderView { pub id: OrderId, pub stage: Stage, pub lines: Vec<PricedLine>, pub total: Money, pub payment: Option<PaymentId>, pub receipt: Option<ReceiptId>, pub at: u64 }
pub enum OrderError { NotFound, Denied, NotLeader { leader: Option<NodeName> }, Busy }
2 => Place(PlaceReq { c: C, checkout: CheckoutId, pay: pay::TestToken, ship_to: AddressId }) -> Result<OrderView, OrderError>, needs "shop.orders:place", deadline_ms 20000;
3 => Get(OrderRef) -> Result<OrderView, OrderError>, needs "shop.orders:own";
4 => List(C) -> Result<Vec<OrderView>, OrderError>, needs "shop.orders:own";
5 => Cancel(OrderRef) -> Result<OrderView, OrderError>, needs "shop.orders:own";
6 => Events(OrderRef) -> stream OrderEvent, needs "shop.orders:own";
7 => PaymentSettled(pay::Settled) -> Result<(), OrderError>, needs "shop.orders:settle";      // from shop.pay
8 => Resume(()) -> ResumeReport { resumed: u32, compensated: u32 }, needs "shop.orders:operate";
```

The saga, one row per step in `saga_steps` written **before** the call it describes:

| # | Step | Call | Idempotent on | Compensation when a later step fails |
|---|---|---|---|---|
| 0 | open | `OrderId = f(tenant, checkout)`; insert `Pending` | checkout | none |
| 1 | freeze | `shop.cart::Freeze`, `shop.catalog::Price` | checkout | none (cart stays) |
| 2 | reserve | `shop.inventory::Reserve` | order | `Release` |
| 3 | charge | `shop.pay::Charge`, chain narrowed to order and amount | order (idempotency key) | `Refund` |
| 4 | commit | `shop.inventory::Commit` | order | `Refund`, then `Release` |
| 5 | confirm | stage `Confirmed`; `shop.receipts::Issue`; `shop.cart::Clear`; `shop.notify::Send` | order | none: these are retried until done |

- `OrderId` is derived from the checkout id, so a browser that retries `Checkout` after a timeout, through any
  gateway and any orders replica, reaches the same order and gets its current view. No second order can exist.
- Orders runs two replicas under a `ce-replica` lease. A new leader runs `Resume`: every order not in a final
  stage is continued from its last written step. Each step is idempotent at the callee, so running a step twice
  is the same as running it once.
- A step that has neither succeeded nor failed within its deadline is asked again (`shop.pay::Status`,
  `shop.inventory::Levels`/`Reserve`) before anything is compensated: the saga never refunds a charge it has not
  seen and never releases stock for a charge that went through.
- Stages form a one-way order (`Pending < Reserved < Charged < Confirmed`, and the failure branch
  `Releasing/Refunding < Failed/Cancelled`). The record merge keeps the later stage, so a stale write from a
  deposed leader cannot move an order backwards under last-writer-wins.
- Final states are exactly two: `Confirmed` with stock committed and one captured payment, or `Failed`/`Cancelled`
  with stock released and nothing captured (or captured and refunded in full).

Rights held: `shop.cart:checkout`, `shop.catalog:price`, `shop.inventory:reserve`, `shop.receipts:issue`,
`shop.notify:send`, `shop.pay:status`, `shop.pay:refund` (own orders); `shop.pay:charge` only through a narrowed
checkout chain. Records: `orders` key `(tenant, id)`, index `customer`, `stage`; `saga_steps` key
`(order, step)` with `{ state: Started|Done|Failed|Compensated, attempt, answer_hash, at }`; replicas 3.

### 5.7 shop.pay (test mode) and shop.pay.sim (the test provider)

```rust
api ShopPay = "shop.pay", version 1;
/// The only mode. There is no other variant: a build of this contract cannot express a live charge.
pub enum Mode { Test }
/// A token of the test provider. Not a card: the contract has no field that can hold a card number.
pub enum TestToken { Ok, Declined, InsufficientFunds, Timeout, SlowWebhook { delay_ms: u32 }, DuplicateWebhook }
pub enum PayState { Pending, Captured, Declined(String), Refunded, Failed(String) }
pub struct Payment { pub id: PaymentId, pub order: OrderId, pub amount: Money, pub state: PayState, pub mode: Mode, pub provider_ref: String, pub at: u64 }
pub struct Settled { pub c: C, pub order: OrderId, pub payment: PaymentId, pub state: PayState }
pub enum PayError { Denied(String), AlreadyUsed, AmountMismatch { chain: Money, asked: Money }, Provider(String), NotFound }
2 => Charge(ChargeReq { c: C, order: OrderId, amount: Money, token: TestToken, key: IdemKey }) -> Result<Payment, PayError>, needs "shop.pay:charge", deadline_ms 15000;
3 => Refund(RefundReq { c: C, order: OrderId, amount: Money, key: IdemKey }) -> Result<Payment, PayError>, needs "shop.pay:refund";
4 => Status(OrderRef) -> Result<Option<Payment>, PayError>, needs "shop.pay:status";
5 => Webhook(WebhookReq { event: EventId, kind: EventKind, provider_ref: String, body: Vec<u8>, sig: [u8; 32] }) -> WebhookAck { applied: bool }, needs "shop.pay:webhook";

api ShopPaySim = "shop.pay.sim", version 1;
2 => Authorize(SimReq { key: IdemKey, amount: Money, token: TestToken, notify: CellRef }) -> Result<SimAuth, SimError>, needs "shop.pay.sim:use";
3 => Reverse(SimReverse { provider_ref: String, key: IdemKey }) -> Result<SimAuth, SimError>, needs "shop.pay.sim:use";
4 => Ledger(()) -> Vec<SimRow>, needs "shop.pay.sim:read";
5 => Fault(SimFault) -> (), needs "shop.pay.sim:fault";          // chaos: drop, delay or repeat the next webhook
```

- **Test mode by construction**: `Mode` has one variant; `TestToken` is the only payment instrument type; the only
  provider client compiled in is `shop-pay-sim-api`. No network address of a card processor, no live key name and
  no card field exists anywhere in the workspace. `shop.pay` refuses to start if `keys` holds anything under
  `shop/pay/live/`. Every receipt and every order page says "test payment".
- **Idempotency**: `key` is derived from the order id by the caller. `idem` maps key to the first answer and a hash
  of the request; the same key with the same request returns the stored answer, with a different request it is
  `Denied`. The row is written (state `Pending`) before the provider is called.
- **Webhook as a typed op**: the provider reports the outcome by calling `Webhook`. The signature is an HMAC over
  the body with the secret `shop/pay/sim/webhook` that both cells read from `keys`. `events_seen` makes delivery
  at-least-once safe: a repeated `event` answers `applied: false`. A captured event calls
  `shop.orders::PaymentSettled`. A `Charge` whose webhook is slow answers `Pending`; the saga waits on `Status`.
- **One charge per order**: besides the idempotency key, the charge chain is single-use (section 4).

Rights held by `shop.pay`: `shop.pay.sim:use`, `shop.orders:settle`, `keys:read` on `shop/pay/sim/*` only. By
`shop.pay.sim`: `shop.pay:webhook`, `keys:read` on `shop/pay/sim/webhook`. Records (`shop.pay`): `payments` key
`(tenant, order)`, `idem` key `key`, `events_seen` key `event`, `charges_used` key `chain`; replicas 3, each a
write-once row, so last-writer-wins cannot lose anything.

### 5.8 shop.customers

```rust
api ShopCustomers = "shop.customers", version 1;
pub struct Customer { pub id: CustomerId, pub email: String, pub name: String, pub created: u64 }
pub struct Address { pub id: AddressId, pub name: String, pub line1: String, pub postcode: String, pub city: String, pub country: String }
pub enum AuthError { BadCredentials, Locked { until: u64 }, Exists, Invalid(String) }
2 => Register(RegisterReq { c: C, email, name, secret: PassphraseProof }) -> Result<Customer, AuthError>, needs "shop.customers:auth";
3 => Verify(VerifyReq { c: C, email, proof: PassphraseProof }) -> Result<Customer, AuthError>, needs "shop.customers:auth";
4 => Me(C) -> Result<Customer, AuthError>, needs "shop.customers:own";
5 => Addresses(C) -> Vec<Address>, needs "shop.customers:own";
6 => AddressPut(AddressPutReq) -> Result<Address, AuthError>, needs "shop.customers:own";
7 => Erase(C) -> Result<(), AuthError>, needs "shop.customers:own";      // right to erasure
```
Rights held: none. Records: `customers` key `(tenant, id)`, unique index `(tenant, email)`; `credentials`
(argon2id hash only); `addresses`. Passkeys reuse `account.passkey` when it is placed on the mesh. Only the
gateway holds `shop.customers:auth`; it turns a successful `Verify` into a session chain.

### 5.9 shop.search

```rust
api ShopSearch = "shop.search", version 1;
pub struct Query { pub c: C, pub text: String, pub tag: Option<String>, pub limit: u16 }
pub struct Hits { pub hits: Vec<Hit { sku, title, price: Money, score: f32 }>, pub took_ms: u32 }
2 => Find(Query) -> Result<Hits, SearchError>, needs "shop.search:find";
3 => Reindex(C) -> Result<u32, SearchError>, needs "shop.search:index";
```
A thin adapter: it follows `shop.catalog::Changes`, registers each product as a source document of
`search.mesh` under the tenant's scope, and answers `Find` with `search.mesh::Local`/`Mesh`, filtered to the
tenant and to active products. Rights held: `shop.catalog:read`, `search.mesh:local`, `search.mesh:find`.

### 5.10 shop.notify

```rust
api ShopNotify = "shop.notify", version 1;
pub enum Template { OrderConfirmed, OrderFailed, Refunded, Shipped }
2 => Send(SendReq { c: C, to: CustomerId, template: Template, order: OrderId, key: IdemKey }) -> Result<NoteId, NotifyError>, needs "shop.notify:send";
3 => Outbox(C) -> Vec<Note>, needs "shop.notify:read";
```
An outbox (`notes` key `(tenant, key)`) drained by channel workers; the same key is sent once. Slice 1 has one
channel, `log`, that writes the rendered text to the outbox so the proof can read it. Mail is a later channel
over the existing mail cells. Rights held: `shop.customers:contact` (email of one customer, nothing else).

### 5.11 shop.receipts (audit and receipts)

```rust
api ShopReceipts = "shop.receipts", version 1;
pub struct Receipt { pub id: ReceiptId, pub order: OrderId, pub seller: Seller, pub lines: Vec<PricedLine>, pub total: Money, pub vat: Money, pub payment: PaymentId, pub mode: pay::Mode, pub chain: ChainId, pub audit_seq: u64, pub sig: Signature }
2 => Issue(IssueReq { c: C, order: orders::OrderView }) -> Result<Receipt, ReceiptError>, needs "shop.receipts:issue";   // idempotent on order
3 => Get(OrderRef) -> Result<Receipt, ReceiptError>, needs "shop.receipts:own";
4 => Check(Receipt) -> CheckReport { signature: bool, in_audit_log: bool }, needs "shop.receipts:public";
```
Signs the receipt with the tenant's receipt key (held in `keys`; the cell asks `keys` to sign, it never reads the
key), appends it to `audit.log::Append`, and stores `audit_seq` so anyone can check it against `audit.log::Verify`.
`Seller` carries the legal name and address of the shop. Rights held: `audit:append`, `keys:sign` on
`shop/<tenant>/receipt`. Every other cell also appends its state changes to `audit.log` with `Ctx.chain`.

### 5.12 Orchestration cells

```rust
api ShopNet = "shop.net", version 1;            // slice 1 home of what becomes net.plan@1 / net.apply@1
2 => Describe(()) -> Network, needs "shop.net:read";
3 => Plan(PlanReq { target: NetworkRef }) -> Result<Plan, NetError>, needs "shop.net:plan";
4 => Apply(ApplyReq { plan: PlanId, chain: ChainId }) -> stream ApplyEvent, needs "shop.net:apply";
5 => Verify(VerifySpec) -> Result<VerifyReport, VerifyError>, needs "shop.net:read";

api ShopRollout = "shop.rollout", version 1;
pub enum Strategy { Rolling { max_unavailable: u8 }, BlueGreen { steps: Vec<u8>, bake_s: u32 } }
pub enum RolloutState { Planned, Running { step: u16 }, Verifying, Done, RollingBack { reason: VerifyError }, RolledBack, Stuck(String) }
2 => Start(RolloutReq { cell: CellName, artifact: Sha256, strategy: Strategy, verify: VerifySpec }) -> Result<RolloutId, RolloutError>, needs "shop.rollout:start";
3 => Status(RolloutId) -> RolloutView, needs "shop.rollout:read";
4 => Abort(RolloutId) -> RolloutView, needs "shop.rollout:start";
5 => Watch(RolloutId) -> stream RolloutEvent, needs "shop.rollout:read";

api ShopTrace = "shop.trace", version 1;
2 => Put(Vec<Span { trace, span, parent, chain, cell, op, node, start_us, dur_us, ok, err }>) -> (), needs "shop.trace:write";
3 => Trace(TraceId) -> Vec<Span>, needs "shop.trace:read";
4 => ByChain(ChainId) -> Vec<Span>, needs "shop.trace:read";
5 => Slow(SlowReq { cell: Option<String>, over_ms: u32, since: u64 }) -> Vec<Span>, needs "shop.trace:read";

api ShopChaos = "shop.chaos", version 1;        // placed only on networks marked `chaos allowed`
pub enum Fault { StopCell { cell: CellName, node: NodeName }, StopNode { node: NodeName }, Delay { cell: CellName, ms: u32 }, DropWebhook { n: u32 }, Heal }
2 => Inject(Fault) -> Result<FaultId, ChaosError>, needs "shop.chaos:inject";
3 => Run(Scenario) -> stream ChaosEvent, needs "shop.chaos:inject";
4 => Report(RunId) -> ProofReport, needs "shop.chaos:read";
```
`shop.net` holds nothing itself: `Apply` runs under the operator's chain, which must carry the right of every
change in the plan (`orch:manage`, `cell.install:install`, `cell.install:upgrade`, `artifact.index:read`).
`shop.rollout` holds `shop.net:plan`, `shop.net:apply`, `shop.gateway:route`. `shop.chaos` holds `orch:manage`
on the nodes of its own network only and `shop.pay.sim:fault`.

## 6. Rights (least privilege)

One file, `crates/shop-policy/src/lib.rs`; one grant per line; all from the tenant root with `tenant == T`.

| Holder | May call | Caveats |
|---|---|---|
| browser session | `shop.gateway:public` | rate limit per session |
| shop.gateway | `shop.customers:auth`, `:own`; `shop.catalog:read`; `shop.search:find`; `shop.cart:own`; `shop.orders:place`, `:own`; `shop.inventory:read`; `shop.web:read` | per call narrowed to `customer == C`; `orders:place` further to `checkout == K, total == M, once` |
| shop.cart | `shop.catalog:price` | |
| shop.search | `shop.catalog:read`, `search.mesh:local`, `search.mesh:find` | scope `shop/<T>` |
| shop.orders | `shop.cart:checkout`, `shop.catalog:price`, `shop.inventory:reserve`, `shop.pay:status`, `shop.pay:refund`, `shop.receipts:issue`, `shop.notify:send` | `refund`: `order` must be one this cell placed, `amount <= captured` |
| shop.orders, per order | `shop.pay:charge` | only by narrowing the checkout chain: `order == O, amount == M, currency == X, once, until +10m` |
| shop.inventory | `shop.inventory.peer:replicate` | callee must be a placed replica of the same tenant set |
| shop.pay | `shop.pay.sim:use`, `shop.orders:settle`, `keys:read` | keys path `shop/pay/sim/*` |
| shop.pay.sim | `shop.pay:webhook`, `keys:read` | keys path `shop/pay/sim/webhook` |
| shop.receipts | `audit:append`, `keys:sign` | keys path `shop/<T>/receipt` |
| shop.notify | `shop.customers:contact` | one customer per call |
| every cell | `audit:append`, `shop.trace:write`, `state.store` on its own cell namespace | |
| shop.rollout | `shop.net:plan`, `shop.net:apply`, `shop.gateway:route` | |
| operator | `shop.rollout:start`, `shop.net:*` | a plan's own grant (`plan.grant(to)`): that plan, once |

What is absent is the point: the gateway cannot charge or touch stock; the catalog, cart, search, notify and
web cells can call nothing that moves money or stock; payments cannot read customers or orders; no cell but
`keys` holds a secret at rest; nobody but a replica can append to the inventory log.

`cargo test -p shop-policy` builds the full caller/callee matrix from the contracts and fails when a cell's
program crate depends on an `-api` crate it has no grant for, or holds a grant it never uses.

## 7. Data

| Record (cell namespace) | Key | Consistency | Replicas | Why |
|---|---|---|---|---|
| `shop.catalog/products` | tenant, sku | lww + `expect_rev` | 3 | read-mostly |
| `shop.inventory` log | seq | leader + quorum log | 3 | no oversell |
| `shop.inventory/levels` | tenant, sku | lww, single writer | 3 | read model |
| `shop.cart/carts` | tenant, owner | lww per line | 3 | converge across devices |
| `shop.orders/orders` | tenant, id | lww, one-way stage merge, single writer | 3 | failover-safe |
| `shop.orders/saga_steps` | order, step | write-once per state | 3 | resume |
| `shop.pay/payments`, `idem`, `events_seen`, `charges_used` | order / key / event / chain | write-once rows | 3 | exactly-once effects |
| `shop.customers/*` | tenant, id | lww, unique email by claim row | 3 | |
| `shop.gateway/sessions`, `routes` | id / cell | lww | all edges | |
| `shop.notify/notes` | tenant, key | write-once + sent flag | 2 | outbox |
| `shop.receipts/receipts` | tenant, order | write-once, also in audit.log | 3 | evidence |
| `shop.trace/spans` | trace, span | append, 7 day expiry | 1 | |

Every record is `#[derive(Record)]` with `#[record(cell = "<cell>", collection = "..", version = N)]`; the tenant is
the first key component in all of them. Rule: last-writer-wins only where a lost concurrent write is harmless
(write-once rows, single-writer rows, per-field merges). Anything that decides (stock) goes through the log.

## 8. Deployment in typed Rust

### 8.1 Description

`crates/shop-net/src/lib.rs`, in the vocabulary of MACROS.md section 9. With `cell-net` shipped it is `net!`;
slice 1 writes the same `Network` value with the plain structs (`NodeSpec`, `CellSpec`, `Placement`).

```rust
net! {
    pub SHOP;
    node ops-1     { labels [linux, edge, eu-north]; }
    node leif-desk { labels [linux, edge, eu-north]; }
    node leif-mac  { labels [macos, asia]; }
    node sim-a | sim-b | sim-c { labels [sim]; chaos allowed; }

    platform { cell state.store@2 replicas 3 spread nodes; cell keys@1; cell audit.log@1; cell search.mesh@1;
               cell orch@1 on all; cell cell.install@1 on all; cell artifact.index@1; cell cell.build@1; }

    tenant umea-outdoor   { host "outdoor.shop.ce-net.com"; currency Sek; }
    tenant kungsgatan-te  { host "te.shop.ce-net.com";      currency Sek; }

    cell shop.gateway@1    on [edge]            rollout blue_green(steps [10, 50, 100], bake 60s);
    cell shop.web@1        on [edge]            rollout blue_green(steps [100], bake 30s);
    cell shop.catalog@1    replicas 2 spread nodes needs [state.store@2];
    cell shop.search@1     replicas 1             needs [shop.catalog@1, search.mesh@1];
    cell shop.cart@1       replicas 2 spread nodes needs [state.store@2, shop.catalog@1];
    cell shop.customers@1  replicas 2 spread nodes needs [state.store@2];
    cell shop.inventory@1  replicas 3 spread nodes leader one consistency quorum_log
                           rollout rolling(max_unavailable 1, followers_first);
    cell shop.orders@1     replicas 2 spread nodes leader one
                           needs [shop.inventory@1, shop.pay@1, shop.cart@1, shop.receipts@1, shop.notify@1];
    cell shop.pay@1        per tenant replicas 2  needs [shop.pay.sim@1, keys@1];      // one instance set per shop
    cell shop.pay.sim@1    replicas 1 on [sim | linux];
    cell shop.receipts@1   per tenant replicas 2  needs [audit.log@1, keys@1];
    cell shop.notify@1     replicas 1;
    cell shop.trace@1      replicas 1;   cell shop.rollout@1 replicas 1;   cell shop.chaos@1 on [sim];

    verify each { healthy; schema; within 20s; }
    verify shop.inventory@1 { replicas >= 3; leaders == 1; }
    verify smoke shop_smoke::checkout;          // a typed test order with TestToken::Ok on a reserved test sku
    policy shop_policy::POLICY;
    build  repo:live-shop@<pinned commit>;
}
```

Checked at compile time (MACROS 9.1): every node and cell is a pinned ref; every `needs` is placed; no cycle;
`replicas` not above the eligible nodes (`shop.inventory` 3 needs three nodes: ops-1, leif-desk, leif-mac in the
full network; sim-a, sim-b, sim-c in slice 1); every `policy` grant names an op that exists. Environments are
values, not files: `SHOP` (full), `SHOP_SLICE1` (ops-1 + three sim nodes), and tests use `#[cell_test(net = SHOP_SLICE1, sim)]`.

### 8.2 Plan and apply

```rust
let plan = SHOP.plan(cx).await?;     // desired vs orch::Ps + cell.install::List + ce-replica placement
println!("{plan}");                  // + install shop.inventory@1 on node:sim-b   needs cell.install:install
                                     // ~ upgrade shop.gateway@1 3f2a.. -> 9c1e.. blue_green   needs cell.install:upgrade, shop.gateway:route
plan.apply(cx, &chain).await?;       // refuses when the chain lacks a listed right; streams ApplyEvent
```
`Plan` is `Vec<Change>` with `Change::{Install, Start, Stop, Move, Upgrade, Grant, Revoke, Route}`, each naming
the op it will call and the right it needs. `apply` is durable (its own step log, like the saga) and orders
changes by `needs`. An empty plan after apply is the definition of converged; `cargo run -p shop-net -- plan`
printing nothing is the check. Other machines are changed only by calling their `orch` and `cell.install` cells.

### 8.3 Rollout and automatic rollback

- **Rolling** (replicated cells): one follower at a time: `Upgrade`, wait `HealthOp`, compare schema hash, wait
  until it has caught up (`Role.commit`), then the next; the leader last, by handing the lease over first. A
  contract change ships as v(N+1) with `versions/v<N>.rs` frozen, so old and new replicas talk during the roll.
- **Blue-green** (gateway, web): green is installed beside blue; `shop.gateway::Route` moves weight 10, 50, 100
  with a bake time at each step; blue stays installed for 10 minutes after 100.
- **Verify** after every step: `healthy`, `schema`, the placement checks, the smoke checkout, and error rate and
  p95 of the new instances from `shop.trace` against the old ones.
- **Rollback**: any `VerifyError` moves the rollout to `RollingBack`: route weight back to blue or `Upgrade` to
  the previous artifact hash (kept from `artifact.index::Resolve` at plan time), in reverse order, then verify
  again. The reason is stored and appended to `audit.log`. A rollback that fails verify is `Stuck` and pages a
  person; nothing is retried blindly.
- Proof (slice 2): publish a gateway build whose `Products` fails, start a blue-green rollout, and show weight
  never passing 10, state `RolledBack`, and zero failed checkouts in the trace of the old color.

### 8.4 Tenants

Two shops on one mesh. Isolation has four layers, each tested alone:
1. **Edge**: host name to tenant; a session chain is rooted in that tenant's key.
2. **Capability**: every grant has `tenant == T`; a cell compares `Ctx.tenant` with the chain before any read.
3. **Data**: tenant is the first key component; multi-tenant cells open records through a `TenantScope` handle
   that cannot be built without a verified chain, so an unscoped read does not compile.
4. **Instances**: money and evidence (`shop.pay`, `shop.receipts`) run per tenant with their own keys.
Proof: with a valid session of `umea-outdoor`, call `Order`, `Cart`, `Receipt` and `Charge` for ids of
`kungsgatan-te` directly at each cell; every call must answer `Denied` and leave an audit row.

### 8.5 Observability

`Ctx.trace` is made by the gateway from the session chain id plus a request counter; each hop adds a span and
sends it to `shop.trace` in batches (when `layer-trace` ships this is the `#[layer(trace)]` line). Because the
chain id is in every span and in every audit row, one question has one answer: "what did this customer's
approval of 499 SEK cause?" is `shop.trace::ByChain(chain)`: gateway, orders, inventory leader and both
followers, pay, pay.sim, webhook, receipts, notify, with nodes and timings. `Slow` and per-cell error rates feed
rollout verify. Health of every placed instance is `shop.net::Verify`.

## 9. Chaos proof

`crates/shop-chaos` is a Rust program (and the `shop.chaos` cell), not a shell script. Scenario
`kill-mid-checkout` on `SHOP_SLICE1`:

1. Seed tenant `umea-outdoor`: sku `tent-2p` with 50 on hand; 5 other skus with plenty.
2. Start 200 buyers, each one checkout of 1 `tent-2p` with `TestToken::Ok` (10 % `Declined`, 5 % `SlowWebhook`,
   5 % `DuplicateWebhook`), each retrying the same `CheckoutId` on timeout or `NotLeader`, through the gateway.
3. At a random moment while at least 20 sagas are between `reserve` and `commit`: stop every cell of the node
   holding the inventory leader (`Fault::StopNode`); 3 s later stop the orders leader's node if it is another one.
4. Wait for failover; after 20 s `Heal`; wait until every buyer has a final answer or 120 s.
5. Read everything back through typed ops and assert:

| # | Assertion |
|---|---|
| A1 | no oversell: confirmed units of `tent-2p` <= 50, and equal to `on_hand` consumed on every inventory replica |
| A2 | no double order: every `CheckoutId` maps to at most one order |
| A3 | no lost order: every buyer got a final answer (`Confirmed` or `Failed`), and it equals what `Order` returns now |
| A4 | money matches: captured minus refunded in `shop.pay.sim::Ledger` == sum of totals of `Confirmed` orders; at most one capture per order |
| A5 | no orphan: no reservation left for an order in a final failed stage; no capture for a non-confirmed order |
| A6 | logs agree: the three inventory logs are identical up to the commit index after heal |
| A7 | one receipt per confirmed order, each passing `shop.receipts::Check`; `audit.log::Verify` is clean |
| A8 | every confirmed order has a complete trace under one chain id |

The run writes `proof/chaos-<utc>.json` (seed, timeline of faults, counts, every assertion with its numbers) and
exits non-zero on any failure. The fault moment is driven by a printed seed, so a failure is replayed exactly.
`cargo run -p shop-chaos -- kill-mid-checkout --runs 20` is the acceptance gate.

## 10. Repository

```
live-shop/
  crates/shop-types  shop-chain  shop-policy                      shared types, chain check, grants
  crates/<cell>-api  crates/<cell>                                one pair per cell of section 5
  crates/shop-net    shop-rollout  shop-trace  shop-chaos  xtask  deployment and proof
  web/               shop.web source (TypeScript, imports only ../sdk/ts)
  sdk/ts  sdk/py  sdk/swift                                      generated, committed, checked for drift
  proof/             chaos and break-proof outputs
  docs/DESIGN.md
```
One workspace while slice 1 is built; each cell pair has its own `cell.toml` and moves to its own repository with
`cell-split` once its contract is stable, which changes nothing for callers because they pin the `-api` crate.

## 11. Slice 1

Goal: on ops-1 plus a local sim of three nodes, a browser buys a tent with a test payment, and the chaos proof
passes 20 runs. A few hours, in this order; every item has its own test and leaves the workspace building.

| # | Build | Done when |
|---|---|---|
| 0 | Sim nodes: `sim-a`, `sim-b`, `sim-c` on ops-1, each its own data dir, door and state.store replica; `orch` reachable on each. Check `ce-replica` exposes the leadership generation to the cell. | `ce-replica ps` shows 3 nodes; a cell started on each answers `HealthOp` |
| 1 | `shop-types`, `shop-chain` (v3 chain verify + narrow with `tenant`, `customer`, `order`, `amount`, `once`), `shop-policy` | unit tests: widen refused, second use refused, wrong tenant refused |
| 2 | `shop-catalog-api` + `shop-catalog` (Get, List, Price, Put) with seed data | contract test + `cell-check conform` |
| 3 | `shop-inventory-api`, `shop-inventory-peer-api`, `shop-inventory`: log, quorum `Append`, `Tail` on election, state machine, 3 replicas under `ce-replica` | in-process test: 3 replicas, 1000 concurrent reserves of 50 units, leader stopped twice, A1 and A6 hold |
| 4 | `shop-cart-api` + `shop-cart` (Get, Set, Freeze, Clear) | per-line merge test across two replicas |
| 5 | `shop-pay-sim-api` + `shop-pay-sim`; `shop-pay-api` + `shop-pay` (Charge, Refund, Status, Webhook; idem, events_seen, charges_used; webhook secret from `keys`) | tests: same key twice = one capture; duplicate webhook = applied once; amount unequal to chain = refused |
| 6 | `shop-orders-api` + `shop-orders`: saga steps 0-5, compensation, `Resume`, 2 replicas | fault-injection test at every step boundary: final state is one of the two legal ones |
| 7 | `shop-gateway-api` + `shop-gateway`: Hello, SignIn (one seeded test customer), Products, Product, Cart, CartSet, Checkout, Order; session to chain; `wire.ws-door` listener | typed end-to-end test from a Rust client |
| 8 | `xtask gen` / `check` / `break-proof`; `sdk/ts`; `web/` with four pages (list, product, cart, checkout + order status) served by `shop-web` | `tsc --noEmit` clean; break-proof fails the build on the renamed field; screenshot of a completed test order |
| 9 | `shop-net`: `Network`, `SHOP_SLICE1`, `plan` (diff against `orch::Ps` + `ce-replica`), `apply` (`orch::Up`, `ce-replica up`), `verify` (healthy, schema, replicas, leaders, smoke) | `plan` lists 9 installs on a clean sim; after `apply`, `plan` is empty and `verify` passes |
| 10 | `shop-chaos`: `kill-mid-checkout`, assertions A1-A6, JSON report | 20 seeded runs pass; one run with quorum acks disabled by a test flag fails A1, proving the check can fail |

Left for slice 2: customers beyond the seeded one, search, notify channels, receipts and A7, trace and A8,
blue-green and rolling rollout with the rollback proof, the second tenant and the isolation proof, the Mac and
leif-desk placement, `py` and `swift` clients, moving to `net!`, `cap!` and the layer crates as they ship.

## 12. Open points

- Taking real payments: not designed here; needs Leif's decision and a separate provider cell with its own review.
- `ce-gate` on cap v3 (MACROS slice 8) removes `shop-chain`; until then three cells verify caveats themselves.
- `consensus.log@1` (MACROS slice 12) should absorb the inventory peer contract; build it there first if that
  slice is started before slice 1 item 3.
- Whether the sim nodes of item 0 are separate door instances on one machine or `net.sim@1`; item 0 decides by
  what works today.
