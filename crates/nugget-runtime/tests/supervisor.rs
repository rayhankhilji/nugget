//! Supervisor behaviour against the fake llama-server.
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{addr, alive, env, get, read_all_logs, wait_until};
use nugget_core::Error;
use nugget_core::capability::{Locality, Tier};
use nugget_core::egress::NullEgress;
use nugget_infer::{
    ApiKeyRef, ChatRequest, ChatResponse, Message, ProviderKind, SecretResolver, SystemSecrets,
    build_provider_with,
};
use nugget_runtime::{RuntimeOptions, RuntimeRole, RuntimeState, Tuning, api_key_ref, choose_port};

fn with_extra(role: RuntimeRole, args: &[&str]) -> RuntimeOptions {
    let mut o = common::opts();
    let t = Tuning {
        extra_args: args.iter().map(|s| s.to_string()).collect(),
        ..Tuning::default()
    };
    match role {
        RuntimeRole::Chat => o.chat = t,
        RuntimeRole::Embedding => o.embedding = t,
    }
    o
}

fn key(rt: &nugget_runtime::Runtime, role: RuntimeRole) -> String {
    rt.secrets()
        .resolve(&api_key_ref(role))
        .unwrap()
        .expose()
        .to_string()
}

#[tokio::test]
async fn ensure_spawns_on_loopback_and_returns_an_on_device_provider_config() {
    let e = env().await;
    let rt = e.runtime(common::opts());
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();

    assert_eq!(cfg.id.as_str(), "local-chat");
    assert_eq!(cfg.kind, ProviderKind::OpenaiCompatible);
    assert_eq!(cfg.model, "chat-model");
    let base = cfg.base_url.clone().unwrap();
    // On Unix the server listens on a socket in a private directory; elsewhere on loopback.
    #[cfg(unix)]
    let sock = {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(base, "http://localhost/v1");
        let sock = cfg.socket_path.clone().expect("a Unix socket");
        assert_eq!(sock, e.paths.data.join("run").join("chat.sock"));
        let dir = std::fs::metadata(sock.parent().unwrap()).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        let d = rt.details(RuntimeRole::Chat).unwrap();
        assert_eq!((d.socket.as_ref(), d.port), (Some(&sock), None));
        sock
    };
    #[cfg(not(unix))]
    {
        assert!(
            base.starts_with("http://127.0.0.1:") && base.ends_with("/v1"),
            "{base}"
        );
        assert_eq!(cfg.socket_path, None);
    }
    assert_eq!(
        cfg.tiers,
        vec![Tier::Triage, Tier::Light],
        "embedding tier dropped for chat"
    );
    assert_eq!(cfg.effective_locality().unwrap(), Locality::OnDevice);
    // A reference to this server's key generation, resolved only by the runtime.
    assert!(
        matches!(&cfg.api_key, Some(ApiKeyRef::Env(n)) if n.starts_with("NUGGET_RUNTIME_API_KEY_CHAT_")),
        "{:?}",
        cfg.api_key
    );
    assert_eq!(
        rt.secrets()
            .resolve(cfg.api_key.as_ref().unwrap())
            .unwrap()
            .expose(),
        key(&rt, RuntimeRole::Chat)
    );

    let status = rt.status();
    assert_eq!(status.len(), 1);
    assert_eq!(status[0].state, RuntimeState::Ready);
    assert_eq!(status[0].provider.as_str(), "local-chat");
    assert_eq!(status[0].model_id, "chat-model");

    // Unauthenticated: only /health answers.
    let port = addr(&cfg);
    assert_eq!(get(&port, "/health", None).await.0, 200);
    assert_eq!(get(&port, "/v1/models", None).await.0, 401);
    assert_eq!(get(&port, "/v1/models", Some("wrong")).await.0, 401);
    let k = key(&rt, RuntimeRole::Chat);
    assert_eq!(k.len(), 64);
    let (code, body) = get(&port, "/v1/models", Some(&k)).await;
    assert_eq!(code, 200);
    assert!(body.contains("chat-model"));

    // The key is not on the command line, and the environment is scrubbed.
    let (_, info) = get(&port, "/fake/info", Some(&k)).await;
    let info: serde_json::Value = serde_json::from_str(&info).unwrap();
    let argv: Vec<String> = serde_json::from_value(info["argv"].clone()).unwrap();
    assert!(!argv.iter().any(|a| a.contains(&k)));
    #[cfg(unix)]
    {
        let sock = sock.to_string_lossy().into_owned();
        assert!(argv.windows(2).any(|w| w == ["--host", sock.as_str()]));
        assert!(!argv.iter().any(|a| a == "--port"));
    }
    #[cfg(not(unix))]
    assert!(argv.windows(2).any(|w| w == ["--host", "127.0.0.1"]));
    assert!(argv.windows(2).any(|w| w == ["--alias", "chat-model"]));
    assert!(!argv.iter().any(|a| a == "--embedding"));
    let env_names: Vec<String> = serde_json::from_value(info["env"].clone()).unwrap();
    assert!(env_names.contains(&"LLAMA_API_KEY".to_string()));
    for name in &env_names {
        assert!(
            [
                "LLAMA_API_KEY",
                "PATH",
                "SystemRoot",
                "SYSTEMROOT",
                "windir",
                "TEMP",
                "TMP",
                "TMPDIR",
                "LANG",
                "LC_ALL"
            ]
            .contains(&name.as_str()),
            "unexpected env var {name} leaked to the server"
        );
    }

    // Plain env lookup (without the runtime's resolver) fails closed.
    assert!(
        SystemSecrets
            .resolve(&api_key_ref(RuntimeRole::Chat))
            .is_err()
    );
    rt.shutdown().await;
}

