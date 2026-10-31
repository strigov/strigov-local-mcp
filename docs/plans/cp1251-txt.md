---
spec: docs/specs/2026-10-30-cp1251-txt.md
orca_run: run_988382cf4a72
status: done
phases:
  - id: F1
    title: Тесты и behavioral RED
    implementer: gemini
    risk: none
    area: backend
    hard: false
    covers: [AC1, AC18]
    verify:
      - python3 scripts/verify-cp1251.py red
      - cargo fmt --check
      - cargo clippy --locked --all-targets -- -D warnings
      - cargo test --locked cp1251_lock
    status: done
  - id: F2
    title: Декодер Windows-1251, интеграция, документация и локальный GREEN
    implementer: gemini
    risk: risky
    area: backend
    hard: false
    covers: [AC2, AC3, AC4, AC5, AC6, AC7, AC8, AC9, AC10, AC11, AC12, AC13, AC14, AC15, AC16, AC17, AC18, AC19, AC25, AC26, AC27]
    verify:
      - cargo fmt --check
      - cargo clippy --locked --all-targets -- -D warnings
      - cargo test --locked
      - cargo build --release --locked --bin local-files
      - node --test test/native.test.mjs
      - python3 scripts/verify-cp1251.py local
    status: done
  - id: F3
    title: Изолированный GREEN на native Windows HPC
    implementer: sol
    risk: risky
    area: backend
    hard: true
    covers: [AC20, AC21, AC22, AC23, AC24, AC27]
    verify:
      - git bundle verify artifacts/cp1251-fix/hpc-green/cp1251.bundle
      - python3 scripts/verify-cp1251.py hpc
    status: done
---

# План: чтение и поиск TXT в Windows-1251

Спека: `docs/specs/2026-10-30-cp1251-txt.md`. Ниже ссылки «Дизайн», «Тесты», «Процесс проверки» и «Синтетические фикстуры» ведут в её разделы.

## Общие условия

- **Порядок приёмки.** Воркеры не коммитят. Фаза принимается по verify и независимым ревью незакоммиченного черновика. Только после этого координатор коммитит её в ветку `fix/cp1251-txt`.
  - Ветка начинается от коммита координатора со спекой и планом; его родитель — `13dbae0fa651aeb8acca53b28efc0f58cd259c63`. Если ветки ещё нет, F1 создаёт её командой `git switch -c fix/cp1251-txt`, без коммита.
  - F2 начинается от принятого коммита F1, F3 — от принятого коммита F2.
  - Push не выполняется. Версия, `Cargo.toml`, `Cargo.lock`, `dist/`, установщики и `scripts/install-*` не трогаются (AC24).
- **Привязка доказательств к черновику** — через отпечаток исходников, а не через HEAD или чистое дерево. `fingerprint.json` — это SHA-256 каждого отслеживаемого и неотслеживаемого файла в `native/`, `test/` и `scripts/`, а также `README.md`, `Cargo.toml`, `Cargo.lock`, `package.json` и `package-lock.json`. В отдельном ключе `tests` лежат SHA-256 строк от `mod tests {` до конца `native/filesystem.rs` и `native/documents.rs` и SHA-256 всего `test/native.test.mjs`.
  - Отпечаток пишется вместе с логами. Режим проверки пересчитывает его и требует равенства; `scripts/verify-cp1251.py` в общий отпечаток не входит.
  - Если исходники изменились после записи, доказательства снимаются заново.
- **Verify** — одна программа на команду: без `&&`, конвейеров, `$()`, циклов, `cd` и префиксов окружения. Составные проверки живут в `scripts/verify-cp1251.py`.
  - Это stdlib Python. В F1 появляются общие хелперы и режимы `record-red` и `red`, F2 добавляет `record-local` и `local`, F3 — `hpc`.
  - Хелперы пишутся один раз и переиспользуются: отпечаток, чтение `.exit`, разбор логов, запуск подпроцесса с логом.
  - `record-*` запускают команды спеки через `subprocess`. Вывод идёт прямо в `<имя>.log` (stdout и stderr), в `<имя>.exit` пишется настоящий `returncode`. Переменные окружения передаются через `env`, а не через shell.
  - Режим завершается с кодом 0 только при выполнении всех своих проверок, иначе печатает первую невыполненную и выходит с 1.
- **Где выполнять.** Команды выполняются из корня основного checkout `/Users/strigov/Documents/Claude/Projects/projects-code/GPT-MCP-filesystem`.
  - `artifacts/` и `target/` там — gitignored-симлинки на `/Volumes/Samsung990/build-cache.noindex/GPT-MCP-filesystem/…`, а `node_modules/` и `dist/` — обычные каталоги.
  - В отдельном worktree нужен такой же симлинк `artifacts` и `npm ci --ignore-scripts`. `dist/` проверяется по абсолютному пути основного checkout.
