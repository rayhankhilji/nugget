//! Fixture helpers: committed text fixtures and programmatically built PDF / DOCX / XLSX files.

use std::io::Write;
use std::path::PathBuf;

use nugget_extract::{ExtractOptions, Extracted, Source, extract};
use zip::write::SimpleFileOptions;

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixture_path(name)).expect("fixture")
}

pub fn run(mime: &str, bytes: &[u8]) -> Extracted {
    extract(mime, Source::Bytes(bytes), &ExtractOptions::default())
        .unwrap_or_else(|e| panic!("{mime}: {e}"))
}

/// A fresh, empty temporary directory.
pub fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nugget-extract-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn pdf_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// A minimal valid PDF: one page per entry, each line drawn with Helvetica, plus an Info title.
pub fn make_pdf(title: &str, pages: &[&[&str]]) -> Vec<u8> {
    let n = pages.len();
    // Objects: 1 catalog, 2 pages, 3 font, 4 info, then (page, contents) pairs.
    let mut objects: Vec<String> = Vec::new();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {n} >>",
        kids.join(" ")
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    objects.push(format!(
        "<< /Title ({}) /Author (Nugget Tests) >>",
        pdf_escape(title)
    ));
    for (i, lines) in pages.iter().enumerate() {
        let mut content = String::new();
        for (j, line) in lines.iter().enumerate() {
            let y = 720 - 20 * j as i32;
            content.push_str(&format!(
                "BT /F1 12 Tf 72 {y} Td ({}) Tj ET\n",
                pdf_escape(line)
            ));
        }
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            6 + 2 * i
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{obj}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for off in offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R /Info 4 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

/// A ZIP archive of `(name, contents)` entries.
pub fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in entries {
        w.start_file(*name, opts).expect("zip entry");
        w.write_all(data).expect("zip write");
    }
    w.finish().expect("zip finish").into_inner()
}

const W_NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn para(style: Option<&str>, text: &str) -> String {
    let ppr = style
        .map(|s| format!(r#"<w:pPr><w:pStyle w:val="{s}"/></w:pPr>"#))
        .unwrap_or_default();
    format!(r#"<w:p>{ppr}<w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>"#)
}

/// The body XML of the standard test DOCX.
pub fn docx_body() -> String {
    let mut b = String::new();
    b.push_str(&para(Some("Title"), "Project Plan"));
    b.push_str(&para(None, "Prepared for the team &amp; partners."));
    b.push_str(&para(Some("Heading1"), "Goals"));
    b.push_str(r#"<w:p><w:r><w:t>Ship the </w:t></w:r><w:r><w:t>beta</w:t><w:tab/><w:t>soon.</w:t></w:r></w:p>"#);
    b.push_str(&para(Some("MyHeading2"), "Risks"));
    b.push_str(r#"<w:p><w:hyperlink r:id="rIdLink"><w:r><w:t>Tracker</w:t></w:r></w:hyperlink><w:r><w:delText>deleted words</w:delText></w:r></w:p>"#);
    b.push_str(r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Risk</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Owner</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>Delay</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Ann</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#);
    b.push_str(&para(Some("Heading1"), "Timeline"));
    b.push_str(&para(None, "Q3 &#8211; launch."));
    b
}

pub fn make_docx_with_body(body: &str) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W_NS}><w:body>{body}</w:body></w:document>"#
    );
    let styles = format!(
        r#"<?xml version="1.0"?><w:styles {W_NS}>
<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
<w:style w:type="paragraph" w:styleId="MyHeading2"><w:name w:val="Custom"/><w:pPr><w:outlineLvl w:val="1"/></w:pPr></w:style>
</w:styles>"#
    );
    let doc_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rIdStyles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
<Relationship Id="rIdLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://tracker.example.com/board" TargetMode="External"/>
</Relationships>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Core Title</dc:title><dc:creator>Ann Author</dc:creator></cp:coreProperties>"#;
    make_zip(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/_rels/document.xml.rels", doc_rels.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
        ("docProps/core.xml", core.as_bytes()),
    ])
}

pub fn make_docx() -> Vec<u8> {
    make_docx_with_body(&docx_body())
}

pub fn make_xlsx() -> Vec<u8> {
    let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>
<sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Notes &amp; Links" sheetId="2" r:id="rId2"/></sheets></workbook>"#;
    let wb_rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet2.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"#;
    let rels = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
    let shared = r#"<?xml version="1.0"?><sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="4" uniqueCount="4">
<si><t>Item</t></si><si><t>Cost</t></si><si><r><t>Lap</t></r><r><t>top</t></r><rPh><t>ignored</t></rPh></si><si><t>Desk &amp; chair</t></si></sst>"#;
    let sheet1 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
<row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>1299.5</v></c><c r="D2" t="b"><v>1</v></c></row>
<row r="3"><c r="A3" t="s"><v>3</v></c><c r="B3"><f>SUM(1,2)</f><v>300</v></c></row>
<row r="4"/>
</sheetData></worksheet>"#;
    let sheet2 = r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>See https://example.com/budget</t></is></c><c r="B1" t="str"><v>formula text</v></c></row>
</sheetData></worksheet>"#;
    let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Budget 2024</dc:title></cp:coreProperties>"#;
    make_zip(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("_rels/.rels", rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", wb_rels.as_bytes()),
        ("xl/sharedStrings.xml", shared.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet1.as_bytes()),
        ("xl/worksheets/sheet2.xml", sheet2.as_bytes()),
        ("docProps/core.xml", core.as_bytes()),
    ])
}

/// A tiny valid PNG (2x2, RGB), built with a stored (uncompressed) zlib stream.
pub fn make_png() -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn adler32(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &x in data {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, ty: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut c = ty.to_vec();
        c.extend(data);
        out.extend(&c);
        out.extend(crc32(&c).to_be_bytes());
    }
    let raw: Vec<u8> = [0u8, 255, 255, 255, 0, 0, 0, 0u8, 0, 0, 0, 255, 255, 255].to_vec();
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(&raw);
    z.extend(adler32(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend(2u32.to_be_bytes());
    ihdr.extend(2u32.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
