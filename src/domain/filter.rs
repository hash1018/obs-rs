//! What is done to a Source's picture before it reaches the Canvas.
//!
//! A Source carries an ordered list of these, and each is applied in turn
//! between the Source producing a frame and the compositor drawing it.
//!
//! # They belong to the Source, not the SceneItem
//!
//! [`SceneItem`](super::SceneItem) holds a `source_id`, so one Source can
//! appear in several Scenes. A Transform and a Crop are the SceneItem's,
//! because placing the same camera differently in two Scenes is the point of
//! having it twice. Keying its green screen is not: that is a property of
//! what the camera is showing, and wanting it in one Scene and not another
//! would be wanting two different cameras.
//!
//! This is also what OBS does, and it falls out of the existing model rather
//! than being chosen — the filters hang off the Source because that is the
//! row they describe.

/// Row identity for one filter, assigned by the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FilterId(pub i64);

/// What a filter does.
///
/// One variant today. The list, the ordering and the per-kind settings table
/// are what make the second one small — see [`FilterSettings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    ChromaKey,
}

impl FilterKind {
    pub(crate) fn storage_name(self) -> &'static str {
        match self {
            Self::ChromaKey => "chroma_key",
        }
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "chroma_key" => Some(Self::ChromaKey),
            _ => None,
        }
    }
}

/// One filter on a Source, as it is stored and shown.
///
/// `enabled` is deliberately not part of [`FilterSettings`]: it says whether
/// to run the filter, not how, and turning one off must not cost it the
/// settings it was tuned to. It is also the only thing here that a running
/// Source can be told about without being rebuilt.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub id: FilterId,
    pub enabled: bool,
    /// Which kind this is, and its settings, in one.
    ///
    /// There is no `kind` field beside this. The database keeps a `kind`
    /// column because it has to know which settings table to join; in memory
    /// the settings are that answer, and a second copy could only ever
    /// disagree with them.
    pub settings: FilterSettings,
}

/// The settings of whichever filter this is.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterSettings {
    ChromaKey(ChromaKeySettings),
}

/// Which colour a chroma key treats as transparent, and how forgivingly.
///
/// Mirrors `media_pp::elements::ChromaKeyOptions` rather than reusing it, for
/// the reason [`RtspTransport`](super::RtspTransport) is mirrored: this is
/// written to the database and drawn by the UI, neither of which should have
/// to know what the element takes.
///
/// It differs from that type in one way, and on purpose. There, a custom
/// colour lives inside the method; here it sits beside it, so picking a
/// colour, switching to Green to compare, and switching back finds the colour
/// still there. A dropdown and a colour well are two controls that each keep
/// their own value, and this is the shape that lets them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromaKeySettings {
    pub method: ChromaKeyMethod,
    /// The colour [`ChromaKeyMethod::Custom`] keys, kept whichever method is
    /// selected.
    pub custom_rgb: [u8; 3],
    /// How far a pixel may differ from the key colour before it counts as
    /// foreground, as a fraction of the largest possible difference. `0.0` to
    /// `1.0` is the meaningful range.
    pub threshold: f32,
    /// Width of the soft band straddling `threshold`, in the same units.
    /// Zero is a hard cutout with a jagged edge.
    pub smoothing: f32,
}

impl Default for ChromaKeySettings {
    /// A green screen, keyed the way one usually wants to be keyed.
    ///
    /// These are the numbers the `d3d11_chroma_key` example settled on
    /// against a backdrop that is exactly the key colour — enough threshold
    /// to cover the feathered edge, and enough smoothing to make one. A
    /// custom colour nobody has picked yet starts on green, so switching to
    /// it changes nothing until it is changed.
    fn default() -> Self {
        Self {
            method: ChromaKeyMethod::Green,
            custom_rgb: [0, 255, 0],
            threshold: 0.15,
            smoothing: 0.1,
        }
    }
}

/// Which colour is being keyed out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaKeyMethod {
    /// The conventional pure-green screen.
    Green,
    /// The conventional pure-blue screen.
    Blue,
    /// Whatever [`ChromaKeySettings::custom_rgb`] holds — a differently
    /// coloured backdrop, or a solid background that is not a screen at all.
    Custom,
}

impl ChromaKeyMethod {
    pub(crate) fn storage_name(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Custom => "custom",
        }
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_survives_a_round_trip_through_storage() {
        // One kind today, so this reads as a list of one rather than a
        // loop. It is the shape the second one is added to.
        let kind = FilterKind::ChromaKey;
        assert_eq!(
            FilterKind::from_storage_name(kind.storage_name()),
            Some(kind)
        );
        assert_eq!(FilterKind::from_storage_name("motion_blur"), None);
    }

    #[test]
    fn every_key_method_survives_a_round_trip_through_storage() {
        for method in [
            ChromaKeyMethod::Green,
            ChromaKeyMethod::Blue,
            ChromaKeyMethod::Custom,
        ] {
            assert_eq!(
                ChromaKeyMethod::from_storage_name(method.storage_name()),
                Some(method)
            );
        }
        assert_eq!(ChromaKeyMethod::from_storage_name("magenta"), None);
    }

    /// The reason the colour sits beside the method rather than inside it:
    /// choosing another method has to leave it alone.
    #[test]
    fn a_picked_colour_survives_a_trip_through_the_other_methods() {
        let mut settings = ChromaKeySettings {
            method: ChromaKeyMethod::Custom,
            custom_rgb: [12, 34, 56],
            ..ChromaKeySettings::default()
        };

        settings.method = ChromaKeyMethod::Green;
        settings.method = ChromaKeyMethod::Custom;

        assert_eq!(
            settings.custom_rgb,
            [12, 34, 56],
            "a method the colour is not used by must not cost it the colour"
        );
    }
}
