# Proof

| File | What it shows |
|---|---|
| `chaos-20261115T204411Z.json`, `chaos-20-runs.log` | 20 seeded `kill-mid-checkout` runs, A1-A6 each, with timeline, counts and numbers |
| `chaos-20261115T203603Z-unsafe-no-quorum.json` | the same scenario with quorum acks off: A1 fails, so the check can fail |
| `step-faults-20261115T203458Z.json` | an orders replica stops after each saga step record; every order ends in a legal final state |
| `break-proof.txt` | `Product.title` renamed in a copy: the page (tsc) and the Rust caller both refuse to build |
| `ops1-deploy.txt` | `shop-net` on ops-1: 13 actions planned, 0 after apply, every verify check passing |
| `ops1-*.png` | a test customer buys a tent on the ops-1 deployment (desktop and phone width) |
| `pages-*.png` | the same flow on the read-only page, where the contract runs in the browser |
| `superseded/` | earlier batches, including the one that found the stale-read bug fixed in `shop.orders` |
