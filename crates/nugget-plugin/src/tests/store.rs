use super::{EMPTY_COMPONENT, key, manifest_toml, package, temp_store};
use crate::ApprovalRequest;

const ID: &str = "dev.nugget.test";

fn yes(_: &ApprovalRequest<'_>) -> bool {
    true
}

fn pkg(version: &str, caps: &str, seed: u8) -> crate::PluginPackage {
    package(
        &manifest_toml(ID, version, "[\"tool\"]", caps),
        EMPTY_COMPONENT,
        &key(seed),
    )
}

#[test]
fn install_list_get_load() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key()];
    let mut asked = Vec::new();
    let installed = store
        .install(
            &pkg("1.0.0", "hosts = [\"example.com\"]", 1),
            &trusted,
            &mut |r| {
                asked.push((r.manifest.id.clone(), r.previously_approved.is_none()));
                true
            },
        )
        .unwrap();
    assert_eq!(asked, [(ID.to_string(), true)]);
    assert_eq!(installed.approved.hosts, ["example.com"]);
    assert_eq!(store.list().unwrap(), std::slice::from_ref(&installed));
    assert_eq!(store.get(ID).unwrap(), Some(installed));
    assert_eq!(store.load(ID).unwrap().wasm, EMPTY_COMPONENT);
    assert!(store.get("dev.nugget.other").unwrap().is_none());
    // Installing twice is an error: updates go through `update`.
    assert!(
        store
            .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
            .is_err()
    );
}

#[test]
fn declined_approval_installs_nothing() {
    let (_d, store) = temp_store();
    let err = store
        .install(&pkg("1.0.0", "", 1), &[key(1).verifying_key()], &mut |_| {
            false
        })
        .unwrap_err();
    assert!(matches!(err, nugget_core::Error::Unauthorized(_)));
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn bad_signature_installs_nothing() {
    let (_d, store) = temp_store();
    let mut p = pkg("1.0.0", "", 1);
    p.wasm.push(1);
    let mut called = false;
    assert!(
        store
            .install(&p, &[key(1).verifying_key()], &mut |_| {
                called = true;
                true
            })
            .is_err()
    );
    assert!(
        !called,
        "the user is never asked about an unverified package"
    );
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn update_rules() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key(), key(2).verifying_key()];
    store
        .install(
            &pkg("1.0.0", "hosts = [\"a.com\"]\nkv_max_bytes = 100", 1),
            &trusted,
            &mut yes,
        )
        .unwrap();

    // Not newer.
    assert!(
        store
            .update(&pkg("1.0.0", "", 1), &trusted, &mut yes)
            .is_err()
    );
    assert!(
        store
            .update(&pkg("0.9.0", "", 1), &trusted, &mut yes)
            .is_err()
    );
    // Another (even trusted) key cannot take over an installed plugin.
    let err = store
        .update(&pkg("1.1.0", "", 2), &trusted, &mut yes)
        .unwrap_err();
    assert!(matches!(err, nugget_core::Error::Integrity(_)));

    // Same or narrower capabilities: no prompt.
    let updated = store
        .update(
            &pkg("1.1.0", "hosts = [\"a.com\"]", 1),
            &trusted,
            &mut |_| panic!("should not ask"),
        )
        .unwrap();
    assert_eq!(updated.manifest.version.to_string(), "1.1.0");
    assert_eq!(updated.approved.kv_max_bytes, 0);

    // Wider capabilities: asked again, with what was approved before.
    let wider = pkg("1.2.0", "hosts = [\"a.com\", \"b.com\"]", 1);
    assert!(store.update(&wider, &trusted, &mut |_| false).is_err());
    assert_eq!(
        store.get(ID).unwrap().unwrap().manifest.version.to_string(),
        "1.1.0"
    );
    let mut prev = None;
    store
        .update(&wider, &trusted, &mut |r| {
            prev = r.previously_approved.cloned();
            true
        })
        .unwrap();
    assert_eq!(prev.unwrap().hosts, ["a.com"]);
    assert_eq!(store.get(ID).unwrap().unwrap().approved.hosts.len(), 2);

    // Updating something that is not installed.
    let other = package(
        &manifest_toml("dev.nugget.other", "1.0.0", "[\"tool\"]", ""),
        EMPTY_COMPONENT,
        &key(1),
    );
    assert!(matches!(
        store.update(&other, &trusted, &mut yes),
        Err(nugget_core::Error::NotFound(_))
    ));
}

#[test]
fn remove_deletes_state_and_unreferenced_blobs() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key()];
    store
        .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
        .unwrap();
    store
        .update(&pkg("1.1.0", "", 1), &trusted, &mut yes)
        .unwrap();
    let blobs = || {
        std::fs::read_dir(store.root().join("blobs/sha256"))
            .unwrap()
            .count()
    };
    // One component (unchanged bytes) and the current manifest: the old manifest was collected.
    assert_eq!(blobs(), 2);
    let state = store.state_root().join(ID);
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("tools.json"), "{}").unwrap();

    assert!(store.remove(ID).unwrap());
    assert!(!store.remove(ID).unwrap());
    assert!(store.list().unwrap().is_empty());
    assert!(!state.exists());
    assert_eq!(blobs(), 0);
}

#[test]
fn tampering_on_disk_is_detected_at_load() {
    let (_d, store) = temp_store();
    let installed = store
        .install(&pkg("1.0.0", "", 1), &[key(1).verifying_key()], &mut yes)
        .unwrap();
    let blob = store
        .root()
        .join("blobs/sha256")
        .join(&installed.wasm_sha256);
    let mut bytes = std::fs::read(&blob).unwrap();
    bytes.push(0);
    std::fs::write(&blob, bytes).unwrap();
    assert!(matches!(
        store.load(ID),
        Err(nugget_core::Error::Integrity(_))
    ));
}
