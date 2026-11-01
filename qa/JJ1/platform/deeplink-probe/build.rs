use std::{env, fs, path::Path};
fn main() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../apps/desktop/src-tauri/src/deeplink.rs");
    println!("cargo:rerun-if-changed={}", src.display());
    let text = fs::read_to_string(&src).expect("deeplink.rs");
    let start = text.find("const MAX_URL_LEN").expect("start marker");
    let end = text.find("/// The link waiting for the main window").expect("end marker");
    let body = text[start..end].replace("tauri::Url", "url::Url");
    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("deeplink_pure.rs");
    fs::write(out, format!("#[allow(dead_code)]\npub const SCHEME: &str = \"nugget\";\n#[allow(dead_code)]\npub const PLUGIN_EXTENSION: &str = \"nuggetplugin\";\nuse std::path::{{Path, PathBuf}};\nuse serde::Serialize;\n{body}")).unwrap();
}
