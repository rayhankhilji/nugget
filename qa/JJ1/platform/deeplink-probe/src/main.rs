//! Prints how deeplink.rs's validation treats each line of stdin (one raw argument per line;
//! `\uXXXX` escapes are decoded so bidi and control characters can be written in the case file).
#[allow(dead_code, clippy::all)]
mod dl {
    include!(concat!(env!("OUT_DIR"), "/deeplink_pure.rs"));
}
use std::io::BufRead;

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' && it.peek() == Some(&'u') {
            it.next();
            let hex: String = (0..4).filter_map(|_| it.next()).collect();
            out.push(char::from_u32(u32::from_str_radix(&hex, 16).unwrap()).unwrap());
        } else {
            out.push(c);
        }
    }
    out
}

fn main() {
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.is_empty() || line.starts_with('#') { continue; }
        let (label, raw) = line.split_once('\t').unwrap_or(("", &line));
        let mut raw = unescape(raw);
        if let Some(n) = raw.strip_prefix("LONGQ:") { raw = format!("nugget://search?q={}", "a".repeat(n.parse().unwrap())); }
        let r = dl::parse_arg(&raw);
        let shown = match &r { None => "IGNORED".to_string(), Some(Ok(l)) => format!("OK {l:?}"), Some(Err(e)) => format!("REFUSED {e}") };
        println!("{label}\t{}\t{}", raw.escape_debug().to_string().chars().take(120).collect::<String>(), shown.escape_debug());
    }
}
