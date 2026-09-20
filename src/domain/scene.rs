#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneId(pub i64);

pub struct Scene {
    pub id: SceneId,
    pub name: String,
}

/// How the Canvas gets from one Scene to the next.
///
/// One choice for the whole project rather than one per Scene. "This Scene is
/// entered differently from that one" is a requirement nobody has had here
/// yet, and it would want a Scene properties dialog that otherwise has no
/// reason to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransitionKind {
    /// The next Scene is simply there, which is what switching did before
    /// there was anything else. The default, so an existing project behaves
    /// as it did.
    #[default]
    Cut,
    /// The Scene being left stays where it is while the one arriving is drawn
    /// over it, fading up from nothing.
    ///
    /// Exactly a dissolve wherever the arriving Scene covers the Canvas,
    /// which is the ordinary case — a capture, a camera, a colour. Where it
    /// does not, the Scene underneath shows through what is uncovered until
    /// the very end of the transition, which is what
    /// [`FadeToBlack`](Self::FadeToBlack) is for. Blending two Scenes
    /// properly would mean compositing each to a picture of its own first,
    /// and this compositor draws one Canvas.
    Fade,
    /// Out to black, then in from black: the Scene being left fades away over
    /// the first half and the one arriving appears over the second.
    ///
    /// Slower to watch, and right whatever the two Scenes hold.
    FadeToBlack,
}

impl TransitionKind {
    /// Every kind, in the order the Scenes dock offers them.
    pub const ALL: [Self; 3] = [Self::Cut, Self::Fade, Self::FadeToBlack];

    /// Whether switching Scenes takes any time at all.
    pub fn is_animated(self) -> bool {
        !matches!(self, Self::Cut)
    }

    pub(crate) fn storage_name(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Fade => "fade",
            Self::FadeToBlack => "fade_to_black",
        }
    }

    pub(crate) fn from_storage_name(name: &str) -> Option<Self> {
        match name {
            "cut" => Some(Self::Cut),
            "fade" => Some(Self::Fade),
            "fade_to_black" => Some(Self::FadeToBlack),
            _ => None,
        }
    }
}

/// The shortest and longest transition the Scenes dock offers, in
/// milliseconds.
///
/// The floor is a frame or two at sixty: anything shorter is a cut with extra
/// steps. The ceiling is where a switch stops reading as a switch.
pub const MIN_TRANSITION_MS: u32 = 50;
pub const MAX_TRANSITION_MS: u32 = 3_000;
pub const DEFAULT_TRANSITION_MS: u32 = 300;

/// What a Scene switch does, as the project holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub kind: TransitionKind,
    /// How long it takes, in milliseconds. Kept while the kind is
    /// [`TransitionKind::Cut`] rather than thrown away, so turning a fade off
    /// and on again does not forget how long it was.
    pub milliseconds: u32,
}

impl Default for Transition {
    fn default() -> Self {
        Self {
            kind: TransitionKind::default(),
            milliseconds: DEFAULT_TRANSITION_MS,
        }
    }
}

impl Transition {
    /// How long a switch takes: nothing at all for a cut.
    pub fn duration(self) -> std::time::Duration {
        match self.kind.is_animated() {
            true => std::time::Duration::from_millis(u64::from(
                self.milliseconds
                    .clamp(MIN_TRANSITION_MS, MAX_TRANSITION_MS),
            )),
            false => std::time::Duration::ZERO,
        }
    }
}
