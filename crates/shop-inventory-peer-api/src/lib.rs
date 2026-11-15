//! `shop.inventory.peer@1`: replica to replica only.
use shop_types::*;
shop_wire::types! {
    /// One decision of the inventory state machine. Every entry is idempotent on its key.
    pub enum Entry {
        Reserve { tenant: TenantId, order: OrderId, lines: Vec<Line>, expires: u64 },
        Commit { tenant: TenantId, order: OrderId },
        Release { tenant: TenantId, order: OrderId },
        Restock { tenant: TenantId, sku: Sku, add: u32, key: IdemKey },
        /// Release every uncommitted reservation that expired before `now_ms` (written by the leader).
        Expire { now_ms: u64 },
    }
}
shop_replog::peer_api!(ShopInventoryPeer = "shop.inventory.peer", entry Entry, needs "shop.inventory.peer:replicate");
