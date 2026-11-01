//! Probe nugget_proactive::text on stdin. Usage: probe YYYY-MM-DD < body.txt
//! Prints own_text cut point, per-sentence asks, dates, amounts, promises.
use nugget_proactive::text::*;
use std::io::Read;

fn main() {
    let d = std::env::args().nth(1).unwrap_or("2026-10-27".into());
    let r = parse_iso_date(&d).expect("date");
    let mut body = String::new();
    std::io::stdin().read_to_string(&mut body).unwrap();
    let own = own_text(&body);
    let (n_all, n_own) = (body.lines().count(), own.lines().count());
    println!("own_text: {n_own}/{n_all} lines kept; asks_something={}", asks_something(&body));
    for s in sentences(&own) {
        if sentence_asks(&s) {
            println!("  ASK  {s}");
        }
    }
    println!("sentences={} last_own_line={:?}", sentences(&own).len(), own.lines().filter(|l| !l.trim().is_empty()).last());
    println!("find_dates={:?}", find_dates(&body, r));
    println!("find_amounts={:?}", find_amounts(&body).iter().map(|m| m.to_string()).collect::<Vec<_>>());
    println!("promises={:?} deadline={:?}", find_promises(&body), find_promises(&body).first().and_then(|p| deadline_in(p, r)));
}
