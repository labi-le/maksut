# Changelog

## [Unreleased]

### Added

- Added owner-only `/settings` Telegram Mini App for editing code facts in bot memory and whitelisted `config.toml` settings ([#117](https://github.com/labi-le/maksut/pull/117) by [@int8-ptr](https://github.com/int8-ptr))
- Research replies can now attach files: for media questions the bot searches rutracker and The Pirate Bay, shows release name, size and swarm in the dossier, and sends the found `.torrent` files plus direct file links after the answer ([#121](https://github.com/labi-le/maksut/issues/121))
- Added per-chat router settings: the bot owner can set `router_mode` (`off`/`jev`) and `router_min_confidence` for one chat through the `chat_settings` tool or the Mini App's «Чаты» tab, with the global `[router]` section as the fallback ([#123](https://github.com/labi-le/maksut/issues/123))
- Chat media is archived to Garage by sha256 content address (`chat_media` and `chat_media_blobs`), images are described once per blob with a vision model (`garage.describe_model`) and indexed in Qdrant (`kind = "media"`), `chat_search` searches both messages and media descriptions, and `send_chat_media` re-sends archived chat media via cached `file_id` or Garage fallback ([#113](https://github.com/labi-le/maksut/issues/113))
- Voice and audio messages are transcribed by the bot's own local Speaches (faster-whisper on CPU) service instead of a cloud model: speech recognition no longer needs an LLM provider key or quota, audio is only sent to `voice.base_url`, and a failed local service substitutes the usual `[не удалось распознать голосовое]` marker without a cloud fallback

### Fixed

- Chat replies no longer show the model's raw provider tool-call tokens (`<｜｜DSML｜｜ …>`): such blocks are stripped before delivery instead of being sent as message text
- Failed video uploads are retried once on a non-timeout network failure while the media deadline can still fit another attempt, and are then delivered as a document instead of being lost, so TikTok links that reach the download step no longer end with only a network error
- TikTok short links no longer hang for minutes when the redirect host resolves to an unreachable Akamai address: DNS and the redirect request are bounded, and a failed hop is retried once
- Updated yt-dlp to 2026.10.26, restoring YouTube audio and small video downloads that the previous build rejected with HTTP 403
- Long voice notes now finish local transcription instead of hitting the old timeout: `voice.timeout_secs` defaults to 90 s (CPU faster-whisper runs about 3x faster than real time) and the STT client refuses HTTP redirects
- Preserved substantive text replies on directly addressed turns when `jev` classifies the turn as `sticker_mode` without a sticker tool call
- Kept the speaker's group follow-up window active on second-person sentences that mention another participant without a vocative comma
- Handled `PASS` on closing replies to the bot without an empty-reply error while keeping the speaker's follow-up window open
- Suppressed owner keyword sticker triggers in groups during active follow-up windows, replies, or bot-addressed messages
- Kept follow-up mode silent on punctuation-only replies and short closing thanks/farewells while preserving the active speaker's window across cross-participant reactions
- Enabled referencing recent uncaptioned media in private chat (including follow-up translation requests)
- Summarized multi-line feature goals to a compact first line in `issue_created` notifications and expanded `explicit_feature_request` triggers
- Enabled `read_url` to queue direct image and file URLs (plus `<img src>` links discovered on HTML pages) for Telegram attachment upload, expanded tracker release search triggers to game/repack requests, and kept follow-up windows open on standalone `"да"` / `"нет"` choice replies
