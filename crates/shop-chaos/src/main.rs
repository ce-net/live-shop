//! `shop-chaos kill-mid-checkout [--runs N] [--seed S] [--buyers 200] [--outage 20] [--unsafe-no-quorum] [--out proof]`
//! `shop-chaos step-faults [--out proof]`
//! `shop-chaos smoke`
//!
//! Writes `proof/chaos-<utc>.json` and exits non-zero when any assertion fails.
use shop_chaos::*;

fn utc() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let (d, t) = (s / 86400, s % 86400);
    // civil-from-days (Howard Hinnant)
    let z = d as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}{m:02}{day:02}T{:02}{:02}{:02}Z", t / 3600, t % 3600 / 60, t % 60)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let flag = |k: &str, d: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| d.to_string());
    let has = |k: &str| args.iter().any(|a| a == k);
    let out = std::path::PathBuf::from(flag("--out", "proof"));
    std::fs::create_dir_all(&out).unwrap();
    match args.get(1).map(String::as_str) {
        Some("kill-mid-checkout") => {
            let runs: u64 = flag("--runs", "1").parse().unwrap();
            let seed: u64 = flag("--seed", "1").parse().unwrap();
            let o = Opts { buyers: flag("--buyers", "200").parse().unwrap(), stock: 50, unsafe_no_quorum: has("--unsafe-no-quorum"), outage_s: flag("--outage", "20").parse().unwrap() };
            let mut reports = Vec::new();
            let mut failed = 0;
            for r in 0..runs {
                let s = seed + r;
                let base = 21500 + (r % 8) as u16 * 40;
                match kill_mid_checkout(s, base, &o).await {
                    Ok(rep) => {
                        let line: Vec<String> = rep.checks.iter().map(|c| format!("{}={}", c.id, if c.ok { "ok" } else { "FAIL" })).collect();
                        eprintln!(
                            "run {r} seed {s}: {} confirmed={} orders={} retried={} {:.0}s {}",
                            if rep.ok { "PASS" } else { "FAIL" },
                            rep.counts["confirmed"],
                            rep.counts["orders"],
                            rep.counts["buyers_retried"],
                            rep.seconds,
                            line.join(" ")
                        );
                        if !rep.ok {
                            failed += 1;
                        }
                        reports.push(rep);
                    }
                    Err(e) => {
                        eprintln!("run {r} seed {s}: ERROR {e}");
                        failed += 1;
                    }
                }
            }
            let summary = serde_json::json!({
                "scenario": "kill-mid-checkout",
                "command": args[1..].join(" "),
                "runs": runs,
                "passed": reports.iter().filter(|r| r.ok).count(),
                "failed": failed,
                "reports": reports,
            });
            let name = format!("chaos-{}{}.json", utc(), if o.unsafe_no_quorum { "-unsafe-no-quorum" } else { "" });
            std::fs::write(out.join(&name), serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
            eprintln!("wrote {}", out.join(&name).display());
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        Some("step-faults") => {
            let rows = step_faults(21900).await.unwrap_or_else(|e| panic!("{e}"));
            let ok = rows.iter().all(|r| r.legal && r.crashes >= 1);
            let name = format!("step-faults-{}.json", utc());
            std::fs::write(out.join(&name), serde_json::to_vec_pretty(&serde_json::json!({ "scenario": "step-faults", "ok": ok, "rows": rows })).unwrap()).unwrap();
            eprintln!("wrote {} ok={ok}", out.join(&name).display());
            std::process::exit(if ok { 0 } else { 1 });
        }
        Some("smoke") => {
            let r = smoke(21950).await.unwrap_or_else(|e| panic!("{e}"));
            println!("{}", serde_json::to_string_pretty(&r).unwrap());
        }
        _ => {
            eprintln!("usage: shop-chaos kill-mid-checkout|step-faults|smoke [flags]");
            std::process::exit(2);
        }
    }
}
