//! MIME detection (magic bytes, extensions, content sniffing) and language tables.

use std::path::Path;

pub(crate) const DOCX: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
pub(crate) const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
pub(crate) const XLSM: &str = "application/vnd.ms-excel.sheet.macroenabled.12";

/// Lower-cased MIME type without parameters.
pub(crate) fn base(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// A MIME parameter value (`charset` in `text/plain; charset="utf-8"`).
pub(crate) fn param(mime: &str, name: &str) -> Option<String> {
    mime.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().trim_matches('"').to_owned())
    })
}

/// Source-code and data-file extensions: (extension, language, MIME).
const LANGS: &[(&str, &str, &str)] = &[
    ("rs", "rust", "text/x-rust"),
    ("py", "python", "text/x-python"),
    ("pyi", "python", "text/x-python"),
    ("js", "javascript", "text/javascript"),
    ("mjs", "javascript", "text/javascript"),
    ("cjs", "javascript", "text/javascript"),
    ("jsx", "jsx", "text/x-jsx"),
    ("ts", "typescript", "text/x-typescript"),
    ("mts", "typescript", "text/x-typescript"),
    ("cts", "typescript", "text/x-typescript"),
    ("tsx", "tsx", "text/x-tsx"),
    ("go", "go", "text/x-go"),
    ("java", "java", "text/x-java"),
    ("kt", "kotlin", "text/x-kotlin"),
    ("kts", "kotlin", "text/x-kotlin"),
    ("scala", "scala", "text/x-scala"),
    ("swift", "swift", "text/x-swift"),
    ("c", "c", "text/x-c"),
    ("h", "c", "text/x-c"),
    ("cc", "cpp", "text/x-c++"),
    ("cpp", "cpp", "text/x-c++"),
    ("cxx", "cpp", "text/x-c++"),
    ("hpp", "cpp", "text/x-c++"),
    ("hh", "cpp", "text/x-c++"),
    ("hxx", "cpp", "text/x-c++"),
    ("m", "objective-c", "text/x-objective-c"),
    ("mm", "objective-cpp", "text/x-objective-c++"),
    ("cs", "csharp", "text/x-csharp"),
    ("fs", "fsharp", "text/x-fsharp"),
    ("vb", "vbnet", "text/x-vb"),
    ("rb", "ruby", "text/x-ruby"),
    ("php", "php", "text/x-php"),
    ("pl", "perl", "text/x-perl"),
    ("pm", "perl", "text/x-perl"),
    ("lua", "lua", "text/x-lua"),
    ("r", "r", "text/x-r"),
    ("jl", "julia", "text/x-julia"),
    ("hs", "haskell", "text/x-haskell"),
    ("ml", "ocaml", "text/x-ocaml"),
    ("mli", "ocaml", "text/x-ocaml"),
    ("ex", "elixir", "text/x-elixir"),
    ("exs", "elixir", "text/x-elixir"),
    ("erl", "erlang", "text/x-erlang"),
    ("clj", "clojure", "text/x-clojure"),
    ("cljs", "clojure", "text/x-clojure"),
    ("el", "elisp", "text/x-elisp"),
    ("lisp", "lisp", "text/x-lisp"),
    ("dart", "dart", "text/x-dart"),
    ("zig", "zig", "text/x-zig"),
    ("nim", "nim", "text/x-nim"),
    ("v", "verilog", "text/x-verilog"),
    ("sol", "solidity", "text/x-solidity"),
    ("f90", "fortran", "text/x-fortran"),
    ("sh", "shell", "text/x-shellscript"),
    ("bash", "shell", "text/x-shellscript"),
    ("zsh", "shell", "text/x-shellscript"),
    ("fish", "fish", "text/x-fish"),
    ("ps1", "powershell", "text/x-powershell"),
    ("bat", "batch", "text/x-bat"),
    ("sql", "sql", "text/x-sql"),
    ("graphql", "graphql", "text/x-graphql"),
    ("proto", "protobuf", "text/x-protobuf"),
    ("css", "css", "text/css"),
    ("scss", "scss", "text/x-scss"),
    ("sass", "sass", "text/x-sass"),
    ("less", "less", "text/x-less"),
    ("vue", "vue", "text/x-vue"),
    ("svelte", "svelte", "text/x-svelte"),
    ("json", "json", "application/json"),
    ("jsonc", "json", "application/json"),
    ("yaml", "yaml", "application/yaml"),
    ("yml", "yaml", "application/yaml"),
    ("toml", "toml", "application/toml"),
    ("xml", "xml", "application/xml"),
    ("ini", "ini", "text/x-ini"),
    ("cfg", "ini", "text/x-ini"),
    ("tex", "latex", "text/x-tex"),
    ("nix", "nix", "text/x-nix"),
    ("tf", "hcl", "text/x-hcl"),
    ("cmake", "cmake", "text/x-cmake"),
    ("gradle", "groovy", "text/x-groovy"),
    ("groovy", "groovy", "text/x-groovy"),
    ("diff", "diff", "text/x-diff"),
    ("patch", "diff", "text/x-diff"),
];

