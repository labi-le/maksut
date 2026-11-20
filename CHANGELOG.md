# Changelog

## [Unreleased]

### Added

- Added owner-only `/settings` Telegram Mini App for editing code facts in bot memory and whitelisted `config.toml` settings ([#117](https://github.com/labi-le/maksut/pull/117) by [@int8-ptr](https://github.com/int8-ptr))
- Research replies can now attach files: for media questions the bot searches rutracker and The Pirate Bay, shows release name, size and swarm in the dossier, and sends the found `.torrent` files plus direct file links after the answer ([#121](https://github.com/labi-le/maksut/issues/121))
- Added per-chat router settings: the bot owner can set `router_mode` (`off`/`jev`) and `router_min_confidence` for one chat through the `chat_settings` tool or the Mini App's «Чаты» tab, with the global `[router]` section as the fallback ([#123](https://github.com/labi-le/maksut/issues/123))
