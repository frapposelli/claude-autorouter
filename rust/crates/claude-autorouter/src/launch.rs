//! Whole-launch engine selection. Claude owns the terminal and permissions;
//! gateway/status/history resources remain owned until child exit and cleanup.
use autorouter_core::auth::{build_claude_env, client_profile_for_launch, conflicting_providers};
use autorouter_core::config::{
    AuthMode, ClientProfile, Evaluator, SessionLogMode, read_config_document, require_keys,
};
use autorouter_runtime::http_client::NativeHttpClient;
use autorouter_runtime::keychain::MacKeychain;
use autorouter_runtime::ollama_setup::{SetupOptions, setup_ollama};
use autorouter_runtime::server::{Gateway, GatewayHandle};
use autorouter_runtime::server_events::EventSinks;
use autorouter_runtime::session_log::{SessionLog, SessionLogOptions};
use autorouter_runtime::status_cleanup::{CleanupOptions, remove_stale_status_directories};
use autorouter_runtime::status_settings::add_status_line_settings;
use autorouter_runtime::status_store::{StatusOptions, StatusStore};
use autorouter_runtime::user_config::{ConfigContext, LoadOptions, load_user_config};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::ffi::{OsStr, OsString};
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::Arc;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

pub async fn run(
    claude: bool,
    args: &[OsString],
    context: &ConfigContext<'_>,
) -> Result<u8, String> {
    if !claude && !args.is_empty() {
        return Err("Usage: claude-autorouter serve".into());
    }
    let loaded = load_user_config(
        context,
        &LoadOptions::default(),
        &mut MacKeychain::default(),
    )
    .await?;
    let mut settings = loaded.env_document.clone();
    if claude && client_profile_for_launch(ClientProfile::Compatible, args) == ClientProfile::Auto {
        settings
            .set_root_field_json("AUTOROUTER_CLIENT_PROFILE", b"\"auto\"")
            .expect("scalar setting");
    }
    let mut config = read_config_document(&settings, false, context.cwd)?;
    require_keys(&config)?;
    let diagnostic = !claude
        || loaded
            .env
            .get(OsStr::new("AUTOROUTER_DEBUG"))
            .is_some_and(|value| value == "1");
    if claude {
        if config.auth_mode == AuthMode::Subscription && args.iter().any(|arg| arg == "--bare") {
            return Err("--bare disables Claude Code OAuth; omit it in subscription mode".into());
        }
        if let Some(key) = conflicting_providers(&loaded.env).first() {
            return Err(format!(
                "Unset {key}; this router supports the Anthropic Messages API"
            ));
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| "Could not create the local router credential.")?;
        config.local_token = Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
    }
    let mut interrupt =
        signal(SignalKind::interrupt()).map_err(|_| "Could not initialize the Claude launcher.")?;
    let mut terminate =
        signal(SignalKind::terminate()).map_err(|_| "Could not initialize the Claude launcher.")?;
    let transport = Arc::new(NativeHttpClient::new().map_err(|error| error.to_string())?);
    if config.evaluator == Evaluator::Ollama {
        eprintln!(
            "Preparing local Ollama evaluator ({}); {}…",
            config.ollama_model,
            crate::onboarding::ollama_deadline_text(config.ollama_timeout_ms)
        );
        let cancellation = CancellationToken::new();
        let options = SetupOptions::default();
        let mut progress = |_| {};
        let preparing = setup_ollama(
            transport.as_ref(),
            &config,
            &cancellation,
            &options,
            &mut progress,
        );
        tokio::pin!(preparing);
        let result = tokio::select! {
            result=&mut preparing=>result,
            _=interrupt.recv()=>{cancellation.cancel();let _=preparing.await;return Ok(130)},
            _=terminate.recv()=>{cancellation.cancel();let _=preparing.await;return Ok(143)},
        };
        if result.is_err() {
            eprintln!(
                "Ollama could not be prepared. Requests will use the conservative fallback while it is unavailable; run claude-autorouter doctor."
            );
        }
    }
    let mut status: Option<Arc<StatusStore>> = None;
    let mut history: Option<Arc<SessionLog>> = None;
    let mut gateway: Option<GatewayHandle> = None;
    let result=async {
        let mut forwarded=args.to_vec();
        if claude && loaded.env.get(OsStr::new("AUTOROUTER_STATUSLINE")).is_none_or(|value|value!="0") {
            remove_stale_status_directories(CleanupOptions::default()).await;
            let store=Arc::new(StatusStore::create(StatusOptions{baseline_model:Some(config.models.opus.clone()),..Default::default()}));
            let ready=store.ready().await;
            if let Some(path)=ready {
                let overlay=(|| {
                    let scalar_args:Option<Vec<_>>=args.iter().map(|arg|arg.to_str().map(str::to_owned)).collect();
                    let executable=std::env::current_exe().map_err(|_|"Cannot locate native status command".to_owned())?;
                    if executable.to_str().is_none(){return Err("Cannot encode native status command".into())}
                    add_status_line_settings(&scalar_args.ok_or("Cannot encode settings arguments")?,path.parent().ok_or("Missing status directory")?,context.cwd,&executable)
                })();
                match overlay {
                    Ok(args)=>{forwarded=args.into_iter().map(OsString::from).collect();status=Some(store)},
                    Err(_)=>{store.close().await;eprintln!("AutoRouter status line unavailable: could not safely prepare session settings. Passing your original settings to Claude.");}
                }
            }else{store.close().await;eprintln!("AutoRouter status line unavailable: could not create local status storage.");}
        }
        if let Some(directory)=&config.session_log_dir {history=Some(Arc::new(SessionLog::create(directory.into(),SessionLogOptions{include_prompts:config.session_log_mode==SessionLogMode::Prompts,warn:Arc::new(|message|eprintln!("{message}")),..Default::default()}).await));}
        let mut sinks=EventSinks::default();
        if diagnostic {sinks.log=Some(Arc::new(|event|eprintln!("{}",event.stringify())));}
        if let Some(status)=&status {let status=status.clone();sinks.status=Some(Arc::new(move|event|status.update(&event.to_serde_observation_lossy())));}
        if let Some(history)=&history {let history=history.clone();sinks.record=Some(Arc::new(move|event|{history.record(&event);}));}
        let server=Gateway::new(config.clone(),transport,sinks)?;let handle=server.listen(if claude{0}else{config.port}).await?;let base_url=format!("http://{}",handle.address);gateway=Some(handle);
        if diagnostic {eprintln!("AutoRouter listening on {base_url} ({} authentication)",if config.auth_mode==AuthMode::Subscription{"subscription"}else{"api-key"});}
        if diagnostic && claude && config.client_profile==ClientProfile::Compatible {eprintln!("AutoRouter uses Haiku-compatible requests with client thinking disabled; {} selects the upstream model.",if config.evaluator==Evaluator::Ollama{"Ollama"}else{"Jev"});}
        if !claude {tokio::select!{_=interrupt.recv()=>{},_=terminate.recv()=>{}}return Ok(0)}
        let mut env=build_claude_env(&config,&base_url,&loaded.env);env.remove(OsStr::new("AUTOROUTER_CONFIG"));env.remove(OsStr::new("AUTOROUTER_STATUS_FILE"));
        if let Some(path)=status.as_ref().and_then(|store|store.path()){env.insert("AUTOROUTER_STATUS_FILE".into(),path.into_os_string());}
        let mut child=tokio::process::Command::new("claude").args(&forwarded).env_clear().envs(&env).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).kill_on_drop(true).spawn().map_err(|_|"Could not launch Claude Code. Ensure `claude` is installed and on PATH.")?;
        loop {
            tokio::select! {
                result=child.wait()=>return result.map(|status|status.code().unwrap_or(match status.signal(){Some(2)=>130,Some(15)=>143,_=>1})as u8).map_err(|_|"Could not wait for Claude Code.".into()),
                _=interrupt.recv()=>if let Some(pid)=child.id(){let _=kill(Pid::from_raw(pid as i32),Signal::SIGINT);},
                _=terminate.recv()=>if let Some(pid)=child.id(){let _=kill(Pid::from_raw(pid as i32),Signal::SIGTERM);},
            }
        }
    }.await;
    // Stop ingress first, then drain accepted writes and remove overlays only
    // after the writer has finished. Every startup error follows this path.
    if let Some(handle) = gateway {
        handle.close().await;
    }
    tokio::join!(
        async {
            if let Some(store) = status {
                store.close().await;
            }
        },
        async {
            if let Some(log) = history {
                log.close().await;
            }
        }
    );
    result
}
