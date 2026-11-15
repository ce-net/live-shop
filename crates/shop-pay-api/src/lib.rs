//! `shop.pay@1`. Test mode by construction: [`Mode`] has one variant and the only instrument is a test token.
use shop_types::*;
pub use shop_pay_sim_api::TestToken;
shop_wire::api! {
    api ShopPay = "shop.pay", version 1;
    types {
        /// The only mode. There is no other variant: a build of this contract cannot express a live charge.
        #[derive(Eq, Copy)]
        pub enum Mode { Test }
        #[derive(Eq)]
        pub enum PayState { Pending, Captured, Declined(String), Refunded, Failed(String) }
        pub struct Payment { pub id: PaymentId, pub order: OrderId, pub amount: Money, pub state: PayState, pub mode: Mode, pub provider_ref: String, pub at: u64 }
        pub struct ChargeReq { pub order: OrderId, pub amount: Money, pub token: TestToken, pub key: IdemKey }
        pub struct RefundReq { pub order: OrderId, pub amount: Money, pub key: IdemKey }
        pub struct StatusReq { pub order: OrderId }
        /// `body` is the JSON of a `shop_pay_sim_api::SimEvent`; `sig` the hex HMAC-SHA256 of it.
        pub struct WebhookReq { pub event: String, pub body: String, pub sig: String }
        pub struct WebhookAck { pub applied: bool }
        pub enum PayError { Denied(String), AlreadyUsed, AmountMismatch(String), Provider(String), NotFound }
    }
    ops {
        2 => Charge(ChargeReq) -> Payment | PayError, needs "shop.pay:charge", deadline_ms 15000;
        3 => Refund(RefundReq) -> Payment | PayError, needs "shop.pay:refund";
        4 => Status(StatusReq) -> Option<Payment> | PayError, needs "shop.pay:status";
        5 => Webhook(WebhookReq) -> WebhookAck | PayError, needs "shop.pay:webhook";
        6 => Payments(()) -> Vec<Payment> | PayError, needs "shop.pay:read";
    }
}
