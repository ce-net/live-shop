//! Typed end to end from a Rust client through `shop.gateway` (needs the cell binaries of the same profile:
//! `cargo build --workspace` first; CI does).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn buy_a_tent_through_the_gateway() {
    let r = shop_chaos::smoke(22100).await.expect("network up");
    assert_eq!(r["ok_order"], "Confirmed", "{r}");
    assert!(r["declined_order"].as_str().unwrap().starts_with("Failed(Declined"), "{r}");
    assert_eq!(r["tent_in_stock_before"], 50);
    assert_eq!(r["tent_in_stock_after"], 49);
    assert_eq!(r["unknown_session"], "Err(Unauthenticated)");
    assert_eq!(r["hello"]["address"], "Kungsgatan 103, 903 31 Umeå, Sweden");
    assert_eq!(r["hello"]["test_mode"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn payment_rules() {
    let r = shop_chaos::pay_rules(22200).await.expect("network up");
    for (k, v) in r.as_object().unwrap() {
        assert_eq!(v, true, "{k}: {r}");
    }
}
