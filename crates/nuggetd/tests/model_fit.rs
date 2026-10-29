//! `models.fit` through IPC: on a given machine, every manifest model is explained, and the
//! recommended set is exactly `models.list`'s.

use std::sync::Arc;
use std::time::Duration;

use nugget_core::ipc::{ModelInfo, method};
use nugget_core::paths::Paths;
use nugget_ingest::MemorySecrets;
use nugget_ipc::Client;
use nugget_models::{GIB, GpuKind, Hardware};
use nugget_store::StoreKey;
use nuggetd::{Daemon, DaemonOptions, RuntimeOverrides, Startup};
use serde_json::{Value, json};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn model_fit_explains_the_recommendation() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(tmp.path().to_path_buf());
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.config_file(), "[runtime]\nenabled = false\n").unwrap();
    let mut opts = DaemonOptions::new(paths.clone());
    opts.store_key = Some(StoreKey::from_bytes([9; 32]));
    opts.source_secrets = Some(Arc::new(MemorySecrets::new()));
    opts.runtime = RuntimeOverrides {
        hardware: Some(Hardware {
            total_ram: 8 * GIB * 95 / 100,
            available_ram: 4 * GIB,
            cpu_cores: 4,
            gpu: GpuKind::Unknown,
        }),
        ..RuntimeOverrides::default()
    };
    let daemon = match Daemon::start(opts).await.unwrap() {
        Startup::Ready(d) => d,
        Startup::AlreadyRunning { socket } => panic!("unexpected daemon on {socket}"),
    };
    let (tx, rx) = oneshot::channel::<()>();
    let server = tokio::spawn(daemon.run_until(async {
        let _ = rx.await;
    }));
    let client = Client::connect(&paths).await.unwrap();

    let fit: Value = tokio::time::timeout(WAIT, client.call("models.fit", &json!({})))
        .await
        .expect("models.fit timed out")
        .unwrap();
    assert_eq!(fit["ram_gb"], 8, "{fit}");
    assert_eq!(fit["hardware"]["gpu"], "unknown");
    assert_eq!(fit["hardware"]["cpu_cores"], 4);
    assert!(fit["free_disk_bytes"].is_u64(), "{fit}");
    let models = fit["models"].as_array().unwrap();
    let small = models.iter().find(|m| m["id"] == "qwen3-4b-q4km").unwrap();
    assert_eq!(small["recommended"], true);
    assert_eq!(small["accel"], "cpu");
    assert_eq!(
        small["reasons"],
        json!(["ram_fits_largest", "cpu_only_slower"])
    );
    assert!(small["fits_disk"].is_boolean());
    let large = models.iter().find(|m| m["id"] == "qwen3-8b-q4km").unwrap();
    assert_eq!(large["fits_ram"], false);
    assert_eq!(large["reasons"], json!(["ram_too_small"]));

    // `null` params work too, and the recommended set matches models.list.
    let again: Value = client.call("models.fit", &Value::Null).await.unwrap();
    let list: Value = client.call(method::MODELS_LIST, &json!({})).await.unwrap();
    let list: Vec<ModelInfo> = serde_json::from_value(list).unwrap();
    for info in list {
        let f = again["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == info.id.as_str())
            .unwrap();
        assert_eq!(f["recommended"], info.recommended, "{}", info.id);
    }

    drop(client);
    let _ = tx.send(());
    tokio::time::timeout(WAIT, server)
        .await
        .expect("daemon stopped")
        .unwrap()
        .unwrap();
}
