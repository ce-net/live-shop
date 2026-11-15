//! Shared types of cell shop. Every contract crate depends on this one, types only.
shop_wire::types! {
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct TenantId(pub String);
    /// Stock keeping unit, for example `tent-2p`.
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct Sku(pub String);
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct CustomerId(pub String);
    /// Derived from the tenant and the checkout id (see [`order_id`]), so a retried checkout reaches the same order.
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct OrderId(pub String);
    /// Made by the browser when the customer presses Pay; retries reuse it.
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct CheckoutId(pub String);
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct PaymentId(pub String);
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct SessionId(pub String);
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct IdemKey(pub String);
    #[derive(Eq, Hash, PartialOrd, Ord)]
    pub struct NodeName(pub String);

    #[derive(Eq, Hash, Copy)]
    pub enum Currency { Sek, Eur }

    /// Smallest unit of the currency (1999 = 19.99). Arithmetic is checked and currencies never mix.
    #[derive(Eq, Hash)]
    pub struct Money { pub amount: i64, pub currency: Currency }

    #[derive(Eq, Hash)]
    pub struct Line { pub sku: Sku, pub qty: u32 }

    #[derive(Eq)]
    pub struct PricedLine { pub sku: Sku, pub title: String, pub qty: u32, pub unit: Money, pub total: Money }
}

impl Currency {
    pub fn code(self) -> &'static str {
        match self {
            Currency::Sek => "SEK",
            Currency::Eur => "EUR",
        }
    }
}

impl Money {
    pub fn sek(amount: i64) -> Money {
        Money { amount, currency: Currency::Sek }
    }
    pub fn zero(currency: Currency) -> Money {
        Money { amount: 0, currency }
    }
    /// `None` on overflow or when the currencies differ.
    pub fn checked_add(&self, o: &Money) -> Option<Money> {
        (self.currency == o.currency).then_some(())?;
        Some(Money { amount: self.amount.checked_add(o.amount)?, currency: self.currency })
    }
    pub fn checked_mul(&self, n: u32) -> Option<Money> {
        Some(Money { amount: self.amount.checked_mul(n as i64)?, currency: self.currency })
    }
    /// The form capability caveats carry: `(amount, "SEK")`.
    pub fn caveat(&self) -> (i64, String) {
        (self.amount, self.currency.code().to_string())
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:02} {}", self.amount / 100, self.amount.rem_euclid(100), self.currency.code())
    }
}

/// `OrderId = f(tenant, checkout)`: the first 16 bytes of a FNV-1a-128-free stable hash, written as hex.
pub fn order_id(tenant: &TenantId, checkout: &CheckoutId) -> OrderId {
    let mut a: u64 = 0xcbf29ce484222325;
    let mut b: u64 = 0x84222325cbf29ce4;
    for byte in tenant.0.bytes().chain([0u8]).chain(checkout.0.bytes()) {
        a = (a ^ byte as u64).wrapping_mul(0x100000001b3);
        b = (b ^ byte as u64).wrapping_mul(0x1000000001b3).rotate_left(7);
    }
    OrderId(format!("o-{a:016x}{b:08x}").chars().take(26).collect())
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub const TEST_TENANT: &str = "umea-outdoor";

#[cfg(test)]
mod tests {
    use super::*;
    use shop_wire::ts::{Registry, Ts};

    #[test]
    fn money_never_mixes() {
        let a = Money::sek(100);
        assert_eq!(a.checked_add(&Money::sek(50)).unwrap().amount, 150);
        assert!(a.checked_add(&Money { amount: 1, currency: Currency::Eur }).is_none());
        assert!(Money::sek(i64::MAX).checked_mul(2).is_none());
        assert_eq!(Money::sek(49900).to_string(), "499.00 SEK");
    }

    #[test]
    fn order_id_is_stable() {
        let t = TenantId("t".into());
        assert_eq!(order_id(&t, &CheckoutId("k1".into())), order_id(&t, &CheckoutId("k1".into())));
        assert_ne!(order_id(&t, &CheckoutId("k1".into())), order_id(&t, &CheckoutId("k2".into())));
        assert_ne!(order_id(&t, &CheckoutId("k1".into())), order_id(&TenantId("u".into()), &CheckoutId("k1".into())));
    }

    #[test]
    fn ts_text() {
        let mut r = Registry::default();
        assert_eq!(PricedLine::ts_ref(&mut r), "PricedLine");
        let t = r.text();
        assert!(t.contains("export type Currency =\n  | \"Sek\"\n  | \"Eur\";"), "{t}");
        assert!(t.contains("export interface Money {\n  amount: number;\n  currency: Currency;\n}"), "{t}");
        assert!(t.contains("export type Sku = string;"), "{t}");
        // serde writes exactly what the TypeScript says
        assert_eq!(shop_wire::serde_json::to_string(&Money::sek(5)).unwrap(), r#"{"amount":5,"currency":"Sek"}"#);
    }
}
