//! Native transport, process and storage adapters for AutoRouter.

pub mod bounded_json;
pub mod classifier;
pub mod evaluator;
pub mod http_client;
pub mod keychain;
pub mod local_diagnostic;
pub mod ollama_setup;
pub mod policy;

pub mod response_observer;
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

pub mod tls_roots;
