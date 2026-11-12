//! Editing `config.toml` from the Mini App.
//!
//! The running process keeps its installed `Config`, so an edit is a file
//! change that applies on the next start. Two rules keep that honest: a
//! candidate must pass the startup parser and validation before it replaces the
//! file, and a key that an environment variable currently overrides is refused
//! instead of written as a no-op. Only whitelisted scalar keys are editable, so
//! a web edit can never point the bot at another database, host, or binary.

use crate::*;
use tokio::io::AsyncWriteExt;
use toml_edit::{DocumentMut, Item, Table, Value as TomlValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum EditKind {
    Int,
    Str,
}

impl EditKind {
    fn describe(self) -> &'static str {
        match self {
            Self::Int => "целое число",
            Self::Str => "строку",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum EditValue {
    Int(i64),
    Str(String),
}

impl EditValue {
    fn matches(&self, kind: EditKind) -> bool {
        matches!(
            (kind, self),
            (EditKind::Int, Self::Int(_)) | (EditKind::Str, Self::Str(_))
        )
    }
}

pub(crate) struct EditableKey {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: EditKind,
    /// The environment variable that shadows this key at startup, if any.
    pub(crate) env: Option<&'static str>,
}

/// The whole editable surface. Every entry is a scalar that only this process
/// reads, so a restart cannot leave another service on an older value.
pub(crate) const EDITABLE: &[EditableKey] = &[
    EditableKey {
        key: "log.level",
        label: "Уровень логов",
        kind: EditKind::Str,
        env: None,
    },
    EditableKey {
        key: "chat.reply_max_chars",
        label: "Максимальная длина ответа",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "chat.tool_rounds",
        label: "Раундов вызова инструментов на ответ",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "chat.followup_window_secs",
        label: "Окно продолжения разговора, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "chat.discovered_url_ttl_secs",
        label: "Время жизни найденных ссылок, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "chat.max_active_chats",
        label: "Максимум активных чатов",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "memory.ignore_secs",
        label: "Игнор после инъекции, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "memory.recent_messages",
        label: "Сообщений контекста",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "memory.summary_max_chars",
        label: "Длина сводки, символов",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "media.max_files",
        label: "Файлов в одной доставке",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "media.max_video_duration_secs",
        label: "Максимальная длительность видео, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "media.download_timeout_secs",
        label: "Таймаут загрузки, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "media.max_bytes",
        label: "Максимальный размер файла, байт",
        kind: EditKind::Int,
        env: Some("MAX_MEDIA_BYTES"),
    },
    EditableKey {
        key: "research.max_sources",
        label: "Источников в ресерче",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "research.timeout_secs",
        label: "Таймаут ресерча, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "read_url.timeout_secs",
        label: "Таймаут чтения страницы, с",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "read_url.max_chars",
        label: "Символов из страницы",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "code.memory_max_rows",
        label: "Строк памяти бота",
        kind: EditKind::Int,
        env: None,
    },
    EditableKey {
        key: "usage.retention_days",
        label: "Хранение статистики, дней",
        kind: EditKind::Int,
        env: None,
    },
];

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct KeyView {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) kind: EditKind,
    pub(crate) value: EditValue,
    pub(crate) env_shadowed: bool,
    pub(crate) env_variable: Option<&'static str>,
    pub(crate) restart_required: bool,
}

/// The effective value the running process uses, mirroring the whitelist.
pub(crate) fn effective(config: &Config, key: &str) -> Option<EditValue> {
    Some(match key {
        "log.level" => EditValue::Str(config.log.level.clone()),
        "chat.reply_max_chars" => EditValue::Int(config.chat.reply_max_chars as i64),
        "chat.tool_rounds" => EditValue::Int(config.chat.tool_rounds as i64),
        "chat.followup_window_secs" => EditValue::Int(config.chat.followup_window_secs as i64),
        "chat.discovered_url_ttl_secs" => {
            EditValue::Int(config.chat.discovered_url_ttl_secs as i64)
        }
        "chat.max_active_chats" => EditValue::Int(config.chat.max_active_chats as i64),
        "memory.ignore_secs" => EditValue::Int(config.memory.ignore_secs),
        "memory.recent_messages" => EditValue::Int(config.memory.recent_messages as i64),
        "memory.summary_max_chars" => EditValue::Int(config.memory.summary_max_chars as i64),
        "media.max_files" => EditValue::Int(config.media.max_files as i64),
        "media.max_video_duration_secs" => {
            EditValue::Int(config.media.max_video_duration_secs as i64)
        }
        "media.download_timeout_secs" => EditValue::Int(config.media.download_timeout_secs as i64),
        "media.max_bytes" => EditValue::Int(config.media.max_bytes as i64),
        "research.max_sources" => EditValue::Int(config.research.max_sources as i64),
        "research.timeout_secs" => EditValue::Int(config.research.timeout_secs as i64),
        "read_url.timeout_secs" => EditValue::Int(config.read_url.timeout_secs as i64),
        "read_url.max_chars" => EditValue::Int(config.read_url.max_chars as i64),
        "code.memory_max_rows" => EditValue::Int(config.code.memory_max_rows),
        "usage.retention_days" => EditValue::Int(config.usage.retention_days as i64),
        _ => return None,
    })
}

/// The environment variable that shadows a key at startup.
fn read_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// Why a config edit did not land. Rejections are the caller's input (400);
/// internal failures are the volume or an unreadable file (500).
#[derive(Debug)]
pub(crate) enum ConfigWriteError {
    Rejected(String),
    Internal(&'static str),
}

impl ConfigWriteError {
    fn rejected(message: impl Into<String>) -> Self {
        Self::Rejected(message.into())
    }

    const fn internal(message: &'static str) -> Self {
        Self::Internal(message)
    }

    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Rejected(message) => message,
            Self::Internal(message) => message,
        }
    }

    pub(crate) fn is_internal(&self) -> bool {
        matches!(self, Self::Internal(_))
    }
}

pub(crate) struct ConfigEditor {
    path: PathBuf,
    /// Injected so a test can decide what the environment contains without
    /// mutating the process-wide variables its siblings read.
    env: fn(&str) -> Option<String>,
}

impl ConfigEditor {
    pub(crate) fn resolve() -> Self {
        Self::new(config_path(), read_env)
    }

    pub(crate) fn new(path: PathBuf, env: fn(&str) -> Option<String>) -> Self {
        Self { path, env }
    }

    fn shadowed(&self, name: &str) -> bool {
        (self.env)(name).is_some()
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn describe(&self, config: &Config) -> Vec<KeyView> {
        EDITABLE
            .iter()
            .filter_map(|entry| {
                let value = effective(config, entry.key)?;
                Some(KeyView {
                    key: entry.key,
                    label: entry.label,
                    kind: entry.kind,
                    value,
                    env_shadowed: entry.env.is_some_and(|name| self.shadowed(name)),
                    env_variable: entry.env,
                    restart_required: true,
                })
            })
            .collect()
    }

    pub(crate) async fn write(&self, key: &str, value: &EditValue) -> Result<(), ConfigWriteError> {
        let entry = EDITABLE
            .iter()
            .find(|entry| entry.key == key)
            .ok_or_else(|| ConfigWriteError::rejected(format!("неизвестный ключ: {key}")))?;
        if let Some(variable) = entry.env {
            if self.shadowed(variable) {
                return Err(ConfigWriteError::rejected(format!(
                    "{key} задаётся переменной окружения {variable}; правка файла не действует"
                )));
            }
        }
        if !value.matches(entry.kind) {
            return Err(ConfigWriteError::rejected(format!(
                "{key} ожидает {}",
                entry.kind.describe()
            )));
        }
        let mut document = self.document().await?;
        let item = match value {
            EditValue::Int(number) => Item::Value(TomlValue::from(*number)),
            EditValue::Str(text) => Item::Value(TomlValue::from(text.as_str())),
        };
        set_path(&mut document, key, item)?;
        let text = document.to_string();
        // The candidate must load exactly like startup, or the file would make
        // the next start fail instead of applying the edit.
        Config::preview(&text).map_err(ConfigWriteError::rejected)?;
        self.persist(&text).await
    }

    async fn document(&self) -> Result<DocumentMut, ConfigWriteError> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => text
                .parse::<DocumentMut>()
                .map_err(|_| ConfigWriteError::internal("config.toml не разбирается")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
            Err(_) => Err(ConfigWriteError::internal(
                "не удалось прочитать config.toml",
            )),
        }
    }

    async fn persist(&self, text: &str) -> Result<(), ConfigWriteError> {
        let path = self.path();
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let filename = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".to_string());
        let temp = directory.join(format!(".{filename}.tmp"));
        let backup = directory.join(format!("{filename}.bak"));
        // A 0600 file must not become world-readable through the rewrite.
        #[cfg(unix)]
        let permissions = tokio::fs::metadata(path)
            .await
            .ok()
            .map(|metadata| metadata.permissions());
        let mut file = tokio::fs::File::create(&temp)
            .await
            .map_err(|_| ConfigWriteError::internal("не удалось создать временный файл"))?;
        file.write_all(text.as_bytes())
            .await
            .map_err(|_| ConfigWriteError::internal("не удалось записать config.toml"))?;
        file.sync_all()
            .await
            .map_err(|_| ConfigWriteError::internal("не удалось записать config.toml"))?;
        drop(file);
        #[cfg(unix)]
        if let Some(permissions) = permissions {
            tokio::fs::set_permissions(&temp, permissions)
                .await
                .map_err(|_| {
                    ConfigWriteError::internal("не удалось сохранить права config.toml")
                })?;
        }
        if tokio::fs::metadata(path).await.is_ok() {
            tokio::fs::copy(path, &backup)
                .await
                .map_err(|_| ConfigWriteError::internal("не удалось сохранить резервную копию"))?;
        }
        tokio::fs::rename(&temp, path)
            .await
            .map_err(|_| ConfigWriteError::internal("не удалось заменить config.toml"))?;
        // A rename is durable only after the directory entry is synced; without
        // this a crash can silently restore the previous file after the API
        // reported the edit as written.
        match tokio::fs::File::open(directory).await {
            Ok(directory) => {
                if directory.sync_all().await.is_err() {
                    tracing::warn!(
                        stage = "web-config-sync",
                        "config.toml directory sync failed"
                    );
                }
            }
            Err(_) => {
                tracing::warn!(
                    stage = "web-config-sync",
                    "unable to open the config directory for a sync"
                );
            }
        }
        Ok(())
    }
}

/// Writes one dotted key, keeping the existing value's decor so an inline
/// comment on that line survives the edit.
fn set_path(document: &mut DocumentMut, key: &str, item: Item) -> Result<(), ConfigWriteError> {
    let parts: Vec<&str> = key.split('.').collect();
    let Some((last, parents)) = parts.split_last() else {
        return Err(ConfigWriteError::rejected("пустой ключ"));
    };
    let mut table = document.as_table_mut();
    for part in parents {
        let entry = table
            .entry(part)
            .or_insert_with(|| Item::Table(Table::new()));
        table = entry
            .as_table_mut()
            .ok_or_else(|| ConfigWriteError::internal("секция в config.toml — не таблица"))?;
    }
    if let Some(existing) = table.get_mut(last) {
        let Some(old) = existing.as_value() else {
            return Err(ConfigWriteError::internal("ключ в config.toml — не скаляр"));
        };
        let decor = old.decor().clone();
        *existing = item;
        if let Some(new) = existing.as_value_mut() {
            *new.decor_mut() = decor;
        }
        return Ok(());
    }
    table.insert(last, item);
    Ok(())
}
