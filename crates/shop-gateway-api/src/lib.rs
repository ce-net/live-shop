//! `shop.gateway@1`. The browser calls only this contract. A field renamed in `shop.catalog`'s `Product` changes
//! `sdk/ts/shop_gateway.ts`, and every page that reads the old field stops compiling.
use shop_cart_api::Cart;
use shop_catalog_api::{ListReq, Page, Product};
use shop_orders_api::{OrderView, TestToken};
use shop_types::*;
shop_wire::api! {
    api ShopGateway = "shop.gateway", version 1;
    types {
        pub struct HelloReq { pub host: String }
        pub struct ShopInfo { pub name: String, pub tenant: TenantId, pub address: String, pub test_mode: bool, pub currency: Currency }
        /// Slice 1 signs in test customers only (any `test-…@live.shop` address); no password exists to leak.
        pub struct SignInReq { pub email: String }
        pub struct Session { pub id: SessionId, pub customer: CustomerId, pub email: String, pub expires: u64 }
        pub struct ProductView { pub product: Product, pub in_stock: Option<u32> }
        pub struct CartSetReq { pub session: SessionId, pub sku: Sku, pub qty: u32 }
        pub struct CheckoutReq { pub session: SessionId, pub checkout: CheckoutId, pub pay: TestToken }
        pub struct OrderReq { pub session: SessionId, pub order: OrderId }
        pub enum GwError {
            Unauthenticated,
            Denied(String),
            NotFound,
            RateLimited { retry_ms: u32 },
            Upstream { cell: String, reason: String },
            Unavailable,
        }
    }
    ops {
        2 => Hello(HelloReq) -> ShopInfo | GwError, needs "shop.gateway:public";
        3 => SignIn(SignInReq) -> Session | GwError, needs "shop.gateway:public";
        4 => SignOut(SessionId) -> () | GwError, needs "shop.gateway:public";
        5 => Products(ListReq) -> Page | GwError, needs "shop.gateway:public";
        6 => Product(Sku) -> ProductView | GwError, needs "shop.gateway:public";
        8 => Cart(SessionId) -> Cart | GwError, needs "shop.gateway:public";
        9 => CartSet(CartSetReq) -> Cart | GwError, needs "shop.gateway:public";
        10 => Checkout(CheckoutReq) -> OrderView | GwError, needs "shop.gateway:public", deadline_ms 20000;
        11 => Order(OrderReq) -> OrderView | GwError, needs "shop.gateway:public";
        12 => Orders(SessionId) -> Vec<OrderView> | GwError, needs "shop.gateway:public";
    }
}
