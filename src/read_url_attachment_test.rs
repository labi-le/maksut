use crate::bounded_fetch::{FetchCategory, FetchError, Fetched};
use crate::chat_client::ChatConfig;
use crate::chat_tools::{ToolContext, ToolState};
use crate::media::MediaKind;
use crate::read_url_attachment::queue;
use crate::research_attachments::ResearchAttachment;
use crate::search::SearchProvider;
use crate::test_support::{test_pool, MockAttachmentFetcher};
use crate::*;

fn attachment(url: &str, filename: &str) -> ResearchAttachment {
    ResearchAttachment {
        url: url.to_owned(),
        filename: filename.to_owned(),
        prepared: None,
    }
}

fn png() -> Vec<u8> {
    vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0]
}

fn jpeg() -> Vec<u8> {
    vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F']
}

fn image(url_path: &str) -> Fetched {
    Fetched {
        body: png(),
        content_type: "image/png".to_owned(),
        host: Some("files.example.com".to_owned()),
        path: Some(url_path.to_owned()),
    }
}

struct FileGuard(PathBuf);

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn context<'a>(
    chat: &'a ChatConfig,
    pool: &'a SqlitePool,
    stickers: &'a StickerReplies,
    catalog: &'a StickerCatalog,
) -> ToolContext<'a> {
    ToolContext {
        chat,
        pool,
        stickers,
        catalog,
        proposal: None,
        owner: true,
        chat_id: ChatId(1),
        user_id: UserId(1),
        sender_name: "tester",
        username: None,
        message_id: 1,
        reply_user_id: None,
        bot: None,
        read_url_targets: &[],
    }
}

