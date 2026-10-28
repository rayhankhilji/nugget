//! Plain text and source code.

use nugget_core::Result;
use serde_json::Value;

use crate::util::{self, LINK_CAP};
use crate::{Ctx, Extracted, mime};

pub(crate) fn extract(data: &[u8], ctx: &Ctx<'_>) -> Result<Extracted> {
    if util::looks_binary(data) {
        return Err(nugget_core::Error::Invalid("text: binary content".into()));
    }
    let text = util::decode_text(data, ctx.charset);
    let mut meta = util::obj();
    let language = ctx
        .path
        .and_then(mime::language_for_path)
        .or_else(|| mime::language_for_mime(ctx.mime));
    if let Some(lang) = language {
        meta.insert("language".into(), Value::from(lang));
    }
    let mut links = Vec::new();
    util::find_urls(&text, |_, url| util::push_unique(&mut links, url, LINK_CAP));
    Ok(Extracted {
        title: None,
        text,
        links,
        metadata: Value::Object(meta),
        ..Default::default()
    })
}
