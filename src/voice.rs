use crate::*;

// ─────────────────────────────────────────────────────────────────────
// Voice card: what this session sounds like, from claude-code-tts.
// Contract: docs/voice-card.md in github.com/melderan/claude-code-tts
// (schema 1). The card is the only file read; config.json and the
// session files belong to that project and may change shape.
// ─────────────────────────────────────────────────────────────────────

pub(crate) const VOICE_CARD_SCHEMA: i64 = 1;

#[derive(Deserialize, Default)]
pub(crate) struct VoiceCard {
    pub(crate) schema: Option<i64>,
    pub(crate) persona: Option<String>,
    pub(crate) voice: Option<String>,
    pub(crate) speed: Option<f64>,
    pub(crate) muted: Option<bool>,
}

/// The session id claude-tts keys the card by: `$CLAUDE_TTS_SESSION` (the
/// kits set it to the room's name), else the Claude Code project folder
/// name. The pin under `active/<host>-<pid>.session` is not tried: the
/// binary has no portable parent pid and the first two cover every room.
pub(crate) fn voice_session(env_session: Option<&str>, project_dir: &str) -> Option<String> {
    if let Some(s) = env_session
        && !s.is_empty()
    {
        return Some(s.to_string());
    }
    if project_dir.is_empty() {
        return None;
    }
    Some(project_slug(project_dir))
}

/// `/Users/me/code/app` becomes `-Users-me-code-app`: every character that
/// is not an ASCII letter or digit turns into a dash.
pub(crate) fn project_slug(dir: &str) -> String {
    dir.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

pub(crate) fn voice_card_path(home: &str, session: &str) -> String {
    format!("{}/.claude-tts/voice.d/{}.json", home, session)
}

/// Reads the card; None when there is none (the session never spoke) or
/// it cannot be read. Never a guess.
pub(crate) fn read_voice_card(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `voice: statusline-amy (en_US-amy-medium) 2.0x`, `muted` appended when
/// the session is muted. None for a card of another schema or without a
/// persona. The engine's voice string is shortened to the part after the
/// last colon (an mlx speaker preset), so `mlx-community/Kokoro-82M-bf16:af_heart`
/// reads as `af_heart`.
pub(crate) fn voice_segment(card_json: &str) -> Option<String> {
    let card: VoiceCard = serde_json::from_str(card_json).ok()?;
    if card.schema != Some(VOICE_CARD_SCHEMA) {
        return None;
    }
    let persona = card.persona.as_deref().filter(|p| !p.is_empty())?;
    let mut s = format!("voice: {}", persona);
    if let Some(v) = card.voice.as_deref()
        && !v.is_empty()
    {
        let short = v.rsplit(':').next().unwrap_or(v);
        let _ = write!(s, " ({})", short);
    }
    if let Some(sp) = card.speed {
        let _ = write!(s, " {:.1}x", sp);
    }
    if card.muted == Some(true) {
        s.push_str(" muted");
    }
    Some(s)
}
