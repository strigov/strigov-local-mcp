#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn payload() -> Result<PathBuf> {
    let executable = std::env::current_exe()?;
    Ok(executable
        .parent()
        .context("Missing executable directory")?
        .join("../Resources/Strigov Local MCP.app"))
}
fn verify(app: &Path) -> Result<()> {
    anyhow::ensure!(
        app.join("Contents/MacOS/local-files").is_file()
            && app.join("Contents/Resources/tunnel-client").is_file(),
        "В установщике отсутствуют обязательные файлы. Скачайте полный дистрибутив."
    );
    let result = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "--deep"])
        .arg(app)
        .output()?;
    anyhow::ensure!(result.status.success(), "Нарушена целостность приложения");
    Ok(())
}
fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        anyhow::ensure!(
            !kind.is_symlink(),
            "Unexpected symlink in installer payload"
        );
        if kind.is_dir() {
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        } else if kind.is_file() {
            fs::copy(entry.path(), destination.join(entry.file_name()))?;
        } else {
            anyhow::bail!("Unexpected special file in installer payload");
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn gui_pids(app: &Path) -> Result<Vec<i32>> {
    let executable = app.join("Contents/MacOS/local-files");
    let uid = unsafe { libc::getuid() }.to_string();
    let output = Command::new("/bin/ps")
        .args(["-ww", "-axo", "uid=,pid=,comm="])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (owner, rest) = line.split_once(char::is_whitespace)?;
            let (pid, command) = rest.trim().split_once(char::is_whitespace)?;
            (owner == uid && command.trim() == executable.to_string_lossy())
                .then(|| pid.parse().ok())
                .flatten()
        })
        .collect())
}
#[cfg(target_os = "macos")]
fn stop(app: &Path) -> Result<()> {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    for label in ["app.localfiles.tray", "app.localfiles.tunnel"] {
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("{domain}/{label}")])
            .output();
    }
    for pid in gui_pids(app)? {
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    for _ in 0..50 {
        if gui_pids(app)?.is_empty() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!("Закройте Strigov Local MCP и повторите установку")
}

#[cfg(target_os = "macos")]
fn launch(app: &Path) -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?);
    let settings = home.join("Library/Application Support/Local Files");
    let logs = settings.join("logs");
    fs::create_dir_all(&logs)?;
    let agents = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents)?;
    let mut values = plist::Dictionary::new();
    values.insert("Label".into(), "app.localfiles.tray".into());
    values.insert(
        "ProgramArguments".into(),
        plist::Value::Array(vec![
            app.join("Contents/MacOS/local-files")
                .to_string_lossy()
                .into_owned()
                .into(),
            "tray".into(),
        ]),
    );
    values.insert("RunAtLoad".into(), true.into());
    values.insert("LimitLoadToSessionType".into(), "Aqua".into());
    values.insert("ProcessType".into(), "Interactive".into());
    values.insert(
        "StandardOutPath".into(),
        logs.join("tray.stdout.log")
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    values.insert(
        "StandardErrorPath".into(),
        logs.join("tray.stderr.log")
            .to_string_lossy()
            .into_owned()
            .into(),
    );
    let target = agents.join("app.localfiles.tray.plist");
    let mut temp = tempfile::NamedTempFile::new_in(&agents)?;
    plist::Value::Dictionary(values).to_writer_xml(&mut temp)?;
    temp.persist(&target)?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let _ = Command::new("/bin/launchctl")
        .args(["enable", &format!("{domain}/app.localfiles.tray")])
        .output()?;
    let result = Command::new("/bin/launchctl")
        .arg("bootstrap")
        .arg(domain)
        .arg(target)
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "Приложение установлено, но автозапуск не удалось включить. Откройте Strigov Local MCP из папки Applications."
    );
    Ok(())
}

