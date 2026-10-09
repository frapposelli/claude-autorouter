use std::ffi::{OsStr, OsString};
use std::process::{ExitCode, Stdio};

mod configuration;
mod doctor;
mod launch;
mod onboarding;
mod secret_input;
mod sessions;
mod statusline;

fn help(command: &str) -> &'static str {
    match command {
        "setup" => include_str!("help/setup.txt"),
        "doctor" => include_str!("help/doctor.txt"),
        "config" => include_str!("help/config.txt"),
        "serve" => include_str!("help/serve.txt"),
        "sessions" => include_str!("help/sessions.txt"),
        "claude" => include_str!("help/claude.txt"),
        _ => include_str!("help/help.txt"),
    }
}

fn main() -> ExitCode {
    // Frozen Node rejects these selector combinations before loading the app,
    // including help/status commands. This check parses options without loading
    // certificates or touching configuration, the Keychain, or the network.
    if let Some(message) = autorouter_runtime::tls_roots::startup_diagnostic(
        &std::env::var("NODE_OPTIONS").unwrap_or_default(),
    ) {
        let executable = std::env::args_os()
            .next()
            .unwrap_or_else(|| "claude-autorouter".into());
        eprintln!("{}: {message}", executable.to_string_lossy());
        return ExitCode::from(9);
    }
    // Panics from optional sinks may be caught by runtime adapters; the default
    // hook runs before catch_unwind and can otherwise print private payloads.
    std::panic::set_hook(Box::new(|_| {
        eprintln!("AutoRouter encountered an internal error.")
    }));
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next().unwrap_or_else(|| OsString::from("help"));
    let args: Vec<_> = arguments.collect();
    let command_text = command.to_string_lossy();
    if command == "statusline" {
        statusline::run();
        return ExitCode::SUCCESS;
    }
    if matches!(command_text.as_ref(), "--version" | "-v" | "version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if matches!(command_text.as_ref(), "help" | "--help" | "-h")
        || (matches!(
            command_text.as_ref(),
            "setup" | "doctor" | "serve" | "config" | "sessions"
        ) && args.iter().any(|arg| arg == "--help" || arg == "-h"))
    {
        let topic = if command == "help" {
            args.first().and_then(|v| v.to_str()).unwrap_or("help")
        } else {
            &command_text
        };
        print!("{}", help(topic));
        return ExitCode::SUCCESS;
    }
    if command == "claude"
        && args.len() == 1
        && ["--help", "-h", "--version", "-v"]
            .iter()
            .any(|flag| args[0] == OsStr::new(flag))
    {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                eprintln!("Could not initialize the Claude launcher.");
                return ExitCode::FAILURE;
            }
        };
        return runtime.block_on(passthrough(&args));
    }
    if command == "serve" || command == "claude" {
        let result = (|| {
            let env = std::env::vars_os().collect();
            let cwd =
                std::env::current_dir().map_err(|_| "Could not resolve the current directory.")?;
            let home = std::env::home_dir().ok_or("Could not resolve the home directory.")?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "Could not initialize AutoRouter.")?;
            runtime.block_on(launch::run(
                command == "claude",
                &args,
                &autorouter_runtime::user_config::ConfigContext {
                    env: &env,
                    cwd: &cwd,
                    home: &home,
                },
            ))
        })();
        return match result {
            Ok(code) => ExitCode::from(code),
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    if command == "config" || command == "setup" || command == "doctor" || command == "sessions" {
        let result = (|| {
            let env = std::env::vars_os().collect();
            let cwd =
                std::env::current_dir().map_err(|_| "Could not resolve the current directory.")?;
            let home = std::env::home_dir().ok_or("Could not resolve the home directory.")?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "Could not initialize AutoRouter.")?;
            let context = autorouter_runtime::user_config::ConfigContext {
                env: &env,
                cwd: &cwd,
                home: &home,
            };
            runtime.block_on(async {
                let mut keychain = autorouter_runtime::keychain::MacKeychain::default();
                if command == "sessions" {
                    return sessions::command(&args, &context, &mut keychain).await;
                }
                if command == "config" {
                    return configuration::command(&args, &context, &mut keychain).await;
                }
                let cancellation = tokio_util::sync::CancellationToken::new();
                let signal_task = cancel_on_signal(cancellation.clone())?;
                if command == "doctor" {
                    let evaluate_local = args.iter().any(|a| a == "--evaluate-local");
                    let json = args.iter().any(|a| a == "--json");
                    let unique: std::collections::HashSet<_> = args.iter().collect();
                    if args
                        .iter()
                        .any(|a| a != "--evaluate-local" && a != "--json")
                        || unique.len() != args.len()
                        || (json && !evaluate_local)
                    {
                        signal_task.abort();
                        return Err(
                            "Usage: claude-autorouter doctor [--evaluate-local [--json]]".into(),
                        );
                    }
                    let result = if evaluate_local {
                        doctor::evaluate_local(
                            &context,
                            &mut keychain,
                            &cancellation,
                            json,
                            &mut |line| println!("{line}"),
                        )
                        .await
                    } else {
                        doctor::doctor(&context, &mut keychain, &cancellation, &mut |line| {
                            println!("{line}")
                        })
                        .await
                    };
                    signal_task.abort();
                    return result.map(|success| configuration::CommandOutput {
                        success,
                        lines: Vec::new(),
                    });
                }
                let result =
                    onboarding::setup(&args, &context, &mut keychain, &cancellation, &mut |line| {
                        println!("{line}")
                    })
                    .await;
                signal_task.abort();
                result.map(|()| configuration::CommandOutput {
                    success: true,
                    lines: Vec::new(),
                })
            })
        })();
        return match result {
            Ok(output) => {
                for line in output.lines {
                    println!("{line}");
                }
                if output.success {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::FAILURE
            }
        };
    }
    eprintln!("Unknown command. Run claude-autorouter --help.");
    ExitCode::FAILURE
}