/// Well-known extension-less file names.
const NAMED: &[(&str, &str, &str)] = &[
    ("dockerfile", "dockerfile", "text/x-dockerfile"),
    ("containerfile", "dockerfile", "text/x-dockerfile"),
    ("makefile", "make", "text/x-makefile"),
    ("gnumakefile", "make", "text/x-makefile"),
    ("justfile", "just", "text/x-just"),
    ("rakefile", "ruby", "text/x-ruby"),
    ("gemfile", "ruby", "text/x-ruby"),
    ("cmakelists.txt", "cmake", "text/x-cmake"),
];

/// Other MIME types that are text with a language.
const MIME_LANGS: &[(&str, &str)] = &[
    ("application/javascript", "javascript"),
    ("application/x-javascript", "javascript"),
    ("application/typescript", "typescript"),
    ("application/x-yaml", "yaml"),
    ("text/yaml", "yaml"),
    ("text/x-yaml", "yaml"),
    ("text/xml", "xml"),
    ("application/x-sh", "shell"),
    ("application/sql", "sql"),
    ("application/x-toml", "toml"),
    ("text/x-sh", "shell"),
    ("text/x-script.python", "python"),
];

fn file_name_lower(path: &Path) -> Option<String> {
    path.file_name()?.to_str().map(str::to_ascii_lowercase)
}

fn ext_lower(path: &Path) -> Option<String> {
    path.extension()?.to_str().map(str::to_ascii_lowercase)
}

/// Programming/data language of a file, from its name or extension.
pub(crate) fn language_for_path(path: &Path) -> Option<&'static str> {
    if let Some(name) = file_name_lower(path)
        && let Some((_, lang, _)) = NAMED.iter().find(|(n, _, _)| *n == name)
    {
        return Some(lang);
    }
    let ext = ext_lower(path)?;
    LANGS.iter().find(|(e, _, _)| *e == ext).map(|(_, l, _)| *l)
}

/// Language implied by a MIME type (`text/x-rust` → `rust`).
pub(crate) fn language_for_mime(mime: &str) -> Option<&'static str> {
    LANGS
        .iter()
        .chain(NAMED)
        .find(|(_, _, m)| *m == mime)
        .map(|(_, l, _)| *l)
        .or_else(|| MIME_LANGS.iter().find(|(m, _)| *m == mime).map(|(_, l)| *l))
}

fn mime_for_path(path: &Path) -> Option<&'static str> {
    if let Some(name) = file_name_lower(path)
        && let Some((_, _, mime)) = NAMED.iter().find(|(n, _, _)| *n == name)
    {
        return Some(mime);
    }
    let ext = ext_lower(path)?;
    Some(match ext.as_str() {
        "md" | "markdown" | "mdown" | "mkd" | "mdx" => "text/markdown",
        "txt" | "text" | "log" | "rst" | "adoc" | "org" => "text/plain",
        "html" | "htm" | "xhtml" | "shtml" => "text/html",
        "csv" => "text/csv",
        "tsv" | "tab" => "text/tab-separated-values",
        "eml" | "mht" => "message/rfc822",
        "ics" | "ical" | "ifb" => "text/calendar",
        "pdf" => "application/pdf",
        "docx" => DOCX,
        "xlsx" => XLSX,
        "xlsm" => XLSM,
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "zip" => "application/zip",
        _ => return LANGS.iter().find(|(e, _, _)| *e == ext).map(|(_, _, m)| *m),
    })
}

