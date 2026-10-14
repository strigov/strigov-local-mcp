---
protocol_version: 2
orca_run: run_000000000000
plan_base: cd27662ce918c24e91d03460533dff8851c6633b
request_sha256: d5e405eabe37613c1f7d67b3459dd2dadf0a007a5a2f5fecf6ee0b5a44188fd5
slug: windows-ci-installer
created: 2026-10-13
status: done
contracts: ["C1","C2","C3"]
verify:
  - {"id":"diff-whitespace","command":"git diff --check; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":60,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[],"environment":["PATH",{"tool":"git","version_args":["--version"]}],"hermetic":false,"independent_replay":false}
  - {"id":"manual-ci-evidence","command":"$ErrorActionPreference='Stop'; $saved=Get-Content -LiteralPath '.superpowers/sdd/windows-ci-installer/ci/ci-run.json' -Raw | ConvertFrom-Json; $raw=gh run view $saved.databaseId --repo strigov/GPT-MCP-filesystem --json databaseId,url,headSha,headBranch,event,status,conclusion,jobs; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; $live=$raw | ConvertFrom-Json; if($live.databaseId -ne $saved.databaseId -or $live.headSha -ne 'cd27662ce918c24e91d03460533dff8851c6633b' -or $live.event -ne 'workflow_dispatch' -or $live.status -ne 'completed' -or $live.conclusion -ne 'success'){throw 'Manual CI binding or result failed'}; foreach($name in @('test (windows-latest)','test (macos-latest)')){ $job=@($live.jobs | Where-Object { $_.name -eq $name }); if($job.Count -ne 1 -or $job[0].conclusion -ne 'success'){throw ('Required CI job failed: '+$name)} }; $raw","checkpoint":"final","timeout_seconds":180,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[".superpowers/sdd/windows-ci-installer/ci/ci-run.json"],"environment":["PATH",{"tool":"gh","version_args":["--version"]}],"hermetic":false,"independent_replay":true}
  - {"id":"windows-distribution","command":"python -B scripts/verify-windows-distribution.py dist/Strigov-Local-MCP-2.1.1-windows-x64.zip --output-root .superpowers/sdd/windows-ci-installer/distribution-checks; exit $LASTEXITCODE","checkpoint":"final","timeout_seconds":900,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":"windows-release","progress_file":null,"inputs":["dist/Strigov-Local-MCP-2.1.1-windows-x64.zip","dist/Strigov-Local-MCP-2.1.1-windows-x64.zip.sha256","node_modules"],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]},"SystemRoot","NATIVE_BINARY","NATIVE_WORKER",{"tool":"node","version_args":["--version"]},{"tool":"python","version_args":["--version"]}],"hermetic":false,"independent_replay":true}
phases:
  - id: F1
    base: 4882d5b20a7632f648b205648b1f999514d5fa2f
    title: Нативная проверка Windows и ручной CI
    scope: native-windows-validation
    status: done
    risk: REGRESSION
    consumes: []
    produces: ["C1","C2"]
    verify:
      - {"id":"rust-format","command":"cargo fmt --check; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":120,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-clippy","command":"cargo clippy --locked --all-targets -- -D warnings; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":900,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":"windows-release","progress_file":null,"inputs":[],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-rust-tests","command":"cargo test --locked; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":900,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":"windows-release","progress_file":null,"inputs":[],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-native-mcp","command":"$ErrorActionPreference='Stop'; $env:NATIVE_BINARY=(Resolve-Path -LiteralPath 'target/release/local-files.exe').Path; Remove-Item Env:NATIVE_WORKER -ErrorAction SilentlyContinue; node --test test/native.test.mjs; exit $LASTEXITCODE","checkpoint":"integration","timeout_seconds":300,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":["target/release/local-files.exe","node_modules"],"environment":["PATH","TEMP","TMP","SystemRoot","NATIVE_BINARY","NATIVE_WORKER",{"tool":"node","version_args":["--version"]}],"hermetic":false,"independent_replay":true}
      - {"id":"manual-ci-evidence","command":"$ErrorActionPreference='Stop'; $saved=Get-Content -LiteralPath '.superpowers/sdd/windows-ci-installer/ci/ci-run.json' -Raw | ConvertFrom-Json; $raw=gh run view $saved.databaseId --repo strigov/GPT-MCP-filesystem --json databaseId,url,headSha,headBranch,event,status,conclusion,jobs; if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}; $live=$raw | ConvertFrom-Json; if($live.databaseId -ne $saved.databaseId -or $live.headSha -ne 'cd27662ce918c24e91d03460533dff8851c6633b' -or $live.event -ne 'workflow_dispatch' -or $live.status -ne 'completed' -or $live.conclusion -ne 'success'){throw 'Manual CI binding or result failed'}; foreach($name in @('test (windows-latest)','test (macos-latest)')){ $job=@($live.jobs | Where-Object { $_.name -eq $name }); if($job.Count -ne 1 -or $job[0].conclusion -ne 'success'){throw ('Required CI job failed: '+$name)} }; $raw","checkpoint":"integration","timeout_seconds":180,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[".superpowers/sdd/windows-ci-installer/ci/ci-run.json"],"environment":["PATH",{"tool":"gh","version_args":["--version"]}],"hermetic":false,"independent_replay":true}
  - id: F2
    title: Windows-установщик и проверка финального дистрибутива
    scope: windows-installer
    status: done
    risk: DATA_LOSS
    consumes: ["C1","C2"]
    produces: ["C3"]
    verify:
      - {"id":"diff-whitespace","command":"git diff --check; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":60,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[],"environment":["PATH",{"tool":"git","version_args":["--version"]}],"hermetic":false,"independent_replay":false}
      - {"id":"rust-format","command":"cargo fmt --check; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":120,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-clippy","command":"cargo clippy --locked --all-targets -- -D warnings; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":900,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":"windows-release","progress_file":null,"inputs":[],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-rust-tests","command":"cargo test --locked; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":900,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":"windows-release","progress_file":null,"inputs":[],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-packaging-tests","command":"python -B -m unittest discover -s test -p test_windows_packaging.py; exit $LASTEXITCODE","checkpoint":"directed","timeout_seconds":180,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":null,"progress_file":null,"inputs":[],"environment":["PATH",{"tool":"python","version_args":["--version"]}],"hermetic":false,"independent_replay":false}
      - {"id":"windows-distribution","command":"python -B scripts/verify-windows-distribution.py dist/Strigov-Local-MCP-2.1.1-windows-x64.zip --output-root .superpowers/sdd/windows-ci-installer/distribution-checks; exit $LASTEXITCODE","checkpoint":"final","timeout_seconds":900,"cancel_grace_seconds":5,"no_progress_seconds":60,"resource_key":"windows-release","progress_file":null,"inputs":["dist/Strigov-Local-MCP-2.1.1-windows-x64.zip","dist/Strigov-Local-MCP-2.1.1-windows-x64.zip.sha256","node_modules"],"environment":["PATH","CARGO_HOME","RUSTUP_HOME","RUSTUP_TOOLCHAIN","RUSTFLAGS","CARGO_TARGET_DIR","INCLUDE","LIB","LIBPATH","WindowsSdkDir","VCToolsInstallDir","TEMP","TMP",{"tool":"cargo","version_args":["--version"]},{"tool":"rustc","version_args":["-vV"]},"SystemRoot","NATIVE_BINARY","NATIVE_WORKER",{"tool":"node","version_args":["--version"]},{"tool":"python","version_args":["--version"]}],"hermetic":false,"independent_replay":true}
---

# Проверка Windows, ручной CI и установщик 2.1.1

## Goal

Проверить текущий Strigov Local MCP на настоящей Windows x64, выполнить существующий ручной GitHub Actions workflow и передать проверочный Windows-дистрибутив с работающим установщиком, абсолютным путём и SHA-256. Положительный результат должен опираться на исполнение Windows-бинарников и установку из выданного архива, а не на кросс-компиляцию или наличие файла с расширением .exe.

Основание: запрос «проверить работу на Windows, выполнить ручной CI и собрать установщик для Windows». Рабочая ветка `codex/release-2.1.1`, исходный HEAD `cd27662ce918c24e91d03460533dff8851c6633b`. При исследовании удалённая одноимённая ветка указывала на тот же SHA. Запуск тестов и CI при написании плана не выполнялся.

## Acceptance criteria

1. **A1 — нативная Windows.** На Windows x64/MSVC проходят `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked`, release-сборка и существующий `node --test test/native.test.mjs`. Проверяются реальные .exe, кириллица, права только на чтение, ограничения путей и перечитывание конфигурации. Ошибка создания symlink — ошибка предпосылки, не PASS; штатный Unix FIFO-тест остаётся единственным платформенным исключением этого MCP-набора. Фаза F1, C1; доказательства: C4 `checks/<run_id>/result.json` и полные логи, сводка `docs/windows-verification-2026-10-13.md`.
2. **A2 — ручной CI.** Новый `workflow_dispatch` workflow `.github/workflows/native.yml` для исходного SHA завершился success в обеих существующих задачах Windows/macOS; сохранены URL, run ID, event, headSha, результаты jobs и логи. Триггеры push/pull_request не появляются. Удалённый запуск удостоверяет только опубликованный исходный SHA; локальные изменения установщика удостоверяются A1/A4/A5 и отдельной локальной проверкой после изменений. Фаза F1, C2; доказательства: `.superpowers/sdd/windows-ci-installer/ci/ci-run.json`, `ci/jobs.log`, независимое чтение того же запуска через `gh run view`.
3. **A3 — комплектный установщик.** Получен `dist/Strigov-Local-MCP-2.1.1-windows-x64.zip`, содержащий Windows-установщик, приложение, закреплённый tunnel runtime, лицензии и инструкцию. После распаковки `local-files-installer.exe --check` проходит на Windows, `--install-to ABSOLUTE_PATH --no-launch` устанавливает комплект без Rust, Python, Node или PowerShell на стороне пользователя. Установщик без аргументов предлагает пользовательский каталог и устанавливает после подтверждения; архив и одиночный bootstrap .exe не объявляются взаимозаменяемыми. Фаза F2, C3; доказательства: финальный ZIP, sidecar SHA-256 и машинный отчёт проверки.
4. **A4 — установка и обновление без повреждения данных.** Из финального ZIP установка в новый изолированный каталог с пробелами, апострофом и кириллицей создаёт рабочую пару `local-files.exe`/`tunnel-client.exe`; `init` создаёт ноль корней, `status` показывает отсутствие подключения. Повторная установка сохраняет байты folders.json и connection.json, сохраняет прежние бинарники в backup; повреждённый payload, чужой каталог, reparse point и занятый бинарник отклоняются с ненулевым кодом и сохранением исходного состояния. На основной машине не создаются автозапуск, ярлыки, службы, реальные подключения или постоянная установка. Фаза F2, C3; доказательства: свежие `distribution-checks/<attempt>/verification.json`, инвентарь, хеши до/после и subprocess logs.
5. **A5 — интеграция и выдача именно проверенного файла.** Через установленный из ZIP `worker` независимо проходят native MCP-тесты; реальный упакованный runtime принимает профиль от `connection::profile`, запускает установленный worker и даёт local health/ready при `connected:false` на локальном тестовом endpoint. Независимый проверяющий повторяет установку и проверки из тех же неизменённых байтов ZIP, сам вычисляет SHA-256 и сравнивает с sidecar. Итог содержит абсолютный путь, размер, SHA-256, версии инструментов/ОС, исходный и фактический Git/snapshot идентификаторы, CI URL и честную матрицу PASS/FAIL/NOT_RUN. Фаза F2, C3; доказательства: независимый C4-run, его новый `verification.json`, обновлённая документация.

Инженерная приёмка A1–A5 не подтверждает авторизацию чужого аккаунта ChatGPT, вход/выход из Windows, восстановление после реальной потери сети или все клики нативных диалогов. Эти внешние сценарии требуют интерактивной Windows-сессии и/или данных владельца; их результат записывается отдельно, неизвестный результат остаётся NOT_RUN.

## Non-goals / deferred

- Публикация GitHub Release, push, включение автоматического CI, постоянная установка в профиль этой машины, изменение глобального PATH/ExecutionPolicy или системных настроек.
- MSI, самораспаковывающийся однофайловый setup, Inno Setup/WiX, автообновление и удаление программы с удалением данных. Минимальный дистрибутив — ZIP с нативным установщиком и payload, аналогично отдельному payload существующего macOS bundle.
- Windows ARM64, Linux, новая минимальная версия Windows или заявление о прохождении всех Windows 10/11. Обязательная среда — имеющаяся Windows 11 x64; версия ОС фиксируется в evidence.
- Перенос персональной установки 2.0, изменение формата настроек, объединение нескольких машин/туннелей, изменение девяти MCP-инструментов или их read-only границ.
- Authenticode, сертификат издателя и публичная готовность SmartScreen. Артефакт локальный и неподписанный, это должно быть явно указано.
- Фактическая авторизация ChatGPT, создание/перезапись задачи `Local Files Tunnel` на основной машине, тест входа в систему; для них нужен отдельный разрешённый сеанс или одноразовая VM. Компиляция, локальный fake endpoint и статический разбор schtasks не выдаются за такую проверку.

## Global Constraints

- Корень: `C:\Users\USER\src\GPT-MCP-filesystem`. Нативная PowerShell; основной воспроизводимый shell — `C:\Program Files\PowerShell\7\pwsh.exe` с `-NoProfile`. Bash/WSL не заменяют Windows-исполнение. Существующий Bash-шаг GitHub Actions на Windows можно оставить: он запускает настоящий Windows .exe.
- Сохранять Cargo package `local-files` 2.1.1, edition 2024, declared rust-version 1.88 и Node >=22. Не обещать тест минимального Rust: обнаружены Rust/Cargo 1.89.0, Node 22.22.3, Python 3.11.8, PowerShell 7.6.6, Windows 11 Pro 10.0.26200 x64 и Visual Studio 2022 MSVC 14.43.34808. Windows SDK и успешная линковка ещё требуют preflight/build; наличие compiler в PATH не доказывает их исправность.
- Предварительно проверить конкретный toolchain, компоненты rustfmt/clippy, linker/SDK, свободное место и право symlink. Не понижать Clippy и не пропускать тесты для зелёного статуса. Если locked dependencies требуют другой установленный Rust, выбрать подходящий через process-only `RUSTUP_TOOLCHAIN` и записать версию; если подходящего нет — явный blocker и запрос ограниченной подготовки, без молчаливого глобального обновления.
- Существующие команды — исходная точка: Cargo, `npm ci --ignore-scripts`, Node MCP-тесты, `python scripts/fetch-tunnel-runtime.py --platform windows-amd64`. Новый PowerShell build wrapper нужен потому, что оба существующих build .sh ограничены macOS.
- Runtime остаётся 0.0.16 из `scripts/tunnel-client.lock.json`; все три Windows-amd64 asset проверяются закреплёнными SHA-256. Нельзя заменять runtime «последней» версией. Подход macOS packager с явным allowlist и сбором лицензий переносится без копирования пользовательского профиля.
- Только необходимые Windows-исправления по воспроизводимому отказу; неожиданные изменения публичных контрактов требуют уточнения плана. Обычная реализация нового installer path уже входит в F2. Общие macOS функции не переписывать ради Windows.
- Рабочий каталог и runtime могут содержать персональные данные: payload собирается из явно указанных build outputs/vendor/licenses/docs. Не читать рабочие ключи/профили и не переносить `folders.json`, `connection.json`, `tunnel.json`, `health.url`, `welcome.seen`, логи, `.env*` или домашний путь разработчика.
- `LOCAL_FILES_DATA_DIR` и `--config` изолируют файлы, но **не** имя schtasks и **не** все системные credential записи. Не вызывать `service::control` или tray с connection.json рабочего пользователя в проверках. Допустимый vault roundtrip использует только новый случайный synthetic account и удаляет только его запись.
- Все длительные команды — через `C:\Users\USER\tools\run-check.ps1` по C3/C4, уникальные попытки внутри `.superpowers/sdd/windows-ci-installer/checks/`, фактическая Orca owner identity, полные stdout/stderr, deadline и проверка cleanup. Реальные shell/tool identities фиксируются; `hermetic:false`, потому что есть registry/network, ОС и сторонние инструменты.
- Budgets конечные: F1 до 90 минут работы, F2 до 150 минут, независимая приёмка до 45 минут; отдельных запусков/повторов — ниже. По исчерпании бюджета сохранить evidence и остаточный blocker, не продлевать молча. Планирование: до 15 минут, три focused exploratory commands, без suite; это ограничение относится к автору плана.
- Фазы последовательные, без parallel_group. Координатор записывает **реальный** `base` каждой фазы непосредственно перед dispatch. Будущие bases и результаты в этом плане не придуманы.

## Files

Файлы evidence в workspace и игнорируемые build outputs не коммитятся как исходники. Новые report/attempt paths не перезаписывают принятые evidence.

| Фаза | Create / Modify / Test | Ответственность |
|---|---|---|
| F1 | Modify/Test `native/vault.rs` | Добавить Windows-only тест реального CredWriteW/CredReadW/CredDeleteW с уникальным synthetic account, cleanup guard и без вывода секрета; production API сохранить. |
| F1 | Test `native/config.rs`, `native/connection.rs`, `native/service.rs`, `native/tray.rs`, `native/nickname_dialog.rs`, `native/filesystem.rs`, `test/native.test.mjs` | Существующие Cargo/Clippy/MCP проверки реальных Windows ветвей; не добавлять изменения без зафиксированного отказа. |
| F1 | Conditional Modify `native/service.rs`, `native/tray.rs`, `native/nickname_dialog.rs`, `test/native.test.mjs` | Только Windows-дефект, мешающий названным проверкам: cfg/lint, платформенный запуск/fixture; не отключать защитные symlink assertions. Более широкий исправляемый файл — явное обновление scope перед редактированием. |
| F1 | Test `.github/workflows/native.yml` | Сохранить только workflow_dispatch и существующую матрицу; запустить и прочитать фактический исходный remote run. Изменение workflow не требуется в выбранном дизайне. |
| F1 | Create `docs/windows-verification-2026-10-13.md`; Modify `docs/windows-validation.md` | Проверенный Windows протокол, команды, версии, исходный SHA/CI URL, фактические исключения и оставшаяся упаковка. |
| F1 | Evidence `.superpowers/sdd/windows-ci-installer/ci/ci-run.json`, `ci/jobs.log`, `ci/dispatch.log` | Оригинальные stdout GH-команд для единственного нового remote run; runtime IDs получаются при исполнении. |
| F2 | Modify `native/installer.rs`; Create `native/installer_windows.rs` | Windows dispatch и Windows-реализация существующих installer CLI + интерактивного подтверждения. macOS path, codesign и LaunchAgents сохранить в своей cfg-ветви. |
| F2 | Modify `Cargo.toml`, `Cargo.lock` | Минимальная Windows-only SHA-256 зависимость `sha2 = "0.10"` для проверки manifest; lock обновлять только из-за неё, не делать массовый upgrade. |
| F2 | Create `scripts/build-installer-windows.ps1` | Нативный preflight, fetch pinned runtime, Cargo release build, вызов packager, итоговые пути и хеш. |
| F2 | Create `scripts/package-windows.py` | Allowlist, vendor/license inventory, manifest с размерами и SHA-256, чистый staging, ZIP и sidecar; без системной установки. |
| F2 | Create `scripts/verify-windows-distribution.py` | Изолированное распаковывание и установка именно ZIP, negative/update tests, installed-worker MCP, bounded runtime probe, JSON evidence с hash. |
| F2 | Create/Test `test/test_windows_packaging.py` | Узкие fixtures для неполного payload/лишних файлов, manifest/path traversal и упаковки без личных данных; новые scripts можно импортировать без запуска build. |
| F2 | Modify/Test `native/connection.rs` | Расширить существующий ignored `packaged_runtime_accepts_generated_profile_without_external_account` на Windows, включая layout installed directory, Windows environment и надёжное завершение своего дерева процессов; сохранить macOS ветвь. |
| F2 | Test `test/native.test.mjs`, `native/installer_windows.rs` | Исполнение установленного worker; directed тесты проверки/копирования/отказов Windows installer. |
| F2 | Create `docs/distribution-readme-windows.txt`; Modify `README.md`, `docs/windows-validation.md`, `docs/windows-verification-2026-10-13.md` | Инструкция распаковать весь ZIP, запустить installer, расположение приложения/данных, сохранение настроек и финальные факты Windows-проверки. Не менять macOS-only readme ради Windows. |
| F2 | Build `dist/Strigov-Local-MCP-2.1.1-windows-x64.zip`, `dist/Strigov-Local-MCP-2.1.1-windows-x64.zip.sha256` | Локальный передаваемый дистрибутив и digest; не публиковать. |
| F2 | Evidence `.superpowers/sdd/windows-ci-installer/distribution-checks/<attempt>/verification.json` и logs | Уникальный запуск для автора и отдельный для независимого проверяющего; содержат точный hash проверенного ZIP. |

Отдельной architecture memory в исследованных файлах нет; актуальные границы Windows/пакета документируются в существующих README и windows-validation в F2.

## Contracts

### C1 — нативный Windows executable и проверка приложения

Producer F1; consumer F2 (`build-installer-windows.ps1`, `verify-windows-distribution.py`).

Существующие интерфейсы, сохраняемые без изменения:

- Cargo bins `local-files` (`native/main.rs`) и `local-files-installer` (`native/installer.rs`, feature desktop). Сборка `cargo build --release --locked --bins` создаёт `target/release/local-files.exe` и `target/release/local-files-installer.exe`.
- `local-files.exe --version` возвращает `Strigov Local MCP 2.1.1`. Команды `init|check|status|serve|worker|background|tray [--config ABSOLUTE_PATH]` уже обрабатывает main. Проверки используют stdio capture/ожидание завершения: Windows GUI subsystem нельзя проверять одним запуском из интерактивной консоли без ожидания.
- `config::data_dir() -> PathBuf`: абсолютный `LOCAL_FILES_DATA_DIR`, иначе `%LOCALAPPDATA%/Local Files`; `Config` остаётся version 1, roots, next_root_id, у Root id/path/optional nickname. `Connection` остаётся version 1, tunnel_id, credential_id; ключ не входит в JSON.
- `connection::tunnel_binary() -> Result<PathBuf>` на Windows ожидает `tunnel-client.exe` рядом с текущим exe. `connection::profile(connection: &Connection, exe: &Path, folders: &Path) -> Result<serde_json::Value>` формирует `mcp.commands[0].command` с `worker --config`, `api_key: env:CONTROL_PLANE_API_KEY`, localhost health URL.
- Node test consumer получает абсолютный `NATIVE_BINARY` и `NATIVE_WORKER=1` только для установленного worker. При исходной CI parity используется serve, как в существующем workflow.
- Windows-only vault test — предлагаемый тест, не новый production API; существующие `vault::set(account: &str, secret: &str) -> Result<()>`, `get(account: &str) -> Result<Vec<u8>>`, `delete(account: &str) -> Result<()>` сохраняются.

C1 подтверждает конкретную Windows проверку, а не переносит PASS старого macOS отчёта на Windows. F2 обязан заново собрать release после изменения исходников.

### C2 — точная привязка ручного CI

Producer F1; consumers F2 delivery docs и независимая final-проверка.

Единственный workflow `.github/workflows/native.yml`: `on.workflow_dispatch`, matrix `macos-latest, windows-latest`; rustfmt, Clippy, Rust tests, release build, npm ci и native MCP. Не менять workflow только ради упаковки: новый установщик локально проверяется F2.

`ci-run.json` — сохранённый оригинальный JSON `gh run view RUN_ID --repo strigov/GPT-MCP-filesystem --json databaseId,url,headSha,headBranch,event,status,conclusion,jobs`. Требуемые факты: event=workflow_dispatch; headSha=исходный полный plan_base; status=completed; conclusion=success; обе matrix jobs success. `jobs.log` — полный `gh run view RUN_ID --log`. Секреты GH CLI не записываются.

Dispatch выполняется один раз на существующей удалённой ветке после повторного сравнения её SHA с plan_base. Если ref изменился, нет workflow на ref или запуск нельзя однозначно определить по ID/event/SHA/времени — остановить этот пункт и сообщить blocker; не публиковать изменения, не создавать тег, не выдавать старый push-run за ручную проверку. Run ID не предсказывается.

### C3 — полный Windows installer distribution и проверяемая установка

Producer F2; consumers пользователь и независимый final verifier. Следующие Windows-интерфейсы **предлагаются**, сейчас их нет.

`pwsh -NoProfile -File scripts/build-installer-windows.ps1` из корня: проверяет native Windows x64, получает pinned windows-amd64 runtime существующим fetch script, задаёт process-only remap-path-prefix, выполняет Cargo release --locked --bins и `python -B scripts/package-windows.py`. Не меняет профиль shell и global config; все native exit codes проверяются. Если `CARGO_TARGET_DIR` отличается от ожидаемого target, wrapper либо использует явно вычисленный output path, либо заранее отказывает — не упаковывает старые бинарники.

Структура выдаваемого ZIP:

```text
local-files-installer.exe
Read Me.txt
payload/
  local-files.exe
  tunnel-client.exe
  install-manifest.json
  licenses/
    tunnel-client-LICENSE.txt
    tunnel-client-NOTICE.txt
    tunnel-client-runtime-v0.0.16-windows-amd64-licenses.txt
    tunnel-client-runtime-v0.0.16-windows-amd64.spdx.json
    Third Party Notices.txt
```

Runtime executable обнаруживается по фактическому распакованному upstream inventory; допустимы имя tunnel-client.exe или tunnel-client-runtime.exe, ровно один кандидат, на выходе всегда tunnel-client.exe. LICENSE/NOTICE и закреплённые non-ZIP assets проверяются по наличию; отсутствие любого обязательного элемента — ошибка сборки. Third Party Notices собирается по `cargo metadata --locked --format-version 1` и файлам лицензий зависимостей, по существующему macOS pattern.

`install-manifest.json` — UTF-8 JSON:
`{schema_version:1, product:"app.localfiles.desktop", version:"2.1.1", platform:"windows-amd64", files:[{path:string, bytes:integer, sha256:64_lowercase_hex}]}`.
Список точный и отсортированный, paths относительные POSIX, без .., абсолютных/drive/UNC путей и NTFS alternate streams; manifest не хеширует сам себя. Никаких произвольных extra files. SHA-256 проверяет целостность, но не является подписью издателя.

Нативный `local-files-installer.exe`:

- `--check`: находит sibling payload, проверяет schema/platform/version, inventory, длины и SHA-256; exit 0 без файлов настроек, служб, GUI и записи в системные каталоги; неверный payload — nonzero.
- `--install-to ABSOLUTE_PATH --no-launch`: без GUI и дополнительных запросов устанавливает в заданный каталог. Проверяет все предки target на reparse/symlink, payload на reparse/symlink, чужой непустой destination отклоняет. Отдельный staging и проверка завершаются до replacement; существующий собственный product с корректным manifest переносится в уникальный соседний `Strigov Local MCP backup ...`, затем stage становится destination; ошибка восстанавливает прежний каталог. Не перезаписывать чужие файлы и не очищать backup.
- Без аргументов: существующий rfd confirmation, default `%LOCALAPPDATA%/Programs/Strigov Local MCP`, тот же install path; после успеха предложить/выполнить запуск установленного tray, не регистрируя installer-owned автозапуск. На этой машине QA использует только --no-launch и изолированный launch. Обновление занятого приложения завершается объяснимым отказом «закройте приложение», без убийства всех процессов по имени.
- Никаких действий с Credential Manager, folders.json/connection.json и schtasks при установке. Пользовательские данные сохраняют прежний путь C1. В macOS сохраняются имеющиеся CLI, .app layout, codesign и LaunchAgents; Windows `cfg` не вызывает macOS команды.

`python -B scripts/verify-windows-distribution.py ZIP --output-root DIR`: native Windows only, создаёт новый уникальный child output directory (никогда не перезаписывает evidence), безопасно распаковывает ZIP, вызывает реальные installer/exe и сохраняет subprocess exit/output. Для каждой попытки создаёт отдельные app/settings/documents paths, никогда не подменяет HOME или глобальный LOCALAPPDATA. При запуске приложения передаёт абсолютный LOCAL_FILES_DATA_DIR или --config в окружении дочернего процесса.

`verification.json` минимум:
`{schema_version:1, artifact:{path,bytes,sha256}, source:{git_head,dirty}, platform:{os,arch}, tools:{...}, checks:[{id,status,exit_code,log_paths}], installed_files:[{path,bytes,sha256}], user_settings_changed:false, gui_interaction:"PASS|NOT_RUN", external_account:"NOT_RUN", system_autostart:"NOT_RUN", signed:false}`.
Обязательные automated checks должны быть PASS; неверный/missing/timeout результат даёт nonzero verifier, а не report со статусом success. Фактический C4 snapshot/result снаружи привязывает исходники и artifact; JSON приложения не заменяет sealed C4 evidence. Sidecar содержит lowercase SHA-256 и точное имя ZIP, вычисленные из финальных байтов.

## Test strategy

### Подготовка и конечные checkpoints

Никакой suite при планировании. Реализация сначала фиксирует среду и зависимости; подготовительные команды не смешиваются с независимыми checks:

1. До F1 suites: preflight ОС/arch/tool versions/linker/SDK, проверка symlink на временном fixture с cleanup; `npm ci --ignore-scripts` один раз (300 с). Выход любого native subprocess проверяется сразу. Настройки Developer Mode/политик автоматически не менять.
2. F1 directed: format (120 с), Clippy (900 с), Cargo tests (900 с). Они покрывают новую synthetic vault проверку и существующие unit paths.
3. F1 integration: `cargo build --release --locked --bins` (1200 с, отдельный C3/C4 build run с source inputs; build output не объявлять входом этого же build); после успешной сборки `windows-native-mcp` (300 с). Node modules и release exe — явные immutable inputs именно test-run, не dependency preparation.
4. В F1 один remote `gh workflow run native.yml --repo strigov/GPT-MCP-filesystem --ref codex/release-2.1.1` (60 с на dispatch). Сохранить время начала и stdout. Через `gh run list --workflow native.yml --branch codex/release-2.1.1 --event workflow_dispatch --json databaseId,headSha,createdAt,url,status` однозначно установить новый ID; не перезапускать неоднозначный dispatch. `gh run watch RUN_ID --repo strigov/GPT-MCP-filesystem --exit-status --interval 10` — до 1800 с под owned runner. Сохранить raw view JSON и полный --log. При timeout не считать удалённый процесс закончившимся и не dispatch повторно; запись статуса и дальнейшее согласованное наблюдение того же ID в пределах бюджета.
5. F2 directed после правок: format, Clippy, Rust unit tests и узкие Python packaging tests. Build command `pwsh -NoProfile -File scripts/build-installer-windows.ps1` (1800 с, C3/C4). Перед выпуском — никаких mutable declared input ZIP во время build; финальный zip включается в inputs следующих проверок.
6. F2 integration внутри distribution verifier: payload/check/install/reinstall/negative tests; установленный worker через `NATIVE_BINARY=installed/local-files.exe`, `NATIVE_WORKER=1`; `cargo test --locked --bin local-files packaged_runtime_accepts_generated_profile_without_external_account -- --ignored` с `LOCAL_FILES_TEST_APP=installed directory`. Runtime probe использует настоящий `connection::profile` с локальным endpoint и synthetic key; assert live/ready/child_state, connected=false. Порождение и завершение своего полного дерева процессов обязательно, никакой регистрации scheduled task.
7. F2 final: `windows-distribution` на окончательном ZIP (900 с); отдельный независимый проверяющий физически повторяет тот же check с новым owner, новым output directory и причиной independent_replay. Дополнительно independently re-read CI C2 (180 с) и `git diff --check` (60 с). Файл после accepted hash не пересобирать; любая пересборка делает прежний artifact evidence устаревшим.

C4 lists в frontmatter — источник выбора gate; `--phase F1 --checkpoint integration` выбирает только F1, `--phase F2 --checkpoint final` — F2, без phase выбирается верхний delivery verify. Directed/integration/final включают предыдущие уровни согласно runtime, поэтому не вызывать последовательно все три полных gate без причины; автор выполняет checkpoints по мере готовности, итоговый gate выполняет один выбранный набор. Если подготовительные npm/build prerequisites отсутствуют у отдельного verifier, он готовит их один раз под своим C3 owner, фиксирует preparation/linked dependencies и затем выполняет тест; это не reusable HEAD/BASE evidence.

Все команды verify работают из корня, используют PowerShell, явные дополнительные inputs/tools/env и конечные deadlines. MSVC/SDK version details дописываются в environment evidence (не предполагать hermetic по наличию tool PATH). `independent_replay:true` у native MCP, CI evidence и distribution означает новый физический test/read run, не повторную сборку всего проекта и не новый GitHub dispatch. При baseline диагностиках зависимости/ignored binaries не подставляются как якобы независимые от HEAD; exact-input C4 contract обязателен.

### Матрица приёмки и бюджет повторов

| Критерий | Producer/consumer проверки | Независимое подтверждение |
|---|---|---|
| A1 | F1 directed + Windows release build + native MCP serve; synthetic credential roundtrip | F1 replay native MCP под новым owner на тех же объявленных bytes; сверка Cargo лога/версий |
| A2 | F1 remote workflow_dispatch, обе matrix jobs | GET того же run и jobs с headSha/event; никакого second dispatch |
| A3 | F2 package fixture/unit tests, build, реальный installer --check | Тот же ZIP на новой временной установке final verifier |
| A4 | F2 corruption/missing/foreign/reparse/locked-file и clean/update checks | Независимое повторение всех required automated distribution checks |
| A5 | Установленный worker + реальный bundled tunnel profile probe + SHA | Новый C4-run на том же ZIP, независимый Get-FileHash/Python hashlib |

Полные Cargo/Node наборы не требуются на каждом промежуточном edit. На фазу — один исходный запуск применимых проверок; дополнительные запуски только после релевантной правки или именованной диагностической гипотезы, максимум две повторные попытки одного failing check в выделенный бюджет. Независимый replay — отдельная обязательная попытка и не расходует диагностический повтор. Не повторять npm ci, remote dispatch или release build при неизменных входах только ради нового отчёта.

macOS: обязательная baseline CI matrix остаётся. Новую F2 macOS-ветку физически на этой Windows-машине не исполнять; сохранить существующее поведение cfg и зафиксировать final-source macOS NOT_RUN. Если F2 затрагивает общую installer логику или production profile behavior, требуются отдельное доступное macOS доказательство или пересмотр scope; не выдавать baseline CI за проверку новых общих изменений.

Интерактивный optional checkpoint (до 10 минут): tray из изолированной установки без connection.json, видимость значка/меню, добавление/переименование синтетической папки, закрытие; если есть оператор/инструмент GUI — сохранить снимки и результаты. Один живой процесс не доказывает правильные клики/диалоги. При отсутствии интерактивного доступа явно NOT_RUN; A1–A5 не получают выдуманный GUI PASS.

## Risks / unknowns / assumptions

- **Формат поставки выбран:** ZIP с нативным installer .exe и полным payload считается установщиком для этой локальной проверки. Одиночный self-contained .exe не требуется исходным запросом. Выдать путь ZIP и hash; отдельно можно указать внутренний exe, но без обещания его автономности.
- **Удалённый CI и финальный source различаются.** На момент исследования remote release branch совпадала с plan_base; no-push запрещает разместить F1/F2 изменения для нового remote CI. Поэтому A2 явно baseline, финальный Windows path проверяется локально. Если требуется именно remote CI финального commit, это материальное изменение требования и blocker без разрешения push; его нельзя скрыть зелёной ссылкой.
- **Toolchain age.** Rust 1.89 удовлетворяет manifest floor, но сборка locked dependencies ещё не проверена; отсутствие инструмента/допустимого toolchain фиксируется до долгих циклов. План не утверждает, что компиляция уже прошла.
- **Symlink privilege.** test/native.test.mjs создаёт ссылки в каждой fixture; отсутствие права может остановить весь набор. Не обходить sandbox/path tests, не повышать права и не менять Windows security policy молча.
- **Replacement/locked files.** Windows не даёт обычный Unix rename-over-executable. Обязательны проверяемый stage/backup/rollback и отказ на busy/foreign/reparse destinations. Поэтому F2 имеет риск DATA_LOSS и negative checks, а не просто Copy-Item -Force.
- **Runtime integration сейчас macOS-only.** На Windows нельзя оставить libc::kill, /usr/bin:/bin и .app layout в ignored test. Нужно фактическое выполнение Windows counterpart и завершение детей, а не одна новая cfg-аннотация.
- **Vault и schtasks isolation различаются.** Credentials проверить временным уникальным account, существующую задачу с фиксированным именем не трогать. Реальный task restart/logon и реальные ключи вне автоматической приёмки.
- **Подписи/GUI/другие машины.** Неподписанный файл и инженерные проверки не доказывают отсутствие SmartScreen prompt, работу на другой версии ОС и полную пользовательскую приёмку; итоговые документы должны различать эти факты.
- **Нет фальшивых положительных исходов.** Обязательный check с FAIL/timeout/missing prerequisites оставляет соответствующий A-критерий незавершённым. Можно сохранить локальный ZIP для диагностики, но нельзя назвать его принятым артефактом.

## Phases

## F1: Нативная Windows-проверка и ручной baseline CI

**Files in this phase:** Modify/Test `native/vault.rs`; Test `native/config.rs`, `native/connection.rs`, `native/service.rs`, `native/tray.rs`, `native/nickname_dialog.rs`, `native/filesystem.rs`, `test/native.test.mjs`, `.github/workflows/native.yml`; Conditional Modify `native/service.rs`, `native/tray.rs`, `native/nickname_dialog.rs`, `test/native.test.mjs` только по воспроизводимому отказу; Create `docs/windows-verification-2026-10-13.md`; Modify `docs/windows-validation.md`. Evidence paths и ответственность перечислены в Files.

**Interfaces:** Consumes: nothing. Produces: C1 — существующие CLI `local-files.exe <command> [--config PATH]`, абсолютный NATIVE_BINARY, config/connection version 1 и факты Windows-выполнения; C2 — raw `gh run view` JSON с databaseId/url/headSha/event/status/conclusion/jobs и полные job logs.

Результат: честное Windows evidence для A1 и ручное удалённое baseline evidence для A2. Зафиксировать environment, выполнить preflight/dependency preparation, добавить безопасный Windows credential roundtrip, провести локальные directed/integration команды. При ошибках применять только минимальные относящиеся к Windows исправления, не ослабляя tests/lints. Сформировать protocol report по реальным logs.

Один раз dispatch native.yml на remote release branch после SHA guard; связать ID с фактическим новым запуском, дождаться bounded result, сохранить original JSON/logs. Независимый F1 gate повторяет native MCP и read-only CI result. Фаза отдельно коммитится как Windows-исправления/тесты/документированные результаты; приложение остаётся собираемым, отсутствие нового установщика ещё отражено в документации. Ни статический clippy для Windows на Mac, ни прошлый push-run не закрывают A1/A2. Если required check не прошёл, phase status остаётся незавершённым с конкретным evidence/blocker.

## F2: Полный Windows-установщик и независимая приёмка ZIP

**Files in this phase:** Modify `native/installer.rs`, `native/connection.rs`, `Cargo.toml`, `Cargo.lock`, `README.md`, `docs/windows-validation.md`, `docs/windows-verification-2026-10-13.md`; Create `native/installer_windows.rs`, `scripts/build-installer-windows.ps1`, `scripts/package-windows.py`, `scripts/verify-windows-distribution.py`, `test/test_windows_packaging.py`, `docs/distribution-readme-windows.txt`; Test `native/installer_windows.rs`, `native/connection.rs`, `test/test_windows_packaging.py`, `test/native.test.mjs`. Exact dist/evidence paths определены в Files.

**Interfaces:** Consumes: C1 — Windows application CLI, config isolation и sibling tunnel_binary layout; C2 — фактический manual baseline CI report. Produces: C3 — предложенные `build-installer-windows.ps1`, `package-windows.py`, Windows `installer --check|--install-to PATH --no-launch`, ZIP layout/manifest и `verify-windows-distribution.py ZIP --output-root DIR`/verification.json.

Результат A3–A5: реализовать Windows cfg branch существующего installer, проверяемый payload/backup/отказ, wrapper/packager, verifier и Windows counterpart реального runtime integration test. Не сначала включать runtime path с будущим обещанием проверок: phase deliverable включает working implementation и тест из выданного архива.

Собрать release и полный ZIP. Проверить installer CLI, чистую установку, сохранность настроек/backup, негативные сценарии и actual installed-worker/runtime. Final verifier под новым владельцем независимо распаковывает те же bytes, повторяет required checks, считает hash; до его прохождения архив остаётся candidate. После приёмки обновить docs фактическими результатами и разницей CI baseline/final source. Передать абсолютный путь ZIP, размер, lowercase SHA-256, sidecar, CI URL и evidence paths; no push/publish/permanent install. Любой required FAIL/unknown — незавершённая фаза, точный список оставшегося; optional GUI/external-account NOT_RUN — отдельная явная граница результата.

