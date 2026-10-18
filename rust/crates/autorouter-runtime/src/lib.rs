//! Native transport, process and storage adapters for AutoRouter.

pub mod async_observers;
pub mod bounded_json;
pub mod callback_privacy;
pub mod classifier;
pub mod evaluator;
pub mod http_client;
pub mod keychain;
pub mod local_diagnostic;
pub mod observer_tasks;
pub mod ollama_setup;
pub mod policy;

pub mod response_observer;
pub mod route_counter;
pub mod router;
pub mod server;
pub mod server_events;
pub mod session_history;
pub mod session_log;
pub mod status_cleanup;
pub mod status_settings;
pub mod status_store;
pub mod token_counter;
pub mod transport_completion;
pub mod user_config;

mod tls_policy;
pub mod tls_roots;

/// Fresh lowercase hex with the caller's original entropy width and error policy.
/// Filesystem naming, permissions, collision handling and cleanup stay local.
pub fn random_hex<const N: usize>() -> Result<String, getrandom::Error> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
pub(crate) fn bounded<F: std::future::Future>(
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    let deadline = tokio::time::timeout(std::time::Duration::from_secs(2), future);
    async move { deadline.await.unwrap() }
}

#[cfg(test)]
pub(crate) fn test_config(env: &serde_json::Value) -> autorouter_core::config::RouterConfig {
    autorouter_core::config::read_config(env, false, std::path::Path::new("/synthetic")).unwrap()
}

#[cfg(test)]
pub(crate) fn test_config_document(
    document: &autorouter_core::js_json::JsDocument,
) -> autorouter_core::config::RouterConfig {
    autorouter_core::config::read_config_document(
        document,
        false,
        std::path::Path::new("/synthetic"),
    )
    .unwrap()
}

mod http_receive;
