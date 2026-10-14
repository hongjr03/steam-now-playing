//! Turns a GSMTC `Track` into the exact Steam status string.

use crate::gsmtc::Track;

/// e.g. `Apple Music · ♪ Sharpest Tool — Sabrina Carpenter`
pub fn status_text(track: &Track) -> String {
    let title = track.title.trim();
    let artist = track.artist.trim();

    if artist.is_empty() {
        format!("Apple Music · ♪ {title}")
    } else {
        format!("Apple Music · ♪ {title} — {artist}")
    }
}