- **Счётчик Node-тестов.** Node 26 со стандартным репортёром пишет `ℹ pass 1`, а не `# pass 1`. Проверки принимают обе формы (`^(ℹ|#) pass 1$`), команды спеки не меняются.

## F1: Тесты и behavioral RED

Цель — RED из AC1: тесты из раздела «Тесты» и две изменённые фикстуры AC18 при неизменном production-коде. Тесты пишутся один раз. Изменения в них после RED ловит `local` в F2; исправлять их можно только через эскалацию координатору, потому что RED придётся снять заново.

1. **Охранные снимки до любой сборки** (AC24, проверяются в F3), выполняются воркером вручную:
   ```sh
   mkdir -p artifacts/cp1251-fix/guards
   (cd /Users/strigov/Documents/Claude/Projects/projects-code/GPT-MCP-filesystem && find dist | LC_ALL=C sort) > artifacts/cp1251-fix/guards/dist-before.txt
   pgrep -fl local-files > artifacts/cp1251-fix/guards/pgrep-before.txt
   ```
   Ожидаются процессы установленного приложения (`…/local-files tray` и `…/local-files serve --config …`). Если в снимок попал короткоживущий чужой процесс, снимок снимается заново.
2. **Rust, `native/filesystem.rs`** — в конце файла новый `#[cfg(test)] mod tests` (строка `mod tests {` с нулевым отступом).
   - Хелперы:
     - `Filesystem::new(Config { version: 1, roots: vec![Root { id: "work".into(), path, nickname: None }], next_root_id: 1 })` на `tempfile::tempdir()`, как в `native/doc_search.rs:128`, и вызовы только через `Filesystem::call`;
     - хелпер успешного вызова с `assert!(r.is_ok(), "{tool} {args}: {:?}", …)`, чтобы на RED тест падал на assertion и лог содержал `NOT_TEXT`;
     - хелпер кода ошибки.
   - Фикстуры — по таблице «Синтетические фикстуры»:
     - `TEXT` и `TYPO` — однострочные литералы с `\r\n` и `\u{a0}`;
     - CP1251 — через `encoding_rs::WINDOWS_1251.encode` с проверкой отсутствия ошибок;
     - длины: `cp1251.txt` 116 байт, `utf8.txt` 192, `utf8-bom.txt` 195, `damaged-utf8.txt` 193 (`FF` после первого CRLF), `large-cp1251.txt` 5 242 881, `huge.txt` 67 108 865 (через `File::set_len`);
     - `archive.zip` — `zip::ZipWriter` со Stored-элементом `cp1251.txt`, как `depth_cap_zip` в тестах `native/documents.rs`.
   - Каталоги: отдельный `tempdir` на основной набор и на каждый из наборов `controls`, `ambiguous`, `large`, `huge` и `zip`. Symlink и `.env` в Rust-тестах не создаются: AC17 проверяет только Node, а эти тесты в F3 идут на Windows.
   - Девять тестов с именами и покрытием по таблице «Тесты»: `cp1251_text_reads_match_utf8_in_every_tool`, `cp1251_ranges_max_chars_and_cursors`, `cp1251_typography_and_yo`, `cp1251_folder_search_counts_undecodable`, `cp1251_zip_member_and_limits`, `cp1251_damaged_and_binary_messages`, `cp1251_nbsp_and_ambiguous_bytes`, `cp1251_lock_utf8_and_bom_unchanged`, `cp1251_lock_rejects_binary_and_controls`.
   - Lock-тесты не проверяют поле `encoding`. Для `utf8.txt` и `utf8-bom.txt` они сверяют значения ответов 2.2.0 из `artifacts/cp1251-hpc-20261030/results.json`:
     - `text` — `TEXT` с LF;
     - `totalLines` и `endLine` = 4;
     - `locator.line` = 2;
     - `snippet` = `Замещающие сделки: взыскание убытков.`;
     - текст `utf8-bom.txt` начинается с U+FEFF.
   - STALE_CURSOR из AC5: файл перезаписывается содержимым другой длины, потому что отпечаток — это размер и mtime.
3. **`native/documents.rs:1403`**: `b"valid\n\xff"` → `b"valid\n\xff\x00"`. Больше в файле ничего не меняется.
4. **`test/native.test.mjs`**:
   - строка 402: `[65,10,0xff]` → `[65,10,0xff,0x00]`;
   - два теста с точными именами `cp1251: Windows-1251 text is read and searched by every tool` и `cp1251 lock: UTF-8, BOM, binary, limits and access rules are unchanged`, покрытие — как в «Тестах»;
   - раскладка каталогов: `work/cp1251/`, `work/cp1251-controls/`, `work/cp1251-ambiguous/`, `work/cp1251-large/`, `work/cp1251-huge/` и `work/cp1251-access/` (`.env` и абсолютный symlink `escape-cp1251.txt` на файл с `СЕКРЕТ_1251` в `cowork`);
   - CP1251-кодировщик в Node можно собрать обращением таблицы `new TextDecoder('windows-1251')` для байтов 0x80–0xFF;
   - SHA-256 файлов `cp1251.txt`, `utf8.txt` и `utf8-bom.txt` сверяется через `node:crypto` со значениями из таблицы фикстур.
