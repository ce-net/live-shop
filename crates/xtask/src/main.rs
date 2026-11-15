//! `cargo run -p xtask -- gen|check|break-proof`
//!
//! - `gen` writes `sdk/ts/shop_gateway.ts` from the `shop.gateway@1` contract (and through it every inner
//!   contract type the browser sees).
//! - `check` regenerates in memory and fails on any difference from the committed file, then runs `tsc` on the
//!   page. CI runs it, so a page that does not compile against the current contract never ships.
//! - `break-proof` renames `Product.title` to `Product.name` in a copy of the workspace, regenerates there and
//!   passes only if the page then fails to compile (and the Rust caller too).
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn generated() -> String {
    shop_wire::gen::ts_module::<shop_gateway_api::ShopGateway>("crates/shop-gateway-api (shop.gateway@1)")
}

fn out_file(r: &Path) -> PathBuf {
    r.join("sdk/ts/shop_gateway.ts")
}

fn tsc(web: &Path, r: &Path) -> (bool, String) {
    let tsc = r.join("web/node_modules/typescript/bin/tsc");
    if !tsc.exists() {
        let st = Command::new("npm").args(["ci", "--no-audit", "--no-fund"]).current_dir(r.join("web")).status().expect("npm");
        assert!(st.success(), "npm ci in web/ failed");
    }
    let o = Command::new("node").arg(&tsc).args(["--noEmit", "-p"]).arg(web).output().expect("node");
    (o.status.success(), String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let n = e.file_name();
        if n == "target" || n == "node_modules" || n == "dist" || n == ".git" {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(&n));
        } else {
            std::fs::copy(&p, to.join(&n)).unwrap();
        }
    }
}

fn main() {
    let r = root();
    match std::env::args().nth(1).as_deref() {
        Some("gen") => {
            std::fs::create_dir_all(r.join("sdk/ts")).unwrap();
            std::fs::write(out_file(&r), generated()).unwrap();
            println!("wrote sdk/ts/shop_gateway.ts (schema {})", shop_wire::schema_hash::<shop_gateway_api::ShopGateway>());
        }
        Some("check") => {
            let have = std::fs::read_to_string(out_file(&r)).unwrap_or_default();
            if have != generated() {
                eprintln!("sdk/ts/shop_gateway.ts is stale: run `cargo run -p xtask -- gen` and commit it");
                std::process::exit(1);
            }
            println!("sdk/ts/shop_gateway.ts matches the contracts");
            let (ok, out) = tsc(&r.join("web"), &r);
            if !ok {
                eprintln!("{out}");
                eprintln!("web/ does not compile against the generated client");
                std::process::exit(1);
            }
            println!("tsc --noEmit -p web: clean");
        }
        Some("break-proof") => {
            let tmp = std::env::temp_dir().join(format!("live-shop-break-proof-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            for d in ["crates", "sdk", "web"] {
                copy_dir(&r.join(d), &tmp.join(d));
            }
            for f in ["Cargo.toml", "Cargo.lock"] {
                std::fs::copy(r.join(f), tmp.join(f)).unwrap();
            }
            let api = tmp.join("crates/shop-catalog-api/src/lib.rs");
            let src = std::fs::read_to_string(&api).unwrap();
            let changed = src.replacen("            pub title: String,", "            pub name: String,", 1);
            assert_ne!(src, changed, "the field to rename is where the proof expects it");
            std::fs::write(&api, changed).unwrap();
            // share the release dependencies already built; the copy's xtask gets its own binary name
            let xt = tmp.join("crates/xtask/Cargo.toml");
            std::fs::write(&xt, std::fs::read_to_string(&xt).unwrap().replacen("name = \"xtask\"", "name = \"xtask-break-proof\"", 1)).unwrap();
            let target = r.join("target");
            let g = Command::new("cargo").args(["run", "-q", "--release", "-p", "xtask-break-proof", "--", "gen"]).current_dir(&tmp).env("CARGO_TARGET_DIR", &target).output().unwrap();
            assert!(g.status.success(), "gen in the copy: {}", String::from_utf8_lossy(&g.stderr));
            let diff = Command::new("diff").arg(out_file(&r)).arg(out_file(&tmp)).output().unwrap();
            let (ts_ok, ts_out) = tsc(&tmp.join("web"), &r);
            let rs = Command::new("cargo").args(["check", "-q", "--release", "-p", "shop-catalog"]).current_dir(&tmp).env("CARGO_TARGET_DIR", &target).output().unwrap();
            let rs_err: String = String::from_utf8_lossy(&rs.stderr).lines().filter(|l| l.starts_with("error")).take(6).collect::<Vec<_>>().join("\n");
            let report = format!(
                "break-proof: rename shop.catalog Product.title -> Product.name in a copy of the workspace\n\n\
                 == generated client diff (sdk/ts/shop_gateway.ts)\n{}\n\
                 == tsc --noEmit -p web (must fail)\nexit: {}\n{}\n\
                 == cargo check -p shop-catalog (Rust caller, must fail)\nexit: {}\n{}\n\n\
                 result: {}\n",
                String::from_utf8_lossy(&diff.stdout),
                if ts_ok { "0 (compiled: proof FAILED)" } else { "non-zero (refused)" },
                ts_out.replace(tmp.to_str().unwrap(), "<copy>"),
                if rs.status.success() { "0" } else { "non-zero (refused)" },
                rs_err,
                if !ts_ok && !rs.status.success() { "PASS: a renamed contract field breaks the frontend build and the Rust build" } else { "FAIL" }
            );
            std::fs::create_dir_all(r.join("proof")).unwrap();
            std::fs::write(r.join("proof/break-proof.txt"), &report).unwrap();
            print!("{report}");
            let _ = std::fs::remove_dir_all(&tmp);
            if ts_ok || rs.status.success() {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: cargo run -p xtask -- gen|check|break-proof");
            std::process::exit(2);
        }
    }
}
