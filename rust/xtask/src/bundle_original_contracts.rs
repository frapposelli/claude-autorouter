//! Source-only archive assertions. Native schema2/source-build prerequisites
//! are explicit adaptations. The combined executable integration separately
//! checks complete sources and dependency-free help in one artifact.
use super::*;
use crate::tool_process::Scratch;

#[test]
fn original_bundle_archive_hash_count_fixture_and_privacy_assertions_are_retained() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let scratch = Scratch::new("original-bundle-contract").unwrap();
    let archive = scratch.0.join("benchmark.tar.gz");
    let created = create(&root, &archive).unwrap();
    let bytes = fs::read(&archive).unwrap();
    let checksum = sha(&bytes);
    assert_eq!(created["sha256"], checksum);
    assert_eq!(
        fs::read_to_string(scratch.0.join("benchmark.tar.gz.sha256")).unwrap(),
        format!("{checksum}  benchmark.tar.gz\n")
    );
    let (files, _) = decode_root(&bytes, ROOT, SOURCE).unwrap().into_parts();
    // decode_root also checks every member's exact root prefix and refuses
    // absolute/parent traversal; inspect each decoded path independently.
    let forbidden = regex::Regex::new(
        r"(?:^|/)(?:\.env|\.git|node_modules|artifacts|config\.json|autorouter-session)",
    )
    .unwrap();
    assert!(files.keys().all(|path| safe(path) && !path.contains("..")));
    let forbidden_paths: Vec<_> = files
        .keys()
        .filter(|path| forbidden.is_match(path))
        .collect();
    // The native source-only format includes the complete reviewed upstream
    // vendor inventory. These three public upstream files are an explicit
    // format adaptation, not private checkout .git metadata. Keep the original
    // predicate strict for every other path and retain this exact exception set.
    assert_eq!(
        forbidden_paths,
        [
            "rust/vendor/hyper-util/.github/workflows/CI.yml",
            "rust/vendor/hyper-util/.github/workflows/rustdoc-preview.yml",
            "rust/vendor/hyper-util/.gitignore",
        ]
    );
    for path in forbidden_paths {
        assert!(HYPER_UTIL_FILES.contains(&path.strip_prefix("rust/vendor/hyper-util/").unwrap()));
    }
    let manifest: Value = serde_json::from_slice(&files["source-manifest.json"].bytes).unwrap();
    assert_eq!(manifest["schema_version"], 2); // versioned native source format
    assert_eq!(
        created["files"].as_u64().unwrap() as usize,
        manifest["files"].as_object().unwrap().len() + 1
    );
    for (path, hash) in manifest["files"].as_object().unwrap() {
        assert_eq!(sha(&files[path].bytes), hash.as_str().unwrap(), "{path}");
    }
    assert_eq!(
        manifest["fixture_sha256"],
        manifest["files"]["test/fixtures/ollama-routing.json"]
    );
    assert_eq!(manifest["live_calls_performed"], false);
    assert_eq!(manifest["private_material_included"], false);
    assert!(
        manifest["build_requires"]
            .as_str()
            .unwrap()
            .contains("locked dependencies")
    );
    assert_eq!(created["dependencies_bundled"], false);
    assert!(
        !files
            .keys()
            .any(|path| path.contains("target/") || path.ends_with(".rlib"))
    );
}
