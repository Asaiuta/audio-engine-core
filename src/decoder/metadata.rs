use symphonia::core::formats::FormatReader;
use symphonia::core::meta::{MetadataRevision, RawValue, StandardTag};

use crate::channel_layout::ChannelLayout;

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
    /// Positional channel layout for the `channels` interleaved channels.
    ///
    /// Derived from the container's channel mask when available, otherwise a
    /// best-effort standard layout for the channel count (see
    /// [`ChannelLayout::from_count`]). Carries channel-order information that a
    /// bare `channels` count cannot, so downstream downmix and loudness
    /// weighting can reason about which slot is which speaker.
    pub channel_layout: ChannelLayout,
    pub bits_per_sample: Option<u32>,
    pub total_frames: Option<u64>,
    pub duration_secs: Option<f64>,
    pub encoder_delay: u32,
    pub end_padding: u32,
    pub metadata: TrackMetadata,
}

pub(super) fn extract_metadata(format_reader: &mut dyn FormatReader) -> TrackMetadata {
    let mut metadata = TrackMetadata::default();

    let metadata_log = format_reader.metadata();
    if let Some(revision) = metadata_log.current() {
        merge_metadata_revision(&mut metadata, revision);
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
    for tag in &revision.media.tags {
        merge_tag(metadata, tag);
    }
    for track in &revision.per_track {
        for tag in &track.metadata.tags {
            merge_tag(metadata, tag);
        }
    }

    if metadata.cover_art.is_none() {
        if let Some(visual) = revision.media.visuals.first() {
            metadata.cover_art = Some(visual.data.to_vec());
            metadata.cover_art_mime = visual.media_type.clone();
        } else if let Some(visual) = revision
            .per_track
            .iter()
            .flat_map(|track| track.metadata.visuals.iter())
            .next()
        {
            metadata.cover_art = Some(visual.data.to_vec());
            metadata.cover_art_mime = visual.media_type.clone();
        }
    }
}

fn merge_tag(metadata: &mut TrackMetadata, tag: &symphonia::core::meta::Tag) {
    match tag.std.as_ref() {
        Some(StandardTag::TrackTitle(value)) => {
            metadata.title = metadata
                .title
                .take()
                .or_else(|| Some(value.as_ref().clone()));
        }
        Some(StandardTag::Artist(value)) => {
            metadata.artist = metadata
                .artist
                .take()
                .or_else(|| Some(value.as_ref().clone()));
        }
        Some(StandardTag::AlbumArtist(value)) => {
            metadata.artist = metadata
                .artist
                .take()
                .or_else(|| Some(value.as_ref().clone()));
        }
        Some(StandardTag::Album(value)) => {
            metadata.album = metadata
                .album
                .take()
                .or_else(|| Some(value.as_ref().clone()));
        }
        Some(StandardTag::TrackNumber(value)) => {
            metadata.track_number = metadata.track_number.or_else(|| (*value).try_into().ok());
        }
        Some(StandardTag::DiscNumber(value)) => {
            metadata.disc_number = metadata.disc_number.or_else(|| (*value).try_into().ok());
        }
        Some(StandardTag::Genre(value)) => {
            metadata.genre = metadata
                .genre
                .take()
                .or_else(|| Some(value.as_ref().clone()));
        }
        Some(StandardTag::RecordingYear(value) | StandardTag::ReleaseYear(value)) => {
            metadata.year = metadata.year.or_else(|| Some(u32::from(*value)));
        }
        Some(StandardTag::RecordingDate(value) | StandardTag::ReleaseDate(value)) => {
            metadata.year = metadata.year.or_else(|| parse_year_str(value.as_ref()));
        }
        Some(StandardTag::Lyrics(value)) => {
            metadata.lyrics = metadata
                .lyrics
                .take()
                .or_else(|| non_empty_string(value.as_ref().clone()));
        }
        Some(StandardTag::ReplayGainTrackGain(value)) => {
            metadata.rg_track_gain = metadata
                .rg_track_gain
                .or_else(|| parse_rg_gain_str(value.as_ref()));
        }
        Some(StandardTag::ReplayGainTrackPeak(value)) => {
            metadata.rg_track_peak = metadata
                .rg_track_peak
                .or_else(|| parse_rg_peak_str(value.as_ref()));
        }
        Some(StandardTag::ReplayGainAlbumGain(value)) => {
            metadata.rg_album_gain = metadata
                .rg_album_gain
                .or_else(|| parse_rg_gain_str(value.as_ref()));
        }
        Some(StandardTag::ReplayGainAlbumPeak(value)) => {
            metadata.rg_album_peak = metadata
                .rg_album_peak
                .or_else(|| parse_rg_peak_str(value.as_ref()));
        }
        _ => {}
    }

    merge_non_standard_tag(metadata, &tag.raw.key, &tag.raw.value);
}

fn merge_non_standard_tag(metadata: &mut TrackMetadata, key: &str, value: &RawValue) {
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

fn tag_value_to_string(value: &RawValue) -> Option<String> {
    match value {
        RawValue::String(s) => Some(s.as_ref().clone()),
        RawValue::StringList(values) => Some(values.join("\n")),
        RawValue::UnsignedInt(n) => Some(n.to_string()),
        RawValue::SignedInt(n) => Some(n.to_string()),
        RawValue::Float(n) => Some(n.to_string()),
        _ => None,
    }
}

fn tag_value_to_non_empty_string(value: &RawValue) -> Option<String> {
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

fn tag_value_to_u32(value: &RawValue) -> Option<u32> {
    match value {
        RawValue::String(s) => s.parse().ok(),
        RawValue::UnsignedInt(n) => (*n).try_into().ok(),
        RawValue::SignedInt(n) => (*n).try_into().ok(),
        _ => None,
    }
}

fn parse_rg_gain_from_value(value: &RawValue) -> Option<f64> {
    let s = tag_value_to_string(value)?;
    parse_rg_gain_str(&s)
}

fn parse_rg_peak_from_value(value: &RawValue) -> Option<f64> {
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

fn parse_year_str(s: &str) -> Option<u32> {
    s.get(..4).and_then(|year| year.parse().ok())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{merge_tag, TrackMetadata};
    use symphonia::core::meta::{RawValue, StandardTag, Tag};

    #[test]
    fn standard_tags_and_raw_aliases_preserve_existing_metadata_contract() {
        let mut metadata = TrackMetadata::default();
        let title = Tag::new_from_parts(
            "TITLE",
            RawValue::String(Arc::new("raw title".to_string())),
            Some(StandardTag::TrackTitle(Arc::new(
                "standard title".to_string(),
            ))),
        );
        let album_artist = Tag::new_from_parts(
            "ALBUMARTIST",
            RawValue::String(Arc::new("album artist".to_string())),
            Some(StandardTag::AlbumArtist(Arc::new(
                "album artist".to_string(),
            ))),
        );
        let replaygain = Tag::new_from_parts(
            "replaygain_track_gain",
            RawValue::String(Arc::new("-7.25 dB".to_string())),
            None,
        );

        merge_tag(&mut metadata, &title);
        merge_tag(&mut metadata, &album_artist);
        merge_tag(&mut metadata, &replaygain);

        assert_eq!(metadata.title.as_deref(), Some("standard title"));
        assert_eq!(metadata.artist.as_deref(), Some("album artist"));
        assert_eq!(metadata.rg_track_gain, Some(-7.25));
    }
}
