//! Standalone native client. Runtime session code has no Python dependency.
use isharescreen_protocol::auth;
#[cfg(target_os = "linux")]
mod audio;
#[cfg(target_os = "linux")]
mod extensions;
#[cfg(target_os = "linux")]
mod gpu;
#[cfg(target_os = "linux")]
mod transport;
#[cfg(target_os = "linux")]
mod viewer;
mod wire;
use serde_json::Value;
use std::io::{self, IsTerminal, Read};
use std::process::{Command, Stdio};
use std::time::Duration;
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn keyring_password(host: &str, user: &str, port: u16) -> Result<Zeroizing<String>> {
    let mut child = Command::new("secret-tool")
        .args([
            "lookup",
            "application",
            "isharescreen",
            "protocol",
            "rfb",
            "server",
            &host.to_lowercase().trim_end_matches('.'),
            "user",
            user,
            "port",
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("login keyring is locked or unavailable (lookup timed out)".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        return Err(
            "no keyring credential; use --password-stdin or save it once with the reference CLI"
                .into(),
        );
    }
    let mut bytes = Zeroizing::new(Vec::new());
    child
        .stdout
        .take()
        .ok_or("keyring lookup has no stdout")?
        .take(4097)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err("keyring credential exceeds size limit".into());
    }
    let password = Zeroizing::new(String::from_utf8(bytes.to_vec())?);
    Ok(Zeroizing::new(
        password.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "iss-native --last [--host HOST] [--user USER] [--port PORT] [--auth srp|nonsrp] [--password-stdin] [--background-fps 5] [--no-audio] [--reconnect] [--auto-quit-secs N]\n\nStandalone Linux VAAPI/EGL viewer. --auth-check and --negotiate-check\nperform a connection check and exit without opening a window."
        );
        return Ok(());
    }
    let mut last = false;
    let mut check = false;
    let mut negotiate = false;
    let mut stdin = false;
    let mut no_audio = false;
    let mut reconnect = false;
    let mut overrides = std::collections::HashMap::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--last" => last = true,
            "--auth-check" => check = true,
            "--negotiate-check" => negotiate = true,
            "--password-stdin" => stdin = true,
            "--no-audio" => no_audio = true,
            "--reconnect" => reconnect = true,
            "--host" | "--user" | "--port" | "--auth" | "--background-fps" | "--auto-quit-secs" => {
                let key = args[index].trim_start_matches("--").to_owned();
                index += 1;
                overrides.insert(key, args.get(index).ok_or("option needs a value")?.clone());
            }
            _ => return Err("unknown option; run iss-native --help".into()),
        }
        index += 1;
    }
    if check && negotiate {
        return Err("choose only one connection check".into());
    }
    let mut settings = if last {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .map(|path| path.join("iss"))
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".iss"))
            })
            .ok_or("no user state directory")?;
        serde_json::from_slice::<Value>(&std::fs::read(base.join("last.json"))?)?
    } else {
        serde_json::json!({})
    };
    for (key, value) in overrides {
        settings[&key] = Value::String(value);
    }
    let host = settings["host"].as_str().ok_or("missing --host")?;
    let user = settings["user"].as_str().ok_or("missing --user")?;
    let port: u16 = settings["port"]
        .as_u64()
        .map(|port| port.to_string())
        .or_else(|| settings["port"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "5900".into())
        .parse()?;
    let mode = settings["auth"].as_str().unwrap_or("srp");
    if !matches!(mode, "srp" | "nonsrp") {
        return Err("--auth must be srp or nonsrp".into());
    }
    let password = if stdin {
        if io::stdin().is_terminal() {
            return Err(
                "pipe a password into --password-stdin, or use the saved login-keyring credential"
                    .into(),
            );
        }
        let mut password = Zeroizing::new(String::new());
        io::stdin().read_line(&mut password)?;
        Zeroizing::new(password.trim_end_matches(['\r', '\n']).to_owned())
    } else {
        keyring_password(host, user, port)?
    };
    if password.is_empty() {
        return Err("empty password; authentication was not attempted".into());
    }
    let background: u32 = settings["background-fps"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| std::env::var("ISS_BACKGROUND_FPS").ok())
        .unwrap_or("5".into())
        .parse()?;
    if !(1..=60).contains(&background) {
        return Err("background FPS must be between 1 and 60".into());
    }
    let auto_quit: f64 = settings["auto-quit-secs"].as_str().unwrap_or("0").parse()?;
    if !auto_quit.is_finite() || auto_quit < 0.0 {
        return Err("auto-quit seconds must be finite and nonnegative".into());
    }
    let mut retry_delay = 1;
    loop {
        let outcome = (|| -> Result<()> {
            #[cfg(target_os = "linux")]
            let sockets = if !check && !negotiate {
                Some(transport::Sockets::open(host, port)?)
            } else {
                None
            };
            let warmup = auth::open(host, port)?;
            drop(warmup);
            std::thread::sleep(Duration::from_millis(1400));
            let (stream, key) = auth::connect(host, port, user, &password, mode == "srp")?;
            if !check {
                let scale = match settings["hidpi"].as_str().unwrap_or("2") {
                    "on" => 2.0,
                    "off" | "auto" => 1.0,
                    value => value.parse::<f64>()?,
                };
                let backing = settings["advertise"]
                    .as_str()
                    .filter(|value| *value != "auto")
                    .unwrap_or("3840x2160");
                let (width, height) = backing
                    .split_once('x')
                    .ok_or("invalid saved display geometry")?;
                let width = (width.parse::<u32>()? as f64 / scale).round() as u32;
                let height = (height.parse::<u32>()? as f64 / scale).round() as u32;
                let audio = !no_audio && !negotiate && settings["audio"].as_bool().unwrap_or(true);
                let result = wire::negotiate(
                    stream,
                    &*key,
                    width,
                    height,
                    scale,
                    settings["curtain"].as_bool().unwrap_or(true),
                    audio,
                )?;
                if negotiate {
                    println!(
                        "Native negotiation succeeded: {}x{} canvas (no Python process; no viewer opened)",
                        result.width, result.height
                    );
                } else {
                    #[cfg(target_os = "linux")]
                    {
                        viewer::run(result, sockets.unwrap(), background, auto_quit, audio)?;
                    }
                    #[cfg(not(target_os = "linux"))]
                    return Err("standalone GPU presentation currently requires Linux/VAAPI".into());
                }
            } else {
                println!(
                    "Native {mode} authentication succeeded; checkpoint complete (no Python process)"
                );
            }
            Ok(())
        })();
        match outcome {
            Err(error) if reconnect && !check && !negotiate => {
                eprintln!("Session ended: {error}; reconnecting in {retry_delay}s");
                std::thread::sleep(Duration::from_secs(retry_delay));
                retry_delay = (retry_delay * 2).min(8);
            }
            result => return result,
        }
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("iss-native: {error}");
        std::process::exit(1);
    }
}
