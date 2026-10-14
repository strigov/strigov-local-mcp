use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
};

const PRODUCT: &str = "app.localfiles.desktop";
const PLATFORM: &str = "windows-amd64";
const MANIFEST: &str = "install-manifest.json";
const EXECUTABLES: [&str; 2] = ["local-files.exe", "tunnel-client.exe"];
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const BUSY: &str = "Strigov Local MCP запущен. Остановите туннель и закройте значок в трее, затем повторите установку.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    product: String,
    version: String,
    platform: String,
    files: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    bytes: u64,
    sha256: String,
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}
fn relative(value: &str) -> Result<PathBuf> {
    let parts: Vec<&str> = value.split('/').collect();
    anyhow::ensure!(
        !value.contains(['\\', ':'])
            && parts
                .iter()
                .all(|p| !p.is_empty() && *p != "." && *p != ".."),
        "Unsafe path in installer manifest: {value:?}"
    );
    Ok(parts.iter().collect())
}
fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn read_manifest(dir: &Path) -> Result<Manifest> {
    let mut data = Vec::new();
    fs::File::open(dir.join(MANIFEST))?
        .take(1 << 20)
        .read_to_end(&mut data)?;
    Ok(serde_json::from_slice(&data)?)
}
fn inventory(root: &Path, dir: &Path, files: &mut BTreeSet<String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        anyhow::ensure!(
            !is_reparse(&metadata),
            "Unexpected link in installer payload"
        );
        if metadata.is_dir() {
            inventory(root, &entry.path(), files)?;
        } else if metadata.is_file() {
            let name = entry.path().strip_prefix(root)?.to_owned();
            let parts: Vec<_> = name.iter().map(|p| p.to_string_lossy()).collect();
            files.insert(parts.join("/"));
        } else {
            anyhow::bail!("Unexpected special file in installer payload");
        }
    }
    Ok(())
}

/// Checks the exact file list, sizes and SHA-256 digests. This proves integrity
/// against the shipped manifest; it is not a publisher signature.
fn verify(dir: &Path) -> Result<Manifest> {
    let damaged =
        "Дистрибутив повреждён или распакован не полностью. Распакуйте весь архив заново.";
    let metadata = fs::symlink_metadata(dir).context(damaged)?;
    anyhow::ensure!(metadata.is_dir() && !is_reparse(&metadata), damaged);
    let manifest = read_manifest(dir).context(damaged)?;
    anyhow::ensure!(
        manifest.schema_version == 1
            && manifest.product == PRODUCT
            && manifest.platform == PLATFORM
            && manifest.version == env!("CARGO_PKG_VERSION"),
        "Дистрибутив предназначен для другой версии или платформы"
    );
    let listed: Vec<&str> = manifest.files.iter().map(|e| e.path.as_str()).collect();
    anyhow::ensure!(
        listed.windows(2).all(|w| w[0] < w[1])
            && EXECUTABLES.iter().all(|e| listed.contains(e))
            && !listed.contains(&MANIFEST),
        "Invalid installer manifest"
    );
    let mut actual = BTreeSet::new();
    inventory(dir, dir, &mut actual)?;
    actual.remove(MANIFEST);
    anyhow::ensure!(
        actual.iter().map(String::as_str).eq(listed.iter().copied()),
        damaged
    );
    for entry in &manifest.files {
        let path = dir.join(relative(&entry.path)?);
        anyhow::ensure!(
            entry.sha256.len() == 64
                && entry
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                && fs::metadata(&path)?.len() == entry.bytes
                && sha256(&path)? == entry.sha256,
            "{damaged} ({})",
            entry.path
        );
    }
    Ok(manifest)
}

fn payload() -> Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .context("Missing executable directory")?
        .join("payload"))
}