/// Formats identified by their leading bytes. These win over the extension.
fn magic(path: Option<&Path>, head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if head.starts_with(b"\xFF\xD8\xFF") {
        return Some("image/jpeg");
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.starts_with(b"PK\x03\x04") {
        let by_ext = path.and_then(mime_for_path);
        if let Some(m @ (DOCX | XLSX | XLSM)) = by_ext {
            return Some(m);
        }
        if contains(head, b"word/") {
            return Some(DOCX);
        }
        if contains(head, b"xl/") {
            return Some(XLSX);
        }
        return Some(
            by_ext
                .filter(|m| m.starts_with("application/"))
                .unwrap_or("application/zip"),
        );
    }
    if head.starts_with(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        let by_ext = path.and_then(mime_for_path);
        return Some(match by_ext {
            Some(m @ ("application/msword" | "application/vnd.ms-excel")) => m,
            _ => "application/x-ole-storage",
        });
    }
    None
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Header names that mark the start of an RFC 822 message.
const MAIL_HEADERS: &[&str] = &[
    "from",
    "to",
    "cc",
    "date",
    "subject",
    "message-id",
    "received",
    "return-path",
    "delivered-to",
    "mime-version",
    "reply-to",
    "in-reply-to",
    "references",
    "dkim-signature",
    "authentication-results",
    "content-type",
    "x-mailer",
    "x-gmail-labels",
    "x-gm-thrid",
    "sender",
];

fn looks_like_mail(text: &str) -> bool {
    let mut known = 0;
    let mut strong = false;
    for line in text.lines().take(40) {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            continue; // folded header
        }
        let Some((name, _)) = line.split_once(':') else {
            return false;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return false;
        }
        let name = name.to_ascii_lowercase();
        if MAIL_HEADERS.contains(&name.as_str()) {
            known += 1;
            strong |= matches!(
                name.as_str(),
                "from" | "message-id" | "received" | "subject" | "delivered-to" | "return-path"
            );
        }
    }
    known >= 2 && strong
}

/// The head as text, if it looks like text (tolerating a multi-byte char cut at the end).
fn head_text(head: &[u8]) -> Option<String> {
    if head.starts_with(b"\xFF\xFE") || head.starts_with(b"\xFE\xFF") {
        return Some(crate::util::decode_text(head, None));
    }
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    if head.contains(&0) {
        return None;
    }
    match std::str::from_utf8(head) {
        Ok(s) => Some(s.to_owned()),
        Err(e) if e.error_len().is_none() => {
            Some(String::from_utf8_lossy(&head[..e.valid_up_to()]).into_owned())
        }
        Err(_) => {
            // Legacy 8-bit text: mostly printable ASCII.
            let printable = head
                .iter()
                .filter(|b| b.is_ascii_graphic() || b.is_ascii_whitespace() || **b >= 0x80)
                .count();
            let control = head.len() - printable;
            (control * 20 <= head.len()).then(|| crate::util::decode_text(head, None))
        }
    }
}

pub(crate) fn detect(path: Option<&Path>, head: &[u8]) -> String {
    if let Some(m) = magic(path, head) {
        return m.to_owned();
    }
    if let Some(m) = path.and_then(mime_for_path) {
        return m.to_owned();
    }
    let Some(text) = head_text(head) else {
        return "application/octet-stream".to_owned();
    };
    let start = text.trim_start();
    let lower: String = start
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return "text/html".to_owned();
    }
    if lower.starts_with("begin:vcalendar") {
        return "text/calendar".to_owned();
    }
    if looks_like_mail(&text) {
        return "message/rfc822".to_owned();
    }
    "text/plain".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: Option<&str>, head: &[u8]) -> String {
        detect(name.map(Path::new), head)
    }

    #[test]
    fn extensions_and_magic() {
        assert_eq!(d(Some("a.md"), b"# hi"), "text/markdown");
        assert_eq!(d(Some("a.rs"), b"fn main(){}"), "text/x-rust");
        assert_eq!(d(Some("Dockerfile"), b"FROM x"), "text/x-dockerfile");
        assert_eq!(d(Some("a.txt"), b"%PDF-1.7"), "application/pdf");
        assert_eq!(d(None, b"\x89PNG\r\n\x1a\n...."), "image/png");
        assert_eq!(d(None, b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(d(None, b"PK\x03\x04....word/document.xml"), DOCX);
        assert_eq!(d(Some("x.xlsx"), b"PK\x03\x04"), XLSX);
        assert_eq!(d(Some("x.zip"), b"PK\x03\x04"), "application/zip");
        assert_eq!(d(None, b"  <!DOCTYPE html><html>"), "text/html");
        assert_eq!(d(None, b"BEGIN:VCALENDAR\r\n"), "text/calendar");
        assert_eq!(
            d(None, b"From: a@b.c\r\nSubject: hi\r\n\r\nbody"),
            "message/rfc822"
        );
        assert_eq!(d(None, b"Note: just text\nmore"), "text/plain");
        assert_eq!(d(None, b"caf\xC3"), "text/plain");
        assert_eq!(d(None, b"\0\x01\x02\x03"), "application/octet-stream");
    }

    #[test]
    fn params() {
        assert_eq!(base(" Text/HTML ; charset=x"), "text/html");
        assert_eq!(
            param("text/plain; Charset=\"ISO-8859-1\"", "charset").as_deref(),
            Some("ISO-8859-1")
        );
        assert_eq!(language_for_mime("text/x-rust"), Some("rust"));
        assert_eq!(language_for_mime("text/plain"), None);
    }
}
