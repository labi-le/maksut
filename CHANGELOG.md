# Changelog

## [Unreleased]

### Added

- Added owner-only `/settings` Telegram Mini App for editing code facts in bot memory and whitelisted `config.toml` settings ([#117](https://github.com/labi-le/maksut/pull/117) by [@int8-ptr](https://github.com/int8-ptr))
- Research replies can now attach files: for media questions the bot searches rutracker and The Pirate Bay, shows release name, size and swarm in the dossier, and sends the found `.torrent` files plus direct file links after the answer ([#121](https://github.com/labi-le/maksut/issues/121))
- Added per-chat router settings: the bot owner can set `router_mode` (`off`/`jev`) and `router_min_confidence` for one chat through the `chat_settings` tool or the Mini App's «Чаты» tab, with the global `[router]` section as the fallback ([#123](https://github.com/labi-le/maksut/issues/123))
- Chat media is archived to Garage by sha256 content address: `chat_media` binds each message to a blob and `chat_media_blobs` deduplicates bytes across chats, with ingest disabled until `[garage]` endpoint and credentials are set ([#113](https://github.com/labi-le/maksut/issues/113))

### Fixed

- Preserved substantive text replies on directly addressed turns when `jev` classifies the turn as `sticker_mode` without a sticker tool call
- Kept the speaker's group follow-up window active on second-person sentences that mention another participant without a vocative comma
- Handled `PASS` on closing replies to the bot without an empty-reply error while keeping the speaker's follow-up window open
- Suppressed owner keyword sticker triggers in groups during active follow-up windows, replies, or bot-addressed messages
- Kept follow-up mode silent on punctuation-only replies and short closing thanks/farewells while preserving the active speaker's window across cross-participant reactions
- Enabled referencing recent uncaptioned media in private chat (including follow-up translation requests)
- Summarized multi-line feature goals to a compact first line in `issue_created` notifications and expanded `explicit_feature_request` triggers