#[tokio::test]
async fn configs_work_with_nugget_infer_providers() {
    let e = env().await;
    let rt = e.runtime(common::opts());
    let chat_cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let emb_cfg = rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    assert_ne!(addr(&chat_cfg), addr(&emb_cfg), "one listener per role");
    assert_eq!(emb_cfg.tiers, vec![Tier::Embedding]);
    assert_eq!(emb_cfg.id.as_str(), "local-embedding");

    let egress = Arc::new(common::VecSink::default());
    let secrets = rt.secrets();
    let (chat, emb) = tokio::task::spawn_blocking({
        let (c, m, egress, secrets) = (
            chat_cfg.clone(),
            emb_cfg.clone(),
            egress.clone(),
            secrets.clone(),
        );
        move || {
            (
                build_provider_with(&c, egress.clone(), &secrets).unwrap(),
                build_provider_with(&m, egress, &secrets).unwrap(),
            )
        }
    })
    .await
    .unwrap();
    assert_eq!(chat.meta().locality, Locality::OnDevice);

    let stream = chat
        .chat(ChatRequest::new(vec![Message::user_text("hi")]))
        .await
        .unwrap();
    let resp = ChatResponse::collect(stream).await.unwrap();
    assert_eq!(resp.text(), "Hello from fake");

    let v = emb.embed(&["alpha".into(), "beta".into()]).await.unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].len(), 8);
    assert_ne!(v[0], v[1]);
    // On-device: nothing in the egress ledger.
    assert!(egress.0.lock().unwrap().is_empty());
    // The chat server refuses embeddings and vice versa (roles are separate processes).
    assert!(chat.embed(&["x".into()]).await.is_err());

    // Without the runtime's resolver the config cannot be built (fail closed).
    let c2 = chat_cfg.clone();
    let built = tokio::task::spawn_blocking(move || {
        build_provider_with(&c2, Arc::new(NullEgress), &SystemSecrets).map(|_| ())
    })
    .await
    .unwrap();
    assert!(built.is_err());
    rt.shutdown().await;
}

#[tokio::test]
async fn ensure_is_idempotent_and_switches_models() {
    let e = env().await;
    let rt = e.runtime(common::opts());
    let a = rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    let pid = rt.details(RuntimeRole::Embedding).unwrap().pid.unwrap();
    let b = rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(rt.details(RuntimeRole::Embedding).unwrap().pid, Some(pid));

    // A different model replaces the server.
    let c = rt.ensure(RuntimeRole::Embedding, &e.chat).await.unwrap();
    assert_eq!(c.model, "chat-model");
    let pid2 = rt.details(RuntimeRole::Embedding).unwrap().pid.unwrap();
    assert_ne!(pid, pid2);
    wait_until("old server gone", Duration::from_secs(5), || !alive(pid)).await;
    rt.shutdown().await;
}

