# Проверка Windows и установщика — 13 октября 2026

Среда: Windows 11 Pro 10.0.26200 x64, Rust/Cargo 1.89.0 (`x86_64-pc-windows-msvc`),
Visual Studio 2022 MSVC, Node.js 22.22.3, Python 3.11.8, PowerShell 7.6.
Все проверки выполнены нативно на Windows, а не кросс-компиляцией.

## Ручной CI (исходный коммит)

Workflow `.github/workflows/native.yml` (только `workflow_dispatch`) был выключен
на GitHub. Его включили и запустили вручную один раз на ветке `codex/release-2.1.1`:

- run: https://github.com/strigov/strigov-local-mcp/actions/runs/38367875880
- event `workflow_dispatch`, headSha `cd27662ce918c24e91d03460533dff8851c6633b`
- `test (windows-latest)` — success, `test (macos-latest)` — success

Этот запуск подтверждает только опубликованный исходный коммит. Изменения ниже
пока не запушены и проверены локально.

## Локальная проверка приложения

| Проверка | Результат |
|---|---|
| `cargo fmt --check` | PASS |
| `cargo clippy --locked --all-targets -- -D warnings` | PASS |
| `cargo test --locked` (вкл. реальный Credential Manager roundtrip на синтетической записи) | PASS, 8 тестов, 1 ignored |
| `cargo build --release --locked --bins` | PASS |
| `node --test test/native.test.mjs` с release `local-files.exe serve` | PASS 9, skip 1 (FIFO — только Unix) |
| `cargo clippy --target aarch64-apple-darwin` (статически, без запуска на Mac) | PASS |

MCP-тесты создают symlink: без режима разработчика Windows (или прав
администратора) они падают на подготовке с `EPERM`. Это предпосылка, а не дефект
приложения; после включения режима разработчика тесты проходят.

## Найденный и исправленный дефект

`tunnel-client` разбирает `mcp.commands[].command` по правилам POSIX shell и на
Windows. Путь в двойных кавычках терял обратные слэши
(`C:Users...local-files.exe: The system cannot find the file specified`),
поэтому туннель не смог бы запустить worker. Профиль туннеля теперь использует
одинарные кавычки на всех платформах (`connection::shell_word`); для `schtasks`
остаются двойные кавычки Windows (`connection::command_word`).

## Ручная проверка с настоящим аккаунтом (14.10.2026) — версия 2.1.2

Установка из ZIP, ввод настоящего tunnel ID и runtime-ключа, запуск туннеля
из трея и подключение к ChatGPT выявили ещё три дефекта, все исправлены:

- `schtasks /Create /SC ONLOGON` требует прав администратора (`Access is
  denied`). Задача «Local Files Tunnel» теперь регистрируется из XML с
  триггером входа только текущего пользователя — права администратора не нужны
  (тест `logon_task_registers_for_current_user`).
- Ошибки `schtasks` выводились кракозябрами: вывод консольных утилит в OEM-кодировке
  (CP866) читался как UTF-8. Теперь он декодируется через `MultiByteToWideChar(CP_OEMCP)`
  (тест `console_errors_are_decoded_from_oem_code_page`).
- `tunnel-client.exe` (консольное приложение), запущенный из GUI-процесса
  `local-files.exe background`, открывал видимое окно консоли. Теперь он
  запускается с `CREATE_NO_WINDOW`, а вывод пишется в
  `%LOCALAPPDATA%\Local Files\logs\tunnel.stdout.log` / `tunnel.stderr.log`.

После исправлений пользователь подтвердил: туннель запускается без окна,
коннектор в ChatGPT работает.

## Установщик

`scripts/build-installer-windows.ps1` получает закреплённый runtime 0.0.16
(SHA-256 из `scripts/tunnel-client.lock.json`), собирает release с удалением
локальных путей из бинарников и вызывает `scripts/package-windows.py`.
Результат:

```text
dist/Strigov-Local-MCP-2.1.2-windows-x64.zip
  local-files-installer.exe
  Read Me.txt
  payload/local-files.exe, tunnel-client.exe, install-manifest.json, licenses/…
```

`local-files-installer.exe`:

- без аргументов — диалог, установка в `%LOCALAPPDATA%\Programs\Strigov Local MCP`, запуск трея;
- `--check` — проверка точного списка файлов, размеров и SHA-256 по манифесту;
- `--install-to ABSOLUTE_PATH --no-launch` — установка без запуска.

Установка идёт через staging-каталог; существующая собственная установка
переносится в `Strigov Local MCP backup …`, при ошибке возвращается обратно.
Отказ без изменений: запущенное приложение или туннель, чужая непустая папка,
ссылка/junction в пути, повреждённый или неполный payload. Настройки
(`%LOCALAPPDATA%\Local Files`), Credential Manager и задача планировщика
установщиком не затрагиваются. Сборка не подписана (Authenticode нет).

## Проверка дистрибутива

`python -B scripts/verify-windows-distribution.py dist/Strigov-Local-MCP-2.1.2-windows-x64.zip --output-root …`
распаковывает именно ZIP и ставит его в изолированные каталоги с пробелами,
апострофом и кириллицей. Все 19 проверок PASS:

sidecar SHA-256, безопасные имена в ZIP, состав, `--check`, чистая установка,
совпадение установленных файлов с payload, `--version`, `init` без папок,
`check`, `status` с `connected:false`, переустановка с сохранением байтов
`folders.json`/`connection.json` и backup, отказ при запущенном приложении,
чужой папке, junction в пути, повреждённом и лишнем файле, MCP-тесты через
установленный `worker`, реальный упакованный `tunnel-client.exe` с профилем от
`connection::profile` — local health/ready, `child_state: running`,
`connected:false` на локальном endpoint без аккаунта.

Проверенный артефакт 2.1.2: 9 642 263 байта, SHA-256
`74e989cc9096e92e163c6837205febcba99e275bd0aa31307b2f5bf5ea2fdf58`
(все 19 проверок PASS; ручная проверка с аккаунтом выполнялась на сборке с теми же
исправлениями до смены номера версии).

## Не проверено (NOT_RUN)

- Автоматизированные клики в трее и диалогах установщика (проверено только вручную).
- Задача «Local Files Tunnel» при следующем входе в Windows, восстановление после
  потери сети.
- Другие версии Windows, ARM64, SmartScreen на чистой машине.
- Повторный удалённый CI на финальном коде 2.1.2.
