use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const TUNNEL_SETTINGS: &str = "https://platform.openai.com/settings/organization/tunnels";
pub const KEY_SETTINGS: &str = "https://platform.openai.com/settings/organization/api-keys";
pub const CHATGPT_SETTINGS: &str = "https://chatgpt.com/#settings/Connectors";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    pub version: u32,
    pub tunnel_id: String,
    // This identifies a Keychain/Credential Manager record. It is not the secret.
    pub credential_id: String,
}

pub fn directory(folders: &Path) -> Result<&Path> {
    folders
        .parent()
        .context("Settings require a parent directory")
}
pub fn path(folders: &Path, name: &str) -> Result<PathBuf> {
    Ok(directory(folders)?.join(name))
}
pub fn validate_tunnel_id(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.starts_with("tunnel_")
            && (15..=128).contains(&value.len())
            && value[7..]
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "Введите ID туннеля из OpenAI Platform: tunnel_…"
    );
    Ok(())
}
impl Connection {
    pub fn new(tunnel_id: &str) -> Result<Self> {
        let tunnel_id = tunnel_id.trim().to_owned();
        validate_tunnel_id(&tunnel_id)?;
        Ok(Self {
            version: 1,
            tunnel_id,
            credential_id: format!(
                "connection-{:x}-{:x}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            ),
        })
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.version == 1, "Unsupported connection settings");
        validate_tunnel_id(&self.tunnel_id)?;
        anyhow::ensure!(
            (1..=100).contains(&self.credential_id.len())
                && self
                    .credential_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
            "Invalid credential record"
        );
        Ok(())
    }
    pub fn load(folders: &Path) -> Result<Option<Self>> {
        let file = match fs::File::open(path(folders, "connection.json")?) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut data = Vec::new();
        file.take(16_385).read_to_end(&mut data)?;
        anyhow::ensure!(data.len() <= 16_384, "Connection settings too large");
        let value: Self = serde_json::from_slice(&data)?;
        value.validate()?;
        Ok(Some(value))
    }
    pub fn save(&self, folders: &Path) -> Result<()> {
        self.validate()?;
        atomic_write(
            &path(folders, "connection.json")?,
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing parent directory")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn tunnel_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let parent = exe.parent().context("Missing executable directory")?;
    #[cfg(target_os = "macos")]
    let binary = parent.join("../Resources/tunnel-client");
    #[cfg(not(target_os = "macos"))]
    let binary = parent.join(if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    });
    anyhow::ensure!(
        binary.is_file(),
        "В сборке отсутствует tunnel-client. Переустановите Strigov Local MCP из полного дистрибутива."
    );
    Ok(binary)
}

/// Quotes one argument for Windows command lines such as the scheduled task action.
#[cfg(windows)]
pub fn command_word(value: &Path) -> String {
    format!("\"{}\"", value.to_string_lossy())
}
/// tunnel-client splits mcp.commands with POSIX shell rules on every platform,
/// so Windows backslashes must be protected by single quotes as well.
fn shell_word(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"))
}
pub fn profile(connection: &Connection, exe: &Path, folders: &Path) -> Result<serde_json::Value> {
    connection.validate()?;
    Ok(serde_json::json!({
        "config_version": 1,
        "control_plane": { "base_url": "https://api.openai.com", "tunnel_id": connection.tunnel_id,
            "api_key": "env:CONTROL_PLANE_API_KEY" },
        "health": { "listen_addr": "127.0.0.1:0", "url_file": path(folders, "health.url")? },
        "admin_ui": { "open_browser": false },
        "log": { "level": "info", "format": "json" },
        "mcp": { "commands": [{"channel": "main",
            "command": format!("{} worker --config {}", shell_word(exe), shell_word(folders))}] }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    #[ignore = "requires LOCAL_FILES_TEST_APP pointing to a packaged or installed app"]
    fn packaged_runtime_accepts_generated_profile_without_external_account() -> Result<()> {
        use std::{
            net::{TcpListener, TcpStream},
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };
        struct Runtime(Child);
        impl Drop for Runtime {
            fn drop(&mut self) {
                // Let tunnel-client terminate its MCP child before collecting it.
                #[cfg(unix)]
                unsafe {
                    libc::kill(self.0.id() as i32, libc::SIGTERM)
                };
                // Windows has no SIGTERM: end exactly this process tree.
                #[cfg(windows)]
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &self.0.id().to_string()])
                    .output();
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline {
                    if self.0.try_wait().ok().flatten().is_some() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let app = PathBuf::from(std::env::var("LOCAL_FILES_TEST_APP")?);
        #[cfg(target_os = "macos")]
        let (exe, runtime) = (
            app.join("Contents/MacOS/local-files"),
            app.join("Contents/Resources/tunnel-client"),
        );
        #[cfg(windows)]
        let (exe, runtime) = (app.join("local-files.exe"), app.join("tunnel-client.exe"));
        // Spaces and apostrophes must survive the tunnel client's command parser.
        let temp = tempfile::Builder::new()
            .prefix("Strigov Local MCP QA's ")
            .tempdir()?;
        let folders = temp.path().join("folders.json");
        crate::config::initialize(&folders)?;
        let conn = Connection::new("tunnel_0123456789abcdef0123456789abcdef")?;
        conn.save(&folders)?;
        // Bind a local endpoint but never answer requests. No real account or
        // credential is used; a running local worker must not imply connected.
        let control_plane = TcpListener::bind("127.0.0.1:0")?;
        let mut value = profile(&conn, &exe, &folders)?;
        value["control_plane"]["base_url"] =
            format!("http://{}", control_plane.local_addr()?).into();
        let profile_path = temp.path().join("tunnel.json");
        atomic_write(&profile_path, &serde_json::to_vec(&value)?)?;
        let output = fs::File::create(temp.path().join("runtime.log"))?;
        let mut command = Command::new(runtime);
        command
            .args(["run", "--config"])
            .arg(profile_path)
            .env_clear()
            .env("CONTROL_PLANE_API_KEY", "local-test-placeholder");
        #[cfg(unix)]
        command
            .env("HOME", temp.path())
            .env("PATH", "/usr/bin:/bin");
        #[cfg(windows)]
        {
            let system = std::env::var_os("SystemRoot").context("SystemRoot unavailable")?;
            command
                .env("SystemRoot", &system)
                .env("PATH", PathBuf::from(&system).join("System32"))
                .env("USERPROFILE", temp.path())
                .env("LOCALAPPDATA", temp.path())
                .env("APPDATA", temp.path())
                .env("TEMP", temp.path())
                .env("TMP", temp.path());
        }
        let mut child = Runtime(
            command
                .stdin(Stdio::null())
                .stdout(output.try_clone()?)
                .stderr(output)
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            anyhow::ensure!(
                child.0.try_wait()?.is_none(),
                "Bundled runtime exited: {}",
                fs::read_to_string(temp.path().join("runtime.log")).unwrap_or_default()
            );
            let health = crate::service::health(&folders);
            if health.live && health.ready {
                anyhow::ensure!(
                    !health.connected,
                    "A local worker is not an authenticated tunnel"
                );
                break;
            }
            anyhow::ensure!(Instant::now() < deadline, "Runtime health was not ready");
            std::thread::sleep(Duration::from_millis(50));
        }
        let url = fs::read_to_string(path(&folders, "health.url")?)?;
        let address = url
            .trim()
            .trim_end_matches('/')
            .strip_prefix("http://")
            .unwrap();
        let mut stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        write!(
            stream,
            "GET /health?details=true HTTP/1.0\r\nHost: localhost\r\n\r\n"
        )?;
        let mut response = String::new();
        stream.take(128 * 1024).read_to_string(&mut response)?;
        let (_, body) = response
            .split_once("\r\n\r\n")
            .context("Missing health response")?;
        let details: serde_json::Value = serde_json::from_str(body)?;
        assert_eq!(
            details["components"]["mcp"]["details"]["child_state"],
            "running"
        );
        Ok(())
    }

    #[test]
    fn portable_profile_contains_only_explicit_settings() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let folders = temp.path().join("folders.json");
        assert!(Connection::load(&folders)?.is_none());
        let conn = Connection::new("tunnel_0123456789abcdef0123456789abcdef")?;
        conn.save(&folders)?;
        assert_eq!(
            Connection::load(&folders)?.unwrap().tunnel_id,
            conn.tunnel_id
        );
        let value = profile(&conn, &temp.path().join("Program's Files/app"), &folders)?;
        assert_eq!(
            value["control_plane"]["api_key"],
            "env:CONTROL_PLANE_API_KEY"
        );
        assert_eq!(value["health"]["listen_addr"], "127.0.0.1:0");
        assert!(!serde_json::to_string(&value)?.contains("sk-"));
        for invalid in [
            "",
            "tunnel_",
            "https://example.org",
            "tunnel_abcd\n--x",
            "tunnel_../../etc",
        ] {
            assert!(Connection::new(invalid).is_err());
        }
        Ok(())
    }
}