5. `cargo fmt`. Затем `cargo clippy --locked --all-targets -- -D warnings` должен пройти без замечаний, чтобы F2 не пришлось трогать тесты.
6. **`scripts/verify-cp1251.py`**: общие хелперы и режимы `record-red` и `red`. Скрипт создаётся до первого verify.
   - **`record-red`** выполняет команды «Процесса проверки» §1 с логами и кодами в `artifacts/cp1251-fix/red/`:
     - `cargo-test`: `cargo test --locked cp1251`;
     - `ac18-cargo`: `cargo test --locked validates_utf8_beyond_the_requested_output_and_guards_panics`;
     - `build`: `cargo build --release --locked --bin local-files`;
     - `native-test`: `node --test --test-name-pattern cp1251 test/native.test.mjs` с `NATIVE_BINARY=target/release/local-files`;
     - `ac18-native`: `node --test --test-name-pattern "document continuation progresses at maxChars=1 and rejects stale files" test/native.test.mjs` с тем же `NATIVE_BINARY`.

     Затем пишет `diff.patch` = вывод `git diff 13dbae0 -- native test`. diff ограничен тестовыми файлами, чтобы в него не попал docs-коммит. Копирует RED-бинарник в `red/local-files`, пишет его SHA-256 в `red/local-files.sha256` и записывает `red/fingerprint.json`.
   - **`red`** проверяет, ничего не пересобирая:
     1. отпечаток текущего черновика равен `red/fingerprint.json`;
     2. изменённые и новые файлы вне `docs/` (по `git status --porcelain` и `git diff --name-only 13dbae0`) — ровно `native/documents.rs`, `native/filesystem.rs`, `test/native.test.mjs` и `scripts/verify-cp1251.py`;
     3. в `git diff 13dbae0 -- native/filesystem.rs` нет удалённых строк, а в `native/documents.rs` изменена ровно одна строка (−1/+1);
     4. `diff.patch` затрагивает ровно три тестовых файла и содержит `b"valid\n\xff\x00"` и `[65,10,0xff,0x00]`;
     5. каждый `.exit` содержит целое число: `cargo-test` ≠ 0, `build` = 0, `native-test` ≠ 0, `ac18-cargo` = 0, `ac18-native` = 0;
     6. SHA-256 `red/local-files` совпадает с `local-files.sha256`;
     7. в `cargo-test.log` строки `test filesystem::tests::cp1251_<имя> ... ok|FAILED` есть ровно для девяти тестов, `ok` — у обоих `cp1251_lock_*` и только у них; `error[E` нет, `NOT_TEXT` есть;
     8. в `native-test.log` — `pass 1` и `fail 1`, упал `cp1251: Windows-1251 text is read and searched by every tool`, прошёл `cp1251 lock: …`;
     9. `ac18-cargo.log` содержит ` 1 passed;`, в `ac18-native.log` — `pass 1` и `fail 0`;
     10. `guards/dist-before.txt` не пуст, `guards/pgrep-before.txt` существует;
     11. живой запуск `node --test --test-name-pattern "cp1251 lock" test/native.test.mjs` с `NATIVE_BINARY=artifacts/cp1251-fix/red/local-files` даёт 0.
7. Порядок: `record-red`, затем verify. Если результат не соответствует AC1 (не-lock-тест проходит или lock падает), исправляются тесты, а не production-код, и `record-red` запускается заново. Коммитит координатор после приёмки.

Закрывает AC1 и RED-половину AC18. Lock-части AC11–AC14, AC16, AC17 и AC27 (`utf8-nul.txt`) проходят на RED-дереве.

## F2: Декодер Windows-1251, интеграция, документация и локальный GREEN

Реализация по «Дизайну» спеки (правила 1–5, точные сообщения, запрещённые байты 0x00–0x08, 0x0B, 0x0E–0x1F, 0x7F, 0x98) без изменения тестов F1. Порядок — TDD: сначала юнит-тесты детектора, затем интеграция, которая переводит RED-тесты F1 в GREEN.

1. **`native/text.rs`** и `mod text;` в `native/main.rs`; API — как в «Новом модуле `native/text.rs`».
   - Сначала пишутся юнит-тесты `cp1251_detector_*` при заглушках `todo!()`:
     - приоритет UTF-8;
     - BOM;
     - `cp1251-nbsp.txt`, `text: ` + `D1`, `B8 FF FF FF FF C3` и `FF FE AC 20` → Windows-1251, а `41 00 42` → UTF-8;
     - каждый запрещённый байт и соседние разрешённые (TAB, LF, FF, CR, 0x97, 0x99);
     - одинаковое решение при подаче по строкам и целиком.

     `cargo test --locked cp1251_detector` должен сначала упасть, потом пройти.
   - Реализация:
     - `scan` сам ограничивает чтение `size + 1` байтами, читает строками через `read_until(b'\n')`, при росте файла возвращает `FILE_TOO_LARGE` «File grew beyond the read limit», а число строк считает как `1 + число '\n'`;
     - `decode` повторно проверяет UTF-8 или запрещённые байты и декодирует через `encoding_rs::WINDOWS_1251.decode_without_bom_handling` (`encoding_rs` 0.8.42 уже в `Cargo.lock`).
