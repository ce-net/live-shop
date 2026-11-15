//! `shop.inventory@1`. Only the leader answers writes; a follower answers `NotLeader { leader }`.
use shop_types::*;
shop_wire::api! {
    api ShopInventory = "shop.inventory", version 1;
    types {
        pub struct Level { pub sku: Sku, pub on_hand: u32, pub reserved: u32, pub committed: u32, pub as_of: u64 }
        pub struct Reservation { pub order: OrderId, pub lines: Vec<Line>, pub expires: u64, pub seq: u64, pub committed: bool }
        pub enum InvError {
            OutOfStock { sku: Sku, available: u32 },
            NotLeader { leader: Option<String> },
            NoQuorum,
            Unknown(OrderId),
            Expired(OrderId),
        }
        pub struct LevelsReq { pub skus: Vec<Sku> }
        pub struct ReserveReq { pub order: OrderId, pub lines: Vec<Line>, pub ttl_s: u32 }
        pub struct InvOrderRef { pub order: OrderId }
        pub struct RestockReq { pub sku: Sku, pub add: u32, pub key: IdemKey }
        pub struct InvRole { pub role: String, pub epoch: u64, pub commit: u64, pub last: u64, pub leader: Option<String>, pub node: String }
    }
    ops {
        2 => Levels(LevelsReq) -> Vec<Level> | InvError, needs "shop.inventory:read";
        3 => Reserve(ReserveReq) -> Reservation | InvError, needs "shop.inventory:reserve";
        4 => Commit(InvOrderRef) -> u64 | InvError, needs "shop.inventory:reserve";
        5 => Release(InvOrderRef) -> u64 | InvError, needs "shop.inventory:reserve";
        6 => Restock(RestockReq) -> Level | InvError, needs "shop.inventory:restock";
        7 => Role(()) -> InvRole | InvError, needs "shop.inventory:read";
        8 => Reservations(()) -> Vec<Reservation> | InvError, needs "shop.inventory:read";
    }
}

/// Retry policy for `Replicas::call`: follow `NotLeader` and `NoQuorum`, answer everything else.
pub fn redirect(e: &InvError) -> shop_wire::Redirect {
    match e {
        InvError::NotLeader { leader } => shop_wire::Redirect::To(leader.clone()),
        InvError::NoQuorum => shop_wire::Redirect::To(None),
        _ => shop_wire::Redirect::No,
    }
}