enum Existing {
    Missing,
    Empty,
    Ours,
}
fn existing(destination: &Path) -> Result<Existing> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Existing::Missing),
        Err(e) => return Err(e.into()),
    };
    anyhow::ensure!(
        metadata.is_dir() && !is_reparse(&metadata),
        "Путь установки занят файлом или ссылкой: {}",
        destination.display()
    );
    if fs::read_dir(destination)?.next().is_none() {
        return Ok(Existing::Empty);
    }
    let ours = read_manifest(destination)
        .is_ok_and(|m| m.schema_version == 1 && m.product == PRODUCT && m.platform == PLATFORM);
    anyhow::ensure!(
        ours,
        "Папка установки содержит чужие файлы и не будет изменена: {}",
        destination.display()
    );
    Ok(Existing::Ours)
}
/// A running executable cannot be opened exclusively for writing.
fn ensure_not_running(destination: &Path) -> Result<()> {
    for name in EXECUTABLES {
        let path = destination.join(name);
        if !path.exists() {
            continue;
        }
        let opened = fs::OpenOptions::new().write(true).share_mode(0).open(&path);
        anyhow::ensure!(opened.is_ok(), BUSY);
    }
    Ok(())
}
fn copy_payload(manifest: &Manifest, source: &Path, destination: &Path) -> Result<()> {
    for entry in &manifest.files {
        let target = destination.join(relative(&entry.path)?);
        fs::create_dir_all(target.parent().context("Invalid payload path")?)?;
        fs::copy(source.join(relative(&entry.path)?), target)?;
    }
    fs::copy(source.join(MANIFEST), destination.join(MANIFEST))?;
    Ok(())
}

