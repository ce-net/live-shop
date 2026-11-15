//! `shop.pay.sim@1`, the test provider. Its only instrument is a [`TestToken`]: there is no card field.
use shop_types::*;
shop_wire::api! {
    api ShopPaySim = "shop.pay.sim", version 1;
    types {
        /// A token of the test provider. Not a card: no field here can hold a card number.
        #[derive(Eq)]
        pub enum TestToken { Ok, Declined, InsufficientFunds, Timeout, SlowWebhook { delay_ms: u32 }, DuplicateWebhook }
        pub struct SimReq { pub key: IdemKey, pub amount: Money, pub token: TestToken }
        #[derive(Eq)]
        pub enum SimState { Captured, Pending, Declined(String), Reversed }
        pub struct SimAuth { pub provider_ref: String, pub state: SimState }
        pub struct SimReverse { pub provider_ref: String, pub key: IdemKey }
        #[derive(Eq, Copy)]
        pub enum SimKind { Capture, Reverse }
        pub struct SimRow { pub provider_ref: String, pub key: IdemKey, pub kind: SimKind, pub amount: Money, pub at: u64 }
        /// Chaos: drop or repeat the next webhooks, or delay them.
        pub struct SimFault { pub drop_next: u32, pub repeat_next: u32, pub delay_ms: u32 }
        pub enum SimError { Unknown(String), Mismatch(String) }
        /// What the provider signs and sends to `shop.pay::Webhook`.
        pub struct SimEvent { pub event: String, pub provider_ref: String, pub key: IdemKey, pub state: SimState, pub amount: Money }
    }
    ops {
        2 => Authorize(SimReq) -> SimAuth | SimError, needs "shop.pay.sim:use";
        3 => Reverse(SimReverse) -> SimAuth | SimError, needs "shop.pay.sim:use";
        4 => Ledger(()) -> Vec<SimRow> | SimError, needs "shop.pay.sim:read";
        5 => Fault(SimFault) -> () | SimError, needs "shop.pay.sim:fault";
    }
}
