//! Assistant text sanitizing: tool-call syntax that some providers emit as
//! ordinary message text, and the inline sticker call the model may print
//! instead of calling the tool.

/// Removes the XML `<tool_call>` block and returns the sticker hint it carried.
/// Runs for every delivered reply, so the no-tag path returns the input
/// without a second allocation.
pub(crate) fn strip_raw_tool_call_tags(text: &str) -> (String, Option<String>) {
    let (cleaned, hint) = strip_xml_tool_tags(text);
    if !cleaned.contains('<') {
        return (cleaned, hint);
    }
    (strip_provider_tool_markup(&cleaned), hint)
}

fn strip_xml_tool_tags(text: &str) -> (String, Option<String>) {
    if !text.contains("<tool_call>") && !text.contains("</tool_call>") {
        return (text.to_owned(), None);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut sticker_hint = None;
    while let Some(open) = rest.find("<tool_call>") {
        out.push_str(rest.get(..open).unwrap_or(""));
        let after_open = rest.get(open + "<tool_call>".len()..).unwrap_or("");
        let (inner, next_rest) = match after_open.find("</tool_call>") {
            Some(close) => (
                after_open.get(..close).unwrap_or(""),
                after_open.get(close + "</tool_call>".len()..).unwrap_or(""),
            ),
            None => (after_open, ""),
        };
        if sticker_hint.is_none() {
            sticker_hint = parse_inline_sticker_call(inner);
        }
        rest = next_rest;
    }
    out.push_str(rest);
    let cleaned = out.replace("</tool_call>", "").trim().to_owned();
    (cleaned, sticker_hint)
}

fn parse_inline_sticker_call(raw: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let name = val
        .get("name")
        .or_else(|| val.pointer("/function/name"))
        .and_then(serde_json::Value::as_str)?;
    if name != crate::ToolKind::SendSticker.name() {
        return None;
    }
    let args = val
        .get("arguments")
        .or_else(|| val.pointer("/function/arguments"))?;
    let parsed_args = if let Some(s) = args.as_str() {
        serde_json::from_str::<serde_json::Value>(s).ok()?
    } else {
        args.clone()
    };
    for key in ["sticker_id", "query", "name"] {
        if let Some(v) = parsed_args
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(v.to_owned());
        }
    }
    Some(String::new())
}

/// Some providers emit their native tool-call syntax as ordinary assistant
/// text when the turn is called without a tool schema (the final synthesis
/// round). Fullwidth bars (U+FF5C) never occur in prose worth delivering, so
/// an opening tag is dropped together with everything up to its closing tag:
/// the `<｜｜DSML｜｜ parameter>URL</…>` pair also hides the URL it carried.
/// An opening tag with no closer means the markup was truncated, and the rest
/// of the message is its payload, not prose.
pub(crate) fn strip_provider_tool_markup(text: &str) -> String {
    const BAR: char = '\u{FF5C}';
    const CLOSE: &str = "</\u{FF5C}";
    if !text.contains('<') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut removed = false;
    while let Some(pos) = rest.find('<') {
        out.push_str(rest.get(..pos).unwrap_or(""));
        let after = rest.get(pos..).unwrap_or("");
        let candidate = after
            .strip_prefix("</")
            .or_else(|| after.strip_prefix('<'))
            .unwrap_or(after);
        if !candidate.starts_with(BAR) {
            out.push('<');
            rest = after.get(1..).unwrap_or("");
            continue;
        }
        removed = true;
        let closing = after.starts_with("</");
        let cut = if closing {
            Some(after)
        } else {
            after.find(CLOSE).and_then(|close| after.get(close..))
        };
        rest = match cut {
            Some(cut) => match cut.find('>') {
                Some(end) => cut.get(end + 1..).unwrap_or(""),
                None => "",
            },
            None => "",
        };
    }
    out.push_str(rest);
    if !removed {
        return out;
    }
    tracing::warn!("stripped provider tool-call markup from message text");
    collapse_blank_lines(out)
}

fn collapse_blank_lines(mut text: String) -> String {
    let mut newlines = 0usize;
    text.retain(|ch| {
        if ch == '\n' {
            newlines += 1;
            newlines <= 2
        } else {
            newlines = 0;
            true
        }
    });
    let start = text.len() - text.trim_start().len();
    text.drain(..start);
    let end = text.trim_end().len();
    text.truncate(end);
    text
}
