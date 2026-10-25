//! Native executable migration checkpoint. Auth-check opens no viewer and
//! performs no Python subprocess calls. Streaming/session UI follows next.
use isharescreen_protocol::auth;
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
            "iss-native (--auth-check | --negotiate-check) --last [--host HOST] [--user USER] [--port PORT] [--auth srp|nonsrp] [--password-stdin]\n\nNative migration checkpoints: authenticate or negotiate the media session\nwithout Python, then disconnect. The standalone viewer is still being ported."
        );
        return Ok(());
    }
    let mut last = false;
    let mut check = false;
    let mut negotiate = false;
    let mut stdin = false;
    let mut overrides = std::collections::HashMap::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--last" => last = true,
            "--auth-check" => check = true,
            "--negotiate-check" => negotiate = true,
            "--password-stdin" => stdin = true,
            "--host" | "--user" | "--port" | "--auth" => {
                let key = args[index].trim_start_matches("--").to_owned();
                index += 1;
                overrides.insert(key, args.get(index).ok_or("option needs a value")?.clone());
            }
            _ => return Err("unknown option; run iss-native --help".into()),
        }
        index += 1;
    }
    if check == negotiate {
        return Err("choose --auth-check or --negotiate-check; standalone presentation is still being ported".into());
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
    let warmup = auth::open(host, port)?;
    drop(warmup);
    std::thread::sleep(Duration::from_millis(1400));
    let (stream, key) = auth::connect(host, port, user, &password, mode == "srp")?;
    if negotiate {
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
        let result = wire::negotiate(
            stream,
            &*key,
            width,
            height,
            scale,
            settings["curtain"].as_bool().unwrap_or(true),
            false,
        )?;
        println!(
            "Native negotiation succeeded: {}x{} canvas (no Python process; no viewer opened)",
            result.width, result.height
        );
    } else {
        println!("Native {mode} authentication succeeded; checkpoint complete (no Python process)");
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("iss-native: {error}");
        std::process::exit(1);
    }
}
