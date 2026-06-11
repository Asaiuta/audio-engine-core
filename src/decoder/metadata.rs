use symphonia::core::meta::{MetadataRevision, StandardTagKey, Value};
use symphonia::core::probe::ProbeResult;

/// Track metadata extracted from audio file tags.
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub cover_art: Option<Vec<u8>>,
    pub cover_art_mime: Option<String>,
    pub lyrics: Option<String>,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
}

/// Audio format information extracted from file.
#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits_per_sample: Option<u32>,
    pub total_frames: Option<u64>,
    pub duration_secs: Option<f64>,
    pub encoder_delay: u32,
    pub end_padding: u32,
    pub metadata: TrackMetadata,
}

pub(super) fn extract_metadata(probed: &mut ProbeResult) -> TrackMetadata {
    let mut metadata = TrackMetadata::default();

    if let Some(meta) = probed.metadata.get() {
        if let Some(revision) = meta.current() {
            merge_metadata_revision(&mut metadata, revision);
        }
    }

    if metadata.title.is_some() || metadata.artist.is_some() {
        log::debug!(
            "Extracted metadata: {:?} by {:?} from {:?}",
            metadata.title,
            metadata.artist,
            metadata.album
        );
    }

    metadata
}

pub(super) fn merge_metadata_revision(metadata: &mut TrackMetadata, revision: &MetadataRevision) {
    for tag in revision.tags() {
        match tag.std_key {
            Some(StandardTagKey::TrackTitle) => {
                metadata.title = metadata
                    .title
                    .take()
                    .or_else(|| tag_value_to_string(&tag.value));
            }
            Some(StandardTagKey::Artist) => {
                metadata.artist = metadata
                    .artist
                    .take()
                    .or_else(|| tag_value_to_string(&tag.value));
            }
            Some(StandardTagKey::Album) => {
                metadata.album = metadata
                    .album
                    .take()
                    .or_else(|| tag_value_to_string(&tag.value));
            }
            Some(StandardTagKey::TrackNumber) => {
                metadata.track_number = metadata
                    .track_number
                    .or_else(|| tag_value_to_u32(&tag.value));
            }
            Some(StandardTagKey::DiscNumber) => {
                metadata.disc_number = metadata
                    .disc_number
                    .or_else(|| tag_value_to_u32(&tag.value));
            }
            Some(StandardTagKey::Genre) => {
                metadata.genre = metadata
                    .genre
                    .take()
                    .or_else(|| tag_value_to_string(&tag.value));
            }
            Some(StandardTagKey::Date) => {
                metadata.year = metadata.year.or_else(|| tag_value_to_u32(&tag.value));
            }
            Some(StandardTagKey::Lyrics) => {
                metadata.lyrics = metadata
                    .lyrics
                    .take()
                    .or_else(|| tag_value_to_non_empty_string(&tag.value));
            }
            _ => merge_non_standard_tag(metadata, &tag.key, &tag.value),
        }
    }

    if metadata.cover_art.is_none() {
        if let Some(visual) = revision.visuals().first() {
            metadata.cover_art = Some(visual.data.to_vec());
            metadata.cover_art_mime = Some(visual.media_type.clone());
        }
    }
}

fn merge_non_standard_tag(metadata: &mut TrackMetadata, key: &str, value: &Value) {
    match key.to_lowercase().as_str() {
        "title" => {
            metadata.title = metadata.title.take().or_else(|| tag_value_to_string(value));
        }
        "artist" | "albumartist" | "album_artist" => {
            metadata.artist = metadata
                .artist
                .take()
                .or_else(|| tag_value_to_string(value));
        }
        "album" => {
            metadata.album = metadata.album.take().or_else(|| tag_value_to_string(value));
        }
        "tracknumber" | "track_number" => {
            metadata.track_number = metadata.track_number.or_else(|| tag_value_to_u32(value));
        }
        "discnumber" | "disc_number" => {
            metadata.disc_number = metadata.disc_number.or_else(|| tag_value_to_u32(value));
        }
        "genre" => {
            metadata.genre = metadata.genre.take().or_else(|| tag_value_to_string(value));
        }
        "date" | "year" => {
            metadata.year = metadata.year.or_else(|| tag_value_to_u32(value));
        }
        "lyrics"
        | "lyric"
        | "unsyncedlyrics"
        | "unsynced lyrics"
        | "unsynchronisedlyrics"
        | "unsynchronised lyrics"
        | "unsynchronizedlyrics"
        | "unsynchronized lyrics"
        | "syncedlyrics"
        | "synced lyrics" => {
            metadata.lyrics = metadata
                .lyrics
                .take()
                .or_else(|| tag_value_to_non_empty_string(value));
        }
        "replaygain_track_gain" => {
            metadata.rg_track_gain = metadata
                .rg_track_gain
                .or_else(|| parse_rg_gain_from_value(value));
        }
        "replaygain_track_peak" => {
            metadata.rg_track_peak = metadata
                .rg_track_peak
                .or_else(|| parse_rg_peak_from_value(value));
        }
        "replaygain_album_gain" => {
            metadata.rg_album_gain = metadata
                .rg_album_gain
                .or_else(|| parse_rg_gain_from_value(value));
        }
        "replaygain_album_peak" => {
            metadata.rg_album_peak = metadata
                .rg_album_peak
                .or_else(|| parse_rg_peak_from_value(value));
        }
        _ => {}
    }
}

fn tag_value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::UnsignedInt(n) => Some(n.to_string()),
        Value::SignedInt(n) => Some(n.to_string()),
        _ => None,
    }
}

fn tag_value_to_non_empty_string(value: &Value) -> Option<String> {
    tag_value_to_string(value).and_then(non_empty_string)
}

fn non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.len() == value.len() {
        Some(value)
    } else {
        Some(trimmed.to_string())
    }
}

fn tag_value_to_u32(value: &Value) -> Option<u32> {
    match value {
        Value::String(s) => s.parse().ok(),
        Value::UnsignedInt(n) => Some(*n as u32),
        Value::SignedInt(n) => Some(*n as u32),
        _ => None,
    }
}

fn parse_rg_gain_from_value(value: &Value) -> Option<f64> {
    let s = tag_value_to_string(value)?;
    parse_rg_gain_str(&s)
}

fn parse_rg_peak_from_value(value: &Value) -> Option<f64> {
    let s = tag_value_to_string(value)?;
    parse_rg_peak_str(&s)
}

fn parse_rg_gain_str(s: &str) -> Option<f64> {
    s.trim()
        .trim_end_matches("dB")
        .trim()
        .trim_end_matches("db")
        .trim()
        .parse::<f64>()
        .ok()
}

fn parse_rg_peak_str(s: &str) -> Option<f64> {
    s.split_whitespace()
        .next()
        .and_then(|p| p.parse::<f64>().ok())
}