#[tokio::test]
async fn slow_start_is_awaited_and_a_hung_start_times_out() {
    let e = env().await;
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &["--fake-start-delay-ms", "700"],
    ));
    let t = Instant::now();
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    assert!(
        t.elapsed() >= Duration::from_millis(600),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(get(&addr(&cfg), "/health", None).await.0, 200);
    rt.shutdown().await;

    let mut o = with_extra(RuntimeRole::Chat, &["--fake-start-delay-ms", "60000"]);
    o.health_timeout = Duration::from_millis(400);
    let rt = e.runtime(o);
    let err = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap_err();
    assert!(
        matches!(&err, Error::Provider(m) if m.contains("not ready")),
        "{err}"
    );
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.info.state, RuntimeState::Failed);
    assert!(d.info.error.unwrap().contains("not ready"));
    assert_eq!(d.pid, None, "the hung server was killed");
}

#[tokio::test]
async fn startup_failure_reports_the_server_output() {
    let e = env().await;
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &["--fake-exit-immediately", "7"],
    ));
    let err = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("exited during start-up"), "{msg}");
    assert!(msg.contains("exiting immediately with 7"), "{msg}");
    assert_eq!(rt.status()[0].state, RuntimeState::Failed);
}

#[tokio::test]
async fn crashed_server_is_restarted_on_the_same_port() {
    let e = env().await;
    let marker = e.tmp.path().join("crashed-once");
    let marker_s = marker.to_string_lossy().into_owned();
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &[
            "--fake-crash-after-ms",
            "300",
            "--fake-crash-once",
            &marker_s,
        ],
    ));
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let pid1 = rt.details(RuntimeRole::Chat).unwrap().pid.unwrap();
    wait_until("crash and restart", Duration::from_secs(10), || {
        rt.details(RuntimeRole::Chat)
            .is_some_and(|d| d.restarts == 1 && d.info.state == RuntimeState::Ready)
    })
    .await;
    assert!(marker.exists());
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_ne!(d.pid.unwrap(), pid1);
    let cfg2 = d.config.clone().unwrap();
    assert_eq!(cfg2.base_url, cfg.base_url);
    assert_eq!(addr(&cfg2), addr(&cfg), "same port or socket");
    assert_ne!(cfg2.api_key, cfg.api_key, "new key generation");
    assert!(!alive(pid1));
    // The port keeps working, with the new key.
    let k = key(&rt, RuntimeRole::Chat);
    assert_eq!(get(&addr(&cfg), "/v1/models", Some(&k)).await.0, 200);

    // A crash on request (exit 42) is handled the same way, and ensure() waits it out.
    let a = addr(&cfg);
    let _ = a
        .client()
        .post(a.url("/fake/crash"))
        .bearer_auth(&k)
        .send()
        .await;
    let again = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    assert_eq!(addr(&again), a);
    wait_until("second restart", Duration::from_secs(10), || {
        rt.details(RuntimeRole::Chat)
            .is_some_and(|d| d.restarts == 2 && d.info.state == RuntimeState::Ready)
    })
    .await;
    let logs = read_all_logs(&e.paths.data.join("logs"));
    assert!(logs.contains("exited unexpectedly"), "{logs}");
    rt.shutdown().await;
}

#[tokio::test]
async fn crash_loop_gives_up_and_ensure_retries() {
    let e = env().await;
    let mut o = with_extra(RuntimeRole::Chat, &["--fake-crash-after-ms", "150"]);
    o.max_restarts = 2;
    let rt = e.runtime(o);
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    wait_until("give up", Duration::from_secs(10), || {
        rt.details(RuntimeRole::Chat).is_some_and(|d| {
            d.info.state == RuntimeState::Failed
                && d.info
                    .error
                    .as_deref()
                    .is_some_and(|m| m.contains("gave up"))
        })
    })
    .await;
    assert_eq!(rt.details(RuntimeRole::Chat).unwrap().pid, None);
    assert!(rt.details(RuntimeRole::Chat).unwrap().config.is_none());
    // A new ensure starts from scratch (and succeeds until the next crash).
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    rt.shutdown().await;
}

