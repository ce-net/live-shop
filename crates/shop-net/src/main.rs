//! `shop-net plan|apply|verify|down|dir [--net sim|ops1] [--data DIR]`
//!
//! `sim`: every instance as a local process, simulated nodes `sim-a/b/c` with their own data dirs and ports.
//! `ops1`: the same placement as systemd --user units on ops-1, restarted on failure.
use shop_net::*;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("plan");
    let flag = |k: &str, d: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned().unwrap_or_else(|| d.to_string());
    let which = flag("--net", "sim");
    let (net, default_data) = match which.as_str() {
        "ops1" => (shop_slice1("ops1", 21300), format!("{}/.local/share/live-shop/ops1", std::env::var("HOME").unwrap_or_default())),
        _ => (shop_slice1("sim", 21100), "data/sim".to_string()),
    };
    let data = PathBuf::from(flag("--data", &default_data));
    let web = PathBuf::from(flag("--web", "web"));
    let web = std::fs::canonicalize(&web).unwrap_or(web);
    std::fs::create_dir_all(&data).unwrap();
    let data = std::fs::canonicalize(&data).unwrap();
    let procs = Procs::new(bin_dir(), data.clone(), web);
    let mut runner: Box<dyn Runner> = if which == "ops1" { Box::new(Systemd { procs }) } else { Box::new(procs) };
    match cmd {
        "dir" => println!("{}", serde_json::to_string_pretty(&net.directory()).unwrap()),
        "plan" => {
            let p = plan(&net).await;
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "network": net.name, "actions": p.len(), "plan": p })).unwrap());
        }
        "apply" => match apply(&net, &data, runner.as_mut()).await {
            Ok(a) => {
                let op = operator(&net).await;
                let seeded = seed(&net, &op, 50).await;
                let after = plan(&net).await;
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "network": net.name, "applied": a, "seed": seeded.is_ok(), "plan_after": after })).unwrap());
                if which != "ops1" {
                    // local processes belong to this command: keep them until it is stopped
                    eprintln!("running; stop with Ctrl-C (or `shop-net down`)");
                    let _ = tokio::signal::ctrl_c().await;
                    down(&net, runner.as_mut()).await;
                }
            }
            Err(e) => {
                eprintln!("apply failed: {e}");
                std::process::exit(1);
            }
        },
        "verify" => {
            let v = verify(&net).await;
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            std::process::exit(if v.ok { 0 } else { 1 });
        }
        "down" => down(&net, runner.as_mut()).await,
        _ => {
            eprintln!("usage: shop-net plan|apply|verify|down|dir [--net sim|ops1] [--data DIR] [--web DIR]");
            std::process::exit(2);
        }
    }
}
