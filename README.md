# Максут

Telegram-бот на Rust (`maksut`) для скачивания медиа по ссылкам YouTube, Pinterest, X/Twitter, Instagram и TikTok, свободного общения в чате и самоулучшения через изолированный пайплайн.

Что изменилось в поведении бота — в [`CHANGELOG.md`](./CHANGELOG.md).

Архитектура, ограничения и детали реализации вынесены в [`AGENTS.md`](./AGENTS.md) и тематические гайды в [`docs/guides/`](./docs/guides/):

- [`docs/guides/layout.md`](./docs/guides/layout.md): карта модулей `src/` и Python-сервисов `tools/`
- [`docs/guides/media.md`](./docs/guides/media.md): загрузка видео и фото, проверка аудио, конвертация в MP4, альбомы, подписи, 24-часовой кэш `file_id` и категории ошибок
- [`docs/guides/chat.md`](./docs/guides/chat.md): свободное общение с LLM, память чата, стикеры, `web_search`, `research`, `read_url` и реестр инструментов
- [`docs/guides/self-improve.md`](./docs/guides/self-improve.md): двухэтапный пайплайн самоулучшения (`plan` -> `deploy`) и контракт `tools/omp_worker.py`
- [`docs/guides/cookies.md`](./docs/guides/cookies.md): экспорт cookies Chrome по SSH и сервис `cookie-refresh`
- [`docs/guides/security.md`](./docs/guides/security.md): SSRF-фильтры, границы прав инструментов, защита от промпт-инъекций и изоляция контейнеров
- [`docs/guides/workflow.md`](./docs/guides/workflow.md): обязательный цикл разработки и ревью

## Запуск и обслуживание

Бот работает через исходящие соединения, без `host network` и внешней СУБД. Единственный опубликованный порт — `127.0.0.1:8090` для Telegram Mini App, и он слушает только после `[web] enabled = true`; до контейнера его достигает лишь хостовый reverse proxy. Для первого запуска создайте `.env` из `.env.example` (`chmod 600 .env`), задайте `BOT_TOKEN` и `OWNER_ID`, настройте SSH-ключ `cookie-refresh` по [`docs/guides/cookies.md`](./docs/guides/cookies.md) и создайте каталог `self-improve/`:

```sh
mkdir -p self-improve
docker compose -f compose.yaml up --build -d
docker compose -f compose.yaml logs -f maksut
```

Голосовые и аудио расшифровывает отдельный контейнер `speaches` (faster-whisper на CPU, порт на хост не публикуется): бот обращается к нему по `http://speaches:8000/v1`, а веса модели лежат в отдельном volume `maksut_speaches`, поэтому SQLite-том `maksut_data` стороннему образу не виден. Прогрейте кэш до первого голосового — иначе загрузка весов съест почти весь `voice.timeout_secs` (по умолчанию 90 с, допустимо 5–120):

```sh
make speaches-models   # скачать VOICE_MODEL (по умолчанию Systran/faster-whisper-small); повторный запуск ничего не качает
make logs-speaches     # логи распознавания
docker exec maksut-speaches-1 curl -s http://127.0.0.1:8000/v1/models   # проверить, что модель видна сервису
```

Telegram-голосовые приходят в OGG/Opus, а `faster-whisper-small` многоязычная: русскую речь распознаёт без дополнительных настроек и без поля `language` (язык определяется автоматически). На CPU хоста распознавание идёт примерно втрое быстрее реального времени (19 с записи — около 8 с, минута — около 21 с), так что дефолтных 90 с хватает примерно на четыре минуты; для записей у границы `voice.max_duration_secs` поднимите `voice.timeout_secs`, но не выше 120 с. Если голосовое стабильно возвращает «не удалось распознать», смотрите `docker logs maksut-speaches-1`: медленный ответ виден по строке `Voice transcription exceeded`, а проблемы модели — в логах самого контейнера.

