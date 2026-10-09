//! Pure AutoRouter policy and state transitions. No I/O or provider calls.

pub mod auth;
pub mod auto_routing;
pub mod config;
pub mod evaluation_report;
pub mod fixture;
pub mod js_json;
pub mod model_catalog;
pub mod model_request;
pub mod policy;
pub mod prompt_state;
pub mod redaction;
pub mod request_validation;
pub mod router;
pub mod savings;
pub mod session_history;
pub mod status_state;
pub mod statusline;
pub mod telemetry_event;
pub mod turn_state;