#[tokio::test]
async fn stop_kills_the_server_and_its_process_group() {
    let e = env().await;
    let pidfile = e.tmp.path().join("child.pid");
    let p = pidfile.to_string_lossy().into_owned();
    let rt = e.runtime(with_extra(RuntimeRole::Chat, &["--fake-spawn-child", &p]));
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let pid = rt.details(RuntimeRole::Chat).unwrap().pid.unwrap();
    let child: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(pid) && alive(child));
    assert!(e.paths.data.join("runtime/chat.pid").exists());

    rt.stop(RuntimeRole::Chat).await;
    assert!(!alive(pid));
    #[cfg(unix)]
    wait_until(
        "grandchild killed with the group",
        Duration::from_secs(5),
        || !alive(child),
    )
    .await;
    #[cfg(not(unix))]
    let _ = child;
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.info.state, RuntimeState::Stopped);
    assert!(d.config.is_none());
    assert!(!e.paths.data.join("runtime/chat.pid").exists());
    assert_eq!(get(&addr(&cfg), "/health", None).await.0, 0);
    // The socket file is gone with the server.
    if let Some(sock) = &cfg.socket_path {
        assert!(!sock.exists());
    }
    // Stopping again is a no-op; ensure brings it back.
    rt.stop(RuntimeRole::Chat).await;
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    rt.shutdown().await;
}

#[tokio::test]
async fn dropping_the_runtime_kills_every_server() {
    let e = env().await;
    let pidfile = e.tmp.path().join("child.pid");
    let p = pidfile.to_string_lossy().into_owned();
    let rt = e.runtime(with_extra(
        RuntimeRole::Embedding,
        &["--fake-spawn-child", &p],
    ));
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    let pids: Vec<u32> = [RuntimeRole::Chat, RuntimeRole::Embedding]
        .iter()
        .map(|r| rt.details(*r).unwrap().pid.unwrap())
        .collect();
    let child: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let clone = rt.clone();
    drop(rt);
    assert!(pids.iter().all(|p| alive(*p)), "a clone keeps them alive");
    drop(clone);
    wait_until("servers killed on drop", Duration::from_secs(5), || {
        pids.iter().all(|p| !alive(*p))
    })
    .await;
    #[cfg(unix)]
    wait_until("grandchild killed on drop", Duration::from_secs(5), || {
        !alive(child)
    })
    .await;
    #[cfg(not(unix))]
    let _ = child;
    assert!(!e.paths.data.join("runtime/chat.pid").exists());
}

