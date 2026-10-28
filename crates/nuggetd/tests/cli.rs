//! The real binary: first run writes the default config, `status` talks to a running daemon, a
//! second instance exits 0, and SIGTERM (Unix) shuts it down cleanly. Runs with
//! `NUGGET_KEYCHAIN=off`, so the store key goes to a key file and no OS keychain is touched.

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_nuggetd");

fn nuggetd(home: &Path) -> Command {
    let mut c = Command::new(EXE);
    c.env("NUGGET_HOME", home)
        .env("NUGGET_KEYCHAIN", "off")
        .env_remove("NUGGET_LOG")
        .env_remove("RUST_LOG");
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    nuggetd(home).args(args).output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Kills the daemon if a test assertion fails before it is stopped.
struct Daemon(Option<Child>);

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn wait_for_status(home: &Path, child: &mut Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let out = run(home, &["status"]);
        if out.status.success() {
            return out;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("daemon exited early with {status}");
        }
        assert!(
            Instant::now() < deadline,
            "daemon never answered: {}",
            text(&out.stderr)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn version_flag() {
    let out = Command::new(EXE).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout).trim(),
        format!("nuggetd {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn status_without_a_daemon_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run(tmp.path(), &["status"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("not running"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn invalid_config_exits_with_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "[[providers]]\nid = \n").unwrap();
    let out = run(tmp.path(), &[]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(
        err.contains("invalid config") && err.contains("config.toml"),
        "{err}"
    );
}

#[test]
fn runs_reports_status_and_shuts_down() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let child = nuggetd(home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut daemon = Daemon(Some(child));

    let out = wait_for_status(home, daemon.0.as_mut().unwrap());
    let status = text(&out.stdout);
    assert!(
        status.starts_with(&format!("nuggetd {} running", env!("CARGO_PKG_VERSION"))),
        "{status}"
    );
    assert!(status.contains("store:     0 items, 0 chunks"), "{status}");
    assert!(
        status.contains("local  openai_compatible qwen3:4b  on-device  tiers: triage, light, synthesis  not probed"),
        "{status}"
    );

    // First run wrote the local-first default config, the key file and JSON logs.
    let config = std::fs::read_to_string(home.join("config/config.toml")).unwrap();
    assert_eq!(config, nuggetd::DEFAULT_CONFIG);
    assert!(
        home.join("data/store.key").exists(),
        "key file (keychain off)"
    );
    assert!(home.join("data/nugget.db").exists());
    let logs: Vec<_> = std::fs::read_dir(home.join("data/logs"))
        .unwrap()
        .flatten()
        .collect();
    assert!(!logs.is_empty());

    let json = run(home, &["status", "--json"]);
    assert!(json.status.success());
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["protocol"], 1);
    assert_eq!(v["providers"][0]["id"], "local");

    // A second instance sees the first and exits 0.
    let second = run(home, &[]);
    assert!(second.status.success(), "{}", text(&second.stderr));
    assert!(text(&second.stdout).contains("already running"));

    let mut child = daemon.0.take().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in ["data", "data/logs", "config"] {
            let mode = std::fs::metadata(home.join(dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{dir} must be private");
        }
        let killed = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(killed.success());
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            assert!(Instant::now() < deadline, "daemon did not stop on SIGTERM");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "graceful exit, got {status}");
        let mut stderr = String::new();
        use std::io::Read;
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(stderr.contains("wrote default config"), "{stderr}");

        let log = std::fs::read_dir(home.join("data/logs"))
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect::<String>();
        let lines: Vec<serde_json::Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).expect("JSON log line"))
            .collect();
        let has = |msg: &str| {
            lines
                .iter()
                .any(|l| l["fields"]["message"].as_str() == Some(msg))
        };
        assert!(has("nuggetd ready"), "{log}");
        assert!(has("stopped"), "{log}");
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    let after = run(home, &["status"]);
    assert!(!after.status.success());
}

/// With the keychain off, source secrets live in daemon memory: they work for the session and
/// never reach a file (logs, `sources.toml`, config) under the home directory.
#[cfg(unix)]
#[test]
fn keychain_off_keeps_source_secrets_out_of_files() {
    use nugget_core::ipc::{SourceInfo, SourceState, method};
    use serde_json::{Value, json};

    const SECRET: &str = "pw-never-in-a-file-7731";
    let tmp = tempfile::Builder::new()
        .prefix("nugget-cli-")
        .tempdir()
        .unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("config")).unwrap();
    std::fs::write(
        home.join("config/config.toml"),
        "[runtime]\nenabled = false\n",
    )
    .unwrap();
    let child = nuggetd(&home)
        .env("NUGGET_LOG", "debug")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Daemon(Some(child));
    wait_for_status(&home, daemon.0.as_mut().unwrap());

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let paths = nugget_core::paths::Paths::rooted(home.clone());
        let client = nugget_ipc::Client::connect(&paths).await.unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let info: SourceInfo = client
            .call(
                method::SOURCES_ADD,
                &json!({"kind": "imap", "name": "Mail", "settings": {
                    "host": "127.0.0.1", "port": port, "username": "me", "folders": ["INBOX"]}}),
            )
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let list: Vec<SourceInfo> = client.call(method::SOURCES_LIST, &()).await.unwrap();
            if list.iter().any(|s| s.state == SourceState::NeedsAuth) {
                break;
            }
            assert!(Instant::now() < deadline, "imap never asked for a password");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let r: Value = client
            .call(
                method::SOURCES_SECRET_SET,
                &json!({"id": info.id, "secret": SECRET}),
            )
            .await
            .unwrap();
        assert_eq!(r, Value::Null);
        // With the password set the source leaves needs_auth (and fails to connect instead).
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let list: Vec<SourceInfo> = client.call(method::SOURCES_LIST, &()).await.unwrap();
            if list.iter().all(|s| s.state != SourceState::NeedsAuth) {
                break;
            }
            assert!(Instant::now() < deadline, "password was not picked up");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let mut child = daemon.0.take().unwrap();
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "daemon did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }

    fn scan(dir: &Path, needle: &[u8]) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, needle);
            } else if let Ok(bytes) = std::fs::read(&p) {
                assert!(
                    !bytes.windows(needle.len()).any(|w| w == needle),
                    "{} contains the secret",
                    p.display()
                );
            }
        }
    }
    scan(&home, SECRET.as_bytes());
    let sources = std::fs::read_to_string(home.join("config/sources.toml")).unwrap();
    assert!(sources.contains("password:imap-1"), "{sources}");
}
