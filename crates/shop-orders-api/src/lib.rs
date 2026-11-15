//! `shop.orders@1`. `OrderId` is derived from the checkout id, so a retried checkout reaches the same order.
use shop_types::*;
pub use shop_pay_api::{PayState, TestToken};
shop_wire::api! {
    api ShopOrders = "shop.orders", version 1;
    types {
        #[derive(Eq)]
        pub enum FailReason { OutOfStock(Sku), Declined(String), Timeout, Internal(String) }
        /// One-way: `Pending < Reserved < Charged < Confirmed`, and `Releasing/Refunding < Failed/Cancelled`.
        #[derive(Eq)]
        pub enum Stage { Pending, Reserved, Charged, Confirmed, Releasing, Refunding, Failed(FailReason), Cancelled }
        pub struct OrderView {
            pub id: OrderId,
            pub checkout: CheckoutId,
            pub customer: CustomerId,
            pub stage: Stage,
            pub lines: Vec<PricedLine>,
            pub total: Money,
            pub payment: Option<PaymentId>,
            pub at: u64,
            /// Always true: every payment of this shop is a test payment.
            pub test_payment: bool,
        }
        /// `chain` is the checkout chain (JSON) the gateway narrowed to this checkout, its amount and to
        /// `shop.orders` as holder. Orders narrows it once more for the charge and holds no charge right itself.
        pub struct PlaceReq { pub checkout: CheckoutId, pub pay: TestToken, pub chain: String }
        pub struct OrderGet { pub order: OrderId }
        pub struct Settled { pub order: OrderId, pub payment: PaymentId, pub state: PayState }
        pub struct ResumeReport { pub resumed: u32, pub compensated: u32 }
        pub struct OrdersRole { pub role: String, pub epoch: u64, pub commit: u64, pub last: u64, pub leader: Option<String>, pub node: String }
        pub enum OrderError { NotFound, Denied(String), NotLeader { leader: Option<String> }, Busy, Invalid(String) }
    }
    ops {
        2 => Place(PlaceReq) -> OrderView | OrderError, needs "shop.orders:place", deadline_ms 20000;
        3 => Get(OrderGet) -> OrderView | OrderError, needs "shop.orders:own";
        4 => List(()) -> Vec<OrderView> | OrderError, needs "shop.orders:own";
        6 => PaymentSettled(Settled) -> () | OrderError, needs "shop.orders:settle";
        7 => Resume(()) -> ResumeReport | OrderError, needs "shop.orders:operate";
        8 => All(()) -> Vec<OrderView> | OrderError, needs "shop.orders:operate";
        9 => Role(()) -> OrdersRole | OrderError, needs "shop.orders:operate";
    }
}

impl Stage {
    pub fn is_final(&self) -> bool {
        matches!(self, Stage::Confirmed | Stage::Failed(_) | Stage::Cancelled)
    }
    /// Position in the one-way order; a merge keeps the higher rank.
    pub fn rank(&self) -> u8 {
        match self {
            Stage::Pending => 0,
            Stage::Reserved => 1,
            Stage::Charged => 2,
            Stage::Releasing | Stage::Refunding => 3,
            Stage::Confirmed | Stage::Failed(_) | Stage::Cancelled => 4,
        }
    }
}

pub fn redirect(e: &OrderError) -> shop_wire::Redirect {
    match e {
        OrderError::NotLeader { leader } => shop_wire::Redirect::To(leader.clone()),
        OrderError::Busy => shop_wire::Redirect::To(None),
        _ => shop_wire::Redirect::No,
    }
}
