use crate::{
    connection::{self, Connection},
    vault,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::{fs, path::Path, process::Stdio};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    process::Command,
    time::Duration,
};

#[cfg(target_os = "macos")]
pub const LABEL: &str = "app.localfiles.tunnel";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Health {
    pub live: bool,
    pub ready: bool,
    pub connected: bool,
    pub last_poll_unix: f64,
    pub message: String,
}

// Loopback-only, bounded HTTP. No TLS stack, shell, or child process for periodic checks.
fn address(folders: &Path) -> Result<SocketAddr> {
    let url = fs::read_to_string(connection::path(folders, "health.url")?)?;
    let addr: SocketAddr = url
        .trim()
        .trim_end_matches('/')
        .strip_prefix("http://")
        .context("Invalid health URL")?
        .parse()?;
    anyhow::ensure!(
        addr.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) && addr.port() != 0,
        "Health endpoint must be on localhost"
    );
    Ok(addr)
}
fn get(folders: &Path, path: &str) -> Result<(bool, String)> {
    let address = address(folders)?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(600))?;
    stream.set_read_timeout(Some(Duration::from_millis(600)))?;
    stream.set_write_timeout(Some(Duration::from_millis(600)))?;
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:8080\r\nConnection: close\r\n\r\n"
    )?;
    let mut output = String::new();
    stream.take(512 * 1024).read_to_string(&mut output)?;
    let ok = output
        .lines()
        .next()
        .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"));
    Ok((
        ok,
        output
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .unwrap_or("")
            .into(),
    ))
}
pub fn health(folders: &Path) -> Health {
    match Connection::load(folders) {
        Ok(Some(_)) => {}
        value => {
            return Health {
                live: false,
                ready: false,
                connected: false,
                last_poll_unix: 0.0,
                message: if value.is_err() {
                    "Не удалось прочитать настройки подключения"
                } else {
                    "Подключение к ChatGPT не настроено"
                }
                .into(),
            };
        }
    }
    let live = get(folders, "/healthz").is_ok_and(|(ok, _)| ok);
    if !live {
        return Health {
            live: false,
            ready: false,
            connected: false,
            last_poll_unix: 0.0,
            message: "Туннель остановлен".into(),
        };
    }
    let (ready, detail) = get(folders, "/readyz").unwrap_or_default();
    let metrics = get(folders, "/metrics")
        .map(|(_, body)| body)
        .unwrap_or_default();
    let last_poll_unix = last_success(&metrics);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let connected = ready && fresh(last_poll_unix, now);
    Health {
        live,
        ready,
        connected,
        last_poll_unix,
        message: if !ready {
            format!(
                "Сервер ещё не готов: {}",
                detail.trim().chars().take(120).collect::<String>()
            )
        } else if !connected {
            "Ожидаем ответ OpenAI".into()
        } else {
            "Соединение установлено".into()
        },
    }
}
fn last_success(metrics: &str) -> f64 {
    metrics
        .lines()
        .filter(|line| {
            line.starts_with("commands_poll_last_successful_timestamp_seconds ")
                || line.starts_with("commands_poll_last_successful_timestamp_seconds{")
        })
        .filter_map(|line| line.split_whitespace().nth(1)?.parse::<f64>().ok())
        .fold(0.0, f64::max)
}
fn fresh(last: f64, now: f64) -> bool {
    last > 0.0 && last.is_finite() && (-30.0..=90.0).contains(&(now - last))
}
#[cfg(target_os = "macos")]
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
pub fn register(label: &str, args: Vec<String>, folders: &Path, keep_alive: bool) -> Result<()> {
    let logs = connection::path(folders, "logs")?;
    fs::create_dir_all(&logs)?;
    let mut values = plist::Dictionary::new();
    values.insert("Label".into(), label.into());
    values.insert(
        "ProgramArguments".into(),
        plist::Value::Array(args.into_iter().map(Into::into).collect()),
    );
    values.insert("RunAtLoad".into(), true.into());
    values.insert("KeepAlive".into(), keep_alive.into());
    values.insert("ThrottleInterval".into(), 15i64.into());
    values.insert("LimitLoadToSessionType".into(), "Aqua".into());
    values.insert(
        "ProcessType".into(),
        if keep_alive {
            "Background"
        } else {
            "Interactive"
        }
        .into(),
    );
    values.insert(
        "StandardOutPath".into(),
        logs.join(format!("{label}.stdout.log"))
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    values.insert(
        "StandardErrorPath".into(),
        logs.join(format!("{label}.stderr.log"))
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    let target = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?)
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    let mut bytes = Vec::new();
    plist::Value::Dictionary(values).to_writer_xml(&mut bytes)?;
    connection::atomic_write(&target, &bytes)?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn control(action: &str, folders: &Path) -> Result<()> {
    let domain = domain();
    let target = format!("{domain}/{LABEL}");
    let loaded = Command::new("/bin/launchctl")
        .args(["print", &target])
        .output()?
        .status
        .success();
    if action == "stop" {
        if loaded {
            checked(
                Command::new("/bin/launchctl").args(["bootout", &target]),
                "Остановить туннель",
            )?;
        }
        let _ = fs::remove_file(connection::path(folders, "health.url")?);
        return Ok(());
    }
    anyhow::ensure!(
        matches!(action, "start" | "restart"),
        "Unknown service action"
    );
    anyhow::ensure!(
        Connection::load(folders)?.is_some(),
        "Сначала настройте подключение к ChatGPT"
    );
    connection::tunnel_binary()?;
    let exe = std::env::current_exe()?;
    register(
        LABEL,
        vec![
            exe.to_string_lossy().into_owned(),
            "background".into(),
            "--config".into(),
            folders.to_string_lossy().into_owned(),
        ],
        folders,
        true,
    )?;
    if !loaded {
        let plist = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?)
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        checked(
            Command::new("/bin/launchctl")
                .arg("bootstrap")
                .arg(&domain)
                .arg(plist),
            "Запустить туннель",
        )?;
    } else {
        let mut command = Command::new("/bin/launchctl");
        command.arg("kickstart");
        if action == "restart" {
            command.arg("-k");
        }
        checked(command.arg(target), "Запустить туннель")?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn control(action: &str, folders: &Path) -> Result<()> {
    let task = "Local Files Tunnel";
    if action == "stop" || action == "restart" {
        let _ = Command::new("schtasks")
            .args(["/End", "/TN", task])
            .output();
        let _ = fs::remove_file(connection::path(folders, "health.url")?);
        if action == "stop" {
            return Ok(());
        }
    }
    anyhow::ensure!(
        Connection::load(folders)?.is_some(),
        "Сначала настройте подключение к ChatGPT"
    );
    connection::tunnel_binary()?;
    // `/SC ONLOGON` means "any user" and needs administrator rights; a logon
    // trigger bound to the current user can be registered from XML without them.
    let dir = tempfile::tempdir()?;
    let xml = dir.path().join("task.xml");
    fs::write(
        &xml,
        logon_task_xml(&std::env::current_exe()?, folders)
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )?;
    checked(
        Command::new("schtasks")
            .args(["/Create", "/F", "/TN", task, "/XML"])
            .arg(&xml),
        "Настроить автозапуск туннеля",
    )?;
    checked(
        Command::new("schtasks").args(["/Run", "/TN", task]),
        "Запустить туннель",
    )
}
#[cfg(target_os = "windows")]
fn logon_task_xml(exe: &Path, folders: &Path) -> String {
    fn escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
    let user = escape(
        &match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
            (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
            (_, name) => name.unwrap_or_default(),
        },
    );
    format!(
        "\u{feff}<?xml version=\"1.0\" encoding=\"UTF-16\"?>
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Enabled>true</Enabled></Settings>
  <Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>background --config {}</Arguments></Exec></Actions>
</Task>
",
        escape(&connection::command_word(exe)),
        escape(&connection::command_word(folders))
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn control(_action: &str, _folders: &Path) -> Result<()> {
    anyhow::bail!("Service controls require macOS or Windows")
}

fn checked(command: &mut Command, action: &str) -> Result<()> {
    let output = command.output()?;
    let message = if output.stderr.trim_ascii().is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    anyhow::ensure!(
        output.status.success(),
        "{action}: {}",
        console_text(message).trim()
    );
    Ok(())
}

/// Windows console tools (schtasks, taskkill) write in the OEM code page, e.g. CP866.
#[cfg(all(target_os = "windows", feature = "desktop"))]
fn console_text(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{CP_OEMCP, MultiByteToWideChar};
    let Ok(size) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if size == 0 {
        return String::new();
    }
    // SAFETY: the input slice is valid for `size` bytes; a null output asks for the length.
    let len =
        unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), size, std::ptr::null_mut(), 0) };
    if len <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; len as usize];
    // SAFETY: `wide` holds exactly the `len` UTF-16 units reported above.
    let written =
        unsafe { MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), size, wide.as_mut_ptr(), len) };
    String::from_utf16_lossy(&wide[..written.max(0) as usize])
}
#[cfg(not(all(target_os = "windows", feature = "desktop")))]
fn console_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn background(folders: &Path) -> Result<()> {
    let connection = Connection::load(folders)?.context("Подключение не настроено")?;
    let key = vault::get(&connection.credential_id)?;
    let key = std::str::from_utf8(&key).context("Invalid key encoding")?;
    vault::validate(key)?;
    let exe = std::env::current_exe()?;
    let profile = connection::path(folders, "tunnel.json")?;
    connection::atomic_write(
        &profile,
        &serde_json::to_vec_pretty(&connection::profile(&connection, &exe, folders)?)?,
    )?;
    let _ = fs::remove_file(connection::path(folders, "health.url")?);
    let mut command = Command::new(connection::tunnel_binary()?);
    command
        .arg("run")
        .arg("--config")
        .arg(profile)
        .env_clear()
        .env("CONTROL_PLANE_API_KEY", key)
        .env(
            "PATH",
            if cfg!(windows) {
                std::env::var("PATH").unwrap_or_default()
            } else {
                "/usr/bin:/bin:/usr/sbin:/sbin".into()
            },
        )
        .stdin(Stdio::null());
    for name in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "SystemRoot",
        "TEMP",
        "TMP",
        "TMPDIR",
        "HTTPS_PROXY",
        "HTTP_PROXY",
        "NO_PROXY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(windows)]
    {
        // local-files.exe is a GUI-subsystem app, so a console child would get
        // its own visible window; keep tunnel-client hidden and log to files.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let logs = connection::path(folders, "logs")?;
        fs::create_dir_all(&logs)?;
        let log = |name: &str| {
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(logs.join(name))
        };
        command
            .stdout(log("tunnel.stdout.log")?)
            .stderr(log("tunnel.stderr.log")?)
            .creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(unix))]
    {
        let status = command.status()?;
        anyhow::ensure!(status.success(), "Tunnel client exited with {status}");
        Ok(())
    }
}

