//! `shop.cart@1`. The customer is read from the verified chain, never from the request.
use shop_types::*;
shop_wire::api! {
    api ShopCart = "shop.cart", version 1;
    types {
        pub struct CartLine { pub sku: Sku, pub title: String, pub qty: u32, pub unit: Money, pub total: Money }
        pub struct Cart { pub owner: CustomerId, pub lines: Vec<CartLine>, pub total: Money, pub rev: u64, pub frozen: Option<CheckoutId> }
        /// `qty` 0 removes the line.
        pub struct SetLine { pub sku: Sku, pub qty: u32 }
        pub struct FreezeReq { pub checkout: CheckoutId }
        pub struct Frozen { pub checkout: CheckoutId, pub lines: Vec<PricedLine>, pub total: Money }
        /// `empty`: the order was confirmed, empty the cart. Otherwise only unfreeze it.
        pub struct ClearReq { pub checkout: CheckoutId, pub empty: bool }
        pub struct QuoteReq { pub checkout: CheckoutId }
        pub enum CartError { Unknown, Invalid(String), Frozen(CheckoutId), Upstream(String) }
    }
    ops {
        2 => Get(()) -> Cart | CartError, needs "shop.cart:own";
        3 => Set(SetLine) -> Cart | CartError, needs "shop.cart:own";
        4 => Freeze(FreezeReq) -> Frozen | CartError, needs "shop.cart:checkout";
        5 => Clear(ClearReq) -> () | CartError, needs "shop.cart:checkout";
        /// What a checkout of the cart costs now (or what it cost, if that checkout already froze it).
        6 => Quote(QuoteReq) -> Frozen | CartError, needs "shop.cart:own";
    }
}
