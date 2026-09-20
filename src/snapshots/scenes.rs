use crate::domain::{SceneId, SceneItemId, Transition};

#[derive(Clone, Default)]
pub struct ScenesSnapshot {
    pub items: Vec<SceneSnapshot>,
    pub selected_scene_id: Option<SceneId>,
    /// What switching between them does. One answer for the project, shown
    /// and set under the list itself.
    pub transition: Transition,
}

#[derive(Clone)]
pub struct SceneSnapshot {
    pub id: SceneId,
    pub name: String,
    /// The Scenes that show this one as a Source, by name.
    ///
    /// What deleting it would take with it, which the dock has to be able to
    /// say before it asks. Empty for a Scene nothing shows, which is most of
    /// them.
    pub shown_in: Vec<String>,
    /// What this Scene holds, front-most first, for the Hotkeys page: an
    /// item there is bound one at a time, and the name it is listed under is
    /// its Source's.
    ///
    /// Every Scene's, not only the selected one's. The page lists them all,
    /// and a key bound to an item of a Scene nobody is looking at is the
    /// ordinary case.
    pub items: Vec<SceneItemName>,
}

/// One placement, as the Hotkeys page names it.
#[derive(Clone)]
pub struct SceneItemName {
    pub id: SceneItemId,
    pub name: String,
}