pub fn worker(folders: &Path) -> Result<()> {
    // Build the child environment explicitly: no runtime/admin key reaches MCP.
    let exe = std::env::current_exe()?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let policy =
            "(version 1) (allow default) (deny file-write*) (deny network*) (deny process-fork)";
        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.args(["-p", policy]).arg(&exe);
        cmd
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = Command::new(&exe);
    command
        .arg("serve")
        .arg("--config")
        .arg(folders)
        .env_clear();
    for name in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "SystemRoot",
        "TEMP",
        "TMP",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec().into())
    }
    #[cfg(not(unix))]
    {
        anyhow::ensure!(command.status()?.success(), "MCP worker stopped");
        Ok(())
    }
}

pub fn disconnect(folders: &Path) -> Result<()> {
    control("stop", folders)?;
    if let Some(old) = Connection::load(folders)? {
        fs::remove_file(connection::path(folders, "connection.json")?)?;
        #[cfg(any(target_os = "macos", all(target_os = "windows", feature = "desktop")))]
        vault::delete(&old.credential_id)?;
    }
    Ok(())
}

pub fn open_url(url: &str) -> Result<()> {
    open(Path::new(url))
}

pub fn open(path: &std::path::Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command
        .arg(path)
        .spawn()
        .context("Не удалось открыть папку")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn health_requires_recent_success() {
        assert!(fresh(1000.0, 1030.0));
        assert!(!fresh(1000.0, 1100.0));
        assert!(!fresh(0.0, 0.0));
        assert!(!fresh(f64::NAN, 1000.0));
        assert_eq!(
            last_success(
                "# TYPE commands_poll_last_successful_timestamp_seconds gauge\ncommands_poll_last_successful_timestamp_seconds 1234\ncommands_poll_errors_total 9999"
            ),
            1234.0
        );
    }

    // Registers a throwaway task exactly as `control` does, without administrator rights.
    #[cfg(target_os = "windows")]
    #[test]
    fn logon_task_registers_for_current_user() -> Result<()> {
        let dir = tempfile::Builder::new()
            .prefix("Strigov Local MCP QA's & co ")
            .tempdir()?;
        let xml = dir.path().join("task.xml");
        fs::write(
            &xml,
            logon_task_xml(Path::new(r"C:\Windows\System32\cmd.exe"), dir.path())
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )?;
        let task = format!("Local Files Tunnel QA {}", std::process::id());
        let created = checked(
            Command::new("schtasks")
                .args(["/Create", "/F", "/TN", &task, "/XML"])
                .arg(&xml),
            "create",
        );
        let _ = Command::new("schtasks")
            .args(["/Delete", "/F", "/TN", &task])
            .output();
        created
    }

    #[cfg(all(target_os = "windows", feature = "desktop"))]
    #[test]
    fn console_errors_are_decoded_from_oem_code_page() {
        let error = checked(
            Command::new("schtasks").args(["/Query", "/TN", "Local Files Tunnel QA missing"]),
            "query",
        )
        .unwrap_err()
        .to_string();
        assert!(error.len() > "query: ".len());
        assert!(!error.contains('\u{fffd}'), "{error}");
    }
}
