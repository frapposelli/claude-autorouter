use super::*;

fn source() -> &'static str {
    "git+https://github.com/frapposelli/claude-autorouter.git?rev=2286fd91d8165b4b56b273c2ebe5fb4f462ed672#2286fd91d8165b4b56b273c2ebe5fb4f462ed672"
}
fn lock() -> String {
    format!(
        "version = 4\n\n[[package]]\nname = \"nix\"\nversion = \"0.31.3\"\nsource = {REGISTRY:?}\nchecksum = {ARCHIVE:?}\n\n[[package]]\nname = \"nix\"\nversion = \"0.31.3\"\nsource = {:?}\n",
        source()
    )
}
fn fixture(root: &Path) -> Value {
    let mut packages = vec![
        json!({"id":GIT_ID,"name":"nix","version":"0.31.3","source":source(),"manifest_path":"/fixture/git/rust/vendor/nix/Cargo.toml"}),
        json!({"id":REGISTRY_ID,"name":"nix","version":"0.31.3","source":REGISTRY,"manifest_path":"/fixture/registry/nix-0.31.3/Cargo.toml"}),
    ];
    let mut nodes = vec![
        json!({"id":GIT_ID,"features":["event","feature","fs","poll","process","signal","term","user"]}),
        json!({"id":REGISTRY_ID,"features":["event","feature","fs","poll","process","resource","signal","user"]}),
    ];
    for (name, directory, id, supplier) in [
        (
            "autorouter-runtime",
            "crates/autorouter-runtime",
            "runtime-owner",
            GIT_ID,
        ),
        (
            "claude-autorouter",
            "crates/claude-autorouter",
            "cli-owner",
            GIT_ID,
        ),
        ("xtask", "xtask", "tool-owner", REGISTRY_ID),
    ] {
        packages.push(json!({"name":name,"id":id,"source":null,"manifest_path":root.join("rust").join(directory).join("Cargo.toml")}));
        nodes.push(json!({"id":id,"deps":[{"name":"nix","pkg":supplier,"dep_kinds":[{"kind":null,"target":null}]}]}));
    }
    json!({"version":1,"workspace_root":root.join("rust"),"packages":packages,"resolve":{"nodes":nodes}})
}
fn artifact(suppliers: &Suppliers, role: Role) -> Value {
    let package = match role {
        Role::Production => &suppliers.production,
        Role::Bootstrap => &suppliers.bootstrap,
    };
    let features = match role {
        Role::Production => vec!["feature", "fs", "process", "signal", "term", "user"],
        Role::Bootstrap => vec![
            "event", "feature", "fs", "poll", "process", "resource", "signal", "user",
        ],
    };
    json!({"reason":"compiler-artifact","package_id":package.id,"manifest_path":package.manifest,"target":{"name":"nix","kind":["lib"],"crate_types":["lib"],"edition":"2021","src_path":package.manifest.parent().unwrap().join("src/lib.rs")},"features":features,"profile":{"test":false,"opt_level":"3","debug_assertions":false}})
}
fn encode(rows: &[Value]) -> Vec<u8> {
    rows.iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn actual_source_ids_remain_distinct_and_each_first_party_edge_is_required() {
    let root = Path::new("/fixture");
    let original = fixture(root);
    let suppliers = admit(&original, &lock(), source(), root).unwrap();
    assert_ne!(suppliers.production.id, suppliers.bootstrap.id);
    assert_ne!(suppliers.production.source, suppliers.bootstrap.source);
    assert_eq!(suppliers.vendor(), Path::new("/fixture/git/rust/vendor"));
    for index in 2..5 {
        for kind in ["swap", "missing", "duplicate", "only-dev", "conditional"] {
            let mut changed = original.clone();
            let edges = changed["resolve"]["nodes"][index]["deps"]
                .as_array_mut()
                .unwrap();
            match kind {
                "swap" => edges[0]["pkg"] = json!(if index == 4 { GIT_ID } else { REGISTRY_ID }),
                "missing" => edges.clear(),
                "duplicate" => edges.push(edges[0].clone()),
                "only-dev" => edges[0]["dep_kinds"][0]["kind"] = json!("dev"),
                "conditional" => edges[0]["dep_kinds"][0]["target"] = json!("cfg(FALSE)"),
                _ => unreachable!(),
            }
            assert!(
                admit(&changed, &lock(), source(), root).is_err(),
                "{index}:{kind}"
            );
        }
    }
}

#[test]
fn altered_duplicated_missing_or_unreviewed_metadata_and_lock_sources_fail_privately() {
    let root = Path::new("/fixture");
    let original = fixture(root);
    for (index, field, value) in [
        (0, "id", json!("PRIVATE_FOREIGN_PACKAGE")),
        (0, "version", json!("0.31.2")),
        (0, "source", json!("git+https://PRIVATE_FOREIGN_REPO#wrong")),
        (0, "manifest_path", json!("/PRIVATE_PATH/Cargo.toml")),
        (1, "source", json!(source())),
        (1, "source", json!(true)),
        (1, "version", json!("0.32.0")),
    ] {
        let mut changed = original.clone();
        changed["packages"][index][field] = value;
        let error = admit(&changed, &lock(), source(), root).err().unwrap();
        assert!(!error.contains("PRIVATE_"));
    }
    for section in ["packages", "nodes"] {
        for duplicate in [false, true] {
            let mut changed = original.clone();
            let values = if section == "packages" {
                changed[section].as_array_mut().unwrap()
            } else {
                changed["resolve"][section].as_array_mut().unwrap()
            };
            if duplicate {
                values.push(values[0].clone());
            } else {
                values.remove(0);
            }
            assert!(admit(&changed, &lock(), source(), root).is_err());
        }
    }
    let mut third = original.clone();
    let mut package = third["packages"][0].clone();
    package["id"] = json!("PRIVATE_THIRD_SUPPLIER");
    third["packages"].as_array_mut().unwrap().push(package);
    assert!(admit(&third, &lock(), source(), root).is_err());
    let mut features = original.clone();
    features["resolve"]["nodes"][1]["features"]
        .as_array_mut()
        .unwrap()
        .push(json!("term"));
    assert!(admit(&features, &lock(), source(), root).is_err());
    for changed in [
        lock().replace(ARCHIVE, &"0".repeat(64)),
        lock().replace("0.31.3", "0.31.2"),
        lock().replace(source(), "PRIVATE_FOREIGN_SOURCE"),
        lock() + &lock(),
        lock()
            .split("[[package]]")
            .take(2)
            .collect::<Vec<_>>()
            .join("[[package]]"),
        lock().replace("name = \"nix\"", "name = 'nix'"),
    ] {
        let error = admit(&original, &changed, source(), root).err().unwrap();
        assert!(!error.contains("PRIVATE_"));
    }
}

#[test]
fn production_and_bootstrap_artifacts_reject_opposite_suppliers_and_feature_unification() {
    let root = Path::new("/fixture");
    let suppliers = admit(&fixture(root), &lock(), source(), root).unwrap();
    for role in [Role::Production, Role::Bootstrap] {
        let original = artifact(&suppliers, role);
        let accepted = suppliers
            .library(&encode(std::slice::from_ref(&original)), role)
            .unwrap();
        assert_eq!(accepted["package_id"], original["package_id"]);
        assert_eq!(accepted["manifest_path"], original["manifest_path"]);
        assert_eq!(accepted["features"], original["features"]);
        let mut extra = original.clone();
        extra["PRIVATE_EXTRA"] = json!("PRIVATE_MODEL_BODY");
        assert_eq!(
            suppliers.library(&encode(&[extra]), role).unwrap(),
            accepted
        );
        assert!(!accepted.to_string().contains("PRIVATE_"));
        assert!(suppliers.library(b"", role).is_err());
        assert!(
            suppliers
                .library(&encode(&[original.clone(), original.clone()]), role)
                .is_err()
        );
        let opposite = match role {
            Role::Production => Role::Bootstrap,
            Role::Bootstrap => Role::Production,
        };
        for custom_build in [false, true] {
            let mut changed = artifact(&suppliers, opposite);
            if custom_build {
                changed["target"] = json!({"name":"build-script-build","kind":["custom-build"]});
            }
            assert!(
                suppliers
                    .library(&encode(&[original.clone(), changed]), role)
                    .is_err()
            );
        }
        let mut changed = original.clone();
        changed["target"] = json!({"name":"build-script-build","kind":["custom-build"]});
        changed["features"] = json!(["term"]);
        assert!(
            suppliers
                .library(&encode(&[original.clone(), changed]), role)
                .is_err()
        );
        for (field, value) in [
            ("package_id", json!("PRIVATE_UNKNOWN_PACKAGE")),
            ("manifest_path", json!("/PRIVATE_PATH/Cargo.toml")),
            ("features", json!(["term"])),
            ("features", json!([true])),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            let error = suppliers.library(&encode(&[changed]), role).err().unwrap();
            assert!(!error.contains("PRIVATE_"));
        }
        let mut repeated = original.clone();
        let feature = repeated["features"][0].clone();
        repeated["features"].as_array_mut().unwrap().push(feature);
        assert!(suppliers.library(&encode(&[repeated]), role).is_err());
        let mut changed = original.clone();
        changed["target"]["kind"] = json!(["bin"]);
        assert!(suppliers.library(&encode(&[changed]), role).is_err());
    }
    let mut metadata = fixture(root);
    metadata["packages"].as_array_mut().unwrap().push(json!({"name":"foreign","id":"foreign-owner","source":REGISTRY,"manifest_path":"/fixture/foreign/Cargo.toml"}));
    let extended = admit(&metadata, &lock(), source(), root).unwrap();
    let mut foreign = artifact(&suppliers, Role::Production);
    foreign["package_id"] = json!("foreign-owner");
    foreign["manifest_path"] = json!("/fixture/foreign/Cargo.toml");
    assert!(
        extended
            .library(
                &encode(&[artifact(&suppliers, Role::Production), foreign]),
                Role::Production
            )
            .is_err()
    );
    for role in [Role::Production, Role::Bootstrap] {
        let original = artifact(&suppliers, role);
        for (field, value) in [
            ("name", json!("test")),
            ("kind", json!(["lib", "custom-build"])),
            ("crate_types", json!(["bin"])),
            ("edition", json!("2024")),
            ("src_path", json!("/PRIVATE_UNREVIEWED_SOURCE.rs")),
        ] {
            let mut changed = original.clone();
            changed["target"][field] = value;
            let error = suppliers
                .library(&encode(&[original.clone(), changed]), role)
                .err()
                .unwrap();
            assert!(!error.contains("PRIVATE_"));
        }
        let package = match role {
            Role::Production => &suppliers.production,
            Role::Bootstrap => &suppliers.bootstrap,
        };
        let mut script = original.clone();
        script["target"] = json!({"name":"build-script-build","kind":["custom-build"],"crate_types":["bin"],"edition":"2021","src_path":package.manifest.parent().unwrap().join("build.rs")});
        assert!(
            suppliers
                .library(&encode(&[script.clone(), original.clone()]), role)
                .is_ok()
        );
        script["target"]["src_path"] = json!("/PRIVATE_UNREVIEWED_BUILD.rs");
        assert!(
            suppliers
                .library(&encode(&[script, original]), role)
                .is_err()
        );
    }
    let original = artifact(&suppliers, Role::Production);
    for (field, value) in [
        ("test", json!(true)),
        ("opt_level", json!("0")),
        ("debug_assertions", json!(true)),
    ] {
        let mut changed = original.clone();
        changed["profile"][field] = value;
        assert!(
            suppliers
                .library(&encode(&[changed]), Role::Production)
                .is_err()
        );
    }
}

#[test]
fn every_byte_entry_enforces_metadata_admission_before_parsing_or_filesystem_reads() {
    let oversized = vec![b' '; METADATA_CAP + 1];
    assert_eq!(
        from_bytes(Path::new("/PRIVATE_NONEXISTENT_ROOT"), &oversized)
            .err()
            .unwrap(),
        "Supplier metadata exceeds admission limit"
    );
    assert_eq!(
        parse_metadata(&vec![b' '; METADATA_CAP]).unwrap_err(),
        "Invalid supplier metadata JSON"
    );
    assert_eq!(parse_metadata(b"{}").unwrap(), json!({}));
    assert_eq!(
        parse_metadata(b"PRIVATE_MALFORMED_JSON").unwrap_err(),
        "Invalid supplier metadata JSON"
    );
}