2. **`native/filesystem.rs` — `Filesystem::read`** (`:437–557`):
   - после разбора `startLine`, `endLine` и `maxChars` (`:497–499`) вызвать `text::scan(BufReader::new(&mut opened.source), read_size, is_text(&p))`, затем `opened.source.rewind()` (`Source: Seek`, `native/documents.rs:168`);
   - в цикле `from_utf8` (`:518`) заменить на `text::decode(encoding, &line)` только для строк диапазона;
   - в ответ добавить `"encoding"`;
   - бинарная ветка (`:463–485`) не меняется.
3. **`Filesystem::search`** (`:786–790`):
   - вместо `from_utf8` — `text::decode_buffer(&buffer, true)`; при `Err` — `inaccessible += 1`;
   - `line` считается по декодированному тексту;
   - `scannedBytes` по-прежнему считает исходные байты.
4. **`native/documents.rs` — `PlainText`** (`:532–615`):
   - новое поле `encoding`;
   - `open` вызывает `text::scan(&mut reader, opened.size, true)` и затем `rewind`;
   - `unit` отрезает `\n`, вызывает `text::decode` и отрезает `\r`;
   - `summary` возвращает `{"size","lines","encoding"}`;
   - `fetch` (`:1049`) копирует `encoding` в `metadata.encoding`.
5. `cargo test --locked cp1251` — все девять тестов F1 и тесты детектора зелёные. Затем Node-тесты `cp1251` в режимах serve и `NATIVE_WORKER=1`.
6. **Документация (AC25)**:
   - `native/mcp.rs`, описания инструментов:
     - строка 53: `… search in UTF-8 or Windows-1251 text; …`;
     - строка 60: `Read UTF-8 or Windows-1251 text or selected lines. …`;
     - строка 67: `Read UTF-8/Windows-1251 text, PDF, …`;
     - строка 81: `… plain text (UTF-8 or Windows-1251). …`.
   - `README.md`:
     - строка 122: ``| `search_text` | Поиск в UTF-8 или Windows-1251, PDF, …``;
     - строка 141: `| UTF-8 или Windows-1251 текст | Строки с номерами…`;
     - строка 195: `| UTF-8 или Windows-1251 текст — до 64 МиБ…`;
     - один абзац: правило выбора кодировки, поле `encoding`, `skippedInaccessible`, приоритет корректного UTF-8 (включая NUL, Ctrl-Z и байты, которые одновременно являются CP1251), условия `NOT_TEXT` (`EF BB BF` или байт из списка в точной записи `0x00–0x08, 0x0B, 0x0E–0x1F, 0x7F, 0x98`) и ограничения из «Вне скоупа». Отказ для данных из AC27 абзац не обещает.
7. **`scripts/verify-cp1251.py`**: режимы `record-local` и `local`, на общих хелперах F1.
   - **`record-local`** пишет в `artifacts/cp1251-fix/green-local/` пары `<имя>.log`/`<имя>.exit`:

     | Имя | Команда |
     |---|---|
     | `fmt` | `cargo fmt --check` |
     | `clippy` | `cargo clippy --locked --all-targets -- -D warnings` |
     | `cargo-test` | `cargo test --locked` |
     | `build` | `cargo build --release --locked --bin local-files` |
     | `native-serve` | `node --test test/native.test.mjs` с `NATIVE_BINARY=target/release/local-files` |
     | `native-worker` | то же и `NATIVE_WORKER=1` |
     | `npm-check` | `npm run check` |
     | `cargo-cp1251` | `cargo test --locked cp1251` |
     | `native-cp1251` | `node --test --test-name-pattern cp1251 test/native.test.mjs` с тем же `NATIVE_BINARY` |

     В конце пишет `green-local/fingerprint.json`.
   - **`local`** проверяет:
     1. отпечаток текущего черновика равен `green-local/fingerprint.json`;
     2. ключ `tests` в нём равен ключу `tests` из `red/fingerprint.json` — тесты F1 не изменены;
     3. все девять `.exit` содержат 0;
     4. изменённые и новые файлы вне `docs/` относительно `13dbae0` входят в набор: `native/text.rs`, `native/main.rs`, `native/filesystem.rs`, `native/documents.rs`, `native/mcp.rs`, `README.md`, `test/native.test.mjs`, `scripts/verify-cp1251.py`;
     5. `git diff 13dbae0` пуст для `Cargo.toml`, `Cargo.lock`, `native/doc_search.rs`, `native/cursor.rs` и `native/walk.rs` (AC24, «Изменения по файлам»);
     6. в `git diff 13dbae0 -- native/` нет строк `^[-+].*const (TEXT_LIMIT|BINARY_LIMIT|SCAN_LIMIT|OUTPUT_LIMIT|WALK_LIMIT|TEXT_READ_LIMIT|SEARCH_DEADLINE|DOC_SCAN_LIMIT|SEARCH_DOC_FILE_LIMIT|DOC_DEFAULT_CHARS|ZIP_ENTRY_LIMIT|ZIP_RATIO_LIMIT|CONTAINER_DEPTH)` (AC15);
     7. `native/mcp.rs` содержит четыре фразы из шага 6;
     8. `README.md` содержит три строки таблиц из шага 6 и подстроки `0x00–0x08, 0x0B, 0x0E–0x1F, 0x7F, 0x98`, `EF BB BF`, `skippedInaccessible` и `` `encoding` ``;
     9. живой запуск `node --test --test-name-pattern cp1251 test/native.test.mjs` с `NATIVE_WORKER=1` даёт 0. Режим serve проверяет отдельная verify-команда `node --test test/native.test.mjs`, бинарник по умолчанию — `target/release/local-files`.
