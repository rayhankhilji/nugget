//! Stable element ids: a short hash of the window's native id and the path of [`IdKey`]s from
//! the window root to the element.
//!
//! The hash is FNV-1a (64-bit) written out here, so ids are stable across builds, platforms and
//! toolchains (`std`'s `DefaultHasher` makes no such promise).

use std::collections::HashMap;

use super::{ElementId, WindowId};
use crate::tree::Node;

/// One step of an element's path: its role, its identifier (or else its name), and its ordinal
/// among earlier siblings with the same role and identifier/name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdKey {
    pub role: String,
    pub label: String,
    pub ordinal: u32,
}

impl IdKey {
    /// The role and label of `node` (sanitised), with ordinal 0.
    pub fn of(node: &Node) -> IdKey {
        let label = node
            .attr("identifier")
            .filter(|s| !s.is_empty())
            .or(node.name.as_deref())
            .unwrap_or("");
        IdKey {
            role: node.role.as_str().to_string(),
            label: label.to_string(),
            ordinal: 0,
        }
    }
}

/// Assigns ordinals to the children of one element, in order.
#[derive(Debug, Default)]
pub struct Ordinals {
    seen: HashMap<(String, String), u32>,
}

impl Ordinals {
    /// The key of the next child `node`.
    pub fn next(&mut self, node: &Node) -> IdKey {
        let mut key = IdKey::of(node);
        let n = self
            .seen
            .entry((key.role.clone(), key.label.clone()))
            .or_insert(0);
        key.ordinal = *n;
        *n += 1;
        key
    }
}

/// FNV-1a, 64-bit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub(crate) fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    /// A length-prefixed string, so adjacent fields cannot run into each other.
    pub(crate) fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.bytes(s.as_bytes());
    }

    pub(crate) fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}

const ID_CHARS: usize = 7;

fn base36(mut h: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = [b'0'; ID_CHARS];
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(h % 36) as usize];
        h /= 36;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The id of the element at `path` in the window `window_native_id`.
pub fn element_id(window_native_id: &str, path: &[IdKey]) -> ElementId {
    let mut h = Fnv::default();
    h.str("element");
    h.str(window_native_id);
    for k in path {
        h.str(&k.role);
        h.str(&k.label);
        h.u64(u64::from(k.ordinal));
    }
    ElementId(format!("e{}", base36(h.finish())))
}

/// The id of the window with the platform handle `native_id`.
pub fn window_id(native_id: &str) -> WindowId {
    let mut h = Fnv::default();
    h.str("window");
    h.str(native_id);
    WindowId(format!("w{}", base36(h.finish())))
}
