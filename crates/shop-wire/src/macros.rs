/// Shared types without ops (for example `shop-types`). Same item syntax as `api! { types { .. } }`.
#[macro_export]
macro_rules! types {
    ($($body:tt)*) => { $crate::__items!{ $($body)* } };
}

/// A cell's contract: its name and version, the types that cross it and its ops with the right each needs.
///
/// ```ignore
/// shop_wire::api! {
///     api ShopCatalog = "shop.catalog", version 1;
///     types { pub struct GetReq { pub sku: Sku } }
///     ops { 2 => Get(GetReq) -> Product | CatalogError, needs "shop.catalog:read"; }
/// }
/// ```
/// Each op becomes a marker type `ops::<Op>` implementing [`crate::Op`]; Rust callers name the op and the
/// compiler checks the request and both answers. The same tokens produce the TypeScript client.
#[macro_export]
macro_rules! api {
    (
        api $api:ident = $name:literal, version $ver:literal;
        types { $($types:tt)* }
        ops { $( $(#[$om:meta])* $id:literal => $op:ident ( $req:ty ) -> $ok:ty | $err:ty , needs $right:literal $(, deadline_ms $dl:literal)? ; )* }
    ) => {
        $crate::__items!{ $($types)* }

        #[derive(Debug, Clone, Copy)]
        pub struct $api;

        impl $crate::Api for $api {
            const NAME: &'static str = $name;
            const VERSION: u32 = $ver;
            fn ops() -> Vec<$crate::OpMeta> {
                vec![ $( $crate::OpMeta { id: $id, name: stringify!($op), right: $right, deadline_ms: $crate::__dl!($($dl)?) } ),* ]
            }
            fn schema(reg: &mut $crate::ts::Registry) -> Vec<$crate::OpTs> {
                vec![ $( $crate::OpTs {
                    id: $id, name: stringify!($op), right: $right,
                    req: <$req as $crate::ts::Ts>::ts_ref(reg),
                    ok: <$ok as $crate::ts::Ts>::ts_ref(reg),
                    err: <$err as $crate::ts::Ts>::ts_ref(reg),
                } ),* ]
            }
        }

        /// One marker type per op of this contract.
        pub mod ops {
            $( $(#[$om])* #[derive(Debug, Clone, Copy)] pub struct $op; )*
        }
        $(
            impl $crate::Op for ops::$op {
                type Api = $api;
                const ID: u16 = $id;
                const NAME: &'static str = stringify!($op);
                const RIGHT: &'static str = $right;
                const DEADLINE_MS: u64 = $crate::__dl!($($dl)?);
                type Req = $req;
                type Ok = $ok;
                type Err = $err;
            }
        )*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __dl {
    () => { 10_000 };
    ($d:literal) => { $d };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __items {
    () => {};
    (
        $(#[$m:meta])*
        pub struct $name:ident { $( $(#[$fm:meta])* pub $f:ident : $ft:ty ),* $(,)? }
        $($rest:tt)*
    ) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, $crate::serde::Serialize, $crate::serde::Deserialize)]
        #[serde(crate = "shop_wire::serde")]
        pub struct $name { $( $(#[$fm])* pub $f : $ft ),* }
        impl $crate::ts::Ts for $name {
            fn ts_ref(reg: &mut $crate::ts::Registry) -> String {
                let n = stringify!($name).to_string();
                if reg.begin(&n, concat!(module_path!(), "::", stringify!($name))) {
                    #[allow(unused_mut)]
                    let mut s = format!("export interface {} {{\n", n);
                    $( { let t = <$ft as $crate::ts::Ts>::ts_ref(reg); s.push_str(&format!("  {}: {};\n", stringify!($f), t)); } )*
                    s.push_str("}\n");
                    reg.finish(&n, s);
                }
                n
            }
        }
        $crate::__items!{ $($rest)* }
    };
    (
        $(#[$m:meta])*
        pub struct $name:ident ( pub $t:ty ) ;
        $($rest:tt)*
    ) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, $crate::serde::Serialize, $crate::serde::Deserialize)]
        #[serde(crate = "shop_wire::serde")]
        pub struct $name(pub $t);
        impl $crate::ts::Ts for $name {
            fn ts_ref(reg: &mut $crate::ts::Registry) -> String {
                let n = stringify!($name).to_string();
                if reg.begin(&n, concat!(module_path!(), "::", stringify!($name))) {
                    let t = <$t as $crate::ts::Ts>::ts_ref(reg);
                    reg.finish(&n, format!("export type {} = {};\n", n, t));
                }
                n
            }
        }
        $crate::__items!{ $($rest)* }
    };
    (
        $(#[$m:meta])*
        pub enum $name:ident { $( $(#[$vm:meta])* $v:ident $( ( $vt:ty ) )? $( { $( $vf:ident : $vft:ty ),* $(,)? } )? ),* $(,)? }
        $($rest:tt)*
    ) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, $crate::serde::Serialize, $crate::serde::Deserialize)]
        #[serde(crate = "shop_wire::serde")]
        pub enum $name { $( $(#[$vm])* $v $( ($vt) )? $( { $( $vf : $vft ),* } )? ),* }
        impl $crate::ts::Ts for $name {
            fn ts_ref(reg: &mut $crate::ts::Registry) -> String {
                let n = stringify!($name).to_string();
                if reg.begin(&n, concat!(module_path!(), "::", stringify!($name))) {
                    let vs: Vec<String> = vec![ $( $crate::__variant!(reg, $v $( ($vt) )? $( { $( $vf : $vft ),* } )?) ),* ];
                    reg.finish(&n, format!("export type {} =\n  | {};\n", n, vs.join("\n  | ")));
                }
                n
            }
        }
        $crate::__items!{ $($rest)* }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __variant {
    ($reg:ident, $v:ident) => { format!("\"{}\"", stringify!($v)) };
    ($reg:ident, $v:ident ($t:ty)) => { format!("{{ {}: {} }}", stringify!($v), <$t as $crate::ts::Ts>::ts_ref($reg)) };
    ($reg:ident, $v:ident { $( $f:ident : $ft:ty ),* }) => {{
        #[allow(unused_mut)]
        let mut fs: Vec<String> = Vec::new();
        $( { let t = <$ft as $crate::ts::Ts>::ts_ref($reg); fs.push(format!("{}: {}", stringify!($f), t)); } )*
        format!("{{ {}: {{ {} }} }}", stringify!($v), fs.join("; "))
    }};
}