fn install(source: &Path, destination: &Path) -> Result<()> {
    let manifest = verify(source)?;
    anyhow::ensure!(
        destination.is_absolute() && destination.file_name().is_some(),
        "Select an absolute installation path"
    );
    for ancestor in destination.ancestors().skip(1) {
        if let Ok(metadata) = fs::symlink_metadata(ancestor) {
            anyhow::ensure!(
                !is_reparse(&metadata),
                "Путь установки проходит через ссылку: {}",
                ancestor.display()
            );
        }
    }
    let state = existing(destination)?;
    let parent = destination
        .parent()
        .context("Invalid installation directory")?;
    fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".strigov-local-mcp-install-")
        .tempdir_in(parent)?;
    let staged = stage.path().join("app");
    copy_payload(&manifest, source, &staged)?;
    verify(&staged)?;
    match state {
        Existing::Missing => fs::rename(&staged, destination)?,
        Existing::Empty => {
            fs::remove_dir(destination)?;
            fs::rename(&staged, destination)?;
        }
        Existing::Ours => {
            ensure_not_running(destination)?;
            let folder = tempfile::Builder::new()
                .prefix("Strigov Local MCP backup ")
                .tempdir_in(parent)?
                .keep();
            let backup = folder.join(destination.file_name().context("Invalid name")?);
            fs::rename(destination, &backup).context(BUSY)?;
            if let Err(error) = fs::rename(&staged, destination) {
                fs::rename(&backup, destination)?;
                let _ = fs::remove_dir(&folder);
                return Err(error.into());
            }
        }
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let source = payload()?;
    if args == ["--check"] {
        verify(&source)?;
        println!("Installer payload verified; no settings or services changed.");
        return Ok(());
    }
    if args.len() == 3 && args[0] == "--install-to" && args[2] == "--no-launch" {
        install(&source, Path::new(&args[1]))?;
        println!("Installed without launching or changing user settings.");
        return Ok(());
    }
    anyhow::ensure!(
        args.is_empty(),
        "Usage: local-files-installer [--check | --install-to PATH --no-launch]"
    );
    let destination =
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA unavailable")?)
            .join("Programs/Strigov Local MCP");
    let button = rfd::MessageDialog::new().set_title("Установка Strigov Local MCP")
        .set_description(format!("Strigov Local MCP {}\n\nПриложение будет установлено в:\n{}\n\nПри первом запуске выберите папки и настройте собственное подключение к ChatGPT. Дистрибутив не содержит аккаунтов, ключей или подключённых папок.\n\nПри обновлении пользовательские настройки сохраняются.", env!("CARGO_PKG_VERSION"), destination.display()))
        .set_buttons(rfd::MessageButtons::OkCancelCustom("Установить".into(), "Отмена".into())).show();
    if button != rfd::MessageDialogResult::Custom("Установить".into())
        && button != rfd::MessageDialogResult::Ok
    {
        return Ok(());
    }
    install(&source, &destination)?;
    Command::new(destination.join("local-files.exe"))
        .arg("tray")
        .current_dir(&destination)
        .spawn()
        .context("Приложение установлено, но не запустилось. Откройте local-files.exe из папки установки.")?;
    rfd::MessageDialog::new().set_title("Strigov Local MCP установлен")
        .set_description(format!("Приложение установлено в:\n{}\n\nНажмите значок серверной стойки в области уведомлений, добавьте папки и настройте подключение к своему аккаунту ChatGPT.", destination.display()))
        .show();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(root: &Path) -> Result<PathBuf> {
        let dir = root.join("payload");
        fs::create_dir_all(dir.join("licenses"))?;
        let files = [
            ("licenses/NOTICE.txt", "notice"),
            ("local-files.exe", "app"),
            ("tunnel-client.exe", "runtime"),
        ];
        let mut entries = Vec::new();
        for (name, content) in files {
            let path = dir.join(relative(name)?);
            fs::write(&path, content)?;
            entries.push(
                serde_json::json!({"path": name, "bytes": content.len(), "sha256": sha256(&path)?}),
            );
        }
        let manifest = serde_json::json!({"schema_version": 1, "product": PRODUCT,
            "version": env!("CARGO_PKG_VERSION"), "platform": PLATFORM, "files": entries});
        fs::write(dir.join(MANIFEST), serde_json::to_vec(&manifest)?)?;
        Ok(dir)
    }

    #[test]
    fn payload_must_match_manifest_exactly() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = payload(temp.path())?;
        verify(&source)?;
        fs::write(source.join("extra.dll"), "x")?;
        assert!(verify(&source).is_err());
        fs::remove_file(source.join("extra.dll"))?;
        fs::write(source.join("local-files.exe"), "ap!")?;
        assert!(verify(&source).is_err());
        fs::write(source.join("local-files.exe"), "app")?;
        fs::remove_file(source.join("tunnel-client.exe"))?;
        assert!(verify(&source).is_err());
        for bad in ["../x", "a\\b", "C:/x", "a//b", "./a", "a:stream"] {
            assert!(relative(bad).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn update_keeps_backup_and_rejects_foreign_or_busy_targets() -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix("Strigov QA's Тест ")
            .tempdir()?;
        let source = payload(temp.path())?;
        let destination = temp.path().join("Programs/Strigov Local MCP");
        install(&source, &destination)?;
        verify(&destination)?;

        fs::write(destination.join("local-files.exe"), "old")?;
        install(&source, &destination)?;
        verify(&destination)?;
        let backups: Vec<_> = fs::read_dir(temp.path().join("Programs"))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("Strigov Local MCP backup ")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        let old = backups[0].path().join("Strigov Local MCP/local-files.exe");
        assert_eq!(fs::read_to_string(old)?, "old");

        let busy = fs::File::open(destination.join("local-files.exe"))?;
        let error = install(&source, &destination).unwrap_err();
        assert!(format!("{error:#}").contains("Остановите туннель"));
        drop(busy);
        verify(&destination)?;

        let foreign = temp.path().join("Foreign");
        fs::create_dir_all(&foreign)?;
        fs::write(foreign.join("data.txt"), "keep")?;
        assert!(install(&source, &foreign).is_err());
        assert_eq!(fs::read_to_string(foreign.join("data.txt"))?, "keep");
        assert_eq!(fs::read_dir(&foreign)?.count(), 1);

        fs::write(source.join("tunnel-client.exe"), "tampered")?;
        assert!(install(&source, &temp.path().join("Fresh")).is_err());
        assert!(!temp.path().join("Fresh").exists());
        Ok(())
    }
}