Автодеплой перезапускает `speaches` вместе с ботом и на каждом деплое добивает кэш модели (`--profile provision`, сбой прогрева только логируется и деплой бота не блокирует). Модель задаётся одним ключом `VOICE_MODEL`: он уходит и в контейнер бота, и в `speaches-models`; при запуске без compose используется `voice.model` в `config.toml`. `compose.yaml` выставляет `VOICE_BASE_URL`/`VOICE_MODEL` через `${…:-default}`, поэтому переменные окружения перекрывают файл конфига — после смены модели снова выполните `make speaches-models`.

Обновление и остановка (при пуше в `main` GitHub Actions сам публикует образ `ghcr.io/labi-le/maksut:<sha>`; сделайте пакет `maksut` публичным в настройках GitHub Packages или выполните один раз `gh auth token | docker login ghcr.io -u labi-le --password-stdin` на хосте, после чего вебхук publish-image (`tools/deploy_hook.py`) сам делает `docker pull` и `docker compose up -d --no-build maksut qdrant embeddings speaches` по `origin/main`, таймера-страховки нет: пропущенную доставку восстанавливают кнопкой *Redeliver* в GitHub или `make deploy-now` (образ, `compose.yaml` и исходники для `inspect_code` берутся из одного коммита, незакоммиченные правки чекаута деплой не блокируют и в контейнер не попадают, а тик с уже совпадающим `deployed_sha` всё равно сверяет образ и `MAKSUT_CODE_DIR` запущенного контейнера с этим sha и пересоздаёт контейнер при расхождении — это видно в `make deploy-status` строкой `drift`; [ADR 023](docs/adr/023-auto-deploy-from-remote.md)); не используйте `docker compose down -v`, чтобы не удалить volume `maksut_data` с SQLite-базой):

```sh
docker compose -f compose.yaml pull maksut && docker compose -f compose.yaml up -d --no-build maksut
docker compose -f compose.yaml down
```

Для смены `BOT_TOKEN`, `OWNER_ID` или путей в `.env` пересоздайте контейнер через `docker compose -f compose.yaml up -d --force-recreate` (`env_file` читает переменные из `.env`, экспортированные в оболочке значения их не переопределяют). Обычное обновление `cookies.txt` перезапуска не требует.

### Конфигурация

Настройки читаются из `config.toml` в рабочем каталоге процесса (в образе это `/data`, поэтому файл кладётся в volume как `/data/config.toml`), другой путь задаёт `MAKSUT_CONFIG`; явно заданный, но отсутствующий путь считается ошибкой запуска. Шаблон со всеми секциями и значениями по умолчанию лежит в [`example.config.toml`](./example.config.toml). Приоритет: встроенные значения < файл < переменные окружения, поэтому переменные (`DATABASE_URL`, `DOWNLOAD_DIR`, `LLM_*` и остальные) по-прежнему переопределяют файл. Секреты (`BOT_TOKEN`, `OWNER_ID`, `LLM_API_KEY`, `TAVILY_API_KEY`) задаются только через окружение. Неизвестный ключ или значение вне допустимого диапазона останавливает запуск с сообщением об этом ключе. Уровень логов задаётся ключом `log.level`, строка лога несёт модуль и файл:строку, а `#[instrument]`-спаны добавляют имя функции.

### Telegram Mini App

`/settings` в личном чате (только `OWNER_ID`) отвечает кнопкой, открывающей мини-апп по `web.public_url`. В приложении три вкладки: память `bot_self_memory` (правка и удаление только код-фактов со `source_url`; директивы владельца остаются за `remember_self`), настройки из whitelist (`src/web_config.rs`) и чаты. Правки настроек пишутся в `config.toml` и применяются после рестарта, ключи, перекрытые переменной окружения, недоступны; во вкладке «Чаты» задаются `router_mode`/`router_min_confidence` отдельного чата (переопределение `[router]`, см. ниже).

Включение в `/data/config.toml`:

```toml
[web]
enabled = true
listen = "0.0.0.0:8090"
public_url = "https://maksut.example.org/"
```

Контейнер слушает все интерфейсы своей bridge-сети, а `compose.yaml` публикует только петлю (`127.0.0.1:8090`), поэтому снаружи есть один путь — хостовый nginx, который завершает TLS:

```nginx
server {
    listen 443 ssl;
    server_name maksut.example.org;
    # ssl_certificate ...;
    location / {
        proxy_pass http://127.0.0.1:8090;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }
}
```

Авторизация — только Telegram `initData` (`HMAC-SHA256` с ключом `WebAppData` от `BOT_TOKEN`, свежесть `web.auth_max_age_secs`, `user.id == OWNER_ID`). Статика публична, потому что страница грузится до запуска JS; все `/api/*` закрыты. Подробности и границы — [ADR 027](./docs/adr/027-telegram-mini-app.md).

Запуск хостового оркестратора самоулучшения и вебхука автодеплоя из GHCR:

```sh
cp packaging/maksut-self-improve.service packaging/maksut-deploy-hook.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now maksut-self-improve.service maksut-deploy-hook.service
make deploy-status
make logs
```

Для локальной разработки используется `shell.nix`:

```sh
CARGO_TARGET_DIR=/tmp/maksut-target nix-shell --run "cargo test"
nix-shell --run "python3 -m unittest discover -s tools -p 'test_*.py'"
```

## Сборка и проверки

Rust компилируется на `pc` через `cargo-slot`: команда занимает общий слот сборки, при необходимости переносит дерево на `pc` и держит `CARGO_TARGET_DIR` в `/home/labile/.cache/cargo-pc`. Аргументы совпадают с обычным `cargo`, тулчейн поставляет `nix-shell` (`shell.nix`):

```sh
timeout 900 cargo-slot cargo check --all-targets
timeout 900 cargo-slot cargo test
timeout 900 cargo-slot cargo clippy --all-targets
```

## Использование