8. Порядок: `record-local` из итогового черновика, затем verify. Коммитит координатор после приёмки.

Память и лимиты: константы не меняются (AC15). Декодирование CP1251 даёт не больше 3 байт на исходный байт. Это разбор недоверенных байтов, поэтому правила доступа, лимиты и коды ошибок должны остаться прежними (AC15–AC17).

Закрывает AC2–AC19, AC25–AC27 на итоговом дереве.

## F3: Изолированный GREEN на native Windows HPC

Выполняется по «Процессу проверки» §4 со snapshot принятого коммита F2.

- Рабочее дерево HPC `D:\Projects\GPT-MCP-filesystem`, установленное приложение и `%LOCALAPPDATA%\Local Files\folders.json` не меняются. Завершаются только собственные дочерние процессы.
- Исходники продукта в F3 не меняются. Меняются только `scripts/verify-cp1251.py` (режим `hpc`) и `artifacts/`.
- Если среда HPC, GitHub с HPC или toolchain недоступны, это эскалация координатору, а не обходной путь.
- **Переименование фикстуры NUL (портируемость на Windows).** Третья попытка F3 (`attempts/third`) дала на native Windows RED: 159 тестов прошли, 2 упали. Причина — фикстура `cp1251-controls/nul.txt`: базовое имя `NUL` зарезервировано в нативной Windows, и Rust-тесты получали `NOT_FOUND` вместо `NOT_TEXT`. Исправление минимальное: только точные голые ссылки `nul.txt` и `cp1251-controls/nul.txt` заменяются на `nul-control.txt` и `cp1251-controls/nul-control.txt`. Байты, ожидания и assertions не меняются. `utf8-nul.txt` — корректное имя, его не трогают.
  - Автор спеки отдельной задачей делает то же точное переименование в спеке.
  - Sol отвечает за исправления в двух тестовых файлах (`native/filesystem.rs`, `test/native.test.mjs`), а также за `probe.py` и `scripts/verify-cp1251.py` в F3.
  - Новый исходный SHA с переименованием сначала независимо принимается и коммитится как отдельный предварительный коммит фикстуры. Только после этого запускается новый финальный прогон F3, и все гейты повторяются на этом SHA.

1. **Bundle принятого коммита F2.** До начала проверяется, что `git diff fix/cp1251-txt -- native test README.md Cargo.toml Cargo.lock package.json package-lock.json` пуст. Затем:
   ```sh
   git bundle create artifacts/cp1251-fix/hpc-green/cp1251.bundle 13dbae0fa651aeb8acca53b28efc0f58cd259c63..fix/cp1251-txt
   (cd artifacts/cp1251-fix/hpc-green && shasum -a 256 cp1251.bundle > cp1251.bundle.sha256)
   ```
2. **`artifacts/cp1251-fix/hpc-green/probe.py`** — на основе `artifacts/cp1251-hpc-20261030/probe.py` (тот же JSON-RPC по stdio, только stdlib). Подкоманды:
   - `preserve-before <этап>` — замер `before`;
   - `gates <этап>` — вспомогательная: `clone`, `bundle verify`, `fetch`, `checkout` из §4.3 и команды AC20 по порядку `docs/windows-validation.md` с `CARGO_TARGET_DIR=<этап>\cargo-target`, логами в `<этап>\logs` и записью `snapshot.json`;
   - `run <этап>` — фикстуры, `fixtures.json`, `folders.json`, прогоны и `results.json`, `verification.json`;
   - `preserve-after <этап>` — замер `after` и `preservation_ok`.

   До отправки `run` стоит прогнать локально против macOS-бинаря через симлинк `<этап>/cargo-target/release/local-files.exe` → `target/release/local-files`.