#[tokio::test]
async fn idle_servers_stop_and_come_back_on_ensure() {
    let e = env().await;
    let mut o = common::opts();
    o.idle_shutdown = Some(Duration::from_millis(600));
    o.idle_check_interval = Duration::from_millis(100);
    let rt = e.runtime(o);
    let cfg = rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    let a = addr(&cfg);

    // Requests (seen through /metrics) keep it alive past the idle timeout.
    let k = key(&rt, RuntimeRole::Embedding);
    let client = a.client();
    let busy_until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < busy_until {
        let r = client
            .post(a.url("/v1/embeddings"))
            .bearer_auth(&k)
            .header("content-type", "application/json")
            .body(r#"{"input": ["x"], "model": "embed-model"}"#)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(rt.status()[0].state, RuntimeState::Ready);
    let pid = rt.details(RuntimeRole::Embedding).unwrap().pid.unwrap();

    wait_until("idle stop", Duration::from_secs(5), || {
        rt.status()[0].state == RuntimeState::Stopped
    })
    .await;
    // `Stopped` is reported (and the config withdrawn) before the process is signalled, so the
    // process may still be exiting here.
    wait_until("process exit", Duration::from_secs(10), || !alive(pid)).await;
    let again = rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    assert_eq!(addr(&again), a, "the old port (or the socket) is reused");
    rt.shutdown().await;
}

#[tokio::test]
async fn server_output_goes_to_a_redacted_log_file() {
    let e = env().await;
    let rt = e.runtime(with_extra(RuntimeRole::Chat, &["--fake-print-key"]));
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let k = key(&rt, RuntimeRole::Chat);
    let log = rt.log_file(RuntimeRole::Chat);
    assert!(log.starts_with(e.paths.data.join("logs")));
    wait_until("log lines", Duration::from_secs(5), || {
        std::fs::read_to_string(&log).is_ok_and(|s| s.contains("model loaded"))
    })
    .await;
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("fake: api key is <redacted>"), "{text}");
    assert!(!text.contains(&k));
    assert!(text.contains("[stdout] fake: listening"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    rt.shutdown().await;
    // Nothing else under data/ carries the key either (pid files, install records).
    assert!(!read_all_logs(&e.paths.data.join("runtime")).contains(&k));
}

#[tokio::test]
async fn orphans_from_a_dead_daemon_are_reaped() {
    let e = env().await;
    // A server started by "a previous daemon" (not in our process group).
    let port = choose_port(None).unwrap();
    let model = e.store.path("chat-model").unwrap();
    let mut orphan = std::process::Command::new(common::fake_bin())
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--model",
        ])
        .arg(&model)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let dir = e.paths.data.join("runtime");
    std::fs::create_dir_all(&dir).unwrap();
    let rec = serde_json::json!({"pid": orphan.id(), "exe": common::fake_bin(), "port": port});
    std::fs::write(dir.join("chat.pid"), rec.to_string()).unwrap();
    // A stale record whose pid now belongs to something else (this test) is left alone.
    let me = serde_json::json!({"pid": std::process::id(), "exe": common::fake_bin(), "port": 1});
    std::fs::write(dir.join("embedding.pid"), me.to_string()).unwrap();

    let _rt = e.runtime(common::opts());
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(s) = orphan.try_wait().unwrap() {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("orphan killed");
    assert!(!status.success());
    assert!(!dir.join("chat.pid").exists() && !dir.join("embedding.pid").exists());
}

#[tokio::test]
async fn bad_requests_are_refused_before_spawning() {
    let e = env().await;
    let rt = e.runtime(common::opts());
    // Role/model mismatch.
    let err = rt
        .ensure(RuntimeRole::Embedding, &{
            let mut m = e.chat.clone();
            m.tiers = vec![Tier::Light];
            m
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    let err = rt.ensure(RuntimeRole::Chat, &e.embed).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    // Model not installed.
    let mut missing = e.embed.clone();
    missing.id = "not-installed".into();
    let err = rt
        .ensure(RuntimeRole::Embedding, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    // Dangerous pass-through flags.
    for bad in [
        ["--host", "0.0.0.0"],
        ["--host=0.0.0.0", ""],
        ["--api-key", "x"],
        ["--port", "80"],
    ] {
        let args: Vec<&str> = bad.iter().copied().filter(|s| !s.is_empty()).collect();
        let rt = e.runtime(with_extra(RuntimeRole::Chat, &args));
        let err = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{bad:?}: {err}");
    }
    // A missing binary.
    let rt = e
        .runtime(common::opts())
        .with_server_binary(e.tmp.path().join("nope"), nugget_models::RuntimeAccel::Cpu);
    let err = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    assert!(rt.status().iter().all(|s| s.state == RuntimeState::Failed));
}

#[test]
fn free_port_selection() {
    let p = choose_port(None).unwrap();
    assert_ne!(p, 0);
    // A preferred free port is kept.
    assert_eq!(choose_port(Some(p)).unwrap(), p);
    // A taken one is replaced.
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = l.local_addr().unwrap().port();
    let other = choose_port(Some(taken)).unwrap();
    assert_ne!(other, taken);
    assert!(std::net::TcpListener::bind(("127.0.0.1", other)).is_ok());
}

/// With Unix sockets turned off (and always on Windows) servers listen on a loopback port.
#[tokio::test]
async fn tcp_listeners_when_unix_sockets_are_off() {
    let e = env().await;
    let mut o = common::opts();
    o.unix_socket = false;
    let rt = e.runtime(o);
    let cfg = rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    let base = cfg.base_url.clone().unwrap();
    assert!(
        base.starts_with("http://127.0.0.1:") && base.ends_with("/v1"),
        "{base}"
    );
    assert_eq!(cfg.socket_path, None);
    assert_eq!(cfg.effective_locality().unwrap(), Locality::OnDevice);
    let d = rt.details(RuntimeRole::Embedding).unwrap();
    assert_eq!(d.socket, None);
    let a = addr(&cfg);
    assert_eq!(a, common::Addr::Port(d.port.unwrap()));
    assert_eq!(get(&a, "/health", None).await.0, 200);
    let (_, info) = get(&a, "/fake/info", Some(&key(&rt, RuntimeRole::Embedding))).await;
    let info: serde_json::Value = serde_json::from_str(&info).unwrap();
    let argv: Vec<String> = serde_json::from_value(info["argv"].clone()).unwrap();
    assert!(argv.windows(2).any(|w| w == ["--host", "127.0.0.1"]));
    assert!(!e.paths.data.join("run").exists());
    rt.shutdown().await;
}

/// A socket file left behind by a server that died (or anything else at its path) does not
/// stop the next server from binding it.
#[cfg(unix)]
#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let e = env().await;
    let dir = e.paths.data.join("run");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("chat.sock"), b"stale").unwrap();
    let rt = e.runtime(common::opts());
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    assert_eq!(get(&addr(&cfg), "/health", None).await.0, 200);
    rt.shutdown().await;
    assert!(!dir.join("chat.sock").exists());
}

// ---- Regression tests: who is on the port, stale configs, key rotation, detached starts ----

/// Another local process grabs the server's port before the server binds it, and answers
/// `/health` with ok. The runtime must not take it for the server: no config, no key, no prompt.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_squatter_on_the_servers_port_is_rejected() {
    let e = env().await;
    let pidfile = e.tmp.path().join("squatter.pid");
    let p = pidfile.to_string_lossy().into_owned();
    let mut o = with_extra(RuntimeRole::Chat, &["--fake-squat", &p]);
    o.health_timeout = Duration::from_secs(5);
    // The port race exists only for TCP listeners.
    o.unix_socket = false;
    let rt = e.runtime(o);
    let res = rt.ensure(RuntimeRole::Chat, &e.chat).await;
    let squatter = common::KillOnDrop(
        std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse().ok()),
    );
    let squatter_pid = squatter.0.expect("the fake server started a squatter");
    assert!(alive(squatter_pid), "the squatter is still listening");

    let err = res.expect_err("a foreign listener must not count as ready");
    assert!(
        matches!(&err, Error::Provider(m) if m.contains("another process")),
        "{err}"
    );
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.info.state, RuntimeState::Failed);
    assert!(d.config.is_none());
    assert_eq!(d.pid, None, "our server was stopped");
    // The squatter only ever saw unauthenticated health probes: neither the key nor a request.
    let seen = std::fs::read_to_string(format!("{p}.log")).unwrap_or_default();
    assert!(seen.contains("GET /health"), "{seen}");
    assert!(!seen.contains("authorization=true"), "{seen}");
    assert!(!seen.contains("/v1/"), "{seen}");
}

