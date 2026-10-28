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

#[test]
fn enabled_flag_persists_and_survives_update() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key()];
    let installed = store
        .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
        .unwrap();
    assert!(installed.enabled);
    assert!(!store.set_enabled("dev.nugget.other", false).unwrap());

    assert!(store.set_enabled(ID, false).unwrap());
    assert!(!store.get(ID).unwrap().unwrap().enabled);
    // Persisted: a fresh handle on the same directory sees it.
    let reopened = crate::PluginStore::open_at(store.root().to_path_buf()).unwrap();
    assert!(!reopened.get(ID).unwrap().unwrap().enabled);
    assert!(!reopened.list().unwrap()[0].enabled);
    // Disabled plugins still load (the caller decides).
    reopened.load(ID).unwrap();

    let updated = store
        .update(&pkg("1.1.0", "", 1), &trusted, &mut yes)
        .unwrap();
    assert!(!updated.enabled, "update keeps the enabled flag");
    assert!(store.set_enabled(ID, true).unwrap());
    assert!(store.get(ID).unwrap().unwrap().enabled);

    // Reinstalling after removal starts enabled again.
    store.set_enabled(ID, false).unwrap();
    store.remove(ID).unwrap();
    assert!(
        store
            .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
            .unwrap()
            .enabled
    );
}

#[test]
fn legacy_index_without_enabled_loads_as_enabled() {
    let (_d, store) = temp_store();
    store
        .install(&pkg("1.0.0", "", 1), &[key(1).verifying_key()], &mut yes)
        .unwrap();
    let path = store.root().join("index.json");
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let rec = index["plugins"][ID].as_object_mut().unwrap();
    assert!(rec.remove("enabled").is_some());
    std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();

    let got = store.get(ID).unwrap().unwrap();
    assert!(got.enabled);
    store.load(ID).unwrap();
    // Writing the index again adds the flag.
    assert!(store.set_enabled(ID, false).unwrap());
    assert!(!store.get(ID).unwrap().unwrap().enabled);
}

#[test]
fn preview_reports_without_writing() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key(), key(2).verifying_key()];
    let index = store.root().join("index.json");

    // New install.
    let p = store
        .preview(&pkg("1.0.0", "hosts = [\"a.com\"]", 1), &trusted)
        .unwrap();
    assert_eq!(p.manifest.id, ID);
    assert_eq!(p.signer, key(1).verifying_key());
    assert!(p.installed_version.is_none());
    assert!(p.previously_approved.is_none());
    assert!(p.needs_approval);
    assert!(!index.exists(), "preview writes nothing");
    assert_eq!(
        std::fs::read_dir(store.root().join("blobs/sha256"))
            .unwrap()
            .count(),
        0
    );
    assert!(store.list().unwrap().is_empty());

    store
        .install(&pkg("1.0.0", "hosts = [\"a.com\"]", 1), &trusted, &mut yes)
        .unwrap();
    let before = std::fs::read(&index).unwrap();

    // Update within the approved capabilities.
    let p = store
        .preview(&pkg("1.1.0", "hosts = [\"a.com\"]", 1), &trusted)
        .unwrap();
    assert_eq!(p.installed_version, Some(semver::Version::new(1, 0, 0)));
    assert_eq!(p.previously_approved.unwrap().hosts, ["a.com"]);
    assert!(!p.needs_approval);

    // Update asking for more.
    let p = store
        .preview(&pkg("1.1.0", "hosts = [\"a.com\", \"b.com\"]", 1), &trusted)
        .unwrap();
    assert!(p.needs_approval);
    assert_eq!(p.manifest.capabilities.hosts.len(), 2);

    // Same errors as update.
    assert!(matches!(
        store.preview(&pkg("1.0.0", "", 1), &trusted),
        Err(nugget_core::Error::Invalid(_))
    ));
    assert!(matches!(
        store.preview(&pkg("0.9.0", "", 1), &trusted),
        Err(nugget_core::Error::Invalid(_))
    ));
    assert!(matches!(
        store.preview(&pkg("2.0.0", "", 2), &trusted),
        Err(nugget_core::Error::Integrity(_))
    ));
    // And as install for an untrusted signer or a tampered package.
    assert!(store.preview(&pkg("2.0.0", "", 3), &trusted).is_err());
    let mut tampered = pkg("2.0.0", "", 1);
    tampered.wasm.push(0);
    assert!(store.preview(&tampered, &trusted).is_err());

    assert_eq!(
        std::fs::read(&index).unwrap(),
        before,
        "preview writes nothing"
    );
    assert_eq!(
        store.get(ID).unwrap().unwrap().manifest.version.to_string(),
        "1.0.0"
    );
}

