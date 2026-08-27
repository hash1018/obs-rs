#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneId(pub i64);

pub struct Scene {
    pub id: SceneId,
    pub name: String,
}