/// While a server is shutting down (here it ignores SIGTERM for the whole grace period), its
/// port must not be handed out: the next owner of the port could be anybody.
#[cfg(unix)]
#[tokio::test]
async fn no_config_is_handed_out_while_stopping() {
    let e = env().await;
    let mut o = with_extra(RuntimeRole::Chat, &["--fake-ignore-sigterm"]);
    o.shutdown_grace = Duration::from_millis(1500);
    let rt = e.runtime(o);
    rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let pid = rt.details(RuntimeRole::Chat).unwrap().pid.unwrap();

    let stopping = tokio::spawn({
        let rt = rt.clone();
        async move { rt.stop(RuntimeRole::Chat).await }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(alive(pid), "still in its shutdown grace period");
    assert!(!stopping.is_finished());
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.info.state, RuntimeState::Stopping);
    assert!(d.config.is_none(), "config handed out during stop");
    assert_eq!(rt.status()[0].state, RuntimeState::Stopping);
    assert_eq!(
        serde_json::to_value(&rt.status()[0]).unwrap()["state"],
        "stopping"
    );
    stopping.await.unwrap();
    assert!(!alive(pid));
    assert_eq!(
        rt.details(RuntimeRole::Chat).unwrap().info.state,
        RuntimeState::Stopped
    );
}

/// Same for an idle stop, which the supervisor does on its own.
#[cfg(unix)]
#[tokio::test]
async fn no_config_is_handed_out_during_an_idle_stop() {
    let e = env().await;
    let mut o = with_extra(RuntimeRole::Embedding, &["--fake-ignore-sigterm"]);
    o.shutdown_grace = Duration::from_millis(1500);
    o.idle_shutdown = Some(Duration::from_millis(200));
    o.idle_check_interval = Duration::from_millis(50);
    let rt = e.runtime(o);
    rt.ensure(RuntimeRole::Embedding, &e.embed).await.unwrap();
    let pid = rt.details(RuntimeRole::Embedding).unwrap().pid.unwrap();
    wait_until("idle stop begins", Duration::from_secs(5), || {
        rt.details(RuntimeRole::Embedding)
            .is_some_and(|d| d.info.state != RuntimeState::Ready)
    })
    .await;
    // The server has been asked to stop but is still there: no config for it any more.
    assert!(
        alive(pid),
        "the config was withdrawn only after the server was gone"
    );
    let d = rt.details(RuntimeRole::Embedding).unwrap();
    assert!(d.config.is_none());
    assert_eq!(d.info.state, RuntimeState::Stopping);
    wait_until("idle server gone", Duration::from_secs(5), || !alive(pid)).await;
    wait_until("stopped", Duration::from_secs(5), || {
        rt.details(RuntimeRole::Embedding)
            .is_some_and(|d| d.info.state == RuntimeState::Stopped)
    })
    .await;
    rt.shutdown().await;
}

/// A crashed server is reported as not ready as soon as anybody looks, not on the
/// supervisor's next tick, so its (now free) port is not handed out.
#[tokio::test]
async fn a_crash_is_reported_at_once() {
    let e = env().await;
    let mut o = common::opts();
    // The supervisor would only notice after 30 s.
    o.watch_interval = Duration::from_secs(30);
    let rt = e.runtime(o);
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let pid = rt.details(RuntimeRole::Chat).unwrap().pid.unwrap();
    let a = addr(&cfg);
    let _ = a
        .client()
        .post(a.url("/fake/crash"))
        .bearer_auth(key(&rt, RuntimeRole::Chat))
        .send()
        .await;
    wait_until("server exited", Duration::from_secs(5), || !alive(pid)).await;
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_ne!(d.info.state, RuntimeState::Ready);
    assert!(d.config.is_none());
    assert_eq!(d.pid, None, "a dead server has no pid");
    assert!(
        d.info
            .error
            .as_deref()
            .is_some_and(|m| m.contains("exited unexpectedly")),
        "{:?}",
        d.info.error
    );
    rt.shutdown().await;
}

/// Every server process gets a new API key: a key captured while the port was unguarded (or
/// held by a stale client) does not open the restarted server, and old configs fail closed.
#[tokio::test]
async fn the_api_key_rotates_on_every_restart() {
    let e = env().await;
    let marker = e.tmp.path().join("crashed-once");
    let marker_s = marker.to_string_lossy().into_owned();
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &[
            "--fake-crash-after-ms",
            "300",
            "--fake-crash-once",
            &marker_s,
        ],
    ));
    let cfg1 = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let k1 = key(&rt, RuntimeRole::Chat);
    let secrets = rt.secrets();
    assert_eq!(
        secrets
            .resolve(cfg1.api_key.as_ref().unwrap())
            .unwrap()
            .expose(),
        k1
    );

    // Crash and restart by the supervisor.
    wait_until("crash and restart", Duration::from_secs(10), || {
        rt.details(RuntimeRole::Chat)
            .is_some_and(|d| d.restarts == 1 && d.info.state == RuntimeState::Ready)
    })
    .await;
    let cfg2 = rt.details(RuntimeRole::Chat).unwrap().config.unwrap();
    let k2 = key(&rt, RuntimeRole::Chat);
    assert_ne!(k1, k2, "the key changed with the restart");
    assert_ne!(cfg1.api_key, cfg2.api_key);
    assert!(
        secrets.resolve(cfg1.api_key.as_ref().unwrap()).is_err(),
        "a config from before the restart no longer resolves"
    );
    assert_eq!(
        secrets
            .resolve(cfg2.api_key.as_ref().unwrap())
            .unwrap()
            .expose(),
        k2
    );
    let port = addr(&cfg2);
    assert_eq!(get(&port, "/v1/models", Some(&k1)).await.0, 401);
    assert_eq!(get(&port, "/v1/models", Some(&k2)).await.0, 200);

    // And after a stop and a new start.
    rt.stop(RuntimeRole::Chat).await;
    let cfg3 = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    let k3 = key(&rt, RuntimeRole::Chat);
    assert_ne!(k2, k3);
    assert_ne!(cfg2.api_key, cfg3.api_key);
    let port = addr(&cfg3);
    assert_eq!(get(&port, "/v1/models", Some(&k2)).await.0, 401);
    assert_eq!(get(&port, "/v1/models", Some(&k3)).await.0, 200);
    // The other role's key is untouched.
    rt.shutdown().await;
}

