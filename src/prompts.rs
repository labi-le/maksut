//! Compile-time embedded prompt catalog (`go:embed`-style via `embed_prompts!`).
//!
//! Every `.md` file under `src/prompts/` (plus the read-only specification in
//! `docs/communication-guide.md`) is embedded into the binary at compile time
//! via `include_str!` and stripped of leading/trailing ASCII whitespace in
//! `const` context, so runtime prompt assembly performs zero file I/O.

pub(crate) const fn trim_prompt(s: &str) -> &str {
    // Filtering leading/trailing ASCII whitespace preserves UTF-8 validity.
    match std::str::from_utf8(s.as_bytes().trim_ascii()) {
        Ok(trimmed) => trimmed,
        Err(_) => s,
    }
}

macro_rules! embed_prompts {
    ($($(#[$meta:meta])* $name:ident => $path:literal),+ $(,)?) => {
        $(
            $(#[$meta])*
            pub(crate) const $name: &str = $crate::prompts::trim_prompt(include_str!($path));
        )+
    };
}

embed_prompts! {
    /// Read-only operating specification compiled into every chat system prompt.
    COMMUNICATION_GUIDE => "../docs/communication-guide.md",
    /// Core mandatory bot rules (`§ Role & Tone`, `§ Context, Identity & Owner Authority`, `§ Tool Policy`).
    SYSTEM_PROMPT => "prompts/system.md",
    /// Owner-only code/repo/memory/logs instructions (`§ Owner Mode`).
    OWNER_CODE_PROMPT => "prompts/owner_code.md",
    /// Owner-only feature queue and GitHub issue instructions (`§ Owner Feature Queue`).
    OWNER_FEATURE_PROMPT => "prompts/owner_feature.md",
    /// Group follow-up window continuation/PASS rules (`§ Follow-up Mode`).
    FOLLOWUP_PROMPT => "prompts/followup.md",
    /// One-time reminder injected after `inspect_code` runs in a turn.
    INSPECT_REMINDER_PROMPT => "prompts/inspect_reminder.md",
    /// One-time reminder injected after `research` returns sources in a turn.
    RESEARCH_MODE_PROMPT => "prompts/research_mode.md",
    /// System prompt for the one-shot GitHub issue title generator.
    FEATURE_TITLE_PROMPT => "prompts/feature_title.md",
    /// System prompt for the OpenAI-compatible audio transcription call.
    VOICE_TRANSCRIBE_PROMPT => "prompts/voice_transcribe.md",
    /// Empty-caption media/sticker prompt inside an active follow-up window.
    MEDIA_FOLLOWUP_PROMPT => "prompts/media_followup.md",
    /// Empty-caption media/sticker prompt on a direct mention or reply.
    MEDIA_DIRECT_PROMPT => "prompts/media_direct.md",
    /// Explicit sticker request directive appended to the system prompt.
    EXPLICIT_STICKER_PROMPT => "prompts/explicit_sticker.md",
    /// Untrusted-history boundary footer appended to `<memory>` / `<self_memory>`.
    MEMORY_FOOTER_PROMPT => "prompts/memory_footer.md",
    /// Header prepended to per-speaker owner directives in `user_text`.
    SPEAKER_DIRECTIVE_HEADER => "prompts/speaker_directive.md",
    /// Jev router question instructions.
    JEV_INSTRUCTIONS_PROMPT => "prompts/jev_instructions.md",
    /// Jev router `reply` criterion.
    JEV_REPLY_PROMPT => "prompts/jev_reply.md",
    /// Jev router `sticker` criterion.
    JEV_STICKER_PROMPT => "prompts/jev_sticker.md",
    /// Jev router `wait` criterion.
    JEV_WAIT_PROMPT => "prompts/jev_wait.md",
    /// Jev router `silent` criterion.
    JEV_SILENT_PROMPT => "prompts/jev_silent.md",
    /// System prompt block injected when the Jev router selects `Decision::Sticker`.
    STICKER_MODE_PROMPT => "prompts/sticker_mode.md",
}
