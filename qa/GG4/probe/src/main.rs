//! `gg4-probe HOME NOW_RFC3339 OFFSET_SECS`: the corpus and the generators' candidates at `NOW`
//! for a stopped daemon's store (HOME/data/nugget.db, key HOME/data/store.key), as JSON.
use nugget_proactive::{Proactive, ProactiveConfig};
use nugget_store::{Store, StoreKey};
use serde_json::json;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let home = std::path::PathBuf::from(&a[1]);
    let now = OffsetDateTime::parse(&a[2], &Rfc3339).expect("now");
    let off = UtcOffset::from_whole_seconds(a[3].parse().expect("offset")).expect("offset");
    let hex = std::fs::read_to_string(home.join("data/store.key")).expect("key");
    let mut key = [0u8; 32];
    for (i, b) in key.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex.trim()[2 * i..2 * i + 2], 16).expect("hex");
    }
    let store = Store::open(home.join("data/nugget.db"), &StoreKey::from_bytes(key)).expect("store");
    let user = a.get(4).cloned().unwrap_or_else(|| "maya@larkspur.example".into());
    let cfg = ProactiveConfig { user_addresses: vec![user], utc_offset: off, ..Default::default() };
    let p = Proactive::new(store, cfg);
    let c = p.corpus().expect("corpus");
    let mail: Vec<_> = c.mail.iter().map(|m| json!({
        "key": m.item.key.as_str(), "subject": m.subject, "date": m.date.format(&Rfc3339).unwrap(),
        "from_user": m.from_user, "thread": m.thread, "auto": m.automated_reason(), "sensitive": m.sensitive,
        "labels": m.labels})).collect();
    let events: Vec<_> = c.events.iter().map(|e| json!({
        "key": e.item.key.as_str(), "title": e.title, "uid": e.uid, "start": e.start.format(&Rfc3339).unwrap(),
        "end": e.end.map(|t| t.format(&Rfc3339).unwrap()), "all_day": e.all_day, "cancelled": e.cancelled,
        "attendees": e.attendees.iter().map(|x| x.address.email.clone()).collect::<Vec<_>>()})).collect();
    let notes: Vec<_> = c.notes.iter().map(|n| json!({"key": n.item.key.as_str(), "title": n.title})).collect();
    let cands = p.candidates(now).expect("candidates");
    let cands: Vec<_> = cands.iter().map(|c| serde_json::to_value(c).unwrap()).collect();
    println!("{}", serde_json::to_string_pretty(&json!({
        "user": c.user_addresses, "internal": c.internal_domains, "mail": mail, "events": events,
        "notes": notes, "candidates": cands})).unwrap());
}
