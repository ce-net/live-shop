//! TypeScript text for every type that crosses a contract, built from the same tokens as the Rust type.
use std::collections::{BTreeMap, HashMap};

/// The TypeScript shape of a type exactly as `serde_json` writes it.
pub trait Ts {
    /// Register the declaration (and its dependencies) and return the name or inline type to use.
    fn ts_ref(reg: &mut Registry) -> String;
}

#[derive(Default)]
pub struct Registry {
    seen: HashMap<String, &'static str>,
    decls: BTreeMap<String, String>,
}

impl Registry {
    /// `true` the first time `name` is seen. Two different Rust types with one TypeScript name is a bug in the
    /// contract and panics, so the generated client can never silently merge them.
    pub fn begin(&mut self, name: &str, rust_path: &'static str) -> bool {
        match self.seen.get(name) {
            Some(p) if *p == rust_path => false,
            Some(p) => panic!("two contract types share the TypeScript name {name}: {p} and {rust_path}"),
            None => {
                self.seen.insert(name.to_string(), rust_path);
                true
            }
        }
    }
    pub fn finish(&mut self, name: &str, decl: String) {
        self.decls.insert(name.to_string(), decl);
    }
    /// Every declaration, sorted by name so the output is stable.
    pub fn text(&self) -> String {
        self.decls.values().cloned().collect::<Vec<_>>().join("\n")
    }
}

macro_rules! prim {
    ($s:literal: $($t:ty),*) => { $( impl Ts for $t { fn ts_ref(_: &mut Registry) -> String { $s.to_string() } } )* };
}
prim!("number": u8, u16, u32, u64, i8, i16, i32, i64, usize);
prim!("boolean": bool);
prim!("string": String);
prim!("null": ());

fn wrap(s: String) -> String {
    if s.contains(' ') { format!("({s})") } else { s }
}
impl<T: Ts> Ts for Vec<T> {
    fn ts_ref(reg: &mut Registry) -> String {
        format!("{}[]", wrap(T::ts_ref(reg)))
    }
}
impl<T: Ts, const N: usize> Ts for [T; N] {
    fn ts_ref(reg: &mut Registry) -> String {
        format!("{}[]", wrap(T::ts_ref(reg)))
    }
}
impl<T: Ts> Ts for Option<T> {
    fn ts_ref(reg: &mut Registry) -> String {
        format!("{} | null", T::ts_ref(reg))
    }
}
impl<T: Ts> Ts for Box<T> {
    fn ts_ref(reg: &mut Registry) -> String {
        T::ts_ref(reg)
    }
}
impl<K, V: Ts> Ts for BTreeMap<K, V> {
    fn ts_ref(reg: &mut Registry) -> String {
        format!("Record<string, {}>", V::ts_ref(reg))
    }
}
impl<A: Ts, B: Ts> Ts for (A, B) {
    fn ts_ref(reg: &mut Registry) -> String {
        format!("[{}, {}]", A::ts_ref(reg), B::ts_ref(reg))
    }
}
impl<A: Ts, B: Ts, C: Ts> Ts for (A, B, C) {
    fn ts_ref(reg: &mut Registry) -> String {
        format!("[{}, {}, {}]", A::ts_ref(reg), B::ts_ref(reg), C::ts_ref(reg))
    }
}