fn install(source: &Path, destination: &Path, _launch_after: bool) -> Result<()> {
    verify(source)?;
    anyhow::ensure!(
        destination.is_absolute() && destination.file_name().is_some(),
        "Select an absolute installation path"
    );
    anyhow::ensure!(
        !destination.is_symlink(),
        "Installation path cannot be a symlink"
    );
    let parent = destination
        .parent()
        .context("Invalid installation directory")?;
    fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".local-files-install-")
        .tempdir_in(parent)?;
    let staged = stage.path().join("Strigov Local MCP.app");
    copy_tree(source, &staged)?;
    verify(&staged)?;
    let mut previous = Vec::new();
    if destination.exists() {
        previous.push(destination.to_owned());
    }
    // Only migrate our generic 2.1 bundle. The older personal installation has
    // a different identity and must not be mistaken for a distribution update.
    #[cfg(target_os = "macos")]
    if destination
        .file_name()
        .is_some_and(|n| n == "Strigov Local MCP.app")
    {
        let old = parent.join("Local Files.app");
        if !old.is_symlink()
            && plist::Value::from_file(old.join("Contents/Info.plist"))
                .ok()
                .and_then(|v| {
                    v.as_dictionary()?
                        .get("CFBundleIdentifier")?
                        .as_string()
                        .map(str::to_owned)
                })
                .is_some_and(|id| id == "app.localfiles.desktop")
        {
            previous.push(old);
        }
    }
    // The complete replacement is ready before the existing application is stopped.
    #[cfg(target_os = "macos")]
    if _launch_after {
        stop(destination)?;
        for app in &previous {
            if app != destination {
                stop(app)?;
            }
        }
    }
    let mut backups = Vec::new();
    let replacement = (|| -> Result<()> {
        for app in &previous {
            let folder = tempfile::Builder::new()
                .prefix("Strigov Local MCP backup ")
                .tempdir_in(parent)?
                .keep();
            let backup = folder.join(app.file_name().context("Invalid app name")?);
            fs::rename(app, &backup)?;
            backups.push((app, backup));
        }
        fs::rename(&staged, destination)?;
        Ok(())
    })();
    if let Err(error) = replacement {
        for (app, backup) in backups.iter().rev() {
            fs::rename(backup, app)?;
        }
        return Err(error);
    }
    #[cfg(target_os = "macos")]
    if _launch_after {
        launch(destination)?;
    }
    Ok(())
}

fn run() -> Result<()> {
    anyhow::ensure!(cfg!(target_os = "macos"), "This installer is for macOS");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = payload()?;
    if args == ["--check"] {
        verify(&source)?;
        println!("Installer payload verified; no settings or services changed.");
        return Ok(());
    }
    if args.len() == 3 && args[0] == "--install-to" && args[2] == "--no-launch" {
        install(&source, Path::new(&args[1]), false)?;
        println!("Installed without launching or changing user settings.");
        return Ok(());
    }
    anyhow::ensure!(
        args.is_empty(),
        "Usage: local-files-installer [--check | --install-to PATH --no-launch]"
    );
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME unavailable")?);
    let destination = home.join("Applications/Strigov Local MCP.app");
    let button = rfd::MessageDialog::new().set_title("Установка Strigov Local MCP")
        .set_description(format!("Strigov Local MCP {}\\n\\nПриложение будет установлено в:\\n{}\\n\\nПри первом запуске выберите папки и настройте собственное подключение к ChatGPT. Дистрибутив не содержит аккаунтов, ключей или подключённых папок.\\n\\nПри обновлении пользовательские настройки сохраняются.", env!("CARGO_PKG_VERSION"), destination.display()).replace("\\n", "\n"))
        .set_buttons(rfd::MessageButtons::OkCancelCustom("Установить".into(), "Отмена".into())).show();
    if button != rfd::MessageDialogResult::Custom("Установить".into())
        && button != rfd::MessageDialogResult::Ok
    {
        return Ok(());
    }
    install(&source, &destination, true)?;
    rfd::MessageDialog::new().set_title("Strigov Local MCP установлен")
        .set_description("Приложение запущено. Нажмите значок серверной стойки в строке меню, добавьте папки и настройте подключение к своему аккаунту ChatGPT.").show();
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        if std::env::args().len() > 1 {
            eprintln!("{error:#}");
        } else {
            rfd::MessageDialog::new()
                .set_title("Установка Strigov Local MCP")
                .set_level(rfd::MessageLevel::Error)
                .set_description(format!("{error:#}"))
                .show();
        }
        std::process::exit(1);
    }
}
