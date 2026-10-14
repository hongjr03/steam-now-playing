//! Reads the current "now playing" track from Windows GSMTC.
//!
//! Windows exposes active media sessions through
//! `GlobalSystemMediaTransportControlsSessionManager`. We filter to Apple Music
//! by its App User Model ID.

use anyhow::{Context, Result};
use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;

/// App User Model ID of the Apple Music desktop app (confirmed on this machine).
pub const APPLE_MUSIC_AUMID: &str = "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App";

/// Apple Music stuffs `artist — album` into the artist field, with the album
/// field left empty. Split on the em-dash separator to recover just the artist.
const ARTIST_SEP: &str = " — ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
}

fn manager() -> Result<GlobalSystemMediaTransportControlsSessionManager> {
    Ok(
        GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .context("RequestAsync failed")?
            .join()
            .context("awaiting session manager failed")?,
    )
}

/// Returns the current Apple Music track, if a session exists and exposes a
/// title. A paused track still counts as "present" — paused music is treated
/// like a game that is still running.
pub fn current_apple_music() -> Result<Option<Track>> {
    let mgr = manager()?;
    let sessions = mgr.GetSessions().context("GetSessions failed")?;
    let count = sessions.Size().context("Size failed")?;

    for i in 0..count {
        let session = sessions.GetAt(i).context("GetAt failed")?;

        let aumid = session
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();

        if aumid != APPLE_MUSIC_AUMID {
            continue;
        }

        let props = session
            .TryGetMediaPropertiesAsync()
            .context("TryGetMediaPropertiesAsync failed")?
            .join()
            .context("awaiting media properties failed")?;

        let title = props.Title().map(|h| h.to_string()).unwrap_or_default();
        let raw_artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
        let artist = raw_artist
            .split(ARTIST_SEP)
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();

        return Ok(Some(Track { title, artist }));
    }

    Ok(None)
}

/// Dump every active session — handy for discovering an app's AUMID.
pub fn probe() -> Result<()> {
    let mgr = manager()?;
    let sessions = mgr.GetSessions().context("GetSessions failed")?;
    let count = sessions.Size().context("Size failed")?;

    let current_id = mgr
        .GetCurrentSession()
        .ok()
        .and_then(|s| s.SourceAppUserModelId().ok())
        .map(|h| h.to_string());

    println!("sessions: {count}");
    println!("current : {current_id:?}\n");

    for i in 0..count {
        let session = sessions.GetAt(i).context("GetAt failed")?;

        let aumid = session
            .SourceAppUserModelId()
            .map(|h| h.to_string())
            .unwrap_or_default();

        let (title, artist, album) = match session
            .TryGetMediaPropertiesAsync()
            .and_then(|op| op.join())
        {
            Ok(props) => (
                props.Title().map(|h| h.to_string()).unwrap_or_default(),
                props.Artist().map(|h| h.to_string()).unwrap_or_default(),
                props
                    .AlbumTitle()
                    .map(|h| h.to_string())
                    .unwrap_or_default(),
            ),
            Err(_) => (String::new(), String::new(), String::new()),
        };

        let status = session
            .GetPlaybackInfo()
            .ok()
            .and_then(|p| p.PlaybackStatus().ok());

        let marker = if Some(aumid.clone()) == current_id {
            "*CURRENT*"
        } else {
            "         "
        };
        println!("[{i}] {marker}");
        println!("    aumid  = {aumid}");
        println!("    title  = {title}");
        println!("    artist = {artist}");
        println!("    album  = {album}");
        println!("    status = {status:?}");
    }

    Ok(())
}
