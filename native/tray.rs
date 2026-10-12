use crate::{
    config::{self, Config},
    connection::{self, Connection},
    service::{self, Health},
    vault,
};
use anyhow::{Context, Result};
use fs4::FileExt;
use std::{collections::HashMap, path::PathBuf, time::Duration};
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};
#[path = "nickname_dialog.rs"]
mod nickname_dialog;
#[path = "tray_art.rs"]
mod tray_art;

#[derive(Debug)]
enum UiEvent {
    Menu(MenuEvent),
    Health(Health),
    Action(std::result::Result<(), String>),
    Setup,
}

fn icon(disconnected: bool) -> Result<Icon> {
    Ok(Icon::from_rgba(
        tray_art::pixels(disconnected),
        tray_art::SIZE,
        tray_art::SIZE,
    )?)
}
fn alert(message: &str) {
    rfd::MessageDialog::new()
        .set_title("Strigov Local MCP")
        .set_description(message)
        .set_level(rfd::MessageLevel::Error)
        .show();
}
struct Ui {
    tray: TrayIcon,
    icons: [Icon; 2],
    connected: bool,
    status: MenuItem,
    detail: MenuItem,
    folder_status: MenuItem,
    actions: HashMap<String, (String, String)>,
}
impl Ui {
    fn new(path: &std::path::Path) -> Result<Self> {
        let icons = [icon(true)?, icon(false)?];
        let builder = TrayIconBuilder::new()
            .with_icon(icons[0].clone())
            .with_tooltip("Strigov Local MCP — проверка соединения");
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_as_template(true);
        let tray = builder.build()?;
        let mut ui = Self {
            tray,
            icons,
            connected: false,
            status: MenuItem::new("Проверка соединения…", false, None),
            detail: MenuItem::new("", false, None),
            folder_status: MenuItem::new("", false, None),
            actions: HashMap::new(),
        };
        ui.rebuild(path)?;
        Ok(ui)
    }
    fn rebuild(&mut self, path: &std::path::Path) -> Result<()> {
        let config = Config::load(path)?;
        let menu = Menu::new();
        menu.append_items(&[
            &self.status,
            &self.detail,
            &self.folder_status,
            &PredefinedMenuItem::separator(),
        ])?;
        for (id, label) in [
            ("start", "Запустить"),
            ("stop", "Остановить"),
            ("restart", "Перезапустить"),
        ] {
            menu.append(&MenuItem::with_id(id, label, true, None))?;
        }
        let connection_menu = Submenu::new("Подключение ChatGPT", true);
        if let Some(c) = Connection::load(path)? {
            connection_menu.append(&MenuItem::new(c.tunnel_id, false, None))?;
        }
        for (id, title) in [
            ("setup", "Настроить или сменить подключение…"),
            ("tunnels", "Создать туннель в OpenAI Platform…"),
            ("keys", "Получить runtime API key…"),
            ("chatgpt", "Подключить в ChatGPT…"),
            ("disconnect", "Отключить аккаунт…"),
        ] {
            connection_menu.append(&MenuItem::with_id(id, title, true, None))?;
        }
        menu.append(&connection_menu)?;
        let folders = Submenu::new("Подключённые папки", true);
        self.actions.clear();
        for root in &config.roots {
            let available = root.path.is_dir();
            let label = format!(
                "{}{}",
                if available { "" } else { "⚠ " },
                root.display_name()
            );
            let item = Submenu::new(label, true);
            item.append(&MenuItem::new(root.path.to_string_lossy(), false, None))?;
            item.append(&MenuItem::new("Доступ только для чтения", false, None))?;
            for (action, label) in [
                ("rename", "Изменить прозвище…"),
                (
                    "open",
                    if cfg!(target_os = "macos") {
                        "Открыть в Finder"
                    } else {
                        "Открыть в проводнике"
                    },
                ),
                ("replace", "Выбрать другую папку…"),
                ("remove", "Отключить доступ"),
            ] {
                let id = format!("{action}:{}", root.id);
                self.actions
                    .insert(id.clone(), (action.into(), root.id.clone()));
                item.append(&MenuItem::with_id(id, label, true, None))?;
            }
            folders.append(&item)?;
        }
        if config.roots.is_empty() {
            folders.append(&MenuItem::new("Папки пока не подключены", false, None))?;
        }
        folders.append(&PredefinedMenuItem::separator())?;
        folders.append(&MenuItem::with_id("add", "Добавить папку…", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&folders)?;
        menu.append(&MenuItem::with_id("logs", "Открыть журнал", true, None))?;
        menu.append(&MenuItem::with_id(
            "about",
            "О Strigov Local MCP…",
            true,
            None,
        ))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&MenuItem::with_id(
            "quit",
            "Закрыть значок (туннель продолжит работу)",
            true,
            None,
        ))?;
        self.tray.set_menu(Some(Box::new(menu)));
        self.folder_status.set_text(format!(
            "Папок: {} · найдены: {}",
            config.roots.len(),
            config.roots.iter().filter(|r| r.path.is_dir()).count()
        ));
        Ok(())
    }
    fn health(&mut self, health: &Health, path: &std::path::Path) {
        let label = if health.connected {
            "Работает"
        } else if !health.live {
            "Отключён"
        } else {
            "Подключение…"
        };
        self.status.set_text(format!("Strigov Local MCP · {label}"));
        self.detail.set_text(&health.message);
        if self.connected != health.connected {
            let icon = Some(self.icons[usize::from(health.connected)].clone());
            #[cfg(target_os = "macos")]
            let changed = self.tray.set_icon_with_as_template(icon, true);
            #[cfg(not(target_os = "macos"))]
            let changed = self.tray.set_icon(icon);
            if changed.is_ok() {
                self.connected = health.connected;
            }
        }
        let _ = self
            .tray
            .set_tooltip(Some(format!("Strigov Local MCP · {label}")));
        if let Ok(c) = Config::load(path) {
            self.folder_status.set_text(format!(
                "Папок: {} · найдены: {}",
                c.roots.len(),
                c.roots.iter().filter(|r| r.path.is_dir()).count()
            ));
        }
    }
    fn configure(&mut self, path: &std::path::Path) -> Result<()> {
        connection::tunnel_binary()?;
        let old = Connection::load(path)?;
        let initial = old.as_ref().map(|c| c.tunnel_id.as_str()).unwrap_or("");
        let Some(id) = nickname_dialog::text(
            "Подключение к ChatGPT",
            "Вставьте ID собственного туннеля из OpenAI Platform. Ссылки для создания туннеля и получения ключа находятся в меню «Подключение ChatGPT».",
            initial,
            false,
        )?
        else {
            return Ok(());
        };
        let next = Connection::new(&id)?;
        let Some(key) = nickname_dialog::text(
            "Runtime API key",
            "Ключ должен иметь право использовать выбранный туннель. Он будет сохранён только на этом компьютере в системном хранилище секретов.",
            "",
            true,
        )?
        else {
            return Ok(());
        };
        vault::validate(&key)?;
        let folders = Config::load(path)?;
        let names = folders
            .roots
            .iter()
            .map(|r| r.display_name())
            .collect::<Vec<_>>()
            .join(", ");
        let description = format!(
            "Туннель: {}\n\nДоступ только для чтения к папкам: {}.\n\nПроверьте, что туннель привязан к нужному аккаунту или workspace. Создайте подключение к нему в ChatGPT после запуска.",
            next.tunnel_id,
            if names.is_empty() {
                "пока ни одной"
            } else {
                &names
            }
        );
        if rfd::MessageDialog::new()
            .set_title("Подключить выбранные папки?")
            .set_description(description)
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show()
            != rfd::MessageDialogResult::Ok
        {
            return Ok(());
        }
        vault::set(&next.credential_id, key.trim())?;
        let outcome = (|| -> Result<()> {
            service::control("stop", path)?;
            next.save(path)?;
            Ok(())
        })();
        if let Err(error) = outcome {
            let _ = vault::delete(&next.credential_id);
            return Err(error);
        }
        if let Some(old) = old {
            let _ = vault::delete(&old.credential_id);
        }
        self.rebuild(path)?;
        self.health(&service::health(path), path);
        service::control("start", path)?;
        Ok(())
    }
    fn menu(&mut self, id: &str, path: &std::path::Path) -> Result<()> {
        if id == "about" {
            rfd::MessageDialog::new().set_title(concat!("Strigov Local MCP ", env!("CARGO_PKG_VERSION")))
                .set_description("Файлы для ChatGPT\n\nТолько чтение выбранных вами папок.\n\nСерверная стойка: соединение работает.\nПеречёркнутая стойка: соединение не подтверждено.\nПодробности — в строках состояния меню.\n\nmacOS может отдельно запросить разрешение на чтение папки.\nЗакрытие значка не останавливает фоновую службу.")
                .show();
            return Ok(());
        }
        if id == "logs" {
            let logs = connection::path(path, "logs")?;
            std::fs::create_dir_all(&logs)?;
            return service::open(&logs);
        }
        match id {
            "setup" => return self.configure(path),
            "tunnels" => return service::open_url(connection::TUNNEL_SETTINGS),
            "keys" => return service::open_url(connection::KEY_SETTINGS),
            "chatgpt" => return service::open_url(connection::CHATGPT_SETTINGS),
            "disconnect" => {
                if rfd::MessageDialog::new().set_title("Отключить аккаунт?")
                    .set_description("Туннель будет остановлен, ключ этого подключения удалён из системного хранилища. Список папок сохранится.")
                    .set_buttons(rfd::MessageButtons::OkCancel).show() == rfd::MessageDialogResult::Ok {
                    service::disconnect(path)?; self.rebuild(path)?;
                }
                return Ok(());
            }
            _ => {}
        }
        if id == "add" {
            if let Some(folder) = rfd::FileDialog::new()
                .set_title("Разрешить ChatGPT чтение этой папки")
                .pick_folder()
            {
                let mut c = Config::load(path)?;
                c.add(&folder)?;
                c.save(path)?;
                self.rebuild(path)?;
            }
            return Ok(());
        }
        if let Some((action, root_id)) = self.actions.get(id).cloned() {
            let mut c = Config::load(path)?;
            let root = c
                .roots
                .iter()
                .find(|r| r.id == root_id)
                .context("Папка уже отключена")?
                .clone();
            match action.as_str() {
                "open" => service::open(&root.path)?,
                "rename" => {
                    let mut value = root.display_name();
                    while let Some(nickname) = nickname_dialog::prompt(&value)? {
                        // The modal dialog can stay open while another process updates the config.
                        c = Config::load(path)?;
                        match c.rename(&root_id, &nickname) {
                            Ok(()) => {
                                c.save(path)?;
                                self.rebuild(path)?;
                                break;
                            }
                            Err(e) => {
                                alert(&e.to_string());
                                value = nickname;
                            }
                        }
                    }
                }
                "remove" => {
                    c.roots.retain(|r| r.id != root_id);
                    c.save(path)?;
                    self.rebuild(path)?;
                }
                "replace" => {
                    if let Some(folder) = rfd::FileDialog::new()
                        .set_directory(&root.path)
                        .set_title("Выбрать другую папку")
                        .pick_folder()
                    {
                        c = Config::load(path)?;
                        // A different folder must not inherit old document IDs.
                        c.add(&folder)?;
                        c.roots.retain(|r| r.id != root_id);
                        c.save(path)?;
                        self.rebuild(path)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}
pub fn run(path: PathBuf) -> Result<()> {
    config::initialize(&path)?;
    let lock_path = path.with_extension("tray.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    match FileExt::try_lock(&lock) {
        Ok(()) => {}
        Err(fs4::TryLockError::WouldBlock) => return Ok(()),
        Err(e) => return Err(anyhow::anyhow!("Cannot lock tray instance: {e}")),
    }
    // Hold the file lock for the entire event loop, including the diverging run call.
    let events = EventLoopBuilder::<UiEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    let events = {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        let mut events = events;
        events.set_activation_policy(ActivationPolicy::Accessory);
        events
    };
    let proxy = events.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UiEvent::Menu(event));
    }));
    let proxy = events.create_proxy();
    let health_path = path.clone();
    std::thread::Builder::new()
        .name("connection-health".into())
        .spawn(move || {
            loop {
                if proxy
                    .send_event(UiEvent::Health(service::health(&health_path)))
                    .is_err()
                {
                    break;
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        })?;
    let action_proxy = events.create_proxy();
    let setup_proxy = events.create_proxy();
    let mut ui: Option<Ui> = None;
    events.run(move |event, _, flow| {
        let _keep_lock = &lock;
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => match Ui::new(&path) {
                Ok(v) => {
                    ui = Some(v);
                    if Connection::load(&path).ok().flatten().is_none() && !connection::path(&path, "welcome.seen").is_ok_and(|p| p.exists()) {
                        let _ = setup_proxy.send_event(UiEvent::Setup);
                    } else if Connection::load(&path).ok().flatten().is_some() {
                        let proxy = action_proxy.clone();
                        let folders = path.clone();
                        std::thread::spawn(move || {
                            let result = service::control("start", &folders).map_err(|e| e.to_string());
                            let _ = proxy.send_event(UiEvent::Action(result));
                        });
                    }
                },
                Err(e) => {
                    alert(&e.to_string());
                    *flow = ControlFlow::Exit;
                }
            },
            Event::UserEvent(UiEvent::Health(health)) => {
                if let Some(ui) = ui.as_mut() {
                    ui.health(&health, &path);
                }
            }
            Event::UserEvent(UiEvent::Action(Err(error))) => alert(&error),
            Event::UserEvent(UiEvent::Setup) => if let Some(ui) = ui.as_mut() {
                if let Ok(marker) = connection::path(&path, "welcome.seen") { let _ = connection::atomic_write(&marker, b"1"); }
                rfd::MessageDialog::new().set_title("Добро пожаловать в Strigov Local MCP")
                    .set_description("Пока ни одна папка не открыта для ChatGPT.\n\n1. Добавьте нужные папки в меню «Подключённые папки».\n2. В меню «Подключение ChatGPT» создайте свой туннель и runtime API key.\n3. Выберите «Настроить или сменить подключение» и укажите полученные данные.\n4. Подключите туннель в своём аккаунте ChatGPT.\n\nДля каждой машины используйте отдельный туннель.").show();
                let _ = ui.rebuild(&path);
            },
            Event::UserEvent(UiEvent::Menu(event)) => {
                if let Some(ui) = ui.as_mut() {
                    let id = event.id.0.as_str();
                    if id == "quit" {
                        *flow = ControlFlow::Exit;
                        return;
                    }
                    if matches!(id, "start" | "stop" | "restart") {
                        ui.status.set_text("Strigov Local MCP · применяем…");
                        let action = id.to_owned();
                        let proxy = action_proxy.clone();
                        let folders = path.clone();
                        std::thread::spawn(move || {
                            let r = service::control(&action, &folders).map_err(|e| e.to_string());
                            let _ = proxy.send_event(UiEvent::Action(r));
                        });
                        return;
                    }
                    if let Err(e) = ui.menu(id, &path) {
                        alert(&e.to_string());
                    }
                }
            }
            _ => {}
        }
    })
}
