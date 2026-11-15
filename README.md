# cell shop

A store where every part is a cell: catalog, cart, inventory, the order saga, payments, the payment
provider, the gateway, the web page, keys, and the deployment itself. Each cell has its own crate and its
own typed contract (`crates/<cell>-api`). Cells call each other only through those contracts. No cell links
another cell's code. A contract change that breaks a caller fails the build, and that includes the browser
page.

This is a live system on ce-net. **Payments are test mode only.** The payment contract has one mode (`Mode::Test`).
Its only payment instrument is a `TestToken`. No type in the workspace has a field that can hold a card number.

Design: [docs/DESIGN.md](docs/DESIGN.md). This repository is slice 1 of that design (section 11).

Read-only page: <https://ce-net.github.io/live-shop/>. That page runs the `shop.gateway@1` contract inside the
browser, against the same generated types, and sends nothing anywhere. The live page runs on ops-1 (proof below).

## Quickstart (fresh clone)

Needs Rust (stable) and Node 18 or newer.

```sh
git clone https://github.com/ce-net/live-shop && cd live-shop
npm ci --prefix web && npm run build --prefix web     # tsc against sdk/ts, then the bundle
cargo build --workspace --release
target/release/shop-net apply --net sim               # 13 cell instances on ops-1 + sim-a/b/c; Ctrl-C stops them
```

Then open <http://127.0.0.1:21119/>, put a tent in the cart and pay with a test token. In a second terminal:

```sh
target/release/shop-net verify --net sim              # health, schema hash, one leader per replicated cell, smoke
target/release/shop-net plan --net sim                # empty while everything answers with its contract
```

The proofs:

```sh
cargo test --workspace --release                      # unit, in-process replicas, typed end to end
cargo run --release -p xtask -- check                 # generated client matches the contracts; tsc clean
cargo run --release -p xtask -- break-proof           # rename Product.title: the page and the Rust caller must fail
target/release/shop-chaos kill-mid-checkout --runs 20 --seed 1000
target/release/shop-chaos kill-mid-checkout --runs 1 --seed 1 --unsafe-no-quorum   # must FAIL: shows A1 can fail
target/release/shop-chaos step-faults                 # an orders replica stops at every saga step boundary
```

## Cells

| Cell | Crates | Instances | What it decides |
|---|---|---|---|
| `shop.keys` | `shop-keys-api`, `shop-keys` | 1 | tenant root, cell identities and grants, the only place secrets live |
| `shop.catalog` | `shop-catalog-api`, `shop-catalog` | 1 | products and prices |
| `shop.cart` | `shop-cart-api`, `shop-cart` | 1 | carts, merged per line by hybrid logical clock; frozen per checkout |
| `shop.inventory` | `shop-inventory-api`, `shop-inventory-peer-api`, `shop-inventory` | 3 | stock, through a quorum log with epoch fencing (never last-writer-wins) |
| `shop.orders` | `shop-orders-api`, `shop-orders-peer-api`, `shop-orders` | 3 | the checkout saga: freeze, reserve, charge, commit, confirm, with compensation |
| `shop.pay` | `shop-pay-api`, `shop-pay` | 1 | test charges: idempotency keys, single-use charge chains, signed webhooks |
| `shop.pay.sim` | `shop-pay-sim-api`, `shop-pay-sim` | 1 | the test payment provider and its ledger |
| `shop.gateway` | `shop-gateway-api`, `shop-gateway` | 1 | the only public listener; sessions become capability chains |
| `shop.web` | `shop-web`, `web/` | 1 | the page, built against the generated client |

Shared crates: `shop-wire` (the `api!` contract macro, TypeScript generation, schema hash, client and server),
`shop-chain` (capability chains), `shop-types`, `shop-policy` (rights), `shop-replog` (replicated log),
`shop-store`, `shop-cell` (boot). Deployment and proof: `shop-net`, `shop-chaos`, `xtask`. Each cell
crate has a `cell.toml` with its contract, replica count and rights.

## Type safety, end to end

```
crates/<cell>-api/src/lib.rs   shop_wire::api! { api … ; types { … } ops { 2 => Get(GetReq) -> Product | CatalogError, needs "shop.catalog:read"; } }
        │ cargo build          Rust callers name the op type; a wrong field, op or answer does not compile
        │ xtask gen            sdk/ts/shop_gateway.ts: every type, the op table, a client, a Handlers type
        ▼
web/src/*.ts                   imports only sdk/ts; tsc --noEmit --strict runs in CI before the bundle
```

- The browser calls one contract, `shop.gateway@1`. Its types are the inner contracts' types, so the
  TypeScript of `Product` comes from `shop-catalog-api`.
- Every frame carries the caller's schema hash. The hash covers every type reachable from every op, including
  types defined in other crates. The callee refuses a different hash (`Drift`); it does not misread the frame.
  `shop-net plan` restarts any instance that answers with a different hash than the build it deploys.
- `xtask check` regenerates the client and fails on any drift, then runs `tsc`. `xtask break-proof` renames
  `Product.title` to `Product.name` in a copy of the workspace. It passes only if the page and the Rust caller
  both fail to compile ([proof/break-proof.txt](proof/break-proof.txt)).
- The read-only page implements the whole contract in `web/src/sim.ts` as a generated `Handlers`, so it is
  type-checked the same way.

## Rights

Every op names the right it needs. Each server checks the caller's capability chain before the handler runs.

```
tenant root (in shop.keys)
  └─ shop.gateway             customer-facing rights, tenant == umea-outdoor
       └─ session             customer == C, 30 min
            └─ checkout       handed to shop.orders: checkout == K, amount == M, once, 10 min
                 └─ charge    narrowed by shop.orders: order == O, amount == M, once
```

