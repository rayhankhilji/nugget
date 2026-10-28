use nugget_browser::install::{
    self, Browser, CHROMIUM_DEV_EXTENSION_ID, FIREFOX_EXTENSION_ID, InstallConfig, InstallError,
    Os, Step,
};
use serde_json::{Value, json};

fn config(os: Os) -> InstallConfig {
    let (home, lad, host) = match os {
        Os::MacOs => (
            "/Users/ada",
            None,
            "/Applications/Nugget.app/Contents/MacOS/nugget-browser-host",
        ),
        Os::Linux => (
            "/home/ada",
            None,
            "/home/ada/.local/bin/nugget-browser-host",
        ),
        Os::Windows => (
            r"C:\Users\ada",
            Some(r"C:\Users\ada\AppData\Local".to_owned()),
            r"C:\Users\ada\AppData\Local\Programs\Nugget\nugget-browser-host.exe",
        ),
    };
    InstallConfig {
        os,
        home: home.into(),
        local_app_data: lad,
        host_path: host.into(),
        chromium_extension_ids: vec![CHROMIUM_DEV_EXTENSION_ID.into()],
        firefox_extension_ids: vec![FIREFOX_EXTENSION_ID.into()],
    }
}

/// Compare with `tests/golden/<os>.json`; `UPDATE_GOLDEN=1` rewrites the files.
fn golden(os: Os) {
    let cfg = config(os);
    let doc = json!({
        "install": install::plan(&Browser::ALL, &cfg).unwrap(),
        "uninstall": install::uninstall_plan(&Browser::ALL, &cfg).unwrap(),
    });
    let text = serde_json::to_string_pretty(&doc).unwrap() + "\n";
    let path = format!(
        "{}/tests/golden/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        os.name()
    );
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\r\n", "\n");
    assert_eq!(
        text, want,
        "golden mismatch for {path}; rerun with UPDATE_GOLDEN=1 if intended"
    );
}

#[test]
fn golden_macos() {
    golden(Os::MacOs);
}

#[test]
fn golden_linux() {
    golden(Os::Linux);
}

#[test]
fn golden_windows() {
    golden(Os::Windows);
}

#[test]
fn manifests_follow_each_browsers_format() {
    let cfg = config(Os::Linux);
    let chrome = install::manifest(Browser::Chrome, &cfg);
    assert_eq!(chrome["name"], "dev.nugget.browser");
    assert_eq!(chrome["type"], "stdio");
    assert_eq!(
        chrome["allowed_origins"],
        json!([format!("chrome-extension://{CHROMIUM_DEV_EXTENSION_ID}/")])
    );
    assert_eq!(chrome.get("allowed_extensions"), None);
    let ff = install::manifest(Browser::Firefox, &cfg);
    assert_eq!(ff["allowed_extensions"], json!([FIREFOX_EXTENSION_ID]));
    assert_eq!(ff.get("allowed_origins"), None);
}

#[test]
fn rejects_relative_host_path_and_missing_localappdata() {
    let mut cfg = config(Os::Linux);
    cfg.host_path = "nugget-browser-host".into();
    assert!(matches!(
        install::plan(&[Browser::Chrome], &cfg),
        Err(InstallError::RelativeHostPath(_))
    ));
    let mut win = config(Os::Windows);
    win.local_app_data = None;
    assert!(matches!(
        install::plan(&[Browser::Edge], &win),
        Err(InstallError::MissingLocalAppData)
    ));
}

#[test]
fn dry_run_touches_nothing_and_apply_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let os = Os::current();
    let mut cfg = config(os);
    cfg.home = dir.path().to_string_lossy().into_owned();
    cfg.local_app_data = Some(cfg.home.clone());
    cfg.host_path = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    // Registry steps can't run in tests; files only.
    let steps: Vec<Step> = install::plan(&Browser::ALL, &cfg)
        .unwrap()
        .into_iter()
        .filter(|s| matches!(s, Step::WriteFile { .. }))
        .collect();
    let log = install::apply(&steps, true).unwrap();
    assert_eq!(log.len(), 4);
    assert!(log.iter().all(|l| l.starts_with("would write ")));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    install::apply(&steps, false).unwrap();
    for b in Browser::ALL {
        let path = install::manifest_path(b, &cfg).unwrap();
        let m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(m["path"], cfg.host_path.as_str());
    }
    let remove: Vec<Step> = install::uninstall_plan(&Browser::ALL, &cfg)
        .unwrap()
        .into_iter()
        .filter(|s| matches!(s, Step::RemoveFile { .. }))
        .collect();
    install::apply(&remove, false).unwrap();
    install::apply(&remove, false).unwrap(); // idempotent
    for b in Browser::ALL {
        assert!(!std::path::Path::new(&install::manifest_path(b, &cfg).unwrap()).exists());
    }
}

#[cfg(not(windows))]
#[test]
fn registry_steps_fail_off_windows() {
    let step = Step::SetRegistryDefault {
        key: install::registry_key(Browser::Chrome),
        value: "x".into(),
    };
    assert!(matches!(
        install::apply(&[step], false),
        Err(InstallError::RegistryUnsupported)
    ));
}