/// Rewrite the index record of `ID` with `edit`.
fn edit_record(store: &crate::PluginStore, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = store.root().join("index.json");
    let mut index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut index["plugins"][ID]);
    std::fs::write(&path, serde_json::to_vec(&index).unwrap()).unwrap();
}

fn integrity<T: std::fmt::Debug>(r: nugget_core::Result<T>) -> String {
    match r {
        Err(nugget_core::Error::Integrity(m)) => m,
        other => panic!("expected an integrity error, got {other:?}"),
    }
}

#[test]
fn load_trusted_checks_the_trusted_keys() {
    let (_d, store) = temp_store();
    let k1 = key(1).verifying_key();
    let k2 = key(2).verifying_key();
    store
        .install(&pkg("1.0.0", "", 1), &[k1, k2], &mut yes)
        .unwrap();
    assert_eq!(
        store.load_trusted(ID, &[k2, k1]).unwrap().wasm,
        EMPTY_COMPONENT
    );
    // The signing key was later removed from the trusted list.
    integrity(store.load_trusted(ID, &[k2]));
    integrity(store.load_trusted(ID, &[]));
    assert!(matches!(
        store.load_trusted("dev.nugget.other", &[k1]),
        Err(nugget_core::Error::NotFound(_))
    ));
}

#[test]
fn edited_signer_in_the_index_is_refused() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key(), key(2).verifying_key()];
    store
        .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
        .unwrap();
    // Point the record at another trusted key, with a signature by that key over the same files.
    let forged = pkg("1.0.0", "", 2);
    edit_record(&store, |rec| {
        rec["signer"] = hex::encode(key(2).verifying_key().as_bytes()).into();
        rec["signature"] = String::from_utf8(forged.signature.clone()).unwrap().into();
    });
    integrity(store.load_trusted(ID, &trusted));
    integrity(store.load(ID));
    // Nor can it be used to take over updates.
    integrity(store.update(&pkg("1.1.0", "", 2), &trusted, &mut yes));
    integrity(store.preview(&pkg("1.1.0", "", 2), &trusted));
}

#[test]
fn edited_approved_capabilities_are_refused() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key()];
    store
        .install(&pkg("1.0.0", "hosts = [\"a.com\"]", 1), &trusted, &mut yes)
        .unwrap();
    edit_record(&store, |rec| {
        rec["approved"]["hosts"] = serde_json::json!(["a.com", "evil.com"]);
    });
    integrity(store.load_trusted(ID, &trusted));
    // An update must not skip the prompt on the strength of the edited approval.
    integrity(store.update(
        &pkg("1.1.0", "hosts = [\"a.com\", \"evil.com\"]", 1),
        &trusted,
        &mut |_| panic!("must not ask on a tampered record"),
    ));
    // Removing and installing again re-approves.
    assert!(store.remove(ID).unwrap());
    store
        .install(&pkg("1.0.0", "hosts = [\"a.com\"]", 1), &trusted, &mut yes)
        .unwrap();
    store.load_trusted(ID, &trusted).unwrap();
}

#[test]
fn records_without_a_seal_fail_closed() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key()];
    store
        .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
        .unwrap();
    edit_record(&store, |rec| {
        assert!(
            rec.as_object_mut()
                .unwrap()
                .remove("approval_seal")
                .is_some()
        );
    });
    let msg = integrity(store.load_trusted(ID, &trusted));
    assert!(msg.contains("re-approve the plugin"), "{msg}");
    integrity(store.update(&pkg("1.1.0", "", 1), &trusted, &mut yes));
    // Still listed, and removable.
    assert_eq!(store.list().unwrap().len(), 1);
    assert!(store.remove(ID).unwrap());
}

#[test]
fn the_approval_key_is_per_store() {
    let (_d, store) = temp_store();
    let trusted = [key(1).verifying_key()];
    store
        .install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
        .unwrap();
    let key_file = store.root().join("approval.key");
    assert_eq!(std::fs::read(&key_file).unwrap().len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Reopening reuses the key.
    let reopened = crate::PluginStore::open_at(store.root().to_path_buf()).unwrap();
    reopened.load_trusted(ID, &trusted).unwrap();
    // Another key does not validate the seals.
    let other = crate::PluginStore::open_with_key(store.root().to_path_buf(), [9; 32]).unwrap();
    integrity(other.load_trusted(ID, &trusted));
    // A caller-supplied key works end to end and writes no key file.
    let dir = tempfile::tempdir().unwrap();
    let own = crate::PluginStore::open_with_key(dir.path().join("p"), [7; 32]).unwrap();
    own.install(&pkg("1.0.0", "", 1), &trusted, &mut yes)
        .unwrap();
    own.load_trusted(ID, &trusted).unwrap();
    assert!(!dir.path().join("p/approval.key").exists());
}