`shop.orders` holds no charge right of its own. It can charge only by narrowing the checkout chain the
customer's session handed it. `shop.pay` checks the order, the amount and `once` against the request, and it
records the chain id. Every grant is written against op types in `shop-policy`, so a grant that names an op
which does not exist does not compile. Chains are Ed25519 and attenuate only. A request carries a proof
signed by the leaf holder over its exact bytes.

## Deployment in Rust

`shop-net` describes the network: the nodes (`ops-1` and the simulated `sim-a`, `sim-b`, `sim-c`, each with
its own data dir and ports), and where each instance runs. `plan` compares that description with what actually
answers op 1 (health, contract and schema hash). `apply` starts what is missing or drifted, `shop.keys`
first, and waits until every instance answers with its expected hash. `verify` checks health, hashes, one
leader per replicated cell and a call through the gateway.

- `--net sim`: local processes. The chaos proof uses the same library.
- `--net ops1`: the same placement as `systemd --user` units on ops-1, restarted on failure.

Proof of the ops-1 deployment: [proof/ops1-deploy.txt](proof/ops1-deploy.txt). Before apply the plan has
13 actions, after apply it has 0, and every verify check passes. A test order placed on it from the page:
[proof/ops1-4-order-confirmed.png](proof/ops1-4-order-confirmed.png) (phone width:
[proof/ops1-5-order-phone.png](proof/ops1-5-order-phone.png)).

## Chaos proof

`shop-chaos kill-mid-checkout` builds a fresh network and gives `tent-2p` 50 units. It then starts 200 buyers
through the gateway. Each buyer makes one checkout of one tent: 80 % approve, 10 % decline, 5 % slow webhook,
5 % duplicate webhook. A buyer retries the same checkout id after any error. While at least 20 units are held
by sagas between reserve and commit, the program stops every cell on the node with the inventory leader. 3 s
later it stops the node with the orders leader, if that is another node. After 20 s it restarts both, then
reads everything back through typed ops:

| # | Assertion |
|---|---|
| A1 | no oversell: confirmed units <= 50, and equal to stock consumed on every inventory replica |
| A2 | no double order: each checkout id maps to at most one order |
| A3 | no lost order: every buyer has a final answer, and it equals what the order says now |
| A4 | money matches: captured minus refunded equals the totals of confirmed orders; at most one capture per order |
| A5 | no orphan: no reservation left for a failed order, no capture kept for an order that is not confirmed |
| A6 | logs agree: the three inventory logs are identical up to the commit index |

Recorded with `--runs 20 --seed 1000` ([proof/chaos-20261115T204411Z.json](proof/chaos-20261115T204411Z.json),
[log](proof/chaos-20-runs.log)):

| | |
|---|---|
| runs | **20 of 20 pass A1-A6** |
| units held by sagas between reserve and commit when the inventory leader's node stopped | 21 to 50 (median 47) |
| runs where the orders leader's node was then stopped too | 19 (in the 20th it was on the same node) |
| orders | 4000 (200 per run), every checkout id exactly one order |
| confirmed | 971, at most 50 per run and equal to the stock consumed on all three replicas |
| failed, sold out / declined / other | 2936 / 93 / 0 |
| checkout calls | 6740; 2740 buyers retried the same checkout id during a failover |
| test money captured | 2 426 529.00 kr, equal to the confirmed totals; no order captured twice |

The check can fail. With `--unsafe-no-quorum` the inventory leader answers before a majority has the entry. That
run fails A1 ([proof/chaos-20261115T203603Z-unsafe-no-quorum.json](proof/chaos-20261115T203603Z-unsafe-no-quorum.json)):
acknowledged stock was lost in the failover, 50 units are gone and 0 were sold.

`shop-chaos step-faults` stops each orders replica once, right after one saga step is written: freeze, reserve,
charge, commit, confirm, and the release that compensates a decline. All 13 cases end in one of the two legal
final states ([proof/step-faults-20261115T203458Z.json](proof/step-faults-20261115T203458Z.json)).

`proof/superseded/` holds earlier batches. One of them found a real bug: a just-restarted orders follower answered
reads with an empty book, and A3 failed. Order reads are now answered only by a leader whose epoch has committed.

## Not done yet (slice 2)

- `shop.customers`, `shop.search`, `shop.notify`, `shop.receipts`, and A7 (receipts) and A8 (traces).
- Blue-green and rolling rollout with the rollback proof; the second tenant and its isolation proof.
- Placement on leif-desk and the Mac; py and swift clients.
- Single-instance cells (catalog, cart, pay, pay.sim, keys, gateway) keep their records in local append-only
  files. The move to `state.store@2` replicas is slice 2. Cart lines already merge per line by HLC
  (`shop-store`).
- Slice 1 has its own small runtime. `shop-wire` stands in for `ce-wire` and `ce-wire-gen`, `shop-chain` for
  cap v3 in `ce-gate`, `shop-replog` for `ce-replica` and `consensus.log`, and `shop-net`'s systemd runner for
  `orch@1`. They keep the same shapes (`api!`, op rights, schema hash, epochs, plan/apply), so the move
  changes the crates underneath and not the contracts.
- `shop.keys` trusts enrollment from the loopback network it listens on. Slice 2 binds enrollment to the
  node identity.
- The browser talks HTTP to the gateway through `shop.web`. A WebSocket listener is slice 2.

Leif Rydenfalk, Kungsgatan 103, 903 31 Umeå, Sweden. Apache-2.0.