fn cancel_on_signal(
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<tokio::task::JoinHandle<()>, String> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())
        .map_err(|_| "Could not initialize cancellation handling.")?;
    let mut terminate = signal(SignalKind::terminate())
        .map_err(|_| "Could not initialize cancellation handling.")?;
    Ok(tokio::spawn(async move {
        tokio::select! {_=interrupt.recv()=>{},_=terminate.recv()=>{}}
        cancellation.cancel();
    }))
}

#[cfg(unix)]
async fn passthrough(args: &[OsString]) -> ExitCode {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use std::os::unix::process::ExitStatusExt;
    use tokio::signal::unix::{SignalKind, signal};

    // Subscribe before spawning so an early signal cannot orphan the child.
    let (Ok(mut interrupt), Ok(mut terminate)) = (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) else {
        eprintln!("Could not initialize the Claude launcher.");
        return ExitCode::FAILURE;
    };
    let mut child = match tokio::process::Command::new("claude")
        .args(args)
        .env_remove("TYPESAFE_API_KEY")
        .env_remove("AUTOROUTER_TOKEN")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            eprintln!("Could not launch Claude Code. Ensure `claude` is installed and on PATH.");
            return ExitCode::FAILURE;
        }
    };
    loop {
        tokio::select! {
            result = child.wait() => return match result {
                Ok(status) => ExitCode::from(status.code().unwrap_or(match status.signal() { Some(2) => 130, Some(15) => 143, _ => 1 }) as u8),
                Err(_) => ExitCode::FAILURE,
            },
            _ = interrupt.recv() => if let Some(id) = child.id() { let _ = kill(Pid::from_raw(id as i32), Signal::SIGINT); },
            _ = terminate.recv() => if let Some(id) = child.id() { let _ = kill(Pid::from_raw(id as i32), Signal::SIGTERM); },
        }
    }
}

#[cfg(not(unix))]
async fn passthrough(_: &[OsString]) -> ExitCode {
    eprintln!("AutoRouter supports macOS and Linux, including WSL.");
    ExitCode::FAILURE
}
