fn main() {
    tauri_build::build();
    guard_release_without_frontend();
}

/// A release build without tauri's `custom-protocol` feature does not embed the frontend: the
/// window loads `devUrl` (the Vite dev server), which nothing serves outside `tauri dev`, so the
/// app opens on a white window. `tauri build` turns the feature on; a plain
/// `cargo build --release -p nugget-desktop` does not. Stop such a build here with what to run.
///
/// tauri's build script exports `DEP_TAURI_DEV` (read by `tauri_build::is_dev`) when the feature
/// is off, which also sets `cfg(dev)` for this crate. Debug builds are left alone so `tauri dev`
/// and `cargo build`/`cargo test` keep working. `NUGGET_ALLOW_DEV_RELEASE=1` lets an optimized
/// build that loads the dev server through (`tauri dev --release`).
fn guard_release_without_frontend() {
    println!("cargo:rerun-if-env-changed=NUGGET_ALLOW_DEV_RELEASE");
    let optimized = std::env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_none();
    let allowed = std::env::var("NUGGET_ALLOW_DEV_RELEASE").is_ok_and(|v| v.trim() == "1");
    if optimized && tauri_build::is_dev() && !allowed {
        println!(
            "cargo::error=nugget-desktop: this release build would not embed the frontend and \
             would open on a white window. Build it with the frontend embedded: \
             `cargo build --release -p nugget-desktop --features tauri/custom-protocol` (after \
             `pnpm -C apps/desktop build`), or `pnpm tauri build` in apps/desktop, or on macOS \
             scripts/build-macos-app.sh. To build an optimized app that loads the Vite dev \
             server on purpose (`tauri dev --release`), set NUGGET_ALLOW_DEV_RELEASE=1."
        );
    }
}
