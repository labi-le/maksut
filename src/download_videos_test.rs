use crate::test_support::*;
use crate::*;

#[cfg(unix)]
#[tokio::test]
async fn tiktok_requests_carry_firefox_user_agent_only_for_tiktok_hosts() {
    let root = tempfile::tempdir().unwrap();
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' download >> '{0}/calls'
printf '%s\n' "$@" > '{0}/args'
exit 1
"#,
        root.path().display()
    );
    let cfg = fake_settings(root.path(), &script).await;
    let tiktok = allowed_url("https://www.tiktok.com/@name/video/111").unwrap();
    let work = tempfile::tempdir_in(root.path()).unwrap();
    let result = download_with_timeout(&tiktok, &cfg, work.path(), Duration::from_secs(3))
        .await
        .unwrap();
    assert!(!result.process_ok);
    let args = fs::read_to_string(root.path().join("args")).unwrap();
    assert!(args.contains(&format!("--add-header\nUser-Agent: {TIKTOK_USER_AGENT}\n")));
    assert!(args.contains("-S\nvcodec:h264,res\n"));
    assert!(!args.lines().any(|arg| arg == "--cookies"));
    assert_eq!(
        fs::read_to_string(root.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    let youtube = allowed_url("https://youtu.be/abc").unwrap();
    let work = tempfile::tempdir_in(root.path()).unwrap();
    let result = download_with_timeout(&youtube, &cfg, work.path(), Duration::from_secs(3))
        .await
        .unwrap();
    assert!(!result.process_ok);
    let args = fs::read_to_string(root.path().join("args")).unwrap();
    assert!(!args
        .lines()
        .any(|arg| arg == "--add-header" || arg.starts_with("User-Agent:")));
    assert!(!args.lines().any(|arg| arg == "-S"));
    assert_eq!(
        fs::read_to_string(root.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[cfg(unix)]
#[tokio::test]
async fn oversized_video_pair_retries_with_a_size_bounded_format() {
    let root = tempfile::tempdir().unwrap();
    let script = format!(
        r#"#!/bin/sh
all="$*"
out=""
manifest=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output) shift; out=$1 ;;
    --print-to-file) shift; shift; manifest=$1 ;;
  esac
  shift
done
dir=${{out%/*}}
case "$all" in
  *"filesize<="*)
    printf '%s\n' bounded >> '{0}/calls'
    printf '%s\n' "$all" > '{0}/bounded-args'
    printf mp4 > "$dir/result.mp4"
    printf '%s\n' "$dir/result.mp4" > "$manifest"
    ;;
  *)
    printf '%s\n' first >> '{0}/calls'
    printf audio > "$dir/result.mp3"
    ;;
esac
exit 0
"#,
        root.path().display()
    );
    let cfg = fake_settings(root.path(), &script).await;
    let url = allowed_url("https://www.tiktok.com/@name/video/111").unwrap();
    let work = tempfile::tempdir_in(root.path()).unwrap();
    let result = download_videos(&url, &cfg, work.path(), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(result.selection.files.len(), 1);
    assert_eq!(result.selection.files[0].kind, MediaKind::Video);
    let args = fs::read_to_string(root.path().join("bounded-args")).unwrap();
    assert!(
        args.contains(&format!("filesize<={}", cfg.max_media_bytes)),
        "{args}"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("calls"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}
