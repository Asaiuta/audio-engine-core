use symphonia::core::formats::FormatReader;
use symphonia::core::meta::{Metadata, MetadataRevision, RawValue, StandardTag};

use crate::channel_layout::ChannelLayout;

/// Track metadata extracted from audio file tags.
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    /// Track title.
    pub title: Option<String>,
    /// Primary performer or artist name.
    pub artist: Option<String>,
    /// Album name.
    pub album: Option<String>,
    /// Position of this track within the album.
    pub track_number: Option<u32>,
    /// Disc number the track appears on.
    pub disc_number: Option<u32>,
    /// Genre tag, when present.
    pub genre: Option<String>,
    /// Release year, when tagged.
    pub year: Option<u32>,
    /// Embedded cover art bytes (format depends on [`Self::cover_art_mime`]).
    pub cover_art: Option<Vec<u8>>,
    /// MIME type of [`Self::cover_art`], e.g. `image/jpeg`.
    pub cover_art_mime: Option<String>,
    /// Unsynchronized lyrics or comment text, when tagged.
    pub lyrics: Option<String>,
    /// ReplayGain track gain in dB, when tagged.
    pub rg_track_gain: Option<f64>,
    /// ReplayGain track peak (linear), when tagged.
    pub rg_track_peak: Option<f64>,
    /// ReplayGain album gain in dB, when tagged.
    pub rg_album_gain: Option<f64>,
    /// ReplayGain album peak (linear), when tagged.
    pub rg_album_peak: Option<f64>,
}

/// Audio format information extracted from file.
#[derive(Debug, Clone)]
pub struct AudioInfo {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Interleaved channel count.
    pub channels: usize,
    /// Positional channel layout for the `channels` interleaved channels.
    ///
    /// Derived from explicit container channel metadata when available.
    /// Supported roles retain their interleave position and unsupported,
    /// discrete, or ambisonic roles become `ChannelPosition::Unspecified`.
    /// Only missing channel metadata uses a best-effort standard layout for the
    /// channel count (see [`ChannelLayout::from_count`]). Carries channel-order
    /// information that a bare `channels` count cannot, so downstream downmix
    /// and loudness weighting can reason about which slot is which speaker.
    pub channel_layout: ChannelLayout,
    /// Bits per sample when the container reports it (e.g. 16, 24, 32).
    pub bits_per_sample: Option<u32>,
    /// Total decoded frames, when known from the container.
    pub total_frames: Option<u64>,
    /// Total duration in seconds, when derivable from the container.
    pub duration_secs: Option<f64>,
    /// Encoder delay in frames that must be skipped at stream start.
    pub encoder_delay: u32,
    /// Encoder end padding in frames that must be trimmed at stream end.
    pub end_padding: u32,
    /// Track metadata extracted from file tags.
    pub metadata: TrackMetadata,
}

/// Merge staging for one extraction pass.
///
/// `AlbumArtist` is only a fallback for [`TrackMetadata::artist`]: a real
/// `Artist` tag must win regardless of the order tags or revisions arrive in,
/// so it is staged separately instead of racing keep-first into the same
/// field.
#[derive(Default)]
struct MetadataAccumulator {
    metadata: TrackMetadata,
    album_artist: Option<String>,
}

impl MetadataAccumulator {
    fn finish(mut self) -> TrackMetadata {
        if self.metadata.artist.is_none() {
            self.metadata.artist = self.album_artist;
        }
        self.metadata
    }
}