#[tokio::test]
async fn non_photo_keeps_the_lazy_promise() {
    let (_dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let mut chat = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let fetcher = Arc::new(MockAttachmentFetcher::new());
    chat.attachment_fetcher = fetcher.clone();
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = context(&chat, &pool, &stickers, &catalog).await;
    let mut state = ToolState::new(false, false);

    let output = queue(
        &ctx,
        &mut state,
        attachment("https://files.example.com/book.pdf", "book.pdf"),
        3,
    )
    .await;

    assert_eq!(
        output,
        "файл book.pdf будет отправлен в чат вместе с ответом"
    );
    assert_eq!(state.read_url_calls, 1);
    assert_eq!(state.research.attachments.len(), 1);
    assert!(state.research.attachments[0].prepared.is_none());
    assert_eq!(fetcher.calls(), 0);
}

#[tokio::test]
async fn duplicate_url_is_not_downloaded_twice() {
    let (_dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let mut chat = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let fetcher = Arc::new(MockAttachmentFetcher::new());
    fetcher.push(Ok(image("/cat.png")));
    chat.attachment_fetcher = fetcher.clone();
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = context(&chat, &pool, &stickers, &catalog).await;
    let mut state = ToolState::new(false, false);
    let first = attachment("https://files.example.com/cat.png", "cat.png");

    let output = queue(&ctx, &mut state, first.clone(), 3).await;
    assert!(output.contains("готова к отправке"), "{output}");
    let prepared = state.research.attachments[0]
        .prepared
        .as_ref()
        .expect("prepared image");
    let _guard = FileGuard(prepared.path.clone());

    let output = queue(&ctx, &mut state, first, 3).await;

    assert_eq!(output, "картинка cat.png уже в очереди на отправку");
    assert_eq!(fetcher.calls(), 1, "the duplicate must not be downloaded");
    assert_eq!(state.research.attachments.len(), 1);
}

#[tokio::test]
async fn a_full_attachment_budget_removes_the_prepared_file() {
    let (_dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let mut chat = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let fetcher = Arc::new(MockAttachmentFetcher::new());
    fetcher.push(Ok(Fetched {
        body: jpeg(),
        content_type: "image/jpeg".to_owned(),
        host: Some("files.example.com".to_owned()),
        path: Some("/cat.jpg".to_owned()),
    }));
    chat.attachment_fetcher = fetcher.clone();
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = context(&chat, &pool, &stickers, &catalog).await;
    let mut state = ToolState::new(false, false);

    let output = queue(
        &ctx,
        &mut state,
        attachment("https://files.example.com/cat.png", "cat.png"),
        0,
    )
    .await;

    assert_eq!(output, "лимит вложений на ответ");
    assert!(state.research.attachments.is_empty());
    let dir = PathBuf::from(&config().paths.download_dir);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("attach-") {
                assert_ne!(
                    std::fs::read(entry.path()).unwrap_or_default(),
                    jpeg(),
                    "the rejected prepared file must be removed"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_429_is_reported_without_queueing() {
    let (_dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let mut chat = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let fetcher = Arc::new(MockAttachmentFetcher::new());
    fetcher.push(Err(FetchError {
        category: FetchCategory::Http,
        status: Some(429),
        host: Some("files.example.com".to_owned()),
        path: Some("/cat.png".to_owned()),
        message: "хост ответил HTTP 429".to_owned(),
    }));
    chat.attachment_fetcher = fetcher.clone();
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = context(&chat, &pool, &stickers, &catalog).await;
    let mut state = ToolState::new(false, false);

    let output = queue(
        &ctx,
        &mut state,
        attachment("https://files.example.com/cat.png", "cat.png"),
        3,
    )
    .await;

    assert!(
        output.contains("не отправлено: сайт ограничил запросы"),
        "{output}"
    );
    assert!(state.research.attachments.is_empty());
    assert_eq!(fetcher.calls(), 1);
}

#[test]
fn photo_attachment_is_recognised_by_kind() {
    assert_eq!(
        attachment("https://files.example.com/cat.png", "cat.png").kind(),
        MediaKind::Photo
    );
}

#[tokio::test]
async fn an_exhausted_turn_budget_does_not_touch_the_network() {
    let (_dir, pool) = test_pool().await;
    let url = Url::parse("http://127.0.0.1:1/v1").unwrap();
    let mut chat = ChatConfig::new(
        url.clone(),
        "key".into(),
        "deepseek-v4.1-flash".into(),
        url,
        SearchProvider::DuckDuckGo,
        None,
    );
    let fetcher = Arc::new(MockAttachmentFetcher::new());
    fetcher.push(Ok(image("/cat.png")));
    chat.attachment_fetcher = fetcher.clone();
    let stickers = StickerReplies::for_tests();
    let catalog = StickerCatalog::for_tests(&[]);
    let ctx = context(&chat, &pool, &stickers, &catalog).await;
    let mut state = ToolState::new(false, false);
    state.attachment_budget = Some(Duration::ZERO);

    let output = queue(
        &ctx,
        &mut state,
        attachment("https://files.example.com/cat.png", "cat.png"),
        3,
    )
    .await;

    assert!(output.contains("лимит времени на вложения"), "{output}");
    assert_eq!(fetcher.calls(), 0);
    assert!(state.research.attachments.is_empty());
}

#[tokio::test]
async fn cleanup_prepared_removes_a_file_left_by_a_failed_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("attach-test.png");
    std::fs::write(&path, png()).unwrap();
    let mut state = ToolState::new(false, false);
    state.research.attachments.push(ResearchAttachment {
        url: "https://files.example.com/cat.png".to_owned(),
        filename: "cat.png".to_owned(),
        prepared: Some(crate::attachment_image::PreparedAttachment {
            path: path.clone(),
            kind: MediaKind::Photo,
            filename: "attach-test.png".to_owned(),
            host: Some("files.example.com".to_owned()),
            url_path: Some("/cat.png".to_owned()),
            bytes: png().len() as u64,
        }),
    });

    state.cleanup_prepared().await;

    assert!(!path.exists(), "the prepared file must be removed");
    assert!(state.research.attachments.is_empty());
}
