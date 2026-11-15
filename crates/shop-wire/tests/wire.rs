//! A contract served and called over TCP: typed answers, rights refused, schema drift refused.
use shop_wire::chain::{Caveats, Chain, Holder, Identity};
use shop_wire::{Client, Gate, Server, WireError};
use std::sync::Arc;

mod v1 {
    shop_wire::api! {
        api Echo = "test.echo", version 1;
        types {
            pub struct Say { pub text: String }
            pub enum EchoError { Empty }
        }
        ops {
            2 => Shout(Say) -> String | EchoError, needs "test.echo:use";
            3 => Ping(()) -> u32 | EchoError, needs "test.echo:public";
        }
    }
}
mod v2 {
    // the same contract with one field renamed: another schema hash
    shop_wire::api! {
        api Echo = "test.echo", version 1;
        types {
            pub struct Say { pub words: String }
            pub enum EchoError { Empty }
        }
        ops {
            2 => Shout(Say) -> String | EchoError, needs "test.echo:use";
            3 => Ping(()) -> u32 | EchoError, needs "test.echo:public";
        }
    }
}

#[tokio::test]
async fn typed_calls_rights_and_drift() {
    let root = Identity::generate();
    let me = Identity::generate();
    let chain = Chain::root(&root, &me.public_hex(), vec!["test.echo:use".into()], Caveats { tenant: "t".into(), until: shop_wire::chain::now() + 60, ..Default::default() });
    let holder = Arc::new(Holder { id: me, chain });
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let s = Server::new::<v1::Echo>(Gate { root: root.public_hex() }, "n", "i")
        .on::<v1::ops::Shout, _, _>(|c, r| async move {
            if r.text.is_empty() {
                return Err(v1::EchoError::Empty);
            }
            Ok(format!("{} from {}", r.text.to_uppercase(), c.tenant().unwrap_or("?")))
        })
        .on::<v1::ops::Ping, _, _>(|_, ()| async { Ok(7) });
    tokio::spawn(s.serve(l));

    let c = Client::with(addr.clone(), Some(holder.clone()));
    assert_eq!(c.call::<v1::ops::Shout>(&v1::Say { text: "hej".into() }).await.unwrap(), Ok("HEJ from t".into()));
    assert_eq!(c.call::<v1::ops::Shout>(&v1::Say { text: "".into() }).await.unwrap(), Err(v1::EchoError::Empty));
    // no chain: the gate refuses before the handler runs; a public op needs none
    let anon = Client::new(addr.clone());
    assert!(matches!(anon.call::<v1::ops::Shout>(&v1::Say { text: "x".into() }).await, Err(WireError::Denied(_))));
    assert_eq!(anon.call::<v1::ops::Ping>(&()).await.unwrap(), Ok(7));
    // a caller built from the renamed contract is refused as drift, not misread
    let old = Client::with(addr.clone(), Some(holder));
    assert!(matches!(old.call::<v2::ops::Shout>(&v2::Say { words: "x".into() }).await, Err(WireError::Drift { .. })));
    assert_ne!(shop_wire::schema_hash::<v1::Echo>(), shop_wire::schema_hash::<v2::Echo>());
    // the generated TypeScript names the field
    let ts = shop_wire::gen::ts_module::<v1::Echo>("test");
    assert!(ts.contains("export interface Say {\n  text: string;\n}"), "{ts}");
    assert!(ts.contains("Shout(req: Say): Promise<Result<string, EchoError>>"), "{ts}");
}
