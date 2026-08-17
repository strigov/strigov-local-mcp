# Local Files MCP для ChatGPT

Готовый MCP-сервер только для чтения двух локальных папок:

- `work` → `/Users/USER/Documents/Claude/Projects/Work (local)`
- `cowork` → `/Users/USER/Documents/Claude/Projects/Cowork (local)`

Сервер предназначен для подключения к ChatGPT через официальный OpenAI Secure MCP Tunnel. Он не открывает локальный HTTP-порт и не публикует компьютер через ngrok или Cloudflare Tunnel.

## Архитектура

```text
ChatGPT
   │
   │ OpenAI Secure MCP Tunnel (исходящий HTTPS)
   ▼
tunnel-client на Mac
   │
   │ MCP по stdin/stdout
   ▼
Docker-контейнер без сети
   ├── /data/work   ← Work (local), bind mount read-only
   └── /data/cowork ← Cowork (local), bind mount read-only
```

Контейнер запускается с неизменяемой файловой системой, без Linux capabilities, без повышения привилегий и без сетевого доступа. Исходные папки монтируются напрямую, но физически только для чтения; файлы не копируются и не изменяются.

## Инструменты

- `list_roots` — показать разрешённые корни;
- `list_directory` — получить список файлов и каталогов;
- `get_file_info` — размер, даты, MIME-тип и URI;
- `search_files` — поиск по именам и относительным путям;
- `search_text` — полнотекстовый поиск в UTF-8/Markdown/CSV/JSON/RTF и других текстовых файлах;
- `read_text_file` — прочитать текст целиком или по строкам;
- `read_file` — прочитать текст или передать ограниченный по размеру бинарный файл как MCP resource;
- `search` и `fetch` — совместимый с ChatGPT интерфейс поиска и получения документов.

Все инструменты помечены как `readOnlyHint: true`. Инструментов записи, удаления, shell и сетевых запросов нет.

## Уже выполнено на этом Mac

- установлены зависимости Node.js;
- собран образ `strigov/local-files-mcp:1.0.0`;
- установлен официальный `tunnel-client`;
- сервер проверен как напрямую, так и внутри изолированного контейнера;
- проверено чтение обеих реальных папок.

Повторная проверка:

```bash
cd '/Users/USER/Documents/Claude/Projects/projects-code/GPT-MCP-filesystem'
npm run check
```

Повторная сборка контейнера после изменения кода:

```bash
./scripts/build-image.sh
```

## Подключение туннеля

Для последнего шага нужны значения из вашего аккаунта OpenAI Platform:

1. Создайте tunnel в [Platform → Tunnels](https://platform.openai.com/settings/organization/tunnels).
2. Свяжите его с личной Platform organization и целевой рабочей областью ChatGPT.
3. Создайте отдельный runtime API key с правами `Tunnels Read + Use` в [Platform → API keys](https://platform.openai.com/settings/organization/api-keys).
4. Не используйте административный ключ как постоянный runtime-ключ.

Передайте ключ текущему shell без сохранения в истории:

```zsh
read -s 'CONTROL_PLANE_API_KEY?OpenAI runtime API key: '
echo
export CONTROL_PLANE_API_KEY
```

Создайте профиль, подставив полученный идентификатор:

```bash
cd '/Users/USER/Documents/Claude/Projects/projects-code/GPT-MCP-filesystem'
./scripts/setup-tunnel.sh 'tunnel_ВАШ_ID'
```

Запустите туннель:

```bash
./scripts/run-tunnel.sh
```

Пока процесс работает, откройте [ChatGPT → настройки приложений](https://chatgpt.com/#settings/Connectors), включите Developer mode, создайте приложение, выберите тип подключения **Tunnel**, выберите созданный tunnel и выполните **Scan Tools**.

Рекомендуемое имя приложения: `Local Work Files — Read Only`.

## Проверка в ChatGPT

Первый запрос:

> Используй Local Work Files — Read Only. Вызови list_roots и покажи только разрешённые корни.

Ожидаемый результат: `work` и `cowork`, без абсолютных путей хоста.

Затем:

> Покажи первые 20 элементов в корне work.

Отрицательная проверка:

> Попробуй прочитать ../Cowork (local)/любой-файл через корень work.

Ожидается ошибка `PATH_ESCAPE`.

## Ограничения и защита данных

- Типовые секреты и служебные каталоги блокируются: `.env*`, `.git`, `.ssh`, `.aws`, `node_modules`, приватные ключи и сертификаты.
- Симлинк, ведущий за пределы выбранного корня, блокируется после проверки реального пути.
- Текстовый файл читается максимум до 5 МБ, бинарный — до 15 МБ, один текстовый ответ — до 200 000 символов.
- Один полнотекстовый поиск читает максимум 128 МБ; для точного результата указывайте более узкий `root` и `path`.
- PDF, DOCX, XLSX и сканы лучше анализировать через отдельные Markdown-версии. Для локального преобразования сохраняйте оригинал и производный Markdown раздельно; текстовые PDF не следует повторно распознавать, а OCR нужен только страницам без пригодного текстового слоя.
- Любой файл, реально прочитанный через MCP, передаётся в ChatGPT через туннель. Secure MCP Tunnel закрывает публичный вход к Mac, но не превращает обработку ChatGPT в полностью локальную.
- Документ внутри одной из папок может содержать prompt injection. Подключая оба корня одним приложением, вы разрешаете модели читать материалы из обоих корней в рамках одного разговора.

Официальная документация: [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) и [подключение MCP к ChatGPT](https://developers.openai.com/plugins/deploy/connect-chatgpt).

## Постоянная работа в фоне на macOS

После успешной ручной проверки остановите foreground-процесс `./scripts/run-tunnel.sh` через `Ctrl+C`, затем установите LaunchAgent:

```bash
cd '/Users/USER/Documents/Claude/Projects/projects-code/GPT-MCP-filesystem'
./scripts/install-background-service.sh
```

Если `CONTROL_PLANE_API_KEY` всё ещё экспортирован в текущем Terminal, установщик использует его. Иначе он один раз запросит ключ со скрытым вводом. Ключ сохраняется в macOS Login Keychain, а не в plist, репозитории или логах.

LaunchAgent:

- запускается при входе пользователя `strigov` в macOS;
- использует runtime-копию wrapper-скриптов из `~/.local/share/strigov-local-files-mcp/`, потому что macOS TCC запрещает фоновым процессам запускать код непосредственно из `Documents`;
- при необходимости запускает Docker Desktop и ждёт его готовности;
- запускает `tunnel-client` с отдельным фоновым профилем `strigov-local-files-background`;
- не даёт Mac перейти в idle sleep, пока tunnel работает;
- перезапускает tunnel после аварийного завершения;
- пишет логи в `~/Library/Logs/strigov-local-files-tunnel/`.

Проверка состояния:

```bash
./scripts/status-background-service.sh
```

Ожидаемый результат:

```text
LaunchAgent: loaded
Health: PASS
Readiness: PASS
```

Удаление фоновой службы:

```bash
./scripts/uninstall-background-service.sh
```

При удалении службы runtime-ключ намеренно остаётся в Keychain. Удалить его отдельно можно так:

```bash
security delete-generic-password \
  -a 'strigov-local-files' \
  -s 'com.strigov.openai-tunnel.local-files.runtime-key'
```

Это пользовательский LaunchAgent, поэтому после полной перезагрузки Mac требуется вход пользователя в macOS. До входа Docker Desktop и tunnel запущены не будут.
