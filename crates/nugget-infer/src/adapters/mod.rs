//! Wire-protocol adapters.

pub mod anthropic;
pub mod openai_compatible;
pub mod openai_responses;

/// `data:` URL for an inline image.
pub(crate) fn data_url(media_type: &str, data_base64: &str) -> String {
    format!("data:{media_type};base64,{data_base64}")
}
