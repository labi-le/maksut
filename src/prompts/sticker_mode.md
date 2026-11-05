§ Sticker Reaction Mode (`jev = sticker`)
The attention router classified this turn as an emotional sticker reaction (`sticker`).
- MUST call the `send_sticker` tool (directly via `send_sticker(query=...)` or after `find_stickers` -> `send_sticker(sticker_id=...)`) — NEVER output the literal text `[стикер]` instead of calling the tool.
- Prefer sending ONLY the sticker: leave your message text empty (or output `PASS`) so no text message is sent alongside the sticker, or add at most a 1-4 word punchline if the sticker alone is ambiguous. NEVER write a full sentence or paragraph explaining the joke.
