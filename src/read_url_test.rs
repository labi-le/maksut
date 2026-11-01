use crate::prelude::*;
use crate::read_url::*;

#[test]
fn validates_only_public_http_urls() {
    for raw in [
        "http://localhost/",
        "http://127.0.0.1/",
        "http://10.0.0.1/",
        "http://192.168.1.1/",
        "http://intranet/",
        "file:///etc/passwd",
        "ftp://example.com/",
        "http://user:pass@example.com/",
        "http://example.com:8080/",
    ] {
        let url = Url::parse(raw).unwrap();
        assert!(validate_url(&url).is_err(), "{raw} must be rejected");
    }
    assert!(validate_url(&Url::parse("https://example.com/article").unwrap()).is_ok());
    assert!(validate_url(&Url::parse("http://example.com/article").unwrap()).is_ok());
}

#[test]
fn extracts_article_and_skips_chrome() {
    let html = r#"
        <html><head><title>Заголовок статьи</title>
        <style>body { color: red }</style></head>
        <body>
          <nav>меню сайта</nav>
          <article>
            <h1>Заголовок статьи</h1>
            <p>Первый <b>абзац</b> статьи.</p>
            <p>Второй абзац.</p>
            <script>alert('x')</script>
          </article>
          <footer>подвал</footer>
        </body></html>
    "#;
    let (title, text) = extract_html_with_links(
        html,
        &Url::parse("https://example.com/article").unwrap(),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(title.as_deref(), Some("Заголовок статьи"));
    let text = normalize_text(&text);
    assert!(text.contains("Первый"), "{text}");
    assert!(text.contains("абзац"), "{text}");
    assert!(text.contains("статьи."), "{text}");
    assert!(text.contains("Второй абзац."), "{text}");
    assert!(!text.contains("меню сайта"), "{text}");
    assert!(!text.contains("подвал"), "{text}");
    assert!(!text.contains("alert"), "{text}");
}

#[test]
fn renders_text_with_a_limit() {
    let body = b"hello\n\n\nworld";
    let page = render_page(
        &Url::parse("https://example.com/a").unwrap(),
        "text/plain",
        body,
    )
    .unwrap();
    assert!(page.text.contains("hello"));
    assert!(page.text.contains("world"));
}

#[tokio::test]
#[ignore = "network smoke test"]
async fn live_fetch_example_page() {
    let page = read("https://example.com/").await.unwrap();
    assert!(page.text.contains("Example Domain"), "{}", page.text);
}

#[tokio::test]
#[ignore = "network smoke test"]
async fn live_fetch_article_page() {
    let page = read("https://en.wikipedia.org/wiki/Rust_(programming_language)")
        .await
        .unwrap();
    assert!(page.text.contains("Rust"), "{}", page.text);
    assert!(page.text.chars().count() <= READ_URL_MAX_CHARS + 200);
}

#[test]
fn fallback_drops_scripts_and_preserves_spacing() {
    let document = strip_chrome(dom_query::Document::from(
        "<html><body><script>alert('secret')</script><style>.x{}</style>             <p>hello <b>world</b></p></body></html>",
    ));
    let text = fallback_text(&document);
    assert!(text.contains("hello world"), "{text}");
    assert!(!text.contains("secret"), "{text}");
    assert!(!text.contains(".x"), "{text}");
}

#[test]
fn oversized_titles_are_capped_and_sanitized() {
    let raw = format!("{}\n\u{0007}{}", "a".repeat(5_000), "b".repeat(5_000));
    let title = sanitize_title(&raw);
    assert!(title.chars().count() <= READ_URL_TITLE_CHARS);
    assert!(!title.chars().any(char::is_control));
}

#[test]
fn tiny_pages_fall_back_to_body_text() {
    let html = "<html><head><title>Tiny</title></head><body><p>hello</p></body></html>";
    let (title, text) = extract_html_with_links(
        html,
        &Url::parse("https://example.com/tiny").unwrap(),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(title.as_deref(), Some("Tiny"));
    assert!(text.contains("hello"), "{text}");
}

#[test]
fn decodes_declared_non_utf8_charsets() {
    let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode("привет");
    assert_eq!(
        decode_body(&bytes, "text/plain; charset=windows-1251"),
        "привет"
    );
}

#[test]
fn only_urls_from_the_prompt_are_allowed() {
    let prompt = "прочитай https://example.com/article?x=1 и всё";
    let allowed = prompt_urls(prompt);
    assert!(allowed_target(
        "https://example.com/article?x=1",
        &allowed,
        false
    ));
    assert!(!allowed_target(
        "https://example.com/article?x=2",
        &allowed,
        false
    ));
    assert!(!allowed_target(
        "https://attacker.example/?q=chat-summary",
        &allowed,
        false
    ));
    assert!(!allowed_target(
        "https://example.com/article?x=1&q=secret",
        &allowed,
        true
    ));
    assert!(allowed_target(
        "https://example.com/other-page",
        &allowed,
        true
    ));
}

#[test]
fn deeply_nested_html_does_not_overflow() {
    let html = format!(
        "{}deep text{}",
        "<div>".repeat(5_000),
        "</div>".repeat(5_000)
    );
    assert!(matches!(
        extract_html_with_links(
            &html,
            &Url::parse("https://example.com/deep").unwrap(),
            &mut Vec::new(),
        ),
        Err(ReadUrlError::TooComplex)
    ));
}

#[test]
fn truncation_does_not_split_utf8() {
    let (text, truncated) = truncate_chars("привет", 2);
    assert_eq!(text, "пр");
    assert!(truncated);
}

#[test]
fn table_layout_with_sidebar_stub_preserves_main_body_and_links() {
    let items = (1..=20)
        .map(|i| {
            format!("<li><a href=\"/node/{i}\">Новость номер {i} про разработку и книги</a></li>")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let html = format!(
        "<html><head><title>Сайт Столярова</title></head><body>\
         <table><tr><td><h1>Последние новости</h1><ul>{items}</ul></td>\
         <td><div class=\"rblock\"><h4>пояснение</h4><p>Вы находитесь на официальном сайте Андрея Викторовича Столярова.</p></div></td></tr></table>\
         </body></html>"
    );
    let page = render_page(
        &Url::parse("http://stolyarov.info/").unwrap(),
        "text/html",
        html.as_bytes(),
    )
    .unwrap();
    assert!(page.text.contains("Новость номер 1"), "{}", page.text);
    assert!(
        page.text.contains("http://stolyarov.info/node/1"),
        "{}",
        page.text
    );
    assert!(page
        .links
        .contains(&Url::parse("http://stolyarov.info/node/1").unwrap()));
}

#[test]
fn render_page_caps_links_and_stays_within_max_chars() {
    let body = (0..200)
        .map(|i| format!("https://example.com/item/{i} "))
        .collect::<String>()
        + &"слово ".repeat(6000);
    let long_url = Url::parse(&format!("https://example.com/{}", "a".repeat(25_000))).unwrap();
    let long_kind = format!("text/plain{}", "x".repeat(25_000));
    let page = render_page(&long_url, &long_kind, body.as_bytes()).unwrap();
    assert!(page.links.len() <= 32, "links: {}", page.links.len());
    assert!(
        page.text.chars().count() <= READ_URL_MAX_CHARS,
        "chars: {}",
        page.text.chars().count()
    );
}

#[test]
fn xml_rss_extracts_links_and_html_footer_includes_anchor_labels() {
    let rss = r#"<?xml version="1.0" encoding="UTF-8"?>
        <rss version="2.0"><channel><title>Новости</title>
        <item><title>Заметка</title><link>http://stolyarov.info/Guestbook/2026_07.html#n17</link></item>
        </channel></rss>"#
        .as_bytes();
    let rss_page = render_page(
        &Url::parse("http://www.stolyarov.info/rss.xml").unwrap(),
        "application/rss+xml",
        rss,
    )
    .unwrap();
    assert!(
        rss_page
            .links
            .contains(&Url::parse("http://stolyarov.info/Guestbook/2026_07.html").unwrap()),
        "{:?}",
        rss_page.links
    );
    let html = r#"<html><head><title>Главная</title></head><body>
        <p>Короткий текст.</p>
        <a href="/news.html">Свежие новости</a>
        </body></html>"#
        .as_bytes();
    let html_page = render_page(
        &Url::parse("http://stolyarov.info/").unwrap(),
        "text/html",
        html,
    )
    .unwrap();
    assert!(
        html_page
            .text
            .contains("- Свежие новости: http://stolyarov.info/news.html"),
        "{}",
        html_page.text
    );
}
