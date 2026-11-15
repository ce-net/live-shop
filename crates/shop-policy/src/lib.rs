//! Section 6 of the design as one Rust file. Each right is read from an op type (`O::RIGHT`), so a grant for an op
//! that does not exist, or whose right changed, does not compile or follows the contract.
use shop_cart_api::ops as cart;
use shop_catalog_api::ops as catalog;
use shop_inventory_api::ops as inv;
use shop_inventory_peer_api::ops as inv_peer;
use shop_orders_api::ops as orders;
use shop_orders_peer_api::ops as orders_peer;
use shop_pay_api::ops as pay;
use shop_pay_sim_api::ops as sim;
use shop_wire::Op;

fn r<O: Op>() -> String {
    O::RIGHT.to_string()
}

/// The webhook secret both payment cells read from `shop.keys`.
pub const WEBHOOK_SECRET: &str = "shop/pay/sim/webhook";
/// Anything under this prefix in `shop.keys` makes `shop.pay` refuse to start.
pub const LIVE_PREFIX: &str = "shop/pay/live/";

fn secret(name: &str) -> Vec<String> {
    vec![r::<shop_keys_api::ops::Secret>(), format!("{}/{name}", r::<shop_keys_api::ops::Secret>())]
}

/// Standing rights a cell gets from the tenant root when it enrolls (all with `tenant == T`, no customer).
pub fn grant(cell: &str) -> Option<Vec<String>> {
    let mut v = match cell {
        // customer-facing rights only; it delegates them per session and narrows them per checkout
        "shop.gateway" => vec![
            r::<catalog::Get>(),
            r::<catalog::List>(),
            r::<inv::Levels>(),
            r::<cart::Get>(),
            r::<cart::Freeze>(),
            r::<orders::Place>(),
            r::<orders::Get>(),
            r::<pay::Charge>(),
        ],
        "shop.cart" => vec![r::<catalog::Price>()],
        // no charge right: shop.orders charges only through the checkout chain the customer's session handed it
        "shop.orders" => vec![
            r::<inv::Reserve>(),
            r::<inv::Levels>(),
            r::<pay::Refund>(),
            r::<pay::Status>(),
            r::<orders_peer::Append>(),
        ],
        "shop.pay" => {
            let mut v = vec![r::<sim::Authorize>(), r::<orders::PaymentSettled>()];
            v.extend(secret(WEBHOOK_SECRET));
            v
        }
        "shop.pay.sim" => {
            let mut v = vec![r::<pay::Webhook>()];
            v.extend(secret(WEBHOOK_SECRET));
            v
        }
        "shop.inventory" => vec![r::<inv_peer::Append>()],
        "shop.catalog" | "shop.web" => vec![],
        // the operator: deployment, seeding, verification and the chaos proof
        "shop.ops" => vec![
            r::<inv::Restock>(),
            r::<inv::Levels>(),
            r::<inv_peer::Tail>(),
            r::<orders_peer::Tail>(),
            r::<orders::All>(),
            r::<orders::Resume>(),
            r::<catalog::Put>(),
            r::<catalog::List>(),
            r::<pay::Payments>(),
            r::<sim::Ledger>(),
            r::<sim::Fault>(),
        ],
        _ => return None,
    };
    v.sort();
    v.dedup();
    Some(v)
}

/// What a signed-in customer's session may do (narrowed from the gateway's grant, `customer == C`).
pub fn session_rights() -> Vec<String> {
    vec![r::<catalog::Get>(), r::<cart::Get>(), r::<cart::Freeze>(), r::<orders::Place>(), r::<orders::Get>(), r::<pay::Charge>()]
}

/// The checkout link handed to `shop.orders` (`checkout == K`, `amount == M`, `once`).
pub fn checkout_rights() -> Vec<String> {
    vec![r::<orders::Place>(), r::<cart::Freeze>(), r::<pay::Charge>()]
}

/// The charge link `shop.orders` narrows for `shop.pay` (`order == O`, `amount == M`, `once`).
pub fn charge_rights() -> Vec<String> {
    vec![r::<pay::Charge>()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_cannot_charge_on_its_own() {
        assert!(!grant("shop.orders").unwrap().contains(&r::<pay::Charge>()));
        assert!(checkout_rights().iter().all(|x| session_rights().contains(x)));
        assert!(session_rights().iter().all(|x| grant("shop.gateway").unwrap().contains(x)));
    }

    #[test]
    fn same_right_covers_sibling_ops() {
        // Freeze and Clear share one right, so a checkout chain can unfreeze what it froze
        assert_eq!(r::<cart::Freeze>(), r::<cart::Clear>());
        assert_eq!(r::<cart::Get>(), r::<cart::Quote>());
        assert_eq!(r::<inv::Reserve>(), r::<inv::Commit>());
    }

    #[test]
    fn unknown_cell_gets_nothing() {
        assert!(grant("shop.evil").is_none());
    }
}
