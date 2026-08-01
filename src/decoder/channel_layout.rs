use symphonia::core::audio::{ChannelLabel, Channels, Position};

use crate::channel_layout::{ChannelLayout, ChannelPosition};

/// Adapt codec channel metadata into the crate's role-level layout model.
///
/// Explicit metadata owns its slot count and order. Count-based speaker
/// inference is reserved for the absence of channel metadata.
pub(super) fn layout_from_codec(
    channels: Option<&Channels>,
    fallback_count: usize,
) -> ChannelLayout {
    match channels {
        None => ChannelLayout::from_count(fallback_count),
        Some(Channels::Positioned(positions)) => {
            ChannelLayout::from_positions(positions.iter().map(map_position).collect::<Vec<_>>())
        }
        Some(Channels::Custom(labels)) => {
            ChannelLayout::from_positions(labels.iter().map(map_custom_label).collect::<Vec<_>>())
        }
        Some(channels) => unspecified_layout(channels.count()),
    }
}

fn unspecified_layout(channel_count: usize) -> ChannelLayout {
    ChannelLayout::from_positions(vec![ChannelPosition::Unspecified; channel_count])
}

fn map_custom_label(label: &ChannelLabel) -> ChannelPosition {
    match label {
        ChannelLabel::Positioned(position) => map_position(*position),
        ChannelLabel::Discrete(_)
        | ChannelLabel::Ambisonic(_)
        | ChannelLabel::AmbisonicBFormat(_) => ChannelPosition::Unspecified,
        _ => ChannelPosition::Unspecified,
    }
}

fn map_position(position: Position) -> ChannelPosition {
    match position {
        position if position == Position::FRONT_LEFT => ChannelPosition::FrontLeft,
        position if position == Position::FRONT_RIGHT => ChannelPosition::FrontRight,
        position if position == Position::FRONT_CENTER => ChannelPosition::FrontCenter,
        position if position == Position::LFE1 || position == Position::LFE2 => {
            ChannelPosition::LowFrequency
        }
        position if position == Position::REAR_LEFT => ChannelPosition::RearLeft,
        position if position == Position::REAR_RIGHT => ChannelPosition::RearRight,
        position if position == Position::FRONT_LEFT_CENTER => ChannelPosition::FrontLeftCenter,
        position if position == Position::FRONT_RIGHT_CENTER => ChannelPosition::FrontRightCenter,
        position if position == Position::REAR_CENTER => ChannelPosition::RearCenter,
        position if position == Position::SIDE_LEFT => ChannelPosition::SideLeft,
        position if position == Position::SIDE_RIGHT => ChannelPosition::SideRight,
        _ => ChannelPosition::Unspecified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positioned_mask_preserves_every_slot_in_canonical_bit_order() {
        let channels = Channels::Positioned(
            Position::FRONT_LEFT
                | Position::FRONT_RIGHT
                | Position::TOP_CENTER
                | Position::TOP_FRONT_LEFT,
        );

        let layout = layout_from_codec(Some(&channels), 99);
        assert_eq!(
            layout.positions(),
            [
                ChannelPosition::FrontLeft,
                ChannelPosition::FrontRight,
                ChannelPosition::Unspecified,
                ChannelPosition::Unspecified,
            ]
        );
    }

    #[test]
    fn unsupported_position_does_not_shift_later_supported_lfe() {
        let channels =
            Channels::Positioned(Position::FRONT_LEFT | Position::TOP_CENTER | Position::LFE2);

        let layout = layout_from_codec(Some(&channels), 1);
        assert_eq!(
            layout.positions(),
            [
                ChannelPosition::FrontLeft,
                ChannelPosition::Unspecified,
                ChannelPosition::LowFrequency,
            ]
        );
    }

    #[test]
    fn custom_labels_preserve_declared_order_and_unknown_slots() {
        let channels = Channels::from(vec![
            ChannelLabel::Positioned(Position::FRONT_RIGHT),
            ChannelLabel::Discrete(7),
            ChannelLabel::Positioned(Position::TOP_FRONT_LEFT),
            ChannelLabel::Positioned(Position::FRONT_LEFT),
        ]);

        let layout = layout_from_codec(Some(&channels), 2);
        assert_eq!(
            layout.positions(),
            [
                ChannelPosition::FrontRight,
                ChannelPosition::Unspecified,
                ChannelPosition::Unspecified,
                ChannelPosition::FrontLeft,
            ]
        );
    }

    #[test]
    fn non_positional_metadata_never_guesses_speaker_roles() {
        for channels in [Channels::Discrete(4), Channels::Ambisonic(1)] {
            let layout = layout_from_codec(Some(&channels), 2);
            assert_eq!(layout.channel_count(), 4);
            assert!(layout
                .positions()
                .iter()
                .all(|position| *position == ChannelPosition::Unspecified));
        }
    }

    #[test]
    fn only_absent_metadata_uses_count_based_inference() {
        assert_eq!(layout_from_codec(None, 4), ChannelLayout::from_count(4));

        let explicitly_empty = layout_from_codec(Some(&Channels::None), 4);
        assert!(explicitly_empty.is_empty());
    }
}
