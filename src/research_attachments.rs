use crate::media::MediaKind;
use crate::prelude::*;
use crate::research::ResearchSource;
use crate::torrent::TorrentHit;

/// Extensions a research reply may attach. Anything else stays a link in the
/// dossier: the bot downloads only formats it can name and hand to Telegram.
const ATTACHMENT_EXTENSIONS: [&str; 10] = [
    "torrent", "pdf", "epub", "zip", "jpg", "jpeg", "png", "webp", "mp4", "mp3",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResearchAttachment {
    pub(crate) url: String,
    pub(crate) filename: String,
    /// Filled in once the download pipeline has resolved the planned file;
    /// selection only ever produces the plan, so everything here is unprepared.
    pub(crate) prepared: Option<crate::attachment_image::PreparedAttachment>,
}

impl ResearchAttachment {
    pub(crate) fn kind(&self) -> MediaKind {
        attachment_kind(&self.filename).unwrap_or(MediaKind::Document)
    }
}

pub(crate) fn attachment_kind(filename: &str) -> Option<MediaKind> {
    let ext = Path::new(filename).extension()?.to_str()?;
    let kind = if ["jpg", "jpeg", "png", "webp"]
        .iter()
        .any(|item| ext.eq_ignore_ascii_case(item))
    {
        MediaKind::Photo
    } else if ["mp4", "mp3", "torrent", "pdf", "epub", "zip"]
        .iter()
        .any(|item| ext.eq_ignore_ascii_case(item))
    {
        // An untrusted download is never sent as a video or audio upload:
        // `prepare_media`/`require_video_audio` only run for the media pipeline,
        // and AGENTS.md forbids sending an unprobed file as a video.
        MediaKind::Document
    } else {
        return None;
    };
    Some(kind)
}

/// Attachments for one research pass: every tracker hit that exposes a
/// `.torrent` endpoint first, then direct file links found among the sources.
/// Both are bounded by `limit`, and a URL appears once.
pub(crate) fn select(
    sources: &[ResearchSource],
    hits: &[TorrentHit],
    limit: usize,
) -> Vec<ResearchAttachment> {
    if limit == 0 {
        return Vec::new();
    }
    let mut attachments: Vec<ResearchAttachment> = Vec::new();
    for hit in hits {
        let Some(url) = hit.download_url.as_ref() else {
            continue;
        };
        if !push_bounded(
            &mut attachments,
            ResearchAttachment {
                url: url.clone(),
                filename: torrent_filename(hit),
                prepared: None,
            },
            limit,
        ) && attachments.len() >= limit
        {
            return attachments;
        }
    }
    for source in sources {
        if attachments.len() >= limit {
            break;
        }
        if let Some(attachment) = source_attachment(&source.url) {
            let _ = push_bounded(&mut attachments, attachment, limit);
        }
    }
    attachments
}

fn torrent_filename(hit: &TorrentHit) -> String {
    let stem = sanitize_stem(&hit.title);
    let stem = if stem.is_empty() {
        "release".to_string()
    } else {
        stem
    };
    format!("{stem}.torrent")
}

/// A direct link only qualifies when its path ends in an attachable extension;
/// an HTML page with a `.pdf` query parameter is not a file.
pub(crate) fn push_bounded(
    attachments: &mut Vec<ResearchAttachment>,
    attachment: ResearchAttachment,
    limit: usize,
) -> bool {
    if attachments.len() >= limit || attachments.iter().any(|e| e.url == attachment.url) {
        return false;
    }
    attachments.push(attachment);
    true
}

pub(crate) fn source_attachment(raw: &str) -> Option<ResearchAttachment> {
    let url = Url::parse(raw).ok()?;
    source_attachment_url(&url)
}

pub(crate) fn source_attachment_url(url: &Url) -> Option<ResearchAttachment> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let name = url.path_segments()?.next_back()?;
    let ext = Path::new(name).extension()?.to_str()?.to_ascii_lowercase();
    if !ATTACHMENT_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let cut = name.len().checked_sub(ext.len() + 1)?;
    let stem = sanitize_stem(name.get(..cut)?);
    let stem = if stem.is_empty() {
        "file".to_string()
    } else {
        stem
    };
    Some(ResearchAttachment {
        url: url.as_str().to_owned(),
        filename: format!("{stem}.{ext}"),
        prepared: None,
    })
}

/// Telegram shows the temp file's name, so it must survive as a display name:
/// no separators, no control or bidi/zero-width characters (a visually reversed
/// release name is a spoofing vector), and a bounded length.
fn sanitize_stem(raw: &str) -> String {
    let mut stem = String::new();
    for ch in raw.chars() {
        if crate::chat_text::is_bidi_or_control(ch)
            || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        {
            stem.push('_');
        } else {
            stem.push(ch);
        }
        if stem.chars().count() >= 80 {
            break;
        }
    }
    stem.trim_matches(['.', ' ', '_']).to_string()
}
