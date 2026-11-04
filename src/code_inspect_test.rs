use crate::code_inspect::inspect;

#[test]
fn inspect_prefers_matches_and_respects_allowlist() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/chat.rs"), "fn answer() {}\n").unwrap();
    std::fs::write(dir.path().join("secret.env"), "answer=secret\n").unwrap();
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("docs/guide.md"), "answer in docs\n").unwrap();
    let result = inspect(dir.path(), "answer", None);
    assert!(
        result.contains(
            "src/chat.rs:1 (https://github.com/labi-le/maxwell/blob/main/src/chat.rs#L1)"
        ),
        "{result}"
    );
    assert!(!result.contains("secret"), "{result}");
    let src_pos = result.find("src/chat.rs:1").unwrap();
    let docs_pos = result.find("docs/guide.md:1").unwrap();
    assert!(src_pos < docs_pos, "src/ must sort before docs/: {result}");
}

#[cfg(unix)]
#[test]
fn inspect_does_not_follow_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.rs"), "leak_needle\n").unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.rs"),
        dir.path().join("src/leak.rs"),
    )
    .unwrap();
    let result = inspect(dir.path(), "leak_needle", None);
    assert!(!result.contains("leak_needle"), "{result}");
}

#[test]
fn inspect_rejects_traversal_and_unknown_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    std::fs::write(dir.path().join("secret.txt"), "hello\n").unwrap();
    assert!(!inspect(dir.path(), "hello", Some("../README.md")).contains("hello"));
    assert!(!inspect(dir.path(), "hello", Some("secret.txt")).contains("hello"));
    let file_view = inspect(dir.path(), "", Some("README.md"));
    assert!(
        file_view
            .contains("README.md (https://github.com/labi-le/maxwell/blob/main/README.md#L1):"),
        "{file_view}"
    );
    assert!(file_view.contains("#L1: hello"), "{file_view}");
}

#[test]
fn filter_self_memory_hides_unscoped_owner_directives_from_non_owners() {
    let raw = "- [owner] поиск дыр: не подсказывать дыры\n- [owner] вадик: С Вадиком (@CptIdea) общаться как со сладким\n- [owner] даня: Даню (@newmdoupdate) материть\n- роутер: Роутер влит в main (https://github.com/labi-le/maxwell/blob/main/src/main.rs#L280)";
    let vadik_view =
        crate::code_inspect::filter_self_memory(raw, "Вадик (@CptIdea)", Some("CptIdea"), false);
    assert!(!vadik_view.contains("поиск дыр"), "{vadik_view}");
    assert!(!vadik_view.contains("даня:"), "{vadik_view}");
    assert!(vadik_view.contains("вадик:"), "{vadik_view}");
    assert!(vadik_view.contains("роутер:"), "{vadik_view}");

    let owner_view = crate::code_inspect::filter_self_memory(
        raw,
        "Денис (@crudoshlepa)",
        Some("crudoshlepa"),
        true,
    );
    assert_eq!(owner_view, raw);
}