3. **Передача** — через собственный терминал задачи в среде HPC: `orca terminal list --environment HPC`, при необходимости `orca terminal create --environment HPC --title cp1251-green --json`. Терминалы других задач не используются.
   - Ввод — `orca terminal send --environment HPC --terminal <handle> --text '…' --enter`, вывод — `orca terminal read --environment HPC --terminal <handle>`.
   - Как в RED-пробе (`artifacts/cp1251-hpc-20261030/send-receipt.json`, `terminal-capture.json`), `python -c` с base64(zlib) создаёт этап `tempfile.mkdtemp(prefix='local-files-cp1251-green-')` и записывает туда `probe.py` и по частям `cp1251.bundle`. SHA-256 bundle сверяется на HPC.
   - Квитанции `--json` сохраняются в `artifacts/cp1251-fix/hpc-green/transfer/`.
4. **На HPC по порядку**: `python -B probe.py preserve-before <этап>`, затем `gates`, затем `run`. `preserve-after` выполняется всегда, даже если предыдущий шаг упал.
5. **Возврат результатов.** `snapshot.json`, `results.json`, `verification.json`, `fixtures.json`, `preservation.json` и логи переносятся в `artifacts/cp1251-fix/hpc-green/`. Туда же кладутся `probe.py` и `REPORT.txt`:
   - `REPORT.txt` — на русском, с вердиктами и матрицей «метод × фикстура × EXE»;
   - при `BASELINE_NOT_RUN` это указано в отчёте;
   - отказ для данных из AC27 отчёт не обещает.

   Каталог этапа на HPC сохраняется.
6. Режим `hpc` в `scripts/verify-cp1251.py` — по контракту ниже, затем verify.

### Контракт данных HPC

Все времена — ISO 8601 UTC.

**`snapshot.json`**:
```json
{"stage":"…","bundle_sha256":"<hex>","head":"<sha>","status_porcelain":"",
 "clone_started_utc":"…","gates_finished_utc":"…",
 "exit_codes":{"fmt":0,"clippy":0,"test":0,"build":0,"npm_ci":0,"native_serve":0,"native_worker":0},
 "versions":{"os":"…","rustc":"<rustc -vV>","cargo":"…","node":"…","python":"…"}}
```
Коды — настоящие `returncode` подпроцессов.

**`fixtures.json`**: `{"<имя>":{"bytes":N,"sha256":"…"}}` для всех 13 файлов корня `probe`. SHA-256 пишутся сразу после создания каждого файла. Файлы:
- `cp1251.txt`, `utf8.txt`, `utf8-bom.txt`, `cp1251-typography.txt`, `utf8-also-cp1251.txt`, `bom-cp1251.txt`, `damaged-utf8.txt`, `cp1251-nbsp.txt`, `binary.txt`, `binary.bin`, `cp1251.dat` — по таблице «Синтетические фикстуры»;
- `control.docx` и `control.eml` — как в RED-пробе.

**`results.json`**:
```json
{"started_utc":"…","finished_utc":"…",
 "baseline":{"path":"…","status":"run|missing|sha_mismatch","sha256":"<hex>|null"},
 "runs":[{"exe":"snapshot|baseline","mode":"serve|worker","sha256":"…","exit_code":0,
          "cases":{"<case id>":[{"tool":"…","args":{…},"response":<сырой JSON-RPC ответ>}]}}]}
```
- Прогоны snapshot/serve и snapshot/worker обязательны. baseline/serve и baseline/worker обязательны, если `baseline.status` = `run`, то есть файл есть и его SHA-256 = `8b0b32a9d8239a361cccb237ad4599b2a0bf2ebeeba1b01d28bfe07c32520f0f`.
- Каждый прогон содержит каждый case id из каталога ниже ровно один раз.
- Ответ инструмента — `response.result`. Успех — без `isError`, данные в `structuredContent`. Ошибка — `isError: true` и `structuredContent.error.{code,message}`. Бинарный `read_file` отдаёт `blob` в элементе `content[]` типа `resource`.

**`verification.json`**: `{"verdicts":{"snapshot":"GREEN_NATIVE_WINDOWS","baseline":"BASELINE_REPRODUCED_2.2.0|BASELINE_NOT_RUN"},"checks":[{"check":"…","pass":true}]}`. Его вердикты должны совпасть с вердиктами, которые `hpc` вычисляет сам.

**`preservation.json`**:
```json
{"before":{"time_utc":"…","folders_json_sha256":"<hex>","installed_exe_sha256":"<hex>|null",
           "repo_head":"<sha>","repo_status":"<porcelain>","pids":[{"pid":N,"image":"local-files.exe"}]},
 "after":{…те же ключи…,"fixtures_sha256":{"<имя>":"<hex>"}},
 "preservation_ok":true}
```
- `folders_json_sha256` — SHA-256 `%LOCALAPPDATA%\Local Files\folders.json`;
- `installed_exe_sha256` — SHA-256 `%LOCALAPPDATA%\Programs\Strigov Local MCP\local-files.exe`;
- `repo_*` — для `D:\Projects\GPT-MCP-filesystem`;
- `pids` — из `tasklist /FI "IMAGENAME eq local-files.exe" /FO CSV /NH`.

