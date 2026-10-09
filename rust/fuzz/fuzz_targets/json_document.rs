#![no_main]

use autorouter_core::js_json::{JsDocument, JsNode, JsString};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 65_536 {
        return;
    }
    if let Ok(mut document) = JsDocument::parse(data) {
        // Canonical serialization must survive a second parse, including
        // duplicate keys, lone surrogates, overflow numbers and opaque depth.
        let before = document.stringify();
        let reparsed = JsDocument::parse(before.as_bytes()).unwrap();
        assert_eq!(before, reparsed.stringify());
        // A rejected edit must leave the observable document unchanged.
        assert!(
            document
                .set_root_field_json("synthetic_fuzz", b"[")
                .is_err()
        );
        assert_eq!(document.stringify(), before);
        let fields = match document.node(document.root()).unwrap() {
            JsNode::Object(object) => object
                .entries()
                .iter()
                .filter(|(key, _)| key != &JsString::from_scalar("synthetic_fuzz"))
                .map(|(key, value)| (key.clone(), document.stringify_node(*value)))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        if document.set_root_field_json("synthetic_fuzz", data).is_ok() {
            let inserted = document.get(document.root(), "synthetic_fuzz").unwrap();
            assert_eq!(document.stringify_node(inserted), before);
            let Some(JsNode::Object(object)) = document.node(document.root()) else {
                panic!("successful root-field edit must preserve object shape");
            };
            for (key, original) in fields {
                let (_, field) = object
                    .entries()
                    .iter()
                    .find(|(candidate, _)| candidate == &key)
                    .unwrap();
                assert_eq!(document.stringify_node(*field), original);
            }
            let changed = document.stringify();
            assert_eq!(
                changed,
                JsDocument::parse(changed.as_bytes()).unwrap().stringify()
            );
        }
    }
});
