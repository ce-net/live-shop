//! `shop.catalog@1`. Read-mostly product records; `expect_rev` stops a lost update between two editors.
use shop_types::*;
shop_wire::api! {
    api ShopCatalog = "shop.catalog", version 1;
    types {
        pub struct Product {
            pub sku: Sku,
            pub title: String,
            pub text: String,
            pub price: Money,
            /// Path of the product image under the web root.
            pub image: String,
            pub tags: Vec<String>,
            pub active: bool,
            pub rev: u64,
        }
        pub struct GetReq { pub sku: Sku }
        pub struct ListReq { pub tag: Option<String>, pub after: Option<Sku>, pub limit: u16 }
        pub struct Page { pub items: Vec<Product>, pub next: Option<Sku> }
        pub struct PriceReq { pub lines: Vec<Line> }
        pub struct Priced { pub lines: Vec<PricedLine>, pub total: Money }
        pub struct PutReq { pub product: Product, pub expect_rev: u64 }
        pub enum CatalogError { NotFound(Sku), Invalid(String), Stale { have: u64 } }
    }
    ops {
        2 => Get(GetReq) -> Product | CatalogError, needs "shop.catalog:read";
        3 => List(ListReq) -> Page | CatalogError, needs "shop.catalog:read";
        4 => Price(PriceReq) -> Priced | CatalogError, needs "shop.catalog:price";
        5 => Put(PutReq) -> u64 | CatalogError, needs "shop.catalog:write";
    }
}
