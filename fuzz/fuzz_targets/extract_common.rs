//! Shared by the `mail` and `calendar` targets: run one extractor and check its output contract.

use nugget_extract::{ChunkOptions, ExtractOptions, Source, chunk, extract};

pub fn run(mime: &str, data: &[u8]) {
    let opts = ExtractOptions {
        max_input_bytes: 1024 * 1024,
        max_text_chars: 20_000,
        ocr: false,
    };
    // Every error is fine; panics are not. libfuzzer-sys's panic hook aborts before the
    // extractor's `catch_unwind` can turn a panic into `Error::Invalid`.
    let Ok(e) = extract(mime, Source::Bytes(data), &opts) else {
        return;
    };
    assert!(e.text.chars().count() <= opts.max_text_chars);
    assert!(e.metadata.is_object());
    for s in &e.sections {
        assert!(s.start < s.end && s.end <= e.text.len());
        assert!(e.text.is_char_boundary(s.start) && e.text.is_char_boundary(s.end));
    }
    let chunk_opts = ChunkOptions {
        target_tokens: 24,
        max_tokens: 32,
        overlap_tokens: 6,
    };
    let _ = chunk(&e, &chunk_opts);
}
