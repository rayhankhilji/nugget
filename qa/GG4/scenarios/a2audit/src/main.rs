//! `gg4-audit HOME [LIMIT]`: the newest audit records of a stopped daemon's store, as JSON.
use nugget_store::{Store, StoreKey};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let home = std::path::PathBuf::from(&a[1]);
    let limit: usize = a.get(2).map_or(200, |s| s.parse().expect("limit"));
    let hex = std::fs::read_to_string(home.join("data/store.key")).expect("key");
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex.trim()[2 * i..2 * i + 2], 16).expect("hex");
    }
    let store = Store::open(home.join("data/nugget.db"), &StoreKey::from_bytes(key)).expect("store");
    let recs = store.recent_audit(limit).expect("audit");
    let ok = store.verify_audit_chain().is_ok();
    println!("{}", serde_json::json!({"chain_ok": ok, "records": recs}));
}
