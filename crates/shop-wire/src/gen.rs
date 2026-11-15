//! The TypeScript client generator. It reads the same `api!` tokens as the Rust build, through [`crate::Api`].
use crate::{schema_hash, ts::Registry, Api};

/// One TypeScript module for contract `A`: every type, the op table, a typed client class, a `Handlers` type
/// (anything implementing the contract in TypeScript, for example the read-only page mode, must match it) and
/// two transports.
pub fn ts_module<A: Api>(source: &str) -> String {
    let mut reg = Registry::default();
    let ops = A::schema(&mut reg);
    let name = camel(A::NAME);
    let mut s = String::new();
    s.push_str(&format!(
        "// Generated from {source} by `cargo run -p xtask -- gen`. Do not edit: change the contract and regenerate.\n\
         /* eslint-disable */\n\
         export const API = \"{}\";\nexport const VERSION = {};\nexport const SCHEMA_HASH = \"{}\";\n\n\
         /** Every answer: `{{ Ok }}` or the op's own error type in `{{ Err }}`. */\n\
         export type Result<T, E> = {{ Ok: T }} | {{ Err: E }};\n\n",
        A::NAME,
        A::VERSION,
        schema_hash::<A>()
    ));
    s.push_str(&reg.text());
    s.push_str(&format!("\n/** Op table of {}@{}. */\nexport interface {name}Ops {{\n", A::NAME, A::VERSION));
    for o in &ops {
        s.push_str(&format!("  {}: {{ id: {}; right: \"{}\"; req: {}; ok: {}; err: {} }};\n", o.name, o.id, o.right, o.req, o.ok, o.err));
    }
    s.push_str("}\n\nexport type OpName = keyof ");
    s.push_str(&name);
    s.push_str("Ops;\n");
    s.push_str(&format!(
        "\n/** An implementation of the whole contract, op by op. */\n\
         export type Handlers = {{ [K in OpName]: (req: {name}Ops[K][\"req\"]) => Promise<Result<{name}Ops[K][\"ok\"], {name}Ops[K][\"err\"]>> }};\n\n\
         /** Moves one call. `op` and `id` name the op; the answer is the JSON of `Result<ok, err>`. */\n\
         export type Transport = (op: OpName, id: number, req: unknown) => Promise<unknown>;\n\n\
         export class {name}Client {{\n  constructor(private readonly t: Transport) {{}}\n"
    ));
    for o in &ops {
        s.push_str(&format!(
            "  /** needs `{}` */\n  {}(req: {}): Promise<Result<{}, {}>> {{\n    return this.t(\"{}\", {}, req) as Promise<Result<{}, {}>>;\n  }}\n",
            o.right, o.name, o.req, o.ok, o.err, o.name, o.id, o.ok, o.err
        ));
    }
    s.push_str("}\n\n");
    s.push_str(
        "/** Calls the cell over HTTP (`POST <base>/rpc/<Op>`); the schema hash travels with every call. */\n\
         export function httpTransport(base: string, session: () => string | null = () => null): Transport {\n\
         \x20 return async (op, _id, req) => {\n\
         \x20   const headers: Record<string, string> = { \"content-type\": \"application/json\", \"x-shop-schema\": SCHEMA_HASH };\n\
         \x20   const s = session();\n\
         \x20   if (s) headers[\"x-shop-session\"] = s;\n\
         \x20   const r = await fetch(`${base}/rpc/${op}`, { method: \"POST\", headers, body: JSON.stringify(req) });\n\
         \x20   if (!r.ok) throw new Error(`${op}: HTTP ${r.status} ${await r.text()}`);\n\
         \x20   return r.json();\n\
         \x20 };\n}\n\n\
         /** Calls an in-page implementation of the contract. */\n\
         export function localTransport(h: Handlers): Transport {\n\
         \x20 return (op, _id, req) => (h[op] as (r: unknown) => Promise<unknown>)(req);\n}\n",
    );
    s
}

fn camel(api: &str) -> String {
    api.split(['.', '-', '_'])
        .map(|p| {
            let mut c = p.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect()
}