pub(super) fn extract_metadata(format_reader: &mut dyn FormatReader) -> TrackMetadata {
    let metadata = collect_newest_first(&mut format_reader.metadata());

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

/// Merge every queued metadata revision, newest first.
///
/// Symphonia queues revisions oldest-first (a tail scan pushes ID3v1/APE
/// before the header ID3v2 is read) and `current()` returns that oldest
/// revision. Reading only `current()` therefore let a 30-byte ID3v1 tag
/// shadow the complete ID3v2 title, artist, cover art, and ReplayGain on the
/// very common dual-tagged MP3. Newest-first iteration keeps this module's
/// keep-first field merge but gives the newest revision priority, while older
/// revisions still fill any fields the newest one lacks.
fn collect_newest_first(metadata_log: &mut Metadata<'_>) -> TrackMetadata {
    let mut older = Vec::new();
    while let Some(revision) = metadata_log.pop() {
        older.push(revision);
    }

    let mut accumulator = MetadataAccumulator::default();
    if let Some(newest) = metadata_log.current() {
        merge_metadata_revision(&mut accumulator, newest);
    }
    for revision in older.iter().rev() {
        merge_metadata_revision(&mut accumulator, revision);
    }
    accumulator.finish()
}

fn merge_metadata_revision(accumulator: &mut MetadataAccumulator, revision: &MetadataRevision) {
    for tag in &revision.media.tags {
        merge_tag(accumulator, tag);
    }
    for track in &revision.per_track {
        for tag in &track.metadata.tags {
            merge_tag(accumulator, tag);
        }
    }

    let metadata = &mut accumulator.metadata;
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

fn merge_tag(accumulator: &mut MetadataAccumulator, tag: &symphonia::core::meta::Tag) {
    let MetadataAccumulator {
        metadata,
        album_artist,
    } = accumulator;
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
            *album_artist = album_artist.take().or_else(|| Some(value.as_ref().clone()));
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

    merge_non_standard_tag(accumulator, &tag.raw.key, &tag.raw.value);
}

fn merge_non_standard_tag(accumulator: &mut MetadataAccumulator, key: &str, value: &RawValue) {
    let MetadataAccumulator {
        metadata,
        album_artist,
    } = accumulator;
    match key.to_lowercase().as_str() {
        "title" => {
            metadata.title = metadata.title.take().or_else(|| tag_value_to_string(value));
        }
        "artist" => {
            metadata.artist = metadata
                .artist
                .take()
                .or_else(|| tag_value_to_string(value));
        }
        "albumartist" | "album_artist" => {
            *album_artist = album_artist.take().or_else(|| tag_value_to_string(value));
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

    use super::{collect_newest_first, merge_tag, MetadataAccumulator};
    use symphonia::core::meta::{
        MetadataContainer, MetadataId, MetadataInfo, MetadataLog, MetadataRevision, RawValue,
        StandardTag, Tag,
    };

    #[test]
    fn standard_tags_and_raw_aliases_preserve_existing_metadata_contract() {
        let mut accumulator = MetadataAccumulator::default();
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

        merge_tag(&mut accumulator, &title);
        merge_tag(&mut accumulator, &album_artist);
        merge_tag(&mut accumulator, &replaygain);
        let metadata = accumulator.finish();

        assert_eq!(metadata.title.as_deref(), Some("standard title"));
        assert_eq!(metadata.artist.as_deref(), Some("album artist"));
        assert_eq!(metadata.rg_track_gain, Some(-7.25));
    }

    #[test]
    fn artist_wins_over_album_artist_in_either_arrival_order() {
        let artist_tag = || {
            Tag::new_from_parts(
                "ARTIST",
                RawValue::String(Arc::new("Track Artist".to_string())),
                Some(StandardTag::Artist(Arc::new("Track Artist".to_string()))),
            )
        };
        let album_artist_tag = || {
            Tag::new_from_parts(
                "ALBUMARTIST",
                RawValue::String(Arc::new("Album Artist".to_string())),
                Some(StandardTag::AlbumArtist(Arc::new(
                    "Album Artist".to_string(),
                ))),
            )
        };

        // Tag enumeration order is not guaranteed by any tag format; the
        // track artist must win either way instead of being keep-first
        // shadowed by an earlier album artist.
        for tags in [
            [artist_tag(), album_artist_tag()],
            [album_artist_tag(), artist_tag()],
        ] {
            let mut accumulator = MetadataAccumulator::default();
            for tag in &tags {
                merge_tag(&mut accumulator, tag);
            }
            assert_eq!(accumulator.finish().artist.as_deref(), Some("Track Artist"));
        }
    }

    fn revision_with_tags(id: MetadataId, tags: Vec<Tag>) -> MetadataRevision {
        MetadataRevision {
            info: MetadataInfo {
                metadata: id,
                short_name: "test",
                long_name: "test revision",
            },
            media: MetadataContainer {
                tags,
                visuals: Vec::new(),
            },
            per_track: Vec::new(),
        }
    }

    #[test]
    fn newest_revision_wins_and_older_revisions_fill_remaining_fields() {
        use symphonia::core::meta::well_known::{METADATA_ID_ID3V1, METADATA_ID_ID3V2};

        // Symphonia's probe pushes tail metadata (ID3v1) before the header
        // ID3v2 revision, so the oldest revision sits at the queue front.
        // Reading only `current()` used to let the truncated ID3v1 fields
        // shadow the complete ID3v2 metadata on dual-tagged files.
        let id3v1 = revision_with_tags(
            METADATA_ID_ID3V1,
            vec![
                Tag::new_from_parts(
                    "TITLE",
                    RawValue::String(Arc::new("Truncated 30-byte titl".to_string())),
                    Some(StandardTag::TrackTitle(Arc::new(
                        "Truncated 30-byte titl".to_string(),
                    ))),
                ),
                Tag::new_from_parts(
                    "GENRE",
                    RawValue::String(Arc::new("Rock".to_string())),
                    Some(StandardTag::Genre(Arc::new("Rock".to_string()))),
                ),
            ],
        );
        let id3v2 = revision_with_tags(
            METADATA_ID_ID3V2,
            vec![Tag::new_from_parts(
                "TIT2",
                RawValue::String(Arc::new("The Complete ID3v2 Title".to_string())),
                Some(StandardTag::TrackTitle(Arc::new(
                    "The Complete ID3v2 Title".to_string(),
                ))),
            )],
        );

        let mut log = MetadataLog::default();
        log.push(id3v1);
        log.push(id3v2);
        let metadata = collect_newest_first(&mut log.metadata());

        assert_eq!(metadata.title.as_deref(), Some("The Complete ID3v2 Title"));
        // Fields absent from the newest revision are still filled from the
        // older one.
        assert_eq!(metadata.genre.as_deref(), Some("Rock"));
    }
}