### Каталог случаев

Ожидаемые значения `hpc` вычисляет сам из `TEXT`, `TYPO` и байтов фикстур.

**Обозначения:**
- E(f) — ожидаемый текст: декодированное содержимое с CRLF → LF.
  - `cp1251.txt` — `TEXT` с LF, `windows-1251`;
  - `utf8.txt` — `TEXT` с LF, `utf-8`;
  - `utf8-bom.txt` — U+FEFF + `TEXT` с LF, `utf-8`;
  - `cp1251-typography.txt` — `TYPO` с LF, `windows-1251`;
  - `utf8-also-cp1251.txt` — `»`, `utf-8`;
  - `damaged-utf8.txt` — `bytes.decode('cp1251')`, `windows-1251`;
  - `cp1251-nbsp.txt` — `В\u00a02024, В\u00a02025 — 100`, `windows-1251`.
- Тело `read_document` и `fetch` — текст после первой строки `[source: …]`.
- Q(f) — запрос `search_text` по пути:
  - `Замещающие` — для `cp1251.txt`, `utf8.txt`, `utf8-bom.txt`, `bom-cp1251.txt` и `binary.txt`;
  - `ёмкость` — для типографики;
  - `»` — для `utf8-also-cp1251.txt`;
  - `03.03.2023` — для `damaged-utf8.txt`;
  - `2025 — 100` — для `cp1251-nbsp.txt`.

**Матрица.** `<метод>:<файл>` для девяти `.txt` основного набора × шести методов: `read_text_file`, `read_file`, `read_document`, `inspect_document`, `fetch` (`id` = `probe:<файл>`), `search_text` (`root`, `path`, Q(f)).

Ожидания snapshot:
- семь файлов с E(f):
  - `read_text_file`: `text` = E, `encoding`, `totalLines` = число `\n` в E + 1;
  - `read_file`: `kind:"text"`, `text` = E, `encoding`, ресурса с `blob` нет;
  - `read_document`: `format:"text"`, `encoding`, тело = E, `size` = байты, `lines`;
  - `inspect_document`: `format:"text"`, `encoding`, `size`, `lines`;
  - `fetch`: `metadata.format` = `text`, `metadata.encoding`, тело = E;
  - `search_text`:
    - `cp1251.txt`, `utf8.txt` и `utf8-bom.txt` — одно совпадение, `locator.line` = 2, `snippet` = `Замещающие сделки: взыскание убытков.`;
    - типографика — два совпадения в строке 2;
    - `damaged-utf8.txt` — одно в строке 3;
    - `cp1251-nbsp.txt` — одно в строке 1;
    - `utf8-also-cp1251.txt` — `INVALID_ARGUMENT`;
- `bom-cp1251.txt`: все шесть — `NOT_TEXT` с сообщением `File looks like damaged UTF-8`;
- `binary.txt`: все шесть — `NOT_TEXT` с сообщением `File is neither UTF-8 nor Windows-1251 text`.

**Дополнительные случаи:**

| Case id | Вызов | Ожидание snapshot |
|---|---|---|
| `range` | `read_text_file` `cp1251.txt`, `startLine:2,endLine:2` | `text` = вторая строка E, `startLine` = `endLine` = 2 |
| `maxchars` | `read_text_file` `cp1251.txt`, `maxChars:5` | `Поста`, `truncated:true` |
| `invalid-range` | `read_text_file` `cp1251.txt`, `startLine:3,endLine:2` | `INVALID_RANGE` |
| `cursor` | цепочка `read_document` `cp1251.txt`, `maxChars:1` до исчезновения `nextCursor` | все ответы без ошибок; строки, собранные по `units[].line` и `units[].chars`, равны строкам E |
| `yo` | `search_text` `cp1251-typography.txt`, `ЁЛКИН` | два совпадения, `locator.line` = 3 |
| `folder` | `search_text` `root:"probe"`, `Замещающие` | `relativePath` результатов = {`cp1251.txt`, `utf8.txt`, `utf8-bom.txt`, `control.docx`, `control.eml`}; у `cp1251.txt` `line` = 2; `skippedInaccessible` = 2; `skippedLargeFiles` = 0 |
| `folder-search-line3` | `search_text` `root:"probe"`, `03.03.2023` | `cp1251.txt` есть в результатах с `line` = 3: номер строки декодированного текста, а не байтовое смещение |
| `folder-nbsp` | `search_text` `root:"probe"`, `2025 — 100` | ровно `cp1251-nbsp.txt`, `line` = 1 |
| `connector` | `search` `Замещающие`, затем `fetch` `probe:cp1251.txt` | id есть в `results`; тело `fetch` = E(`cp1251.txt`) |
| `connector-nbsp` | `search` `2025 — 100` | есть id `probe:cp1251-nbsp.txt` |
| `dat` | `read_text_file` и `read_file` `cp1251.dat` | `NOT_TEXT` с сообщением `File is not valid UTF-8. Use read_document for document formats.`; `kind:"binary"` и `blob`, равный байтам `cp1251.txt` |
| `bin` | `read_file` `binary.bin` | `kind:"binary"`, `blob` = 256 байт `bytes(range(256))` |
| `controls` | `read_document` `control.docx` и `control.eml` | тело содержит `Замещающие сделки` |