- **Ссылки и группы:** в личном чате бот принимает ссылки только от владельца (`OWNER_ID`). В группах обработка по умолчанию выключена и управляется владельцем через `/maksut on`, `/maksut off` и `/maksut status` (у бота должен быть выключен Privacy Mode в BotFather и выдано право `can_delete_messages` для удаления исходной ссылки после доставки). Во включённой группе ссылки принимает от любого участника с лимитом на отправителя (`media.user_download_limit`, по умолчанию 3); остальные действия остаются owner-only.
- **Свободное общение (`LLM_API_KEY`):** в личке отвечает владельцу на любой текст без ссылки и команды; во включённой группе отвечает реплаем на `@имя_бота`, реплай на сообщение бота, обращение `максут`/`максутка`/`maksut`, а также поддерживает живой разговор в чате ещё 3 минуты (`chat.followup_window_secs`, по умолчанию 180 s) после своего ответа без повторного упоминания имени (если реплика не по теме — молчит, а при явном посыле окно закрывается сразу). Всем участникам доступны `web_search` (автоматическая цепочка провайдеров без ключей: Firecrawl, DuckDuckGo, Parallel MCP, Exa MCP, Startpage, Mojeek, Ecosia + Tavily при наличии ключа), `research` (параллельный консенсус-опрос нескольких поисковых движков без ключей, как `web/public` в omp), `research_status` (снимок активных и недавних ресерчей чата: id, какой вопрос, прогресс по запросам, сколько источников и сколько времени идёт), `research_cancel` (отмена долгого ресерча с возвратом частичного досье: свою задачу отменяет автор, любую — владелец бота), `read_url` (чтение и навигация по сайтам с селекторами строк `:1-120`, `:-80`, `:raw` и спец-ридерами для GitHub, Reddit, Wikipedia, Hacker News, crates.io, PyPI и RSS/Atom), `chat_search` (семантический и лексический поиск по истории сообщений и VLM-описаниям медиа чата в Qdrant/SQLite), `send_chat_media` (отправка найденного в истории чата фото, видео, гифки, документа, голосового или стикера по `message_id`/`storage_key` через `file_id` с откатом на Garage), `propose_feature`, `feature_status`, `inspect_code`, `find_stickers` и `send_sticker`; владельцу дополнительно доступны `feature_confirmation`, `remember_self`, `chat_index` и `chat_settings`; голосовые сообщения и аудиофайлы расшифровываются локальным сервисом `speaches` (faster-whisper на CPU, OpenAI-совместимый `POST {voice.base_url}/audio/transcriptions`) через `[voice]`: `base_url` по умолчанию `http://speaches:8000/v1`, `model` — `Systran/faster-whisper-small`, лимиты `max_bytes`, `max_duration_secs`, `timeout_secs`, `max_transcript_chars`; облачного запасного распознавания нет, а в запрос не уходит `LLM_API_KEY`, расшифровка попадает в промпт и историю как `[голосовое {n}с]: ...`, а сбой или превышение лимита даёт `[не удалось распознать голосовое]` ([`docs/guides/chat.md`](./docs/guides/chat.md)).
Перед вызовом тяжёлой модели решение «молчать / ждать / ответить текстом / ответить стикером» может принимать роутер внимания (`[router]` в конфиге, режимы `off` и `jev`): `jev` — TypeSafe System One API (`reply` / `sticker` / `wait` / `silent`) с `JEV_API_KEY` и детерминированным откатом (`decide_rules`); прямые вопросы и длинные обращения идут сразу в `reply`, а короткие эмоциональные реплики проходят через `jev`, чтобы бот мог врезать подходящим стикером вместо длинного текста (при этом прямое обращение никогда не игнорируется, кроме стоп-фразы, которая молча закрывает окно) ([ADR 025](./docs/adr/025-chat-decision-router.md)). Режим и порог настраиваются глобально в `[router]` и переопределяются для отдельного чата: владелец пишет боту обычным текстом («включи тут роутер с порогом 0.7», «покажи настройки чата», «выруби роутер в этом чате»), и модель вызывает owner-инструмент `chat_settings` (`get` / `set` / `reset`, `chat_id` по умолчанию текущий), либо правит чаты во вкладке «Чаты» Mini App (`GET /api/chats`, `PUT`/`DELETE /api/chats/{chat_id}`); `router_mode` (`off`/`jev`) и `router_min_confidence` (0..1) хранятся в SQLite-таблице `chat_settings`, а глобальный конфиг остаётся фолбэком.
- **Самоулучшение:** владелец может ставить задачи через `/improve <цель>` или в диалоге (`propose_feature`), проверять очередь (`/improve status [id]`, `feature_status`, `feature_confirmation`) и управлять заявками кнопками либо командами в личке: `одобри #id`, `отклони #id`, `отмени #id`, `подними #id`, `опусти #id`, `приоритет #id <n>`, `/improve off` ([`docs/guides/self-improve.md`](./docs/guides/self-improve.md)).

## Диагностика неудачных загрузок

Безопасная трасса последних 200 ошибок загрузки хранится в таблицах `download_failures` и `download_failure_events` в `/data/maksut.db` (без query-параметров, токенов, cookies и сырого stderr):

```sh
docker compose -f compose.yaml exec -T maksut python3 -c 'import sqlite3; db=sqlite3.connect("file:/data/maksut.db?mode=ro", uri=True); print(*db.execute("SELECT f.rowid, f.created_at, f.url_host, f.url_path, e.event_index, e.stage, e.detail, e.exit_code FROM (SELECT rowid, * FROM download_failures ORDER BY rowid DESC LIMIT 30) AS f JOIN download_failure_events AS e ON e.failure_rowid = f.rowid ORDER BY f.rowid DESC, e.event_index"), sep="\n")'
```