/// A caller that gives up on `ensure` (a timeout drops its future) must not kill a server that
/// is still loading, nor leave the status stuck: the start goes on, and the next `ensure`
/// gets the same server.
#[tokio::test]
async fn an_abandoned_ensure_does_not_cancel_the_start() {
    let e = env().await;
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &["--fake-start-delay-ms", "1500"],
    ));
    let r = tokio::time::timeout(
        Duration::from_millis(300),
        rt.ensure(RuntimeRole::Chat, &e.chat),
    )
    .await;
    assert!(r.is_err(), "the caller timed out");

    // Still starting, with a live server process behind it.
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.info.state, RuntimeState::Starting);
    let pid = d.pid.expect("the loading server keeps running");
    assert!(alive(pid));

    // Another impatient caller joins the same start instead of restarting it.
    let r = tokio::time::timeout(
        Duration::from_millis(200),
        rt.ensure(RuntimeRole::Chat, &e.chat),
    )
    .await;
    assert!(r.is_err());
    assert_eq!(rt.details(RuntimeRole::Chat).unwrap().pid, Some(pid));

    // It becomes ready with nobody waiting...
    wait_until("ready without a caller", Duration::from_secs(10), || {
        rt.details(RuntimeRole::Chat)
            .is_some_and(|d| d.info.state == RuntimeState::Ready)
    })
    .await;
    // ...and a later ensure returns it without restarting.
    let t = Instant::now();
    let cfg = rt.ensure(RuntimeRole::Chat, &e.chat).await.unwrap();
    assert!(
        t.elapsed() < Duration::from_millis(500),
        "{:?}",
        t.elapsed()
    );
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.pid, Some(pid), "same server");
    assert_eq!(d.config, Some(cfg));
    rt.shutdown().await;
}