**Ожидания baseline** (AC22, только при `status` = `run`):
- для `cp1251.txt`, `cp1251-typography.txt`, `cp1251-nbsp.txt` и `damaged-utf8.txt` все шесть методов матрицы дают `NOT_TEXT`;
- в `folder` нет `cp1251.txt`, а `skippedInaccessible` = 0;
- у `utf8.txt` и `utf8-bom.txt` поле `text` `read_text_file` и `read_file` и тела `read_document` и `fetch` равны ответам snapshot в том же режиме.

Прочие случаи baseline записываются, но не оцениваются.

### Режим `hpc`

Проверки пересчитываются по сырым данным. Флагам `pass:true` и `preservation_ok` режим не доверяет.

1. **Snapshot и bundle:**
   - `snapshot.exit_codes` содержит ровно семь ключей, все целые и равны 0; `versions` заполнены;
   - SHA-256 локального `cp1251.bundle` = `cp1251.bundle.sha256` = `snapshot.bundle_sha256`;
   - голова `fix/cp1251-txt` в `git bundle list-heads` = `snapshot.head`, `status_porcelain` пуст;
   - эта голова — предок `HEAD` или равна ему, и от неё до рабочего дерева нет изменений в `native`, `test`, `README.md`, `Cargo.toml`, `Cargo.lock`, `package.json` и `package-lock.json`.
2. **Фикстуры:**
   - в `fixtures.json` все 13 имён;
   - для 11 синтетических файлов SHA-256 и размер совпадают с байтами, которые `hpc` строит сам;
   - `cp1251.txt`, `utf8.txt` и `utf8-bom.txt` равны значениям RED-пробы.
3. **Прогоны:**
   - есть все обязательные прогоны; у каждого `exit_code` = 0 и SHA-256 бинарника записан;
   - у baseline SHA-256 = `8b0b…0f`;
   - каждый case id есть ровно один раз, ответы — JSON-RPC с `result`;
   - все ожидания snapshot выполнены в обоих режимах;
   - при `baseline.status` = `run` выполнены ожидания baseline. При `missing` и `sha_mismatch` вердикт — `BASELINE_NOT_RUN`, и это слово есть в `REPORT.txt`.
4. **Вердикты:** вычисленные вердикты равны вердиктам `verification.json`, а `REPORT.txt` содержит `GREEN_NATIVE_WINDOWS` и вердикт baseline.
5. **Сохранность:**
   - в `before` и `after` есть все ключи схемы;
   - `folders_json_sha256` — строка, и значения в двух замерах равны;
   - `installed_exe_sha256` равны (допустимо `null` в обоих);
   - `repo_head` и `repo_status` равны;
   - каждая пара (`pid`, `image`) из `before` есть в `after`;
   - `after.fixtures_sha256` равен `fixtures.json` по всем 13 именам;
   - `before.time_utc` ≤ `snapshot.clone_started_utc`; `after.time_utc` ≥ `results.finished_utc` и ≥ `snapshot.gates_finished_utc`;
   - `preservation_ok` равен вычисленному результату и равен `true`.
6. **AC24:**
   - `git diff 13dbae0 -- Cargo.toml Cargo.lock` пуст;
   - `git rev-parse origin/main` = `13dbae0fa651aeb8acca53b28efc0f58cd259c63`;
   - список `find dist | LC_ALL=C sort` из основного checkout равен `guards/dist-before.txt`;
   - SHA-256 `artifacts/cp1251-hpc-20261030/REPORT.txt` = `d55ff262a21881cab6923d665753769fa5068e382b568a2c3807da8f00bcdab1`, `results.json` = `97e29980b3a10e9113d67fa10729b766a73ee636cca153ad2fd61be381377d67`;
   - строки `guards/pgrep-before.txt` — подмножество текущего `pgrep -fl local-files`.

Закрывает AC20–AC24 и часть AC27, относящуюся к `REPORT.txt`.

## Покрытие критериев

| Критерий | Фаза |
|---|---|
| AC1 | F1 |
| AC2–AC17, AC19, AC25, AC26 | F2 |
| AC18 | F1 (RED), F2 (итоговое дерево) |
| AC20–AC24 | F3 |
| AC27 | F2 (поведение и README), F3 (`REPORT.txt`) |
