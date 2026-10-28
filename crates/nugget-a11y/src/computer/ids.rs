//! Stable element ids: a short hash of the window's native id and the path of [`IdKey`]s from
//! the window root to the element.

use super::ElementId;

/// One step of an element's path: its role, its identifier (or else its name), and its ordinal
/// among earlier siblings with the same role and identifier/name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdKey {
    pub role: String,
    pub label: String,
    pub ordinal: u32,
}

/// The id of the element at `path` in the window `window_native_id`.
pub fn element_id(window_native_id: &str, path: &[IdKey]) -> ElementId {
    let _ = (window_native_id, path);
    todo!("W2-A")
}
