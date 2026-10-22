//! Actual native route-metadata producer proof with bounded in-memory evaluator.
//! No socket, provider request, client login, shipping API or configuration edit.
use super::*;
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicUsize, Ordering};

struct LocalDecision {
    calls: AtomicUsize,
}
impl HttpTransport for LocalDecision {
    type ResponseBody = Full<Bytes>;
    async fn request(
        &self,
        request: Request<Full<Bytes>>,
    ) -> Result<Response<Self::ResponseBody>, HttpError> {
        assert_eq!(request.uri(), "https://api.typesafe.ai/v1/systemone");
        assert_eq!(request.method(), hyper::Method::POST);
        assert_eq!(
            request.headers()["authorization"],
            "Bearer synthetic-producer-key"
        );
        let bytes = request.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 32 * 1024);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let task = body["state"]["current_task"].as_str().unwrap();
        let mut expected = None;
        for (_, scenario) in cases().as_object().unwrap() {
            if let Some(index) = scenario["prompts"]
                .as_array()
                .unwrap()
                .iter()
                .position(|p| p == task)
            {
                let tier = scenario
                    .get("expectedClassifiedTiers")
                    .and_then(|v| v.get(index))
                    .or_else(|| scenario.get("expectedClassifiedTier"));
                if let Some(tier) = tier {
                    expected = Some(tier.clone());
                    break;
                }
            }
        }
        let tier =
            expected.expect("Only unchanged declared synthetic tasks may reach this transport");
        assert!(self.calls.fetch_add(1, Ordering::SeqCst) < 32);
        Ok(Response::new(Full::new(Bytes::from(
            json!({"answers":{"tier":{"choice":tier,"confidence":1}}}).to_string(),
        ))))
    }
}

#[tokio::test]
async fn actual_native_route_metadata_carries_frozen_prompt_indices_and_finite_timings() {
    let fixtures = cases();
    let names = parse_args(&[]).unwrap()["cases"]
        .as_array()
        .unwrap()
        .clone();
    for client_profile in ["compatible", "auto"] {
        for name in &names {
            let name = name.as_str().unwrap();
            let scenario = fixtures[name].clone();
            let config = read_config(&json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_CLIENT_PROFILE":client_profile,"AUTOROUTER_EVALUATOR":"jev","TYPESAFE_API_KEY":"synthetic-producer-key"}), false, Path::new("/synthetic")).unwrap();
            assert_eq!(config.upstream, "https://api.anthropic.com");
            let requested = if client_profile == "auto" {
                config.models.sonnet.clone()
            } else {
                config.models.haiku.clone()
            };
            let transport = Arc::new(LocalDecision {
                calls: AtomicUsize::new(0),
            });
            let routes = Arc::new(Mutex::new(Vec::new()));
            let router = LiveRouter {
                inner: Router::new(
                    Arc::new(ClassifierTransport {
                        native: transport.clone(),
                        outage: false,
                    }),
                    config,
                ),
                scenario: scenario.clone(),
                routes: routes.clone(),
            };
            let mut messages = Vec::new();
            for (index, prompt) in scenario["prompts"].as_array().unwrap().iter().enumerate() {
                messages.push(json!({"role":"user","content":prompt}));
                let body = json!({"model":requested,"messages":messages,"max_tokens":256,"thinking":{"type":"disabled"}});
                let row = router
                    .observe(
                        Arc::new(JsDocument::parse(body.to_string().as_bytes()).unwrap()),
                        RouteOptions {
                            scope: format!("synthetic/{name}"),
                            request_class: "main".into(),
                            prompt_id: format!("task-{index}"),
                            request_id: Some(format!("request-{index}")),
                            ..Default::default()
                        },
                        &HeaderMap::new(),
                        &CancellationToken::new(),
                        "",
                    )
                    .await
                    .unwrap();
                let observed = routes.lock().unwrap().last().unwrap().clone();
                assert_eq!(observed["scenario_prompt_index"], index);
                assert_eq!(
                    observed["expected_classified_tier"],
                    scenario
                        .get("expectedClassifiedTiers")
                        .and_then(|v| v.get(index))
                        .or_else(|| scenario.get("expectedClassifiedTier"))
                        .unwrap()
                        .clone()
                );
                assert_eq!(
                    observed["classified_tier"],
                    observed["expected_classified_tier"]
                );
                assert_eq!(observed["source"], "jev");
                assert!(crate::compare_live::original_route_shape(&observed));
                assert_eq!(observed["model"], row.model.to_well_formed());
                messages.push(json!({"role":"assistant","content":[{"type":"text","text":"Synthetic completed response."}]}));
            }
            router.inner.close().await;
            assert_eq!(
                routes.lock().unwrap().len(),
                scenario["prompts"].as_array().unwrap().len()
            );
            assert_eq!(
                transport.calls.load(Ordering::SeqCst),
                scenario["prompts"].as_array().unwrap().len()
            );
        }
    }
    // Account restrictions are checked by unchanged read_config before injection.
    assert!(read_config(&json!({"AUTOROUTER_AUTH_MODE":"subscription","AUTOROUTER_UPSTREAM_URL":"http://127.0.0.1:1"}),false,Path::new("/synthetic")).is_err());
}
