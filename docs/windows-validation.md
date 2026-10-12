# Windows: подготовка и проверка

Код macOS и Windows остаётся в одном Rust-пакете. Отдельная Windows-ветка
версий не нужна; установщики для каждой платформы выпускаются отдельно.
Автоматический CI отключён. Команды ниже запускаются вручную.

## Исправления после проверки 2.1.1

Windows-проверка остановилась на `cargo clippy --locked --all-targets -- -D warnings`:

- `native/service.rs`: импорт `PathBuf` теперь включается только на macOS,
  где он используется для LaunchAgents.
- `native/tray.rs`: изменяемый цикл событий нужен только для настройки macOS;
  Windows получает обычный неизменяемый объект.
- `native/nickname_dialog.rs`: упаковка Win32-шаблона использует `as_chunks::<2>()`
  вместо `chunks_exact(2)`. Порядок слов и выравнивание сохранены; API доступен
  с Rust 1.88, указанного в `Cargo.toml`.

Предупреждения не подавляются, строгость Clippy не снижена.

## Выполненные проверки — 12 октября 2026

На Mac с отдельным Rust 1.99.0, соответствующим версии из упавшего CI,
успешно выполнена проверка Windows x64:

```sh
cargo clippy --locked --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

Выход 0, предупреждений нет. Проверены Windows-условные ветви приложения
и тестовых целей. Линковка `.exe` и выполнение Windows-тестов этой командой
не производятся.

На macOS arm64 с Rust 1.98.1 также прошли `cargo fmt --check`,
`cargo clippy --locked --all-targets -- -D warnings` и `cargo test --locked`:
5 тестов прошли, 1 тест упакованного runtime пропущен согласно его `ignore`.
Установленное приложение и действующий туннель в этой проверке не участвовали.

## Проверка на Windows x64

Нужны Rust с MSVC toolchain и Clippy/rustfmt, средства сборки Visual C++
и Windows SDK, Node.js 22 для интеграционных тестов. Самому приложению Node.js
не нужен. Тесты создают символьные ссылки: учётная запись должна иметь право
на их создание; ошибки создания ссылок нельзя считать успешной проверкой.

Из корня репозитория в PowerShell выполняйте команды по очереди,
останавливаясь при ненулевом `$LASTEXITCODE`:

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked --bin local-files
npm ci --ignore-scripts
$env:NATIVE_BINARY = (Resolve-Path 'target/release/local-files.exe').Path
node --test test/native.test.mjs
Remove-Item Env:NATIVE_BINARY
```

Эти проверки не подтверждают работу трея, Credential Manager, автозапуска
и реального подключения к ChatGPT. Для них нужна проверка на Windows-машине.

## Установщик и оставшаяся проверка

`native/installer.rs` и `scripts/build-installer.sh` пока предназначены для macOS.
Сборка `local-files-installer.exe` не создаёт рабочий Windows-установщик.
Исторический `scripts/install-native-windows.ps1` рассчитан на версию 2.0.0
и не подходит для текущего приложения.

Для Windows-дистрибутива ещё нужно подготовить установщик с приложением,
закреплённым `tunnel-client.exe` и лицензиями, без пользовательских настроек
и ключей. После этого проверить:

1. Чистую установку под обычной учётной записью и первый запуск без общих папок.
2. Трей, выбор папок и их прозвищ, включая пути с пробелами и кириллицей.
3. Сохранение ключа в Credential Manager и подключение своего туннеля к ChatGPT.
4. Остановку, повторный запуск, восстановление после потери сети и входа в Windows.
5. Обновление с сохранением папок, прозвищ, ключа и туннеля.

Готовый Windows-установщик пока не опубликован.
