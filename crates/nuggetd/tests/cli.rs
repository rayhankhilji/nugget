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

/// Exit status of `child` once it exits, failing after `secs`.
fn wait_exit(child: &mut Child, secs: u64) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(Instant::now() < deadline, "daemon did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The messages of the daemon's JSON log lines, and the raw log.
fn log_messages(home: &Path) -> (Vec<String>, String) {
    let log = std::fs::read_dir(home.join("data/logs"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("nuggetd"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect::<String>();
    let messages = log
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("JSON log line"))
        .filter_map(|l| l["fields"]["message"].as_str().map(str::to_owned))
        .collect();
    (messages, log)
}

/// Spawns the daemon on `home` with the runtime off and waits until it answers.
fn spawn_quiet(home: &Path) -> Daemon {
    std::fs::create_dir_all(home.join("config")).unwrap();
    std::fs::write(
        home.join("config/config.toml"),
        "[runtime]\nenabled = false\n",
    )
    .unwrap();
    let child = nuggetd(home)
        .env("NUGGET_LOG", "debug")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = Daemon(Some(child));
    wait_for_status(home, daemon.0.as_mut().unwrap());
    daemon
}

#[test]
fn version_status_without_a_daemon_and_invalid_config() {
    let out = Command::new(EXE).arg("--version").output().unwrap();
    let want = format!("nuggetd {}", env!("CARGO_PKG_VERSION"));
    assert_eq!(text(&out.stdout).trim(), want);
    let tmp = tempfile::tempdir().unwrap();
    let out = run(tmp.path(), &["status"]);
    assert!(!out.status.success() && text(&out.stderr).contains("not running"));
    std::fs::create_dir_all(tmp.path().join("config")).unwrap();
    std::fs::write(
        tmp.path().join("config/config.toml"),
        "[[providers]]\nid = \n",
    )
    .unwrap();
    let out = run(tmp.path(), &[]);
    let err = text(&out.stderr);
    assert!(!out.status.success());
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
    // The default config routes to no provider of its own (the runtime has no model installed).
    assert!(status.contains("providers: none configured"), "{status}");

    // First run wrote the local-first default config, the key file and JSON logs.
    let config = std::fs::read_to_string(home.join("config/config.toml")).unwrap();
    assert_eq!(config, nuggetd::DEFAULT_CONFIG);
    assert!(
        home.join("data/store.key").exists(),
        "key file (keychain off)"
    );
    assert!(home.join("data/nugget.db").exists());

    let json = run(home, &["status", "--json"]);
    assert!(json.status.success());
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(v["protocol"], 1);
    assert_eq!(v["providers"], serde_json::json!([]));

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
            .status();
        assert!(killed.unwrap().success());
        let status = wait_exit(&mut child, 20);
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

        let (messages, log) = log_messages(home);
        let has = |m: &str| messages.iter().any(|x| x == m);
        assert!(has("nuggetd ready") && has("stopped"), "{log}");
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    let after = run(home, &["status"]);
    assert!(!after.status.success());
}

/// GG6-8: the desktop app's quit path. `daemon.shutdown` over IPC stops the real binary on
/// every OS (no console signal needed on Windows), with exit code 0 and "stopped" in its log.
#[test]
fn ipc_shutdown_exits_cleanly_and_logs_stopped() {
    use nugget_core::ipc::{DaemonShutdownResponse, StatusResponse, method};

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let mut daemon = spawn_quiet(home);
    let pid = daemon.0.as_ref().unwrap().id();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let answered = rt.block_on(async {
        let paths = nugget_core::paths::Paths::rooted(home.to_path_buf());
        let client = nugget_ipc::Client::connect(&paths).await.unwrap();
        let status: StatusResponse = client.call(method::STATUS, &()).await.unwrap();
        // Started by a test, not by a service manager.
        assert_eq!((status.pid, status.service.as_deref()), (Some(pid), None));
        let r: DaemonShutdownResponse = client
            .call(
                method::DAEMON_SHUTDOWN,
                &serde_json::json!({"reason": "app_quit"}),
            )
            .await
            .unwrap();
        r
    });
    assert!(answered.stopping);
    assert_eq!(answered.pid, pid);

    let status = wait_exit(daemon.0.as_mut().unwrap(), 30);
    assert!(status.success(), "clean exit, got {status}");
    let (messages, log) = log_messages(home);
    let at = |m: &str| messages.iter().position(|x| x == m);
    let (asked, stopped) = (at("stop requested over IPC"), at("stopped"));
    assert!(asked.is_some() && stopped.is_some(), "{log}");
    assert!(asked < stopped, "{log}");
    assert!(log.contains("app_quit"), "the reason is logged: {log}");
    assert!(!run(home, &["status"]).status.success());
}

/// With the keychain off, source secrets live in daemon memory: they work for the session and
/// never reach a file (logs, `sources.toml`, config) under the home directory.
#[cfg(unix)]
#[test]
fn keychain_off_keeps_source_secrets_out_of_files() {
    use nugget_core::ipc::{SourceInfo, SourceState, method};
    use serde_json::{Value, json};

    const SECRET: &str = "pw-never-in-a-file-7731";
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let mut daemon = spawn_quiet(&home);
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

    let child = daemon.0.as_mut().unwrap();
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    wait_exit(child, 20);

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
