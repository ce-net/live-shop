//! `shop.keys@1`: hands each cell its identity and its policy grant, and holds the shop's named secrets.
shop_wire::api! {
    api ShopKeys = "shop.keys", version 1;
    types {
        pub struct EnrollReq { pub cell: String, pub instance: String }
        /// `seed` is the cell's identity key (hex); `chain` its grant from `shop-policy` (JSON of a chain).
        pub struct Enrolled { pub seed: String, pub chain: String, pub root: String, pub tenant: String }
        pub struct PubReq { pub cell: String }
        pub struct SecretReq { pub name: String }
        pub enum KeysError { Unknown(String), Refused(String) }
    }
    ops {
        2 => Enroll(EnrollReq) -> Enrolled | KeysError, needs "shop.keys:public";
        3 => Root(()) -> String | KeysError, needs "shop.keys:public";
        4 => PublicKey(PubReq) -> String | KeysError, needs "shop.keys:public";
        5 => Secret(SecretReq) -> String | KeysError, needs "shop.keys:secret";
        6 => Names(()) -> Vec<String> | KeysError, needs "shop.keys:secret";
    }
}