/// A caller that waits for a start in progress gets its result.
#[tokio::test]
async fn concurrent_ensures_share_one_start() {
    let e = env().await;
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &["--fake-start-delay-ms", "500"],
    ));
    let (a, b) = tokio::join!(
        rt.ensure(RuntimeRole::Chat, &e.chat),
        rt.ensure(RuntimeRole::Chat, &e.chat)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(rt.details(RuntimeRole::Chat).unwrap().restarts, 0);
    rt.shutdown().await;
}

/// `stop` (and dropping the runtime) also ends a start nobody waits for any more.
#[tokio::test]
async fn stop_and_drop_end_a_detached_start() {
    let e = env().await;
    let rt = e.runtime(with_extra(
        RuntimeRole::Chat,
        &["--fake-start-delay-ms", "60000"],
    ));
    let waiter = tokio::spawn({
        let (rt, m) = (rt.clone(), e.chat.clone());
        async move { rt.ensure(RuntimeRole::Chat, &m).await }
    });
    wait_until("spawned", Duration::from_secs(5), || {
        rt.details(RuntimeRole::Chat)
            .is_some_and(|d| d.pid.is_some())
    })
    .await;
    let pid = rt.details(RuntimeRole::Chat).unwrap().pid.unwrap();
    rt.stop(RuntimeRole::Chat).await;
    assert!(!alive(pid));
    let err = waiter.await.unwrap().unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    let d = rt.details(RuntimeRole::Chat).unwrap();
    assert_eq!(d.info.state, RuntimeState::Stopped);
    assert_eq!(d.pid, None);

    // Abandoned, then the runtime is dropped: the loading server dies with it.
    let _ = tokio::time::timeout(
        Duration::from_millis(200),
        rt.ensure(RuntimeRole::Chat, &e.chat),
    )
    .await;
    let pid = rt.details(RuntimeRole::Chat).unwrap().pid.unwrap();
    drop(rt);
    wait_until("server killed on drop", Duration::from_secs(5), || {
        !alive(pid)
    })
    .await;
}
