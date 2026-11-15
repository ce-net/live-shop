//! `shop.orders.peer@1`: replica to replica only. Every saga step is written here before the call it describes.
use shop_orders_api::{OrderView, Stage, TestToken};
use shop_types::*;
shop_wire::types! {
    #[derive(Eq, Copy)]
    pub enum StepState { Started, Done, Failed, Compensated }
    pub enum Entry {
        Open { tenant: TenantId, view: OrderView, token: TestToken, chain: String },
        Step { order: OrderId, step: u8, state: StepState, note: String },
        Stage { order: OrderId, stage: Stage, lines: Vec<PricedLine>, total: Money, payment: Option<PaymentId> },
    }
}
shop_replog::peer_api!(ShopOrdersPeer = "shop.orders.peer", entry Entry, needs "shop.orders.peer:replicate");
