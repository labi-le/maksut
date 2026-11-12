use crate::web_config::{effective, ConfigEditor, ConfigWriteError, EditKind, EditValue, EDITABLE};
use crate::*;

fn no_env(_name: &str) -> Option<String> {
    None
}

fn media_env(name: &str) -> Option<String> {
    (name == "MAX_MEDIA_BYTES").then(|| "1000".to_string())
}

fn editor(path: PathBuf) -> ConfigEditor {
    ConfigEditor::new(path, no_env)
}

#[tokio::test]
async fn edit_preserves_comments_and_other_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(
        &path,
        "# top comment\n[chat]\n# reply comment\nreply_max_chars = 3900  # inline\ntool_rounds = 8\n",
    )
    .unwrap();
    editor(path.clone())
        .write("chat.reply_max_chars", &EditValue::Int(3800))
        .await
        .unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("# top comment"), "{text}");
    assert!(text.contains("# reply comment"), "{text}");
    assert!(text.contains("# inline"), "{text}");
    assert!(text.contains("reply_max_chars = 3800"), "{text}");
    assert!(text.contains("tool_rounds = 8"), "{text}");
    assert_eq!(
        Config::parse(&text).unwrap().chat.reply_max_chars,
        3800,
        "{text}"
    );
}

#[tokio::test]
async fn creates_a_missing_file_with_only_the_edited_section() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    editor(path.clone())
        .write("memory.ignore_secs", &EditValue::Int(3600))
        .await
        .unwrap();
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[memory]"), "{text}");
    assert_eq!(
        Config::parse(&text).unwrap().memory.ignore_secs,
        3600,
        "{text}"
    );
}

#[tokio::test]
async fn refuses_unknown_keys_and_wrong_types() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[chat]\ntool_rounds = 8\n").unwrap();
    let editor = editor(path.clone());
    let unknown = editor
        .write("paths.database_url", &EditValue::Str("x".into()))
        .await;
    assert!(matches!(unknown, Err(ConfigWriteError::Rejected(_))));
    let missing = editor.write("chat.missing", &EditValue::Int(1)).await;
    assert!(matches!(missing, Err(ConfigWriteError::Rejected(_))));
    let wrong_type = editor
        .write("chat.tool_rounds", &EditValue::Str("8".into()))
        .await;
    assert!(matches!(wrong_type, Err(ConfigWriteError::Rejected(_))));
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[chat]\ntool_rounds = 8\n"
    );
}

#[tokio::test]
async fn refuses_a_key_that_the_environment_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[media]\nmax_bytes = 45000000\n").unwrap();
    let editor = ConfigEditor::new(path.clone(), media_env);
    let error = editor
        .write("media.max_bytes", &EditValue::Int(1_000_000))
        .await
        .unwrap_err();
    assert!(
        error.message().contains("MAX_MEDIA_BYTES"),
        "{}",
        error.message()
    );
    assert!(!error.is_internal());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "[media]\nmax_bytes = 45000000\n"
    );
}

#[tokio::test]
async fn refuses_a_value_that_fails_startup_validation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "[chat]\ntool_rounds = 8\n";
    fs::write(&path, original).unwrap();
    let error = editor(path.clone())
        .write("chat.tool_rounds", &EditValue::Int(0))
        .await
        .unwrap_err();
    assert!(
        error.message().contains("chat.tool_rounds"),
        "{}",
        error.message()
    );
    assert!(!error.is_internal());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
}

#[tokio::test]
async fn keeps_a_backup_of_the_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let original = "[chat]\ntool_rounds = 8\n";
    fs::write(&path, original).unwrap();
    editor(path.clone())
        .write("chat.tool_rounds", &EditValue::Int(10))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(path.with_file_name("config.toml.bak")).unwrap(),
        original
    );
}

#[cfg(unix)]
#[tokio::test]
async fn keeps_the_original_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[chat]\ntool_rounds = 8\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    editor(path.clone())
        .write("chat.tool_rounds", &EditValue::Int(9))
        .await
        .unwrap();
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn whitelist_entries_have_effective_values() {
    let config = Config::default();
    for entry in EDITABLE {
        assert!(
            effective(&config, entry.key).is_some(),
            "{} has no effective value",
            entry.key
        );
    }
    assert!(effective(&config, "paths.database_url").is_none());
}

#[test]
fn describe_marks_environment_shadowed_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let views = ConfigEditor::new(path, media_env).describe(&Config::default());
    let media = views
        .iter()
        .find(|view| view.key == "media.max_bytes")
        .unwrap();
    assert!(media.env_shadowed);
    assert_eq!(media.env_variable, Some("MAX_MEDIA_BYTES"));
    assert!(media.restart_required);
    let chat = views
        .iter()
        .find(|view| view.key == "chat.tool_rounds")
        .unwrap();
    assert!(!chat.env_shadowed);
    assert_eq!(chat.kind, EditKind::Int);
}

#[test]
fn web_section_defaults_and_validation() {
    let default = Config::default();
    assert!(!default.web.enabled);
    assert_eq!(default.web.listen, "0.0.0.0:8090");
    assert_eq!(default.web.auth_max_age_secs, 86_400);
    default.validate().unwrap();

    let mut bad_listen = default.clone();
    bad_listen.web.listen = "not-a-socket".into();
    assert!(bad_listen.validate().is_err());

    let mut plain = default.clone();
    plain.web.public_url = Some("http://example.org/app".into());
    assert!(plain.validate().is_err());

    let mut queried = default.clone();
    queried.web.public_url = Some("https://example.org/app?q=1".into());
    assert!(queried.validate().is_err());

    let mut secure = default.clone();
    secure.web.public_url = Some("https://example.org/app".into());
    secure.validate().unwrap();

    let mut stale = default.clone();
    stale.web.auth_max_age_secs = 59;
    assert!(stale.validate().is_err());

    let mut huge = default.clone();
    huge.web.max_body_bytes = 2 * 1024 * 1024;
    assert!(huge.validate().is_err());
}

#[test]
fn example_template_carries_the_web_defaults() {
    let parsed = Config::parse(include_str!("../example.config.toml")).unwrap();
    assert!(!parsed.web.enabled);
    assert_eq!(parsed.web.listen, Config::default().web.listen);
    assert_eq!(parsed.web.public_url, None);
}

#[test]
fn preview_rejects_a_candidate_that_startup_would_reject() {
    assert!(Config::preview("[chat]\nreply_max_chars = 0\n").is_err());
    assert!(Config::preview("[web]\nlisten = \"nope\"\n").is_err());
    assert!(Config::preview("[chat]\nreply_max_chars = 3900\n").is_ok());
}

#[test]
fn every_editable_key_is_unique() {
    let mut keys: Vec<&str> = EDITABLE.iter().map(|entry| entry.key).collect();
    keys.sort_unstable();
    let before = keys.len();
    keys.dedup();
    assert_eq!(keys.len(), before);
}

#[cfg(unix)]
#[tokio::test]
async fn classifies_a_write_failure_as_internal() {
    use std::os::unix::fs::PermissionsExt;
    // root ignores the directory mode, so the failure cannot be provoked.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "[chat]\ntool_rounds = 8\n").unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = editor(path.clone())
        .write("chat.tool_rounds", &EditValue::Int(9))
        .await;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let error = result.unwrap_err();
    assert!(error.is_internal(), "{}", error.message());
}
