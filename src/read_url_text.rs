pub(crate) fn decode_body(body: &[u8], content_type: &str) -> String {
    let header_encoding = content_type.split(';').skip(1).find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("charset") {
            return None;
        }
        let label = value.trim().trim_matches(['"', '\'']);
        encoding_rs::Encoding::for_label(label.as_bytes())
    });
    let encoding = header_encoding.unwrap_or_else(|| {
        let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
        detector.feed(body, true);
        detector.guess(None, chardetng::Utf8Detection::Allow)
    });
    encoding.decode(body).0.into_owned()
}

pub(crate) fn normalize_text(raw: &str) -> String {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() || lines.last().is_some_and(|prev: &String| !prev.is_empty()) {
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_owned()
}

pub(crate) fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    if max == 0 {
        return (String::new(), !text.is_empty());
    }
    match text.char_indices().nth(max) {
        Some((index, _)) => (text.get(..index).unwrap_or_default().to_owned(), true),
        None => (text.to_owned(), false),
    }
}
